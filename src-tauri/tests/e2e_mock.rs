// 验证：给定 mock-pi，send_prompt 后能从 RpcSession 收到 AssistantDelta 并聚合。
// 这里直接测 rpc 层的端到端（Tauri command 的 emit 在手动验证步骤覆盖）。
use std::time::Duration;
use super_agent_os::rpc::{PiEvent, RpcSession};
use super_agent_os::supervisor::Backoff;
use super_agent_os::usage::UsageAccumulator;
use tempfile::tempdir;
use tokio::sync::Mutex;

// `std::env::set_var`/`remove_var` 改变进程全局状态，本文件两个测试都要改
// `SUPERAGENT_PI_BIN`/`MOCK_PI_MODE`；不序列化会在同一测试二进制内并行竞争
// （同 pi_bin.rs 里已验证过的教训）。
static ENV_LOCK: std::sync::LazyLock<Mutex<()>> = std::sync::LazyLock::new(|| Mutex::new(()));

#[tokio::test]
async fn auth_error_surfaces_as_provider_error() {
    let _guard = ENV_LOCK.lock().await;
    let dir = tempdir().unwrap();
    std::env::set_var("SUPERAGENT_PI_BIN", env!("CARGO_BIN_EXE_mock_pi"));
    std::env::set_var("MOCK_PI_MODE", "auth_error");
    let (session, mut rx) = RpcSession::spawn(dir.path(), vec![]).await.unwrap();
    session.send_prompt("hi").await.unwrap();

    let mut saw_error = false;
    while let Some(ev) = rx.recv().await {
        match ev {
            PiEvent::ProviderError(msg) => {
                assert!(msg.contains("401"));
                saw_error = true;
            }
            PiEvent::AgentEnded => break,
            _ => {}
        }
    }
    std::env::remove_var("MOCK_PI_MODE");
    assert!(saw_error);
}

/// 验证 lib.rs `start_main_session` 看护循环里的退避重启机制：mock-pi 在 crash
/// 模式下收到 prompt 后立即 `exit(1)`，rpc.rs 的 stdout 读取循环在 EOF 时 drop
/// tx，rx.recv() 应返回 None（看护循环据此判定"进程退出，需要重启"）。这里复刻
/// 看护循环同样的步骤——`Backoff::next_delay` 计算延迟、sleep、用同一
/// `session_dir` 重新 spawn——不经 Tauri AppHandle（那部分只能在真实 GUI 里手动
/// 验证），但验证的是完全相同的 RpcSession + Backoff 原语，证明"崩溃一次、重启
/// 一次、恢复正常对话"这条链路是通的。
#[tokio::test]
async fn crash_then_backoff_restart_recovers_session() {
    let _guard = ENV_LOCK.lock().await;
    let dir = tempdir().unwrap();
    std::env::set_var("SUPERAGENT_PI_BIN", env!("CARGO_BIN_EXE_mock_pi"));
    std::env::set_var("MOCK_PI_MODE", "crash");

    let (session, mut rx) = RpcSession::spawn(dir.path(), vec![]).await.unwrap();
    session.send_prompt("hi").await.unwrap();

    // crash 模式：mock-pi 收到 prompt 后 exit(1)，事件通道应关闭。
    assert!(
        rx.recv().await.is_none(),
        "crash 模式下 mock-pi 退出后，事件通道应关闭（看护循环重启的触发条件）"
    );

    // 看护循环遇到通道关闭时的处理：Backoff 给出延迟、sleep、重新 spawn。
    let mut backoff = Backoff::new();
    let delay = backoff
        .next_delay()
        .expect("第一次重启应在退避上限（5 次）之内，不应直接故障");

    std::env::set_var("MOCK_PI_MODE", "normal");
    tokio::time::sleep(delay).await;
    let (session2, mut rx2) = RpcSession::spawn(dir.path(), vec![])
        .await
        .expect("退避后重新 spawn 应成功（一次重启）");
    backoff.reset();

    session2.send_prompt("hi").await.unwrap();
    let mut text = String::new();
    let mut ended = false;
    while let Some(ev) = rx2.recv().await {
        match ev {
            PiEvent::AssistantDelta(d) => text.push_str(&d),
            PiEvent::AgentEnded => {
                ended = true;
                break;
            }
            _ => {}
        }
    }
    std::env::remove_var("MOCK_PI_MODE");
    assert_eq!(text, "你好，世界", "重启后的新会话应恢复正常对话");
    assert!(ended);
}

/// 读事件直到 `AgentEnded`（丢弃期间其它事件），供下面的用量测试复用——同
/// `crash_then_backoff_restart_recovers_session` 头部注释的做法一致：不经
/// Tauri AppHandle，直接复刻生产事件循环里"这一步该做什么"的同样步骤。
async fn wait_for_agent_end(rx: &mut tokio::sync::mpsc::Receiver<PiEvent>) {
    while let Some(ev) = rx.recv().await {
        if matches!(ev, PiEvent::AgentEnded) {
            return;
        }
    }
    panic!("通道提前关闭，未收到 AgentEnded");
}

/// 读事件直到 `SessionStats`（即 `get_session_stats` 命令的响应），返回其
/// (input, output, cost)。
async fn wait_for_session_stats(rx: &mut tokio::sync::mpsc::Receiver<PiEvent>) -> (u64, u64, f64) {
    while let Some(ev) = rx.recv().await {
        if let PiEvent::SessionStats {
            input,
            output,
            cost,
        } = ev
        {
            return (input, output, cost);
        }
    }
    panic!("通道提前关闭，未收到 SessionStats");
}

/// P3 Task18 修复的核心回归测试：驱动"一轮对话结束 → 发 get_session_stats →
/// 用响应更新用量"这条完整链路两次，断言最终用量是**最新**的累计值，而不是
/// 把两次响应的数字加总（Task16 的 `PiEvent::Usage`/`accumulate` 就是这个错误
/// 语义——且它假设的 `agent_end.usage` 字段在真实 pi 里根本不存在，生产环境
/// 永远收不到）。`mock_pi.rs` 的 `get_session_stats` 处理器第二次调用起返回
/// 更高的累计数字，模拟真实 pi `SessionStats` 语义（截至目前的累计值，非
/// 增量）。
#[tokio::test]
async fn get_session_stats_after_turn_reflects_latest_cumulative_not_summed() {
    let _guard = ENV_LOCK.lock().await;
    let dir = tempdir().unwrap();
    std::env::set_var("SUPERAGENT_PI_BIN", env!("CARGO_BIN_EXE_mock_pi"));
    std::env::set_var("MOCK_PI_MODE", "normal");

    let (session, mut rx) = RpcSession::spawn(dir.path(), vec![]).await.unwrap();
    let usage = UsageAccumulator::new();

    // 第一轮：prompt → agent_end → 查 get_session_stats → mock 返回第一次的
    // 累计值，写入 usage。
    session.send_prompt("hi").await.unwrap();
    wait_for_agent_end(&mut rx).await;
    session.send_get_session_stats().await.unwrap();
    let (i1, o1, c1) =
        tokio::time::timeout(Duration::from_secs(5), wait_for_session_stats(&mut rx))
            .await
            .expect("应在超时前收到第一次 SessionStats");
    usage.set_latest("app-a", i1, o1, c1).await;

    // 第二轮：mock_pi 内部计数器让第二次 get_session_stats 返回更高的累计值。
    session.send_prompt("hi").await.unwrap();
    wait_for_agent_end(&mut rx).await;
    session.send_get_session_stats().await.unwrap();
    let (i2, o2, c2) =
        tokio::time::timeout(Duration::from_secs(5), wait_for_session_stats(&mut rx))
            .await
            .expect("应在超时前收到第二次 SessionStats");
    assert!(
        i2 > i1 && o2 > o1,
        "mock 第二次应返回更高的累计值，测试前提不成立"
    );
    usage.set_latest("app-a", i2, o2, c2).await;

    let resp = usage.usage_response("app-a").await;
    // 关键断言：最终值是"最新"的 i2/o2/c2，不是 i1+i2 的求和。
    assert_eq!(resp.input, i2);
    assert_eq!(resp.output, o2);
    assert!((resp.cost - c2).abs() < 1e-9);

    std::env::remove_var("MOCK_PI_MODE");
}
