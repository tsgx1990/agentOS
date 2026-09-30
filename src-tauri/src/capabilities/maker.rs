//! 特权能力：Maker 四工具（仅主助手 superagent）。应用安装仍必经
//! `maker::resolve_install`；技能安装（Task7，P6-B）走同一 seam 的技能版
//! `maker::resolve_install_skill`——`__host_maker_install_skill__` 只是新增
//! 一个数组元素，`declared`/`dispatch` 的门控逻辑不需要改一行（见
//! `capability.rs::CapabilityRegistry::dispatch`：找到认领 `method` 的能力后
//! 才检查 `declared`，新方法只要出现在 `methods()` 里就自动继承同一门控）。
use crate::capability::*;
use crate::permissions::Permissions;
use serde_json::Value;

pub struct MakerCapability;
pub const MAKER_METHODS: [&str; 4] = [
    "__host_maker_stage_write__",
    "__host_maker_preview__",
    "__host_maker_install__",
    "__host_maker_install_skill__",
];

#[async_trait::async_trait]
impl Capability for MakerCapability {
    fn key(&self) -> &'static str {
        "maker"
    }
    /// F3（review）：只在深度 0（前台/task-mode 那一层会话）声明——一个拿到
    /// `agents.call: ["superagent"]` 的第三方应用可以把调用链引到 router
    /// （`app_id == MAKER_APP_ID`）、深度 ≥1 的一次 headless call 会话；`app_id`
    /// 相同不代表那是主助手本人在跑，maker 的三个特权工具（stage_write/
    /// preview/install：能往磁盘写包、能安装应用）绝不能在被别人调用出来的那层
    /// 会话里被激活。
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
            bridges: vec!["maker_bridge.ts"],
            tools: MAKER_METHODS.iter().map(|s| s.to_string()).collect(),
            needs_socket: true,
            ..Default::default()
        })
    }
    fn methods(&self) -> &'static [&'static str] {
        &MAKER_METHODS
    }
    async fn handle(
        &self,
        method: &str,
        params: Value,
        id: &CallerIdentity,
        _p: &Permissions,
        ctx: &CallCtx<'_>,
    ) -> Value {
        crate::maker::handle_maker_request(&id.app_id, method, params, ctx.layout, ctx.mcp).await
    }
    fn enforcement(&self) -> &'static [Enforcement] {
        &[Enforcement::Launch, Enforcement::HostMethod]
    }
    fn privileged(&self) -> bool {
        true
    }
}
