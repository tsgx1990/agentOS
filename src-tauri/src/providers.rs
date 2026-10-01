//! 原生 provider 目录：id 与环境变量名逐字照抄 pi 0.84.4 `docs/providers.md` 的表，
//! 并由 tests/fixtures/pi-env-map.json（从同版本 pi 二进制的 envMap 抄出）钉住。
use crate::secrets;

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
    /// 自定义为 None（Task 2 起出现）。
    pub region: Option<Region>,
    /// 钥匙串里有没有 key。
    pub configured: bool,
    /// 原生为 None。
    pub base_url: Option<String>,
    /// 原生为 None。
    pub api: Option<ApiKind>,
    pub presets: Vec<String>,
}

/// 纯函数，便于不碰钥匙串地测试；Task 2 会加 custom 参数。
pub fn provider_infos(is_configured: impl Fn(&str) -> bool) -> Vec<ProviderInfo> {
    NATIVE
        .iter()
        .map(|p| ProviderInfo {
            id: p.id.to_string(),
            display: p.display.to_string(),
            native: true,
            region: Some(p.region),
            configured: is_configured(p.id),
            base_url: None,
            api: None,
            presets: p.presets.iter().map(|m| m.to_string()).collect(),
        })
        .collect()
}

#[tauri::command]
pub fn list_providers() -> Result<Vec<ProviderInfo>, String> {
    Ok(provider_infos(secrets::has_key))
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
        let infos = provider_infos(|id| id == "deepseek");
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
}
