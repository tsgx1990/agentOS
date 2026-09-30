// 宿主工具：把"本应用被授权的 MCP 工具清单"注册成 pi 可调用的工具，execute
// 时转发给宿主的 host_mcp_call（Task7，
// `src-tauri/src/mcp.rs::McpManager::host_mcp_call`）执行"二次授权复核 + 危险
// 分级门 + 审计"之后的真正 MCP `tools/call`。
//
// 授权清单经 env `SUPERAGENT_MCP_TOOLS` 注入（JSON 数组，元素形如
// `{server, tool, name?, description?, inputSchema?}`）。本文件只按清单注册
// `mcp__<server>__<tool>` 工具 + 透传调用，不做二次授权判断——那是宿主
// `host_mcp_call` 自己纵深防御的职责（见 mcp.rs 文档注释：不信任这里的可见性
// 过滤没被绕过，host 侧会用同一份 `authorized_tools` 现场重算一遍）。
//
// 实际的宿主调用传输见 `./mcp_transport.ts`（`hostMcpCall`）——单独拆成一个
// 文件是为了让它在测试里可以整体被 `vi.mock` 掉（同一模块内部调用自身导出的
// 函数没法可靠地被 spy 拦截），也让 Task9 换传输时只用改那一个文件。该文件
// 头部注释记录了本任务对 pi 扩展 API 的调研结论：pi 没有提供任何"扩展→宿主"
// 的请求/响应原语，这个传输是按最可行方案实现的开放项。
import { Type, type TSchema } from "typebox";
import { hostMcpCall } from "./mcp_transport";

/** 单条授权工具的清单形状（宿主经 `SUPERAGENT_MCP_TOOLS` 注入）。 */
export interface AuthorizedMcpTool {
  server: string;
  tool: string;
  name?: string;
  description?: string;
  /** MCP 工具自带的 JSON Schema，原样透传给 registerTool 的 parameters；不在
   * 本层做 JSON Schema → typebox 的语义转换/校验。 */
  inputSchema?: unknown;
}

const TOOLS_ENV = "SUPERAGENT_MCP_TOOLS";

function isAuthorizedMcpTool(value: unknown): value is AuthorizedMcpTool {
  if (!value || typeof value !== "object") return false;
  const v = value as Record<string, unknown>;
  return typeof v.server === "string" && typeof v.tool === "string";
}

/**
 * 解析 `SUPERAGENT_MCP_TOOLS`。缺失/空字符串/JSON 损坏/不是数组/元素形状不对
 * 都静默返回空数组（过滤掉形状不对的单个元素，不因为一条坏数据整体失败），
 * 绝不抛异常——扩展加载失败会导致整个 pi 进程起不来（见
 * `docs/superpowers/spikes/2026-07-17-host-hook-priority.md` 证据2）。
 */
export function parseAuthorizedTools(env: NodeJS.ProcessEnv = process.env): AuthorizedMcpTool[] {
  const raw = env[TOOLS_ENV];
  if (!raw) return [];
  let parsed: unknown;
  try {
    parsed = JSON.parse(raw);
  } catch {
    return [];
  }
  if (!Array.isArray(parsed)) return [];
  return parsed.filter(isAuthorizedMcpTool);
}

export default function (pi: any) {
  const tools = parseAuthorizedTools();
  for (const t of tools) {
    const toolName = `mcp__${t.server}__${t.tool}`;
    pi.registerTool({
      name: toolName,
      label: t.name ?? toolName,
      description: t.description ?? `转发到 MCP server "${t.server}" 的工具 "${t.tool}"`,
      parameters: (t.inputSchema as TSchema | undefined) ?? Type.Object({}),
      async execute(_id: string, params: unknown) {
        try {
          const result = await hostMcpCall({ server: t.server, tool: t.tool, args: params });
          return {
            content: [{ type: "text", text: JSON.stringify(result ?? null) }],
            details: result,
          };
        } catch (err) {
          const message = err instanceof Error ? err.message : String(err);
          // ToolDefinition.execute 的约定是"失败就抛"（AgentTool 类型注释：
          // "Throw on failure instead of encoding errors in content"），而不是
          // 在返回值里编码 isError——所以拒绝/失败原因原样往外抛。
          throw new Error(`MCP 工具 ${toolName} 调用失败：${message}`);
        }
      },
    });
  }
}
