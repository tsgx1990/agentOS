use std::path::Path;
use super_agent_os::capabilities;
use super_agent_os::capability::{CallCtx, CallerIdentity, LaunchCtx};
use super_agent_os::maker::MAKER_APP_ID;
use super_agent_os::mcp::McpManager;
use super_agent_os::paths::DataLayout;
use super_agent_os::permissions::Permissions;

fn ctx<'a>(
    layout: &'a DataLayout,
    mcp: &'a McpManager,
    app_id: &'a str,
    sock: &'a Path,
) -> LaunchCtx<'a> {
    LaunchCtx {
        app_id,
        trusted: true,
        sandboxed: true,
        materialize: true,
        layout,
        hosttools_dir: Path::new("/ht"),
        socket_path: sock,
        mcp,
    }
}

#[test]
fn router_identity_gets_maker_call_and_router_bridges_and_tools() {
    let tmp = tempfile::tempdir().unwrap();
    let layout = DataLayout::new(tmp.path().to_path_buf());
    let mcp = McpManager::new();
    let sock = layout.mcp_socket_path(MAKER_APP_ID);
    let reg = capabilities::builtin();
    let c = reg
        .launch(
            &Permissions::default(),
            &CallerIdentity::installing(MAKER_APP_ID, true),
            &ctx(&layout, &mcp, MAKER_APP_ID, &sock),
        )
        .unwrap();
    for b in [
        "maker_bridge.ts",
        "call_agent_bridge.ts",
        "router_bridge.ts",
    ] {
        assert!(c.bridges.contains(&b), "{:?}", c.bridges);
    }
    for t in [
        "__host_call_agent__",
        "__host_list_agents__",
        "__host_maker_stage_write__",
        "__host_maker_preview__",
        "__host_maker_install__",
    ] {
        assert!(c.tools.contains(&t.to_string()), "{:?}", c.tools);
    }
    assert!(c.needs_socket);
    // 只含 SUPERAGENT_MCP_SOCKET 的最小 env（无 connectors 时），与 legacy bridge_aware_launch_extras 一致
    assert_eq!(
        c.env,
        vec![(
            "SUPERAGENT_MCP_SOCKET".to_string(),
            sock.to_string_lossy().to_string()
        )]
    );
}

#[test]
fn ordinary_app_with_agents_call_gets_only_call_bridge() {
    let tmp = tempfile::tempdir().unwrap();
    let layout = DataLayout::new(tmp.path().to_path_buf());
    let mcp = McpManager::new();
    let sock = layout.mcp_socket_path("a");
    let mut p = Permissions::default();
    p.agents.call.push("@superagent/summarizer".into());
    let c = capabilities::builtin()
        .launch(
            &p,
            &CallerIdentity::installing("a", false),
            &ctx(&layout, &mcp, "a", &sock),
        )
        .unwrap();
    assert!(c.bridges.contains(&"call_agent_bridge.ts"));
    assert!(!c.bridges.contains(&"maker_bridge.ts"));
    assert!(!c.bridges.contains(&"router_bridge.ts"));
    assert!(c.tools.contains(&"__host_call_agent__".to_string()));
    assert!(!c.tools.contains(&"__host_list_agents__".to_string()));
    let human = capabilities::builtin().render_human(&p, &CallerIdentity::installing("a", false));
    assert!(
        human
            .iter()
            .any(|l| l.contains("调用其他应用") && l.contains("@superagent/summarizer")),
        "{human:?}"
    );
}

// ---- F3（review）：maker/router 只在深度 0（前台/task-mode 会话）声明——一个
// 拿到 `agents.call: ["superagent"]` 的第三方能把调用链引到深度≥1 的 router
// 会话，不能因为 app_id 仍是 MAKER_APP_ID 就继续拿到 maker/list_agents 这两个
// 特权工具面 ----

#[test]
fn router_identity_at_depth_1_loses_maker_and_router_but_keeps_call_bridge() {
    let tmp = tempfile::tempdir().unwrap();
    let layout = DataLayout::new(tmp.path().to_path_buf());
    let mcp = McpManager::new();
    let sock = layout.mcp_socket_path(MAKER_APP_ID);
    let reg = capabilities::builtin();
    let id = CallerIdentity {
        app_id: MAKER_APP_ID.into(),
        trusted: true,
        depth: 1,
    };
    let c = reg
        .launch(
            &Permissions::default(),
            &id,
            &ctx(&layout, &mcp, MAKER_APP_ID, &sock),
        )
        .unwrap();
    for b in ["maker_bridge.ts", "router_bridge.ts"] {
        assert!(
            !c.bridges.contains(&b),
            "depth=1 不应再贡献 {b}：{:?}",
            c.bridges
        );
    }
    for t in [
        "__host_maker_stage_write__",
        "__host_maker_preview__",
        "__host_maker_install__",
        "__host_list_agents__",
    ] {
        assert!(
            !c.tools.contains(&t.to_string()),
            "depth=1 不应再贡献 {t}：{:?}",
            c.tools
        );
    }
    // agents_call 能力不按深度门控——call_agent_bridge.ts 仍在（router 豁免）。
    assert!(
        c.bridges.contains(&"call_agent_bridge.ts"),
        "{:?}",
        c.bridges
    );
}

#[tokio::test]
async fn dispatch_of_maker_method_at_depth_1_is_unauthorized() {
    let tmp = tempfile::tempdir().unwrap();
    let layout = DataLayout::new(tmp.path().to_path_buf());
    let mcp = McpManager::new();
    let call_ctx = CallCtx {
        layout: &layout,
        mcp: &mcp,
        hosttools_dir: Some(Path::new("/ht")),
    };
    let id = CallerIdentity {
        app_id: MAKER_APP_ID.into(),
        trusted: true,
        depth: 1,
    };
    let reg = capabilities::builtin();
    let r = reg
        .dispatch(
            "__host_maker_stage_write__",
            serde_json::json!({}),
            &id,
            &Permissions::default(),
            &call_ctx,
        )
        .await;
    assert_eq!(r["ok"], false, "{r}");
    assert!(r["error"].as_str().unwrap().contains("unauthorized"), "{r}");
}

#[tokio::test]
async fn non_router_dispatch_of_maker_and_list_agents_is_denied() {
    let tmp = tempfile::tempdir().unwrap();
    let layout = DataLayout::new(tmp.path().to_path_buf());
    let mcp = McpManager::new();
    let ctx = CallCtx {
        layout: &layout,
        mcp: &mcp,
        hosttools_dir: Some(Path::new("/ht")),
    };
    let id = CallerIdentity {
        app_id: "evil".into(),
        trusted: false,
        depth: 0,
    };
    let reg = capabilities::builtin();
    for m in ["__host_maker_stage_write__", "__host_list_agents__"] {
        let r = reg
            .dispatch(m, serde_json::json!({}), &id, &Permissions::default(), &ctx)
            .await;
        assert_eq!(r["ok"], false, "{m}: {r}");
        assert!(
            r["error"].as_str().unwrap().contains("unauthorized"),
            "{m}: {r}"
        );
    }
}

#[tokio::test]
async fn call_agent_without_hosttools_reports_bus_disabled() {
    let tmp = tempfile::tempdir().unwrap();
    let layout = DataLayout::new(tmp.path().to_path_buf());
    let mcp = McpManager::new();
    let ctx = CallCtx {
        layout: &layout,
        mcp: &mcp,
        hosttools_dir: None,
    };
    let mut p = Permissions::default();
    p.agents.call.push("@x/y".into());
    let id = CallerIdentity {
        app_id: "a".into(),
        trusted: false,
        depth: 0,
    };
    let r = capabilities::builtin()
        .dispatch(
            "__host_call_agent__",
            serde_json::json!({"target":"@x/y","prompt":"hi"}),
            &id,
            &p,
            &ctx,
        )
        .await;
    assert_eq!(r["ok"], false);
    assert!(r["error"]
        .as_str()
        .unwrap()
        .contains("call bus 未在此监听器启用"));
}
