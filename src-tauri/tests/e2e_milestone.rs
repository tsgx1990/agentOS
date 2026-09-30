// P1 里程碑 e2e 验证："装→开→ui_emit→state 持久" 观察路径的最小自动化证据：
// 1. prompt_yields_ui_emit_event：走真实 RpcSession + mock_pi(ui_emit 模式)，
//    验证宿主工具 __host_ui_emit__ 的 tool_execution_end 能被 rpc::classify
//    归类为 PiEvent::UiEmit 并通过 channel 送达调用方（对应"应用主动上报 UI
//    观察事件"这条链路）。
// 2. state_persists_across_reopen：验证 state_store 写入的数据在"重启"
//    （同一 root 目录上新建一个 DataLayout 实例）后仍可读到，模拟应用状态
//    跨进程重启持久化。
// 3. prompt_yields_tool_executed_event（P2）：走真实 RpcSession + mock_pi
//    (tool_audit 模式)，验证任意其它工具（非 __host_ui_emit__）的
//    tool_execution_end 能被 rpc::classify 归类为 PiEvent::ToolExecuted——
//    这是 session_mgr.rs/lib.rs 里 audit::record 审计接线实际消费的信号。
use super_agent_os::paths::DataLayout;
use super_agent_os::rpc::{PiEvent, RpcSession};
use super_agent_os::state_store;
use tokio::sync::Mutex;

// `std::env::set_var`/`remove_var` 改变进程全局状态（非线程本地）：本文件现在有
// 两个都要改 `SUPERAGENT_PI_BIN`/`MOCK_PI_MODE` 的 `#[tokio::test]`，`cargo test`
// 默认并行跑测试线程，不加锁会在同一测试二进制内并行竞争（同 tests/e2e_mock.rs
// 已踩过、已注释过的教训——mock_pi 拿到错的 MOCK_PI_MODE 后不会输出期望的事件，
// 而它在处理完一条 prompt 后仍会阻塞等下一行 stdin、不会自己退出，rx.recv() 也就
// 永远等不到 None，测试直接挂起而不是快速失败）。
static ENV_LOCK: std::sync::LazyLock<Mutex<()>> = std::sync::LazyLock::new(|| Mutex::new(()));

#[tokio::test]
async fn prompt_yields_ui_emit_event() {
    let _guard = ENV_LOCK.lock().await;
    std::env::set_var("SUPERAGENT_PI_BIN", env!("CARGO_BIN_EXE_mock_pi"));
    std::env::set_var("MOCK_PI_MODE", "ui_emit");
    let tmp = tempfile::tempdir().unwrap();
    let (session, mut rx) = RpcSession::spawn_with(tmp.path(), vec![], vec![])
        .await
        .unwrap();
    session.send_prompt("买牛奶").await.unwrap();
    let mut saw = false;
    while let Some(ev) = rx.recv().await {
        if let PiEvent::UiEmit { event, .. } = ev {
            assert_eq!(event, "items_update");
            saw = true;
            break;
        }
    }
    std::env::remove_var("MOCK_PI_MODE");
    assert!(saw, "应收到 __host_ui_emit__ 的 UiEmit 事件");
}

#[tokio::test]
async fn prompt_yields_tool_executed_event() {
    let _guard = ENV_LOCK.lock().await;
    std::env::set_var("SUPERAGENT_PI_BIN", env!("CARGO_BIN_EXE_mock_pi"));
    std::env::set_var("MOCK_PI_MODE", "tool_audit");
    let tmp = tempfile::tempdir().unwrap();
    let (session, mut rx) = RpcSession::spawn_with(tmp.path(), vec![], vec![])
        .await
        .unwrap();
    session.send_prompt("写个文件").await.unwrap();
    let mut saw = false;
    while let Some(ev) = rx.recv().await {
        if let PiEvent::ToolExecuted {
            tool_name,
            args,
            is_error,
        } = ev
        {
            assert_eq!(tool_name, "write");
            assert_eq!(args["path"], "/data/apps/x/notes.json");
            assert!(!is_error);
            saw = true;
            break;
        }
    }
    std::env::remove_var("MOCK_PI_MODE");
    assert!(
        saw,
        "非 __host_ui_emit__ 的工具执行完毕也应产生 PiEvent::ToolExecuted（供 audit::record 消费）"
    );
}

#[test]
fn state_persists_across_reopen() {
    let root = tempfile::tempdir().unwrap();
    let l1 = DataLayout::new(root.path().to_path_buf());
    l1.ensure_app("x").unwrap();
    state_store::set(
        &l1,
        "x",
        "items",
        &serde_json::json!([{ "text": "买牛奶", "done": false }]),
    )
    .unwrap();
    let l2 = DataLayout::new(root.path().to_path_buf()); // 模拟重启
    assert_eq!(
        state_store::get(&l2, "x", "items").unwrap()[0]["text"],
        "买牛奶"
    );
}
