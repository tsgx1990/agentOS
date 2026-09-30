// P6-A Task 7 集成测试：`mcp_socket::process_request` 改走 `CapabilityRegistry::dispatch`
// 之后，`McpSocketListener::start_with_identity` 绑定的完整身份（`CallerIdentity`）与
// 清单权限（`Permissions`）在线上请求里生效——未声明能力的方法被拒且留审计（verdict
// "denied"），未知方法给出明确错误，声明了能力的方法真的能落地执行。
//
// socket 帧发送方式照 `tests/maker_socket_it.rs` 里既有的假客户端手法抄一份到本文件
// 顶部：连一次、写一行 `{method,params}` JSON 请求、读一行 JSON 响应、关连接——与
// `mcp_transport.ts::hostMcpCall` 的线协议保持一致。
use std::sync::Arc;
use super_agent_os::capabilities;
use super_agent_os::capability::CallerIdentity;
use super_agent_os::mcp::McpManager;
use super_agent_os::mcp_socket::McpSocketListener;
use super_agent_os::paths::DataLayout;
use super_agent_os::permissions::Permissions;

async fn send(sock: &std::path::Path, frame: &str) -> serde_json::Value {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let mut s = tokio::net::UnixStream::connect(sock).await.unwrap();
    s.write_all(format!("{frame}\n").as_bytes()).await.unwrap();
    let mut line = String::new();
    BufReader::new(s).read_line(&mut line).await.unwrap();
    serde_json::from_str(&line).unwrap()
}

#[tokio::test]
async fn undeclared_notify_is_denied_and_audited_unknown_method_is_named() {
    let tmp = tempfile::tempdir().unwrap();
    let layout = DataLayout::new(tmp.path().to_path_buf());
    let mcp = McpManager::new();
    let sock = layout.mcp_socket_path("plain");
    let l = McpSocketListener::start_with_identity(
        mcp.clone(),
        layout.clone(),
        CallerIdentity {
            app_id: "plain".into(),
            trusted: false,
            depth: 0,
        },
        Permissions::default(),
        sock.clone(),
        None,
        Arc::new(capabilities::builtin()),
    )
    .unwrap();
    let r = send(
        &sock,
        r#"{"method":"__host_notify__","params":{"title":"t","body":"b"}}"#,
    )
    .await;
    assert_eq!(r["ok"], false);
    assert!(r["error"]
        .as_str()
        .unwrap()
        .contains("未声明能力 system.notifications"));
    let audits = super_agent_os::audit::query(
        &layout,
        &super_agent_os::audit::AuditFilter {
            app_id: Some("plain".into()),
            tool: Some("__host_notify__".into()),
            ..Default::default()
        },
    );
    assert!(audits.iter().any(|a| a.verdict == "denied"), "{audits:?}");
    let u = send(&sock, r#"{"method":"__host_nope__","params":{}}"#).await;
    assert!(u["error"]
        .as_str()
        .unwrap()
        .contains("未知的宿主方法 __host_nope__"));
    l.stop().await;
}

#[tokio::test]
async fn declared_notify_over_socket_lands_in_notification_center() {
    let tmp = tempfile::tempdir().unwrap();
    let layout = DataLayout::new(tmp.path().to_path_buf());
    let mcp = McpManager::new();
    let sock = layout.mcp_socket_path("noisy");
    let mut p = Permissions::default();
    p.system.notifications = true;
    let l = McpSocketListener::start_with_identity(
        mcp.clone(),
        layout.clone(),
        CallerIdentity {
            app_id: "noisy".into(),
            trusted: false,
            depth: 0,
        },
        p,
        sock.clone(),
        None,
        Arc::new(capabilities::builtin()),
    )
    .unwrap();
    let r = send(
        &sock,
        r#"{"method":"__host_notify__","params":{"title":"早报","body":"三件事"}}"#,
    )
    .await;
    assert_eq!(r["ok"], true, "{r}");
    let store = super_agent_os::notifications::NotificationStore::new(layout.clone(), mcp.clone());
    let list = store.list(&super_agent_os::notifications::NotificationFilter {
        app_id: Some("noisy".into()),
        ..Default::default()
    });
    assert_eq!(list.len(), 1);
    l.stop().await;
}

/// M-2（code review）：拒绝路径落审计的 `args` 必须截断到至多 4096 字符——一个
/// 没声明任何能力的 app（这里绑定空 `Permissions`）在必被拒的请求里塞进一个
/// ~20000 字符的 `body`，落盘的审计记录不能把这坨内容原样写进去，否则宿主
/// 共享的审计目录会被没声明任何能力的调用方当放大器写爆（见
/// `mcp_socket.rs::process_request` 里对应注释）。
#[tokio::test]
async fn undeclared_notify_deny_audit_truncates_args_to_4096_chars() {
    let tmp = tempfile::tempdir().unwrap();
    let layout = DataLayout::new(tmp.path().to_path_buf());
    let mcp = McpManager::new();
    let sock = layout.mcp_socket_path("hoarder");
    let l = McpSocketListener::start_with_identity(
        mcp.clone(),
        layout.clone(),
        CallerIdentity {
            app_id: "hoarder".into(),
            trusted: false,
            depth: 0,
        },
        Permissions::default(),
        sock.clone(),
        None,
        Arc::new(capabilities::builtin()),
    )
    .unwrap();
    let big_body = "x".repeat(20_000);
    let frame = serde_json::json!({
        "method": "__host_notify__",
        "params": { "title": "t", "body": big_body },
    })
    .to_string();
    let r = send(&sock, &frame).await;
    assert_eq!(r["ok"], false);
    assert!(r["error"]
        .as_str()
        .unwrap()
        .contains("未声明能力 system.notifications"));
    let audits = super_agent_os::audit::query(
        &layout,
        &super_agent_os::audit::AuditFilter {
            app_id: Some("hoarder".into()),
            tool: Some("__host_notify__".into()),
            ..Default::default()
        },
    );
    let denied = audits
        .iter()
        .find(|a| a.verdict == "denied")
        .expect("应有一条 denied 审计");
    assert!(
        denied.args.chars().count() <= 4096,
        "args 长度应 <= 4096，实际 {}",
        denied.args.chars().count()
    );
    l.stop().await;
}
