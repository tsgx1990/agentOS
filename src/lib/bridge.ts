import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

// 与 Rust `lib.rs` 内 `BRIDGE_JS` 的 origin 前缀一致（`location.origin.startsWith('sagent')`）。
// 两侧各自持有一份字符串字面量，这里导出仅供前端测试断言，避免两处定义悄悄漂移。
export const BRIDGE_SNIPPET_MARKER = "sagent";

/**
 * 宿主父帧侧的消息中继：监听所有 iframe 发来的 `postMessage`，把已知来源
 * （已打开的应用 origin）的消息转发为 Tauri `invoke` 调用；未知/伪造来源
 * （`originToApp` 返回 null）一律直接丢弃，绝不 invoke——这是防止任意网页
 * 或伪造 iframe 冒充应用发指令的安全关键校验点。
 *
 * @param originToApp 由消息的 `event.origin`（应用 iframe 的 `sagent*` scheme）
 *   反解出对应的 appId；未知来源必须返回 null。
 */
export function installBridgeRelay(originToApp: (origin: string) => string | null) {
  const handler = (e: MessageEvent) => {
    const d = (e.data ?? {}) as any;
    if (!d.__superagent) return; // 不是桥消息，忽略
    const appId = originToApp(e.origin);
    if (!appId) return; // 拒绝未知/伪造来源：不反解到 appId 就绝不 invoke
    if (d.kind === "prompt") invoke("app_prompt", { appId, text: d.text });
    else if (d.kind === "command") invoke("app_command", { appId, name: d.name, params: d.params });
    else if (d.kind === "state_set") invoke("app_state_set", { appId, key: d.key, value: d.value });
    else if (d.kind === "state_get") {
      invoke<unknown>("app_state_get", { appId, key: d.key }).then((value) => {
        // 定向回发到已校验的发送方 origin（e.origin，上面 originToApp 已验证过），
        // 而非 "*"：state_get 的回复只应到达发起请求的那个应用帧自己。
        (e.source as Window | null)?.postMessage(
          { __superagent_host: true, event: "__state__:" + d.key, payload: value },
          e.origin,
        );
      });
    }
  };
  window.addEventListener("message", handler);
  // 返回卸载函数：调用方（如 Shell 在 `open` 变化时重新注册 originToApp 闭包）
  // 应在下次注册前先卸载旧的监听器，否则每次状态变化都会叠加一个新监听器。
  return () => window.removeEventListener("message", handler);
}

/**
 * 为某个已打开应用订阅其 `ui-emit:<appId>` Tauri 事件，转发（`postMessage`）
 * 进它自己的 iframe，供 `window.superagent.on(event, cb)` 接收。
 *
 * @param targetOrigin 该应用 iframe 的确切 origin（`sagent<slot>://localhost`）。
 *   postMessage 必须定向到这个 origin，不能用 `"*"`——否则若该 iframe 因某种原因
 *   导航去了别的 origin，宿主消息会被泄漏给非预期的接收方。
 */
export function subscribeUiEmit(appId: string, frame: Window, targetOrigin: string) {
  return listen(`ui-emit:${appId}`, (ev) => {
    const p = ev.payload as { event: string; payload: unknown };
    frame.postMessage({ __superagent_host: true, event: p.event, payload: p.payload }, targetOrigin);
  });
}
