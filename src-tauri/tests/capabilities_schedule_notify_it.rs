use std::path::Path;
use super_agent_os::capabilities;
use super_agent_os::capability::{CallCtx, CallerIdentity};
use super_agent_os::mcp::McpManager;
use super_agent_os::notifications::{NotificationFilter, NotificationStore};
use super_agent_os::paths::DataLayout;
use super_agent_os::permissions::{Permissions, ScheduledTask};
use super_agent_os::registry::InstalledApp;
use super_agent_os::scheduler::TaskRegistry;

fn app(id: &str) -> InstalledApp {
    InstalledApp {
        app_id: id.into(),
        name: id.into(),
        version: "1.0.0".into(),
        display_name: id.into(),
        category: "life".into(),
        icon: None,
        trusted: false,
        domains: vec![],
    }
}

#[test]
fn schedule_on_install_registers_and_on_uninstall_removes_tasks() {
    let tmp = tempfile::tempdir().unwrap();
    let layout = DataLayout::new(tmp.path().to_path_buf());
    let mut p = Permissions::default();
    p.system.schedule = true;
    p.scheduled_tasks.push(ScheduledTask {
        id: "t1".into(),
        cron: "0 8 * * *".into(),
        prompt: "p".into(),
        catch_up: true,
    });
    let reg = capabilities::builtin();
    reg.on_install(&app("a"), &p, &layout).unwrap();
    assert_eq!(
        TaskRegistry::new(&layout)
            .all()
            .iter()
            .filter(|t| t.app_id == "a")
            .count(),
        1
    );
    let human = reg.render_human(&p, &CallerIdentity::installing("a", false));
    assert!(
        human
            .iter()
            .any(|l| l.contains("定时任务") && l.contains("t1")),
        "{human:?}"
    );
    assert!(reg.on_uninstall("a", &layout).is_empty());
    assert_eq!(
        TaskRegistry::new(&layout)
            .all()
            .iter()
            .filter(|t| t.app_id == "a")
            .count(),
        0
    );
}

#[test]
fn schedule_not_permitted_registers_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let layout = DataLayout::new(tmp.path().to_path_buf());
    let mut p = Permissions::default();
    p.scheduled_tasks.push(ScheduledTask {
        id: "t1".into(),
        cron: "0 8 * * *".into(),
        prompt: "p".into(),
        catch_up: true,
    });
    capabilities::builtin()
        .on_install(&app("a"), &p, &layout)
        .unwrap();
    assert!(TaskRegistry::new(&layout).all().is_empty());
}

#[tokio::test]
async fn notify_writes_app_notice_when_declared_and_rate_limits() {
    let tmp = tempfile::tempdir().unwrap();
    let layout = DataLayout::new(tmp.path().to_path_buf());
    let mcp = McpManager::new();
    let ctx = CallCtx {
        layout: &layout,
        mcp: &mcp,
        hosttools_dir: None,
    };
    let mut p = Permissions::default();
    p.system.notifications = true;
    let id = CallerIdentity {
        app_id: "a".into(),
        trusted: false,
        depth: 0,
    };
    let reg = capabilities::builtin();
    let r = reg
        .dispatch(
            "__host_notify__",
            serde_json::json!({"title":"早报","body":"三件事"}),
            &id,
            &p,
            &ctx,
        )
        .await;
    assert_eq!(r["ok"], true, "{r}");
    let store = NotificationStore::new(layout.clone(), mcp.clone());
    let list = store.list(&NotificationFilter {
        app_id: Some("a".into()),
        kind: Some("app_notice".into()),
        ..Default::default()
    });
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].title, "早报");
    for _ in 0..9 {
        reg.dispatch(
            "__host_notify__",
            serde_json::json!({"title":"x","body":"y"}),
            &id,
            &p,
            &ctx,
        )
        .await;
    }
    let over = reg
        .dispatch(
            "__host_notify__",
            serde_json::json!({"title":"x","body":"y"}),
            &id,
            &p,
            &ctx,
        )
        .await;
    assert_eq!(over["ok"], false);
    assert_eq!(over["error"], "rate_limited");
}

#[tokio::test]
async fn notify_undeclared_is_denied() {
    let tmp = tempfile::tempdir().unwrap();
    let layout = DataLayout::new(tmp.path().to_path_buf());
    let mcp = McpManager::new();
    let ctx = CallCtx {
        layout: &layout,
        mcp: &mcp,
        hosttools_dir: None,
    };
    let id = CallerIdentity {
        app_id: "a".into(),
        trusted: false,
        depth: 0,
    };
    let r = capabilities::builtin()
        .dispatch(
            "__host_notify__",
            serde_json::json!({"title":"x","body":"y"}),
            &id,
            &Permissions::default(),
            &ctx,
        )
        .await;
    assert_eq!(r["ok"], false);
    assert!(r["error"]
        .as_str()
        .unwrap()
        .contains("未声明能力 system.notifications"));
}

#[test]
fn notify_launch_contributes_bridge_and_tool_only_when_declared() {
    let tmp = tempfile::tempdir().unwrap();
    let layout = DataLayout::new(tmp.path().to_path_buf());
    let mcp = McpManager::new();
    let sock = layout.mcp_socket_path("a");
    let ctx = super_agent_os::capability::LaunchCtx {
        app_id: "a",
        trusted: false,
        sandboxed: true,
        materialize: true,
        layout: &layout,
        hosttools_dir: Path::new("/ht"),
        socket_path: &sock,
        mcp: &mcp,
    };
    let mut p = Permissions::default();
    p.system.notifications = true;
    let c = capabilities::builtin()
        .launch(&p, &CallerIdentity::installing("a", false), &ctx)
        .unwrap();
    assert!(c.bridges.contains(&"notify_bridge.ts"));
    assert!(c.tools.contains(&"__host_notify__".to_string()));
    let none = capabilities::builtin()
        .launch(
            &Permissions::default(),
            &CallerIdentity::installing("a", false),
            &ctx,
        )
        .unwrap();
    assert!(!none.bridges.contains(&"notify_bridge.ts"));
}
