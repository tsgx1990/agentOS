//! agents.call 能力（P5 互联总线）：声明了非空 `agents.call` 的应用，或 router（主助手，白名单豁免）。
use crate::capability::*;
use crate::permissions::Permissions;
use serde_json::Value;

pub struct AgentsCallCapability;
pub const CALL_AGENT_METHOD: &str = "__host_call_agent__";

#[async_trait::async_trait]
impl Capability for AgentsCallCapability {
    fn key(&self) -> &'static str {
        "agents.call"
    }
    fn declared(&self, p: &Permissions, id: &CallerIdentity) -> bool {
        !p.agents.call.is_empty() || id.is_router()
    }
    fn render_human(&self, p: &Permissions) -> Vec<String> {
        if p.agents.call.is_empty() {
            return vec![];
        }
        vec![format!("调用其他应用：{}", p.agents.call.join("、"))]
    }
    fn launch(&self, _p: &Permissions, ctx: &LaunchCtx<'_>) -> Result<LaunchContribution, String> {
        Ok(LaunchContribution {
            env: vec![(
                super::SOCKET_ENV.to_string(),
                ctx.socket_path.to_string_lossy().to_string(),
            )],
            bridges: vec!["call_agent_bridge.ts"],
            tools: vec![CALL_AGENT_METHOD.to_string()],
            needs_socket: true,
            ..Default::default()
        })
    }
    fn methods(&self) -> &'static [&'static str] {
        &[CALL_AGENT_METHOD]
    }
    async fn handle(
        &self,
        _m: &str,
        params: Value,
        id: &CallerIdentity,
        _p: &Permissions,
        ctx: &CallCtx<'_>,
    ) -> Value {
        match ctx.hosttools_dir {
            Some(h) => {
                crate::call_bus::handle_call_agent(
                    &id.app_id, &params, ctx.layout, ctx.mcp, h, id.depth,
                )
                .await
            }
            None => {
                serde_json::json!({ "ok": false, "text": "", "error": "call bus 未在此监听器启用（缺少 hosttools 上下文）" })
            }
        }
    }
    fn enforcement(&self) -> &'static [Enforcement] {
        &[Enforcement::Launch, Enforcement::HostMethod]
    }
}
