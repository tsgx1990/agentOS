//! P6-A Task3：connectors 能力的启动期贡献额外贡献 `mcp__<server>__<tool>` 工具名
//! （修 --tools 白名单过滤缺陷，spec §1.3）。
//!
//! Task8：迁移前的 `session_mgr::mcp_launch_extras` 已随四条会话路径改走
//! `CapabilityRegistry::launch` 一并删除——`connectors_launch_matches_legacy_...`
//! 那条"与 legacy 字节级等价"测试改为与一份固定快照比较（env[0] 是
//! `SUPERAGENT_MCP_TOOLS`，其 JSON 数组每项恰好含 server/tool/name/description/
//! inputSchema 五个键；env[1] 是 `SUPERAGENT_MCP_SOCKET`），不再依赖已删除的函数。
use std::path::Path;
use super_agent_os::capabilities;
use super_agent_os::capability::{CallerIdentity, LaunchCtx};
use super_agent_os::mcp::McpManager;
use super_agent_os::paths::DataLayout;
use super_agent_os::permissions::{Access, ConnectorReq, Permissions};

fn perms_with_fs_readwrite() -> Permissions {
    let mut p = Permissions::default();
    p.connectors.push(ConnectorReq {
        category: "filesystem".into(),
        access: Access::ReadWrite,
    });
    p
}

#[tokio::test]
async fn connectors_launch_matches_fixed_snapshot_and_adds_tool_names() {
    let tmp = tempfile::tempdir().unwrap();
    let layout = DataLayout::new(tmp.path().to_path_buf());
    let mcp = McpManager::new();
    // 起一个 mock MCP server 让 authorized_tools 非空：复用 tests/mcp_manager_it.rs 里的 ServerConfig 构造方式
    let cfg = super_agent_os::vault::ServerConfig {
        id: "fs1".into(),
        category: "filesystem".into(),
        command: env!("CARGO_BIN_EXE_mock_mcp_server").into(),
        args: vec![],
        env: Default::default(),
        transport: "stdio".into(),
        trust: Default::default(),
    };
    mcp.ensure_server(&cfg).await.unwrap();

    let perms = perms_with_fs_readwrite();
    let hosttools = Path::new("/ht");
    let sock = layout.mcp_socket_path("app1");

    let reg = capabilities::builtin();
    let ctx = LaunchCtx {
        app_id: "app1",
        trusted: false,
        sandboxed: true,
        materialize: true,
        layout: &layout,
        hosttools_dir: hosttools,
        socket_path: &sock,
        mcp: &mcp,
    };
    let c = reg
        .launch(&perms, &CallerIdentity::installing("app1", false), &ctx)
        .unwrap();

    // 固定快照（legacy mcp_launch_extras 已随 Task8 删除，不再跟它比字节级等价）：
    // env[0] 是 SUPERAGENT_MCP_TOOLS，其 JSON 数组每项恰好含 server/tool/name/
    // description/inputSchema 五个键；env[1] 是 SUPERAGENT_MCP_SOCKET = sock。
    assert_eq!(c.env[0].0, "SUPERAGENT_MCP_TOOLS");
    let arr: Vec<serde_json::Value> = serde_json::from_str(&c.env[0].1).expect("应是合法 JSON");
    assert!(!arr.is_empty(), "读写授权应产出非空工具数组");
    for entry in &arr {
        let obj = entry.as_object().expect("每项应是 JSON 对象");
        let mut keys: Vec<&str> = obj.keys().map(|s| s.as_str()).collect();
        keys.sort();
        assert_eq!(
            keys,
            vec!["description", "inputSchema", "name", "server", "tool"],
            "{entry:?}"
        );
    }
    assert_eq!(
        c.env[1],
        (
            "SUPERAGENT_MCP_SOCKET".to_string(),
            sock.to_string_lossy().to_string()
        )
    );

    // 桥：ui_emit 恒有 + mcp_bridge
    assert!(c.bridges.contains(&"ui_emit.ts"));
    assert!(c.bridges.contains(&"mcp_bridge.ts"));
    // 工具名：每个授权工具一个 mcp__<server>__<tool>，外加 __host_ui_emit__
    let authed = mcp.authorized_tools(&perms.connectors);
    assert!(!authed.is_empty());
    for t in &authed {
        assert!(
            c.tools.contains(&format!("mcp__{}__{}", t.server, t.tool)),
            "{:?}",
            c.tools
        );
    }
    assert!(c.tools.contains(&"__host_ui_emit__".to_string()));
    assert!(c.needs_socket);
}

#[test]
fn connectors_render_human_distinguishes_read_and_readwrite() {
    let reg = capabilities::builtin();
    let id = CallerIdentity::installing("app1", false);
    let rw = reg.render_human(&perms_with_fs_readwrite(), &id);
    assert!(
        rw.iter()
            .any(|l| l.contains("文件系统") && l.contains("读写")),
        "{rw:?}"
    );
    let mut ro = Permissions::default();
    ro.connectors.push(ConnectorReq {
        category: "calendar".into(),
        access: Access::Read,
    });
    let ro_lines = reg.render_human(&ro, &id);
    assert!(
        ro_lines
            .iter()
            .any(|l| l.contains("calendar") && l.contains("只读")),
        "{ro_lines:?}"
    );
    assert!(!ro_lines
        .iter()
        .any(|l| l == super_agent_os::capability::NO_EXTRA_PERMISSIONS));
}

#[test]
fn ui_emit_is_always_declared_but_never_rendered() {
    let reg = capabilities::builtin();
    let id = CallerIdentity::installing("app1", false);
    let lines = reg.render_human(&Permissions::default(), &id);
    assert_eq!(
        lines,
        vec![super_agent_os::capability::NO_EXTRA_PERMISSIONS.to_string()]
    );
}

/// P6-C Task5：卸载应用后 `connectors` 能力的 `on_uninstall` 必须清空该
/// `app_id` 名下的放行规则与暂存调用（spec §4 步骤 5「卸载即清」），且对
/// 其它应用的记录秋毫无犯；钩子本身经 `CapabilityRegistry::on_uninstall`
/// 无条件对全部能力调用（不看 `declared`），所以就算这个 app 从未声明过
/// connectors 权限，直接调用钩子也必须是干净的 no-op（幂等）。
#[test]
fn uninstall_hook_clears_rules_and_staged() {
    let tmp = tempfile::tempdir().unwrap();
    let layout = DataLayout::new(tmp.path().to_path_buf());
    let store = super_agent_os::approvals::ApprovalStore::new(layout.clone());

    // app "a"：一条放行规则 + 一条暂存调用；app "b"：一条放行规则，须不受影响。
    store.add_rule("a", "fs1", "read_file", 1).unwrap();
    store.add_rule("b", "fs1", "read_file", 1).unwrap();
    let staged_id = store
        .stage(
            "a",
            "fs1",
            "write_file",
            serde_json::json!({"path": "x"}),
            1,
        )
        .unwrap();

    let reg = capabilities::builtin();
    let errs = reg.on_uninstall("a", &layout);
    assert!(errs.is_empty(), "{errs:?}");

    assert!(store.list_rules(Some("a")).unwrap().is_empty());
    assert!(store.list_staged(Some("a")).unwrap().is_empty());
    // 暂存项已被 take 走，不能再被验收。
    assert!(store.take(&staged_id).unwrap().is_none());
    // app "b" 的规则原样保留。
    assert_eq!(store.list_rules(Some("b")).unwrap().len(), 1);

    // 幂等：该 app 已没有任何记录，再调用一次不报错、不 panic。
    assert!(reg.on_uninstall("a", &layout).is_empty());

    // 从未声明过 connectors 权限的 app 直接调用钩子也是干净的 no-op。
    assert!(reg
        .on_uninstall("never-installed-connectors", &layout)
        .is_empty());
}

/// 终审 Important 3：卸载/升级丢弃暂存调用时不能只审计——原 `confirm_request`
/// 通知必须被 ack（否则永远停在"待处理"），且要落一条 `update` 通知说明发生
/// 了什么；规则被清空时还要另落一条汇总 `update`。
#[test]
fn on_uninstall_acks_confirm_request_and_notifies_update_for_dropped_staged_and_cleared_rules() {
    let tmp = tempfile::tempdir().unwrap();
    let layout = DataLayout::new(tmp.path().to_path_buf());
    let notifications =
        super_agent_os::notifications::NotificationStore::new(layout.clone(), McpManager::new());

    // 一条放行规则 + 一条真正经 `create_confirm` 落了 `confirm_request` 通知
    // 的暂存调用（不是直接 `ApprovalStore::stage`——要覆盖"原通知确实被 ack"
    // 这条断言，必须先有一条通知存在）。
    let staged_id = notifications
        .create_confirm(
            "app-u",
            "fs1",
            "write_file",
            serde_json::json!({"path": "x"}),
        )
        .expect("create_confirm 应成功");
    let store = super_agent_os::approvals::ApprovalStore::new(layout.clone());
    store.add_rule("app-u", "fs1", "read_file", 1).unwrap();

    let confirm_before = notifications.list(&super_agent_os::notifications::NotificationFilter {
        app_id: Some("app-u".into()),
        kind: Some("confirm_request".into()),
        ..Default::default()
    });
    assert_eq!(confirm_before.len(), 1);
    assert!(!confirm_before[0].acked, "卸载前该通知应仍是未读状态");

    let reg = capabilities::builtin();
    let errs = reg.on_uninstall("app-u", &layout);
    assert!(errs.is_empty(), "{errs:?}");

    // 原 confirm_request 通知已被 ack。
    let confirm_after = notifications.list(&super_agent_os::notifications::NotificationFilter {
        app_id: Some("app-u".into()),
        kind: Some("confirm_request".into()),
        ..Default::default()
    });
    assert_eq!(confirm_after.len(), 1);
    assert!(
        confirm_after[0].id == staged_id && confirm_after[0].acked,
        "原 confirm_request 通知应已被 ack，实际：{confirm_after:?}"
    );

    // 新增了针对被丢弃暂存调用的 update 通知，且措辞说明了原因。
    let updates = notifications.list(&super_agent_os::notifications::NotificationFilter {
        app_id: Some("app-u".into()),
        kind: Some("update".into()),
        ..Default::default()
    });
    assert!(
        updates
            .iter()
            .any(|n| n.body.contains("write_file") && n.body.contains("卸载/升级")),
        "应有一条说明暂存调用因卸载/升级被拒绝的 update 通知，实际：{updates:?}"
    );
    // 规则被清空（1 条）也应落一条汇总 update。
    assert!(
        updates.iter().any(|n| n.body.contains("1 条自动放行规则")),
        "应有一条汇总清除放行规则条数的 update 通知，实际：{updates:?}"
    );
}
