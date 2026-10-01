const SERVICE: &str = "super-agent-os";

/// provider → 注入子进程时使用的环境变量名；不在目录里的 provider 返回 None。
pub fn env_var_for(provider: &str) -> Option<String> {
    crate::providers::native(provider).map(|p| p.env_var.to_string())
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
    let key = normalize_key(&key)?;
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

/// 清洗用户粘贴的 Key：去首尾空白；空或含内部空白一律拒绝。
pub fn normalize_key(raw: &str) -> Result<String, String> {
    let k = raw.trim();
    if k.is_empty() {
        return Err("API Key 不能为空".to_string());
    }
    if k.chars().any(char::is_whitespace) {
        return Err("API Key 中不应有空格或换行".to_string());
    }
    Ok(k.to_string())
}

/// 钥匙串里有没有该 provider 的 Key；钥匙串异常视为没有。
pub fn has_key(provider: &str) -> bool {
    read_key(provider).is_some()
}

/// 读出该 provider 的 Key；未配置或钥匙串异常返回 None。
pub fn read_key(provider: &str) -> Option<String> {
    entry(provider).ok()?.get_password().ok()
}

/// 对给定 provider id 逐个查 Key，组装成 spawn 子进程用的环境变量对。
/// 未配置（lookup 返回 None）或不在目录里的 provider 静默跳过。
pub fn key_env_pairs_with(
    ids: &[String],
    lookup: impl Fn(&str) -> Option<String>,
) -> Vec<(String, String)> {
    ids.iter()
        .filter_map(|id| Some((env_var_for(id)?, lookup(id)?)))
        .collect()
}

/// 读出所有已配置原生 provider 的 Key，组装成 spawn 子进程用的环境变量对。
pub fn key_env_pairs() -> Vec<(String, String)> {
    let ids: Vec<String> = crate::providers::NATIVE
        .iter()
        .map(|p| p.id.to_string())
        .collect();
    key_env_pairs_with(&ids, read_key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_provider_to_env_var() {
        let some = |s: &str| Some(s.to_string());
        assert_eq!(env_var_for("anthropic"), some("ANTHROPIC_API_KEY"));
        assert_eq!(env_var_for("openai"), some("OPENAI_API_KEY"));
        assert_eq!(env_var_for("google"), some("GEMINI_API_KEY"));
        assert_eq!(env_var_for("deepseek"), some("DEEPSEEK_API_KEY"));
        assert_eq!(env_var_for("kimi-coding"), some("KIMI_API_KEY"));
        assert_eq!(env_var_for("zai-coding-cn"), some("ZAI_CODING_CN_API_KEY"));
        assert_eq!(env_var_for("moonshotai-cn"), some("MOONSHOT_API_KEY"));
        assert_eq!(env_var_for("moonshotai"), None);
        assert_eq!(env_var_for("unknown"), None);
    }

    #[test]
    fn key_env_pairs_with_skips_unconfigured() {
        let ids: Vec<String> = ["anthropic", "openai", "deepseek"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let pairs = key_env_pairs_with(&ids, |id| {
            (id == "openai").then(|| "sk-test-fake".to_string())
        });
        assert_eq!(
            pairs,
            vec![("OPENAI_API_KEY".to_string(), "sk-test-fake".to_string())]
        );
    }

    #[test]
    fn normalize_key_trims_and_rejects_blank_or_inner_space() {
        assert_eq!(normalize_key("  sk-test-fake\n").unwrap(), "sk-test-fake");
        assert_eq!(normalize_key("   ").unwrap_err(), "API Key 不能为空");
        assert_eq!(normalize_key("").unwrap_err(), "API Key 不能为空");
        assert_eq!(
            normalize_key("sk-test fake").unwrap_err(),
            "API Key 中不应有空格或换行"
        );
        assert_eq!(
            normalize_key("sk-test\nfake").unwrap_err(),
            "API Key 中不应有空格或换行"
        );
    }
}
