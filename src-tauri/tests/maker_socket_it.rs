// Task3（P4）集成测试：`McpSocketListener` 按 method 分发 maker 请求。
//
// P3 已经建好宿主 unix socket 监听器（`mcp_socket.rs`），收
// `{method,params}` 一行 JSON、按 `__host_mcp_call__` 转发给
// `McpManager::host_mcp_call`。本任务给它加一条新分支：`method` 是
// `__host_maker_stage_write__`/`__host_maker_preview__`/`__host_maker_install__`
// 之一时，路由到 `maker::handle_maker_request(app_id, method, params, layout)`
// （Task3 阶段只是个 stub，恒定返回 `{"ok": true}`；T4/T5/T6 会分别替换成真正
// 的 stage_write/preview/install 分支）。
//
// 测试硬件完全照抄 `tests/mcp_socket_it.rs` 的假客户端手法：连一次、写一行
// JSON 请求、读一行 JSON 响应、关连接——与 `mcp_transport.ts::hostMcpCall`
// 的线协议字节对齐。
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use super_agent_os::maker::MAKER_APP_ID;
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

fn socket_path_in(tmp: &tempfile::TempDir, name: &str) -> PathBuf {
    tmp.path().join(name).join("mcp.sock")
}

/// 手写的假客户端：连一次、写一行 `{method,params}` JSON 请求、读一行 JSON
/// 响应、关连接——与 `tests/mcp_socket_it.rs::fake_client_call` 及
/// `mcp_transport.ts::hostMcpCall` 的线协议保持一致。这里不固定 `params` 的
/// 形状（maker 三个方法的 `params` 长得跟 mcp 的 `{server,tool,args}` 不一样），
/// 调用方直接传整个 `method` + `params`。
async fn fake_client_send(
    socket_path: &Path,
    method: &str,
    params: serde_json::Value,
) -> serde_json::Value {
    let stream = tokio::net::UnixStream::connect(socket_path)
        .await
        .unwrap_or_else(|e| panic!("连接 {socket_path:?} 应成功：{e}"));
    let (r, mut w) = stream.into_split();

    let req = serde_json::json!({ "method": method, "params": params });
    w.write_all(format!("{req}\n").as_bytes())
        .await
        .expect("写请求应成功");

    let mut reader = BufReader::new(r);
    let mut line = String::new();
    reader.read_line(&mut line).await.expect("应能读到一行响应");
    serde_json::from_str(&line).unwrap_or_else(|e| panic!("响应应是合法 JSON，实际 {line:?}：{e}"))
}

/// 核心场景：`__host_maker_stage_write__` 请求应被路由到
/// `maker::handle_maker_request`。T3 阶段这里曾是 stub（恒定 `{"ok": true}`）；
/// T4 把 `stage_write` 换成了真正的落盘实现（见 `tests/maker_it.rs` 覆盖的完整
/// 行为矩阵），所以这里只验证"socket 分发本身把请求送到了真正的 handler、client
/// 端原样收到 handler 的返回值"——不重复断言 stage_write 的落盘细节。身份
/// （`app_id`）仍应是监听器绑定时的那个，不读 wire 上的任何字段（本测试没在
/// params 里塞 app_id，规则由 `mcp_socket.rs` 模块文档统一保证）。
///
/// 安全修复后：`process_request` 只在 `app_id == MAKER_APP_ID` 时才路由到
/// `handle_maker_request`（见下面
/// `non_maker_app_id_is_rejected_before_reaching_maker_handler` 这条新测试对
/// 反面场景的覆盖），所以这里必须绑定 `MAKER_APP_ID` 才能继续验证"分发到了
/// 真正的 handler"这件事——绑非 Maker 的 app_id 现在会在到达 handler 之前就被
/// 拒绝，不再是本测试想覆盖的场景。
#[tokio::test]
async fn maker_stage_write_method_routes_to_handle_maker_request_stub() {
    let manager = McpManager::new();
    let (tmp, layout) = temp_layout();
    let socket_path = socket_path_in(&tmp, MAKER_APP_ID);

    let listener = McpSocketListener::start(
        manager,
        layout,
        MAKER_APP_ID.to_string(),
        vec![],
        socket_path.clone(),
    )
    .expect("监听器应能成功 bind");

    let resp = fake_client_send(
        &socket_path,
        "__host_maker_stage_write__",
        serde_json::json!({ "draft_id": "draft-1", "rel_path": "agent/persona.md", "content": "hi" }),
    )
    .await;

    assert_eq!(
        resp["ok"],
        serde_json::json!(true),
        "应成功落盘，实际：{resp:?}"
    );
    let path_field = resp["path"]
        .as_str()
        .unwrap_or_else(|| panic!("应带 path 字段，实际：{resp:?}"));
    let written = std::fs::read_to_string(path_field).expect("path 指向的文件应确实存在");
    assert_eq!(written, "hi", "落盘内容应与请求一致");

    listener.stop().await;
}

/// `__host_maker_preview__`（T6 已换成真实实现——沙盒预览会话，见
/// `tests/maker_preview_it.rs` 覆盖的完整行为）与 `__host_maker_install__`
/// （T5 已换成真实实现，见 `tests/maker_it.rs` 覆盖的完整行为矩阵）应各自被
/// 正确路由——本测试只钉住"socket 分发确实把请求送到了各自当前该走的那条
/// 逻辑"，不重复断言 preview/install 分支各自的沙盒/落盘/校验细节。`draft-1`
/// 在这个测试的暂存区里从未被 `stage_write` 过（没有 `package.json`），所以
/// preview/install 两个分支都会先走到 `pkg::load_and_validate`，因为文件不
/// 存在而 fail-closed，各自回 `{ok:false, error}` 而不是 T3 时代那个恒定的
/// `{"ok": true}` stub——这正是 T5/T6 把 install/preview 从 stub 换成真实实现后
/// 应有的行为（本测试原先对 preview 断言的 `{"ok": true}` 已随 T6 过时，随之
/// 更新）。
#[tokio::test]
async fn maker_preview_and_install_now_route_to_real_fail_closed_handlers() {
    let manager = McpManager::new();
    let (tmp, layout) = temp_layout();
    let socket_path = socket_path_in(&tmp, MAKER_APP_ID);

    let listener = McpSocketListener::start(
        manager,
        layout,
        MAKER_APP_ID.to_string(),
        vec![],
        socket_path.clone(),
    )
    .expect("监听器应能成功 bind");

    let preview_resp = fake_client_send(
        &socket_path,
        "__host_maker_preview__",
        serde_json::json!({ "draft_id": "draft-1" }),
    )
    .await;
    assert_eq!(
        preview_resp["ok"],
        serde_json::json!(false),
        "不存在的草稿应 fail-closed，不应再是 T3 的 {{ok:true}} stub，实际：{preview_resp:?}"
    );
    assert!(
        preview_resp.get("error").is_some(),
        "fail-closed 应带 error 字段"
    );

    let install_resp = fake_client_send(
        &socket_path,
        "__host_maker_install__",
        serde_json::json!({ "draft_id": "draft-1" }),
    )
    .await;
    assert_eq!(
        install_resp["ok"],
        serde_json::json!(false),
        "不存在的草稿应 fail-closed，不应再是 T3 的 {{ok:true}} stub，实际：{install_resp:?}"
    );
    assert!(
        install_resp.get("pending_confirm").is_none(),
        "fail-closed 不应带 pending_confirm"
    );
    assert!(
        install_resp.get("error").is_some(),
        "fail-closed 应带 error 字段"
    );

    listener.stop().await;
}

/// 回归：既有 `__host_mcp_call__` 分发不受影响——同一个监听器上，正常的
/// mcp 调用仍然按原有协议（`{result:...}`）往返成功。
#[tokio::test]
async fn existing_host_mcp_call_dispatch_still_works_regression() {
    let manager = McpManager::new();
    let cfg = mock_server_config_with_category("maker-sock-regression", "filesystem");
    manager
        .ensure_server(&cfg)
        .await
        .expect("ensure_server 应成功");
    let (tmp, layout) = temp_layout();
    let socket_path = socket_path_in(&tmp, "app-regression");

    let connectors = vec![ConnectorReq {
        category: "filesystem".to_string(),
        access: Access::Read,
    }];
    let listener = McpSocketListener::start(
        manager.clone(),
        layout,
        "app-regression".to_string(),
        connectors,
        socket_path.clone(),
    )
    .expect("监听器应能成功 bind");

    let resp = fake_client_send(
        &socket_path,
        "__host_mcp_call__",
        serde_json::json!({ "server": cfg.id, "tool": "read_file", "args": { "path": "/tmp/a.txt" } }),
    )
    .await;

    assert!(
        resp.get("error").is_none(),
        "既有 mcp 分发不应回归破坏，实际：{resp:?}"
    );
    let result = resp
        .get("result")
        .expect("应含 result 字段（既有编码规则不变）");
    let text = result["content"][0]["text"].as_str().unwrap_or("");
    assert!(
        text.contains("mock content of /tmp/a.txt"),
        "应回显 mock 内容，实际：{result:?}"
    );
    assert_eq!(manager.call_count(&cfg.id), 1);

    listener.stop().await;
}

/// 安全回归（本次修复的核心断言）：一个绑在**非 Maker** app_id 上的 socket
/// 监听器（例如某个声明了自己 connector 的普通 app，P3 起每个这样的 app 都有
/// 一条自己的 socket）收到 `__host_maker_stage_write__` 帧时，必须被拒绝——
/// 拒绝发生在 `mcp_socket.rs::process_request` 的分发点上，**不**应该到达
/// `maker::handle_maker_request`，因此 host 侧也绝不能真的把内容写进 maker
/// 暂存树。修复前，这里会像 Maker 自己发的请求一样被无条件路由到
/// `handle_maker_request`，成功落盘——这正是本次 review 发现的越权点。
#[tokio::test]
async fn non_maker_app_id_is_rejected_before_reaching_maker_handler() {
    let manager = McpManager::new();
    let (tmp, layout) = temp_layout();
    let non_maker_app_id = "some-other-app";
    let socket_path = socket_path_in(&tmp, non_maker_app_id);

    let listener = McpSocketListener::start(
        manager,
        layout.clone(),
        non_maker_app_id.to_string(),
        vec![],
        socket_path.clone(),
    )
    .expect("监听器应能成功 bind");

    let resp = fake_client_send(
        &socket_path,
        "__host_maker_stage_write__",
        serde_json::json!({ "draft_id": "draft-hijack", "rel_path": "agent/persona.md", "content": "pwned" }),
    )
    .await;

    assert_eq!(
        resp["ok"],
        serde_json::json!(false),
        "非 Maker app 的越权调用应被拒绝，不应像 Maker 自己的请求一样成功，实际：{resp:?}"
    );
    let error = resp["error"]
        .as_str()
        .unwrap_or_else(|| panic!("拒绝响应应带 error 字段，实际：{resp:?}"));
    assert!(
        error.contains("unauthorized"),
        "拒绝原因应说明是未授权，实际：{error:?}"
    );

    // 关键断言：请求既没有走到 `handle_maker_request`，暂存目录/文件都不应
    // 该被创建——不只是"响应形状看起来像拒绝"，host 侧确实什么都没写。
    let target_dir = layout.maker_staging_dir("draft-hijack");
    assert!(
        !target_dir.exists(),
        "越权请求不应在 maker 暂存区留下任何目录：{target_dir:?}"
    );
    let target_file = target_dir.join("agent/persona.md");
    assert!(
        !target_file.exists(),
        "越权请求不应写入任何文件：{target_file:?}"
    );

    listener.stop().await;
}

/// 安全回归的另一半：同一个 `__host_maker_preview__`/`__host_maker_install__`
/// 方法在非 Maker app_id 的 socket 上同样应被拒绝——不是只有 `stage_write` 一
/// 个方法被拦，三个 maker 方法共用同一条分发 gate，理应一起被挡。
#[tokio::test]
async fn non_maker_app_id_is_rejected_for_preview_and_install_too() {
    let manager = McpManager::new();
    let (tmp, layout) = temp_layout();
    let non_maker_app_id = "some-other-app-2";
    let socket_path = socket_path_in(&tmp, non_maker_app_id);

    let listener = McpSocketListener::start(
        manager,
        layout,
        non_maker_app_id.to_string(),
        vec![],
        socket_path.clone(),
    )
    .expect("监听器应能成功 bind");

    let preview_resp = fake_client_send(
        &socket_path,
        "__host_maker_preview__",
        serde_json::json!({ "draft_id": "draft-1" }),
    )
    .await;
    assert_eq!(
        preview_resp["ok"],
        serde_json::json!(false),
        "preview 越权调用应被拒绝，实际：{preview_resp:?}"
    );
    assert!(
        preview_resp["error"]
            .as_str()
            .unwrap_or("")
            .contains("unauthorized"),
        "实际：{preview_resp:?}"
    );

    let install_resp = fake_client_send(
        &socket_path,
        "__host_maker_install__",
        serde_json::json!({ "draft_id": "draft-1" }),
    )
    .await;
    assert_eq!(
        install_resp["ok"],
        serde_json::json!(false),
        "install 越权调用应被拒绝，实际：{install_resp:?}"
    );
    assert!(
        install_resp["error"]
            .as_str()
            .unwrap_or("")
            .contains("unauthorized"),
        "实际：{install_resp:?}"
    );

    listener.stop().await;
}
