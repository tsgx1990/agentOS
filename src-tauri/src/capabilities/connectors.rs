//! connectors 能力：per-app MCP 授权面（P3）。启动期注入 mcp_bridge + 两个 env + 授权工具名；
//! 调用期处理 `__host_mcp_call__`（宿主二次复核在 McpManager::host_mcp_call 内，字节级不变）。
use crate::capability::*;
use crate::mcp::McpCallResult;
use crate::paths::DataLayout;
use crate::permissions::{Access, Permissions};
use serde_json::Value;

pub struct ConnectorsCapability;
pub const MCP_TOOLS_ENV: &str = "SUPERAGENT_MCP_TOOLS";
pub const MCP_CALL_METHOD: &str = "__host_mcp_call__";

fn access_zh(a: &Access) -> &'static str {
    match a {
        Access::Read => "只读",
        Access::ReadWrite => "读写，写操作需你确认",
    }
}

/// F2（review）：`server`/`tool` 都是第三方 MCP server 自己 `tools/list` 上报的
/// 值，字节级不受宿主控制——一个恶意/写错的 server 报一个名叫 `x,bash` 的工具，
/// 这个名字会原样拼进 `mcp__x,bash__…`，再经 `assemble_launch_plan` 的
/// `tools.join(",")` 把 `bash` 注入进 pi 的 `--tools` 白名单，重开非 macOS 上
/// 已被 P1 `extensions:[]` + SAFE_TOOLS 白名单堵死的那个数据外泄口子。
/// 白名单字符集：`[A-Za-z0-9_.-]` 且非空——`--tools` 是逗号分隔，任何逗号/空白
/// /控制字符都足以打破这个契约，不止逗号本身。
pub(crate) fn is_safe_name(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
}

#[async_trait::async_trait]
impl Capability for ConnectorsCapability {
    fn key(&self) -> &'static str {
        "connectors"
    }
    fn declared(&self, p: &Permissions, _i: &CallerIdentity) -> bool {
        !p.connectors.is_empty()
    }
    fn render_human(&self, p: &Permissions) -> Vec<String> {
        p.connectors
            .iter()
            .map(|c| {
                let name = match c.category.as_str() {
                    "filesystem" => "文件系统".to_string(),
                    other => other.to_string(),
                };
                format!("使用{name}连接器（{}）", access_zh(&c.access))
            })
            .collect()
    }
    fn launch(&self, p: &Permissions, ctx: &LaunchCtx<'_>) -> Result<LaunchContribution, String> {
        let mut authed = ctx.mcp.authorized_tools(&p.connectors);
        // F2（review）：`server`/`tool` 是第三方 server 自报的、宿主不受控的字符串——
        // 校验通过的条目才允许进入 tools_json/tools（两者都是从这份过滤后的
        // `authed` 派生），被丢弃的条目**同时**从 `SUPERAGENT_MCP_TOOLS` env 与
        // `--tools` 白名单里消失，不是只挡其中一处。见 `is_safe_name` 文档。
        authed.retain(|t| {
            let safe = is_safe_name(&t.server) && is_safe_name(&t.tool);
            if !safe {
                eprintln!(
                    "connectors 能力：丢弃不安全的 MCP 工具名 server={:?} tool={:?}（含非 [A-Za-z0-9_.-] 字符，拒绝注入 --tools 白名单）",
                    t.server, t.tool
                );
            }
            safe
        });
        if authed.is_empty() {
            return Ok(LaunchContribution::default());
        }
        // ↓ 从 session_mgr::mcp_launch_extras 原样搬来的 JSON 构造（字段名/顺序不改）
        let tools_json: Vec<Value> = authed
            .iter()
            .map(|t| {
                serde_json::json!({
                    "server": t.server, "tool": t.tool, "name": t.tool,
                    "description": t.description, "inputSchema": t.input_schema,
                })
            })
            .collect();
        Ok(LaunchContribution {
            env: vec![
                (
                    MCP_TOOLS_ENV.to_string(),
                    serde_json::to_string(&tools_json).unwrap(),
                ),
                (
                    super::SOCKET_ENV.to_string(),
                    ctx.socket_path.to_string_lossy().to_string(),
                ),
            ],
            bridges: vec!["mcp_bridge.ts"],
            tools: authed
                .iter()
                .map(|t| format!("mcp__{}__{}", t.server, t.tool))
                .collect(),
            needs_socket: true,
            ..Default::default()
        })
    }
    fn methods(&self) -> &'static [&'static str] {
        &[MCP_CALL_METHOD]
    }
    async fn handle(
        &self,
        _m: &str,
        params: Value,
        id: &CallerIdentity,
        perms: &Permissions,
        ctx: &CallCtx<'_>,
    ) -> Value {
        let Some(server) = params.get("server").and_then(|v| v.as_str()) else {
            return serde_json::json!({ "error": "请求缺少 params.server 字段" });
        };
        let Some(tool) = params.get("tool").and_then(|v| v.as_str()) else {
            return serde_json::json!({ "error": "请求缺少 params.tool 字段" });
        };
        let args = params.get("args").cloned().unwrap_or(Value::Null);
        let result = ctx
            .mcp
            .host_mcp_call(
                &id.app_id,
                &perms.connectors,
                server,
                tool,
                args,
                ctx.layout,
            )
            .await;
        if let McpCallResult::PendingConfirm(ref confirm_id) = result {
            let store =
                crate::notifications::NotificationStore::new(ctx.layout.clone(), ctx.mcp.clone());
            let _ = store.record_pending_confirm(confirm_id, &id.app_id, server, tool);
        }
        crate::mcp_socket::encode_result(result)
    }
    /// P6-C Task5（spec §4 步骤 5「卸载即清」）：卸载应用时清空它名下的放行
    /// 规则与暂存调用——`ApprovalStore` 是持久化的，不会随进程重启自愈，
    /// 卸载后残留的规则会让"重装同名 app_id 后自动免审"这种越权路径成立，
    /// 残留的暂存调用则会指向一个再也不会来验收它的会话。`remove_all_for_app`
    /// 本身就是幂等的（该 app 没有任何记录时返回 `(0, [])`），`CapabilityRegistry
    /// ::on_uninstall` 又对全部能力无条件调用（不看 `declared`），两者叠加
    /// 保证这个钩子对"从未声明过 connectors 权限的 app"和"重复调用"都是
    /// 干净的 no-op（见 `capabilities_connectors_it.rs::
    /// uninstall_hook_clears_rules_and_staged`）。被丢弃的暂存调用逐条审计
    /// `rejected`（同 `notifications.rs::respond_staged` 拒绝分支的 verdict），
    /// 审计写失败不影响清理本身（`audit::record` best-effort 哲学）。
    ///
    /// **终审 Important 3**：此前这里只审计、不通知——被丢弃的 `confirm_request`
    /// 通知永远停在"待处理"（挂到 30 天保留期自然过期，界面上是个点不动的死
    /// 条目），会话侧也从未被告知"你等的结果不会来了"，与 `expire_staged`
    /// 的到期路径不对称（那条路径会 ack + 落 `update`）。现在改叫共用逻辑
    /// `notifications::ack_and_notify_dropped_staged`：ack 掉原 `confirm_request`
    /// + 落一条 `update` 说明"应用已卸载/升级，暂存调用已自动拒绝"（本钩子的
    /// trait 签名只有 `(app_id, layout)`，没有 `AppState`/`deliver` 回调可用，
    /// 因此做不到把拒绝消息 steer 回仍然活着的会话——那句"回送一句拒绝"留在
    /// HANDOFF 已知边界，不在本轮范围内假装做到）。规则被清空（`removed_rules
    /// > 0`）时额外落一条汇总 `update`，因为"以后自动放行"是用户曾经明确
    /// 勾选过的持久授权，悄悄清空却不说一声等于让用户对自己给过的授权状态
    /// 一无所知。
    fn on_uninstall(&self, app_id: &str, layout: &DataLayout) -> Result<(), String> {
        let store = crate::approvals::ApprovalStore::new(layout.clone());
        let (removed_rules, dropped) = store.remove_all_for_app(app_id)?;
        for staged in &dropped {
            let _ = crate::audit::record(
                layout,
                app_id,
                &staged.tool,
                &staged.args.to_string(),
                "rejected",
            );
        }
        crate::notifications::ack_and_notify_dropped_staged(
            layout,
            &dropped,
            "已自动拒绝",
            "因所属应用已卸载/升级而自动拒绝。",
        );
        if removed_rules > 0 {
            let _ = crate::notifications::notify_update(
                layout,
                app_id,
                "自动放行规则已清除",
                &format!("「{app_id}」已卸载/升级，已清除 {removed_rules} 条自动放行规则。"),
            );
        }
        Ok(())
    }
    fn enforcement(&self) -> &'static [Enforcement] {
        &[Enforcement::Launch, Enforcement::HostMethod]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_safe_name_accepts_typical_tool_and_server_ids() {
        for ok in ["read_file", "fs-1", "a.b", "ReadFile123", "a_b-c.d"] {
            assert!(is_safe_name(ok), "{ok:?} 应被接受");
        }
    }

    #[test]
    fn is_safe_name_rejects_empty_comma_space_and_non_ascii() {
        for bad in ["x,bash", "a b", "", "名"] {
            assert!(!is_safe_name(bad), "{bad:?} 应被拒");
        }
    }
}
