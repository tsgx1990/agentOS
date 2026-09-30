//! ui.connectSrc 能力：只约束预制界面 WebView 的 CSP connect-src（install.rs 写 registry.domains → scheme.rs）。
use crate::capability::*;
use crate::permissions::Permissions;
pub struct UiConnectCapability;
#[async_trait::async_trait]
impl Capability for UiConnectCapability {
    fn key(&self) -> &'static str {
        "ui.connectSrc"
    }
    fn declared(&self, p: &Permissions, _i: &CallerIdentity) -> bool {
        !p.ui.connect_src.is_empty()
    }
    fn render_human(&self, p: &Permissions) -> Vec<String> {
        vec![format!("界面可访问的网址：{}", p.ui.connect_src.join("、"))]
    }
    fn launch(&self, _p: &Permissions, _c: &LaunchCtx<'_>) -> Result<LaunchContribution, String> {
        Ok(LaunchContribution::default())
    }
    fn enforcement(&self) -> &'static [Enforcement] {
        &[Enforcement::UiCsp]
    }
}
