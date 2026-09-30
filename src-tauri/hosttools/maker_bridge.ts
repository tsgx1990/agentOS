// 宿主工具：把 Maker（P4）的四个宿主方法注册成 pi 可调用的工具，execute 时经
// `mcp_transport.ts` 共用的 `hostCall(method, params)` socket 客户端转发给
// 宿主的 `maker::handle_maker_request`（`src-tauri/src/mcp_socket.rs::
// process_request` Task3 新增的分发分支）。
//
// 四个工具名字必须与 `mcp_socket.rs`/`maker.rs::handle_maker_request` 里
// `match method` 判断的字面量逐字一致：`__host_maker_stage_write__`/
// `__host_maker_preview__`/`__host_maker_install__`/
// `__host_maker_install_skill__`（第四个是 Task7、P6-B 新增——Maker 生成技能
// 走的是这条 seam，而不是 `__host_maker_install__`：应用与技能各自有自己的
// 安装门，前者装应用、后者装技能，绝不能共用同一个方法名）——宿主按
// `method` 字符串分发，名字对不上就落回 `__host_mcp_call__` 那条分支（缺
// `params.server`/`params.tool` 直接报错），不是"工具不存在"这种容易发现的
// 失败，所以这里不做任何拼接/派生，直接用字面量常量。
//
// 响应处理刻意"原样透传"：mcp_socket.rs 文档注释明确写了 maker 四个方法的
// 返回形状（`{ok, ...}` / `{pending_confirm, confirm_id, ...}`）跟
// `__host_mcp_call__` 的 `{result:...}`/`{error:...}` 编码规则是两套独立协议
// ——所以这里用 `hostCall`（不解包）而不是 `hostMcpCall`（会按 MCP 协议解包
// `result`/`error`，套在 maker 响应上会把 `{ok:false,...}` 这种"业务失败但
// 传输成功"的响应错误地拆开或漏读）。Maker 侧的 LLM/工具消费者自己解释
// `ok`/`pending_confirm` 字段的含义。
//
// 缺 `SUPERAGENT_MCP_SOCKET` 时的降级行为对齐 `mcp_bridge.ts` 的"不让扩展加载
// 失败拖垮整个 pi 进程"原则：`registerTool` 本身不做 env 检查（三个工具总是
// 注册，形状对 LLM 可见），真正连不上时把失败原因编码进返回值里的
// `{ok:false, error}`，而不是 throw——因为 Maker 的三个方法对调用方的约定是
// "返回值里的 `ok`/`pending_confirm` 字段承载结果"，不是"失败就抛"（这点
// 和 `mcp_bridge.ts` 的 MCP 转发不同：那边 `ToolDefinition.execute` 约定
// "Throw on failure"，这边协议本身就是"verbatim 返回值携带成败"，抛出反而
// 破坏了调用方期待的响应形状）。
import { Type } from "typebox";
import { hostCall } from "./mcp_transport";

const STAGE_WRITE = "__host_maker_stage_write__";
const PREVIEW = "__host_maker_preview__";
const INSTALL = "__host_maker_install__";
const INSTALL_SKILL = "__host_maker_install_skill__";

/** hostCall 失败（env 缺失/socket 连不上/响应非法 JSON）时的降级返回值。 */
function transportFailure(toolName: string, err: unknown): { ok: false; error: string } {
  const message = err instanceof Error ? err.message : String(err);
  return { ok: false, error: `Maker 宿主工具 ${toolName} 传输失败：${message}` };
}

export default function (pi: any) {
  pi.registerTool({
    name: STAGE_WRITE,
    label: "Maker: 写暂存文件",
    description: "把生成的一个文件写入当前草稿（draft）的暂存目录。",
    parameters: Type.Object({
      draft_id: Type.String(),
      rel_path: Type.String(),
      content: Type.String(),
    }),
    async execute(_id: string, params: unknown) {
      try {
        return await hostCall(STAGE_WRITE, params);
      } catch (err) {
        return transportFailure(STAGE_WRITE, err);
      }
    },
  });

  pi.registerTool({
    name: PREVIEW,
    label: "Maker: 预览草稿",
    description: "在沙盒里预览当前草稿（draft）暂存目录里的内容。",
    parameters: Type.Object({
      draft_id: Type.String(),
    }),
    async execute(_id: string, params: unknown) {
      try {
        return await hostCall(PREVIEW, params);
      } catch (err) {
        return transportFailure(PREVIEW, err);
      }
    },
  });

  pi.registerTool({
    name: INSTALL,
    label: "Maker: 安装草稿",
    description: "把当前草稿（draft）登记为待确认安装（需要用户在通知中心确认）。",
    parameters: Type.Object({
      draft_id: Type.String(),
    }),
    async execute(_id: string, params: unknown) {
      try {
        return await hostCall(INSTALL, params);
      } catch (err) {
        return transportFailure(INSTALL, err);
      }
    },
  });

  pi.registerTool({
    name: INSTALL_SKILL,
    label: "Maker: 安装技能草稿",
    description:
      "把当前草稿（draft）里的 SKILL.md 登记为待确认的技能安装。调用前先用 " +
      "__host_maker_stage_write__ 把 SKILL.md（YAML frontmatter 至少含 name/" +
      "description，可选 scripts/ 目录放脚本文件）写进同一个 draft_id 目录，再" +
      "调用本工具。返回 pending_confirm 表示已登记成功，需要等用户在「技能」" +
      "页确认后才会真正安装——本工具自身绝不安装任何东西。",
    parameters: Type.Object({
      draft_id: Type.String(),
    }),
    async execute(_id: string, params: unknown) {
      try {
        return await hostCall(INSTALL_SKILL, params);
      } catch (err) {
        return transportFailure(INSTALL_SKILL, err);
      }
    },
  });
}
