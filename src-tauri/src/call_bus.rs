//! P5 互联总线（call_agent）。
//!
//! 一个已装 subagent 的模型经宿主工具 `__host_call_agent__`（socket 分发，见
//! `mcp_socket.rs::process_request`）请求调用另一个已装 subagent。本模块是这条
//! 请求的宿主侧决策 + 执行核心：
//!
//! - **纯授权核心** `authorize_call`（本任务 T1）：不做任何 IO，把三道闸
//!   （深度 / 被调方是否已装 / 调用方白名单，router 豁免白名单）判成一个
//!   `CallDecision`，便于穷举门控单测。IO 版 `handle_call_agent`（T3）在其外围
//!   负责"读 registry / 读调用方权限 / 审计 / 拉起被调方会话"。
//!
//! **安全不变式**（母 spec §8）：
//! - 调用方身份 `caller_app_id` 与调用深度 `depth` 都来自 `McpSocketListener`
//!   绑定值（监听器创建时固定），**绝不**取自线上请求体——否则被注入的扩展可
//!   伪造一个低 `depth` 无限嵌套、或顶着别的 `app_id` 越权发起调用。
//! - 特权路由豁免（可调任意已装应用）**只**给内置 `superagent`
//!   （`crate::maker::MAKER_APP_ID`）**且仅在 `depth == 0`**（顶层会话，用户
//!   直接面对的主助手）；router 被其它应用嵌套调起、自己又发起下一层调用
//!   （`depth >= 1`）时不再豁免，与第三方同规则受限于自己清单声明的
//!   `agents.call` 列表（P6-C Task6）。第三方在任何深度都严格受限于该列表。
//! - 权限绝不随调用链放大：本模块只决定"能不能发起这次调用"；被调方运行时的
//!   约束（trusted / connectors / tools / sandbox）在 `session_mgr::spawn_call_session`
//!   里全部取被调方自己的记录，与调用方无关（见该函数）。

/// 嵌套调用最大深度（母 spec §8「默认限深 3 层防递归失控」）。顶端前台交互
/// 会话 depth=0，被它直接调起的被调方 depth=1，依此类推；depth 已达此值的会话
/// 不得再发起调用（见 `authorize_call` 深度闸）。
pub const MAX_CALL_DEPTH: u32 = 3;

/// 一次 agent 间调用的结果——原样序列化透传给调用方的 pi 扩展（`call_agent_bridge.ts`）。
/// 形状与 `session_mgr::TaskSessionResult` 呼应，但多一个 `ok`/`error`：总线**拒绝**
/// （权限/深度）与被调方**执行失败**都编码成 `{ok:false, error}`，`text` 为空。
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct CallResult {
    pub ok: bool,
    pub text: String,
    pub error: Option<String>,
}

impl CallResult {
    /// 便捷构造一个拒绝/失败结果。
    pub fn deny(error: impl Into<String>) -> Self {
        CallResult {
            ok: false,
            text: String::new(),
            error: Some(error.into()),
        }
    }
}

/// 纯授权决策（无 IO）。`verdict` 是落审计日志用的短标签（allow 分支不在此产生，
/// 由调用方在 `Allow` 时自行记 `"allow"`）。
#[derive(Debug, PartialEq, Eq)]
pub enum CallDecision {
    Allow,
    Deny {
        verdict: &'static str,
        error: String,
    },
}

/// 三道闸的纯判定。参数已由 IO 层解析好：
/// - `caller_is_router`：`caller_app_id == crate::maker::MAKER_APP_ID`。
/// - `depth`：调用方会话（监听器绑定）的当前深度。
/// - `want`：被调方目标经 `normalize_app_id` 归一后的 app_id。
/// - `target_display`：目标的原始写法（错误信息里回显给用户/模型看，人话）。
/// - `callee_installed`：registry 里是否解析得到 `want` 对应的已装应用。
/// - `caller_call_list`：调用方清单 `agents.call` 原始条目（内部归一后与 `want` 比对）。
///
/// 判定顺序刻意固定（深度 → 已装 → 白名单）：深度闸最前，任何情况下 depth 到顶都
/// 不再产生下游副作用（IO 层据此在 spawn 之前就短路）。
pub fn authorize_call(
    caller_is_router: bool,
    depth: u32,
    want: &str,
    target_display: &str,
    callee_installed: bool,
    caller_call_list: &[String],
) -> CallDecision {
    // 1. 深度闸——用 `depth >= MAX` 而非 `depth + 1 > MAX`，语义等价且无整型溢出风险。
    if depth >= MAX_CALL_DEPTH {
        return CallDecision::Deny {
            verdict: "depth-exceeded",
            error: format!("调用链已达最大深度 {MAX_CALL_DEPTH}，拒绝继续嵌套"),
        };
    }
    // 2. 被调方必须已安装。
    if !callee_installed {
        return CallDecision::Deny {
            verdict: "not-found",
            error: format!("未找到应用 {target_display}（未安装）"),
        };
    }
    // 3. 白名单闸——router（内置 superagent）**只在顶层会话**（depth == 0）豁免，
    //    可调任意已装应用；被别的已装应用调用起来、自己又发起下一层调用的 router
    //    （depth >= 1）不再天然豁免——它此刻是"被调用的嵌套会话"，不是用户直接
    //    面对的主助手，必须像第三方一样落到自己清单 agents.call 白名单（P6-C
    //    Task6，修 HANDOFF「P6-A 终审残留」第 1 条：豁免未随深度收紧）。第三方
    //    在任何深度都必须在清单声明过该目标（按 normalize_app_id 归一后比对，
    //    容忍 `@scope/name` 与 app_id 两种写法）。
    if !(caller_is_router && depth == 0) {
        let permitted = caller_call_list
            .iter()
            .any(|n| crate::pkg::normalize_app_id(n) == want);
        if !permitted {
            return CallDecision::Deny {
                verdict: "not-permitted",
                error: format!("未声明调用 {target_display} 的权限"),
            };
        }
    }
    CallDecision::Allow
}

/// 读调用方清单声明的 `agents.call` 列表（IO）。读失败一律返回空列表——非 router
/// 的空列表会让白名单闸拒绝（fail-closed）；router 根本不看这个列表，读失败无影响。
/// 绝不 panic（socket 处理路径上的任何 panic 都不可接受）。
fn load_caller_call_list(layout: &crate::paths::DataLayout, caller_app_id: &str) -> Vec<String> {
    let pkg_dir = layout.packages_dir(caller_app_id);
    let manifest = match crate::pkg::load_and_validate(&pkg_dir) {
        Ok(m) => m,
        Err(_) => return Vec::new(),
    };
    match crate::permissions::load(&pkg_dir, &manifest.superagent.permissions) {
        Ok(p) => p.agents.call,
        Err(_) => Vec::new(),
    }
}

/// `__host_call_agent__` 的 IO 版（socket 分发调用，见 `mcp_socket::process_request`）。
/// 解析 target/prompt → registry 解析被调方 → 加载调用方 `agents.call` → `authorize_call`
/// → 审计 → Allow 则 `session_mgr::spawn_call_session(depth+1)`。返回 `CallResult`
/// 序列化后的 `serde_json::Value`（原样透传给调用方扩展，同 maker 分支不套 encode_result）。
///
/// `caller_app_id`/`depth` 均来自监听器绑定值（`mcp_socket` 不信任 wire）。任何一步
/// 失败都返回 `{ok:false, error}` 且落对应 verdict 审计，绝不 panic。
pub async fn handle_call_agent(
    caller_app_id: &str,
    params: &serde_json::Value,
    layout: &crate::paths::DataLayout,
    manager: &crate::mcp::McpManager,
    hosttools_dir: &std::path::Path,
    depth: u32,
) -> serde_json::Value {
    let value = |r: CallResult| serde_json::to_value(r).unwrap_or(serde_json::Value::Null);

    let target = match params.get("target").and_then(|v| v.as_str()) {
        Some(t) if !t.is_empty() => t,
        _ => return value(CallResult::deny("缺少调用目标 target")),
    };
    let prompt = params.get("prompt").and_then(|v| v.as_str()).unwrap_or("");

    let want = crate::pkg::normalize_app_id(target);
    let reg = crate::registry::RegistryStore::new(layout.registry_path());
    let callee = reg.get(&want);

    let caller_is_router = caller_app_id == crate::maker::MAKER_APP_ID;
    let caller_call_list = load_caller_call_list(layout, caller_app_id);

    let decision = authorize_call(
        caller_is_router,
        depth,
        &want,
        target,
        callee.is_some(),
        &caller_call_list,
    );

    let audit_args = format!("target={target}");
    match decision {
        CallDecision::Deny { verdict, error } => {
            let _ = crate::audit::record(
                layout,
                caller_app_id,
                "__host_call_agent__",
                &audit_args,
                verdict,
            );
            value(CallResult::deny(error))
        }
        CallDecision::Allow => {
            let _ = crate::audit::record(
                layout,
                caller_app_id,
                "__host_call_agent__",
                &audit_args,
                "allow",
            );
            // callee.is_some() 已由 authorize_call 的 not-found 闸保证（Allow 分支必已装）。
            let callee = match callee {
                Some(c) => c,
                None => return value(CallResult::deny("被调应用记录丢失")),
            };
            match crate::session_mgr::spawn_call_session(
                layout,
                hosttools_dir,
                manager,
                &callee,
                prompt,
                depth + 1,
            )
            .await
            {
                Ok(r) => value(r),
                Err(e) => value(CallResult::deny(format!("被调应用拉起失败：{e}"))),
            }
        }
    }
}

/// `__host_list_agents__` 的处理（socket 分发，**仅** router=MAKER_APP_ID 可达——门控在
/// `capabilities::router::RouterCapability::declared`（`id.is_router()`），由
/// `capability::CapabilityRegistry::dispatch` 统一执行，非 router 请求根本不会
/// 调到本函数，见 `mcp_socket.rs` 模块文档"app 身份/权限绝不来自线上请求"）。
/// 返回已装应用目录，供主助手据用户意图选谁来调。只读 registry，不含任何权限/路径细节。
pub fn handle_list_agents(layout: &crate::paths::DataLayout) -> serde_json::Value {
    let reg = crate::registry::RegistryStore::new(layout.registry_path());
    let agents: Vec<serde_json::Value> = reg
        .load()
        .into_iter()
        .map(|a| {
            serde_json::json!({
                "app_id": a.app_id,
                "name": a.name,
                "display_name": a.display_name,
                "category": a.category,
            })
        })
        .collect();
    serde_json::json!({ "ok": true, "agents": agents })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn list(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn depth_gate_denies_at_max_regardless_of_other_inputs() {
        // depth 到顶：即便被调方未装、列表为空，也先被深度闸拦下（顺序保证）。
        let d = authorize_call(false, MAX_CALL_DEPTH, "x", "x", false, &[]);
        assert_eq!(
            d,
            CallDecision::Deny {
                verdict: "depth-exceeded",
                error: format!("调用链已达最大深度 {MAX_CALL_DEPTH}，拒绝继续嵌套"),
            }
        );
        // router 也不能突破深度（深度是资源闸，非权限）。
        assert!(matches!(
            authorize_call(true, MAX_CALL_DEPTH, "x", "x", true, &[]),
            CallDecision::Deny {
                verdict: "depth-exceeded",
                ..
            }
        ));
    }

    #[test]
    fn deepest_allowed_depth_still_passes() {
        // depth = MAX-1 的会话仍可发起（被调方将落在 depth = MAX，是最深的合法层）。
        assert_eq!(
            authorize_call(
                false,
                MAX_CALL_DEPTH - 1,
                "superagent__summarizer",
                "@superagent/summarizer",
                true,
                &list(&["@superagent/summarizer"]),
            ),
            CallDecision::Allow
        );
    }

    #[test]
    fn not_installed_callee_denied() {
        let d = authorize_call(
            false,
            0,
            "superagent__summarizer",
            "@superagent/summarizer",
            false,
            &list(&["@superagent/summarizer"]),
        );
        assert!(matches!(
            d,
            CallDecision::Deny {
                verdict: "not-found",
                ..
            }
        ));
    }

    #[test]
    fn third_party_without_declaration_denied() {
        let d = authorize_call(
            false,
            0,
            "superagent__other",
            "@superagent/other",
            true,
            &list(&["@superagent/summarizer"]),
        );
        assert!(matches!(
            d,
            CallDecision::Deny {
                verdict: "not-permitted",
                ..
            }
        ));
    }

    #[test]
    fn third_party_with_declaration_allowed_via_normalize_match() {
        // 清单写 `@superagent/summarizer`，want 是它 normalize 后的 app_id，须匹配。
        assert_eq!(
            crate::pkg::normalize_app_id("@superagent/summarizer"),
            "superagent__summarizer"
        );
        assert_eq!(
            authorize_call(
                false,
                0,
                "superagent__summarizer",
                "@superagent/summarizer",
                true,
                &list(&["@superagent/summarizer"]),
            ),
            CallDecision::Allow
        );
    }

    #[test]
    fn router_is_exempt_from_allow_list() {
        // 内置 superagent（router）列表为空也能调任意已装应用。
        assert_eq!(
            authorize_call(true, 0, "superagent__anything", "anything", true, &[]),
            CallDecision::Allow
        );
    }

    /// P6-C Task6（spec §4 步骤 6「路由深度」/ §6 不变量「深度闸」/ HANDOFF
    /// 「P6-A 终审残留」第 1 条）：router 的白名单豁免**只**在顶层会话
    /// （`depth == 0`）生效。router 被其它已装应用嵌套调起、自己又发起下一层
    /// 调用（`depth >= 1`）时，不再天然豁免——必须像第三方一样落到清单
    /// `agents.call` 白名单闸，未声明的目标一律 `Deny{verdict:"not-permitted"}`；
    /// 这堵住了"诱导主助手在被调用的嵌套会话里越权调用任意已装应用"这条路径。
    #[test]
    fn router_exemption_only_at_depth_zero() {
        // depth 0：router 豁免白名单，清单为空也放行。
        assert_eq!(
            authorize_call(true, 0, "superagent__anything", "anything", true, &[]),
            CallDecision::Allow
        );
        // depth 1：router 不再豁免，未在（空）清单里的目标须被拒。
        assert_eq!(
            authorize_call(true, 1, "superagent__anything", "anything", true, &[]),
            CallDecision::Deny {
                verdict: "not-permitted",
                error: "未声明调用 anything 的权限".to_string(),
            }
        );
        // depth 1 且清单声明了该目标：与第三方同规则，放行。
        assert_eq!(
            authorize_call(
                true,
                1,
                "superagent__summarizer",
                "@superagent/summarizer",
                true,
                &list(&["@superagent/summarizer"]),
            ),
            CallDecision::Allow
        );
    }

    #[test]
    fn call_result_serializes_with_ok_text_error_keys() {
        let v = serde_json::to_value(CallResult::deny("拒绝原因")).unwrap();
        assert_eq!(v["ok"], serde_json::json!(false));
        assert_eq!(v["text"], serde_json::json!(""));
        assert_eq!(v["error"], serde_json::json!("拒绝原因"));
    }
}
