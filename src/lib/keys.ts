import { invoke } from "@tauri-apps/api/core";

/**
 * P0 BYOK key 命令的薄前端封装（`src-tauri/src/secrets.rs`）。Key 只存在系统
 * 钥匙串，从不落盘文件；这里只是把裸 `invoke` 调用收拢成有类型的函数，供
 * `Shell`（首启检测）与 `OnboardingWizard`（保存）共用，避免各自重复裸调。
 */

/** 查询某 provider 是否已在系统钥匙串配置 Key（`secrets::has_api_key`）。 */
export function hasApiKey(provider: string): Promise<boolean> {
  return invoke("has_api_key", { provider });
}

/** 把某 provider 的 API Key 写入系统钥匙串（`secrets::set_api_key`）。失败时抛出 BYOK 错误文案（如「keychain 写入失败：…」），调用方 catch 后用 `String(e)` 展示。 */
export function setApiKey(provider: string, key: string): Promise<void> {
  return invoke("set_api_key", { provider, key });
}

/** 清除某 provider 的 API Key（`secrets::clear_api_key`）；本来就没有也算成功。 */
export function clearApiKey(provider: string): Promise<void> {
  return invoke("clear_api_key", { provider });
}
