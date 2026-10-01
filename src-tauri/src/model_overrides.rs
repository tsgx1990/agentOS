//! 全局默认模型 / 按应用覆盖，以及启动时「按所选 provider 最小注入密钥」。
//!
//! 优先级：应用覆盖 > 全局默认 > 清单默认 > 无。密钥只来自系统钥匙串，`model-overrides.json`
//! 与每应用 `models.json` 里没有任何密钥字面值（models.json 只写 `${环境变量名}` 引用）。
use crate::paths::DataLayout;
use crate::providers::{self, CustomProvider, ProvidersStore};
use crate::registry::RegistryStore;
use std::collections::BTreeMap;
use std::path::PathBuf;
use tauri::Manager;

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ModelChoice {
    pub provider: String,
    pub model: String,
}

#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct OverridesFile {
    #[serde(default)]
    pub version: u32,
    #[serde(default)]
    pub global: Option<ModelChoice>,
    #[serde(default)]
    pub apps: BTreeMap<String, ModelChoice>,
}

pub struct OverridesStore {
    path: PathBuf,
}

impl OverridesStore {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    /// 文件不存在 -> 默认值；存在但读取/解析失败 -> Err（不静默当空，避免随后的写入覆盖掉用户选择）。
    pub fn load(&self) -> Result<OverridesFile, String> {
        match std::fs::read_to_string(&self.path) {
            Ok(s) => serde_json::from_str(&s).map_err(|e| {
                format!(
                    "{} 解析失败：{e}（请修复或删除该文件）",
                    self.path.display()
                )
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(OverridesFile::default()),
            Err(e) => Err(format!(
                "{} 读取失败：{e}（请修复或删除该文件）",
                self.path.display()
            )),
        }
    }

    fn save(&self, mut f: OverridesFile) -> Result<(), String> {
        f.version = 1;
        // 原子写复用 approvals 的公共 helper（tmp + rename，自动建父目录）。
        crate::approvals::save_json_file(&self.path, &f)
    }

    pub fn set_global(&self, c: Option<ModelChoice>) -> Result<(), String> {
        let mut f = self.load()?;
        f.global = c;
        self.save(f)
    }

    /// `None` = 撤销该应用的覆盖。
    pub fn set_app(&self, app_id: &str, c: Option<ModelChoice>) -> Result<(), String> {
        let mut f = self.load()?;
        match c {
            Some(c) => {
                f.apps.insert(app_id.to_string(), c);
            }
            None => {
                f.apps.remove(app_id);
            }
        }
        self.save(f)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ModelSource {
    App,
    Global,
    Manifest,
    None,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct EffectiveModel {
    pub provider: Option<String>,
    pub model: Option<String>,
    pub source: ModelSource,
}

/// "groq/openai/gpt-oss-120b" -> (Some("groq"), "openai/gpt-oss-120b")：
/// 只在第一个 '/' 之前的前缀是已知 provider 时拆分，否则整串是 model。
pub fn parse_model_ref(s: &str, known: impl Fn(&str) -> bool) -> (Option<String>, String) {
    if let Some((head, rest)) = s.split_once('/') {
        if !rest.is_empty() && known(head) {
            return (Some(head.to_string()), rest.to_string());
        }
    }
    (None, s.to_string())
}

/// 应用覆盖 > 全局默认 > 清单默认 > None；覆盖里的 provider 若 !known（自定义已删）则跳过该级。
/// `app_id` 为 None 表示主会话（不看 apps）。
pub fn resolve(
    app_id: Option<&str>,
    f: &OverridesFile,
    manifest_model: Option<&str>,
    known: impl Fn(&str) -> bool,
) -> EffectiveModel {
    let pick = |c: &ModelChoice, source| {
        known(&c.provider).then(|| EffectiveModel {
            provider: Some(c.provider.clone()),
            model: Some(c.model.clone()),
            source,
        })
    };
    if let Some(e) = app_id
        .and_then(|id| f.apps.get(id))
        .and_then(|c| pick(c, ModelSource::App))
    {
        return e;
    }
    if let Some(e) = f.global.as_ref().and_then(|c| pick(c, ModelSource::Global)) {
        return e;
    }
    match manifest_model {
        Some(m) => {
            let (provider, model) = parse_model_ref(m, &known);
            EffectiveModel {
                provider,
                model: Some(model),
                source: ModelSource::Manifest,
            }
        }
        None => EffectiveModel {
            provider: None,
            model: None,
            source: ModelSource::None,
        },
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ModelLaunch {
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    pub models_json: Option<serde_json::Value>,
}

/// 纯函数：由有效模型算出启动参数、环境变量与 models.json。
pub fn model_launch(
    eff: &EffectiveModel,
    custom: &[CustomProvider],
    lookup_key: impl Fn(&str) -> Option<String>,
    allow_custom: bool,
) -> ModelLaunch {
    let selected = eff
        .provider
        .as_deref()
        .filter(|p| providers::is_known(p, custom));
    if let Some(p) = selected {
        if providers::native(p).is_none() && !allow_custom {
            // 主会话没有私有 agent home，写不了 models.json，自定义 provider 在此无法使用：
            // 按「provider 未知」处理，仍注入全部已配置的原生 key，主助手不至于完全不可用。
            eprintln!("主会话不支持自定义 provider {p}，忽略该选择，注入全部已配置的原生密钥");
            return model_launch(
                &EffectiveModel {
                    provider: None,
                    model: None,
                    source: ModelSource::None,
                },
                custom,
                lookup_key,
                false,
            );
        }
    }
    let mut out = ModelLaunch::default();
    match (selected, eff.model.as_deref()) {
        (Some(p), Some(m)) => {
            out.args = vec!["--provider".into(), p.into(), "--model".into(), m.into()]
        }
        (_, Some(m)) => out.args = vec!["--model".into(), m.into()],
        _ => {}
    }
    match selected {
        Some(p) => {
            // 只注入所选 provider 的 key；其余原生 provider 的变量置空（pi 视空串为未设置），
            // 防止宿主环境里的同名变量被子进程继承。
            // 所选原生 provider 没有 key 时不为它 push：继承宿主里同一家的同名变量。
            for n in providers::NATIVE {
                if n.id == p {
                    if let Some(k) = lookup_key(n.id) {
                        out.env.push((n.env_var.to_string(), k));
                    }
                } else {
                    out.env.push((n.env_var.to_string(), String::new()));
                }
            }
            if providers::native(p).is_none() {
                if let (Some(var), Some(k)) = (crate::secrets::env_var_for(p), lookup_key(p)) {
                    out.env.push((var, k));
                    out.models_json = providers::models_json(custom, |id| id == p);
                }
            }
        }
        None => {
            let mut ids: Vec<String> = providers::NATIVE.iter().map(|n| n.id.to_string()).collect();
            if allow_custom {
                ids.extend(custom.iter().map(|c| c.id.clone()));
            }
            // 每个 id 只读一次钥匙串，env 与 models.json 共用这份结果。
            let found: std::collections::HashMap<String, String> = ids
                .iter()
                .filter_map(|id| Some((id.clone(), lookup_key(id)?)))
                .collect();
            out.env = crate::secrets::key_env_pairs_with(&ids, |id| found.get(id).cloned());
            if allow_custom {
                out.models_json = providers::models_json(custom, |id| found.contains_key(id));
            }
        }
    }
    out
}

fn layout_of(app: &tauri::AppHandle) -> Result<DataLayout, String> {
    Ok(DataLayout::new(
        app.path().app_data_dir().map_err(|e| e.to_string())?,
    ))
}

/// 按「是否已知 provider」判定的闭包要用到自定义列表。
fn known_in(custom: &[CustomProvider]) -> impl Fn(&str) -> bool + '_ {
    move |id| providers::is_known(id, custom)
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct AppModelRow {
    pub app_id: String,
    pub manifest_model: Option<String>,
    pub app_override: Option<ModelChoice>,
    pub effective: EffectiveModel,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct ModelSettingsView {
    pub global: Option<ModelChoice>,
    pub apps: Vec<AppModelRow>,
}

#[tauri::command]
pub fn get_model_settings(app: tauri::AppHandle) -> Result<ModelSettingsView, String> {
    let layout = layout_of(&app)?;
    let custom = ProvidersStore::new(layout.providers_path()).list()?;
    let f = OverridesStore::new(layout.model_overrides_path()).load()?;
    let apps = RegistryStore::new(layout.registry_path())
        .load()
        .into_iter()
        .map(|rec| {
            // 单个清单读失败只影响该行，不让整体失败。
            let manifest_model = crate::pkg::load_and_validate(&layout.packages_dir(&rec.app_id))
                .ok()
                .and_then(|m| m.superagent.model);
            AppModelRow {
                effective: resolve(
                    Some(&rec.app_id),
                    &f,
                    manifest_model.as_deref(),
                    known_in(&custom),
                ),
                app_override: f.apps.get(&rec.app_id).cloned(),
                manifest_model,
                app_id: rec.app_id,
            }
        })
        .collect();
    Ok(ModelSettingsView {
        global: f.global,
        apps,
    })
}

fn check_choice(c: &ModelChoice, custom: &[CustomProvider]) -> Result<(), String> {
    if !providers::is_known(&c.provider, custom) {
        return Err(format!("未知的 provider：{}", c.provider));
    }
    if c.model.trim().is_empty() {
        return Err("模型不能为空".to_string());
    }
    Ok(())
}

#[tauri::command]
pub fn set_global_model(app: tauri::AppHandle, choice: Option<ModelChoice>) -> Result<(), String> {
    let layout = layout_of(&app)?;
    if let Some(c) = &choice {
        check_choice(c, &ProvidersStore::new(layout.providers_path()).list()?)?;
    }
    OverridesStore::new(layout.model_overrides_path()).set_global(choice)
}

#[tauri::command]
pub fn set_app_model(
    app: tauri::AppHandle,
    app_id: String,
    choice: Option<ModelChoice>,
) -> Result<(), String> {
    let layout = layout_of(&app)?;
    if RegistryStore::new(layout.registry_path())
        .get(&app_id)
        .is_none()
    {
        return Err("应用未安装".to_string());
    }
    if let Some(c) = &choice {
        check_choice(c, &ProvidersStore::new(layout.providers_path()).list()?)?;
    }
    OverridesStore::new(layout.model_overrides_path()).set_app(&app_id, choice)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::ApiKind;

    fn known_ds(p: &str) -> bool {
        matches!(p, "deepseek" | "anthropic" | "groq" | "custom-mock")
    }
    fn choice(p: &str, m: &str) -> ModelChoice {
        ModelChoice {
            provider: p.into(),
            model: m.into(),
        }
    }
    fn mock_custom() -> CustomProvider {
        CustomProvider {
            id: "custom-mock".into(),
            display: "Mock".into(),
            base_url: "http://127.0.0.1:9/v1".into(),
            api: ApiKind::OpenAiCompletions,
            models: vec!["m1".into()],
        }
    }
    fn eff(p: Option<&str>, m: Option<&str>) -> EffectiveModel {
        EffectiveModel {
            provider: p.map(String::from),
            model: m.map(String::from),
            source: ModelSource::App,
        }
    }
    fn key_of(id: &str) -> Option<String> {
        matches!(id, "deepseek" | "anthropic" | "custom-mock").then(|| "sk-test-fake".to_string())
    }
    fn env_get<'a>(l: &'a ModelLaunch, k: &str) -> Option<&'a str> {
        l.env.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str())
    }

    #[test]
    fn resolve_precedence_app_over_global_over_manifest() {
        let mut f = OverridesFile {
            version: 1,
            global: Some(choice("anthropic", "g")),
            ..Default::default()
        };
        f.apps.insert("a".into(), choice("deepseek", "x"));
        let r = resolve(Some("a"), &f, Some("groq/m"), known_ds);
        assert_eq!(r.source, ModelSource::App);
        assert_eq!(r.provider.as_deref(), Some("deepseek"));
        let r = resolve(Some("b"), &f, Some("groq/m"), known_ds);
        assert_eq!(r.source, ModelSource::Global);
        assert_eq!(r.model.as_deref(), Some("g"));
        let none = OverridesFile::default();
        let r = resolve(Some("b"), &none, Some("groq/m"), known_ds);
        assert_eq!(r.source, ModelSource::Manifest);
        assert_eq!(r.provider.as_deref(), Some("groq"));
        assert_eq!(r.model.as_deref(), Some("m"));
        let r = resolve(Some("b"), &none, Some("claude-sonnet-5"), known_ds);
        assert_eq!(
            (r.provider, r.model.as_deref()),
            (None, Some("claude-sonnet-5"))
        );
        let r = resolve(Some("b"), &none, None, known_ds);
        assert_eq!(r.source, ModelSource::None);
        // 主会话不看 apps
        let r = resolve(None, &f, None, known_ds);
        assert_eq!(r.source, ModelSource::Global);
    }

    #[test]
    fn clearing_app_override_falls_back_to_global_immediately() {
        let dir = tempfile::tempdir().unwrap();
        let st = OverridesStore::new(dir.path().join("o.json"));
        st.set_global(Some(choice("anthropic", "g"))).unwrap();
        st.set_app("a", Some(choice("deepseek", "x"))).unwrap();
        let f = st.load().unwrap();
        assert_eq!(
            resolve(Some("a"), &f, None, known_ds).source,
            ModelSource::App
        );
        st.set_app("a", None).unwrap();
        let f = st.load().unwrap();
        assert_eq!(
            resolve(Some("a"), &f, None, known_ds).source,
            ModelSource::Global
        );
    }

    #[test]
    fn override_with_unknown_provider_is_skipped() {
        let mut f = OverridesFile {
            version: 1,
            global: Some(choice("anthropic", "g")),
            ..Default::default()
        };
        f.apps.insert("a".into(), choice("custom-deleted", "x"));
        let r = resolve(Some("a"), &f, None, known_ds);
        assert_eq!(r.source, ModelSource::Global);
        f.global = Some(choice("custom-deleted", "y"));
        let r = resolve(Some("a"), &f, Some("groq/m"), known_ds);
        assert_eq!(r.source, ModelSource::Manifest);
    }

    #[test]
    fn parse_model_ref_splits_only_known_provider_prefix() {
        assert_eq!(
            parse_model_ref("anthropic/claude-sonnet-5", known_ds),
            (Some("anthropic".into()), "claude-sonnet-5".into())
        );
        assert_eq!(
            parse_model_ref("claude-sonnet-5", known_ds),
            (None, "claude-sonnet-5".into())
        );
        assert_eq!(
            parse_model_ref("groq/openai/gpt-oss-120b", known_ds),
            (Some("groq".into()), "openai/gpt-oss-120b".into())
        );
        assert_eq!(
            parse_model_ref("openai/gpt-oss-120b", known_ds),
            (None, "openai/gpt-oss-120b".into())
        );
    }

    #[test]
    fn model_launch_native_injects_only_its_key_and_blanks_other_native_vars() {
        let l = model_launch(
            &eff(Some("deepseek"), Some("deepseek-v4-flash")),
            &[],
            key_of,
            true,
        );
        assert_eq!(
            l.args,
            ["--provider", "deepseek", "--model", "deepseek-v4-flash"]
        );
        assert_eq!(env_get(&l, "DEEPSEEK_API_KEY"), Some("sk-test-fake"));
        // 已配置但未选中的原生 provider 也被置空，不继承宿主环境
        assert_eq!(env_get(&l, "ANTHROPIC_API_KEY"), Some(""));
        assert_eq!(env_get(&l, "OPENAI_API_KEY"), Some(""));
        assert_eq!(l.env.len(), crate::providers::NATIVE.len());
        assert!(l.models_json.is_none());
    }

    #[test]
    fn model_launch_unknown_provider_injects_all_configured_keys() {
        let c = [mock_custom()];
        let l = model_launch(&eff(None, Some("claude-sonnet-5")), &c, key_of, true);
        assert_eq!(l.args, ["--model", "claude-sonnet-5"]);
        assert_eq!(env_get(&l, "DEEPSEEK_API_KEY"), Some("sk-test-fake"));
        assert_eq!(env_get(&l, "ANTHROPIC_API_KEY"), Some("sk-test-fake"));
        assert_eq!(env_get(&l, "OPENAI_API_KEY"), None);
        assert_eq!(
            env_get(&l, "SUPERAGENT_KEY_CUSTOM_MOCK"),
            Some("sk-test-fake")
        );
        assert!(l.models_json.is_some());
        let l = model_launch(&eff(None, None), &c, key_of, true);
        assert!(l.args.is_empty());
    }

    #[test]
    fn model_launch_custom_provider_emits_models_json_with_env_reference() {
        let c = [mock_custom()];
        let l = model_launch(&eff(Some("custom-mock"), Some("m1")), &c, key_of, true);
        assert_eq!(l.args, ["--provider", "custom-mock", "--model", "m1"]);
        assert_eq!(
            env_get(&l, "SUPERAGENT_KEY_CUSTOM_MOCK"),
            Some("sk-test-fake")
        );
        assert_eq!(env_get(&l, "DEEPSEEK_API_KEY"), Some(""));
        let mj = l.models_json.expect("应有 models.json");
        assert_eq!(
            mj["providers"]["custom-mock"]["apiKey"],
            "${SUPERAGENT_KEY_CUSTOM_MOCK}"
        );
        assert!(!mj.to_string().contains("sk-test-fake"));
    }

    #[test]
    fn model_launch_main_session_ignores_custom_choice() {
        let c = [mock_custom()];
        let l = model_launch(&eff(Some("custom-mock"), Some("m1")), &c, key_of, false);
        // 退回「provider 未知」路径：已配置的原生 key 都在，没有参数、没有 models.json、不注入自定义。
        assert!(l.args.is_empty());
        assert!(l.models_json.is_none());
        assert_eq!(env_get(&l, "DEEPSEEK_API_KEY"), Some("sk-test-fake"));
        assert_eq!(env_get(&l, "ANTHROPIC_API_KEY"), Some("sk-test-fake"));
        assert_eq!(env_get(&l, "SUPERAGENT_KEY_CUSTOM_MOCK"), None);
    }

    #[test]
    fn model_launch_selected_native_without_key_is_not_blanked() {
        // groq 没有 key：不为它 push（继承宿主同名变量），其余原生变量照样置空。
        let l = model_launch(&eff(Some("groq"), Some("m")), &[], key_of, true);
        assert_eq!(env_get(&l, "GROQ_API_KEY"), None);
        assert_eq!(env_get(&l, "DEEPSEEK_API_KEY"), Some(""));
        assert_eq!(l.env.len(), crate::providers::NATIVE.len() - 1);
    }

    #[test]
    fn model_launch_unknown_provider_reads_each_key_once() {
        let c = [mock_custom()];
        let calls = std::cell::RefCell::new(Vec::<String>::new());
        model_launch(
            &eff(None, None),
            &c,
            |id| {
                calls.borrow_mut().push(id.to_string());
                key_of(id)
            },
            true,
        );
        let calls = calls.into_inner();
        let mut dedup = calls.clone();
        dedup.sort();
        dedup.dedup();
        assert_eq!(calls.len(), dedup.len(), "{calls:?}");
    }

    #[test]
    fn overrides_file_missing_fields_is_not_corrupt() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("o.json");
        std::fs::write(&p, r#"{"version":1}"#).unwrap();
        let f = OverridesStore::new(p).load().unwrap();
        assert!(f.apps.is_empty() && f.global.is_none());
    }

    #[test]
    fn overrides_store_roundtrip_and_corrupt_file_is_error() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("o.json");
        let st = OverridesStore::new(p.clone());
        assert_eq!(st.load().unwrap(), OverridesFile::default());
        st.set_global(Some(choice("anthropic", "g"))).unwrap();
        st.set_app("a", Some(choice("deepseek", "x"))).unwrap();
        let f = st.load().unwrap();
        assert_eq!(f.version, 1);
        assert_eq!(f.global, Some(choice("anthropic", "g")));
        assert_eq!(f.apps.get("a"), Some(&choice("deepseek", "x")));
        std::fs::write(&p, "{ not json").unwrap();
        assert!(st.load().is_err());
        assert!(st.set_app("b", None).is_err(), "损坏文件不得被写入覆盖");
    }
}
