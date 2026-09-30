import { invoke } from "@tauri-apps/api/core";

/**
 * 一条通知记录，与 Rust `notifications::Notification`（Serialize/Deserialize，
 * 无 rename）对齐，序列化即 snake_case。`kind` 取值见 `notifications.rs` 模块
 * 文档："task_result"（Task12 定时任务结果）/"confirm_request"（Task7 MCP 写
 * 操作待确认，`id` 即 confirmId）/"update"（预留）。
 */
export type Notification = {
  id: string;
  ts: string;
  kind: "task_result" | "confirm_request" | "update" | string;
  app_id: string;
  title: string;
  body: string;
  acked: boolean;
};

/**
 * `list_notifications` 的过滤条件，与 Rust `notifications::NotificationFilter`
 * 对齐；全部省略等价于不过滤，只按 `limit` 截断。
 */
export type NotificationFilter = {
  app_id?: string;
  kind?: string;
  acked?: boolean;
  limit?: number;
};

/** 调用后端 `list_notifications` 按过滤条件查询通知中心。 */
export function listNotifications(filter: NotificationFilter = {}): Promise<Notification[]> {
  return invoke("list_notifications", { filter });
}

/** 调用后端 `ack_notification` 把一条通知标记为已读。 */
export function ackNotification(id: string): Promise<void> {
  return invoke("ack_notification", { id });
}

/**
 * 调用后端 `respond_confirm` 响应一条 `confirm_request`（同时完成 ack 该通知 +
 * 续行/丢弃对应的挂起 MCP 写调用，见 `notifications.rs` 模块文档"confirmId 与
 * Notification.id 的关系"一节）：
 * - `allow=true, always=false` → 允许（仅此一次）；
 * - `allow=true, always=true`  → 总是允许（记住该 app+server+tool 偏好，此后
 *   同类写操作不再需要确认）；
 * - `allow=false, always=false` → 拒绝，不执行。
 * 返回值是被续行执行的那次 `tools/call` 结果（`allow=false` 或 confirmId 未知/
 * 已被消费过时为 `null`）。
 */
export function respondConfirm(confirmId: string, allow: boolean, always: boolean): Promise<unknown> {
  return invoke("respond_confirm", { confirmId, allow, always });
}
