import { invoke } from "@tauri-apps/api/core";

/**
 * 已装应用记录：字段与 Rust `registry::InstalledApp`（Serialize，无 rename）
 * 对齐，序列化即 snake_case。
 */
export type InstalledApp = {
  app_id: string;
  name: string;
  version: string;
  display_name: string;
  category: string;
  icon: string | null;
  trusted: boolean;
  domains: string[];
};

/** 左导航类目：key 对应 InstalledApp.category 的取值 */
export const CATEGORIES = [
  { key: "life", label: "生活", dot: "var(--dot-life)" },
  { key: "info", label: "信息", dot: "var(--dot-info)" },
  { key: "create", label: "创作", dot: "var(--dot-create)" },
  { key: "automation", label: "自动化", dot: "var(--dot-automation)" },
  { key: "maker", label: "Maker", dot: "var(--dot-maker)" },
] as const;

/** 调用后端 `list_apps` 拉取已装应用列表 */
export function listApps(): Promise<InstalledApp[]> {
  return invoke("list_apps");
}

/** 按类目统计已装应用数量 */
export function categoryCounts(apps: InstalledApp[]): Record<string, number> {
  const counts: Record<string, number> = {};
  for (const a of apps) counts[a.category] = (counts[a.category] ?? 0) + 1;
  return counts;
}

/**
 * 一条能力被强制的方式：与 Rust `capability::Enforcement`（`#[serde(rename_all
 * = "snake_case")]`）对齐。
 */
export type Enforcement = "launch" | "sandbox" | "host_method" | "install_hook" | "ui_csp";

/**
 * 某个应用对某项能力的诊断报告：与 Rust `capability::CapabilityReport`
 * （Serialize，无 rename）对齐，序列化即 snake_case 字段名。
 */
export type CapabilityReport = {
  key: string;
  declared: boolean;
  human: string[];
  enforcement: Enforcement[];
  tools: string[];
  /** F1（review）：declared 为真但该能力 launch() 报错时的原因；serde 的
   * `Option<String>::None` 序列化为 `null`。 */
  error: string | null;
};

/** `Enforcement` 枚举值 → 面板上展示的中文标签 */
export const ENFORCEMENT_LABELS: Record<Enforcement, string> = {
  launch: "启动时注入",
  sandbox: "OS 沙盒",
  host_method: "宿主复核",
  install_hook: "安装时登记",
  ui_csp: "界面 CSP",
};

/** 调用后端 `app_capabilities` 拉取某已装应用的完整能力诊断报告 */
export function appCapabilities(appId: string): Promise<CapabilityReport[]> {
  return invoke("app_capabilities", { appId });
}
