export const EVENTS = {
  delta: "assistant-delta",
  done: "assistant-done",
  error: "agent-error",
  retry: "retry-status",
} as const;

export type AgentError = { kind: string; message: string };
export type RetryStatus = { attempt: number; max: number; delayMs: number };
