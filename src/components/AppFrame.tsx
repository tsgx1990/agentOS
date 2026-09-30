import { useEffect, useRef, useState } from "react";
import "./AppFrame.css";
import { subscribeUiEmit } from "../lib/bridge";
import { PublishButton } from "./PublishButton";
import { CapabilityPanel } from "./CapabilityPanel";
import { appCapabilities, type CapabilityReport } from "../lib/registry";
import { getSandboxStatus } from "../lib/sandbox";
import { listStagedCalls } from "../lib/approvals";

/** 待批数徽标轮询间隔（P6-C Task7：应用头部「权限」按钮旁的暂存调用数）。 */
const STAGED_POLL_MS = 5000;

/**
 * 中区已打开应用的容器：把该应用渲染进它自己的自定义 scheme iframe
 * （`sagent{slot}://localhost/index.html`，槽位分配见 src-tauri/src/scheme.rs 的
 * `SlotPool`，协议处理器见 src-tauri/src/lib.rs 的 `serve_app_file`）。
 * sandbox 为 `allow-scripts allow-same-origin`：裸 `allow-scripts`（不加
 * `allow-same-origin`）会把 iframe 文档强制降级为 opaque origin（序列化为
 * `"null"`），这会让基于 origin 的桥路由全线失效——`BRIDGE_JS` 里
 * `location.origin.startsWith('sagent')` 恒假、宿主 `postMessage(msg,
 * "sagent{slot}://localhost")` 因目标 origin 不匹配而永不送达、应用→宿主消息的
 * `event.origin` 也恒为 `"null"` 永远匹配不上中继的期望 origin。加上
 * `allow-same-origin` 后，每个应用仍然拥有自己独立、无特权的
 * `sagent{slot}://localhost` origin（见 src-tauri/src/scheme.rs 的
 * `SlotPool`/`scheme_name`），iframe 保留其真实 per-app origin 供桥按 origin
 * 路由；跨应用隔离靠的是不同 scheme/origin 本身（而非 opaque origin），宿主从不
 * 向 iframe 注入 Tauri API、且该 scheme 未被授予任何 Tauri capability，故仍然
 * 安全，不获得访问宿主或其它应用 DOM 的能力。
 * 挂载时订阅该应用的 `ui-emit:<appId>` 事件并定向转发进它自己的 iframe origin，
 * 卸载时取消订阅（见 `../lib/bridge.ts` 的 `subscribeUiEmit`）。
 *
 * 头部带「← 返回」（`onClose`，由 Shell 接线到 `close_app` + 清空 `open` + 刷新应用列表，
 * 否则应用打开后永远无法离开，宿主 gate 名额也永远不释放）与「卸载」（`onUninstall`，由
 * Shell 打开 `UninstallDialog`）两个控制，样式走 C4a token（见 AppFrame.css）。
 *
 * Task10：头部再加「权限」诊断按钮——点开一个绝对定位的弹层，首次打开才惰性拉
 * `appCapabilities(appId)`（诊断报告）+ `getSandboxStatus(appId)`（该 app 是否真被
 * OS 沙盒包住），渲染进 `<CapabilityPanel>`；这是运行时视角，与 `InstallDialog`
 * 装前的 `preview_install.capabilities` 共用同一个渲染组件，两处呈现同一张真相表。
 *
 * P6-C Task7：「权限」按钮旁再加一枚待批数徽标——挂载即拉一次
 * `listStagedCalls(appId)`（该应用当前暂存待批的写调用数），此后每 5 秒轮询
 * 一次（`STAGED_POLL_MS`），数字随审批中心的验收/拒绝操作自然收敛；数量为 0
 * 时不渲染徽标（没有需要提醒的东西）。轮询失败（如应用已关闭、命令报错）静默
 * 忽略，不打断头部渲染——同 `toggleCaps` 对 `appCapabilities`/`getSandboxStatus`
 * 失败的处理哲学（catch 后落一个安全的空/假值）。
 */
export function AppFrame({
  slot,
  appId,
  onClose,
  onUninstall,
}: {
  slot: number;
  appId: string;
  onClose: () => void;
  onUninstall: () => void;
}) {
  const ref = useRef<HTMLIFrameElement>(null);
  const [capsOpen, setCapsOpen] = useState(false);
  const [reports, setReports] = useState<CapabilityReport[] | null>(null);
  const [sandboxed, setSandboxed] = useState(false);
  const [pendingCount, setPendingCount] = useState(0);

  useEffect(() => {
    let alive = true;
    const poll = () => {
      listStagedCalls(appId)
        .then((calls) => { if (alive) setPendingCount(calls.length); })
        .catch(() => { /* 轮询失败静默忽略，不打断头部渲染 */ });
    };
    poll();
    const timer = setInterval(poll, STAGED_POLL_MS);
    return () => { alive = false; clearInterval(timer); };
  }, [appId]);

  useEffect(() => {
    const w = ref.current?.contentWindow;
    if (!w) return;
    const targetOrigin = `sagent${slot}://localhost`;
    const un = subscribeUiEmit(appId, w, targetOrigin);
    return () => { un.then((f) => f()); };
  }, [appId, slot]);

  function toggleCaps() {
    const next = !capsOpen;
    setCapsOpen(next);
    if (next && reports === null) {
      appCapabilities(appId).then(setReports).catch(() => setReports([]));
      getSandboxStatus(appId).then((s) => setSandboxed(s.sandboxed)).catch(() => setSandboxed(false));
    }
  }

  return (
    <div className="app-frame-wrap">
      <div className="app-frame-header">
        <button className="app-frame-back" onClick={onClose}>← 返回</button>
        <PublishButton appId={appId} />
        <button className="app-frame-caps" onClick={toggleCaps}>
          权限
          {pendingCount > 0 && <span className="app-frame-caps-badge">{pendingCount}</span>}
        </button>
        <button className="app-frame-uninstall" onClick={onUninstall}>卸载</button>
      </div>
      {capsOpen && (
        <div className="app-frame-caps-pop">
          {reports === null ? (
            <p className="app-frame-caps-loading">加载中…</p>
          ) : (
            <CapabilityPanel reports={reports} sandboxed={sandboxed} appId={appId} />
          )}
        </div>
      )}
      <iframe
        ref={ref}
        className="app-frame"
        title={appId}
        src={`sagent${slot}://localhost/index.html`}
        sandbox="allow-scripts allow-same-origin"
      />
    </div>
  );
}
