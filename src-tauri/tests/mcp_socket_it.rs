// Task9b 集成测试：mcp_socket::McpSocketListener —— 宿主侧 unix socket 监听器，
// 收 `mcp_transport.ts`（`hostMcpCall`）那种"连一次、写一行 JSON 请求、读一行
// JSON 响应、关连接"的调用，转发给 `McpManager::host_mcp_call`，再把
// `McpCallResult` 按 `mcp_transport.ts` 能解码的形状编码回去。
//
// 同 mcp_manager_it.rs/session_mgr_mcp_it.rs 的路径解析方式：`ServerConfig`
// 直接在测试里手工构造（不走 vault::* 生产自由函数，避免碰真实 keychain 卡死
// `cargo test`），spawn 真实的 mock_mcp_server bin 走 CARGO_BIN_EXE_ 环境变量。
//
// 测试客户端手写复刻 `mcp_transport.ts` 的线协议（见该文件头部注释）：
// 连接 → 写一行 `{"method":"__host_mcp_call__","params":{server,tool,args}}`
// → 读一行响应。这里不复刻 TS 那侧"error 字段则 reject、否则拿 result 字段"
// 的判定逻辑本身（那是 mcp_transport.ts 自己的测试范畴），只断言宿主写回的
// 原始 JSON 形状对不对——因为编码方必须先保证形状正确，TS 侧的解码测试
// （mcp_bridge.test.ts / 若有 mcp_transport.test.ts）才有意义。
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use super_agent_os::audit::{self, AuditFilter};
use super_agent_os::mcp::McpManager;
use super_agent_os::mcp_socket::McpSocketListener;
use super_agent_os::paths::DataLayout;
use super_agent_os::permissions::{Access, ConnectorReq};
use super_agent_os::vault::ServerConfig;

fn temp_layout() -> (tempfile::TempDir, DataLayout) {
    let tmp = tempfile::tempdir().unwrap();
    let layout = DataLayout::new(tmp.path().to_path_buf());
    (tmp, layout)
}

fn mock_server_config_with_category(id: &str, category: &str) -> ServerConfig {
    ServerConfig {
        id: id.to_string(),
        category: category.to_string(),
        command: env!("CARGO_BIN_EXE_mock_mcp_server").to_string(),
        args: vec![],
        env: BTreeMap::new(),
        transport: "stdio".into(),
        trust: Default::default(),
    }
}

/// 手写的假客户端：完全复刻 `mcp_transport.ts::hostMcpCall` 的线协议——每次
/// 调用新开一条连接，写一行 JSON 请求，读一行 JSON 响应后关闭连接。
/// `extra_params` 用于往 `params` 里插入额外字段（例如伪造的 `app_id`），
/// 验证宿主是否会被客户端自称的身份污染。
async fn fake_client_call(
    socket_path: &Path,
    server: &str,
    tool: &str,
    args: serde_json::Value,
    extra_params: Option<serde_json::Value>,
) -> serde_json::Value {
    let stream = tokio::net::UnixStream::connect(socket_path)
        .await
        .unwrap_or_else(|e| panic!("连接 {socket_path:?} 应成功：{e}"));
    let (r, mut w) = stream.into_split();

    let mut params = serde_json::json!({ "server": server, "tool": tool, "args": args });
    if let Some(extra) = extra_params {
        if let (Some(obj), Some(extra_obj)) = (params.as_object_mut(), extra.as_object()) {
            for (k, v) in extra_obj {
                obj.insert(k.clone(), v.clone());
            }
        }
    }
    let req = serde_json::json!({ "method": "__host_mcp_call__", "params": params });
    w.write_all(format!("{req}\n").as_bytes())
        .await
        .expect("写请求应成功");

    let mut reader = BufReader::new(r);
    let mut line = String::new();
    reader.read_line(&mut line).await.expect("应能读到一行响应");
    serde_json::from_str(&line).unwrap_or_else(|e| panic!("响应应是合法 JSON，实际 {line:?}：{e}"))
}

fn socket_path_in(tmp: &tempfile::TempDir, name: &str) -> PathBuf {
    tmp.path().join(name).join("mcp.sock")
}

/// 场景 1：只读授权 app 请求 read_file → Ok 往返：宿主监听器收到请求、转发给
/// `host_mcp_call`、mock server 真正执行、结果按 `{"result": ...}` 编码写回。
#[tokio::test]
async fn read_only_app_read_file_roundtrips_ok_through_socket() {
    let manager = McpManager::new();
    let cfg = mock_server_config_with_category("sock-ro-fs", "filesystem");
    manager
        .ensure_server(&cfg)
        .await
        .expect("ensure_server 应成功");
    let (tmp, layout) = temp_layout();
    let socket_path = socket_path_in(&tmp, "app-ro");

    let connectors = vec![ConnectorReq {
        category: "filesystem".to_string(),
        access: Access::Read,
    }];
    let listener = McpSocketListener::start(
        manager.clone(),
        layout.clone(),
        "app-ro".to_string(),
        connectors,
        socket_path.clone(),
    )
    .expect("监听器应能成功 bind");

    let resp = fake_client_call(
        &socket_path,
        &cfg.id,
        "read_file",
        serde_json::json!({ "path": "/tmp/a.txt" }),
        None,
    )
    .await;

    assert!(
        resp.get("error").is_none(),
        "只读授权的 read_file 不应返回 error，实际：{resp:?}"
    );
    let result = resp.get("result").expect("应含 result 字段");
    let text = result["content"][0]["text"].as_str().unwrap_or("");
    assert!(
        text.contains("mock content of /tmp/a.txt"),
        "应回显 mock 内容，实际：{result:?}"
    );
    assert_eq!(
        manager.call_count(&cfg.id),
        1,
        "read_file 应确实被执行了一次"
    );

    listener.stop().await;
}

/// 场景 2：只读授权 app 请求 write_file（未授权）→ Denied 编码：走 error 分支，
/// 且工具从未被真正执行。
#[tokio::test]
async fn read_only_app_write_file_gets_denied_encoding_and_is_not_executed() {
    let manager = McpManager::new();
    let cfg = mock_server_config_with_category("sock-ro-denied", "filesystem");
    manager
        .ensure_server(&cfg)
        .await
        .expect("ensure_server 应成功");
    let (tmp, layout) = temp_layout();
    let socket_path = socket_path_in(&tmp, "app-ro-denied");

    let connectors = vec![ConnectorReq {
        category: "filesystem".to_string(),
        access: Access::Read,
    }];
    let listener = McpSocketListener::start(
        manager.clone(),
        layout,
        "app-ro-denied".to_string(),
        connectors,
        socket_path.clone(),
    )
    .expect("监听器应能成功 bind");

    let resp = fake_client_call(
        &socket_path,
        &cfg.id,
        "write_file",
        serde_json::json!({ "path": "/tmp/x", "content": "y" }),
        None,
    )
    .await;

    let error = resp
        .get("error")
        .expect("未授权应走 error 分支，实际：{resp:?}");
    assert!(error.is_string() && !error.as_str().unwrap().is_empty());
    assert!(
        resp.get("result").is_none(),
        "Denied 编码不应同时带 result，实际：{resp:?}"
    );
    assert_eq!(
        manager.call_count(&cfg.id),
        0,
        "未授权的 write_file 绝不应被执行"
    );

    listener.stop().await;
}

/// 场景 3：读写授权 app 请求 write_file → PendingConfirm 编码：既不是 error，
/// 也和 Ok 的形状不同（带 pending_confirm 标记 + 待用户确认的提示文案），且
/// 工具尚未被真正执行。这是 Task8 开放项"PendingConfirm 如何讲给 LLM 听"的
/// 落地——不是硬失败，模型应该能看懂"已提交、等待确认"。
#[tokio::test]
async fn readwrite_app_write_file_gets_pending_confirm_encoding_distinct_from_ok_and_denied() {
    let manager = McpManager::new();
    let cfg = mock_server_config_with_category("sock-rw-pending", "filesystem");
    manager
        .ensure_server(&cfg)
        .await
        .expect("ensure_server 应成功");
    let (tmp, layout) = temp_layout();
    let socket_path = socket_path_in(&tmp, "app-rw-pending");

    let connectors = vec![ConnectorReq {
        category: "filesystem".to_string(),
        access: Access::ReadWrite,
    }];
    let listener = McpSocketListener::start(
        manager.clone(),
        layout,
        "app-rw-pending".to_string(),
        connectors,
        socket_path.clone(),
    )
    .expect("监听器应能成功 bind");

    let resp = fake_client_call(
        &socket_path,
        &cfg.id,
        "write_file",
        serde_json::json!({ "path": "/tmp/x", "content": "y" }),
        None,
    )
    .await;

    assert!(
        resp.get("error").is_none(),
        "PendingConfirm 绝不能编码成 error，实际：{resp:?}"
    );
    let result = resp
        .get("result")
        .expect("PendingConfirm 应落在 result 里（非硬失败）");
    assert_eq!(result["pending_confirm"], serde_json::json!(true));
    let message = result["message"].as_str().unwrap_or("");
    // P6-C 裁决2：回执如实说"待批"，不模拟成功——断言新文案的两个关键词
    // （"暂存"+"审批中心"），不再断言笼统的"确认"二字。
    assert!(
        message.contains("暂存") && message.contains("审批中心"),
        "应带如实说明「待批」+ 指向审批中心的提示文案，实际：{message}"
    );
    // 与 Ok(read_file) 的形状（{content:[...]}）明显不同：不应含 content 数组。
    assert!(
        result.get("content").is_none(),
        "PendingConfirm 不应和 Ok 撞形状，实际：{result:?}"
    );

    assert_eq!(
        manager.call_count(&cfg.id),
        0,
        "写操作在 PendingConfirm 分支绝不应被真正执行"
    );

    listener.stop().await;
}

/// 安全钉子：客户端在请求里塞一个伪造的 `app_id`/`connectors` 字段，宿主必须
/// 完全无视它——身份由"这是哪个 app 的 socket"（监听器创建时绑定的
/// app_id/connectors）决定，不能由线上请求自称。用审计记录的 app_id 间接验证：
/// 审计里出现的是监听器真实绑定的 app_id，绝不会出现客户端伪造的那个。
#[tokio::test]
async fn bogus_app_id_in_wire_request_is_ignored_gate_uses_listener_bound_identity() {
    let manager = McpManager::new();
    let cfg = mock_server_config_with_category("sock-bogus-id", "filesystem");
    manager
        .ensure_server(&cfg)
        .await
        .expect("ensure_server 应成功");
    let (tmp, layout) = temp_layout();
    let socket_path = socket_path_in(&tmp, "app-real");

    let real_app_id = "app-real-owner";
    let connectors = vec![ConnectorReq {
        category: "filesystem".to_string(),
        access: Access::Read,
    }];
    let listener = McpSocketListener::start(
        manager.clone(),
        layout.clone(),
        real_app_id.to_string(),
        connectors,
        socket_path.clone(),
    )
    .expect("监听器应能成功 bind");

    let resp = fake_client_call(
        &socket_path,
        &cfg.id,
        "read_file",
        serde_json::json!({ "path": "/tmp/a.txt" }),
        Some(serde_json::json!({
            "app_id": "bogus-attacker-app",
            "connectors": [{ "category": "filesystem", "access": "readwrite" }]
        })),
    )
    .await;

    assert!(resp.get("error").is_none(), "实际：{resp:?}");
    assert!(resp.get("result").is_some());

    let real_entries = audit::query(
        &layout,
        &AuditFilter {
            app_id: Some(real_app_id.to_string()),
            tool: None,
            limit: None,
        },
    );
    assert_eq!(
        real_entries.len(),
        1,
        "审计记录应归属监听器真实绑定的 app_id，实际：{real_entries:?}"
    );

    let bogus_entries = audit::query(
        &layout,
        &AuditFilter {
            app_id: Some("bogus-attacker-app".to_string()),
            tool: None,
            limit: None,
        },
    );
    assert!(
        bogus_entries.is_empty(),
        "绝不应以客户端伪造的 app_id 落审计记录，实际：{bogus_entries:?}"
    );

    listener.stop().await;
}

/// 生命周期：`stop()` 之后 socket 文件应被删除，且监听器不再接受新连接
/// （文件已删除，后续 connect 必然失败，用它间接证明 accept 循环确已结束）。
#[tokio::test]
async fn stop_removes_socket_file_and_ends_accept_loop() {
    let manager = McpManager::new();
    let cfg = mock_server_config_with_category("sock-cleanup", "filesystem");
    manager
        .ensure_server(&cfg)
        .await
        .expect("ensure_server 应成功");
    let (tmp, layout) = temp_layout();
    let socket_path = socket_path_in(&tmp, "app-cleanup");

    let connectors = vec![ConnectorReq {
        category: "filesystem".to_string(),
        access: Access::Read,
    }];
    let listener = McpSocketListener::start(
        manager.clone(),
        layout,
        "app-cleanup".to_string(),
        connectors,
        socket_path.clone(),
    )
    .expect("监听器应能成功 bind");

    assert!(socket_path.exists(), "bind 成功后 socket 文件应存在");

    listener.stop().await;

    assert!(!socket_path.exists(), "stop() 之后 socket 文件应被删除");
    assert!(
        tokio::net::UnixStream::connect(&socket_path).await.is_err(),
        "stop() 之后不应再能连上（accept 循环应已结束）"
    );
}

/// DoS 加固回归测试 1（P3 review）：客户端发送一个超过 `MAX_REQUEST_BYTES`
/// 且全程不带换行符的巨量字节流——修复前 `handle_conn` 的 `read_line` 没有
/// 大小上限，这条连接对应的缓冲区会无限增长下去（宿主是所有 app 共享的单一
/// 进程，等价于拖垮整个宿主，而不只是这个恶意 app 自己）。修复后应该在读满
/// 上限后就丢弃这条连接（不写任何响应），且这次"恶意"连接绝不能拖垮 accept
/// 循环或宿主本身——用一次随后在同一个监听器上的正常请求仍然成功往返来
/// 证明。
#[tokio::test]
async fn oversized_frame_without_newline_is_dropped_and_listener_survives() {
    let manager = McpManager::new();
    let cfg = mock_server_config_with_category("sock-oversize", "filesystem");
    manager
        .ensure_server(&cfg)
        .await
        .expect("ensure_server 应成功");
    let (tmp, layout) = temp_layout();
    let socket_path = socket_path_in(&tmp, "app-oversize");

    let connectors = vec![ConnectorReq {
        category: "filesystem".to_string(),
        access: Access::Read,
    }];
    let listener = McpSocketListener::start(
        manager.clone(),
        layout,
        "app-oversize".to_string(),
        connectors,
        socket_path.clone(),
    )
    .expect("监听器应能成功 bind");

    // 恶意连接：写超过上限（16 MiB）的字节，分块写、全程不带 '\n'，永远凑不
    // 出一整行。宿主应该在读满上限后就主动丢弃这条连接（关闭 socket），所以
    // 继续写多半会在某个时刻遇到管道破裂——用 `is_err()` 提前 break，我们真正
    // 要断言的是"host 没被拖垮 + 随后请求仍成功"，不是"这次写一定会报错"。
    let stream = tokio::net::UnixStream::connect(&socket_path)
        .await
        .expect("恶意连接应能建立");
    let (r, mut w) = stream.into_split();
    let chunk = vec![b'a'; 1024 * 1024]; // 1 MiB / 次
    let total_to_send = 20 * 1024 * 1024; // 20 MiB > 16 MiB 上限
    let mut sent = 0usize;
    while sent < total_to_send {
        if w.write_all(&chunk).await.is_err() {
            break;
        }
        sent += chunk.len();
    }
    drop(w);

    let mut reader = BufReader::new(r);
    let mut resp_line = String::new();
    // 宿主应直接丢弃这条连接、不写任何响应，读到的应是 EOF（Ok(0)）。外层套
    // 一个远大于内部读超时的耐心上限：如果修复失效导致这里被挂起，测试在
    // 有限时间内失败退出，而不是把整个 test suite 也一起挂死（同
    // mcp_manager_it.rs 里握手超时测试的写法）。
    let read_result = tokio::time::timeout(
        std::time::Duration::from_secs(15),
        reader.read_line(&mut resp_line),
    )
    .await
    .expect("宿主应在合理时间内关闭这条超大帧连接，不应把测试也一起挂死");
    assert!(
        matches!(read_result, Ok(0)),
        "超大帧连接应被丢弃、不回任何响应，实际 read_result={read_result:?}, resp_line={resp_line:?}"
    );

    // 关键断言：同一个监听器上，随后一个正常请求仍然成功——证明这个恶意
    // 客户端既没有拖垮 accept 循环，也没有让宿主进程被拖垮。
    let resp = fake_client_call(
        &socket_path,
        &cfg.id,
        "read_file",
        serde_json::json!({ "path": "/tmp/a.txt" }),
        None,
    )
    .await;
    assert!(
        resp.get("error").is_none(),
        "超大帧连接之后，正常请求应仍然成功，实际：{resp:?}"
    );
    let result = resp.get("result").expect("应含 result 字段");
    let text = result["content"][0]["text"].as_str().unwrap_or("");
    assert!(
        text.contains("mock content of /tmp/a.txt"),
        "应回显 mock 内容，实际：{result:?}"
    );

    listener.stop().await;
}

/// DoS 加固回归测试 2（P3 review）：客户端连接后只写半截数据（没有换行符），
/// 然后既不发完也不关闭连接——模拟"连上了但永远凑不出完整一帧"的恶意/异常
/// 客户端。修复前 `read_line` 不带超时，会把处理这条连接的 tokio 任务永久
/// 挂起。修复后应该在 `REQUEST_READ_TIMEOUT` 后主动放弃这条连接，且不影响
/// 随后在同一个监听器上的正常请求。
#[tokio::test]
async fn connection_with_incomplete_frame_is_dropped_after_read_timeout() {
    let manager = McpManager::new();
    let cfg = mock_server_config_with_category("sock-readtimeout", "filesystem");
    manager
        .ensure_server(&cfg)
        .await
        .expect("ensure_server 应成功");
    let (tmp, layout) = temp_layout();
    let socket_path = socket_path_in(&tmp, "app-readtimeout");

    let connectors = vec![ConnectorReq {
        category: "filesystem".to_string(),
        access: Access::Read,
    }];
    let listener = McpSocketListener::start(
        manager.clone(),
        layout,
        "app-readtimeout".to_string(),
        connectors,
        socket_path.clone(),
    )
    .expect("监听器应能成功 bind");

    let stream = tokio::net::UnixStream::connect(&socket_path)
        .await
        .expect("连接应成功");
    let (r, mut w) = stream.into_split();
    w.write_all(b"{\"method\":\"__host_mcp_call__\"")
        .await
        .expect("半截数据应能写成功（没有换行符，故意不凑成完整一行）");

    let mut reader = BufReader::new(r);
    let mut resp_line = String::new();
    // 外层套一个远大于内部读超时的测试自身耐心上限：如果超时机制失效导致
    // 宿主真的永久挂起，这层 timeout 会让测试在有限时间内失败退出，而不是
    // 把整个 test suite 也一起挂死。
    let read_result = tokio::time::timeout(
        std::time::Duration::from_secs(15),
        reader.read_line(&mut resp_line),
    )
    .await
    .expect("宿主应在 REQUEST_READ_TIMEOUT 后主动放弃这条连接，不应永久挂起");
    assert!(
        matches!(read_result, Ok(0)),
        "读超时后连接应被丢弃、不回任何响应，实际 read_result={read_result:?}, resp_line={resp_line:?}"
    );
    drop(w);

    // 随后一个正常请求仍然成功——证明一个"永远发不完整帧"的连接不会拖垮
    // 后续调用。
    let resp = fake_client_call(
        &socket_path,
        &cfg.id,
        "read_file",
        serde_json::json!({ "path": "/tmp/a.txt" }),
        None,
    )
    .await;
    assert!(
        resp.get("error").is_none(),
        "读超时连接之后，正常请求应仍然成功，实际：{resp:?}"
    );

    listener.stop().await;
}
