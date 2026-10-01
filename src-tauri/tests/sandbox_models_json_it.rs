//! 真实 pi + 真实 sandbox-exec：沙盒应用的 pi 必须能写自己的 agent home 与会话目录。
//!
//! 背景（缺陷）：pi 启动时要在 `PI_CODING_AGENT_DIR`（`<root>/agenthome/<app>`）里建
//! `trust.json` 的锁、读写凭据存储、加载 `models.json`，会话文件写在
//! `PI_CODING_AGENT_SESSION_DIR`（`<root>/sessions/<app>`）。这两个目录都不在 `$APP_DATA`
//! （`<root>/apps/<app>`）里，若不进沙盒的可写白名单，真实安装（数据根在
//! `~/Library/Application Support/...`）下 pi 启动阶段就崩溃。以前的真实 pi 测试全部把数据根
//! 放在 `/private/var/folders/...` 下，而基础沙盒规则放行了 `/private/var` 的读取，缺陷被掩盖。
//!
//! 本文件的数据根建在 `CARGO_TARGET_TMPDIR`（target 目录下，本机与 CI 都在用户目录里），
//! 与真实安装的路径形态一致，并且启动参数全部来自生产函数 `build_launch` /
//! `assemble_launch_plan`（沙盒 profile 的读写白名单取自 `LaunchPlan`，与 `sandboxed_argv`
//! 对同一份计划做的事一致；这里不能直接调 `sandboxed_argv`，因为它把内层命令固定成
//! `--mode rpc`，而本测试要跑一次性的 `--mode json -p`）。
//!
//! 需要本机 pi 与 macOS：
//! `SUPERAGENT_PI_BIN=<pi 路径> cargo test --test sandbox_models_json_it -- --ignored --nocapture`。
#![cfg(target_os = "macos")]
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use super_agent_os::capability::LaunchContribution;
use super_agent_os::model_overrides::{model_launch, EffectiveModel, ModelSource};
use super_agent_os::paths::DataLayout;
use super_agent_os::providers::{ApiKind, CustomProvider};
use super_agent_os::registry::InstalledApp;
use super_agent_os::sandbox::{build_profile, sandbox_exec_argv};
use super_agent_os::session_mgr::{assemble_launch_plan, build_settings_json, LaunchPlan};

/// 真实安装形态的数据根：建在 `CARGO_TARGET_TMPDIR` 下（不在 `/private/var` 里）。
fn real_shape_root() -> tempfile::TempDir {
    let base = PathBuf::from(env!("CARGO_TARGET_TMPDIR"));
    std::fs::create_dir_all(&base).unwrap();
    let dir = tempfile::Builder::new()
        .prefix("sandbox-it-")
        .tempdir_in(&base)
        .unwrap();
    let canon = std::fs::canonicalize(dir.path()).unwrap();
    assert!(
        !canon.starts_with("/private/var") && !canon.starts_with("/var"),
        "数据根不能落在 /private/var 下（基础沙盒规则放行它的读取，会掩盖缺陷）：{canon:?}"
    );
    dir
}

/// 读完整个请求（头 + 按 Content-Length 的体），返回请求头原文。
fn read_head(stream: &mut TcpStream) -> String {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 1024];
    loop {
        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&buf[..pos]).to_string();
            let len = head
                .lines()
                .find_map(|l| {
                    l.to_lowercase()
                        .strip_prefix("content-length:")
                        .map(|v| v.trim().parse::<usize>().unwrap_or(0))
                })
                .unwrap_or(0);
            if buf.len() >= pos + 4 + len {
                return head;
            }
        }
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => return String::from_utf8_lossy(&buf).to_string(),
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
    }
}

/// 夹具：对每个请求回一条最小的 OpenAI Chat Completions SSE 成功响应，记录请求头。
fn spawn_mock_openai() -> (String, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let seen2 = seen.clone();
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let head = read_head(&mut stream);
            seen2.lock().unwrap().push(head);
            let body = concat!(
                "data: {\"id\":\"c1\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"mock-model-1\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"pong-from-mock\"},\"finish_reason\":null}]}\n\n",
                "data: {\"id\":\"c1\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"mock-model-1\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":1,\"completion_tokens\":1,\"total_tokens\":2}}\n\n",
                "data: [DONE]\n\n"
            );
            let header = format!(
                "HTTP/1.0 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(header.as_bytes());
            let _ = stream.write_all(body.as_bytes());
            let _ = stream.flush();
        }
    });
    (format!("http://{addr}"), seen)
}

fn installed(id: &str, trusted: bool) -> InstalledApp {
    InstalledApp {
        app_id: id.into(),
        name: id.into(),
        version: "1.0.0".into(),
        display_name: id.into(),
        category: "life".into(),
        icon: None,
        trusted,
        domains: vec![],
    }
}

/// 能力插件会经 `-e` 加载的 hosttools 文件（含 `build_launch` 固定加的 permission_gate.ts）。
const BRIDGES: [(&str, &str); 7] = [
    ("permission_gate.ts", "GATE-LOADED-MARKER"),
    ("call_agent_bridge.ts", "BRIDGE-call_agent-LOADED"),
    ("maker_bridge.ts", "BRIDGE-maker-LOADED"),
    ("mcp_bridge.ts", "BRIDGE-mcp-LOADED"),
    ("router_bridge.ts", "BRIDGE-router-LOADED"),
    ("notify_bridge.ts", "BRIDGE-notify-LOADED"),
    ("ui_emit.ts", "BRIDGE-ui_emit-LOADED"),
];

struct Outcome {
    status: std::process::ExitStatus,
    stdout: String,
    stderr: String,
    layout: DataLayout,
}

/// 按生产路径的做法拉起沙盒 pi 跑一轮 `--mode json -p`。
fn run_sandboxed_turn(root: &Path, trusted: bool, base_url: &str) -> Outcome {
    let layout = DataLayout::new(root.to_path_buf());
    let app = installed("a", trusted);
    // 与生产一致：先建应用目录与会话目录，再写 agent home（settings.json / models.json）。
    layout.ensure_app("a").unwrap();
    let agent_home = layout.agent_home_dir("a");
    std::fs::create_dir_all(&agent_home).unwrap();
    // 应用包里的 persona（`build_launch` 会把它作为 --append-system-prompt 传给 pi）。
    let persona = layout.packages_dir("a").join("agent/persona.md");
    std::fs::create_dir_all(persona.parent().unwrap()).unwrap();
    std::fs::write(&persona, "你是测试助手。").unwrap();

    let custom = [CustomProvider {
        id: "custom-mock".into(),
        display: "Mock".into(),
        base_url: format!("{base_url}/v1"),
        api: ApiKind::OpenAiCompletions,
        models: vec!["mock-model-1".into()],
    }];
    let eff = EffectiveModel {
        provider: Some("custom-mock".into()),
        model: Some("mock-model-1".into()),
        source: ModelSource::App,
    };
    let ml = model_launch(&eff, &custom, |_| Some("sk-test-fake".into()), true);
    // 仅测试：关掉 pi 的重试，免得网络被拒的对照组白等退避时间；其余 settings 取生产值。
    let mut settings = build_settings_json(&app, &layout, true);
    settings["retry"] = serde_json::json!({ "enabled": false });
    std::fs::write(
        agent_home.join("settings.json"),
        serde_json::to_string_pretty(&settings).unwrap(),
    )
    .unwrap();
    std::fs::write(
        agent_home.join("models.json"),
        serde_json::to_string_pretty(&ml.models_json.clone().expect("应生成 models.json")).unwrap(),
    )
    .unwrap();

    let manifest: super_agent_os::pkg::Manifest = serde_json::from_value(serde_json::json!({
        "name": "a", "version": "1.0.0",
        "engines": {"superagent-host": "*"},
        "superagent": {"schemaVersion": 1, "displayName": "a", "category": "life",
                       "ui": "ui/index.html", "permissions": "permissions.json"}
    }))
    .unwrap();
    // 假的 hosttools 目录（与真实安装一样不在沙盒基础白名单里）：每个扩展加载时
    // 往 stderr 打一个标记，用来证明沙盒里的 pi 真的读到并加载了它们。
    let hosttools = root.join("hosttools");
    std::fs::create_dir_all(&hosttools).unwrap();
    for (file, marker) in BRIDGES {
        std::fs::write(
            hosttools.join(file),
            format!("console.error(\"{marker}\");\nexport default function (pi: any) {{}}\n"),
        )
        .unwrap();
    }
    let contribution = LaunchContribution {
        bridges: BRIDGES
            .iter()
            .map(|(f, _)| *f)
            .filter(|f| *f != "permission_gate.ts")
            .collect(),
        ..Default::default()
    };
    let plan: LaunchPlan = assemble_launch_plan(
        &app,
        &manifest,
        &contribution,
        &layout,
        &hosttools,
        true,
        &ml,
    )
    .expect("启动计划应能拼装");

    let pi = super_agent_os::pi_bin::resolve_pi_bin();
    let runtime_paths = super_agent_os::pi_bin::runtime_install_dirs().expect("runtime dirs");
    let app_data = layout.private_dir("apps", "a").unwrap();
    let sp = build_profile(
        &app_data,
        &plan.sandbox_read,
        &plan.sandbox_write,
        &runtime_paths,
        !trusted,
        None,
    )
    .unwrap();
    let mut inner = vec![
        pi.to_string_lossy().to_string(),
        "--mode".into(),
        "json".into(),
        "-p".into(),
    ];
    inner.extend(plan.extra_args.iter().cloned());
    inner.push("hi".into());
    let argv = sandbox_exec_argv(&sp, &inner);
    let mut cmd = Command::new("/usr/bin/sandbox-exec");
    cmd.args(&argv)
        .current_dir(&app_data)
        .env(
            "PI_CODING_AGENT_SESSION_DIR",
            layout.private_dir("sessions", "a").unwrap(),
        )
        .env("PI_TELEMETRY", "0")
        .stdin(Stdio::null());
    for (k, v) in &plan.env {
        cmd.env(k, v);
    }
    let out = cmd.output().expect("应能运行 sandbox-exec");
    Outcome {
        status: out.status,
        stdout: String::from_utf8_lossy(&out.stdout).to_string(),
        stderr: String::from_utf8_lossy(&out.stderr).to_string(),
        layout,
    }
}

fn dir_files(dir: &Path) -> Vec<PathBuf> {
    fn walk(d: &Path, out: &mut Vec<PathBuf>) {
        if let Ok(rd) = std::fs::read_dir(d) {
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    walk(&p, out);
                } else {
                    out.push(p);
                }
            }
        }
    }
    let mut v = Vec::new();
    walk(dir, &mut v);
    v
}

/// 受信应用（`deny_network=false`）：完整链路——pi 在沙盒内启动、读到 models.json、
/// 请求夹具（带插值后的密钥）、拿到回复、会话文件写进自己的会话目录。
#[test]
#[ignore = "需要本机 pi 与 macOS sandbox-exec"]
fn trusted_sandboxed_app_runs_full_turn_with_custom_provider() {
    let root = real_shape_root();
    let (base, seen) = spawn_mock_openai();
    let o = run_sandboxed_turn(root.path(), true, &base);
    println!(
        "退出：{:?}\n--- stdout ---\n{}\n--- stderr ---\n{}",
        o.status, o.stdout, o.stderr
    );
    assert!(
        o.status.success(),
        "沙盒内 pi 应正常退出：{:?} {}",
        o.status,
        o.stderr
    );
    assert!(
        o.stdout.contains("pong-from-mock"),
        "应拿到夹具返回的 assistant 回复：{}",
        o.stdout
    );
    let heads = seen.lock().unwrap();
    assert!(!heads.is_empty(), "夹具应收到请求");
    assert!(
        heads[0].contains("Bearer sk-test-fake"),
        "应带插值后的密钥：{}",
        heads[0]
    );
    assert!(
        !o.stderr
            .contains("Could not read append system prompt file"),
        "应用包里的 persona.md 应可读：{}",
        o.stderr
    );
    for (_, marker) in BRIDGES {
        assert!(
            o.stderr.contains(marker),
            "hosttools 扩展 {marker} 应在沙盒里被加载（读不到时 pi 静默跳过）：{}",
            o.stderr
        );
    }
    let sessions = dir_files(&o.layout.session_dir("a"));
    println!("会话目录下的文件：{sessions:?}");
    assert!(
        !sessions.is_empty(),
        "-p 模式也应把会话写进 sessions/<id>/（沙盒可写）"
    );
}

/// 不受信应用（`deny_network=true`）对照：pi 必须能**启动**（不在信任锁 / agent home 的
/// `mkdir` 处因 EPERM 崩溃），模型请求因沙盒禁网而失败——这是已记录的边界：
/// 不受信应用的 pi 无法直接请求模型服务。断言失败原因是网络类，不是文件权限类。
#[test]
#[ignore = "需要本机 pi 与 macOS sandbox-exec"]
fn untrusted_sandboxed_app_starts_but_model_request_is_network_denied() {
    let root = real_shape_root();
    let (base, seen) = spawn_mock_openai();
    let o = run_sandboxed_turn(root.path(), false, &base);
    println!(
        "退出：{:?}\n--- stdout ---\n{}\n--- stderr ---\n{}",
        o.status, o.stdout, o.stderr
    );
    let all = format!("{}{}", o.stdout, o.stderr);
    // 只盯本修复相关的两个目录：agent home / 会话目录上的 EPERM 不应再出现。
    // （stderr 里仍有向上查找 CLAUDE.md 上下文文件的 EPERM 警告：cwd 祖先目录本就不放行，
    // 是 pi 的非致命警告。）
    let bad: Vec<&str> = all
        .lines()
        .filter(|l| l.contains("EPERM") && (l.contains("/agenthome/") || l.contains("/sessions/")))
        .collect();
    assert!(
        bad.is_empty(),
        "agent home / 会话目录应可写，不应有 EPERM：{bad:?}"
    );
    assert!(
        o.stdout.contains("\"type\":\"session\"") && o.stdout.contains("agent_start"),
        "pi 应已启动并开始一轮（不在信任锁处崩溃）：{all}"
    );
    assert!(
        !all.contains("pong-from-mock"),
        "禁网下不该拿到夹具回复：{all}"
    );
    assert!(
        seen.lock().unwrap().is_empty(),
        "禁网下夹具不该收到任何请求"
    );
    assert!(
        o.stdout.contains("\"stopReason\":\"error\"") && o.stdout.contains("Connection error"),
        "模型请求应因网络被拒而失败（Connection error）：{all}"
    );
}
