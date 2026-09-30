//! system.schedule + scheduledTasks 能力（P3 调度器）。执行点：安装钩子（登记/摘除任务）。
//! `open_app` 里的注册-on-open（session_mgr）保留，语义不变。
use crate::capability::*;
use crate::paths::DataLayout;
use crate::permissions::Permissions;
use crate::registry::InstalledApp;

pub struct ScheduleCapability;

#[async_trait::async_trait]
impl Capability for ScheduleCapability {
    fn key(&self) -> &'static str {
        "system.schedule"
    }
    fn declared(&self, p: &Permissions, _i: &CallerIdentity) -> bool {
        p.system.schedule
    }
    fn render_human(&self, p: &Permissions) -> Vec<String> {
        if p.scheduled_tasks.is_empty() {
            return vec!["创建定时任务".to_string()];
        }
        let items: Vec<String> = p
            .scheduled_tasks
            .iter()
            .map(|t| format!("{}（cron {}）", t.id, t.cron))
            .collect();
        vec![format!("创建定时任务：{}", items.join("、"))]
    }
    fn launch(&self, _p: &Permissions, _c: &LaunchCtx<'_>) -> Result<LaunchContribution, String> {
        Ok(LaunchContribution::default())
    }
    fn on_install(
        &self,
        app: &InstalledApp,
        p: &Permissions,
        layout: &DataLayout,
    ) -> Result<(), String> {
        crate::scheduler::register_scheduled_tasks_if_permitted(
            layout,
            &app.app_id,
            p.system.schedule,
            &p.scheduled_tasks,
        )
    }
    fn on_uninstall(&self, app_id: &str, layout: &DataLayout) -> Result<(), String> {
        crate::scheduler::TaskRegistry::new(layout).deregister_app(app_id)
    }
    fn enforcement(&self) -> &'static [Enforcement] {
        &[Enforcement::InstallHook]
    }
}
