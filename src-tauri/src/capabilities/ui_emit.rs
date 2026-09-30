//! 隐含能力：所有应用都拿到 `ui_emit.ts` 桥与 `__host_ui_emit__` 工具（P1 起如此），不呈现。
use crate::capability::*;
use crate::permissions::Permissions;

pub struct UiEmitCapability;
pub const UI_EMIT_TOOL: &str = "__host_ui_emit__";

#[async_trait::async_trait]
impl Capability for UiEmitCapability {
    fn key(&self) -> &'static str {
        "ui_emit"
    }
    fn declared(&self, _p: &Permissions, _i: &CallerIdentity) -> bool {
        true
    }
    fn render_human(&self, _p: &Permissions) -> Vec<String> {
        vec![]
    }
    fn launch(&self, _p: &Permissions, _c: &LaunchCtx<'_>) -> Result<LaunchContribution, String> {
        Ok(LaunchContribution {
            bridges: vec!["ui_emit.ts"],
            tools: vec![UI_EMIT_TOOL.to_string()],
            ..Default::default()
        })
    }
    fn enforcement(&self) -> &'static [Enforcement] {
        &[Enforcement::Launch]
    }
    fn privileged(&self) -> bool {
        true
    } // 不参与呈现与字段覆盖检查
}
