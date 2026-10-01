import { invoke } from "@tauri-apps/api/core";

/**
 * 某个 app（或主助手会话，`appId === "main"`，见 `lib.rs::app_usage` 文档）
 * 当前累计的 token 用量 + pi 报告的真实花费，与 Rust `usage::UsageResponse`
 * （Serialize，无 rename）对齐。
 *
 * P3 Task18 修复：`cost` 直接来自 pi 的 `get_session_stats`（`SessionStats.cost`），
 * 不再是旧版 `est_cost` 那种按固定单价常量估算的粗略值——旧实现依赖的
 * `agent_end.usage` 字段在真实 pi 里根本不存在，生产环境永远是 0。
 */
export type UsageResponse = {
  input: number;
  output: number;
  cost: number;
};

/** 调用后端 `app_usage` 查询该 app 的累计用量；传 `"main"` 查主助手会话。 */
export function appUsage(appId: string): Promise<UsageResponse> {
  return invoke("app_usage", { appId });
}

/** 与 Rust `usage::ModelUsageRow` 对齐：按 (应用, provider, 模型) 拆分的一行用量。 */
export type ModelUsageRow = {
  app_id: string;
  provider: string;
  model: string;
  input: number;
  output: number;
  cost: number;
};

/**
 * 按模型拆分的用量；不传 `appId` 返回所有应用。总量见 `appUsage`，总量减各模型
 * 之和就是「其他（工具 / 压缩）」。
 */
export function usageByModel(appId?: string): Promise<ModelUsageRow[]> {
  return invoke("usage_by_model", { appId: appId ?? null });
}
