//! 真实 pi：连通性测试的端到端行为，并实测 `pi --mode json -p` 的事件形状。
//! 需要本机 pi；`SUPERAGENT_PI_BIN=<…>/binaries/pi/pi cargo test --test real_pi_probe_it -- --ignored`。
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;
use super_agent_os::model_overrides::{model_launch, EffectiveModel, ModelSource};
use super_agent_os::probe::{run_probe, ProbeKind};
use super_agent_os::providers::{ApiKind, CustomProvider};

/// 读完整个请求（请求头 + 按 Content-Length 的请求体），返回请求头原文（含请求行）。
/// 必须把请求体读完再回应，否则连接会被 reset，pi 看到的是套接字错误而不是 401 响应体。
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

/// 起一个对所有请求回 401 的夹具，返回 (base_url, 收到的请求头列表)。
fn spawn_401_server() -> (String, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let seen2 = seen.clone();
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let head = read_head(&mut stream);
            seen2.lock().unwrap().push(head);
            let body = br#"{"error":{"message":"Incorrect API key provided","type":"invalid_request_error"}}"#;
            let header = format!(
                "HTTP/1.0 401 Unauthorized\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(header.as_bytes());
            let _ = stream.write_all(body);
            let _ = stream.flush();
        }
    });
    (format!("http://{addr}"), seen)
}

fn launch_for(base_url: &str) -> super_agent_os::model_overrides::ModelLaunch {
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
    model_launch(&eff, &custom, |_| Some("sk-test-fake".into()), true)
}

#[tokio::test]
#[ignore = "需要本机 pi"]
async fn probe_reports_invalid_key_on_401() {
    let (base, seen) = spawn_401_server();
    let t = tempfile::tempdir().unwrap();
    let home = t.path().join("probe/1-1");
    let r = run_probe(
        &super_agent_os::pi_bin::resolve_pi_bin(),
        &home,
        "custom-mock",
        "mock-model-1",
        &launch_for(&base),
        Duration::from_secs(30),
    )
    .await;
    println!("报告：{r:?}");
    assert_eq!(r.kind, ProbeKind::InvalidKey, "{r:?}");
    assert!(!r.ok);
    assert!(!home.exists(), "临时 agent home 应已删除");
    let heads = seen.lock().unwrap();
    assert!(!heads.is_empty(), "夹具应收到请求");
    println!("夹具收到的第一个请求头：\n{}", heads[0]);
    assert!(
        heads[0].starts_with("POST /v1/chat/completions"),
        "{}",
        heads[0]
    );
    assert!(
        heads[0].contains("Bearer sk-test-fake"),
        "应带插值后的密钥：{}",
        heads[0]
    );
    assert!(!r.detail.contains("sk-test-fake"));
}

#[tokio::test]
#[ignore = "需要本机 pi"]
async fn probe_reports_network_when_port_closed() {
    // 绑定后立即关闭，拿到一个确定没人监听的端口。
    let port = {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let t = tempfile::tempdir().unwrap();
    let home = t.path().join("probe/1-2");
    let started = std::time::Instant::now();
    let r = run_probe(
        &super_agent_os::pi_bin::resolve_pi_bin(),
        &home,
        "custom-mock",
        "mock-model-1",
        &launch_for(&format!("http://127.0.0.1:{port}")),
        Duration::from_secs(30),
    )
    .await;
    println!("报告：{r:?}，耗时 {:?}", started.elapsed());
    assert_eq!(r.kind, ProbeKind::Network, "{r:?}");
    assert!(!home.exists());
}
