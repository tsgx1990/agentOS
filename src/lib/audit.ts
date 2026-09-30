import { invoke } from "@tauri-apps/api/core";

/**
 * 一条宿主级审计记录，与 Rust `audit::Entry`（Serialize，无 rename）对齐，
 * 序列化即 snake_case。`args` 是已脱敏、JSON 字符串化的工具参数。
 */
export type AuditEntry = {
  ts: string;
  app_id: string;
  tool: string;
  args: string;
  verdict: string;
};

/**
 * `list_audit` 的过滤条件，与 Rust `audit::AuditFilter` 对齐；三个字段全省略
 * 等价于不过滤，只按 `limit` 截断（`limit` 省略即不截断）。
 */
export type AuditFilter = {
  app_id?: string;
  tool?: string;
  limit?: number;
};

/** 调用后端 `list_audit` 查询宿主级审计日志（各应用触发过的工具调用）。 */
export function listAudit(filter: AuditFilter = {}): Promise<AuditEntry[]> {
  return invoke("list_audit", { filter });
}
