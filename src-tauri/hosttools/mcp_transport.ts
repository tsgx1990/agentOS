// mcp-bridge 的宿主调用传输层（Task8 调研产出，Task9 的开放项）。
//
// ## pi 扩展→宿主 请求/响应原语调研结论
//
// 调研对象：npm 包 `@earendil-works/pi-coding-agent@0.74.2`
// （包内的 `dist/core/extensions/types.d.ts`，即 `ExtensionAPI` 的权威
// 声明文件——比 `docs/superpowers/spikes/2026-07-17-host-hook-priority.md`
// 里记录的 0.80.10 略旧，但两版之间 `ExtensionAPI` 相关方法没有增删）。
//
// `ExtensionAPI`（`pi` 参数）暴露的方法逐一过一遍，找"扩展主动发请求、等待
// 宿主返回结果"的原语：
// - `on(event, handler)`——事件订阅。部分事件（如 `tool_call`）handler 可以
//   返回一个结果给 pi 自己的运行时用（例如 `{block:true}`），但这是"pi 内核
//   在问这个扩展一个问题"，方向反了——不是"扩展问宿主一个问题"。
// - `registerTool`——注册一个 LLM 可调用的工具；`execute()` 拿到的
//   `ExtensionContext` 只有 session/model/UI 等 pi 自己的内部状态，没有任何
//   "发消息给外部宿主进程"的字段。
// - `appendEntry`——只是把一条记录写进 session 文件（给 UI/审计读），没有回传
//   通道；`permission_gate.ts` 已经把它当纯 fire-and-forget 用（该文件顶部
//   注释记录了 P1 对 `ui_emit` 的结论同样是 observe-only：宿主只能事后经
//   `tool_execution_end` 事件"观察"，没有返回路径）。
// - `exec(command, args, options)`——让 pi 替扩展跑一条子进程命令，是通用
//   shell 能力，不是"跟宿主 IPC"的专用 API（理论上可以借它 shell 出去打一个
//   socket 客户端，但那和下面直接用 `node:net` 是同一件事，没必要多绕一层
//   子进程）。
// - `events`（EventBus）——文档写明是"extension 之间"通信用的共享事件总线，
//   同进程内，同样不通向宿主进程。
//
// **结论：pi 没有任何官方的、扩展可以发起并等待响应的"调宿主"原语。**
// 宿主（Tauri/Rust）↔ pi（Node 子进程）现有的唯一通道是
// `src-tauri/src/rpc.rs` 那条**宿主发起、pi 应答**的 stdio JSON-RPC，没有
// 反向（pi 发起、宿主应答）通道。这不是某个具体扩展没做好，是 pi 扩展系统
// 本身目前的限制。
//
// ## 这里的实现：Unix Domain Socket（Task9 开放项）
//
// 既然 pi 不提供原语，本文件按"最可行的旁路传输"实现：宿主经 env 注入
// 一个 Unix Domain Socket 路径（`SUPERAGENT_MCP_SOCKET`），每次调用新开一条
// 连接，写一行 JSON 请求 `{method, params}`，读一行 JSON 响应后关闭连接。
//
// `hostCall(method, params)`（Task8 抽出）是这条 socket 客户端的**唯一**
// 实现——只负责"连接、写一行、读一行、JSON.parse"，把解析出来的响应原样
// resolve 给调用方，不对响应形状做任何语义假设。`src-tauri/src/mcp_socket.rs`
// 的 `process_request`（Task3，P4）证实了宿主侧对不同 `method` 用的是两套
// 独立的响应编码规则：`__host_mcp_call__` 回 `{result:...}`/`{error:...}`，
// 而三个 `__host_maker_*__` 方法把 handler 的返回值原样当整条响应写回、不
// 套用 `{result,error}` 包装。所以响应形状的解释权交给各自的上层封装：
// - `hostMcpCall`（本文件）在 `hostCall` 之上做 `{result,error}` 解包，供
//   `mcp_bridge.ts` 使用；
// - `maker_bridge.ts` 直接用 `hostCall("__host_maker_xxx__", params)`，原样
//   透传响应，不做任何解包。
// 两者共用同一条 socket 客户端（DRY），谁都不重复实现 connect/写行/读行。
//
// **协议细节仍是 Task9 的开放项，不是本任务的完成品**：
// - 具体帧协议需要跟 Task9 的宿主监听端对齐，包括错误码、超时、大 payload
//   分片等细节；
// - P2 的 macOS `sandbox-exec` 默认会挡掉任意 socket 连接，需要专门为这一个
//   路径开一条网络例外（否则本文件在真实沙箱里连不上，只能在无沙箱/测试
//   环境下验证）；
// - 本文件只导出函数，刻意让调用方（`mcp_bridge.ts`/`maker_bridge.ts` 的
//   `execute()`）不知道传输细节——测试里整体 `vi.mock` 掉这个文件，Task9 换
//   成任何其它传输（命名管道、回环 TCP 端口……）都只需要改这一个文件。
import * as net from "node:net";

/** 转发给宿主的一次 MCP 工具调用。 */
export interface HostMcpCallPayload {
  server: string;
  tool: string;
  args: unknown;
}

const SOCKET_ENV = "SUPERAGENT_MCP_SOCKET";

/**
 * 低层 socket 客户端（Task8 从 `hostMcpCall` 抽出，供 MCP/Maker 两条调用路径
 * 共用）：连接 `SUPERAGENT_MCP_SOCKET`，写一行 JSON 请求 `{method, params}`，
 * 读一行 JSON 响应，`JSON.parse` 后原样 resolve——不对响应形状做任何
 * `{result,error}` 语义解包，由调用方自己按各自协议解释（见文件头注释）。
 * env 未注入、socket 连接失败、响应不是合法 JSON 均 reject 出 Error，不吞。
 */
export async function hostCall(method: string, params: unknown): Promise<unknown> {
  const socketPath = process.env[SOCKET_ENV];
  if (!socketPath) {
    throw new Error(
      `${SOCKET_ENV} 未注入：宿主尚未提供调用传输通道（Task9 开放项，见 mcp_transport.ts 头部注释）`,
    );
  }

  return new Promise<unknown>((resolve, reject) => {
    const socket = net.createConnection(socketPath);
    let buffer = "";
    let settled = false;

    const finish = (fn: () => void) => {
      if (settled) return;
      settled = true;
      socket.removeAllListeners();
      socket.destroy();
      fn();
    };

    socket.on("connect", () => {
      socket.write(`${JSON.stringify({ method, params })}\n`);
    });

    socket.on("data", (chunk: Buffer) => {
      buffer += chunk.toString("utf8");
      const nl = buffer.indexOf("\n");
      if (nl === -1) return;
      const line = buffer.slice(0, nl);
      finish(() => {
        try {
          resolve(JSON.parse(line));
        } catch (e) {
          reject(e instanceof Error ? e : new Error(String(e)));
        }
      });
    });

    socket.on("error", (err) => {
      finish(() => reject(err));
    });
  });
}

/**
 * 把一次 MCP 工具调用转发给宿主，等待并返回结果（或 reject 出错误）。
 * 建立在共用的 `hostCall` 之上，只额外做 `__host_mcp_call__` 专属的
 * `{result:...}`/`{error:...}` 响应解包（宿主 `mcp.rs::host_mcp_call` 的
 * `encode_result` 编码规则）。见文件头注释：这是 Task9 才会真正接上宿主
 * 监听端的开放项传输。
 */
export async function hostMcpCall(payload: HostMcpCallPayload): Promise<unknown> {
  const parsed = await hostCall("__host_mcp_call__", payload);
  if (parsed && typeof parsed === "object" && "error" in parsed && (parsed as { error?: unknown }).error) {
    throw new Error(String((parsed as { error?: unknown }).error));
  }
  if (parsed && typeof parsed === "object" && "result" in parsed) {
    return (parsed as { result?: unknown }).result;
  }
  return parsed;
}
