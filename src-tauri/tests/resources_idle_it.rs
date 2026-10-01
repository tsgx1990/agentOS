// P6-F Task 1：`close_app_in` 的释放语义（会话 / 槽位 / 闸门 / 活动记录）。
use tokio::sync::Mutex;

use super_agent_os::app_state::AppState;
use super_agent_os::rpc::RpcSession;
use super_agent_os::session_mgr::close_app_in;

// `SUPERAGENT_PI_BIN` 是进程级全局状态，串行化访问（同 `scheduler_running_only_it.rs`）。
static ENV_LOCK: std::sync::LazyLock<Mutex<()>> = std::sync::LazyLock::new(|| Mutex::new(()));

/// 复刻 `open_app` 成功后的状态：mock_pi 会话 + 闸门名额 + 槽位 + 活动记录。
async fn open_fake(state: &AppState, app_id: &str) {
    let dir = tempfile::tempdir().unwrap();
    let (session, _rx) = RpcSession::spawn(dir.path(), vec![])
        .await
        .expect("mock_pi 应能拉起");
    assert!(state.gate.lock().await.try_acquire());
    state
        .slots
        .lock()
        .unwrap()
        .assign(app_id)
        .expect("应有空槽位");
    state
        .app_sessions
        .lock()
        .await
        .insert(app_id.to_string(), session);
    state.activity.on_open(app_id, 100);
    // 保持 tempdir 存活到会话被关闭之后无意义：mock_pi 不依赖该目录文件。
    std::mem::forget(dir);
    std::mem::forget(_rx);
}

#[tokio::test]
async fn close_app_in_releases_session_slot_gate_and_activity() {
    let _g = ENV_LOCK.lock().await;
    std::env::set_var("SUPERAGENT_PI_BIN", env!("CARGO_BIN_EXE_mock_pi"));
    let state = AppState::default();
    open_fake(&state, "app-a").await;

    assert!(close_app_in(&state, "app-a").await);

    assert!(!state.app_sessions.lock().await.contains_key("app-a"));
    assert_eq!(state.slots.lock().unwrap().slot_for_app("app-a"), None);
    assert_eq!(state.activity.get("app-a"), None);
    for i in 0..5 {
        assert!(
            state.gate.lock().await.try_acquire(),
            "第 {i} 次 try_acquire 应成功（名额已还）"
        );
    }
    std::env::remove_var("SUPERAGENT_PI_BIN");
}

#[tokio::test]
async fn close_app_in_unknown_app_is_noop() {
    let state = AppState::default();
    for _ in 0..5 {
        assert!(state.gate.lock().await.try_acquire());
    }
    assert!(!close_app_in(&state, "nope").await);
    assert!(
        !state.gate.lock().await.try_acquire(),
        "未打开的应用不得错放名额"
    );
}
