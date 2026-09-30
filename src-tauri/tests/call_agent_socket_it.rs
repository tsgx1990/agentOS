// P5 T3 集成测试：`__host_call_agent__` 的 socket 分发接线。
//
// 复用 `maker_socket_it.rs` 的假客户端手法（连一次、写一行 {method,params}、读一行
// 响应、关连接——与 `mcp_transport.ts::hostCall` 线协议字节对齐）。本文件只钉住
// **分发 + 三道闸的接线**（未授权 / 深度绑定在监听器 / call bus 未启用 / 非 call_agent
// 方法不被吞）；A→B 成功拉起被调方回传文本的完整happy path 由 T7 的 e2e 覆盖。

use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use super_agent_os::audit::{self, AuditFilter};
use super_agent_os::capabilities;
use super_agent_os::capability::CallerIdentity;
use super_agent_os::maker::MAKER_APP_ID;
use super_agent_os::mcp::McpManager;
use super_agent_os::mcp_socket::McpSocketListener;
use super_agent_os::paths::DataLayout;
use super_agent_os::permissions::Permissions;
use super_agent_os::registry::{InstalledApp, RegistryStore};

fn temp_layout() -> (tempfile::TempDir, DataLayout) {
    let tmp = tempfile::tempdir().unwrap();
    let layout = DataLayout::new(tmp.path().to_path_buf());
    (tmp, layout)
}

fn socket_path_in(tmp: &tempfile::TempDir, name: &str) -> PathBuf {
    tmp.path().join(name).join("mcp.sock")
}

fn hosttools_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("hosttools")
}

fn installed(app_id: &str) -> InstalledApp {
    InstalledApp {
        app_id: app_id.into(),
        name: app_id.into(),
        version: "1.0.0".into(),
        display_name: app_id.into(),
        category: "life".into(),
        icon: None,
        trusted: false,
        domains: vec![],
    }
}

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

/// 未授权：调用方（非 router）声明的 `agents.call` 非空（满足
/// `AgentsCallCapability::declared()` 这道前置闸，能真的进到 `handle()`），但列表里
/// 不含被调方目标 → 请求落到 `call_bus::handle_call_agent`，被其
/// `authorize_call` 白名单闸（`src/call_bus.rs:94` 一带）拒绝，记
/// `verdict="not-permitted"`（`call_bus.rs` 自己写的审计，不是
/// `mcp_socket::process_request` 那条 `verdict="denied"`）。
/// 目标必须先"已装"（upsert 进 registry），否则会先被 not-found 闸拦下——这里要验的
/// 是 not-permitted 这一支，故先把被调方装上。
///
/// **两层判断的输入来源不同**：`call_bus::handle_call_agent` 自己的白名单判定读的是
/// 调用方 app_id 对应**磁盘上**的 `permissions.json`（`load_caller_call_list`），不是
/// 监听器绑定的 `Permissions`——这里没有为 `superagent__caller` 装任何包，磁盘读取
/// 天然返回空列表，因此不管监听器绑定了什么 `agents.call` 都会落进 not-permitted；
/// 监听器绑定的 `agents.call`（这里故意填一个跟目标无关的条目）只用来满足
/// `declared()` 这道"进不进得了 handler"的前置闸——两者语义独立，正是这条测试要
/// 证明的分层（此前误用只绑 `connectors` 的 `start_with`，会在 declared 闸就被拒，
/// 从未真正进过 `call_bus::authorize_call` 的白名单分支，见 code review I-1）。
#[tokio::test]
async fn call_agent_not_permitted_when_caller_did_not_declare_target() {
    let (tmp, layout) = temp_layout();
    // 被调方已装（caller 却没声明调它）。
    RegistryStore::new(layout.registry_path())
        .upsert(installed("superagent__summarizer"))
        .unwrap();
    let socket_path = socket_path_in(&tmp, "superagent__caller");

    // agents.call 非空（但不含目标）——满足 declared() 前置闸，让请求真的能走到
    // call_bus::handle_call_agent 内部；caller 没有装包，call_bus 自己重新读盘拿到的
    // 才是决定白名单结果的那份（这里始终为空），与此处绑定的值无关（见上方文档）。
    let mut perms = Permissions::default();
    perms.agents.call = vec!["@superagent/someone-else".to_string()];
    let listener = McpSocketListener::start_with_identity(
        McpManager::new(),
        layout.clone(),
        CallerIdentity {
            app_id: "superagent__caller".to_string(),
            trusted: false,
            depth: 0,
        },
        perms,
        socket_path.clone(),
        Some(hosttools_dir()),
        Arc::new(capabilities::builtin()),
    )
    .expect("监听器应能 bind");

    let resp = fake_client_send(
        &socket_path,
        "__host_call_agent__",
        serde_json::json!({ "target": "@superagent/summarizer", "prompt": "精简这段" }),
    )
    .await;

    assert_eq!(
        resp["ok"],
        serde_json::json!(false),
        "未声明应被拒，实际：{resp:?}"
    );
    assert!(
        resp["error"].as_str().unwrap_or("").contains("未声明调用"),
        "错误信息应是 call_bus::authorize_call 白名单拒绝的原文，实际：{resp:?}"
    );
    let audits = audit::query(
        &layout,
        &AuditFilter {
            app_id: Some("superagent__caller".into()),
            tool: Some("__host_call_agent__".into()),
            ..Default::default()
        },
    );
    assert!(
        audits.iter().any(|a| a.verdict == "not-permitted"),
        "应落 call_bus 自己写的 not-permitted 审计（证明真的走到了 authorize_call），实际：{audits:?}"
    );
    listener.stop().await;
}

/// 深度绑定在监听器上：depth=MAX 的监听器发起任何调用都被深度闸拒——证明深度来自
/// 监听器绑定值（start_with 的 depth 参数），不取自 wire。
#[tokio::test]
async fn call_agent_depth_exceeded_uses_listener_bound_depth() {
    let (tmp, layout) = temp_layout();
    RegistryStore::new(layout.registry_path())
        .upsert(installed("superagent__summarizer"))
        .unwrap();
    let socket_path = socket_path_in(&tmp, "superagent"); // router 也不能突破深度

    let listener = McpSocketListener::start_with(
        McpManager::new(),
        layout,
        MAKER_APP_ID.to_string(),
        vec![],
        socket_path.clone(),
        Some(hosttools_dir()),
        super_agent_os::call_bus::MAX_CALL_DEPTH, // 已在最深合法层，不得再嵌套
    )
    .expect("监听器应能 bind");

    let resp = fake_client_send(
        &socket_path,
        "__host_call_agent__",
        serde_json::json!({ "target": "@superagent/summarizer", "prompt": "x" }),
    )
    .await;

    assert_eq!(resp["ok"], serde_json::json!(false));
    assert!(
        resp["error"].as_str().unwrap_or("").contains("深度"),
        "应因深度到顶被拒，实际：{resp:?}"
    );
    listener.stop().await;
}

/// P6-C Task6（HANDOFF「P6-A 终审残留」第 1 条，spec §4 步骤 6「路由深度」）：
/// router（内置 `superagent`）的白名单豁免只在 `depth == 0` 生效。这里绑定
/// 一个 `depth=1` 的监听器（模拟 router 被别的已装应用嵌套调起、自己又想
/// 发起下一层调用的场景）——即便调用方就是 router、目标已装，只要它没有
/// 在自己的（磁盘不存在的）清单里声明过该目标，也必须像第三方一样被
/// `not-permitted` 拒绝，而不是像 depth=0 那样天然放行。
/// `agents.call` 能力的 `declared()` 对 router 恒为真（`id.is_router()`豁免），
/// 所以这条请求确实会一路走到 `call_bus::authorize_call` 的白名单闸，不会在
/// 更早的 declared 前置闸被拦下——这正是这条测试要证明 authorize_call 本身
/// 收紧了、而不是靠外层闸门凑巧挡住。
#[tokio::test]
async fn call_agent_router_not_exempt_when_depth_is_nonzero() {
    let (tmp, layout) = temp_layout();
    RegistryStore::new(layout.registry_path())
        .upsert(installed("superagent__summarizer"))
        .unwrap();
    let socket_path = socket_path_in(&tmp, "superagent-nested");

    let listener = McpSocketListener::start_with(
        McpManager::new(),
        layout.clone(),
        MAKER_APP_ID.to_string(),
        vec![],
        socket_path.clone(),
        Some(hosttools_dir()),
        1, // depth=1：router 被嵌套调起后的下一层，不再豁免白名单
    )
    .expect("监听器应能 bind");

    let resp = fake_client_send(
        &socket_path,
        "__host_call_agent__",
        serde_json::json!({ "target": "@superagent/summarizer", "prompt": "x" }),
    )
    .await;

    assert_eq!(
        resp["ok"],
        serde_json::json!(false),
        "depth=1 的 router 未声明该目标应被拒，实际：{resp:?}"
    );
    assert!(
        resp["error"].as_str().unwrap_or("").contains("未声明调用"),
        "应是 authorize_call 白名单拒绝的原文，实际：{resp:?}"
    );
    let audits = audit::query(
        &layout,
        &AuditFilter {
            app_id: Some(MAKER_APP_ID.into()),
            tool: Some("__host_call_agent__".into()),
            ..Default::default()
        },
    );
    assert!(
        audits.iter().any(|a| a.verdict == "not-permitted"),
        "应落 not-permitted 审计（证明真的走到了 authorize_call 白名单闸），实际：{audits:?}"
    );
    listener.stop().await;
}

/// 旧构造 `start(...)`（无 hosttools 上下文）上收到 call_agent → 明确"未启用"，
/// 绝不落到 mcp 分支（既有 mcp/maker 测试用的正是 start，天然不受互联影响）。
///
/// P6-A：`process_request` 改走 `CapabilityRegistry::dispatch` 之后，
/// `agents.call` 能力的 `declared()` 闸（未声明 `agents.call` 且非 router →
/// unauthorized，见 `tests/capabilities_privileged_it.rs::
/// call_agent_without_hosttools_reports_bus_disabled` 与
/// `capabilities/agents_call.rs::declared`）会先于 `handle()` 内的
/// "hosttools_dir 是否为 None" 判断执行——这条测试想单独钉住的是后者，因此
/// 调用方必须是 router（`is_router()` 豁免 declared 闸，与生产环境"router
/// 天生可用 call bus"一致），才能真正走到 `handle()` 内部去触发"未启用"这
/// 条分支；`start(...)` 用普通第三方 app_id 时，未声明 `agents.call` 会在
/// declared 闸就被拒（`unauthorized: 该应用未声明能力 agents.call`），根本
/// 到不了这里要测的分支。
#[tokio::test]
async fn call_agent_unavailable_on_listener_without_hosttools_context() {
    let (tmp, layout) = temp_layout();
    let socket_path = socket_path_in(&tmp, MAKER_APP_ID);

    let listener = McpSocketListener::start(
        McpManager::new(),
        layout,
        MAKER_APP_ID.to_string(),
        vec![],
        socket_path.clone(),
    )
    .expect("监听器应能 bind");

    let resp = fake_client_send(
        &socket_path,
        "__host_call_agent__",
        serde_json::json!({ "target": "@superagent/summarizer", "prompt": "x" }),
    )
    .await;

    assert_eq!(resp["ok"], serde_json::json!(false));
    assert!(
        resp["error"]
            .as_str()
            .unwrap_or("")
            .contains("未在此监听器启用"),
        "无上下文监听器应回未启用，实际：{resp:?}"
    );
    listener.stop().await;
}

/// 非回归：新增的 call_agent 分支不吞非 call_agent 方法——绑定 MAKER_APP_ID 的
/// start_with 监听器收到 maker 方法仍路由到 maker handler（stage_write 落盘成功）。
#[tokio::test]
async fn non_call_agent_methods_still_route_to_maker_dispatch() {
    let (tmp, layout) = temp_layout();
    let socket_path = socket_path_in(&tmp, MAKER_APP_ID);

    let listener = McpSocketListener::start_with(
        McpManager::new(),
        layout,
        MAKER_APP_ID.to_string(),
        vec![],
        socket_path.clone(),
        Some(hosttools_dir()),
        0,
    )
    .expect("监听器应能 bind");

    let resp = fake_client_send(
        &socket_path,
        "__host_maker_stage_write__",
        serde_json::json!({ "draft_id": "draft-1", "rel_path": "agent/persona.md", "content": "hi" }),
    )
    .await;

    assert_eq!(
        resp["ok"],
        serde_json::json!(true),
        "maker 方法应仍路由到 maker handler，实际：{resp:?}"
    );
    listener.stop().await;
}

/// T5：`__host_list_agents__` 仅 router（MAKER_APP_ID）可达，返回已装应用目录。
#[tokio::test]
async fn list_agents_returns_registry_for_router() {
    let (tmp, layout) = temp_layout();
    let reg = RegistryStore::new(layout.registry_path());
    reg.upsert(installed("superagent__summarizer")).unwrap();
    reg.upsert(installed("superagent__researcher")).unwrap();
    let socket_path = socket_path_in(&tmp, MAKER_APP_ID);

    let listener = McpSocketListener::start_with(
        McpManager::new(),
        layout,
        MAKER_APP_ID.to_string(),
        vec![],
        socket_path.clone(),
        Some(hosttools_dir()),
        0,
    )
    .expect("监听器应能 bind");

    let resp = fake_client_send(&socket_path, "__host_list_agents__", serde_json::json!({})).await;

    assert_eq!(resp["ok"], serde_json::json!(true));
    let ids: Vec<&str> = resp["agents"]
        .as_array()
        .expect("agents 应为数组")
        .iter()
        .filter_map(|a| a["app_id"].as_str())
        .collect();
    assert!(
        ids.contains(&"superagent__summarizer") && ids.contains(&"superagent__researcher"),
        "实际：{resp:?}"
    );
    listener.stop().await;
}

/// T5：非 router 发 `__host_list_agents__` → unauthorized，绝不落到 handler。
///
/// P6-A：`process_request` 改走 `CapabilityRegistry::dispatch` 之后，拒绝消息
/// 统一变成通用的 `unauthorized: 该应用未声明能力 <key>`（`router` 能力的
/// `declared()` 就是 `id.is_router()`，见 `capabilities/router.rs`；通用消息
/// 格式见 `capability.rs::CapabilityRegistry::dispatch` 与
/// `tests/capabilities_privileged_it.rs::non_router_dispatch_of_maker_and_list_agents_is_denied`），
/// 不再是旧手写分支专属的"仅限主助手(superagent)"这句话——两者对外都是
/// `ok:false` + `unauthorized:` 前缀，语义（非 router 拒绝）不变，只是措辞从
/// 逐分支手写改成了全体能力统一的模板。
#[tokio::test]
async fn list_agents_denied_for_non_router() {
    let (tmp, layout) = temp_layout();
    let socket_path = socket_path_in(&tmp, "superagent__other");

    let listener = McpSocketListener::start_with(
        McpManager::new(),
        layout,
        "superagent__other".to_string(),
        vec![],
        socket_path.clone(),
        Some(hosttools_dir()),
        0,
    )
    .expect("监听器应能 bind");

    let resp = fake_client_send(&socket_path, "__host_list_agents__", serde_json::json!({})).await;

    assert_eq!(resp["ok"], serde_json::json!(false));
    assert!(
        resp["error"]
            .as_str()
            .unwrap_or("")
            .contains("unauthorized")
            && resp["error"]
                .as_str()
                .unwrap_or("")
                .contains("未声明能力 router"),
        "非 router 应被拒，实际：{resp:?}"
    );
    listener.stop().await;
}
