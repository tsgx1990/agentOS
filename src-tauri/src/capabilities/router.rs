//! 特权能力：意图路由列举（仅主助手 superagent）。
use crate::capability::*;
use crate::permissions::Permissions;
use serde_json::Value;

pub struct RouterCapability;
pub const LIST_AGENTS_METHOD: &str = "__host_list_agents__";

#[async_trait::async_trait]
impl Capability for RouterCapability {
    fn key(&self) -> &'static str {
        "router"
    }
    /// F3（review）：与 `maker::MakerCapability::declared` 同一理由——只在深度 0
    /// 声明，绝不在被别人调出来的深度≥1 会话里把 list_agents 这个特权工具面
    /// 激活（见该函数文档）。
    fn declared(&self, _p: &Permissions, id: &CallerIdentity) -> bool {
        id.is_router() && id.depth == 0
    }
    fn render_human(&self, _p: &Permissions) -> Vec<String> {
        vec![]
    }
    fn launch(&self, _p: &Permissions, ctx: &LaunchCtx<'_>) -> Result<LaunchContribution, String> {
        Ok(LaunchContribution {
            env: vec![(
                super::SOCKET_ENV.to_string(),
                ctx.socket_path.to_string_lossy().to_string(),
            )],
            bridges: vec!["router_bridge.ts"],
            tools: vec![LIST_AGENTS_METHOD.to_string()],
            needs_socket: true,
            ..Default::default()
        })
    }
    fn methods(&self) -> &'static [&'static str] {
        &[LIST_AGENTS_METHOD]
    }
    async fn handle(
        &self,
        _m: &str,
        _params: Value,
        _id: &CallerIdentity,
        _p: &Permissions,
        ctx: &CallCtx<'_>,
    ) -> Value {
        crate::call_bus::handle_list_agents(ctx.layout)
    }
    fn enforcement(&self) -> &'static [Enforcement] {
        &[Enforcement::Launch, Enforcement::HostMethod]
    }
    fn privileged(&self) -> bool {
        true
    }
}
