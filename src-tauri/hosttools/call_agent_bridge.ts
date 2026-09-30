// 宿主工具（P5 互联）：把 `__host_call_agent__` 注册成 pi 可调用的工具，execute 时经
// `mcp_transport.ts` 共用的 `hostCall(method, params)` socket 客户端转发给宿主的
// `call_bus::handle_call_agent`（`mcp_socket.rs::process_request` 的 call_agent 分支）。
//
// 工具名必须与 `mcp_socket.rs` 里 `if method == "__host_call_agent__"` 判断的字面量
// 逐字一致——名字对不上就落回 `__host_mcp_call__` 分支（缺 params.server/tool 报错），
// 不是"工具不存在"这种易发现的失败，所以直接用字面量常量，不做任何拼接/派生。
//
// 响应"原样透传"：宿主 call_bus 返回的是 `CallResult`（`{ok, text, error}`）——与
// `__host_mcp_call__` 的 `{result,error}` 是两套独立协议，故用 `hostCall`（不解包）
// 而非 `hostMcpCall`（会按 MCP 协议解包 `result`/`error`，套在 CallResult 上会误读
// `{ok:false,...}` 这种"业务失败但传输成功"的响应）。调用方 LLM 自己解释 `ok`/`text`/
// `error` 字段。
//
// 缺 `SUPERAGENT_MCP_SOCKET`/socket 连不上时降级：`registerTool` 本身不做 env 检查
// （工具始终注册、形状对 LLM 可见），真正连不上时把失败原因编码进返回值
// `{ok:false, error}` 而不是 throw——CallResult 的协议本身就是"返回值携带成败"。
import { Type } from "typebox";
import { hostCall } from "./mcp_transport";

const CALL_AGENT = "__host_call_agent__";

/** hostCall 失败（env 缺失/socket 连不上/响应非法 JSON）时的降级返回值。 */
function transportFailure(err: unknown): { ok: false; text: string; error: string } {
  const message = err instanceof Error ? err.message : String(err);
  return { ok: false, text: "", error: `互联调用 ${CALL_AGENT} 传输失败：${message}` };
}

export default function (pi: any) {
  pi.registerTool({
    name: CALL_AGENT,
    label: "调用其它应用",
    description:
      "把一个子任务交给另一个已安装的应用（subagent）处理，返回它的文本结果。" +
      "target 为目标应用的包名或 app_id（须在本应用声明的可调用列表内；主助手可调任意已装应用）。",
    parameters: Type.Object({
      target: Type.String(),
      prompt: Type.String(),
    }),
    async execute(_id: string, params: unknown) {
      try {
        return await hostCall(CALL_AGENT, params);
      } catch (err) {
        return transportFailure(err);
      }
    },
  });
}
