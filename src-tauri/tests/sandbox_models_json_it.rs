//! 真实 pi + 真实 sandbox-exec：按应用沙盒的同一 profile（`build_profile`，与
//! `session_mgr::sandboxed_argv` 对第三方应用的参数一致）拉起 pi，`PI_CODING_AGENT_DIR`
//! 指向应用私有的 agent home（`<root>/agenthome/<app>`，与 `<root>/apps/<app>` 并列，
//! 不在沙盒的可写区内）里写了 `custom-mock` 的 models.json，断言 pi 能读到并列出该模型。
//!
//! **当前结论（2026-10-01 实测）：读不到。** 对照组（不套沙盒）能列出 `mock-model-1`；
//! 沙盒内 pi 报 `Failed to load models.json: EPERM: operation not permitted, open …`，
//! 且随后对 agent home 的 `mkdir`（auth.json 加锁）同样 EPERM。agent home 不在 profile 的
//! 读白名单里（沙盒只放行 `$APP_DATA`、系统只读路径与 pi/node 自身的安装前缀）。
//! 修复要动安全内核（`sandbox.rs` 的 profile），本任务不动，见 `docs/known-limitations.md`
//! 「沙盒应用暂不支持自定义 provider」。这条测试保留为将来修复后的验收（届时应转绿）。
//!
//! 需要本机 pi 与 macOS：`SUPERAGENT_PI_BIN=<pi 路径> cargo test --test sandbox_models_json_it -- --ignored --nocapture`。
#![cfg(target_os = "macos")]
use std::process::Command;
use super_agent_os::model_overrides::{model_launch, EffectiveModel, ModelSource};
use super_agent_os::paths::DataLayout;
use super_agent_os::providers::{ApiKind, CustomProvider};
use super_agent_os::sandbox::{build_profile, sandbox_exec_argv};

fn run(cmd: &mut Command) -> String {
    let out = cmd.output().expect("应能运行");
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

#[test]
#[ignore = "需要本机 pi 与 macOS sandbox-exec；已知当前会失败（沙盒读不到 agent home），见文件头注释"]
fn sandboxed_pi_reads_models_json_from_agent_home() {
    let root = tempfile::tempdir().unwrap();
    let layout = DataLayout::new(root.path().to_path_buf());
    let agent_home = layout.agent_home_dir("a");
    let app_data = layout.app_data_dir("a");
    std::fs::create_dir_all(&agent_home).unwrap();
    std::fs::create_dir_all(&app_data).unwrap();

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
    std::fs::write(
        agent_home.join("models.json"),
        serde_json::to_string_pretty(&ml.models_json.expect("应生成 models.json")).unwrap(),
    )
    .unwrap();
    let (var, val) = ml
        .env
        .iter()
        .find(|(k, _)| k.starts_with("SUPERAGENT_KEY_"))
        .cloned()
        .expect("应注入自定义 provider 的密钥变量");

    let pi = super_agent_os::pi_bin::resolve_pi_bin();
    let pi_args = ["--list-models", "custom-mock"];
    let home = root.path().join("home");
    std::fs::create_dir_all(&home).unwrap();

    // 对照：不套沙盒时必须能列出，否则后面的结论说明不了沙盒的事。
    let mut plain = Command::new(&pi);
    plain
        .args(pi_args)
        .env("PI_CODING_AGENT_DIR", &agent_home)
        .env("PI_OFFLINE", "1")
        .env("HOME", &home)
        .env(&var, &val);
    let plain_out = run(&mut plain);
    println!("--- 不套沙盒（对照） ---\n{plain_out}");
    assert!(
        plain_out.contains("mock-model-1"),
        "对照组就没列出：{plain_out}"
    );

    // 沙盒：第三方受限应用（deny_network=true），其余参数同 sandboxed_argv。
    let runtime_paths = super_agent_os::pi_bin::runtime_install_dirs().expect("runtime dirs");
    let sp = build_profile(&app_data, &[], &[], &runtime_paths, true, None).unwrap();
    let mut inner = vec![pi.to_string_lossy().to_string()];
    inner.extend(pi_args.iter().map(|s| s.to_string()));
    let argv = sandbox_exec_argv(&sp, &inner);
    let mut boxed = Command::new("/usr/bin/sandbox-exec");
    boxed
        .args(&argv)
        .current_dir(&app_data)
        .env("PI_CODING_AGENT_DIR", &agent_home)
        .env("PI_OFFLINE", "1")
        .env("HOME", &home)
        .env(&var, &val);
    let boxed_out = run(&mut boxed);
    println!("--- 沙盒内 ---\n{boxed_out}");
    assert!(
        boxed_out.contains("mock-model-1"),
        "沙盒内的 pi 读不到 agent home 里的 models.json：{boxed_out}"
    );
}
