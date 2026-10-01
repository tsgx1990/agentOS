import { invoke } from "@tauri-apps/api/core";

/** 与 Rust `resources::GroupUsage` 对齐（无 rename，snake_case）。 */
export type GroupUsage = {
  label: string;
  root_pids: number[];
  proc_count: number;
  rss_bytes: number;
  cpu_percent: number;
};

/** 与 Rust `resources::AppUsage` 对齐。时间戳为 Unix 秒。 */
export type AppUsage = {
  app_id: string;
  usage: GroupUsage;
  background_sessions: number;
  opened_at: number | null;
  last_activity_at: number | null;
  idle_secs: number | null;
  in_turn: boolean;
  /** 不会被回收的原因（中文短语）；null 表示会被回收。 */
  exempt: string | null;
};

/** 与 Rust `resources::ResourceReport` 对齐。 */
export type ResourceReport = {
  sampled_at: number;
  cpu_ready: boolean;
  host: GroupUsage;
  main: GroupUsage | null;
  apps: AppUsage[];
  mcp_servers: GroupUsage[];
  other: GroupUsage;
  total_rss_bytes: number;
  total_cpu_percent: number;
  total_proc_count: number;
};

/** 与 Rust `idle::IdlePolicy` 对齐。 */
export type IdlePolicy = {
  enabled: boolean;
  timeout_secs: number;
  exempt_apps: string[];
};

/** 与 Rust `disk::AppDisk` 对齐。 */
export type AppDisk = {
  app_id: string;
  sessions_bytes: number;
  data_bytes: number;
  agent_home_bytes: number;
  /** 只按实际字节：会话文件是否超过阈值。 */
  sessions_over_threshold: boolean;
  /** 该应用的目录层级过深，统计不完整（数值偏小）。 */
  incomplete: boolean;
};

/** 与 Rust `disk::DiskReport` 对齐。 */
export type DiskReport = {
  root_bytes: number;
  audit_bytes: number;
  notifications_bytes: number;
  maker_staging_bytes: number;
  main_sessions_bytes: number;
  /** 有目录层级过深而没统计到（数值偏小）。 */
  incomplete: boolean;
  apps: AppDisk[];
  threshold_bytes: number;
};

/** 与 Rust `disk::ClearReport` 对齐。 */
export type ClearReport = {
  freed_bytes: number;
  removed_files: number;
  removed_drafts: number;
  kept_active: string[];
  /** 因目录是符号链接 / 被换链而拒绝处理的目录。 */
  refused: string[];
};

export function resourceReport(): Promise<ResourceReport> {
  return invoke("resource_report");
}
export function getIdlePolicy(): Promise<IdlePolicy> {
  return invoke("get_idle_policy");
}
export function setIdlePolicy(policy: IdlePolicy): Promise<void> {
  return invoke("set_idle_policy", { policy });
}
export function listDormantApps(): Promise<string[]> {
  return invoke("list_dormant_apps");
}
export function diskReport(): Promise<DiskReport> {
  return invoke("disk_report");
}
export function clearCaches(appId?: string): Promise<ClearReport> {
  return invoke("clear_caches", { appId: appId ?? null });
}

/** 字节数 → 「512 B」「1.5 MB」；KB/MB/GB 一位小数。 */
export function formatBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  const units = ["KB", "MB", "GB", "TB"];
  let v = n / 1024;
  let i = 0;
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024;
    i += 1;
  }
  return `${v.toFixed(1)} ${units[i]}`;
}

/** 秒数 → 「59 秒」「3 分钟」「1 小时 5 分钟」。 */
export function formatDuration(secs: number): string {
  const s = Math.max(0, Math.floor(secs));
  if (s < 60) return `${s} 秒`;
  const m = Math.floor(s / 60);
  if (m < 60) return `${m} 分钟`;
  const h = Math.floor(m / 60);
  const rest = m % 60;
  return rest === 0 ? `${h} 小时` : `${h} 小时 ${rest} 分钟`;
}
