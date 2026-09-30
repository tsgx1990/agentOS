import { useEffect, useState } from "react";
import "./NotificationCenter.css";
import { listNotifications, ackNotification, type Notification } from "../lib/notifications";

const KIND_LABELS: Record<string, string> = {
  task_result: "任务结果",
  confirm_request: "确认请求",
  update: "更新",
  app_notice: "应用通知",
};

/**
 * 通知中心：列出通知（`list_notifications`，时间/种类/应用/标题/正文）。
 * `confirm_request` 种类（`id` 即 confirmId，见 `notifications.rs` 模块文档）
 * 渲染一个「去审批中心」按钮（`onOpenApprovals`）——P6-C 起写确认走
 * `ApprovalCenter` 的暂存批量验收流（`list_staged_calls`/`respond_staged`），
 * 这里不再直接摆「允许/总是允许/拒绝」三按钮、不再调旧的 `respond_confirm`
 * （该命令仍在但只是兼容保留，见 `lib.rs` 命令文档）。
 * 其余种类（`task_result`/`update`）未读时渲染「标记已读」→ `ack_notification`。
 * 已读通知仍展示（视觉淡化），不再渲染操作按钮。空态给出提示文案。
 * 挂载点同 P2 `AuditView`：`SessionPanel` 常驻入口 → `Shell` 中区状态分支，
 * 见 task-17-brief 与 Shell.tsx。
 */
export function NotificationCenter({
  onClose,
  onOpenApprovals,
}: { onClose?: () => void; onOpenApprovals?: () => void } = {}) {
  const [items, setItems] = useState<Notification[] | null>(null);
  const [err, setErr] = useState<string | null>(null);

  const refresh = () => listNotifications({}).then(setItems).catch((e) => setErr(String(e)));
  useEffect(() => { refresh(); }, []);

  async function ack(id: string) {
    try { await ackNotification(id); refresh(); } catch (e) { setErr(String(e)); }
  }

  return (
    <div className="notification-center">
      <div className="notification-center-header">
        <h2>通知中心</h2>
        {onClose && <button onClick={onClose}>返回</button>}
      </div>

      {err && <p className="notification-err">{err}</p>}

      {items === null && !err && <p className="notification-hint">加载中…</p>}

      {items !== null && items.length === 0 && (
        <p className="notification-hint">还没有通知。任务结果与写操作确认请求会出现在这里。</p>
      )}

      {items !== null && items.length > 0 && (
        <div className="notification-list">
          {items.map((n) => (
            <div className={`notification-row${n.acked ? " acked" : ""}`} key={n.id}>
              <div className="notification-meta">
                <span className="notification-ts">{n.ts}</span>
                <span className="notification-app">{n.app_id}</span>
                <span className={`notification-kind notification-kind-${n.kind}`}>
                  {KIND_LABELS[n.kind] ?? n.kind}
                </span>
              </div>
              <div className="notification-title">{n.title}</div>
              <div className="notification-body">{n.body}</div>

              {!n.acked && n.kind === "confirm_request" && (
                <div className="notification-actions">
                  <button className="goto-approvals" onClick={() => onOpenApprovals?.()}>去审批中心</button>
                </div>
              )}

              {!n.acked && n.kind !== "confirm_request" && (
                <div className="notification-actions">
                  <button className="ack" onClick={() => ack(n.id)}>标记已读</button>
                </div>
              )}
            </div>
          ))}
        </div>
      )}
    </div>
  );
}
