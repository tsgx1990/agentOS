pub mod app_state;
pub mod approvals;
pub mod audit;
pub mod byok;
pub mod call_bus;
pub mod capabilities;
pub mod capability;
pub mod dirfd;
pub mod idle;
pub mod install;
pub mod jsonl;
pub mod maker;
pub mod market;
pub mod mcp;
pub mod mcp_socket;
pub mod model_overrides;
pub mod notifications;
pub mod paths;
pub mod permissions;
pub mod pi_bin;
pub mod pkg;
pub mod probe;
pub mod providers;
pub mod publish;
pub mod registry;
pub mod rpc;
pub mod sandbox;
pub mod scheduler;
pub mod scheme;
pub mod secrets;
pub mod session_mgr;
pub mod skills;
pub mod state_store;
pub mod supervisor;
pub mod usage;
pub mod vault;

use app_state::AppState;
use byok::{classify_error, frontend_payload};
use paths::DataLayout;
use rpc::{PiEvent, RpcSession};
use tauri::{http, Emitter, Manager, State};

#[tauri::command]
async fn send_prompt(text: String, state: State<'_, AppState>) -> Result<(), String> {
    let guard = state.main_session.lock().await;
    match guard.as_ref() {
        Some(session) => session.send_prompt(&text).await,
        None => Err("主助手会话未就绪".into()),
    }
}

#[tauri::command]
async fn open_app(app: tauri::AppHandle, app_id: String) -> Result<usize, String> {
    session_mgr::open_app(app, app_id).await
}

#[tauri::command]
async fn close_app(app: tauri::AppHandle, app_id: String) -> Result<(), String> {
    session_mgr::close_app(app, app_id).await
}

/// 把宿主桥（`window.superagent.prompt(text)`）转发进该应用自己的 pi 子进程会话。
#[tauri::command]
async fn app_prompt(app: tauri::AppHandle, app_id: String, text: String) -> Result<(), String> {
    let state = app.state::<AppState>();
    let guard = state.app_sessions.lock().await;
    let session = guard.get(&app_id).ok_or("应用会话未就绪")?;
    // P6-F：先 begin_turn 再 send_prompt——pi 可能在 send_prompt 返回前就回 agent_end，
    // 后置会让 in_turn 永久卡在 true。发送失败则回滚。
    let now = idle::now_secs();
    state.activity.begin_turn(&app_id, now);
    let res = session.send_prompt(&text).await;
    if res.is_err() {
        state.activity.end_turn(&app_id, idle::now_secs());
    }
    res
}

/// P1：把结构化指令（`window.superagent.command(name, params)`）格式化为一句
/// 提示词，复用 `app_prompt` 发给该应用会话——尚无独立的结构化指令通道。
#[tauri::command]
async fn app_command(
    app: tauri::AppHandle,
    app_id: String,
    name: String,
    params: serde_json::Value,
) -> Result<(), String> {
    let text = format!("执行指令「{name}」，参数：{params}");
    app_prompt(app, app_id, text).await
}

/// 读取该应用的持久化状态（`state/<app_id>.json` 中的单个 key）。
#[tauri::command]
fn app_state_get(
    app: tauri::AppHandle,
    app_id: String,
    key: String,
) -> Result<Option<serde_json::Value>, String> {
    let root = app.path().app_data_dir().map_err(|e| e.to_string())?;
    Ok(state_store::get(&DataLayout::new(root), &app_id, &key))
}

/// 写入该应用的持久化状态（原子 tmp+rename，见 `state_store::set`）。
#[tauri::command]
fn app_state_set(
    app: tauri::AppHandle,
    app_id: String,
    key: String,
    value: serde_json::Value,
) -> Result<(), String> {
    let root = app.path().app_data_dir().map_err(|e| e.to_string())?;
    state_store::set(&DataLayout::new(root), &app_id, &key, &value)
}

/// 读取已装应用注册表（`registry.json`），供前端 NavRail 做真实类目计数。
#[tauri::command]
fn list_apps(app: tauri::AppHandle) -> Result<Vec<registry::InstalledApp>, String> {
    let root = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let layout = DataLayout::new(root);
    Ok(registry::RegistryStore::new(layout.registry_path()).load())
}

/// 预览安装：校验源包 + 解析权限并转成人话，查已装版本——**不产生安装副作用**
/// （不复制文件、不写 registry），供 `InstallDialog` 在用户确认前展示。
#[derive(serde::Serialize)]
struct InstallPreview {
    display_name: String,
    category: String,
    permissions: Vec<String>,
    existing_version: Option<String>,
    capabilities: Vec<capability::CapabilityReport>,
    sandboxed: bool,
}

#[tauri::command]
fn preview_install(source_path: String, app: tauri::AppHandle) -> Result<InstallPreview, String> {
    let root = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let layout = DataLayout::new(root);
    let src = std::path::Path::new(&source_path);
    let m = pkg::load_and_validate(src).map_err(|e| e.to_string())?;
    let perms = permissions::load(src, &m.superagent.permissions)?;
    let reg = registry::RegistryStore::new(layout.registry_path());
    let app_id = m.app_id();
    let state = app.state::<AppState>();
    let hosttools = hosttools_dir(&app);
    let sock = layout.mcp_socket_path(&app_id);
    let sandboxed = session_mgr::sandboxing_available();
    // 预览时未知该源包是否会以 trusted 身份装（用户还没确认）；`trusted` 不影响
    // 非特权能力的 `declared()` 判定（见 `capability.rs`），按第三方/未知处理即可。
    let identity = crate::capability::CallerIdentity::installing(&app_id, false);
    let ctx = capability::LaunchCtx {
        app_id: &app_id,
        trusted: false,
        sandboxed,
        // F1（review）：预览是只读操作——即便 `describe()` 内部本已强制改成
        // `false`，这里也显式传 `false`（防御性 + 自文档化：任何直接读这段代码
        // 的人都能一眼看出预览绝不落地目录）。
        materialize: false,
        layout: &layout,
        hosttools_dir: &hosttools,
        socket_path: &sock,
        mcp: &state.mcp,
    };
    Ok(InstallPreview {
        display_name: m.superagent.display_name.clone(),
        category: m.superagent.category.clone(),
        permissions: crate::capabilities::builtin().render_human(&perms, &identity),
        existing_version: reg.get(&app_id).map(|a| a.version),
        capabilities: state.capabilities.describe(&perms, &identity, &ctx),
        sandboxed,
    })
}

/// 诊断面板：某个**已装**应用的完整能力报告（不止已声明的——含 `declared:
/// false` 的条目，供 `CapabilityPanel` 过滤/未来审计 UI 展示未声明项），供
/// `AppFrame` 的「权限」弹层在运行时回答"这个应用到底能做什么、由谁强制"。
/// 与 `preview_install` 的差别：这里的 `app_id`/`trusted`/`layout`/
/// `hosttools_dir` 都是该应用真实安装后的值（从 `registry` 读 `trusted`），
/// 而不是预览时的猜测。
#[tauri::command]
fn app_capabilities(
    app_id: String,
    app: tauri::AppHandle,
) -> Result<Vec<capability::CapabilityReport>, String> {
    let state = app.state::<AppState>();
    let root = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let layout = DataLayout::new(root);
    let reg = registry::RegistryStore::new(layout.registry_path());
    let record = reg
        .get(&app_id)
        .ok_or_else(|| format!("未知的 app_id：{app_id}"))?;
    let pkg_dir = layout.packages_dir(&app_id);
    let m = pkg::load_and_validate(&pkg_dir).map_err(|e| e.to_string())?;
    let perms = permissions::load(&pkg_dir, &m.superagent.permissions)?;
    let hosttools = hosttools_dir(&app);
    let sock = layout.mcp_socket_path(&app_id);
    let ctx = capability::LaunchCtx {
        app_id: &app_id,
        trusted: record.trusted,
        sandboxed: session_mgr::sandboxing_available(),
        // F1（review）：诊断面板同样是只读操作，显式传 `false`（`describe()`
        // 内部也会强制改成 `false`，见其文档）。
        materialize: false,
        layout: &layout,
        hosttools_dir: &hosttools,
        socket_path: &sock,
        mcp: &state.mcp,
    };
    Ok(state.capabilities.describe(
        &perms,
        &capability::CallerIdentity::installing(&app_id, record.trusted),
        &ctx,
    ))
}

/// 安装或升级应用（走 `install::install_or_upgrade`）。
#[tauri::command]
fn install_app(
    source_path: String,
    trusted: bool,
    app: tauri::AppHandle,
) -> Result<registry::InstalledApp, String> {
    let root = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let layout = DataLayout::new(root);
    let reg = registry::RegistryStore::new(layout.registry_path());
    install::install_or_upgrade(std::path::Path::new(&source_path), &layout, &reg, trusted)
}

/// 卸载应用（走 `install::uninstall_fs`）。
#[tauri::command]
async fn uninstall_app(
    app_id: String,
    keep_app_data: bool,
    app: tauri::AppHandle,
) -> Result<(), String> {
    // 卸载前先杀该 app 运行中的 pi 会话(若有)：uninstall_fs 会删 session_dir/packages_dir，
    // 若进程还开着这些目录会导致 in-flight 写丢失。close_app 会 kill + 从 app_sessions 摘除 +
    // 释放 slot/gate；若该 app 未打开则是无害 no-op。
    let _ = session_mgr::close_app(app.clone(), app_id.clone()).await;
    let root = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let layout = DataLayout::new(root);
    let reg = registry::RegistryStore::new(layout.registry_path());
    install::uninstall_fs(&app_id, &layout, &reg, keep_app_data)
}

/// 查询宿主级审计日志（`audit/<date>.jsonl`，见 `audit::query`），供未来的审计
/// UI（或调试）按 `app_id`/`tool`/`limit` 过滤查看各应用触发过的工具调用。
#[tauri::command]
fn list_audit(
    app: tauri::AppHandle,
    filter: audit::AuditFilter,
) -> Result<Vec<audit::Entry>, String> {
    let root = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let layout = DataLayout::new(root);
    Ok(audit::query(&layout, &filter))
}

/// Task15：按 filter 查询通知中心（`notifications/<date>.jsonl`，见
/// `notifications::NotificationStore::list`），供通知 UI 展示 task_result/
/// confirm_request/update 三类通知。
#[tauri::command]
fn list_notifications(
    app: tauri::AppHandle,
    filter: notifications::NotificationFilter,
) -> Result<Vec<notifications::Notification>, String> {
    let root = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let layout = DataLayout::new(root);
    let state = app.state::<AppState>();
    let store = notifications::NotificationStore::new(layout, state.mcp.clone());
    Ok(store.list(&filter))
}

/// Task15：把一条通知标记为已读（`notifications::NotificationStore::ack`）。
#[tauri::command]
fn ack_notification(app: tauri::AppHandle, id: String) -> Result<(), String> {
    let root = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let layout = DataLayout::new(root);
    let state = app.state::<AppState>();
    let store = notifications::NotificationStore::new(layout, state.mcp.clone());
    store.ack(&id)
}

/// Task15：响应一条 `confirm_request`（用户在通知中心点了"允许"/"拒绝"，可选
/// "总是允许"）——续行/丢弃登记的待确认 MCP 写操作。`allow=true` 时返回值
/// 是被续行执行的那次 `tools/call` 的结果（`Some`）；`allow=false` 或
/// `confirm_id` 未知/已被消费过则为 `None`。
///
/// **审查修复轮1 Important 2**：此前这里直接调
/// `NotificationStore::respond_confirm`，而那个方法内部传的是恒返回 `false`
/// 的 no-op `deliver`（`NotificationStore` 本身不持有 `AppState`，见其文档）——
/// 结果是生产路径上用户在通知中心点"允许"、工具确实执行了，但结果**从不**
/// 经 `steer` 回送进发起会话，且因为 `delivered` 恒为 `false`，每次都额外
/// 多落一条兜底 `update` 通知（即使会话就在眼前活着）。而 `mcp_socket.rs`
/// 的回执文案明确向模型承诺"批准后执行结果会以宿主消息送回本会话"，模型会
/// 照做地干等——这是 spec §4 流程3 在唯一生产可用入口上没兑现的缺口。
///
/// 这里改为跟 `respond_staged` 命令（见下）共用同一份真实 `deliver` 接线
/// （`session_mgr::steer_app_session` 查 `AppState::app_sessions`），单条 id
/// 走 `NotificationStore::respond_staged`，再用
/// `notifications::staged_outcome_to_confirm_result` 把 `StagedOutcome`
/// 压回本命令原有的三态返回——`NotificationStore::respond_confirm`（无
/// `deliver` 可用的薄封装）保留给不持有 `AppState` 的调用点（既有单元/集成
/// 测试、`create_confirm` 场景）继续用。
#[tauri::command]
async fn respond_confirm(
    app: tauri::AppHandle,
    confirm_id: String,
    allow: bool,
    always: bool,
) -> Result<Option<serde_json::Value>, String> {
    let root = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let layout = DataLayout::new(root);
    let state = app.state::<AppState>();
    let store = notifications::NotificationStore::new(layout, state.mcp.clone());

    let deliver: &(dyn Fn(&str, String) -> notifications::BoxFuture<'_, bool> + Sync) =
        &|app_id: &str, text: String| {
            let app_id = app_id.to_string();
            let app_handle = app.clone();
            Box::pin(async move {
                let state = app_handle.state::<AppState>();
                session_mgr::steer_app_session(state.inner(), &app_id, &text).await
            })
        };

    let mut outcomes = store
        .respond_staged(&[confirm_id], allow, always, deliver)
        .await?;
    let outcome = outcomes
        .pop()
        .expect("respond_staged 对单个 id 应恰好返回一条 StagedOutcome");
    notifications::staged_outcome_to_confirm_result(outcome)
}

/// P6-C Task4：列出当前暂存待批的写调用（供审批中心渲染），`app_id` 为
/// `Some` 时只看该应用的——见 `approvals::ApprovalStore::list_staged`。只读
/// 查询，不消费、不移除任何暂存项。
#[tauri::command]
fn list_staged_calls(
    app: tauri::AppHandle,
    app_id: Option<String>,
) -> Result<Vec<approvals::StagedCall>, String> {
    let root = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let layout = DataLayout::new(root);
    let store = approvals::ApprovalStore::new(layout);
    store.list_staged(app_id.as_deref())
}

/// P6-C Task4：批量验收——审批中心一次勾选多条待批调用后调这一个命令。逐条
/// 独立处理（部分成功是正常结果，不是异常），见
/// `notifications::NotificationStore::respond_staged` 文档。
///
/// `deliver` 回调在这里接线成真正的生产实现：`session_mgr::steer_app_session`
/// 查 `AppState::app_sessions`，会话仍活着就用 pi RPC `steer` 把回执文案送进
/// 去，返回是否投递成功——`respond_staged` 据此决定要不要落一条 `update`
/// 通知兜底。闭包按值捕获一份克隆的 `AppHandle`（Tauri 的 `AppHandle` 是廉价
/// `Clone`——内部本就是共享句柄，不是深拷贝整个 app），每次调用时在 `async
/// move` 块内部现查 `.state::<AppState>()`——不去尝试跨闭包借用外层的
/// `state`/`&AppState`（那样会撞上 `dyn Fn(..) -> BoxFuture<'_, ..>` 的 HRTB
/// 生命周期推导：返回的 future 需要对"每一次调用各自的短生命周期"都成立，
/// 借用外层变量做不到这一点，闭包自带一份拥有的数据才行）。
#[tauri::command]
async fn respond_staged(
    app: tauri::AppHandle,
    ids: Vec<String>,
    allow: bool,
    always: bool,
) -> Result<Vec<notifications::StagedOutcome>, String> {
    let root = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let layout = DataLayout::new(root);
    let state = app.state::<AppState>();
    let store = notifications::NotificationStore::new(layout, state.mcp.clone());

    let deliver: &(dyn Fn(&str, String) -> notifications::BoxFuture<'_, bool> + Sync) =
        &|app_id: &str, text: String| {
            let app_id = app_id.to_string();
            let app_handle = app.clone();
            Box::pin(async move {
                let state = app_handle.state::<AppState>();
                session_mgr::steer_app_session(state.inner(), &app_id, &text).await
            })
        };

    store.respond_staged(&ids, allow, always, deliver).await
}

/// P6-C Task4：列出当前放行规则（供审批中心的"已放行规则"列表+撤销按钮），
/// `app_id` 为 `Some` 时只看该应用的——见 `approvals::ApprovalStore::list_rules`。
#[tauri::command]
fn list_approval_rules(
    app: tauri::AppHandle,
    app_id: Option<String>,
) -> Result<Vec<approvals::ApprovalRule>, String> {
    let root = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let layout = DataLayout::new(root);
    let store = approvals::ApprovalStore::new(layout);
    store.list_rules(app_id.as_deref())
}

/// P6-C Task4：撤销一条放行规则（审批中心"已放行规则"列表的"撤销"按钮）。
/// 撤销后下一次同一 `(app_id, server, tool)` 的写调用立即回到暂存路径——见
/// `approvals::ApprovalStore::remove_rule` 文档。返回是否真的删掉了一条
/// （`false` = 本就不存在，幂等，不是错误）。
#[tauri::command]
fn revoke_approval_rule(
    app: tauri::AppHandle,
    app_id: String,
    server: String,
    tool: String,
) -> Result<bool, String> {
    let root = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let layout = DataLayout::new(root);
    let store = approvals::ApprovalStore::new(layout);
    store.remove_rule(&app_id, &server, &tool)
}

/// P4 T5：响应一条 Maker 安装确认（`__host_maker_install__` 分支登记的
/// pending install，见 `maker::resolve_install` 文档 / 执行期决策"T5 安装
/// 权限确认 seam · 方案 B"）。这是该 pending install 唯一真正的消费者——
/// `allow=true` 时委托 `maker::resolve_install` → `install::install_or_upgrade`
/// （`trusted` 用登记时锁定的 `false`：Maker 输出未受信，不免检 P2 沙盒/受限
/// 模式）安装该草稿并返回 `Some(InstalledApp)`；`allow=false` 时该草稿不被
/// 安装，返回 `Ok(None)`；`confirm_id` 未知/已被消费过则返回 `Err`。
///
/// 构造 `layout`/`RegistryStore` 与既有 `install_app` 命令（见上）完全同法；
/// `McpManager` 取自 `AppState`（`state.mcp`，同 `respond_confirm`/`put_server`
/// 等既有命令读取共享连接池的方式）——`register_pending_install`（socket 侧
/// 登记）与本命令（消费）必须命中同一个 `McpManager` 实例，这靠 `McpManager`
/// 是 `Arc` 级别的 `Clone`、`AppState.mcp` 在整个进程内只有一份来保证（见
/// `mcp.rs::McpManager` 类型文档）。
#[tauri::command]
fn maker_respond_install_confirm(
    confirm_id: String,
    allow: bool,
    app: tauri::AppHandle,
) -> Result<Option<registry::InstalledApp>, String> {
    let root = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let layout = DataLayout::new(root);
    let reg = registry::RegistryStore::new(layout.registry_path());
    let state = app.state::<AppState>();
    maker::resolve_install(&state.mcp, &layout, &reg, &confirm_id, allow)
}

/// P4 T5b：列出当前所有待确认的 Maker 安装，供前端确认面渲染
/// （`display_name` + 权限人话预览 + `confirm_id`）。只读查询——不消费、不
/// 移除任何 pending（见 `maker::list_pending_installs`/
/// `mcp::McpManager::pending_install_dirs` 文档）。与
/// `maker_respond_install_confirm` 共享同一个 `state.mcp`（`AppState.mcp`
/// 全进程唯一 `Arc` 级别的 handle，见 `McpManager` 类型文档），故 socket 侧
/// （`maker::handle_install`）登记的 pending 在这里能被看到；本命令不需要
/// `DataLayout`/`RegistryStore`——`PendingInstall.draft_dir` 已经是完整路径，
/// 展示信息现读现解即可，不必再经过 app_data_dir。
#[tauri::command]
fn list_pending_installs(app: tauri::AppHandle) -> Vec<maker::PendingInstallView> {
    let state = app.state::<AppState>();
    maker::list_pending_installs(&state.mcp)
}

/// `app_sandbox_status` 的返回体：供前端标识某个已装 app 当前实际处于哪种安全态。
#[derive(serde::Serialize)]
struct SandboxStatus {
    sandboxed: bool,
    platform: String,
    restricted: bool,
}

/// 纯计算：`restricted`（该 app 是否处于 P1 受限锁定——untrusted 且没有 OS 沙盒
/// 兜底时，宿主用 `extensions:[]` + SAFE_TOOLS 硬裁剪，见 `session_mgr::build_settings_json`/
/// `resolve_tools`）。抽成纯函数只是为了可单测；`sandboxed` 参数必须来自调用方
/// （`app_sandbox_status`）已经问过的 `session_mgr::sandboxing_available()`——这里
/// 不重新判断平台，避免和 Task 6 钉死的「唯一真源」产生第二份判断。
fn compute_restricted(trusted: bool, sandboxed: bool) -> bool {
    !trusted && !sandboxed
}

/// 该 app 当前实际的安全态，供前端 UI 标识（如"沙盒已启用"/"受限模式"角标）：
/// - `sandboxed`：该 app 的 pi 子进程是否真的被 OS 级 L2 沙盒（`sandbox-exec`）包住
///   ——直接复用 `session_mgr::sandboxing_available()`（THE single source of
///   truth，见其文档注释），不在此重新推导一份 `cfg!(target_os = "macos")`，
///   否则就是该函数文档警告过的双份漂移安全漏洞。
/// - `platform`：`std::env::consts::OS`（如 "macos"/"linux"/"windows"）。
/// - `restricted`：`!trusted && !sandboxed`——即该 app 是否处于 P1 受限锁定
///   （非 macOS 上的 untrusted 第三方应用；macOS 上因为总有 L2 沙盒兜底，
///   untrusted 也不算 restricted；trusted 恒不受限）。
#[tauri::command]
fn app_sandbox_status(app_id: String, app: tauri::AppHandle) -> Result<SandboxStatus, String> {
    let root = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let layout = DataLayout::new(root);
    let reg = registry::RegistryStore::new(layout.registry_path());
    let record = reg
        .get(&app_id)
        .ok_or_else(|| format!("未知的 app_id：{app_id}"))?;
    let sandboxed = session_mgr::sandboxing_available();
    Ok(SandboxStatus {
        sandboxed,
        platform: std::env::consts::OS.to_string(),
        restricted: compute_restricted(record.trusted, sandboxed),
    })
}

/// P3 Task16 引入、Task18 修复：查询该 app（或主会话，`app_id == "main"`）当前
/// 累计的 token 用量 + pi 报告的真实花费（见 `usage::UsageAccumulator::usage_response`
/// 文档——数据来自 `get_session_stats`，不再是价格常量估算）。核心逻辑在
/// `UsageAccumulator` 上，这里只是取共享实例转发调用——单测直接构造
/// `UsageAccumulator` 覆盖核心逻辑，不需要经过这层 tauri IPC（见 `usage.rs`
/// 模块测试）。
#[tauri::command]
async fn app_usage(app_id: String, app: tauri::AppHandle) -> Result<usage::UsageResponse, String> {
    let state = app.state::<AppState>();
    Ok(state.usage.usage_response(&app_id).await)
}

/// P6-D Task5：按 (provider, model) 拆分的用量；`app_id` 为空返回所有应用。
/// 总量见 `app_usage`；总量减各模型之和即「其他（工具 / 压缩）」，界面单列。
#[tauri::command]
async fn usage_by_model(
    app_id: Option<String>,
    app: tauri::AppHandle,
) -> Result<Vec<usage::ModelUsageRow>, String> {
    Ok(app.state::<AppState>().usage.rows(app_id.as_deref()).await)
}

/// P3 Task17：`ConnectorSettings.tsx` 读取已配置的 MCP server 列表。薄封装——
/// 真正逻辑（含 keychain 存取/悬空索引自愈）在 `vault::list_servers`（Task1
/// 已实现+测试，见其文档），Task1 只留了自由函数、没有接进
/// `invoke_handler!`，这里补上前端可调用的命令层。
#[tauri::command]
fn list_servers() -> Result<Vec<vault::ServerConfig>, String> {
    vault::list_servers()
}

/// P3 Task17：`ConnectorSettings.tsx` 新增/更新一个 MCP server 配置（按 `id`
/// upsert，凭据经 `vault::put_server` 只落 keychain，不落 registry/文件）。
///
/// I-1 修复（P3 whole-branch review）：vault 写入成功后立即调用
/// `McpManager::connect_servers` 接入这一个新/改配置——否则新配置的 server
/// 要等到下次应用重启（`setup` 的批量接线）才会真正连上，用户体验上像是
/// "配置了但不起作用"。连接失败（走 best-effort 分支）不影响本命令本身的
/// 返回值：配置已经合法存进了 vault，只是这个 server 暂时连不上，与"配置
/// 是否保存成功"是两件事——前端如需感知连接状态，应另外查询（当前 UI 尚
/// 无此查询点，非本次修复范围）。
#[tauri::command]
async fn put_server(config: vault::ServerConfig, app: tauri::AppHandle) -> Result<(), String> {
    vault::put_server(&config)?;
    let state = app.state::<AppState>();
    state
        .mcp
        .connect_servers(std::slice::from_ref(&config))
        .await;
    Ok(())
}

/// P3 Task17：`ConnectorSettings.tsx` 按 id 删除一个 MCP server 配置（幂等，
/// 见 `vault::delete_server` 文档）。
///
/// **终审 Important 2**：`vault::delete_server` 只清掉 keychain/索引里的配置，
/// 对 `McpManager` 连接池毫无影响——修这个之前，用户在连接器设置里删掉一个
/// server 后，它的子进程仍活着、仍在 `conns` 里，`respond_staged` 的重新
/// 鉴权（`authorized_tools` 遍历的正是 `conns`）照样判它"仍授权"，暂存调用
/// 照样会打到一个本该已被移除的 server。vault 写入成功后调用
/// `McpManager::disconnect` 把它从连接池断开+kill 子进程，堵上这个口子——
/// 之后 `authorized_tools`/`call_tool` 立刻看不到它，见 `McpManager::disconnect`
/// 文档。
#[tauri::command]
async fn delete_server(id: String, app: tauri::AppHandle) -> Result<(), String> {
    vault::delete_server(&id)?;
    let state = app.state::<AppState>();
    state.mcp.disconnect(&id).await;
    Ok(())
}

/// 注入到宿主 webview（父帧，非应用 iframe）的初始化脚本：仅当当前帧的
/// `location.origin` 属于某个 `sagent*` 自定义 scheme（即应用 iframe 自身，见
/// `scheme::SlotPool`）时才定义 `window.superagent`——防止宿主主窗口或其它非
/// 应用来源的帧也能拿到这套桥接口。T16：用 `initialization_script_for_all_frames`
/// 注入主窗口（对主帧与其内嵌的所有应用 iframe 子帧都生效，靠上面的 origin 守卫
/// 自行区分要不要激活），见 `run()` 里手动构建主窗口那一段。
const BRIDGE_JS: &str = r#"
(function () {
  if (!location.origin.startsWith('sagent')) return;   // 仅应用帧
  const post = (m) => parent.postMessage(Object.assign({ __superagent: true }, m), '*');
  const handlers = {};
  window.superagent = {
    prompt: (text) => post({ kind: 'prompt', text }),
    command: (name, params) => post({ kind: 'command', name, params }),
    state: {
      get: (key) => post({ kind: 'state_get', key }),
      set: (key, value) => post({ kind: 'state_set', key, value }),
    },
    on: (event, cb) => { (handlers[event] = handlers[event] || []).push(cb); },
  };
  window.addEventListener('message', (e) => {
    if (e.source !== window.parent) return; // 只收宿主(直接父窗口)的消息，拒绝兄弟应用帧伪造
    const d = e.data || {};
    if (d.__superagent_host && d.event && handlers[d.event]) handlers[d.event].forEach((cb) => cb(d.payload));
  });
})();
"#;

/// 重启主助手会话：先 kill 掉旧的子进程（若还在）并把 `main_session` 置
/// `None`，再重走 `start_main_session` 重新 spawn。供 faulted 之后（或用户
/// 主动想重开配置）从 UI 触发「重启会话」。
#[tauri::command]
async fn restart_session(app: tauri::AppHandle) -> Result<(), String> {
    {
        let state = app.state::<AppState>();
        if let Some(mut s) = state.main_session.lock().await.take() {
            s.kill().await;
        };
    }
    start_main_session(app).await
}

/// 主会话的模型与密钥注入：只看全局默认（没有应用覆盖、没有清单），未设置时
/// 与此前行为一致（注入全部已配置的原生 provider 密钥）。主会话没有私有 agent home，
/// 写不了 models.json，所以选中自定义 provider 时退化为默认行为。
fn main_session_launch(layout: &DataLayout) -> model_overrides::ModelLaunch {
    let custom = providers::ProvidersStore::new(layout.providers_path())
        .list()
        .unwrap_or_else(|e| {
            eprintln!("读取自定义 provider 失败，主会话按无自定义处理：{e}");
            Vec::new()
        });
    let overrides = model_overrides::OverridesStore::new(layout.model_overrides_path())
        .load()
        .unwrap_or_else(|e| {
            eprintln!("读取模型选择失败，主会话按默认处理：{e}");
            Default::default()
        });
    let eff = model_overrides::resolve(None, &overrides, None, |id| {
        providers::is_known(id, &custom)
    });
    model_overrides::model_launch(&eff, &custom, secrets::read_key, false)
}

async fn start_main_session(app: tauri::AppHandle) -> Result<(), String> {
    let root = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let layout = DataLayout::new(root);
    let session_dir = layout.ensure("main").map_err(|e| e.to_string())?;
    let ml = main_session_launch(&layout);
    let (session, rx) = RpcSession::spawn_with(&session_dir, ml.env, ml.args).await?;

    let state = app.state::<AppState>();
    // 新会话的用量从零开始：总量与按模型拆分一起清零。
    state.usage.reset_app("main").await;
    *state.main_session.lock().await = Some(session);

    // 事件转发 + 退避重启看护：PiEvent → 前端事件；rx 关闭（pi 进程退出）时
    // 用 supervisor::Backoff 计算延迟重新 spawn，超过上限则上报 faulted。
    tokio::spawn(async move {
        let mut backoff = supervisor::Backoff::new();
        let mut rx = rx;
        loop {
            while let Some(ev) = rx.recv().await {
                match ev {
                    PiEvent::AssistantDelta(d) => {
                        let _ = app.emit("assistant-delta", d);
                    }
                    PiEvent::AgentEnded => {
                        let _ = app.emit("assistant-done", ());
                        // P3 Task18 修复：每轮结束后查一次累计用量（`PiEvent::SessionStats`
                        // 分支据此更新 `usage`）。fire-and-forget——写 stdin 失败（会话已
                        // 挂掉/正在重启）不应影响本轮已经正常完成的 assistant-done 通知。
                        if let Some(session) =
                            app.state::<AppState>().main_session.lock().await.as_ref()
                        {
                            let _ = session.send_get_session_stats().await;
                        }
                    }
                    PiEvent::AutoRetry {
                        attempt,
                        max,
                        delay_ms,
                    } => {
                        let _ = app.emit(
                            "retry-status",
                            serde_json::json!({
                                "attempt": attempt, "max": max, "delayMs": delay_ms
                            }),
                        );
                    }
                    PiEvent::ProviderError(msg) => {
                        let verdict = classify_error(&msg);
                        if let Some((kind, m)) = frontend_payload(&verdict) {
                            let _ = app.emit(
                                "agent-error",
                                serde_json::json!({
                                    "kind": kind, "message": m
                                }),
                            );
                        }
                    }
                    // 主助手会话不加载 __host_ui_emit__ 宿主工具（仅 per-app 会话，见
                    // session_mgr::open_app），此分支理论上不会触发，保留仅为穷尽匹配。
                    PiEvent::UiEmit { .. } => {}
                    // 主助手自己直接调用工具（bash/read/write 等）时同样会产生
                    // tool_execution_end，与 per-app 会话走同一条 audit::record 落盘
                    // 路径（见 session_mgr.rs 对应分支的文档注释），只是 app_id 固定为
                    // "main"（它不是一个已装 app，没有真正的 app_id）。best-effort：
                    // 审计写入失败不应影响主助手会话本身。
                    PiEvent::ToolExecuted {
                        tool_name,
                        args,
                        is_error,
                    } => {
                        let verdict = session_mgr::audit_verdict_for_tool_execution(is_error);
                        let _ =
                            audit::record(&layout, "main", &tool_name, &args.to_string(), verdict);
                    }
                    // P3 Task18 修复：对上面 AgentEnded 分支发出的 get_session_stats 查询
                    // 的响应，用与 audit 一致的 "main" 伪 app_id 记录，`app_usage("main")`
                    // 因此也能查到主会话的累计用量。`set_latest` 是覆盖不是累加（见
                    // `usage::UsageAccumulator` 文档）——不需要在这里补发 assistant-done，
                    // AgentEnded 现在正常触发，不会被这个分支抢走分类。
                    PiEvent::SessionStats {
                        input,
                        output,
                        cost,
                    } => {
                        app.state::<AppState>()
                            .usage
                            .set_latest("main", input, output, cost)
                            .await;
                    }
                    // P6-D Task5：按 (provider, model) 累加；总量仍由上面的 SessionStats 覆盖。
                    PiEvent::AssistantUsage {
                        provider,
                        model,
                        input,
                        output,
                        cost,
                    } => {
                        app.state::<AppState>()
                            .usage
                            .add_message("main", &provider, &model, input, output, cost)
                            .await;
                    }
                    PiEvent::Other(_) => {}
                }
            }
            // rx 关闭 = 进程退出，尝试退避重启
            match backoff.next_delay() {
                Some(delay) => {
                    tokio::time::sleep(delay).await;
                    // 重启时重新读全局默认与钥匙串：用户在两次重启之间改了选择也立即生效。
                    let ml = main_session_launch(&layout);
                    match RpcSession::spawn_with(&session_dir, ml.env, ml.args).await {
                        Ok((s, new_rx)) => {
                            // 重启后是新的 pi 会话（累计值从零起），两份用量一起清零。
                            app.state::<AppState>().usage.reset_app("main").await;
                            *app.state::<AppState>().main_session.lock().await = Some(s);
                            rx = new_rx;
                            backoff.reset();
                        }
                        Err(_) => continue,
                    }
                }
                None => {
                    // faulted：多次退避重启仍失败——先清空 main_session（`.take()`
                    // 语义上等价于置 None，这里直接赋 None 因为已无 RpcSession 可
                    // take：进程已退出、无需再 kill），让 UI 判空后能启用「重启会话」
                    // 按钮；必须在 `emit` 之前完成，否则前端收到事件时读到的
                    // 仍是旧状态（None 判断依据 `restart_session`/前端轮询）。
                    *app.state::<AppState>().main_session.lock().await = None;
                    let _ = app.emit(
                        "agent-error",
                        serde_json::json!({
                            "kind": "faulted", "message": "主助手多次异常退出，请检查配置"
                        }),
                    );
                    break;
                }
            }
        }
    });
    Ok(())
}

/// P3 Task15b：调度器后台周期循环的触发间隔——cron 粒度是分钟级（`scheduler.rs`
/// 清单声明一律 5 段 `分 时 日 月 周`），30s 远小于 60s，保证两次 tick 之间不会
/// 整个错过任意一个分钟边界。命名常量，不是魔法数字。
const SCHEDULER_TICK_INTERVAL: std::time::Duration = std::time::Duration::from_secs(30);

/// 应用启动时起的调度器后台周期循环：每 `SCHEDULER_TICK_INTERVAL` 跑一次
/// `scheduler::run_scheduler_tick_cycle`（真实 `SystemClock` + 当下打开的 app
/// 集合），是 Task12 `Scheduler::tick` 与 Task15 `NotificationStore` 之间此前
/// 唯一缺失的生产调用点——没有它，只有应用重新打开时的 `run_catch_up_for_app`
/// 会触发定时任务，运行期间到期的任务永远不会被发现（见本函数头部这段话对应
/// 的缺口记录：`.superpowers/sdd/progress.md` Task15b 条目）。
///
/// **running-only 边界的唯一真源**：`AppState::app_sessions`（`app_state.rs`）
/// 的 key 集合就是"当前打开的 app_id"——`session_mgr::open_app_after_acquire`
/// 分配槽位成功后才插入、`close_app`/异常退出且不再重启时才移除，与前台交互
/// 会话的生命周期完全绑定。每轮循环开始时现取一次快照（而不是在 `Scheduler`
/// 构造时固定一份）：`app_sessions` 会随用户开关应用持续变化，`Scheduler` 本身
/// 在整个循环生命周期内只构造一次并反复复用（`RegistryStore`/`TaskRegistry`/
/// `NotificationStore` 都是无内存态、现读现写磁盘的薄封装，见各自文档，反复
/// 复用同一个 `Scheduler` 实例不会读到过期的应用注册表/任务数据）。
///
/// 共享 `AppState` 里的 `mcp`（`NotificationStore::respond_confirm` 需要用它续行
/// 写确认调用），不新起一套并行的 `McpManager` 实例——否则会与 Task9 起的每
/// app MCP 连接池产生第二份互不知情的连接状态。
///
/// 只在生产 `run()` 里被调用，不出现在任何测试里：测试直接驱动
/// `scheduler::run_scheduler_tick_cycle`（注入 `TestClock` + 假的 `is_app_open`
/// 谓词，见 `tests/scheduler_running_only_it.rs`），不依赖真实墙钟或这个 30s
/// sleep 循环本身——循环体没有独立值得测的逻辑，就是"睡一觉 + 调用已经测过的
/// 那个函数"。
async fn start_scheduler_loop(app: tauri::AppHandle) {
    let root = match app.path().app_data_dir() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("调度器后台循环启动失败：无法解析 app_data_dir：{e}");
            return;
        }
    };
    let layout = DataLayout::new(root);
    let hosttools = hosttools_dir(&app);
    let clock = scheduler::SystemClock;

    loop {
        tokio::time::sleep(SCHEDULER_TICK_INTERVAL).await;

        // 每轮现造 Scheduler/NotificationStore：两者构造开销都可忽略（薄封装，
        // 见上方文档），现造而非缓存能保证每轮都用上最新的 `RegistryStore`
        // 已装应用索引（用户可能在两轮之间新装/卸载了应用）。
        let apps = registry::RegistryStore::new(layout.registry_path());
        let state = app.state::<AppState>();
        // Task17b：与 `open_app` 给前台会话用的是同一个 `AppState::mcp`（clone 只
        // 浅拷贝内部 Arc）——task-mode 会话据此算出与前台字节对齐的 MCP 授权注入，
        // 不新起一份互不知情的连接池（同下面 `notifications` 已有的共享方式）。
        let scheduler = scheduler::Scheduler::new(
            layout.clone(),
            apps,
            hosttools.clone(),
            state.mcp.clone(),
            scheduler::DEFAULT_JITTER,
        );

        let notifications =
            notifications::NotificationStore::new(layout.clone(), state.mcp.clone());
        // 现取一份打开集合快照——锁只在这个 block 内持有，构造完 HashSet 后
        // guard 立即释放，不跨下面 `run_scheduler_tick_cycle` 的 `.await` 持有。
        let open_ids: std::collections::HashSet<String> = {
            let sessions = state.app_sessions.lock().await;
            sessions.keys().cloned().collect()
        };
        let is_app_open = move |id: &str| open_ids.contains(id);

        scheduler::run_scheduler_tick_cycle(&scheduler, &clock, &notifications, &is_app_open).await;
    }
}

/// hosttools 目录（`ui_emit.ts` / `permission_gate.ts` 等宿主工具脚本所在处）：
/// 生产打包内随应用资源分发在 `resource_dir()/hosttools`；`resource_dir()` 在
/// `cargo build`/`cargo test`（未真正打包）时会失败，此时回退到仓库内的
/// `src-tauri/hosttools`（dev 场景，与本仓库结构一致）。
pub(crate) fn hosttools_dir(app: &tauri::AppHandle) -> std::path::PathBuf {
    app.path()
        .resource_dir()
        .map(|r| r.join("hosttools"))
        .unwrap_or_else(|_| std::path::PathBuf::from("src-tauri/hosttools"))
}

/// samples 目录（首发范例包 + 内置 Maker 所在处，`samples/<name>/`）：与
/// `hosttools_dir` 同一 base-resolution 手法——生产打包内随应用资源分发在
/// `resource_dir()/samples`（见 `tauri.conf.json` `bundle.resources` 把仓库根
/// `samples/` 映射到 `$RESOURCE/samples`），`resource_dir()` 在 `cargo build`/
/// `cargo test`（未真正打包）时会失败，此时回退到仓库根相对的 `"samples"`
/// （dev 场景；与 `hosttools_dir` 回退到 `"src-tauri/hosttools"`——同样是
/// repo-root 相对路径——语义一致，只是 samples 本身就直接挂在仓库根下）。
///
/// whole-branch review I1 修复引入：此前 `OnboardingWizard.tsx` 把仓库相对
/// 路径字面量 `"samples/maker"` 直接传给 `install_app`
/// （`Path::new(&source_path)`，无 base 解析，生产 CWD 不可预测），是整条
/// "唯一列出的旗舰功能在生产环境里没有可用安装路径"缺口的根因之一；
/// `seed_builtin_maker`/`install_builtin_sample_core` 现在都通过这个函数
/// 取 base，不再依赖前端传来的裸相对路径。
pub(crate) fn samples_dir(app: &tauri::AppHandle) -> std::path::PathBuf {
    app.path()
        .resource_dir()
        .map(|r| r.join("samples"))
        .unwrap_or_else(|_| std::path::PathBuf::from("samples"))
}

/// 应用启动时把内置 Maker（`samples/maker`）播种为 **trusted** 第一方应用
/// （whole-branch review I1/I2 修复）。
///
/// - **I1**：修复前没有任何生产代码路径会安装 Maker——向导传的裸相对路径
///   在生产环境解析不到源目录（见 `samples_dir` 文档），启动期也没有播种
///   逻辑。这里补上：应用每次启动都尝试播种一次。
/// - **I2**：`trusted` 硬编码为 `true`——spec §3.1 把 Maker 定义为第一方内置
///   应用，不是未受信第三方包；如果装成 `trusted=false`，macOS 上会被
///   `sandbox.rs` 的 `(deny network*)` 规则挡住，连不上模型 API，Maker 的
///   生成式对话根本跑不起来。这与 `maker::resolve_install` 里 Maker
///   **生成**出来的应用固定 `trusted=false` 是两件不同的事——那些是未经
///   用户审阅的模型输出，必须继续走沙盒/受限模式，本函数不改变那条路径。
///
/// 幂等：若 `MAKER_APP_ID` 已经在 registry 里，直接跳过、返回 `Ok(())`——
/// 不重新调用 `install_or_upgrade`（同版本重装会被 `upgrade()` 的 semver
/// 检查拒绝为"不高于已装版本"，那是一个真实错误路径，不该在"已经装过了"
/// 这个稳态下被触发）。调用方（`setup()` 的后台 spawn）失败时只应打日志、
/// 不 crash 应用启动。
pub fn seed_builtin_maker(
    samples_dir: &std::path::Path,
    layout: &DataLayout,
    registry: &registry::RegistryStore,
) -> Result<(), String> {
    if registry.get(maker::MAKER_APP_ID).is_some() {
        return Ok(());
    }
    install::install_or_upgrade(&samples_dir.join("maker"), layout, registry, true).map(|_| ())
}

/// 起步向导可安装的内置样例白名单——`install_builtin_sample`/
/// `install_builtin_sample_core` 唯一认可的 `name` 取值集合。任何不在这张表
/// 里的输入一律拒绝，防止前端传来的字符串（哪怕只是"某个样例名字"这种看似
/// 无害的输入）被直接拼进文件系统路径（`samples_dir(&app).join(name)`）造成
/// 路径穿越（例如 `"../../etc"`）——这是这条命令唯一的输入校验边界。
const BUILTIN_SAMPLE_WHITELIST: &[&str] = &[
    "maker",
    "todo-notes",
    "daily-brief",
    "writing-helper",
    "connector-demo",
    "researcher",
    "summarizer",
];

/// `install_builtin_sample` 的可测试核心（不依赖 `tauri::AppHandle`，`samples_dir`
/// 由调用方传入，供集成测试直接调用真实的 `samples/` 目录）：校验 `name` 在
/// `BUILTIN_SAMPLE_WHITELIST` 内，解析 `samples_dir.join(name)`，装成
/// **trusted=true**——这些都是随应用资源分发的第一方样例（与 P1"第一方
/// 全功能，不打折"定调一致），且若用户选中的是 Maker，必须 trusted 才能
/// 联网（I2，同 `seed_builtin_maker` 的理由）。
///
/// 与 Maker **生成**出来的应用（`trusted=false`）是两条不同的安装路径，本
/// 函数不涉及那条路径。
pub fn install_builtin_sample_core(
    name: &str,
    samples_dir: &std::path::Path,
    layout: &DataLayout,
    registry: &registry::RegistryStore,
) -> Result<registry::InstalledApp, String> {
    if !BUILTIN_SAMPLE_WHITELIST.contains(&name) {
        return Err(format!("未知的内置样例：{name}"));
    }
    install::install_or_upgrade(&samples_dir.join(name), layout, registry, true)
}

/// 起步向导安装内置样例应用（whole-branch review I1 修复）：替代此前
/// `OnboardingWizard.tsx` 直接把前端拼出的相对 `sourcePath` 交给 `install_app`
/// 的旧路径——旧路径既没有 base 解析（`install_app` 里 `Path::new(&source_path)`
/// 是相对进程 CWD，生产环境不可预测地失败），也把 `trusted` 交给前端摆布
/// （旧代码写死 `trusted:false`，Maker 若被选中就永远连不上模型 API，见
/// I2）。这里前端只传一个样例名字，服务端持有白名单 + `samples_dir(&app)`
/// 的 resource_dir 解析（核心逻辑见 `install_builtin_sample_core`）。
#[tauri::command]
fn install_builtin_sample(
    name: String,
    app: tauri::AppHandle,
) -> Result<registry::InstalledApp, String> {
    let root = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let layout = DataLayout::new(root);
    let reg = registry::RegistryStore::new(layout.registry_path());
    let samples = samples_dir(&app);
    install_builtin_sample_core(&name, &samples, &layout, &reg)
}

/// P5 市场：拉取并解析精选市场索引（可测核心，不依赖 `tauri::AppHandle`）。
/// `source` 缺省 → 读内置 demo `samples/market/index.json`（`samples_dir` 的 resource_dir
/// 解析，dev 回退，与 `install_builtin_sample_core` 同一手法）；给了 `source` → 按它拉
/// （本地路径读文件；http(s) 明确报错，见 `market::fetch_index`）。
pub fn market_fetch_index_core(
    samples_dir: &std::path::Path,
    source: Option<&str>,
) -> Result<Vec<market::MarketEntry>, String> {
    let raw = match source {
        Some(s) => market::fetch_index(s)?,
        None => {
            let path = samples_dir.join("market").join("index.json");
            market::fetch_index(&path.to_string_lossy())?
        }
    };
    market::parse_index(&raw)
}

/// P5 市场：App 内市场页拉取索引的命令。缺省读内置 demo 精选市场。
#[tauri::command]
fn market_fetch_index(
    source: Option<String>,
    app: tauri::AppHandle,
) -> Result<Vec<market::MarketEntry>, String> {
    let samples = samples_dir(&app);
    market_fetch_index_core(&samples, source.as_deref())
}

/// P5 发布：把一个已装应用导出成可分发形态 + 生成市场条目（可测核心，不依赖
/// `tauri::AppHandle`）。读 registry 记录 + 加载其权限清单 → `export_package`
/// （packages/<app_id> → published/<app_id>）+ `build_entry` + `merge_index_entry`
/// （upsert 进 published/index.json）→ 返回生成的条目。`source` 记为 `app_id`
/// （published/ 下的相对目录名）。真实推送到 GitHub 是手工里程碑，不在此自动执行。
pub fn publish_app_core(app_id: &str, layout: &DataLayout) -> Result<market::MarketEntry, String> {
    let reg = registry::RegistryStore::new(layout.registry_path());
    let app = reg
        .get(app_id)
        .ok_or_else(|| format!("应用 {app_id} 未安装"))?;
    let pkg_dir = layout.packages_dir(app_id);
    let manifest = pkg::load_and_validate(&pkg_dir).map_err(|e| e.to_string())?;
    let perms =
        permissions::load(&pkg_dir, &manifest.superagent.permissions).map_err(|e| e.to_string())?;

    publish::export_package(app_id, layout)?;
    let entry = publish::build_entry(&app, &perms, app_id.to_string());
    publish::merge_index_entry(layout, &entry)?;
    Ok(entry)
}

/// P5 发布：应用详情处「发布」按钮的命令。
#[tauri::command]
fn publish_app(app_id: String, app: tauri::AppHandle) -> Result<market::MarketEntry, String> {
    let root = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let layout = DataLayout::new(root);
    publish_app_core(&app_id, &layout)
}

// ---------------------------------------------------------------------------
// P6-B Task 5：技能命令层——`known_tools()` + `app_tool_set` + `SkillStore` 薄封装
// ---------------------------------------------------------------------------

/// 技能安装门（`SkillStore::preflight`/`install_from_dir`）用的"宿主已知工具名"
/// 全集：`allowed-tools` 声明了任何不在这个集合里的名字一律拒装（fail-closed，
/// spec §4 裁决）。**这道门只回答"pi/宿主认不认识这个名字"——不回答"这个应用能不
/// 能用它"**：后者是 `grant_skill` 在授予时对目标应用实际工具集
/// （`app_tool_set_core`）做的子集校验，两者是两道独立的闸，语义不能混。
///
/// 具体例子：一个声明 `allowed-tools: bash` 的技能能装（`bash` 在
/// `session_mgr::PI_BUILTIN_TOOLS` 里，pi 认识这个名字）；但对一个未放宽
/// （非 trusted 且非 sandboxed）的应用 `grant` 这个技能会被拒——那个应用实际的
/// `--tools` 白名单（`session_mgr::resolve_tools`）不含 `bash`（P1 安全工具集
/// `SAFE_TOOLS` 刻意不含 bash，理由见其文档）。此前的实现在这里直接调用
/// `resolve_tools(&[], true, false)` 只拿到 `SAFE_TOOLS`，把"能不能装"错误地
/// 收窄成了"未放宽应用能不能用"，比 spec §8 的措辞更严——已按 spec 改正。
///
/// 四路并集，任一新增的宿主工具来源以后都应该加进这里，而不是让 `known_tools`
/// 悄悄漏掉：
///
/// 1. **pi 自身内建工具**：`session_mgr::PI_BUILTIN_TOOLS`（"pi 认识哪些名字"，
///    与 `SAFE_TOOLS`"未放宽应用能授予哪些名字"分开维护，见该常量文档）。
/// 2. **宿主自有的 UI 推送桥**：`__host_ui_emit__`——不随任何能力声明而存在，
///    所有会话都可能用到。
/// 3. **各内置能力可能贡献的宿主方法/桥工具名**：直接枚举各能力模块导出的
///    `__host_*__` pub const（不走 `CapabilityRegistry::describe`——那需要一个
///    具体 app 的 `Permissions`/`CallerIdentity`/`LaunchCtx` 才能判定
///    "declared"，而这里要回答的是"宿主整体可能贡献哪些工具名"，与任何单个
///    app 的授权状态无关；`connectors` 能力贡献的是运行时才知道的 `mcp__...`
///    名字，由第 4 条覆盖）。
/// 4. **当前已连接 MCP server 的全部工具名**：`McpManager::all_tool_names`。
pub fn known_tools(mcp: &mcp::McpManager) -> Vec<String> {
    let mut tools: Vec<String> = session_mgr::PI_BUILTIN_TOOLS
        .iter()
        .map(|s| s.to_string())
        .collect();
    for t in [
        "__host_ui_emit__",
        capabilities::agents_call::CALL_AGENT_METHOD,
        capabilities::notifications::NOTIFY_METHOD,
        capabilities::router::LIST_AGENTS_METHOD,
        capabilities::ui_emit::UI_EMIT_TOOL,
    ] {
        if !tools.iter().any(|x| x == t) {
            tools.push(t.to_string());
        }
    }
    for m in capabilities::maker::MAKER_METHODS {
        if !tools.iter().any(|x| x == m) {
            tools.push(m.to_string());
        }
    }
    for t in mcp.all_tool_names() {
        if !tools.contains(&t) {
            tools.push(t);
        }
    }
    tools
}

/// `grant_skill` 校验 `allowed-tools ⊆ app_tools` 用的"该应用最终工具集"（可测
/// 核心，不依赖 `tauri::AppHandle`）：`resolve_tools(manifest.superagent.tools,
/// trusted, sandboxed)`（清单声明部分）∪ 该应用已声明的每个能力
/// `launch().tools`（`registry.describe(...)` 逐能力收集，未声明的能力
/// `describe` 已经把 `tools` 短路成空——见其文档，不需要在这里再判一次
/// `declared`）。
///
/// 这与 `session_mgr::assemble_launch_plan` 实际拼 `--tools` 时用的算法内容上
/// 等价（同一个 `resolve_tools` + 同一份能力贡献的工具名并集），只是故意不调
/// `CapabilityRegistry::launch`（那个方法允许 `materialize:true`，会真的
/// `create_dir_all` 落地 `filesystem` 能力声明的目录）——"这个应用能用哪些
/// 工具"是一次诊断性的只读查询（供 `grant_skill` 校验用），不该仅仅因为查了
/// 一下就在磁盘上产生安装才该有的副作用，与 `describe()` 自己"诊断/预览是
/// 只读操作"的原则一致（见 `capability.rs::CapabilityRegistry::describe` 文档）。
/// `assemble_launch_plan` 最后那道"丢弃含逗号的工具名"防御带不体现在这里——
/// 那是为了保护 `--tools` 的逗号分隔契约，与"这个工具名算不算这个应用拥有的"
/// 无关，即便真出现一个带逗号的脏名字，`grant` 的子集校验只会更严格（脏名字
/// 摆在 `app_tools` 里也不会让校验意外放行）。
pub fn app_tool_set_core(
    manifest: &pkg::Manifest,
    perms: &permissions::Permissions,
    trusted: bool,
    sandboxed: bool,
    identity: &capability::CallerIdentity,
    registry: &capability::CapabilityRegistry,
    ctx: &capability::LaunchCtx<'_>,
) -> Vec<String> {
    let mut tools = session_mgr::resolve_tools(&manifest.superagent.tools, trusted, sandboxed);
    for report in registry.describe(perms, identity, ctx) {
        for t in report.tools {
            if !tools.contains(&t) {
                tools.push(t);
            }
        }
    }
    tools
}

/// `app_tool_set_core` 的薄封装：从 `app_id` 读该应用真实安装状态（registry/
/// 清单/权限）拼出 `LaunchCtx` 后调用——同 `app_capabilities` 命令取
/// `record.trusted`/`layout`/`hosttools_dir` 的手法。
fn app_tool_set(app_id: &str, app: &tauri::AppHandle) -> Result<Vec<String>, String> {
    let state = app.state::<AppState>();
    let root = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let layout = DataLayout::new(root);
    let reg = registry::RegistryStore::new(layout.registry_path());
    let record = reg
        .get(app_id)
        .ok_or_else(|| format!("未知的 app_id：{app_id}"))?;
    let pkg_dir = layout.packages_dir(app_id);
    let m = pkg::load_and_validate(&pkg_dir).map_err(|e| e.to_string())?;
    let perms = permissions::load(&pkg_dir, &m.superagent.permissions)?;
    let hosttools = hosttools_dir(app);
    let sock = layout.mcp_socket_path(app_id);
    let sandboxed = session_mgr::sandboxing_available();
    let identity = capability::CallerIdentity::installing(app_id, record.trusted);
    let ctx = capability::LaunchCtx {
        app_id,
        trusted: record.trusted,
        sandboxed,
        materialize: false,
        layout: &layout,
        hosttools_dir: &hosttools,
        socket_path: &sock,
        mcp: &state.mcp,
    };
    Ok(app_tool_set_core(
        &m,
        &perms,
        record.trusted,
        sandboxed,
        &identity,
        &state.capabilities,
        &ctx,
    ))
}

/// 内置技能白名单（spec §5）：启动时自动播种，均为纯指令、无脚本的第一方技能
/// （`trusted=true`）。
pub const BUILTIN_SKILLS: &[&str] = &[
    "daily-brief-writing",
    "connector-etiquette",
    "app-packaging",
];

/// 应用启动时把内置技能（`samples/skills/*`）播种进技能库（可测核心，不依赖
/// `tauri::AppHandle`，同 `seed_builtin_maker` 的手法）：幂等——已装（`meta.id`
/// 命中）的名字直接跳过，不重新调用 `install_from_dir`（同名重装会被
/// `SkillStore` 的重名检查拒绝，那是一个真实错误路径，不该在"已经装过了"这个
/// 稳态下触发）。逐个尝试、best-effort（同 `connect_servers` 的设计）：单个
/// 内置技能播种失败（记一条 stderr）不阻塞其余内置技能继续播种。
pub fn seed_builtin_skills(
    samples_dir: &std::path::Path,
    layout: &DataLayout,
    mcp: &mcp::McpManager,
) -> Result<(), String> {
    let store = skills::SkillStore::new(layout.clone());
    let installed: std::collections::HashSet<String> =
        store.list()?.into_iter().map(|s| s.meta.id).collect();
    let known = known_tools(mcp);
    let now = approvals::unix_now();
    for name in BUILTIN_SKILLS {
        if installed.contains(*name) {
            continue;
        }
        let dir = samples_dir.join("skills").join(name);
        let source = skills::SkillSource {
            kind: skills::SkillSourceKind::Builtin,
            url: None,
            sha256: None,
        };
        if let Err(e) = store.install_from_dir(&dir, source, true, &known, now) {
            eprintln!("内置技能 {name} 播种失败（跳过，继续播种其余内置技能）：{e}");
        }
    }
    Ok(())
}

#[tauri::command]
fn list_skills(app: tauri::AppHandle) -> Result<Vec<skills::InstalledSkill>, String> {
    let root = app.path().app_data_dir().map_err(|e| e.to_string())?;
    skills::SkillStore::new(DataLayout::new(root)).list()
}

/// 本地导入/市场安装前的预览：走安装门但不落盘（`preflight`），供
/// `SkillInstallDialog` 展示 frontmatter 摘要、扫描发现、`trusted` 恒为
/// `false`——本地导入与市场条目都是第三方来源，预览阶段没有"这次会被信任"这
/// 一说（内置技能不经这条命令，见 `seed_builtin_skills`）。
#[derive(serde::Serialize)]
struct SkillPreview {
    meta: skills::SkillMeta,
    scan: skills::ScanReport,
    trusted: bool,
}

#[tauri::command]
fn preview_skill(path: String, app: tauri::AppHandle) -> Result<SkillPreview, String> {
    let root = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let layout = DataLayout::new(root);
    let state = app.state::<AppState>();
    let known = known_tools(&state.mcp);
    let (meta, scan) = skills::SkillStore::new(layout)
        .preflight(std::path::Path::new(&path), false, &known)
        .map_err(|e| e.to_string())?;
    Ok(SkillPreview {
        meta,
        scan,
        trusted: false,
    })
}

/// 本地目录导入一个技能（spec §5："本地目录：命令 `install_skill_from_path(path)`
/// （`trusted=false`）"）。
#[tauri::command]
fn install_skill_from_path(
    path: String,
    app: tauri::AppHandle,
) -> Result<skills::InstalledSkill, String> {
    let root = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let layout = DataLayout::new(root);
    let state = app.state::<AppState>();
    let known = known_tools(&state.mcp);
    let source = skills::SkillSource {
        kind: skills::SkillSourceKind::Local,
        url: None,
        sha256: None,
    };
    skills::SkillStore::new(layout)
        .install_from_dir(
            std::path::Path::new(&path),
            source,
            false,
            &known,
            approvals::unix_now(),
        )
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn uninstall_skill(id: String, app: tauri::AppHandle) -> Result<bool, String> {
    let root = app.path().app_data_dir().map_err(|e| e.to_string())?;
    skills::SkillStore::new(DataLayout::new(root)).uninstall(&id)
}

#[tauri::command]
fn skill_grants(
    app_id: String,
    app: tauri::AppHandle,
) -> Result<Vec<(skills::InstalledSkill, bool)>, String> {
    let root = app.path().app_data_dir().map_err(|e| e.to_string())?;
    skills::SkillStore::new(DataLayout::new(root)).grants_for(&app_id)
}

#[tauri::command]
fn grant_skill(app_id: String, skill_id: String, app: tauri::AppHandle) -> Result<(), String> {
    let tools = app_tool_set(&app_id, &app)?;
    let root = app.path().app_data_dir().map_err(|e| e.to_string())?;
    skills::SkillStore::new(DataLayout::new(root))
        .grant(&app_id, &skill_id, &tools)
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn revoke_skill(app_id: String, skill_id: String, app: tauri::AppHandle) -> Result<bool, String> {
    let root = app.path().app_data_dir().map_err(|e| e.to_string())?;
    skills::SkillStore::new(DataLayout::new(root)).revoke(&app_id, &skill_id)
}

#[tauri::command]
fn set_skill_enabled(
    app_id: String,
    skill_id: String,
    enabled: bool,
    app: tauri::AppHandle,
) -> Result<(), String> {
    let root = app.path().app_data_dir().map_err(|e| e.to_string())?;
    skills::SkillStore::new(DataLayout::new(root)).set_enabled(&app_id, &skill_id, enabled)
}

/// 市场技能条目安装的可测核心（不依赖 `tauri::AppHandle`，同
/// `market_fetch_index_core`/`publish_app_core` 的既有模式）：`entry.kind` 必须
/// 是 `"skill"`。
/// - **本地条目**（`download_url` 为空）：`entry.source`（如
///   `"skills/connector-etiquette"`）解析规则与 `install_builtin_sample_core`
///   一致——相对 `samples_dir`——但市场索引条目不是固定白名单能穷举的
///   （`install_builtin_sample_core` 那张 `BUILTIN_SAMPLE_WHITELIST` 是给"起步
///   向导"那几个固定样例用的），改用通用的路径穿越校验：拒绝绝对路径与任何
///   `..` 上跳组件（同 `pkg.rs::is_safe_rel` 的判据，防止恶意/写错的索引条目
///   把 `source` 拼成逃出 `samples_dir` 之外读宿主任意目录）；解析出目录后走
///   `install_from_dir`——同 `install_skill_from_path`，只是 `source.kind` 记成
///   `Market` 而不是 `Local`，标出"这条是从市场装的"这个出处。
/// - **HTTP 条目**：`entry.sha256` 缺失直接拒绝（声明了下载地址却不做完整性
///   校验是自相矛盾的索引条目，fail-closed，不猜）；下载 + 校验
///   （`market::download_skill_zip`）→ 解包到 `layout.skills_root()` 下的一个
///   `.market-dl-<fresh_id>` staging 目录（复用 `SkillStore::install_from_dir`
///   内部"staging 在 skills_root 下、失败/完成都清掉"的同一手法，不新增
///   `tempfile` 生产依赖；**终审 Minor 3**：目录名此前是 `.market-dl-{now}`，
///   `now` 只有秒级精度——同一秒内两次市场技能安装会拿到同一个目录名，后进者
///   开工前那句 `remove_dir_all` 会把前者正在用的解包副本清掉，改用
///   `approvals::fresh_id("market-dl")`（纳秒时间戳+计数器，见其文档）后，
///   同秒并发不再互撞）→ 走同一条 `install_from_dir` 安装门；无论安装成功
///   与否，这份临时解包副本用完即删。
///
/// 两条路径都固定 `trusted=false`——市场条目是第三方来源，不因为"来自市场"就
/// 天然可信（与内置技能 `seed_builtin_skills` 走的是完全不同的、`trusted=true`
/// 的路径）。
pub fn skill_market_install_core(
    entry: &market::MarketEntry,
    samples_dir: &std::path::Path,
    layout: &DataLayout,
    known_tools: &[String],
    now: i64,
) -> Result<skills::InstalledSkill, String> {
    if entry.kind != "skill" {
        return Err(format!(
            "市场条目 {} 不是技能条目（kind={}）",
            entry.name, entry.kind
        ));
    }

    match &entry.download_url {
        None => {
            let rel = std::path::Path::new(&entry.source);
            if rel.is_absolute()
                || rel
                    .components()
                    .any(|c| matches!(c, std::path::Component::ParentDir))
            {
                return Err(format!(
                    "市场技能条目 {} 的 source 路径不安全（绝对路径或含 ..）：{}",
                    entry.name, entry.source
                ));
            }
            let dir = samples_dir.join(rel);
            let source = skills::SkillSource {
                kind: skills::SkillSourceKind::Market,
                url: None,
                sha256: None,
            };
            skills::SkillStore::new(layout.clone())
                .install_from_dir(&dir, source, false, known_tools, now)
                .map_err(|e| e.to_string())
        }
        Some(url) => {
            let sha256 = entry.sha256.as_deref().ok_or_else(|| {
                format!(
                    "市场技能条目 {} 声明了 download_url 却没有 sha256，拒绝下载",
                    entry.name
                )
            })?;
            let bytes = market::download_skill_zip(url, sha256)?;

            let skills_root = layout.skills_root();
            std::fs::create_dir_all(&skills_root).map_err(|e| e.to_string())?;
            let unpack_dir = skills_root.join(format!(".{}", approvals::fresh_id("market-dl")));
            let _ = std::fs::remove_dir_all(&unpack_dir); // 清掉可能残留的上次失败产物
            let unpack_result = market::unpack_skill_zip(&bytes, &unpack_dir);

            let install_result = unpack_result.and_then(|()| {
                let source = skills::SkillSource {
                    kind: skills::SkillSourceKind::Market,
                    url: Some(url.clone()),
                    sha256: Some(sha256.to_string()),
                };
                skills::SkillStore::new(layout.clone())
                    .install_from_dir(&unpack_dir, source, false, known_tools, now)
                    .map_err(|e| e.to_string())
            });
            // 无论解包/安装成功还是失败都清掉这份临时解包副本——成功时
            // `install_from_dir` 已经把内容拷进 `skills/<id>/`，这里不必留底。
            let _ = std::fs::remove_dir_all(&unpack_dir);
            install_result
        }
    }
}

/// `skill_market_install_core` 的薄封装：从 `AppHandle` 取 `samples_dir`/
/// `app_data_dir`/`known_tools` 后调用。
#[tauri::command]
fn skill_market_install(
    entry: market::MarketEntry,
    app: tauri::AppHandle,
) -> Result<skills::InstalledSkill, String> {
    let root = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let layout = DataLayout::new(root);
    let state = app.state::<AppState>();
    let known = known_tools(&state.mcp);
    let samples = samples_dir(&app);
    skill_market_install_core(&entry, &samples, &layout, &known, approvals::unix_now())
}

/// P6-B Task7：列出当前所有待确认的 Maker 生成技能安装，供前端「技能」页
/// 确认面渲染（`meta`/`scan` 供 `SkillMetaSummary` 复用，同 `SkillInstallDialog`
/// 展示的信息一致）。只读查询——不消费、不移除（见
/// `maker::list_pending_skill_installs`/`skills::SkillStore::
/// list_pending_skill_installs` 文档）。这条 pending 表持久化在
/// `skills-index.json`（不同于应用安装确认那张进程内存表），所以这里只需要
/// `DataLayout`，不需要 `AppState.mcp`。
#[tauri::command]
fn list_pending_skill_installs(
    app: tauri::AppHandle,
) -> Result<Vec<maker::PendingSkillInstallView>, String> {
    let root = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let layout = DataLayout::new(root);
    let store = skills::SkillStore::new(layout);
    maker::list_pending_skill_installs(&store)
}

/// P6-B Task7：响应一条 Maker 生成技能安装确认——`allow=true` 时委托
/// `maker::resolve_install_skill` → `SkillStore::install_from_dir`
/// （`trusted` 用登记时锁定的 `false`：Maker 输出未受信，不免检安装门的
/// untrusted 分支）把技能真正落到 `skills/<id>/`；`allow=false` 时丢弃并清理
/// 暂存目录；`confirm_id` 未知/已被消费过则 `Err`。
#[tauri::command]
fn skill_respond_install_confirm(
    confirm_id: String,
    allow: bool,
    app: tauri::AppHandle,
) -> Result<Option<skills::InstalledSkill>, String> {
    let root = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let layout = DataLayout::new(root);
    let state = app.state::<AppState>();
    let known = known_tools(&state.mcp);
    let store = skills::SkillStore::new(layout);
    maker::resolve_install_skill(&store, &confirm_id, allow, &known, approvals::unix_now())
}

/// 极简 percent-decode（仅处理 `%XX`）：在做 `..` 路径逃逸检测前先对请求路径
/// 解码规范化，防止用 `%2e%2e` 之类的百分号编码绕过纯字符串匹配的 `..` 检查。
/// 用 `s.get(..)` 而非直接切片，非法/越界的 `%` 序列一律原样保留字节，不 panic。
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            if let Some(hex) = s
                .get(i + 1..i + 3)
                .and_then(|h| u8::from_str_radix(h, 16).ok())
            {
                out.push(hex);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// 按扩展名猜 Content-Type（不引入额外的 `mime` crate 依赖，应用 UI 资源的类型
/// 集合很有限，手写映射足够）。
fn content_type_for(path: &str) -> &'static str {
    let ext = std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "html" | "htm" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "ico" => "image/x-icon",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "wasm" => "application/wasm",
        "txt" => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

fn not_found() -> http::Response<Vec<u8>> {
    http::Response::builder()
        .status(http::StatusCode::NOT_FOUND)
        .header(http::header::CONTENT_TYPE, "text/plain; charset=utf-8")
        .body(b"404 Not Found".to_vec())
        .unwrap()
}

/// scheme 协议处理器的核心逻辑：按 slot 解出的 `app_id` 定位
/// `packages/<app_id>/ui/<path>`（缺省 `index.html`），响应带该应用的
/// 每应用 CSP（`scheme::csp_header`，domains 取自 registry）与正确的
/// Content-Type；`app_id` 未知（槽位空闲/越界）或文件不存在一律 404。
///
/// 安全关键点：对请求路径先 percent-decode 再校验 `..`——拒绝任何形式的路径
/// 逃逸，绝不允许应用 iframe 读到 `packages/<app_id>/ui/` 之外的宿主文件。
fn serve_app_file(
    app: &tauri::AppHandle,
    app_id: Option<&str>,
    raw_path: &str,
) -> http::Response<Vec<u8>> {
    let Some(app_id) = app_id else {
        return not_found(); // 槽位空闲：该 scheme 当前没有对应的已打开应用
    };

    let decoded = percent_decode(raw_path);
    let rel = decoded.trim_start_matches('/');
    let rel = if rel.is_empty() { "index.html" } else { rel };
    if rel.split('/').any(|seg| seg == "..") {
        return not_found(); // 拒绝 `..` 路径逃逸
    }

    let root = match app.path().app_data_dir() {
        Ok(r) => r,
        Err(_) => return not_found(),
    };
    let layout = DataLayout::new(root);
    let file_path = layout.packages_dir(app_id).join("ui").join(rel);
    let bytes = match std::fs::read(&file_path) {
        Ok(b) => b,
        Err(_) => return not_found(),
    };

    let reg = registry::RegistryStore::new(layout.registry_path());
    let domains = reg.get(app_id).map(|a| a.domains).unwrap_or_default();

    http::Response::builder()
        .header(http::header::CONTENT_TYPE, content_type_for(rel))
        .header("Content-Security-Policy", scheme::csp_header(&domains))
        .body(bytes)
        .unwrap()
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let mut builder = tauri::Builder::default()
        .manage(AppState::default())
        .invoke_handler(tauri::generate_handler![
            pi_bin::pi_version,
            secrets::set_api_key,
            secrets::has_api_key,
            secrets::clear_api_key,
            providers::list_providers,
            providers::create_custom_provider,
            providers::save_custom_provider,
            providers::remove_custom_provider,
            providers::custom_provider_presets,
            model_overrides::get_model_settings,
            model_overrides::set_global_model,
            model_overrides::set_app_model,
            probe::test_provider,
            send_prompt,
            restart_session,
            open_app,
            close_app,
            app_prompt,
            app_command,
            app_state_get,
            app_state_set,
            list_apps,
            preview_install,
            app_capabilities,
            install_app,
            install_builtin_sample,
            market_fetch_index,
            publish_app,
            uninstall_app,
            list_audit,
            app_sandbox_status,
            list_notifications,
            ack_notification,
            respond_confirm,
            list_staged_calls,
            respond_staged,
            list_approval_rules,
            revoke_approval_rule,
            maker_respond_install_confirm,
            list_pending_installs,
            app_usage,
            usage_by_model,
            list_servers,
            put_server,
            delete_server,
            list_skills,
            preview_skill,
            install_skill_from_path,
            uninstall_skill,
            skill_grants,
            grant_skill,
            revoke_skill,
            set_skill_enabled,
            skill_market_install,
            list_pending_skill_installs,
            skill_respond_install_confirm,
        ]);

    // 为每个界面槽位注册一个独立的自定义 scheme（sagent0..sagent{SLOT_COUNT-1}），
    // 供已打开应用的 iframe 通过 `sagent<slot>://localhost/<path>` 加载自己的
    // `packages/<appId>/ui/` 静态资源。异步响应签名精确对齐 tauri 2.11
    // （`register_asynchronous_uri_scheme_protocol`，见 docs.rs/tauri/2/tauri/struct.Builder.html）：
    // 处理函数拿到 `UriSchemeContext`（可取 `app_handle()`）+ `http::Request<Vec<u8>>`
    // + `UriSchemeResponder`（`.respond(http::Response<T>)`，`T: Into<Cow<'static,[u8]>>`）。
    // 文件读取丢进 `std::thread::spawn`（而非直接同步执行）避免阻塞 webview 事件循环。
    for slot in 0..scheme::SLOT_COUNT {
        builder = builder.register_asynchronous_uri_scheme_protocol(
            scheme::scheme_name(slot),
            move |ctx, request, responder| {
                let app = ctx.app_handle().clone();
                let raw_path = request.uri().path().to_string();
                std::thread::spawn(move || {
                    // slot -> app_id 是同步的纯内存查表（std::sync::Mutex），
                    // 见 app_state::AppState::slots 与 scheme::SlotPool。
                    let app_id = app
                        .state::<AppState>()
                        .slots
                        .lock()
                        .unwrap()
                        .app_for_slot(slot);
                    let resp = serve_app_file(&app, app_id.as_deref(), &raw_path);
                    responder.respond(resp);
                });
            },
        );
    }

    builder
        .setup(|app| {
            // 主窗口不走 tauri.conf.json 的自动创建（该窗口条目已设 `"create": false`，
            // 见 tauri.conf.json）：这里手动用 WebviewWindowBuilder::from_config 复现
            // 同样的配置，唯一区别是额外挂了 `initialization_script_for_all_frames`——
            // 对主帧与其内嵌的所有应用 iframe 子帧都注入 BRIDGE_JS（子帧靠脚本内的
            // origin 守卫自行判断要不要激活 window.superagent）。
            let window_config = app.config().app.windows[0].clone();
            tauri::WebviewWindowBuilder::from_config(app, &window_config)?
                .initialization_script_for_all_frames(BRIDGE_JS)
                .build()?;

            // P6-E：登记随包 pi 独立二进制——`tauri.conf.json` 的
            // `bundle.resources` 把 `binaries/pi/` 映射到 `$RESOURCE/pi`，生产
            // 打包内因此在 `resource_dir()/pi/pi` 能找到官方独立二进制（见
            // `pi_bin::register_bundled_pi` 文档）。`cargo build`/`cargo test`
            // （未真正打包）时 `resource_dir()` 本身会失败，`if let Ok` 静默
            // 跳过——解析顺序退化到 ③ dev 检出路径 / ④ 裸名 "pi"，与
            // `hosttools_dir`/`samples_dir` 同一 resource_dir-或-回退手法。
            // 这里是同步文件存在性检查，不值得为它开一个后台 spawn。
            if let Ok(r) = app.path().resource_dir() {
                let p = r.join("pi").join("pi");
                if p.is_file() {
                    let registered = crate::pi_bin::register_bundled_pi(p.clone());
                    eprintln!(
                        "随包 pi 二进制登记{}：{}",
                        if registered {
                            "成功"
                        } else {
                            "跳过（进程内已登记过）"
                        },
                        p.display()
                    );
                }
            }

            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                if let Err(e) = start_main_session(handle).await {
                    eprintln!("主助手会话启动失败：{e}");
                }
            });

            // I-1 修复（P3 whole-branch review）：应用启动时把此前已配置过的
            // 全部 MCP server 批量接入连接池——修复前生产代码没有任何调用点
            // 会触发 `McpManager::ensure_server`，`conns` 永远为空，
            // `authorized_tools()` 永远返回 `[]`，配置了 connector 的 app 打开
            // 后也看不到任何 MCP 工具。`vault::list_servers()` 只在这个后台
            // spawn 出来的任务里读（不在任何被 `cargo test` 覆盖的核心路径
            // 上），不会把 vault 对真实 keychain 的访问带进单测——与
            // `put_server` 命令的接线同理，见 `connect_servers` 文档。
            // best-effort：读 vault 失败（极端场景，如 keychain 不可用）只打
            // 一行日志，不阻塞应用启动；单个 server 连接失败由
            // `connect_servers` 自身吞掉，不影响其它 server。
            let mcp_handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                match vault::list_servers() {
                    Ok(configs) => {
                        let state = mcp_handle.state::<AppState>();
                        state.mcp.connect_servers(&configs).await;
                    }
                    Err(e) => eprintln!("启动时读取已配置 MCP server 列表失败：{e}"),
                }
            });

            // whole-branch review I1/I2 修复：应用启动时播种内置 Maker（若尚未
            // 安装），`trusted=true`——理由见 `seed_builtin_maker` 文档。后台
            // spawn（与上面 MCP server 重连、下面调度器循环同一模式），不阻塞
            // 主窗口/主会话启动；失败（例如 dev 环境下仓库 `samples/` 目录被
            // 移走这种边缘场景）只打一行日志，不 crash 应用启动。
            let maker_seed_handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                let root = match maker_seed_handle.path().app_data_dir() {
                    Ok(r) => r,
                    Err(e) => {
                        eprintln!("内置 Maker 播种失败：无法解析 app_data_dir：{e}");
                        return;
                    }
                };
                let layout = DataLayout::new(root);
                let reg = registry::RegistryStore::new(layout.registry_path());
                let samples = samples_dir(&maker_seed_handle);
                if let Err(e) = seed_builtin_maker(&samples, &layout, &reg) {
                    eprintln!("内置 Maker 播种失败：{e}");
                }
            });

            // P6-B：应用启动时把内置技能（`samples/skills/*`）播种进技能库
            // （`trusted=true`，已装跳过）——理由/失败处理策略与
            // `seed_builtin_maker` 上面那个后台任务相同（不阻塞主窗口/主会话
            // 启动，失败只打日志）；核心逻辑见 `seed_builtin_skills` 文档。
            let skills_seed_handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                let root = match skills_seed_handle.path().app_data_dir() {
                    Ok(r) => r,
                    Err(e) => {
                        eprintln!("内置技能播种失败：无法解析 app_data_dir：{e}");
                        return;
                    }
                };
                let layout = DataLayout::new(root);
                let samples = samples_dir(&skills_seed_handle);
                let state = skills_seed_handle.state::<AppState>();
                if let Err(e) = seed_builtin_skills(&samples, &layout, &state.mcp) {
                    eprintln!("内置技能播种失败：{e}");
                }
            });

            // P3 Task15b：调度器后台周期循环——见 `start_scheduler_loop` 文档。
            // 该函数体是一个永不返回的 `loop`，`tauri::async_runtime::spawn` 把它
            // 丢到后台跑，不阻塞应用启动/主窗口创建。
            let scheduler_handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                start_scheduler_loop(scheduler_handle).await;
            });
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

#[cfg(test)]
mod sandbox_status_tests {
    use super::compute_restricted;

    // ---- compute_restricted：trusted × sandboxed 四象限 ----

    #[test]
    fn trusted_never_restricted_regardless_of_sandboxing() {
        assert!(!compute_restricted(true, true));
        assert!(!compute_restricted(true, false));
    }

    #[test]
    fn untrusted_restricted_only_without_os_sandbox() {
        // untrusted + 没有 L2 沙盒兜底（非 macOS）：受限锁定。
        assert!(compute_restricted(false, false));
        // untrusted + 有 L2 沙盒兜底（macOS）：不算受限——OS 沙盒才是硬边界。
        assert!(!compute_restricted(false, true));
    }

    #[test]
    fn restricted_matches_negated_trusted_and_sandboxed_relation() {
        // 穷举四象限，钉死 restricted == !trusted && !sandboxed 这条关系本身，
        // 不只是挑几个案例断言结果。
        for trusted in [false, true] {
            for sandboxed in [false, true] {
                assert_eq!(
                    compute_restricted(trusted, sandboxed),
                    !trusted && !sandboxed
                );
            }
        }
    }

    #[test]
    fn tuple_across_current_build_platform() {
        // 用真实的 sandboxing_available()，让当前 build 实际所在平台的分支
        // （macOS 上 true / 非 macOS 上 false）被实打实地跑一遍，而不是只测
        // 两个手写的布尔值。
        let sandboxed = crate::session_mgr::sandboxing_available();
        for trusted in [false, true] {
            let restricted = compute_restricted(trusted, sandboxed);
            assert_eq!(restricted, !trusted && !sandboxed);
            if trusted {
                assert!(!restricted, "trusted 恒不应处于受限锁定");
            }
        }
    }
}
