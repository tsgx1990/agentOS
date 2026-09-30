// 集成测试（P2 里程碑，macOS）：证明 `RpcSession::spawn_wrapped` 把 mock_pi 包进真实
// `/usr/bin/sandbox-exec` 之后，JSONL stdin/stdout 管道仍然通畅——sandbox-exec 不会
// 打断父子进程间的管道，事件流仍能正确解析、分类、经 channel 送达调用方。
//
// 这与 `sandbox_startup_it.rs`（只断言 mock_pi 不被信号杀）、`sandbox_escape_it.rs`
// （断言真实越权尝试被拒）互补：本文件断言的是"正常路径下功能不受影响"——沙盒必须
// 既挡住越权，又不破坏合法的 JSONL 通信，两者缺一都不算达标。
#![cfg(target_os = "macos")]
use super_agent_os::rpc::{PiEvent, RpcSession};
use super_agent_os::sandbox::{build_profile, sandbox_exec_argv};
use tokio::sync::Mutex;

// `MOCK_PI_MODE` 是进程级全局状态，与其它测试文件共享同一惯例：串行化访问防止并行
// 测试间的环境变量竞争（`pi_bin.rs`/`e2e_mock.rs` 已验证过的教训）。
static ENV_LOCK: std::sync::LazyLock<Mutex<()>> = std::sync::LazyLock::new(|| Mutex::new(()));

/// 经 `/usr/bin/sandbox-exec` 包一层 L2 沙盒起 mock_pi，走一次完整的
/// prompt → AssistantDelta* → AgentEnded 流程，验证管道未被沙盒破坏。
#[tokio::test]
async fn jsonl_pipe_survives_sandbox_exec() {
    let _guard = ENV_LOCK.lock().await;
    let app_data = tempfile::tempdir().unwrap();

    // 受限（deny_network=true）profile：与 open_app 在 macOS 上对第三方应用的
    // 实际用法一致；本用例不涉及网络，禁网与否不影响管道本身是否通畅。
    let sp = build_profile(app_data.path(), &[], &[], &[], true, None).unwrap();
    let inner = vec![
        env!("CARGO_BIN_EXE_mock_pi").to_string(),
        "--mode".to_string(),
        "rpc".to_string(),
    ];
    let argv = sandbox_exec_argv(&sp, &inner);

    let (session, mut rx) =
        RpcSession::spawn_wrapped("/usr/bin/sandbox-exec", argv, app_data.path(), vec![], None)
            .await
            .expect("经 sandbox-exec 包 mock_pi 应能 spawn 成功");

    session
        .send_prompt("hi")
        .await
        .expect("沙盒内 stdin 写入不应失败");

    let mut text = String::new();
    let mut ended = false;
    while let Some(ev) = rx.recv().await {
        match ev {
            PiEvent::AssistantDelta(d) => text.push_str(&d),
            PiEvent::AgentEnded => {
                ended = true;
                break;
            }
            _ => {}
        }
    }
    assert_eq!(
        text, "你好，世界",
        "沙盒内 mock_pi 的 stdout JSONL 应被完整解析"
    );
    assert!(ended, "应收到 agent_end 事件（沙盒未打断管道）");
}

/// 补充：`__host_ui_emit__` 的 `tool_execution_end` 在沙盒内同样应被正确分类为
/// `PiEvent::UiEmit`——覆盖比纯文本 delta 更贴近真实应用（ui_emit.ts 宿主工具）
/// 的事件形状，避免"文本流通但结构化工具事件被沙盒/管道分帧破坏"的漏测。
#[tokio::test]
async fn jsonl_pipe_survives_sandbox_exec_for_ui_emit() {
    let _guard = ENV_LOCK.lock().await;
    std::env::set_var("MOCK_PI_MODE", "ui_emit");
    let app_data = tempfile::tempdir().unwrap();

    let sp = build_profile(app_data.path(), &[], &[], &[], true, None).unwrap();
    let inner = vec![
        env!("CARGO_BIN_EXE_mock_pi").to_string(),
        "--mode".to_string(),
        "rpc".to_string(),
    ];
    let argv = sandbox_exec_argv(&sp, &inner);

    let (session, mut rx) =
        RpcSession::spawn_wrapped("/usr/bin/sandbox-exec", argv, app_data.path(), vec![], None)
            .await
            .expect("经 sandbox-exec 包 mock_pi(ui_emit 模式) 应能 spawn 成功");
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
    assert!(saw, "沙盒内也应收到 __host_ui_emit__ 的 UiEmit 事件");
}
