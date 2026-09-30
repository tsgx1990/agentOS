// McpManager 集成测试：spawn 真实的 mock_mcp_server bin（Task4，同 mock_mcp_it.rs
// 的路径解析方式，走 CARGO_BIN_EXE_ 环境变量），验证：
//  1. ensure_server 之后 server_tools 能看到 tools/list 返回的 2 个工具，且
//     danger 是从 `annotations.readOnlyHint` 推导出来的（write_file→Write，
//     read_file→Read）。
//  2. 同一个 serverId 再次 ensure_server 不会重新 spawn 子进程（connect-once/
//     全局复用）——用 McpManager 暴露的 spawn_count() 断言只真正 spawn 了一次。
//
// ServerConfig 直接在测试里手工构造（不走 vault::* 生产自由函数——那些会碰真实
// keychain，重编译后可能弹交互式授权框，把 `cargo test` 挂死，见 vault.rs 的
// service 隔离说明）。
use std::collections::BTreeMap;
use super_agent_os::approvals::ApprovalStore;
use super_agent_os::audit::{self, AuditFilter};
use super_agent_os::known_tools;
use super_agent_os::mcp::{Danger, McpCallResult, McpManager};
use super_agent_os::notifications::{NotificationFilter, NotificationStore};
use super_agent_os::paths::DataLayout;
use super_agent_os::permissions::{Access, ConnectorReq};
use super_agent_os::skills::{SkillSource, SkillSourceKind, SkillStore};
use super_agent_os::vault::{ServerConfig, Trust};

fn temp_layout() -> (tempfile::TempDir, DataLayout) {
    let tmp = tempfile::tempdir().unwrap();
    let layout = DataLayout::new(tmp.path().to_path_buf());
    (tmp, layout)
}

/// 审查修复轮1 Important 3：`respond_staged`/`respond_confirm` 在真正执行
/// 之前会重新读该 app 当前的 `packages_dir(app_id)/package.json`+
/// `permissions.json` 复核鉴权。本文件多数测试直接手工构造 `ConnectorReq`
/// 喂给 `host_mcp_call`，从不安装真实包——这里写一份满足
/// `pkg::load_and_validate` 最小要求的包目录，让重新鉴权能读到一份声明了
/// 给定 `category`/`access` 连接器的 `Permissions`，同 `p6c_approvals_it.rs`
/// 的同名 helper。
fn install_minimal_package(layout: &DataLayout, app_id: &str, category: &str, access: &str) {
    let dir = layout.packages_dir(app_id);
    std::fs::create_dir_all(dir.join("ui")).expect("建 ui 目录应成功");
    std::fs::write(dir.join("ui/index.html"), "<html></html>").expect("写 ui 入口应成功");
    let package_json = format!(
        r#"{{
  "name": "{app_id}",
  "version": "1.0.0",
  "keywords": ["pi-package", "superagent-app"],
  "engines": {{ "superagent-host": ">=1.0.0, <2.0.0" }},
  "superagent": {{
    "schemaVersion": 1,
    "displayName": "{app_id}",
    "category": "test",
    "ui": "ui/index.html",
    "permissions": "permissions.json"
  }}
}}"#
    );
    std::fs::write(dir.join("package.json"), package_json).expect("写 package.json 应成功");
    let permissions_json =
        format!(r#"{{ "connectors": [{{ "category": "{category}", "access": "{access}" }}] }}"#);
    std::fs::write(dir.join("permissions.json"), permissions_json)
        .expect("写 permissions.json 应成功");
}

fn mock_server_config(id: &str) -> ServerConfig {
    mock_server_config_with_category(id, "dev")
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

/// P6-C Task1 Step4 专用：给子进程传 `MOCK_MCP_NUKE_TOOL=1`（走 `cfg.env`，只
/// 影响 spawn 出来的子进程，不碰宿主测试进程自身的环境变量、不需要
/// `ENV_LOCK`），让 mock server 额外暴露一个名为 `nuke_everything` 且
/// `annotations.readOnlyHint=true` 但名字不落在任何前缀表格子里的工具，专门
/// 验证 `classify_tool_with_trust` 的 byo 不降危规则。
fn mock_server_config_with_trust(id: &str, category: &str, trust: Trust) -> ServerConfig {
    let mut env = BTreeMap::new();
    env.insert("MOCK_MCP_NUKE_TOOL".to_string(), "1".to_string());
    ServerConfig {
        id: id.to_string(),
        category: category.to_string(),
        command: env!("CARGO_BIN_EXE_mock_mcp_server").to_string(),
        args: vec![],
        env,
        transport: "stdio".into(),
        trust,
    }
}

#[tokio::test]
async fn ensure_server_then_server_tools_derives_danger_from_read_only_hint() {
    let manager = McpManager::new();
    let cfg = mock_server_config("mock-1");

    manager
        .ensure_server(&cfg)
        .await
        .expect("ensure_server 应成功");

    let tools = manager.server_tools(&cfg.id);
    assert_eq!(
        tools.len(),
        2,
        "应看到 mock server 的 2 个工具，实际 {tools:?}"
    );

    let write_tool = tools
        .iter()
        .find(|t| t.name == "write_file")
        .expect("应含 write_file 工具");
    assert_eq!(write_tool.danger, Danger::Write);

    let read_tool = tools
        .iter()
        .find(|t| t.name == "read_file")
        .expect("应含 read_file 工具");
    assert_eq!(read_tool.danger, Danger::Read);
}

#[tokio::test]
async fn ensure_server_twice_same_id_does_not_respawn() {
    let manager = McpManager::new();
    let cfg = mock_server_config("mock-2");

    manager
        .ensure_server(&cfg)
        .await
        .expect("第一次 ensure_server 应成功");
    assert_eq!(manager.spawn_count(), 1, "第一次应真实 spawn 一次");

    manager
        .ensure_server(&cfg)
        .await
        .expect("第二次 ensure_server 应成功（复用）");
    assert_eq!(
        manager.spawn_count(),
        1,
        "同一 serverId 的第二次 ensure_server 不应重新 spawn"
    );

    // 复用不影响已缓存的 tools。
    let tools = manager.server_tools(&cfg.id);
    assert_eq!(tools.len(), 2);
}

/// Important 1 回归测试：N 个并发 `ensure_server(同一个从未连接过的 id)` 在修复前
/// 会各自看到 `conns` 未命中、各自 spawn 一个子进程（"最后写入者赢"，其余的
/// `Child` 被各自的崩溃看护任务持有、永久阻塞在 `child.wait().await` 上——
/// 孤儿泄漏）。修复后（per-id 单飞门 + 拿锁后二次检查）N 次并发调用对同一个
/// 未连接 id 应该只真正 spawn 一次。
///
/// 用 multi_thread runtime + 多个 worker 线程，让 8 个 task 有真实机会并发跑到
/// `ensure_server` 内部同一个竞争窗口，而不是被单线程 executor 顺序调度掉。
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn ensure_server_concurrent_same_new_id_spawns_exactly_once() {
    let manager = std::sync::Arc::new(McpManager::new());
    let cfg = mock_server_config("mock-concurrent");

    let mut handles = Vec::new();
    for _ in 0..8 {
        let manager = manager.clone();
        let cfg = cfg.clone();
        handles.push(tokio::spawn(
            async move { manager.ensure_server(&cfg).await },
        ));
    }

    for h in handles {
        h.await
            .expect("ensure_server task 不应 panic")
            .expect("并发场景下 ensure_server 也应成功");
    }

    assert_eq!(
        manager.spawn_count(),
        1,
        "8 个并发 ensure_server(同一个未连接过的新 id) 应只真正 spawn 一次子进程，实际 spawn_count={}",
        manager.spawn_count()
    );

    let tools = manager.server_tools(&cfg.id);
    assert_eq!(tools.len(), 2, "单飞后仍应能看到握手缓存的 2 个工具");
}

/// Important 2 回归测试：mock server 以 `--hang` 启动时对任何请求（含
/// initialize）永不回应。修复前 `ensure_server` 会在 `rx.recv().await` 上永久
/// 挂起；修复后应在 `HANDSHAKE_TIMEOUT`（10s）内返回 `Err`，且必须真正杀掉挂起
/// 的子进程，不留孤儿。
///
/// 外层套一个 15s 的测试自身耐心上限（远大于 10s 的握手超时常量，但远小于
/// "永久挂起"）：如果修复失效导致 `ensure_server` 真的永久挂起，这层 timeout
/// 会让测试在 15s 内失败退出，而不是把整个 test suite 也一起挂死。
#[tokio::test]
async fn ensure_server_handshake_timeout_returns_err_and_kills_child() {
    let manager = McpManager::new();
    // 唯一 marker：避免并行跑的其它测试/进程的 mock_mcp_server 干扰下面的 pgrep 判定。
    let marker = format!(
        "mcp-hang-marker-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let mut cfg = mock_server_config("mock-hang");
    cfg.args = vec!["--hang".to_string(), format!("--marker={marker}")];

    let outcome = tokio::time::timeout(
        std::time::Duration::from_secs(15),
        manager.ensure_server(&cfg),
    )
    .await
    .expect("ensure_server 应在测试自身 15s 耐心内返回（握手超时机制不应让它永久挂起）");

    assert!(
        outcome.is_err(),
        "对一个只挂不答的 server，ensure_server 应返回 Err（握手超时），而不是 Ok"
    );

    // kill 是异步发信号，给内核一拍时间真正回收子进程再检查。
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    let leaked = std::process::Command::new("pgrep")
        .args(["-f", &marker])
        .output()
        .expect("pgrep 应可执行（macOS/Linux 自带）");
    assert!(
        leaked.stdout.is_empty(),
        "握手超时后不应留下孤儿 mock_mcp_server 子进程，但 pgrep 命中：{}",
        String::from_utf8_lossy(&leaked.stdout)
    );
}

/// Minor 3 回归测试：server 在真正的 `initialize` 响应之前，先主动推送一条没有
/// `id` 字段的 notification（例如日志/进度）。健壮的握手应按 JSON-RPC `id` 匹配
/// 期望的响应、跳过不匹配的帧，而不是"按到达顺序把第一条当 initialize 响应、
/// 第二条当 tools/list 响应"（那样会被 notification 挤占顺序，导致后续解析错位）。
#[tokio::test]
async fn ensure_server_skips_unmatched_notification_before_handshake_reply() {
    let manager = McpManager::new();
    let mut cfg = mock_server_config("mock-notify");
    cfg.args = vec!["--emit-notification".to_string()];

    manager.ensure_server(&cfg).await.expect(
        "server 先吐一条无 id 的 notification 不应打断按 id 匹配的握手，ensure_server 仍应成功",
    );

    let tools = manager.server_tools(&cfg.id);
    assert_eq!(
        tools.len(),
        2,
        "跳过 notification 帧后应仍能正确解析出 2 个工具"
    );
}

// ---------------------------------------------------------------------------
// Task 6: authorized_tools —— per-app 授权解析（category × access 可见性隔离）
//
// 安全属性：一个只声明了 Access::Read 的 app，其 authorized_tools() 结果里
// 绝不能出现任何 Danger::Write 的工具。这是 host 侧 Task7 二次复核之前的第一
// 层可见性收窄，也是本任务存在的意义。
// ---------------------------------------------------------------------------

/// 只读授权（Access::Read）→ 只看到 read_file，绝不能看到 write_file。
#[tokio::test]
async fn authorized_tools_read_only_app_sees_only_read_tool() {
    let manager = McpManager::new();
    let cfg = mock_server_config_with_category("mock-authz-read", "filesystem");
    manager
        .ensure_server(&cfg)
        .await
        .expect("ensure_server 应成功");

    let app_connectors = vec![ConnectorReq {
        category: "filesystem".to_string(),
        access: Access::Read,
    }];
    let authed = manager.authorized_tools(&app_connectors);

    assert!(
        authed.iter().any(|t| t.tool == "read_file"),
        "只读授权应看到 read_file，实际：{authed:?}"
    );
    assert!(
        !authed.iter().any(|t| t.tool == "write_file"),
        "只读授权绝不能看到 write_file（可见性隔离安全属性），实际：{authed:?}"
    );
    assert_eq!(
        authed.len(),
        1,
        "只读授权应恰好只有 1 个可见工具，实际：{authed:?}"
    );
}

/// 读写授权（Access::ReadWrite）→ read_file 和 write_file 都可见。
#[tokio::test]
async fn authorized_tools_readwrite_app_sees_both_tools() {
    let manager = McpManager::new();
    let cfg = mock_server_config_with_category("mock-authz-rw", "filesystem");
    manager
        .ensure_server(&cfg)
        .await
        .expect("ensure_server 应成功");

    let app_connectors = vec![ConnectorReq {
        category: "filesystem".to_string(),
        access: Access::ReadWrite,
    }];
    let authed = manager.authorized_tools(&app_connectors);

    assert!(
        authed.iter().any(|t| t.tool == "read_file"),
        "实际：{authed:?}"
    );
    assert!(
        authed.iter().any(|t| t.tool == "write_file"),
        "实际：{authed:?}"
    );
    assert_eq!(
        authed.len(),
        2,
        "读写授权应看到全部 2 个工具，实际：{authed:?}"
    );
}

/// app 声明的 category 没有任何已连接 server 匹配 → 贡献零工具（静默为空，不报错）。
#[tokio::test]
async fn authorized_tools_unmatched_category_returns_empty() {
    let manager = McpManager::new();
    let cfg = mock_server_config_with_category("mock-authz-nomatch", "filesystem");
    manager
        .ensure_server(&cfg)
        .await
        .expect("ensure_server 应成功");

    let app_connectors = vec![ConnectorReq {
        category: "calendar".to_string(),
        access: Access::Read,
    }];
    let authed = manager.authorized_tools(&app_connectors);

    assert!(
        authed.is_empty(),
        "未匹配的 connector category 应贡献零工具，实际：{authed:?}"
    );
}

/// 第二个 server 的 category 不在 app_connectors 里 → 该 server 整体不可见，
/// 即使它与另一个已授权的 server 同时连接着。
#[tokio::test]
async fn authorized_tools_ignores_server_of_unrequested_category() {
    let manager = McpManager::new();
    let fs_cfg = mock_server_config_with_category("mock-authz-multi-fs", "filesystem");
    let cal_cfg = mock_server_config_with_category("mock-authz-multi-cal", "calendar");
    manager
        .ensure_server(&fs_cfg)
        .await
        .expect("fs ensure_server 应成功");
    manager
        .ensure_server(&cal_cfg)
        .await
        .expect("cal ensure_server 应成功");

    let app_connectors = vec![ConnectorReq {
        category: "filesystem".to_string(),
        access: Access::ReadWrite,
    }];
    let authed = manager.authorized_tools(&app_connectors);

    assert!(
        authed.iter().all(|t| t.server == fs_cfg.id),
        "未在 app_connectors 里声明的 category(calendar) 的 server 不应可见，实际：{authed:?}"
    );
    assert_eq!(
        authed.len(),
        2,
        "应只看到 filesystem server 的 2 个工具，实际：{authed:?}"
    );
}

// ---------------------------------------------------------------------------
// Task 7a: call_tool —— 对已连接 server 发起 tools/call 往返（host_mcp_call
// 内部依赖的执行原语，这里单独覆盖，便于问题定位分层）。
// ---------------------------------------------------------------------------

#[tokio::test]
async fn call_tool_read_file_returns_mock_content() {
    let manager = McpManager::new();
    let cfg = mock_server_config("mock-call-read");
    manager
        .ensure_server(&cfg)
        .await
        .expect("ensure_server 应成功");

    let result = manager
        .call_tool(
            &cfg.id,
            "read_file",
            serde_json::json!({ "path": "/tmp/a.txt" }),
        )
        .await
        .expect("call_tool 应成功");

    let text = result["content"][0]["text"].as_str().unwrap_or("");
    assert!(
        text.contains("mock content of /tmp/a.txt"),
        "实际：{result:?}"
    );
}

#[tokio::test]
async fn call_tool_unknown_server_returns_err() {
    let manager = McpManager::new();
    let err = manager
        .call_tool("never-connected", "read_file", serde_json::json!({}))
        .await
        .expect_err("未连接的 server 应返回 Err");
    assert!(
        err.contains("never-connected"),
        "错误信息应带上 server id，实际：{err}"
    );
}

#[tokio::test]
async fn call_tool_unknown_tool_returns_err_from_server_error_response() {
    let manager = McpManager::new();
    let cfg = mock_server_config("mock-call-unknown-tool");
    manager
        .ensure_server(&cfg)
        .await
        .expect("ensure_server 应成功");

    let err = manager
        .call_tool(&cfg.id, "delete_everything", serde_json::json!({}))
        .await
        .expect_err("mock server 对未知工具名应回 JSON-RPC error，call_tool 应转成 Err");
    assert!(
        err.contains("delete_everything") || err.contains("Unknown tool"),
        "实际：{err}"
    );
}

// ---------------------------------------------------------------------------
// Task 7b: host_mcp_call —— __host_mcp_call__ 路由：二次授权复核 + 危险分级门 + 审计
//
// 安全属性：
// - 二次授权复核是纵深防御——即使 in-pi mcp-bridge 扩展的可见性过滤被篡改绕过、
//   请求真的打到了 host 侧，host 仍用同一份 authorized_tools 逻辑重新校验一遍，
//   未授权的 (server,tool) 一律 Denied，绝不会被执行（用 call_count 断言）。
// - Danger::Write 工具在本任务里绝不会被执行——只登记待确认，返回 PendingConfirm，
//   同样用 call_count 断言"确实没有被调用"。
// - 三个分支都必须落一条 audit 记录，且参数经脱敏。
// ---------------------------------------------------------------------------

/// 只读授权 app 调用已授权的 read_file → Ok(mock 内容)，产生 verdict=allowed 的
/// 审计记录，且 call_count 增至 1（工具确实被执行了一次）。
#[tokio::test]
async fn host_mcp_call_read_authorized_returns_ok_and_audits_allowed() {
    let manager = McpManager::new();
    let cfg = mock_server_config_with_category("mock-host-read-ok", "filesystem");
    manager
        .ensure_server(&cfg)
        .await
        .expect("ensure_server 应成功");
    let (_tmp, layout) = temp_layout();

    let app_connectors = vec![ConnectorReq {
        category: "filesystem".to_string(),
        access: Access::Read,
    }];
    let result = manager
        .host_mcp_call(
            "app-read",
            &app_connectors,
            &cfg.id,
            "read_file",
            serde_json::json!({ "path": "/tmp/foo.txt" }),
            &layout,
        )
        .await;

    match result {
        McpCallResult::Ok(v) => {
            let text = v["content"][0]["text"].as_str().unwrap_or("");
            assert!(
                text.contains("mock content of"),
                "应返回 mock server 的内容，实际：{v:?}"
            );
        }
        other => panic!("授权的只读调用应返回 Ok，实际：{other:?}"),
    }
    assert_eq!(
        manager.call_count(&cfg.id),
        1,
        "只读调用应确实执行了一次 tools/call"
    );

    let entries = audit::query(
        &layout,
        &AuditFilter {
            app_id: Some("app-read".into()),
            tool: None,
            limit: None,
        },
    );
    assert_eq!(
        entries.len(),
        1,
        "应恰好产生 1 条审计记录，实际：{entries:?}"
    );
    assert_eq!(entries[0].verdict, "allowed");
    assert_eq!(entries[0].tool, "read_file");
}

/// app 只读授权，调用未授权的 write_file → Denied("unauthorized")，工具未被执行
/// （call_count 保持 0），且产生 verdict=denied 的审计记录。
#[tokio::test]
async fn host_mcp_call_unauthorized_tool_is_denied_and_not_executed() {
    let manager = McpManager::new();
    let cfg = mock_server_config_with_category("mock-host-denied", "filesystem");
    manager
        .ensure_server(&cfg)
        .await
        .expect("ensure_server 应成功");
    let (_tmp, layout) = temp_layout();

    let app_connectors = vec![ConnectorReq {
        category: "filesystem".to_string(),
        access: Access::Read,
    }];
    let result = manager
        .host_mcp_call(
            "app-denied",
            &app_connectors,
            &cfg.id,
            "write_file",
            serde_json::json!({ "path": "/tmp/x", "content": "y" }),
            &layout,
        )
        .await;

    match result {
        McpCallResult::Denied(reason) => assert_eq!(reason, "unauthorized"),
        other => panic!("未授权工具应返回 Denied，实际：{other:?}"),
    }
    assert_eq!(manager.call_count(&cfg.id), 0, "未授权工具绝不应被执行");

    let entries = audit::query(
        &layout,
        &AuditFilter {
            app_id: Some("app-denied".into()),
            tool: None,
            limit: None,
        },
    );
    assert_eq!(
        entries.len(),
        1,
        "应恰好产生 1 条审计记录，实际：{entries:?}"
    );
    assert_eq!(entries[0].verdict, "denied");
}

/// 即便请求的是一个根本不存在于 server 工具列表里的 bogus 工具名，二次授权复核
/// 同样应该 Denied（不会因为"这个名字压根没在 authorized_tools 里"而走到别的分支）。
#[tokio::test]
async fn host_mcp_call_bogus_tool_is_denied_and_not_executed() {
    let manager = McpManager::new();
    let cfg = mock_server_config_with_category("mock-host-bogus", "filesystem");
    manager
        .ensure_server(&cfg)
        .await
        .expect("ensure_server 应成功");
    let (_tmp, layout) = temp_layout();

    let app_connectors = vec![ConnectorReq {
        category: "filesystem".to_string(),
        access: Access::ReadWrite,
    }];
    let result = manager
        .host_mcp_call(
            "app-bogus",
            &app_connectors,
            &cfg.id,
            "delete_everything",
            serde_json::json!({}),
            &layout,
        )
        .await;

    assert!(
        matches!(result, McpCallResult::Denied(_)),
        "不存在的工具应 Denied，实际：{result:?}"
    );
    assert_eq!(manager.call_count(&cfg.id), 0, "不存在的工具绝不应被执行");
}

/// app 读写授权，调用 write_file（Danger::Write）→ PendingConfirm(confirmId)，
/// 工具此时尚未被执行（call_count 保持 0——本任务规定 Write 工具绝不在此路径
/// 被调用，只登记待确认，真正执行留给 Task15 通知中心），且产生 verdict=pending
/// 的审计记录。
#[tokio::test]
async fn host_mcp_call_write_authorized_returns_pending_confirm_without_executing() {
    let manager = McpManager::new();
    let cfg = mock_server_config_with_category("mock-host-pending", "filesystem");
    manager
        .ensure_server(&cfg)
        .await
        .expect("ensure_server 应成功");
    let (_tmp, layout) = temp_layout();

    let app_connectors = vec![ConnectorReq {
        category: "filesystem".to_string(),
        access: Access::ReadWrite,
    }];
    let result = manager
        .host_mcp_call(
            "app-pending",
            &app_connectors,
            &cfg.id,
            "write_file",
            serde_json::json!({ "path": "/tmp/x", "content": "y" }),
            &layout,
        )
        .await;

    match result {
        McpCallResult::PendingConfirm(id) => assert!(!id.is_empty(), "confirmId 不应为空"),
        other => panic!("授权的写操作应返回 PendingConfirm，实际：{other:?}"),
    }
    assert_eq!(
        manager.call_count(&cfg.id),
        0,
        "写操作在本任务里绝不应被实际执行"
    );

    let entries = audit::query(
        &layout,
        &AuditFilter {
            app_id: Some("app-pending".into()),
            tool: None,
            limit: None,
        },
    );
    assert_eq!(
        entries.len(),
        1,
        "应恰好产生 1 条审计记录，实际：{entries:?}"
    );
    assert_eq!(entries[0].verdict, "pending");
    assert_eq!(entries[0].tool, "write_file");
}

/// 三个分支产生的 audit 记录里，参数中的密钥形态值必须被 audit::redact 脱敏，
/// 不能以明文落盘——验证 host_mcp_call 确实把原始 args 原样交给了 audit::record
/// （由 audit 自身负责脱敏），而不是自作主张跳过脱敏或提前动手脚。
#[tokio::test]
async fn host_mcp_call_redacts_secret_looking_arg_in_audit_record() {
    let manager = McpManager::new();
    let cfg = mock_server_config_with_category("mock-host-redact", "filesystem");
    manager
        .ensure_server(&cfg)
        .await
        .expect("ensure_server 应成功");
    let (_tmp, layout) = temp_layout();

    let app_connectors = vec![ConnectorReq {
        category: "filesystem".to_string(),
        access: Access::Read,
    }];
    let secret = "sk-abcdEFGH12345678wxyz";
    let _ = manager
        .host_mcp_call(
            "app-secret",
            &app_connectors,
            &cfg.id,
            "read_file",
            serde_json::json!({ "path": "/tmp/foo.txt", "token": secret }),
            &layout,
        )
        .await;

    let entries = audit::query(
        &layout,
        &AuditFilter {
            app_id: Some("app-secret".into()),
            tool: None,
            limit: None,
        },
    );
    assert_eq!(
        entries.len(),
        1,
        "应恰好产生 1 条审计记录，实际：{entries:?}"
    );
    assert!(
        !entries[0].args.contains(secret),
        "审计记录不应包含明文密钥，实际：{}",
        entries[0].args
    );
    assert!(
        entries[0].args.contains("***"),
        "脱敏后应含 *** 占位，实际：{}",
        entries[0].args
    );
}

// ---------------------------------------------------------------------------
// Task15: notifications::NotificationStore —— MCP 写确认续行流（用真实
// mock_mcp_server 覆盖 host_mcp_call -> PendingConfirm -> respond_confirm ->
// call_tool 的完整往返，不是纯内存 mock）。
// ---------------------------------------------------------------------------

/// 授权写操作 → PendingConfirm；respond_confirm(allow=true) 应续行执行这次
/// 被推迟的 write_file 调用——工具在 respond 之前绝不应被执行（call_count
/// 保持 0），respond 之后应恰好执行一次（call_count == 1），且拿到 mock
/// write_file 的 "ok" 结果。
#[tokio::test]
async fn respond_confirm_allow_resumes_pending_write_only_after_confirm() {
    let manager = McpManager::new();
    let cfg = mock_server_config_with_category("mock-confirm-allow", "filesystem");
    manager
        .ensure_server(&cfg)
        .await
        .expect("ensure_server 应成功");
    let (_tmp, layout) = temp_layout();
    install_minimal_package(&layout, "app-confirm-allow", "filesystem", "readwrite");

    let app_connectors = vec![ConnectorReq {
        category: "filesystem".to_string(),
        access: Access::ReadWrite,
    }];
    let result = manager
        .host_mcp_call(
            "app-confirm-allow",
            &app_connectors,
            &cfg.id,
            "write_file",
            serde_json::json!({ "path": "/tmp/x", "content": "y" }),
            &layout,
        )
        .await;
    let confirm_id = match result {
        McpCallResult::PendingConfirm(id) => id,
        other => panic!("授权的写操作应返回 PendingConfirm，实际：{other:?}"),
    };
    assert_eq!(
        manager.call_count(&cfg.id),
        0,
        "respond_confirm 之前，写操作绝不应被执行"
    );

    let store = NotificationStore::new(layout.clone(), manager.clone());
    let outcome = store
        .respond_confirm(&confirm_id, true, false)
        .await
        .expect("respond_confirm(allow=true) 应成功续行执行");

    assert_eq!(
        manager.call_count(&cfg.id),
        1,
        "respond_confirm(allow=true) 之后，写操作应恰好被执行一次"
    );
    let result_value = outcome.expect("allow=true 应返回被续行调用的结果");
    let text = result_value["content"][0]["text"].as_str().unwrap_or("");
    assert_eq!(
        text, "ok",
        "应拿到 mock write_file 的 ok 结果，实际：{result_value:?}"
    );
}

/// respond_confirm(allow=false) → 待确认写操作被丢弃，绝不执行（call_count
/// 保持 0），返回 `None`。
#[tokio::test]
async fn respond_confirm_deny_does_not_execute_pending_write() {
    let manager = McpManager::new();
    let cfg = mock_server_config_with_category("mock-confirm-deny", "filesystem");
    manager
        .ensure_server(&cfg)
        .await
        .expect("ensure_server 应成功");
    let (_tmp, layout) = temp_layout();

    let app_connectors = vec![ConnectorReq {
        category: "filesystem".to_string(),
        access: Access::ReadWrite,
    }];
    let result = manager
        .host_mcp_call(
            "app-confirm-deny",
            &app_connectors,
            &cfg.id,
            "write_file",
            serde_json::json!({ "path": "/tmp/x", "content": "y" }),
            &layout,
        )
        .await;
    let confirm_id = match result {
        McpCallResult::PendingConfirm(id) => id,
        other => panic!("授权的写操作应返回 PendingConfirm，实际：{other:?}"),
    };

    let store = NotificationStore::new(layout.clone(), manager.clone());
    let outcome = store
        .respond_confirm(&confirm_id, false, false)
        .await
        .expect("respond_confirm(allow=false) 本身不应返回 Err");

    assert!(outcome.is_none(), "deny 应返回 None（未执行）");
    assert_eq!(
        manager.call_count(&cfg.id),
        0,
        "respond_confirm(allow=false) 之后，写操作绝不应被执行"
    );
}

/// always=true → 记一条「总是允许」偏好；随后对同一 (app, server, tool) 的
/// 写操作 host_mcp_call 应直接执行（Ok，不再产生新的 PendingConfirm），证明
/// 总是允许偏好确实短路了 Danger::Write 门。
#[tokio::test]
async fn respond_confirm_always_allow_short_circuits_future_same_tool_write() {
    let manager = McpManager::new();
    let cfg = mock_server_config_with_category("mock-confirm-always", "filesystem");
    manager
        .ensure_server(&cfg)
        .await
        .expect("ensure_server 应成功");
    let (_tmp, layout) = temp_layout();
    install_minimal_package(&layout, "app-confirm-always", "filesystem", "readwrite");

    let app_connectors = vec![ConnectorReq {
        category: "filesystem".to_string(),
        access: Access::ReadWrite,
    }];

    // 第一次调用：走正常的待确认路径。
    let first = manager
        .host_mcp_call(
            "app-confirm-always",
            &app_connectors,
            &cfg.id,
            "write_file",
            serde_json::json!({ "path": "/tmp/a", "content": "1" }),
            &layout,
        )
        .await;
    let confirm_id = match first {
        McpCallResult::PendingConfirm(id) => id,
        other => panic!("第一次调用应返回 PendingConfirm，实际：{other:?}"),
    };

    let store = NotificationStore::new(layout.clone(), manager.clone());
    store
        .respond_confirm(&confirm_id, true, true)
        .await
        .expect("respond_confirm(allow=true, always=true) 应成功");
    assert_eq!(manager.call_count(&cfg.id), 1, "第一次确认续行后应执行一次");

    // 第二次调用：同一个 (app, server, tool)——总是允许偏好应命中，直接 Ok，
    // 不再产生 PendingConfirm。
    let second = manager
        .host_mcp_call(
            "app-confirm-always",
            &app_connectors,
            &cfg.id,
            "write_file",
            serde_json::json!({ "path": "/tmp/b", "content": "2" }),
            &layout,
        )
        .await;

    match second {
        McpCallResult::Ok(v) => {
            let text = v["content"][0]["text"].as_str().unwrap_or("");
            assert_eq!(text, "ok", "总是允许后应直接执行并拿到结果，实际：{v:?}");
        }
        other => {
            panic!("总是允许偏好命中后应直接 Ok，不应再是 PendingConfirm/Denied，实际：{other:?}")
        }
    }
    assert_eq!(
        manager.call_count(&cfg.id),
        2,
        "第二次调用应直接执行（总计两次真实执行）"
    );

    // 审计记录应体现第二次调用的 verdict 是 rule（P6-C：规则命中直接执行，
    // 与手动一次性确认的 executed 区分开，不是 pending）。三条分别来自：第一次
    // host_mcp_call 暂存（pending）、respond_confirm（经 Task4 的
    // respond_staged）验收执行（executed）、第二次 host_mcp_call 规则命中
    // （rule）。
    let entries = audit::query(
        &layout,
        &AuditFilter {
            app_id: Some("app-confirm-always".into()),
            tool: None,
            limit: None,
        },
    );
    assert_eq!(
        entries.len(),
        3,
        "暂存 + 验收执行 + 规则命中各应留一条审计，实际：{entries:?}"
    );
    assert_eq!(
        entries[0].verdict, "rule",
        "总是允许规则命中后的这次调用 verdict 应为 rule"
    );
}

/// 另一个 app 对同一 (server, tool) 的写操作不应受到别的 app 的「总是允许」
/// 偏好影响——总是允许偏好是按 (app_id, server, tool) 三元组记的，不是只按
/// (server, tool)。
#[tokio::test]
async fn always_allow_preference_is_scoped_per_app() {
    let manager = McpManager::new();
    let cfg = mock_server_config_with_category("mock-confirm-scope", "filesystem");
    manager
        .ensure_server(&cfg)
        .await
        .expect("ensure_server 应成功");
    let (_tmp, layout) = temp_layout();
    // "app-owner" 需要能真正过重新鉴权门才能验证「always 落的规则确实生效」；
    // "app-other" 不装包也没关系——它预期的断言只是"仍应走待确认路径"
    // （PendingConfirm 在 host_mcp_call 的规则命中检查之前，不涉及
    // respond_staged 的重新鉴权）。
    install_minimal_package(&layout, "app-owner", "filesystem", "readwrite");

    let app_connectors = vec![ConnectorReq {
        category: "filesystem".to_string(),
        access: Access::ReadWrite,
    }];

    let first = manager
        .host_mcp_call(
            "app-owner",
            &app_connectors,
            &cfg.id,
            "write_file",
            serde_json::json!({ "path": "/tmp/a", "content": "1" }),
            &layout,
        )
        .await;
    let confirm_id = match first {
        McpCallResult::PendingConfirm(id) => id,
        other => panic!("实际：{other:?}"),
    };
    let store = NotificationStore::new(layout.clone(), manager.clone());
    store
        .respond_confirm(&confirm_id, true, true)
        .await
        .expect("应成功");

    // 另一个 app（app-other）对同一 server/tool 的调用仍应走待确认路径。
    let second = manager
        .host_mcp_call(
            "app-other",
            &app_connectors,
            &cfg.id,
            "write_file",
            serde_json::json!({ "path": "/tmp/b", "content": "2" }),
            &layout,
        )
        .await;
    assert!(
        matches!(second, McpCallResult::PendingConfirm(_)),
        "总是允许偏好不应跨 app 生效，实际：{second:?}"
    );
}

// ---------------------------------------------------------------------------
// I-1 修复回归测试：connect_servers —— P3 whole-branch review 发现的生产接线
// 缺口（`ensure_server` 此前只在测试里被调用，生产路径没有任何调用点，
// `conns` 永远为空 → `authorized_tools` 永远返回 `[]`）的可测试核心：批量
// 把 `ServerConfig` 列表接进连接池，best-effort（一个失败不拖累其它）。
// 直接吃 `&[ServerConfig]`（不碰 `vault::*`），供 `lib.rs` 的两个生产触发点
// （启动时读 vault 批量接入 / `put_server` 时接入单个新配置）复用同一份
// 逻辑，也让这里能只用 mock server 配置单测，不碰真实 keychain。
// ---------------------------------------------------------------------------

/// 两个都指向真实 mock server 的配置 → 都应连接成功（`server_tools` 非空）。
#[tokio::test]
async fn connect_servers_connects_all_valid_configs() {
    let manager = McpManager::new();
    let cfg_a = mock_server_config_with_category("connect-a", "filesystem");
    let cfg_b = mock_server_config_with_category("connect-b", "calendar");

    manager
        .connect_servers(&[cfg_a.clone(), cfg_b.clone()])
        .await;

    assert_eq!(
        manager.server_tools(&cfg_a.id).len(),
        2,
        "connect-a 应已连接"
    );
    assert_eq!(
        manager.server_tools(&cfg_b.id).len(),
        2,
        "connect-b 应已连接"
    );
}

/// 一个配置指向不存在的命令（spawn 必然失败）、另一个指向真实 mock server：
/// 排在坏配置*之后*的好配置仍应成功连接——证明 `connect_servers` 是
/// best-effort 的，一个失败不会提前中止循环、也不影响其它配置。
#[tokio::test]
async fn connect_servers_skips_failing_config_without_aborting_others() {
    let manager = McpManager::new();
    let mut bad = mock_server_config_with_category("connect-bad", "filesystem");
    bad.command = "/nonexistent/definitely-not-a-real-binary".to_string();
    let good = mock_server_config_with_category("connect-good", "filesystem");

    manager.connect_servers(&[bad.clone(), good.clone()]).await;

    assert!(
        manager.server_tools(&bad.id).is_empty(),
        "spawn 失败的配置不应被当作已连接"
    );
    assert_eq!(
        manager.server_tools(&good.id).len(),
        2,
        "坏配置的失败不应影响排在它后面的好配置正常连接"
    );
}

/// PendingConfirm 产生的同时应有一条 confirm_request 通知被记下（Task15
/// wiring (a)：host_mcp_call 本身不建 NotificationStore，这里直接调用
/// `NotificationStore::record_pending_confirm`——mcp_socket.rs 里的生产接线
/// 做的正是同一件事，见该文件 process_request 的最小接线注释）。
#[tokio::test]
async fn pending_confirm_can_be_recorded_as_confirm_request_notification() {
    let manager = McpManager::new();
    let cfg = mock_server_config_with_category("mock-confirm-notify", "filesystem");
    manager
        .ensure_server(&cfg)
        .await
        .expect("ensure_server 应成功");
    let (_tmp, layout) = temp_layout();

    let app_connectors = vec![ConnectorReq {
        category: "filesystem".to_string(),
        access: Access::ReadWrite,
    }];
    let result = manager
        .host_mcp_call(
            "app-notify",
            &app_connectors,
            &cfg.id,
            "write_file",
            serde_json::json!({ "path": "/tmp/x", "content": "y" }),
            &layout,
        )
        .await;
    let confirm_id = match result {
        McpCallResult::PendingConfirm(id) => id,
        other => panic!("实际：{other:?}"),
    };

    let store = NotificationStore::new(layout.clone(), manager.clone());
    store
        .record_pending_confirm(&confirm_id, "app-notify", &cfg.id, "write_file")
        .expect("记通知应成功");

    let notifications = store.list(&NotificationFilter {
        app_id: Some("app-notify".into()),
        kind: Some("confirm_request".into()),
        ..Default::default()
    });
    assert_eq!(notifications.len(), 1);
    assert_eq!(notifications[0].id, confirm_id);
}

/// P6-C Task1 Step4：`authorized_tools` 必须现场按每个连接真实的 `trust`
/// 重新分级，而不是信 `ensure_server` 握手时缓存的（固定按 `Vetted` 算的）
/// `ToolInfo.danger`——`nuke_everything` 自称 `readOnlyHint=true` 但名字不落
/// 在前缀表任何一格：byo 下这个自称不生效（仍判 Write），vetted 下沿用旧
/// 规则（注解优先，判 Read）。
#[tokio::test]
async fn authorized_tools_danger_reflects_server_trust_not_just_annotation() {
    let manager = McpManager::new();

    let cfg_byo = mock_server_config_with_trust("mock-nuke-byo", "dev", Trust::Byo);
    manager
        .ensure_server(&cfg_byo)
        .await
        .expect("ensure_server(byo) 应成功");

    let cfg_vetted = mock_server_config_with_trust("mock-nuke-vetted", "dev", Trust::Vetted);
    manager
        .ensure_server(&cfg_vetted)
        .await
        .expect("ensure_server(vetted) 应成功");

    let app_connectors = vec![ConnectorReq {
        category: "dev".to_string(),
        access: Access::ReadWrite,
    }];
    let authed = manager.authorized_tools(&app_connectors);

    let byo_nuke = authed
        .iter()
        .find(|t| t.server == cfg_byo.id && t.tool == "nuke_everything")
        .expect("byo 连接应能看到 nuke_everything（ReadWrite 授权不按 danger 过滤）");
    assert_eq!(
        byo_nuke.danger,
        Danger::Write,
        "byo 下自称 readOnly 的未知工具仍应判 Write（不降危）"
    );

    let vetted_nuke = authed
        .iter()
        .find(|t| t.server == cfg_vetted.id && t.tool == "nuke_everything")
        .expect("vetted 连接应能看到 nuke_everything");
    assert_eq!(
        vetted_nuke.danger,
        Danger::Read,
        "vetted 下沿用旧规则，注解优先判 Read"
    );
}

// ---------------------------------------------------------------------------
// P6-C Task3：ApprovalStore::add_rule 预先落一条放行规则 -> host_mcp_call 对
// 该写工具直接执行（Ok），审计 verdict 是 "rule"（不是 "allowed"/"pending"）；
// 同一个 app 对另一个没有规则的写调用仍走暂存路径，list_staged(app) 能看到它。
// ---------------------------------------------------------------------------

#[tokio::test]
async fn rule_hit_executes_and_audits_verdict_rule() {
    let manager = McpManager::new();
    let cfg = mock_server_config_with_category("mock-rule-hit", "filesystem");
    manager
        .ensure_server(&cfg)
        .await
        .expect("ensure_server 应成功");
    let (_tmp, layout) = temp_layout();

    let app_connectors = vec![ConnectorReq {
        category: "filesystem".to_string(),
        access: Access::ReadWrite,
    }];

    let store = ApprovalStore::new(layout.clone());
    store
        .add_rule("app-rule", &cfg.id, "write_file", 1000)
        .expect("add_rule 应成功");

    let result = manager
        .host_mcp_call(
            "app-rule",
            &app_connectors,
            &cfg.id,
            "write_file",
            serde_json::json!({ "path": "/tmp/x", "content": "y" }),
            &layout,
        )
        .await;

    match result {
        McpCallResult::Ok(v) => {
            let text = v["content"][0]["text"].as_str().unwrap_or("");
            assert_eq!(text, "ok", "规则命中应直接执行并拿到结果，实际：{v:?}");
        }
        other => panic!("规则命中应直接 Ok，不应再是 PendingConfirm/Denied，实际：{other:?}"),
    }
    assert_eq!(
        manager.call_count(&cfg.id),
        1,
        "规则命中后写操作应恰好执行一次"
    );

    let entries = audit::query(
        &layout,
        &AuditFilter {
            app_id: Some("app-rule".into()),
            tool: None,
            limit: None,
        },
    );
    assert_eq!(
        entries.len(),
        1,
        "规则命中应恰好产生 1 条审计记录，实际：{entries:?}"
    );
    assert_eq!(
        entries[0].verdict, "rule",
        "规则命中的 verdict 应为 rule，不是 allowed/pending"
    );

    // 另一个没有放行规则覆盖的 app：同一个 server/tool 仍应走暂存路径，且能在
    // list_staged(app) 里看到它——证明规则命中是精确按 (app,server,tool) 生效
    // （见 approvals.rs `rules_exact_scope_and_revoke` 同款不变量），不是这个
    // server 上的这个 tool 从此对所有 app 都免检。
    let no_rule_app = manager
        .host_mcp_call(
            "app-no-rule",
            &app_connectors,
            &cfg.id,
            "write_file",
            serde_json::json!({ "path": "/tmp/z", "content": "y" }),
            &layout,
        )
        .await;
    match no_rule_app {
        McpCallResult::PendingConfirm(_) => {}
        other => panic!("无规则的 app 应仍走暂存路径，实际：{other:?}"),
    }
    let staged = store
        .list_staged(Some("app-no-rule"))
        .expect("list_staged 应成功");
    assert_eq!(staged.len(), 1, "无规则时应恰好暂存 1 条，实际：{staged:?}");
    assert_eq!(staged[0].tool, "write_file");
}

// ---------------------------------------------------------------------------
// 终审 Important 2：McpManager::disconnect —— 从连接池移除 + kill 子进程，
// 幂等；之后 authorized_tools/server_tools/call_tool 都不应再看到这个 server。
// ---------------------------------------------------------------------------

#[tokio::test]
async fn disconnect_removes_conn_and_kills_child_process() {
    let manager = McpManager::new();
    // 唯一 marker：同 `ensure_server_handshake_timeout_returns_err_and_kills_child`
    // 的 pgrep 判定手法，避免并行跑的其它测试互相干扰。
    let marker = format!(
        "mcp-disconnect-marker-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let mut cfg = mock_server_config("mock-disconnect");
    cfg.args = vec![format!("--marker={marker}")];
    manager
        .ensure_server(&cfg)
        .await
        .expect("ensure_server 应成功");
    assert!(
        manager.server_pid(&cfg.id).is_some(),
        "连上之后应能查到 pid"
    );

    manager.disconnect(&cfg.id).await;

    assert!(
        manager.server_pid(&cfg.id).is_none(),
        "disconnect 后 conns 里不应再有该 server"
    );
    assert!(
        manager.server_tools(&cfg.id).is_empty(),
        "disconnect 后 server_tools 应为空"
    );

    // 幂等：第二次 disconnect 不应 panic / 不应有其它副作用。
    manager.disconnect(&cfg.id).await;

    // kill 是异步发信号，给内核一拍时间真正回收子进程再检查。
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    let leaked = std::process::Command::new("pgrep")
        .args(["-f", &marker])
        .output()
        .expect("pgrep 应可执行（macOS/Linux 自带）");
    assert!(
        leaked.stdout.is_empty(),
        "disconnect 后不应留下孤儿子进程，但 pgrep 命中：{}",
        String::from_utf8_lossy(&leaked.stdout)
    );
}

/// 终审 Important 2：`disconnect` 之后 `authorized_tools` 不应再列出该
/// server 的工具——`respond_staged` 的验收前重新鉴权正是靠这一点对"server
/// 已删"也 fail-closed，见 `p6c_approvals_it.rs::reauth_after_disconnect_*`。
#[tokio::test]
async fn disconnect_makes_authorized_tools_stop_listing_its_tools() {
    let manager = McpManager::new();
    let cfg = mock_server_config_with_category("mock-disconnect-authz", "dev");
    manager
        .ensure_server(&cfg)
        .await
        .expect("ensure_server 应成功");

    let app_connectors = vec![ConnectorReq {
        category: "dev".to_string(),
        access: Access::ReadWrite,
    }];
    assert!(
        manager
            .authorized_tools(&app_connectors)
            .iter()
            .any(|t| t.server == cfg.id),
        "断开前应能看到该 server 的工具"
    );

    manager.disconnect(&cfg.id).await;

    assert!(
        !manager
            .authorized_tools(&app_connectors)
            .iter()
            .any(|t| t.server == cfg.id),
        "disconnect 后 authorized_tools 不应再列出该 server 的任何工具"
    );
}

// ---------------------------------------------------------------------------
// 审查修复轮 2 Important 3：McpManager::all_tool_names——格式化、is_safe_name
// 过滤、known_tools() 端到端把已连接 MCP server 的工具纳入技能安装门。
// ---------------------------------------------------------------------------

fn skill_fixture(name: &str) -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/skills")
        .join(name)
}

fn local_skill_source() -> SkillSource {
    SkillSource {
        kind: SkillSourceKind::Local,
        url: None,
        sha256: None,
    }
}

#[tokio::test]
async fn all_tool_names_formats_dedupes_and_filters_unsafe_names() {
    let manager = McpManager::new();
    let cfg_a = mock_server_config("mcp-names-a");
    let cfg_b = mock_server_config("mcp-names-b");
    manager
        .ensure_server(&cfg_a)
        .await
        .expect("ensure_server a 应成功");
    manager
        .ensure_server(&cfg_b)
        .await
        .expect("ensure_server b 应成功");

    // is_safe_name 的反例：server id 含空格——`connectors::is_safe_name` 只接受
    // ASCII 字母数字与 `_.-`（见其文档/测试），空格被拒。ServerConfig.id 本身只是
    // 一个 HashMap key（不落磁盘路径，见 mcp.rs::spawn_and_handshake），所以能正常
    // spawn+握手，只是格式化后的工具名会被 all_tool_names 整体过滤掉。
    let cfg_unsafe = mock_server_config("unsafe server");
    manager
        .ensure_server(&cfg_unsafe)
        .await
        .expect("ensure_server 不安全 id 应仍能连上（id 只是 HashMap key）");

    let names = manager.all_tool_names();

    for expect in [
        "mcp__mcp-names-a__read_file",
        "mcp__mcp-names-a__write_file",
        "mcp__mcp-names-b__read_file",
        "mcp__mcp-names-b__write_file",
    ] {
        assert!(
            names.iter().any(|n| n == expect),
            "应含 {expect}：{names:?}"
        );
    }

    assert!(
        !names.iter().any(|n| n.contains("unsafe server")),
        "不安全 server id 应被 is_safe_name 过滤掉：{names:?}"
    );

    let mut sorted = names.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(
        sorted.len(),
        names.len(),
        "all_tool_names 不应产生重复项：{names:?}"
    );
    assert_eq!(
        names.len(),
        4,
        "两个安全 server 各 2 个工具，不安全 server 的工具应被整体过滤，实际 {names:?}"
    );
}

/// 端到端：`known_tools()` 把已连接 MCP server 的工具纳入技能安装门
/// （`lib.rs::known_tools` 第 4 条）——未连接时同一技能被 `UnknownTools` 拒绝，
/// 连接后同一技能能装。
#[tokio::test]
async fn known_tools_installs_skill_declaring_mcp_tool_only_after_connect() {
    let manager = McpManager::new();
    let (_tmp, layout) = temp_layout();
    let store = SkillStore::new(layout);

    // 未连接：known_tools() 里不含 mcp__mcp-e2e__read_file，声明它的技能应被拒。
    let known_before = known_tools(&manager);
    assert!(
        !known_before.iter().any(|t| t == "mcp__mcp-e2e__read_file"),
        "未连接前不应已知这个 MCP 工具名：{known_before:?}"
    );
    let err = store
        .install_from_dir(
            &skill_fixture("mcp-tool-skill"),
            local_skill_source(),
            true,
            &known_before,
            0,
        )
        .expect_err("未连接时声明 mcp__mcp-e2e__read_file 的技能应被拒装");
    assert!(
        matches!(
            err,
            super_agent_os::skills::SkillInstallError::UnknownTools(_)
        ),
        "应是 UnknownTools，实际：{err:?}"
    );

    // 连接后：known_tools() 应该纳入这个 server 的工具，同一份技能能装。
    let cfg = mock_server_config("mcp-e2e");
    manager
        .ensure_server(&cfg)
        .await
        .expect("ensure_server 应成功");
    let known_after = known_tools(&manager);
    assert!(
        known_after.iter().any(|t| t == "mcp__mcp-e2e__read_file"),
        "连接后应已知这个 MCP 工具名：{known_after:?}"
    );
    store
        .install_from_dir(
            &skill_fixture("mcp-tool-skill"),
            local_skill_source(),
            true,
            &known_after,
            0,
        )
        .expect("连接后声明 mcp__mcp-e2e__read_file 的技能应能安装");
}
