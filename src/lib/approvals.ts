import { invoke } from "@tauri-apps/api/core";

/**
 * 一条暂存待批的写调用，与 Rust `approvals::StagedCall`（Serialize，无 rename）
 * 对齐，序列化即 snake_case。`created_at` 是 unix 秒。
 */
export type StagedCall = {
  id: string;
  app_id: string;
  server: string;
  tool: string;
  args: unknown;
  created_at: number;
};

/** `respond_staged` 单条结果的裁决，与 Rust `notifications::StagedOutcome.verdict` 对齐。 */
export type StagedVerdict = "executed" | "rejected" | "missing" | "error";

/**
 * 终审 Important 4：`verdict === "rejected"` 的成因细分，与 Rust
 * `notifications::StagedOutcome.reason` 对齐——`"user"` 是用户自己点了拒绝，
 * `"unauthorized"` 是验收前重新鉴权失败（该应用可能已被卸载/降权/连接器被
 * 删除）。`executed`/`missing`/`error` 三种 verdict 下恒为 `null`。
 */
export type StagedRejectReason = "user" | "unauthorized";

/**
 * `respond_staged` 批量验收的单条结果，与 Rust `notifications::StagedOutcome`
 * 对齐——逐条独立处理，部分成功是正常结果，不是异常。
 */
export type StagedOutcome = {
  id: string;
  verdict: StagedVerdict;
  delivered: boolean;
  result?: unknown;
  error?: string | null;
  reason?: StagedRejectReason | null;
};

/**
 * 一条「总是允许」放行规则，与 Rust `approvals::ApprovalRule`（Serialize，
 * 无 rename）对齐；`(app_id, server, tool)` 三元组精确作用域。
 */
export type ApprovalRule = {
  app_id: string;
  server: string;
  tool: string;
  created_at: number;
};

/** 调用后端 `list_staged_calls` 列出当前暂存待批的写调用，`appId` 省略时列全部。 */
export function listStagedCalls(appId?: string): Promise<StagedCall[]> {
  return invoke("list_staged_calls", { appId });
}

/**
 * 调用后端 `respond_staged` 批量验收暂存调用——`allow` 决定允许/拒绝，
 * `always` 为 `true` 时额外落一条「总是允许」放行规则（`(app_id, server, tool)`）。
 */
export function respondStaged(ids: string[], allow: boolean, always: boolean): Promise<StagedOutcome[]> {
  return invoke("respond_staged", { ids, allow, always });
}

/** 调用后端 `list_approval_rules` 列出当前放行规则，`appId` 省略时列全部。 */
export function listApprovalRules(appId?: string): Promise<ApprovalRule[]> {
  return invoke("list_approval_rules", { appId });
}

/** 调用后端 `revoke_approval_rule` 撤销一条放行规则；返回是否真的删掉了一条。 */
export function revokeApprovalRule(appId: string, server: string, tool: string): Promise<boolean> {
  return invoke("revoke_approval_rule", { appId, server, tool });
}
