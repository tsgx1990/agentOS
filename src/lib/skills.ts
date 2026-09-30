import { invoke } from "@tauri-apps/api/core";

/**
 * 技能 frontmatter 解析结果 + 目录扫描得出的 `has_scripts`，与 Rust
 * `skills::SkillMeta`（Serialize，无 rename）对齐，序列化即 snake_case。
 */
export type SkillMeta = {
  id: string;
  name: string;
  description: string;
  license: string | null;
  compatibility: string | null;
  allowed_tools: string[];
  disable_model_invocation: boolean;
  has_scripts: boolean;
};

/** 危险模式命中严重度，与 Rust `skills::Severity`（`rename_all = "lowercase"`）对齐。 */
export type Severity = "high" | "medium";

/** 一条危险模式命中记录，与 Rust `skills::Finding` 对齐。 */
export type Finding = {
  severity: Severity;
  rule: string;
  file: string;
  line: number;
  excerpt: string;
};

/** 技能目录扫描报告，与 Rust `skills::ScanReport` 对齐。 */
export type ScanReport = {
  has_scripts: boolean;
  script_files: string[];
  findings: Finding[];
  total_bytes: number;
  file_count: number;
};

/** 技能来源种类，与 Rust `skills::SkillSourceKind`（`rename_all = "lowercase"`）对齐。 */
export type SkillSourceKind = "local" | "builtin" | "market" | "maker";

/** 一个已装技能的来源，与 Rust `skills::SkillSource` 对齐。 */
export type SkillSource = {
  kind: SkillSourceKind;
  url: string | null;
  sha256: string | null;
};

/** 一条已装技能记录，与 Rust `skills::InstalledSkill` 对齐。 */
export type InstalledSkill = {
  meta: SkillMeta;
  source: SkillSource;
  trusted: boolean;
  installed_at: number;
  scan: ScanReport;
};

/**
 * 本地导入/市场安装前的预览，与 Rust `lib.rs::SkillPreview` 对齐——`trusted`
 * 恒为 `false`（预览阶段没有"这次会被信任"这一说，见该结构体的 Rust 文档）。
 */
export type SkillPreview = {
  meta: SkillMeta;
  scan: ScanReport;
  trusted: boolean;
};

/** 调用后端 `list_skills` 拉取当前已装技能全量列表。 */
export function listSkills(): Promise<InstalledSkill[]> {
  return invoke("list_skills");
}

/** 调用后端 `preview_skill`：走安装门但不落盘，供 `SkillInstallDialog` 展示。 */
export function previewSkill(path: string): Promise<SkillPreview> {
  return invoke("preview_skill", { path });
}

/** 调用后端 `install_skill_from_path`：本地目录导入一个技能。 */
export function installSkillFromPath(path: string): Promise<InstalledSkill> {
  return invoke("install_skill_from_path", { path });
}

/** 调用后端 `uninstall_skill`；返回是否真的卸载了一个技能。 */
export function uninstallSkill(id: string): Promise<boolean> {
  return invoke("uninstall_skill", { id });
}

/**
 * 调用后端 `skill_grants` 列出某应用**已被授予**的技能——与 Rust
 * `SkillStore::grants_for` 一致，只返回真正存在授予记录的技能，不是"全部技能 +
 * 是否授予"的标记表。`(InstalledSkill, bool)` 元组经 serde_json 序列化为
 * 二元数组 `[InstalledSkill, boolean]`，第二个元素是这条授予的启停状态。
 */
export type SkillGrantEntry = [InstalledSkill, boolean];

export function skillGrants(appId: string): Promise<SkillGrantEntry[]> {
  return invoke("skill_grants", { appId });
}

/** 调用后端 `grant_skill`：把某技能授予某应用。 */
export function grantSkill(appId: string, skillId: string): Promise<void> {
  return invoke("grant_skill", { appId, skillId });
}

/** 调用后端 `revoke_skill`：撤销某应用对某技能的授予；返回是否真的删掉了一条。 */
export function revokeSkill(appId: string, skillId: string): Promise<boolean> {
  return invoke("revoke_skill", { appId, skillId });
}

/** 调用后端 `set_skill_enabled`：对一条已存在的授予启停（不隐式创建授予）。 */
export function setSkillEnabled(appId: string, skillId: string, enabled: boolean): Promise<void> {
  return invoke("set_skill_enabled", { appId, skillId, enabled });
}

/**
 * 调用后端 `skill_market_install`：安装一条市场索引里 `kind === "skill"` 的
 * 条目——本地条目直接装，带 `download_url` 的条目走下载 + sha256 校验 + 解包，
 * 两条路径固定 `trusted=false`。`entry` 类型见 `lib/market.ts::MarketEntry`。
 */
export function skillMarketInstall(entry: import("./market").MarketEntry): Promise<InstalledSkill> {
  return invoke("skill_market_install", { entry });
}

// ---------------------------------------------------------------------------
// Maker 技能待确认——**假定签名**：批次 D（Task 7）截至本任务开工时尚未落地
// （`src-tauri/src/lib.rs` 的 `generate_handler!` 里没有 `list_pending_skill_installs`/
// `skill_respond_install_confirm`，`maker.rs` 也没有任何 `skill` 相关分支），
// 这里按简报给出的命令名 + 参照既有 `list_pending_installs`/
// `maker_respond_install_confirm`（应用侧同类流程，见 `MakerInstallConfirm.tsx`）
// 的形状写出前端封装：`confirm_id` + 复用 `SkillPreview` 的 `meta`/`scan`（技能
// 安装确认需要展示 frontmatter 摘要 + 扫描发现，与 `SkillInstallDialog` 呈现的
// 是同一张信息，比"只给一句权限人话"更贴近技能确认的实际需要）。若批次 D 落地
// 后实际返回形状不同，只需改这一处类型 + 两个函数体，`SkillPendingConfirm.tsx`
// 的渲染逻辑复用 `SkillMetaSummary` 不受影响。
// ---------------------------------------------------------------------------

/** 一条待确认的 Maker 生成技能安装（假定签名，见上）。 */
export type PendingSkillInstall = {
  confirm_id: string;
  meta: SkillMeta;
  scan: ScanReport;
};

/** 假定后端命令 `list_pending_skill_installs`：列出当前所有待确认的技能安装。 */
export function listPendingSkillInstalls(): Promise<PendingSkillInstall[]> {
  return invoke("list_pending_skill_installs");
}

/** 假定后端命令 `skill_respond_install_confirm`：批准/拒绝一条待确认技能安装。 */
export function skillRespondInstallConfirm(confirmId: string, allow: boolean): Promise<unknown> {
  return invoke("skill_respond_install_confirm", { confirmId, allow });
}
