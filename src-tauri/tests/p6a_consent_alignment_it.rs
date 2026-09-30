//! 2026-08-10 缺陷回归：connector-demo 申请文件系统读写连接器，同意框不得显示「无额外权限」。
use std::path::Path;
use super_agent_os::capabilities;
use super_agent_os::capability::{CallerIdentity, NO_EXTRA_PERMISSIONS};
use super_agent_os::permissions;
use super_agent_os::pkg;

fn human_for(sample: &str) -> Vec<String> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("samples")
        .join(sample);
    let m = pkg::load_and_validate(&dir).unwrap();
    let p = permissions::load(&dir, &m.superagent.permissions).unwrap();
    capabilities::builtin().render_human(&p, &CallerIdentity::installing(&m.app_id(), false))
}

#[test]
fn connector_demo_shows_connector_not_fallback() {
    let lines = human_for("connector-demo");
    assert!(
        lines
            .iter()
            .any(|l| l.contains("文件系统连接器") && l.contains("读写")),
        "{lines:?}"
    );
    assert!(!lines.iter().any(|l| l == NO_EXTRA_PERMISSIONS));
}

#[test]
fn daily_brief_shows_connector_schedule_and_notifications() {
    let lines = human_for("daily-brief");
    assert!(
        lines
            .iter()
            .any(|l| l.contains("连接器") && l.contains("只读")),
        "{lines:?}"
    );
    assert!(
        lines
            .iter()
            .any(|l| l.contains("定时任务") && l.contains("morning-brief")),
        "{lines:?}"
    );
    assert!(lines.iter().any(|l| l.contains("通知")), "{lines:?}");
    assert!(
        !lines.iter().any(|l| l.contains("$APP_DATA")),
        "$APP_DATA 是隐含的，不呈现"
    );
}

#[test]
fn summarizer_with_empty_permissions_shows_fallback_only() {
    assert_eq!(
        human_for("summarizer"),
        vec![NO_EXTRA_PERMISSIONS.to_string()]
    );
}
