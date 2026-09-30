use super_agent_os::rpc::{PiEvent, RpcSession};
use tempfile::tempdir;

// 用编译出的 mock_pi 作为 pi 替身
fn mock_pi_path() -> String {
    // cargo 把集成测试的依赖 bin 放在 deps 同级；用 CARGO_BIN_EXE_ 提供的路径
    env!("CARGO_BIN_EXE_mock_pi").to_string()
}

#[tokio::test]
async fn streams_text_deltas_then_agent_end() {
    let dir = tempdir().unwrap();
    std::env::set_var("SUPERAGENT_PI_BIN", mock_pi_path());
    let (session, mut rx) = RpcSession::spawn(dir.path(), vec![]).await.unwrap();
    session.send_prompt("hi").await.unwrap();

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
    assert_eq!(text, "你好，世界");
    assert!(ended);
}
