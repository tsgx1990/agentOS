export const EVENTS = {
  delta: "assistant-delta",
  done: "assistant-done",
  error: "agent-error",
  retry: "retry-status",
  dormant: "app-dormant",
} as const;

export type AgentError = { kind: string; message: string };
export type RetryStatus = { attempt: number; max: number; delayMs: number };

/** 空闲超时回收事件（负载与 Rust 侧一致，snake_case）。 */
export type DormantEvent = { app_id: string; idle_secs: number; freed_rss_bytes: number | null };
