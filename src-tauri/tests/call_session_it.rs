// P5 T2：`session_mgr::spawn_call_session`（agent 互联的被调方会话）集成测试。
//
// 复用 mock_pi（`CARGO_BIN_EXE_mock_pi`）替身 pi，keyless、不碰真实 keychain；
// 走的是与定时任务 task-mode 会话完全相同的公共内核 `run_headless_session`
// （P5 §9 收敛设计：spawn_task_session / spawn_call_session 共用一份内核）。
//
// `SUPERAGENT_PI_BIN`/`MOCK_PI_MODE` 是进程级全局状态，本文件用 ENV_LOCK 序列化
// （惯例见 scheduler_it.rs / maker_preview_it.rs 头部注释）。

use tokio::sync::Mutex;

use super_agent_os::mcp::McpManager;
use super_agent_os::paths::DataLayout;
use super_agent_os::registry::InstalledApp;

static ENV_LOCK: std::sync::LazyLock<Mutex<()>> = std::sync::LazyLock::new(|| Mutex::new(()));

fn installed_app(id: &str, trusted: bool) -> InstalledApp {
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

fn hosttools_dir() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("hosttools")
}

/// 主路径：被调方（无 connectors / 无 agents.call）被 headless 拉起、跑到 agent_end、
/// 助手文本原样收全回传成 `CallResult{ok:true, text, error:None}`。
#[tokio::test]
async fn spawn_call_session_runs_callee_and_returns_text() {
    let _guard = ENV_LOCK.lock().await;
    std::env::set_var("SUPERAGENT_PI_BIN", env!("CARGO_BIN_EXE_mock_pi"));

    let tmp = tempfile::tempdir().unwrap();
    let layout = DataLayout::new(tmp.path().to_path_buf());

    // 被调方 trusted=false：走 macOS L2 沙盒包裹路径（与 scheduler task-mode 同规则）。
    // 关键点：spawn_call_session 只吃 `callee` 一个应用参数——结构上无"调用方"入口，
    // 被调方一切约束（trusted/沙盒）都取它自己的记录，权限不可能随调用链放大。
    let callee = installed_app("superagent__summarizer", false);

    let result = super_agent_os::session_mgr::spawn_call_session(
        &layout,
        &hosttools_dir(),
        &McpManager::new(),
        &callee,
        "把这段精简成 3 条要点",
        1, // depth：调用方(depth0) 直接调起 → 被调方 depth1
    )
    .await
    .expect("被调方会话应正常拉起并跑完");

    std::env::remove_var("SUPERAGENT_PI_BIN");

    assert!(result.ok, "mock_pi 默认 mode 正常走完，ok 应为 true");
    assert_eq!(
        result.text, "你好，世界",
        "应把被调方 mock_pi 的助手文本收全"
    );
    assert!(result.error.is_none());
}

/// 被调方以自己的 `trusted=true` 运行也能跑通（证明 trusted 取自 callee 记录，
/// 而非任何外部注入）——与上例仅 `trusted` 不同，spawn 内部据此走不同沙盒决策，
/// 两者都应正常返回文本。
#[tokio::test]
async fn spawn_call_session_uses_callee_own_trusted_flag() {
    let _guard = ENV_LOCK.lock().await;
    std::env::set_var("SUPERAGENT_PI_BIN", env!("CARGO_BIN_EXE_mock_pi"));

    let tmp = tempfile::tempdir().unwrap();
    let layout = DataLayout::new(tmp.path().to_path_buf());
    let callee = installed_app("superagent__trusted-callee", true);

    let result = super_agent_os::session_mgr::spawn_call_session(
        &layout,
        &hosttools_dir(),
        &McpManager::new(),
        &callee,
        "hi",
        1,
    )
    .await
    .expect("trusted 被调方会话应正常拉起");

    std::env::remove_var("SUPERAGENT_PI_BIN");

    assert!(result.ok);
    assert_eq!(result.text, "你好，世界");
}
