//! 原生 provider 目录：id 与环境变量名逐字照抄 pi 0.84.4 `docs/providers.md` 的表，
//! 并由 tests/fixtures/pi-env-map.json（从同版本 pi 二进制的 envMap 抄出）钉住。
use crate::paths::DataLayout;
use crate::secrets;
use tauri::Manager;

pub const PI_CATALOG_VERSION: &str = "0.84.4";

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Region {
    Cn,
    Intl,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ApiKind {
    #[serde(rename = "openai-completions")]
    OpenAiCompletions,
    #[serde(rename = "openai-responses")]
    OpenAiResponses,
    #[serde(rename = "anthropic-messages")]
    AnthropicMessages,
    #[serde(rename = "google-generative-ai")]
    GoogleGenerativeAi,
}

pub struct NativeProvider {
    pub id: &'static str,
    pub display: &'static str,
    pub env_var: &'static str,
    pub region: Region,
    /// 首个为默认；pi 0.84.4 目录快照，可能随 pi 升级过期。
    pub presets: &'static [&'static str],
}

pub const NATIVE: &[NativeProvider] = &[
    NativeProvider {
        id: "anthropic",
        display: "Anthropic（Claude）",
        env_var: "ANTHROPIC_API_KEY",
        region: Region::Intl,
        presets: &["claude-sonnet-5", "claude-opus-5", "claude-haiku-4-5"],
    },
    NativeProvider {
        id: "openai",
        display: "OpenAI",
        env_var: "OPENAI_API_KEY",
        region: Region::Intl,
        presets: &["gpt-5.5", "gpt-5.4-mini", "gpt-5.3-codex"],
    },
    NativeProvider {
        id: "google",
        display: "Google Gemini",
        env_var: "GEMINI_API_KEY",
        region: Region::Intl,
        presets: &[
            "gemini-3.1-pro-preview",
            "gemini-3.5-flash",
            "gemini-2.5-pro",
        ],
    },
    NativeProvider {
        id: "openrouter",
        display: "OpenRouter",
        env_var: "OPENROUTER_API_KEY",
        region: Region::Intl,
        presets: &[
            "anthropic/claude-sonnet-5",
            "~anthropic/claude-sonnet-latest",
            "~google/gemini-flash-latest",
            "~deepseek/deepseek-v4-flash-latest",
        ],
    },
    NativeProvider {
        id: "groq",
        display: "Groq",
        env_var: "GROQ_API_KEY",
        region: Region::Intl,
        presets: &[
            "openai/gpt-oss-120b",
            "llama-3.3-70b-versatile",
            "qwen/qwen3.6-27b",
        ],
    },
    NativeProvider {
        id: "xai",
        display: "xAI（Grok）",
        env_var: "XAI_API_KEY",
        region: Region::Intl,
        presets: &["grok-4.6", "grok-4.5"],
    },
    NativeProvider {
        id: "mistral",
        display: "Mistral",
        env_var: "MISTRAL_API_KEY",
        region: Region::Intl,
        presets: &[
            "mistral-large-latest",
            "devstral-latest",
            "mistral-small-latest",
        ],
    },
    NativeProvider {
        id: "deepseek",
        display: "DeepSeek 深度求索",
        env_var: "DEEPSEEK_API_KEY",
        region: Region::Cn,
        presets: &["deepseek-v4-flash", "deepseek-v4-pro"],
    },
    NativeProvider {
        id: "kimi-coding",
        display: "Kimi For Coding（月之暗面）",
        env_var: "KIMI_API_KEY",
        region: Region::Cn,
        presets: &["kimi-for-coding", "k3"],
    },
    NativeProvider {
        id: "moonshotai-cn",
        display: "月之暗面开放平台（Moonshot）",
        env_var: "MOONSHOT_API_KEY",
        region: Region::Cn,
        presets: &["kimi-k3", "kimi-k2.6"],
    },
    NativeProvider {
        id: "zai-coding-cn",
        display: "智谱 GLM Coding Plan（国内）",
        env_var: "ZAI_CODING_CN_API_KEY",
        region: Region::Cn,
        presets: &["glm-5.2", "glm-5.1", "glm-4.7"],
    },
    NativeProvider {
        id: "minimax-cn",
        display: "MiniMax（国内）",
        env_var: "MINIMAX_CN_API_KEY",
        region: Region::Cn,
        presets: &["MiniMax-M3", "MiniMax-M2.7"],
    },
    NativeProvider {
        id: "qwen-token-plan-cn",
        display: "通义千问 Token Plan（国内）",
        env_var: "QWEN_TOKEN_PLAN_CN_API_KEY",
        region: Region::Cn,
        presets: &["qwen3.8-max", "qwen3.7-plus", "qwen3.8-flash"],
    },
    NativeProvider {
        id: "xiaomi",
        display: "小米 MiMo",
        env_var: "XIAOMI_API_KEY",
        region: Region::Cn,
        presets: &["mimo-v2.5-pro", "mimo-v2.5"],
    },
];

pub fn native(id: &str) -> Option<&'static NativeProvider> {
    NATIVE.iter().find(|p| p.id == id)
}

#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct ProviderInfo {
    pub id: String,
    pub display: String,
    pub native: bool,
    /// 自定义为 None。
    pub region: Option<Region>,
    /// 钥匙串里有没有 key。
    pub configured: bool,
    /// 原生为 None。
    pub base_url: Option<String>,
    /// 原生为 None。
    pub api: Option<ApiKind>,
    pub presets: Vec<String>,
}

/// 自定义 provider 的 id 规则：`^custom-[a-z0-9][a-z0-9-]{0,30}$`。
/// 强制 `custom-` 前缀，防止与 pi 内建 provider 同名而被合并。
pub fn is_valid_custom_id(id: &str) -> bool {
    let Some(rest) = id.strip_prefix("custom-") else {
        return false;
    };
    let mut chars = rest.chars();
    let first_ok = matches!(chars.next(), Some(c) if c.is_ascii_lowercase() || c.is_ascii_digit());
    first_ok
        && rest.len() <= 31
        && rest
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// "custom-moonshot" -> "SUPERAGENT_KEY_CUSTOM_MOONSHOT"；前缀与原生环境变量不相交。
pub fn custom_env_var(id: &str) -> String {
    format!("SUPERAGENT_KEY_{}", id.to_uppercase().replace('-', "_"))
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CustomProvider {
    pub id: String,
    pub display: String,
    pub base_url: String,
    pub api: ApiKind,
    pub models: Vec<String>,
}

/// `http://` 只允许本机回环（本机 Ollama 等）；其余必须 `https://`。
fn base_url_ok(url: &str) -> bool {
    if url.chars().any(char::is_whitespace) {
        return false;
    }
    if let Some(rest) = url.strip_prefix("https://") {
        return !rest.is_empty() && !rest.starts_with('/');
    }
    if let Some(rest) = url.strip_prefix("http://") {
        let authority = rest.split('/').next().unwrap_or("");
        let host = authority.split(':').next().unwrap_or("");
        return host == "127.0.0.1" || host == "localhost";
    }
    false
}

/// 校验并规范化（去 base_url 末尾 `/`、各字段首尾空白）。
pub fn validate_custom(p: &CustomProvider) -> Result<CustomProvider, String> {
    if !is_valid_custom_id(&p.id) {
        return Err(format!(
            "自定义 provider 的 id 必须形如 custom-xxx（小写字母、数字、连字符，最长 38 位）：{}",
            p.id
        ));
    }
    let display = p.display.trim().to_string();
    if display.is_empty() || display.chars().count() > 40 {
        return Err("显示名需为 1 到 40 个字符".to_string());
    }
    let base_url = p.base_url.trim().trim_end_matches('/').to_string();
    if !base_url_ok(&base_url) {
        return Err(
            "接口地址必须以 https:// 开头（本机 127.0.0.1 / localhost 可用 http://）".to_string(),
        );
    }
    if p.models.is_empty() || p.models.len() > 20 {
        return Err("模型 id 需为 1 到 20 个".to_string());
    }
    let models: Vec<String> = p.models.iter().map(|m| m.trim().to_string()).collect();
    if models
        .iter()
        .any(|m| m.is_empty() || m.chars().any(char::is_whitespace))
    {
        return Err("模型 id 不能为空，也不能含空白".to_string());
    }
    Ok(CustomProvider {
        id: p.id.clone(),
        display,
        base_url,
        api: p.api,
        models,
    })
}

#[derive(serde::Serialize, serde::Deserialize)]
struct ProvidersFile {
    version: u32,
    custom: Vec<CustomProvider>,
}

/// `providers.json`（host-global）：只存非密钥配置，密钥在系统钥匙串。
pub struct ProvidersStore {
    path: std::path::PathBuf,
}

impl ProvidersStore {
    pub fn new(path: std::path::PathBuf) -> Self {
        Self { path }
    }

    /// 文件不存在 -> 空；存在但读取/解析失败 -> Err（不静默当空，避免随后的写入覆盖掉用户配置）。
    pub fn list(&self) -> Result<Vec<CustomProvider>, String> {
        match std::fs::read_to_string(&self.path) {
            Ok(s) => serde_json::from_str::<ProvidersFile>(&s)
                .map(|f| f.custom)
                .map_err(|e| format!("{} 解析失败：{e}", self.path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(e) => Err(format!("{} 读取失败：{e}", self.path.display())),
        }
    }

    fn save(&self, custom: Vec<CustomProvider>) -> Result<(), String> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        // 原子写：先写 .json.tmp 再 rename（同 skills / approvals 的存盘方式）。
        let tmp = self.path.with_extension("json.tmp");
        let body = serde_json::to_string_pretty(&ProvidersFile { version: 1, custom })
            .map_err(|e| e.to_string())?;
        std::fs::write(&tmp, body).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, &self.path).map_err(|e| e.to_string())
    }

    pub fn upsert(&self, p: CustomProvider) -> Result<(), String> {
        let p = validate_custom(&p)?;
        let mut all = self.list()?;
        match all.iter_mut().find(|x| x.id == p.id) {
            Some(slot) => *slot = p,
            None => all.push(p),
        }
        self.save(all)
    }

    pub fn remove(&self, id: &str) -> Result<bool, String> {
        let mut all = self.list()?;
        let before = all.len();
        all.retain(|x| x.id != id);
        if all.len() == before {
            return Ok(false);
        }
        self.save(all)?;
        Ok(true)
    }
}

/// 每应用 models.json 内容；`include(id)` 为真者才写入，一个都没有 -> None。
/// `apiKey` 一律写环境变量引用 `${SUPERAGENT_KEY_…}`（pi 支持插值），绝不写字面值。
pub fn models_json(
    custom: &[CustomProvider],
    include: impl Fn(&str) -> bool,
) -> Option<serde_json::Value> {
    let mut providers = serde_json::Map::new();
    for c in custom.iter().filter(|c| include(&c.id)) {
        providers.insert(
            c.id.clone(),
            serde_json::json!({
                "baseUrl": c.base_url,
                "api": c.api,
                "apiKey": format!("${{{}}}", custom_env_var(&c.id)),
                "models": c.models.iter().map(|m| serde_json::json!({ "id": m })).collect::<Vec<_>>(),
            }),
        );
    }
    if providers.is_empty() {
        None
    } else {
        Some(serde_json::json!({ "providers": providers }))
    }
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct CustomPreset {
    pub suggested_id: &'static str,
    pub display: &'static str,
    pub base_url: &'static str,
    pub api: ApiKind,
    pub models: &'static [&'static str],
}

/// 命名预设只预填地址；模型 id 留空由用户填。
pub const CUSTOM_PRESETS: &[CustomPreset] = &[
    CustomPreset {
        suggested_id: "custom-dashscope",
        display: "阿里云百炼（DashScope 兼容模式）",
        base_url: "https://dashscope.aliyuncs.com/compatible-mode/v1",
        api: ApiKind::OpenAiCompletions,
        models: &[],
    },
    CustomPreset {
        suggested_id: "custom-bigmodel",
        display: "智谱开放平台（BigModel）",
        base_url: "https://open.bigmodel.cn/api/paas/v4",
        api: ApiKind::OpenAiCompletions,
        models: &[],
    },
];

/// 纯函数，便于不碰钥匙串地测试：原生在前，自定义在后。
pub fn provider_infos(
    custom: &[CustomProvider],
    is_configured: impl Fn(&str) -> bool,
) -> Vec<ProviderInfo> {
    let native = NATIVE.iter().map(|p| ProviderInfo {
        id: p.id.to_string(),
        display: p.display.to_string(),
        native: true,
        region: Some(p.region),
        configured: is_configured(p.id),
        base_url: None,
        api: None,
        presets: p.presets.iter().map(|m| m.to_string()).collect(),
    });
    let custom = custom.iter().map(|c| ProviderInfo {
        id: c.id.clone(),
        display: c.display.clone(),
        native: false,
        region: None,
        configured: is_configured(&c.id),
        base_url: Some(c.base_url.clone()),
        api: Some(c.api),
        presets: c.models.clone(),
    });
    native.chain(custom).collect()
}

fn store(app: &tauri::AppHandle) -> Result<ProvidersStore, String> {
    let root = app.path().app_data_dir().map_err(|e| e.to_string())?;
    Ok(ProvidersStore::new(DataLayout::new(root).providers_path()))
}

/// 自定义 provider 是否已保存（供 `secrets::set_api_key` 校验）。
pub fn custom_exists(app: &tauri::AppHandle, id: &str) -> Result<bool, String> {
    Ok(store(app)?.list()?.iter().any(|c| c.id == id))
}

#[tauri::command]
pub fn list_providers(app: tauri::AppHandle) -> Result<Vec<ProviderInfo>, String> {
    let custom = store(&app)?.list()?;
    Ok(provider_infos(&custom, secrets::has_key))
}

#[tauri::command]
pub fn save_custom_provider(app: tauri::AppHandle, provider: CustomProvider) -> Result<(), String> {
    store(&app)?.upsert(provider)
}

/// 删除自定义 provider，同时清掉它在钥匙串里的 Key。
#[tauri::command]
pub fn remove_custom_provider(app: tauri::AppHandle, id: String) -> Result<(), String> {
    store(&app)?.remove(&id)?;
    secrets::clear_api_key(id)
}

#[tauri::command]
pub fn custom_provider_presets() -> Vec<CustomPreset> {
    CUSTOM_PRESETS.to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn native_ids_and_env_vars_are_unique() {
        let mut ids = HashSet::new();
        let mut envs = HashSet::new();
        for p in NATIVE {
            assert!(ids.insert(p.id), "重复的 id：{}", p.id);
            assert!(envs.insert(p.env_var), "重复的环境变量：{}", p.env_var);
        }
        assert_eq!(NATIVE.len(), 14);
    }

    #[test]
    fn native_directory_matches_pi_env_map_fixture() {
        let fx: serde_json::Value =
            serde_json::from_str(include_str!("../tests/fixtures/pi-env-map.json")).unwrap();
        let env = fx["env"].as_object().unwrap();
        assert!(!NATIVE.is_empty());
        for p in NATIVE {
            assert_eq!(
                env.get(p.id).and_then(|v| v.as_str()),
                Some(p.env_var),
                "{} 的环境变量与 pi 的 envMap 不一致",
                p.id
            );
        }
    }

    #[test]
    fn pi_env_map_fixture_version_matches_pi_version_txt() {
        let fx: serde_json::Value =
            serde_json::from_str(include_str!("../tests/fixtures/pi-env-map.json")).unwrap();
        let pinned = include_str!("../pi-version.txt").trim();
        assert_eq!(fx["pi_version"].as_str(), Some(pinned));
        assert_eq!(PI_CATALOG_VERSION, pinned);
    }

    #[test]
    fn every_native_provider_has_a_preset() {
        assert!(!NATIVE.is_empty());
        for p in NATIVE {
            assert!(!p.presets.is_empty(), "{} 没有预设模型", p.id);
        }
    }

    #[test]
    fn no_native_id_uses_custom_prefix() {
        assert!(!NATIVE.is_empty());
        for p in NATIVE {
            assert!(!p.id.starts_with("custom-"), "{}", p.id);
        }
    }

    #[test]
    fn provider_infos_marks_configured_from_lookup() {
        let infos = provider_infos(&[], |id| id == "deepseek");
        assert_eq!(infos.len(), 14);
        for i in &infos {
            assert_eq!(i.configured, i.id == "deepseek", "{}", i.id);
            assert!(i.native);
        }
        let ds = infos.iter().find(|i| i.id == "deepseek").unwrap();
        assert_eq!(ds.region, Some(Region::Cn));
        let ms = infos.iter().find(|i| i.id == "moonshotai-cn").unwrap();
        assert_eq!(ms.region, Some(Region::Cn));
        assert!(infos.iter().all(|i| i.id != "moonshotai"));
        assert_eq!(
            infos
                .iter()
                .filter(|i| i.region == Some(Region::Cn))
                .count(),
            7
        );
    }

    fn sample(id: &str) -> CustomProvider {
        CustomProvider {
            id: id.to_string(),
            display: "示例".to_string(),
            base_url: "https://api.example.com/v1".to_string(),
            api: ApiKind::OpenAiCompletions,
            models: vec!["m1".to_string()],
        }
    }

    fn temp_store() -> (tempfile::TempDir, ProvidersStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = ProvidersStore::new(dir.path().join("providers.json"));
        (dir, store)
    }

    #[test]
    fn custom_provider_roundtrip_persists() {
        let (dir, store) = temp_store();
        store.upsert(sample("custom-a")).unwrap();
        let again = ProvidersStore::new(dir.path().join("providers.json"));
        assert_eq!(again.list().unwrap(), vec![sample("custom-a")]);
    }

    #[test]
    fn upsert_same_id_replaces_not_duplicates() {
        let (_d, store) = temp_store();
        store.upsert(sample("custom-a")).unwrap();
        let mut changed = sample("custom-a");
        changed.display = "改名".to_string();
        store.upsert(changed.clone()).unwrap();
        assert_eq!(store.list().unwrap(), vec![changed]);
    }

    #[test]
    fn remove_returns_false_for_unknown_id() {
        let (_d, store) = temp_store();
        assert!(!store.remove("custom-nope").unwrap());
        store.upsert(sample("custom-a")).unwrap();
        assert!(store.remove("custom-a").unwrap());
        assert!(store.list().unwrap().is_empty());
    }

    #[test]
    fn corrupt_providers_json_is_an_error_not_empty() {
        let (dir, store) = temp_store();
        std::fs::write(dir.path().join("providers.json"), "{ not json").unwrap();
        assert!(store.list().is_err());
    }

    #[test]
    fn validate_rejects_id_without_custom_prefix() {
        for bad in [
            "openai",
            "custom-",
            "custom-X",
            "custom--a",
            "custom-a_b",
            "",
        ] {
            assert!(validate_custom(&sample(bad)).is_err(), "{bad}");
        }
        assert!(validate_custom(&sample("custom-a-1")).is_ok());
    }

    #[test]
    fn validate_rejects_http_non_loopback() {
        let mut p = sample("custom-a");
        p.base_url = "http://api.example.com/v1".to_string();
        assert!(validate_custom(&p).is_err());
        p.base_url = "http://127.0.0.1.evil.com/v1".to_string();
        assert!(validate_custom(&p).is_err());
        p.base_url = "ftp://x".to_string();
        assert!(validate_custom(&p).is_err());
    }

    #[test]
    fn validate_accepts_http_localhost() {
        let mut p = sample("custom-a");
        p.base_url = "http://localhost:11434/v1".to_string();
        assert!(validate_custom(&p).is_ok());
        p.base_url = "http://127.0.0.1:8080/v1".to_string();
        assert!(validate_custom(&p).is_ok());
    }

    #[test]
    fn validate_strips_trailing_slash() {
        let mut p = sample("custom-a");
        p.base_url = "https://api.example.com/v1/".to_string();
        assert_eq!(
            validate_custom(&p).unwrap().base_url,
            "https://api.example.com/v1"
        );
    }

    #[test]
    fn validate_rejects_empty_models() {
        let mut p = sample("custom-a");
        p.models = vec![];
        assert!(validate_custom(&p).is_err());
        p.models = vec!["has space".to_string()];
        assert!(validate_custom(&p).is_err());
        p.models = vec![String::new()];
        assert!(validate_custom(&p).is_err());
        p.models = (0..21).map(|i| format!("m{i}")).collect();
        assert!(validate_custom(&p).is_err());
    }

    #[test]
    fn models_json_only_contains_included_providers() {
        let custom = vec![sample("custom-a"), sample("custom-b")];
        let v = models_json(&custom, |id| id == "custom-b").unwrap();
        let provs = v["providers"].as_object().unwrap();
        assert_eq!(provs.len(), 1);
        assert_eq!(provs["custom-b"]["baseUrl"], "https://api.example.com/v1");
        assert_eq!(provs["custom-b"]["api"], "openai-completions");
        assert_eq!(provs["custom-b"]["models"][0]["id"], "m1");
    }

    #[test]
    fn models_json_none_when_nothing_included() {
        let custom = vec![sample("custom-a")];
        assert!(models_json(&custom, |_| false).is_none());
        assert!(models_json(&[], |_| true).is_none());
    }

    #[test]
    fn models_json_api_key_is_env_reference_not_literal() {
        let v = models_json(&[sample("custom-x")], |_| true).unwrap();
        assert_eq!(
            v["providers"]["custom-x"]["apiKey"],
            "${SUPERAGENT_KEY_CUSTOM_X}"
        );
        assert!(!v.to_string().contains("sk-test-fake"));
    }

    #[test]
    fn custom_env_var_never_collides_with_native_env_vars() {
        assert_eq!(
            custom_env_var("custom-moonshot"),
            "SUPERAGENT_KEY_CUSTOM_MOONSHOT"
        );
        assert!(!NATIVE.is_empty());
        for p in NATIVE {
            assert!(!p.env_var.starts_with("SUPERAGENT_KEY_"), "{}", p.id);
        }
    }

    #[test]
    fn provider_infos_appends_custom_after_native() {
        let infos = provider_infos(&[sample("custom-a")], |id| id == "custom-a");
        assert_eq!(infos.len(), 15);
        let last = infos.last().unwrap();
        assert_eq!(last.id, "custom-a");
        assert!(!last.native && last.configured && last.region.is_none());
        assert_eq!(last.base_url.as_deref(), Some("https://api.example.com/v1"));
        assert_eq!(last.api, Some(ApiKind::OpenAiCompletions));
        assert_eq!(last.presets, vec!["m1".to_string()]);
    }

    #[test]
    fn presets_are_valid_custom_providers() {
        assert_eq!(CUSTOM_PRESETS.len(), 2);
        for pr in CUSTOM_PRESETS {
            let p = CustomProvider {
                id: pr.suggested_id.to_string(),
                display: pr.display.to_string(),
                base_url: pr.base_url.to_string(),
                api: pr.api,
                models: if pr.models.is_empty() {
                    vec!["x".to_string()]
                } else {
                    pr.models.iter().map(|m| m.to_string()).collect()
                },
            };
            validate_custom(&p).unwrap_or_else(|e| panic!("{}: {e}", pr.suggested_id));
        }
    }
}
