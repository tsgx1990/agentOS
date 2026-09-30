import { invoke } from "@tauri-apps/api/core";

/**
 * 某个已装应用当前实际所处的安全态，与 Rust `lib.rs::SandboxStatus`
 * （Serialize，无 rename）对齐，序列化即原字段名。
 */
export type SandboxStatus = {
  sandboxed: boolean;
  platform: string;
  restricted: boolean;
};

/**
 * 调用后端 `app_sandbox_status` 查询某应用是否被 OS 级 L2 沙盒（`sandbox-exec` 等）
 * 真的包住，以及是否处于 P1 受限锁定（untrusted 且无沙盒兜底）。
 */
export function getSandboxStatus(appId: string): Promise<SandboxStatus> {
  return invoke("app_sandbox_status", { appId });
}
