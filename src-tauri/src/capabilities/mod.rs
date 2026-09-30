//! 内置能力显式列表（spec §3）。新增一类能力 = 新文件 + 在 builtin() 里加一行；
//! `tests/capability_invariants_it.rs` 会检查它「声明必呈现、声明必执行」。
pub mod agents_call;
pub mod connectors;
pub mod filesystem;
pub mod maker;
pub mod notifications;
pub mod router;
pub mod schedule;
pub mod skills;
pub mod ui_connect;
pub mod ui_emit;

use crate::capability::CapabilityRegistry;
pub const SOCKET_ENV: &str = "SUPERAGENT_MCP_SOCKET";

pub fn builtin() -> CapabilityRegistry {
    CapabilityRegistry::new(vec![
        Box::new(ui_emit::UiEmitCapability),
        Box::new(connectors::ConnectorsCapability),
        Box::new(agents_call::AgentsCallCapability),
        Box::new(router::RouterCapability),
        Box::new(maker::MakerCapability),
        Box::new(schedule::ScheduleCapability),
        Box::new(notifications::NotificationsCapability::new()),
        Box::new(filesystem::FilesystemCapability),
        Box::new(ui_connect::UiConnectCapability),
        Box::new(skills::SkillsCapability),
    ])
}
