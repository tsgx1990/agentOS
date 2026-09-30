// 宿主工具（P5 意图路由）：把 `__host_list_agents__` 注册成 pi 可调用的工具，供主助手
// （内置 superagent=router）据用户意图查有哪些已装应用可派活。经 `mcp_transport.ts` 的
// `hostCall` 转发给宿主 `call_bus::handle_list_agents`（`mcp_socket.rs::process_request`
// 的 list_agents 分支，宿主侧只对 MAKER_APP_ID 放行）。
//
// 本桥仅注入给 router（`session_mgr::agent_launch_extras` 里 is_maker 才挂），所以只有
// 主助手会看到这个工具；即便被误注入到别处，宿主门控也会拒（unauthorized）。工具名
// 必须与宿主分发分支的字面量逐字一致：`__host_list_agents__`。
//
// 响应原样透传（`{ok:true, agents:[...]}`）；传输失败降级 `{ok:false, error}` 不抛，
// 对齐 maker_bridge/call_agent_bridge 的"返回值携带成败"约定。
import { Type } from "typebox";
import { hostCall } from "./mcp_transport";

const LIST_AGENTS = "__host_list_agents__";

export default function (pi: any) {
  pi.registerTool({
    name: LIST_AGENTS,
    label: "列出已装应用",
    description: "列出当前已安装的应用（subagent）目录（app_id/名称/分类），用于据用户意图选择要调用的专家应用。",
    parameters: Type.Object({}),
    async execute(_id: string, _params: unknown) {
      try {
        return await hostCall(LIST_AGENTS, {});
      } catch (err) {
        const message = err instanceof Error ? err.message : String(err);
        return { ok: false, agents: [], error: `列出应用 ${LIST_AGENTS} 传输失败：${message}` };
      }
    },
  });
}
