import { invoke } from "@tauri-apps/api/core";

/**
 * 模型 provider 相关命令的薄前端封装与类型。字段 snake_case，与 Rust 结构体
 * （`providers.rs` / `model_overrides.rs` / `probe.rs`）逐一对应；命令参数在 JS
 * 侧写 camelCase（Tauri 自动映射）。密钥不经过这里：见 `keys.ts`。
 */

export type ApiKind =
  | "openai-completions"
  | "openai-responses"
  | "anthropic-messages"
  | "google-generative-ai";

export interface ProviderInfo {
  id: string;
  display: string;
  native: boolean;
  /** 自定义 provider 为 null */
  region: "cn" | "intl" | null;
  /** 系统钥匙串里有没有密钥 */
  configured: boolean;
  /** 原生为 null */
  base_url: string | null;
  api: ApiKind | null;
  /** 预设模型，首个为默认 */
  presets: string[];
}

export interface CustomProvider {
  id: string;
  display: string;
  base_url: string;
  api: ApiKind;
  models: string[];
}

export interface CustomPreset {
  suggested_id: string;
  display: string;
  base_url: string;
  api: ApiKind;
  models: string[];
}

export interface ModelChoice {
  provider: string;
  model: string;
}

export interface EffectiveModel {
  provider: string | null;
  model: string | null;
  source: "app" | "global" | "manifest" | "none";
}

export interface AppModelRow {
  app_id: string;
  manifest_model: string | null;
  app_override: ModelChoice | null;
  effective: EffectiveModel;
}

export interface ModelSettingsView {
  global: ModelChoice | null;
  apps: AppModelRow[];
}

export interface ProbeReport {
  ok: boolean;
  /** ok / no_key / invalid_key / not_found / rate_limited / network / timeout / other */
  kind: string;
  latency_ms: number;
  provider: string;
  model: string;
  /** 中文人话 */
  message: string;
  /** pi 原始错误（已脱敏、已截断），可能为空串 */
  detail: string;
}

export const TEST_COST_HINT = "会发送一条极短请求，产生少量费用";
export const RATE_LIMIT_HINT = "偶发限流可稍后重试";

export const SOURCE_LABEL: Record<EffectiveModel["source"], string> = {
  app: "应用覆盖",
  global: "全局默认",
  manifest: "清单默认",
  none: "未设置",
};

export function listProviders(): Promise<ProviderInfo[]> {
  return invoke("list_providers");
}

export function saveCustomProvider(provider: CustomProvider): Promise<void> {
  return invoke("save_custom_provider", { provider });
}

export function removeCustomProvider(id: string): Promise<void> {
  return invoke("remove_custom_provider", { id });
}

export function customProviderPresets(): Promise<CustomPreset[]> {
  return invoke("custom_provider_presets");
}

/** 经 pi 发一条极短请求验证密钥与端点；会产生少量费用。`model` 省略则用预设首个。 */
export function testProvider(provider: string, model?: string): Promise<ProbeReport> {
  return invoke("test_provider", { provider, model: model ?? null });
}

export function getModelSettings(): Promise<ModelSettingsView> {
  return invoke("get_model_settings");
}

export function setGlobalModel(choice: ModelChoice | null): Promise<void> {
  return invoke("set_global_model", { choice });
}

export function setAppModel(appId: string, choice: ModelChoice | null): Promise<void> {
  return invoke("set_app_model", { appId, choice });
}
