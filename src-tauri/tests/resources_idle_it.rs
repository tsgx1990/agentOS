// P6-F Task 1：`close_app_in` 的释放语义（会话 / 槽位 / 闸门 / 活动记录）。
use tokio::sync::Mutex;

use super_agent_os::app_state::AppState;
use super_agent_os::approvals::ApprovalStore;
use super_agent_os::idle::{plan_idle_recycle, recycle, IdlePolicy, IdlePolicyStore};
use super_agent_os::mcp::McpManager;
use super_agent_os::notifications::{NotificationFilter, NotificationStore};
use super_agent_os::paths::DataLayout;
use super_agent_os::rpc::RpcSession;
use super_agent_os::session_mgr::{close_app_in, HeadlessGuard};

// `SUPERAGENT_PI_BIN` 是进程级全局状态，串行化访问（同 `scheduler_running_only_it.rs`）。
static ENV_LOCK: std::sync::LazyLock<Mutex<()>> = std::sync::LazyLock::new(|| Mutex::new(()));

/// 复刻 `open_app` 成功后的状态：mock_pi 会话 + 闸门名额 + 槽位 + 活动记录。
async fn open_fake(state: &AppState, app_id: &str, opened_at: i64) {
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
    state.activity.on_open(app_id, opened_at);
    // 保持 tempdir 存活到会话被关闭之后无意义：mock_pi 不依赖该目录文件。
    std::mem::forget(dir);
    std::mem::forget(_rx);
}

#[tokio::test]
async fn close_app_in_releases_session_slot_gate_and_activity() {
    let _g = ENV_LOCK.lock().await;
    std::env::set_var("SUPERAGENT_PI_BIN", env!("CARGO_BIN_EXE_mock_pi"));
    let state = AppState::default();
    open_fake(&state, "app-a", 100).await;

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

const APP: &str = "app-idle";

struct Env {
    _tmp: tempfile::TempDir,
    layout: DataLayout,
    state: AppState,
}

async fn setup(opened_at: i64) -> Env {
    std::env::set_var("SUPERAGENT_PI_BIN", env!("CARGO_BIN_EXE_mock_pi"));
    let tmp = tempfile::tempdir().unwrap();
    let layout = DataLayout::new(tmp.path().to_path_buf());
    let state = AppState::default();
    open_fake(&state, APP, opened_at).await;
    Env {
        _tmp: tmp,
        layout,
        state,
    }
}

#[tokio::test]
async fn idle_app_is_recycled_notified_and_marked_dormant() {
    let _g = ENV_LOCK.lock().await;
    let e = setup(0).await;
    let picks = plan_idle_recycle(&e.state, &e.layout, 960).await;
    assert_eq!(picks.len(), 1);
    assert_eq!(picks[0].app_id, APP);
    let c = picks.into_iter().next().unwrap();
    let store = NotificationStore::new(e.layout.clone(), McpManager::new());
    let done = recycle(&e.state, &store, vec![(c, Some(1234))], 960).await;
    assert_eq!(done.len(), 1);
    assert_eq!(done[0].freed_rss_bytes, Some(1234));
    assert!(!e.state.app_sessions.lock().await.contains_key(APP));
    assert_eq!(e.state.activity.dormant(), vec![APP.to_string()]);
    let list = store.list(&NotificationFilter::default());
    assert!(list
        .iter()
        .any(|n| n.kind == "update" && n.title == "已休眠" && n.app_id == APP));
    for i in 0..5 {
        assert!(e.state.gate.lock().await.try_acquire(), "第 {i} 次应成功");
    }
}

#[tokio::test]
async fn recent_touch_keeps_app() {
    let _g = ENV_LOCK.lock().await;
    let e = setup(0).await;
    e.state.activity.touch(APP, 890);
    assert!(plan_idle_recycle(&e.state, &e.layout, 960).await.is_empty());
    close_app_in(&e.state, APP).await;
}

#[tokio::test]
async fn in_turn_keeps_app() {
    let _g = ENV_LOCK.lock().await;
    let e = setup(0).await;
    e.state.activity.begin_turn(APP, 0);
    assert!(plan_idle_recycle(&e.state, &e.layout, 5000)
        .await
        .is_empty());
    close_app_in(&e.state, APP).await;
}

#[tokio::test]
async fn pending_approval_keeps_app() {
    let _g = ENV_LOCK.lock().await;
    let e = setup(0).await;
    ApprovalStore::new(e.layout.clone())
        .stage(
            APP,
            "srv",
            "write_file",
            serde_json::json!({ "path": "/tmp/x" }),
            1000,
        )
        .expect("stage 应成功");
    assert!(plan_idle_recycle(&e.state, &e.layout, 5000)
        .await
        .is_empty());
    close_app_in(&e.state, APP).await;
}

#[tokio::test]
async fn background_session_keeps_app() {
    let _g = ENV_LOCK.lock().await;
    let e = setup(0).await;
    let g = HeadlessGuard::register(APP, None);
    assert!(plan_idle_recycle(&e.state, &e.layout, 5000)
        .await
        .is_empty());
    drop(g);
    assert_eq!(plan_idle_recycle(&e.state, &e.layout, 5000).await.len(), 1);
    close_app_in(&e.state, APP).await;
}

#[tokio::test]
async fn policy_disabled_or_exempt_keeps_app() {
    let _g = ENV_LOCK.lock().await;
    let e = setup(0).await;
    let store = IdlePolicyStore::new(&e.layout);
    store
        .save(&IdlePolicy {
            enabled: false,
            ..IdlePolicy::default()
        })
        .unwrap();
    assert!(plan_idle_recycle(&e.state, &e.layout, 5000)
        .await
        .is_empty());
    store
        .save(&IdlePolicy {
            exempt_apps: [APP.to_string()].into_iter().collect(),
            ..IdlePolicy::default()
        })
        .unwrap();
    assert!(plan_idle_recycle(&e.state, &e.layout, 5000)
        .await
        .is_empty());
    close_app_in(&e.state, APP).await;
}

#[tokio::test]
async fn touch_between_plan_and_recycle_aborts() {
    let _g = ENV_LOCK.lock().await;
    let e = setup(0).await;
    let picks = plan_idle_recycle(&e.state, &e.layout, 960).await;
    assert_eq!(picks.len(), 1);
    e.state.activity.touch(APP, 960);
    let store = NotificationStore::new(e.layout.clone(), McpManager::new());
    let done = recycle(
        &e.state,
        &store,
        picks.into_iter().map(|c| (c, None)).collect(),
        960,
    )
    .await;
    assert!(done.is_empty());
    assert!(e.state.app_sessions.lock().await.contains_key(APP));
    assert!(e.state.activity.dormant().is_empty());
    close_app_in(&e.state, APP).await;
}

#[tokio::test]
async fn recycled_app_can_reopen() {
    let _g = ENV_LOCK.lock().await;
    let e = setup(0).await;
    let picks = plan_idle_recycle(&e.state, &e.layout, 960).await;
    let store = NotificationStore::new(e.layout.clone(), McpManager::new());
    recycle(
        &e.state,
        &store,
        picks.into_iter().map(|c| (c, None)).collect(),
        960,
    )
    .await;
    assert!(e.state.gate.lock().await.try_acquire());
    assert!(e.state.slots.lock().unwrap().assign(APP).is_some());
    e.state.activity.on_open(APP, 1000);
    assert!(e.state.activity.dormant().is_empty());
}
