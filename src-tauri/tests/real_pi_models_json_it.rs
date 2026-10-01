//! 真实 pi：`model_launch` 产出的 models.json 能让 pi 认出自定义 provider，且
//! `apiKey` 里的 `${环境变量}` 引用真的被插值（没有对应环境变量时该 provider 不可用）。
//! 需要本机 pi；`cargo test --test real_pi_models_json_it -- --ignored`。
use std::process::Command;
use super_agent_os::model_overrides::{model_launch, EffectiveModel, ModelSource};
use super_agent_os::providers::{ApiKind, CustomProvider};

fn list_models(
    agent_dir: &std::path::Path,
    home: &std::path::Path,
    key_env: Option<(&str, &str)>,
) -> String {
    let mut cmd = Command::new(super_agent_os::pi_bin::resolve_pi_bin());
    cmd.args(["--list-models", "custom-mock"])
        .env("PI_CODING_AGENT_DIR", agent_dir)
        .env("PI_OFFLINE", "1")
        .env("HOME", home)
        // 不继承宿主里可能存在的同名变量。
        .env_remove("SUPERAGENT_KEY_CUSTOM_MOCK");
    if let Some((k, v)) = key_env {
        cmd.env(k, v);
    }
    let out = cmd.output().expect("应能运行 pi");
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

#[test]
#[ignore = "需要本机 pi"]
fn custom_provider_from_models_json_is_listed() {
    let custom = [CustomProvider {
        id: "custom-mock".into(),
        display: "Mock".into(),
        base_url: "http://127.0.0.1:9/v1".into(),
        api: ApiKind::OpenAiCompletions,
        models: vec!["mock-model-1".into()],
    }];
    let eff = EffectiveModel {
        provider: Some("custom-mock".into()),
        model: Some("mock-model-1".into()),
        source: ModelSource::App,
    };
    let ml = model_launch(&eff, &custom, |_| Some("sk-test-fake".into()), true);
    let models_json = ml.models_json.expect("应生成 models.json");
    let (var, val) = ml
        .env
        .iter()
        .find(|(k, _)| k.starts_with("SUPERAGENT_KEY_"))
        .cloned()
        .expect("应注入自定义 provider 的密钥变量");

    let dir = tempfile::tempdir().unwrap();
    let agent_dir = dir.path().join("agent");
    std::fs::create_dir_all(&agent_dir).unwrap();
    std::fs::write(
        agent_dir.join("models.json"),
        serde_json::to_string_pretty(&models_json).unwrap(),
    )
    .unwrap();

    let with_key = list_models(&agent_dir, dir.path(), Some((&var, &val)));
    println!("--- 带密钥环境变量 ---\n{with_key}");
    assert!(with_key.contains("mock-model-1"), "{with_key}");

    let without_key = list_models(&agent_dir, dir.path(), None);
    println!("--- 去掉环境变量 ---\n{without_key}");
    assert!(
        !without_key.contains("mock-model-1"),
        "没有环境变量时不应列出（证明 ${{VAR}} 插值生效）：{without_key}"
    );
}
