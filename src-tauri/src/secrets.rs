const SERVICE: &str = "super-agent-os";
const PROVIDERS: &[&str] = &["anthropic", "openai", "google"];

/// provider → 注入子进程时使用的环境变量名；未知 provider 返回 None。
pub fn env_var_for(provider: &str) -> Option<&'static str> {
    match provider {
        "anthropic" => Some("ANTHROPIC_API_KEY"),
        "openai" => Some("OPENAI_API_KEY"),
        "google" => Some("GEMINI_API_KEY"),
        _ => None,
    }
}

fn entry(provider: &str) -> Result<keyring::Entry, String> {
    keyring::Entry::new(SERVICE, provider).map_err(|e| format!("keychain 打开失败：{e}"))
}

/// 把某个 provider 的 API Key 写入系统钥匙串（不落盘文件）。
#[tauri::command]
pub fn set_api_key(provider: String, key: String) -> Result<(), String> {
    if env_var_for(&provider).is_none() {
        return Err(format!("不支持的 provider：{provider}"));
    }
    entry(&provider)?
        .set_password(&key)
        .map_err(|e| format!("keychain 写入失败：{e}"))
}

/// 查询某个 provider 是否已配置 Key（不返回 Key 本身）。
#[tauri::command]
pub fn has_api_key(provider: String) -> Result<bool, String> {
    match entry(&provider)?.get_password() {
        Ok(_) => Ok(true),
        Err(keyring::Error::NoEntry) => Ok(false),
        Err(e) => Err(format!("keychain 读取失败：{e}")),
    }
}

/// 从系统钥匙串删除某个 provider 的 Key；本就不存在视为成功（幂等）。
#[tauri::command]
pub fn clear_api_key(provider: String) -> Result<(), String> {
    match entry(&provider)?.delete_credential() {
        Ok(()) => Ok(()),
        Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(format!("keychain 删除失败：{e}")),
    }
}

/// 读出所有已配置 provider 的 Key，组装成 spawn 子进程用的环境变量对（供 Task 11 注入 pi）。
/// 单个 provider 读取失败（未配置/钥匙串异常）静默跳过，不影响其余 provider。
pub fn key_env_pairs() -> Vec<(String, String)> {
    let mut pairs = Vec::new();
    for p in PROVIDERS {
        if let Ok(entry) = entry(p) {
            if let Ok(key) = entry.get_password() {
                if let Some(env) = env_var_for(p) {
                    pairs.push((env.to_string(), key));
                }
            }
        }
    }
    pairs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_provider_to_env_var() {
        assert_eq!(env_var_for("anthropic"), Some("ANTHROPIC_API_KEY"));
        assert_eq!(env_var_for("openai"), Some("OPENAI_API_KEY"));
        assert_eq!(env_var_for("google"), Some("GEMINI_API_KEY"));
        assert_eq!(env_var_for("unknown"), None);
    }
}
