import { invoke } from "@tauri-apps/api/core";

/**
 * 市场索引条目：字段与 Rust `market::MarketEntry`（Serialize，无 rename）对齐，
 * 序列化即 snake_case。
 */
export type MarketEntry = {
  name: string;
  display_name: string;
  version: string;
  category: string;
  icon: string | null;
  description: string;
  /** 安装源：v1 demo 里是内置样例名（走 install_builtin_sample）；真实第三方条目为 git/路径；
   * 技能条目（`kind === "skill"`）本地安装时指向一个技能目录（`SKILL.md` 所在处）。 */
  source: string;
  /** 人话权限摘要，装前给用户看。 */
  permissions: string[];
  /**
   * 条目类型（P6-B Task 6 新增字段）：`"app"`（应用，走既有 `install_builtin_sample`/
   * `preview_install`→`install_app` 安装流）或 `"skill"`（走 `skillMarketInstall`，见
   * `lib/skills.ts`）。Rust 侧 `#[serde(default = "kind_app")]`，永远有值，缺省 `"app"`。
   */
  kind: string;
  /** 技能条目的下载地址；本地技能条目（`source` 直接可用）为 `null`。 */
  download_url: string | null;
  /** 技能 zip 的期望 sha256（十六进制），`download_url` 非空时必填；否则 `null`。 */
  sha256: string | null;
  /** 展示用字节数，仅供市场页参考，不参与任何校验；无声明时为 `null`。 */
  size: number | null;
  /** 条目作者，无声明时为 `null`。 */
  author: string | null;
};

/** 拉取精选市场索引（缺省读内置 demo）。 */
export function fetchMarketIndex(source?: string): Promise<MarketEntry[]> {
  return invoke("market_fetch_index", { source: source ?? null });
}

/**
 * 安装一个市场条目。v1 的 demo 市场条目全是内置白名单样例（第一方），走
 * `install_builtin_sample`（trusted，服务端白名单校验 + resource_dir 解析）。
 * 真实第三方条目（source 为 git/路径）的第三方确认安装流留待手工里程碑接
 * `preview_install`/`install_app`。
 */
export function installMarketEntry(entry: MarketEntry): Promise<unknown> {
  return invoke("install_builtin_sample", { name: entry.source });
}

/**
 * 发布一个已装应用：后端把它导出到 `published/<app_id>/` + 写入 `published/index.json`，
 * 返回生成的市场条目。真实推送到 GitHub 精选市场仓库是手工里程碑（凭据边界）。
 */
export function publishApp(appId: string): Promise<MarketEntry> {
  return invoke("publish_app", { appId });
}
