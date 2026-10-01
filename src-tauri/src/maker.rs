// Maker subagent 暂存写入的路径防逃逸校验（Task2，P4）。
//
// `stage_write`（T4）会先调用 `resolve_staging_path` 把 Maker 生成的相对路径解析成一个
// 真实文件系统路径，再据此创建父目录 + 写文件——本函数是那次写入的**唯一安全边界**：
// 它必须在父目录尚不存在时也能正确工作（`stage_write` 调用它时还没有 mkdir -p 过）。

use crate::install;
use crate::mcp::McpManager;
use crate::paths::DataLayout;
use crate::permissions::{self, Permissions};
use crate::pkg;
use crate::registry::{self, RegistryStore};
use crate::session_mgr;
use crate::skills;
use std::path::{Path, PathBuf};

/// Maker——特权内置 app（本身是个 `superagent` 包 + pi 会话，见 spec §2 "核心
/// 洞察：Maker 不是新宿主机器，而是一个特权内置 app"）——的 well-known app id。
///
/// 这是 T9（本文件所在任务）与 T10（`samples/maker/package.json`）之间的
/// **跨任务契约**：T10 的 Maker 包的 `package.json` 里 `name`/`superagent`
/// 块必须落到与这里完全一致的 app_id（即安装后 `registry.rs::InstalledApp`
/// 的 `app_id` 字段取值必须等于本常量），`session_mgr.rs::open_app_after_acquire`
/// 才能在打开该 app 时用 `app_id == MAKER_APP_ID` 认出"这次打开的是 Maker"，
/// 从而注入 `maker_bridge.ts` 扩展 + `SUPERAGENT_MCP_SOCKET`（P6-A：`capability::
/// CallerIdentity::is_router` 同样按这个常量判断，`capabilities::maker`/
/// `capabilities::router` 两个能力的 `declared()` 都据此认出 Maker 身份，见
/// `capability.rs`/`capabilities/maker.rs` 文档）。两处若不一致，Maker 会被
/// 当成一个普通内置 app 打开——不会报错，但拿不到 `maker_bridge`，三个
/// `__host_maker_*__` 工具在它的 pi 会话里根本不存在，是一个不容易被发现的
/// 静默失效模式，所以这里单独定义成一个共享常量，而不是让 T9/T10 各自写一遍
/// 字面量 `"superagent"`。
pub const MAKER_APP_ID: &str = "superagent";

/// 把 Maker 生成的相对路径 `rel_path` 解析到 `staging_dir` 内的一个具体文件路径，
/// 拒绝任何形式的目录逃逸。
///
/// 采用与 `sandbox.rs::build_profile` 相同的"危险字符拒绝 + canonicalize + 前缀校验"
/// 防御思路，但**不** canonicalize 目标文件本身或其（可能尚不存在的）父目录——
/// `stage_write`（T4）在调用本函数之后才会 `mkdir -p` 父目录，若这里对整个 join
/// 后的路径做 canonicalize，`agent/persona.md`（`agent/` 尚不存在）这类合法输入
/// 会直接因为路径不存在而报错。
///
/// 解析步骤：
/// 1. **纯词法拒绝**（不碰文件系统）：`rel_path` 为空、以 `/` 开头（绝对路径）、
///    含任何 `..` 路径分量、或含 `"`/`\`/NUL 中任意一个危险字符——一律 `Err`。
///    这一步不依赖任何路径是否存在。
/// 2. **canonicalize base**（`staging_dir`）：要求它已存在，`canonicalize` 失败
///    即 `Err`——解析 `staging_dir` 前缀里的任何符号链接，防"暂存根目录本身
///    是个指向别处的符号链接"这类逃逸。
/// 3. `candidate = canonical_base.join(rel_path)`，再对 `candidate` 做**纯词法**
///    规范化（只折叠 `.` 分量——第 1 步已经拒绝了所有 `..`，候选路径里不会再有
///    需要向上折叠的分量），最后校验规范化结果仍以 `canonical_base` 为前缀
///    （`starts_with`）。校验通过则返回规范化后的路径，否则 `Err`。
///
/// 这一组合是安全的：第 1 步已确保没有任何分量能让路径向上跳出 base；第 2 步
/// 确保 base 自身没有符号链接逃逸；第 3 步的 `starts_with(&canonical_base)`
/// 前缀校验**是承重的（load-bearing），不是打不到的死代码**：第 1 步的纯词法
/// 拒绝只覆盖 Unix 语义下的"绝对路径"/`..`，挡不住 Windows 的"盘符相对路径"
/// （drive-relative path，如 `"C:foo"`——有盘符前缀但没有根 `\\`）。这类路径
/// `Path::is_absolute()` 返回 `false`（Rust 对 Windows 绝对路径的定义是"有前缀
/// 且以根开头"：`c:\windows` 是绝对路径，`c:temp` 不是），也不含 `..` 分量或
/// 危险字符，因此会完整通过第 1 步。到第 3 步 `canonical_base.join(rel)` 时，
/// 按 `PathBuf::push` 的文档语义——"path 有前缀但无根"会整体替换掉 `self`——
/// join 结果不是 `canonical_base/C:foo`，而是裸的 `C:foo`；只有这里的前缀
/// 校验能拦下它。本 crate 的 Windows 编译目标是真实发布目标（见
/// `Cargo.toml` 的 `windows-native`），所以这一检查会被真实触发，必须保留，
/// 不能删除或弱化。
pub fn resolve_staging_path(staging_dir: &Path, rel_path: &str) -> Result<PathBuf, String> {
    // --- 1. 纯词法拒绝 --------------------------------------------------
    if rel_path.is_empty() {
        return Err("rel_path 不能为空".to_string());
    }
    if rel_path.starts_with('/') {
        return Err(format!("rel_path 不能是绝对路径：{rel_path}"));
    }
    if rel_path.contains('"') || rel_path.contains('\\') || rel_path.contains('\0') {
        return Err(format!(
            "rel_path 含危险字符（引号/反斜线/NUL）：{rel_path}"
        ));
    }
    let rel = Path::new(rel_path);
    if rel.is_absolute() {
        // 双保险：某些平台上不以 `/` 开头也可能被判定为绝对路径（如 Windows 的
        // 盘符路径），上面 `starts_with('/')` 未必能覆盖，这里用 std 的判断兜底。
        return Err(format!("rel_path 不能是绝对路径：{rel_path}"));
    }
    if rel
        .components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err(format!("rel_path 不能包含 `..`：{rel_path}"));
    }

    // --- 2. canonicalize base --------------------------------------------
    let canonical_base = std::fs::canonicalize(staging_dir)
        .map_err(|e| format!("无法 canonicalize staging_dir：{e}"))?;

    // --- 3. join + 纯词法规范化 + 前缀校验 --------------------------------
    let candidate = canonical_base.join(rel);
    let normalized = lexically_normalize(&candidate);

    if !normalized.starts_with(&canonical_base) {
        return Err(format!(
            "解析后的路径逃逸出暂存目录：{}",
            normalized.display()
        ));
    }

    Ok(normalized)
}

/// 纯词法路径规范化：只折叠 `.`（当前目录）分量，保留其余分量原样拼接——
/// 不触碰文件系统，也不处理 `..`（调用方在此之前已经拒绝了所有含 `..` 的输入，
/// 这里不需要、也不应该再实现向上跳出的语义）。
fn lexically_normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {} // 丢弃 "."
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Maker 三个 `__host_maker_*__` 方法（`stage_write`/`preview`/`install`）的统一入口
/// （Task3，P4）：`mcp_socket.rs::process_request` 按 `method` 分发到这里，`app_id` 是
/// 监听器绑定时那份（不是 wire 上任何自称的值，见 `mcp_socket.rs` 模块文档"app 身份绝
/// 不来自线上请求"）。
///
/// T3 落的是个 STUB：不管 `method`/`params` 是什么，恒定返回 `{"ok": true}`。T4 把
/// `__host_maker_stage_write__` 换成真正的实现（见 [`handle_stage_write`]）；T5 把
/// `__host_maker_install__` 换成真正的实现（见 [`handle_install`]）；T6（本任务）把
/// `__host_maker_preview__` 换成真正的实现（见 [`handle_preview`]）——三个方法均已
/// 不再是 stub。
///
/// 签名对齐 plan 目标 `handle_maker_request(app_id, method, params, layout)`：之所以现在
/// 就穿透 `&DataLayout`（即使 preview 分支完全不用它），是因为 T4 需要它算
/// `maker_staging_dir` 落盘、T5 需要它接到 P1 安装、T6 需要它接到 P2 沙盒——现在就接好参数，
/// 后续任务不需要再回来改一遍这个函数的签名或调用点。选 `async fn`：T6 的预览分支要
/// 拉起一个真实子进程并等它 ready，天然是异步操作；调用方
/// `mcp_socket.rs::process_request` 本身也已经是 `async fn`（`.await` 着
/// `host_mcp_call`），现在定成 async 不会给 T3 增加任何成本，却省得 T6 落地时再把
/// 这个函数从 sync 改成 async、连带改一遍调用点。
///
/// `manager: &McpManager`（T5 新增末位参数）：`__host_maker_install__` 分支需要
/// 它登记 pending install（`register_pending_install`，见该分支/`resolve_install`
/// 文档"执行期决策：T5 安装权限确认 seam"）。`mcp_socket.rs::process_request`
/// 本身已经持有一份 `&McpManager`（既有 `__host_mcp_call__` 分发就用它），这里
/// 只是把同一份引用多传一步，不新增依赖、调用点只需改这一个参数。
pub async fn handle_maker_request(
    app_id: &str,
    method: &str,
    params: serde_json::Value,
    layout: &DataLayout,
    manager: &McpManager,
) -> serde_json::Value {
    // 身份校验（`app_id == MAKER_APP_ID`）在调用方分发到这里*之前*已经强制
    // 执行过——P6-A 起，这道闸不再是 `mcp_socket.rs::process_request` 里的手写
    // `if app_id != MAKER_APP_ID` 分支，而是 `capabilities::maker::MakerCapability
    // ::declared`（`id.is_router()`，即 `app_id == MAKER_APP_ID`）由
    // `capability::CapabilityRegistry::dispatch` 统一执行：只有监听器绑定时的
    // `identity.app_id` 确实是 Maker，`declared()` 才为真，`dispatch` 才会把请求
    // 路由到 `MakerCapability::handle`（进而调用本函数）；非 Maker app 在 socket
    // 上发来的同名 method 会在 `dispatch` 那一层被直接拒绝（`{"ok":false,
    // "error":"unauthorized: 该应用未声明能力 maker"}`），根本不会到达这里。因此
    // 本函数本身不需要、也不重复这个检查——但也正因为如此，
    // 这个函数不能被安全地暴露给任何绕过了 `CapabilityRegistry::dispatch` 的调用路径；
    // 目前唯一的其它调用方是测试代码直接构造调用（见 `tests/maker_it.rs`），
    // 那些测试传入的 `app_id` 是任意占位值，不代表真实身份校验、也不影响
    // 生产路径的安全性。`app_id` 参数本身在三个分支里都用不到，保留在签名里
    // 只是让调用方无需分裂出两种签名。
    let _ = app_id;
    match method {
        "__host_maker_stage_write__" => handle_stage_write(params, layout),
        "__host_maker_install__" => handle_install(params, layout, manager),
        "__host_maker_install_skill__" => handle_install_skill(params, layout, manager),
        "__host_maker_preview__" => handle_preview(params, layout).await,
        _ => serde_json::json!({ "ok": true }),
    }
}

/// `__host_maker_stage_write__` 的真正实现（T4）：把 Maker subagent 生成的一个文件
/// 落盘到它这次草稿专属的暂存目录里。
///
/// `params` 形状：`{draft_id, rel_path, content}`，三个字段都必须是字符串——缺失或
/// 类型不对，一律当作校验失败处理（`Value::as_str` 对"键不存在"和"值不是字符串"
/// 返回同一个 `None`，天然覆盖两种情况，不需要分别判断，也不会 panic）。
///
/// 步骤（严格按此顺序，任何一步失败都不写任何东西）：
/// 1. 校验 `draft_id` 本身是"单一安全路径段"——不含 `/`、`\`、`..`、NUL、且非空。
///    这一步必须先于 `layout.maker_staging_dir(draft_id)` 做，否则恶意 `draft_id`
///    （例如 `"../x"`）会让 `maker_staging_root().join(draft_id)` 拼出一个跳出
///    `maker-staging/` 的路径，后面 `create_dir_all` 会真的在宿主数据目录之外建目
///    录——这条检查是 `resolve_staging_path`（T2，只防 `rel_path` 逃逸）覆盖不到的
///    第二个逃逸面，两者缺一不可。
/// 2. `create_dir_all(staging_dir)`：`resolve_staging_path` 需要 `canonicalize`
///    这个目录，目录不存在会直接报错，所以必须先在这里建好。
/// 3. `resolve_staging_path(staging_dir, rel_path)`（T2）——真正的 `rel_path` 逃逸
///    防线。失败直接返回错误，不做任何写入。
/// 4. `create_dir_all(resolved.parent())`：`rel_path` 可能带子目录（如
///    `"agent/persona.md"`），写文件前把父目录建好。
/// 5. `std::fs::write` 落盘，成功则回 `{ok:true, path}`（`path` 是解析出的绝对路径，
///    调用方可能需要它做进一步操作，例如后续 T5 安装阶段按这个路径读文件）。
///
/// 任何一步的 IO 错误都通过 `map_err`/`match` 转成 `{ok:false, error}`，不
/// unwrap/expect——远端 Maker subagent 传来的 `params` 和文件系统状态都不可信。
fn handle_stage_write(params: serde_json::Value, layout: &DataLayout) -> serde_json::Value {
    let draft_id = match params.get("draft_id").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return stage_write_error("缺少或类型错误的 draft_id 参数（应为字符串）"),
    };
    let rel_path = match params.get("rel_path").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return stage_write_error("缺少或类型错误的 rel_path 参数（应为字符串）"),
    };
    let content = match params.get("content").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return stage_write_error("缺少或类型错误的 content 参数（应为字符串）"),
    };

    if let Err(e) = validate_draft_id_is_single_safe_segment(draft_id) {
        return stage_write_error(&e);
    }

    // 暂存目录在预览会话里对草稿应用可写：路径经 `checked_maker_staging_dir` 校验（真目录、
    // 非链接、等于由规范化数据根按字面推出的预期路径），随后立刻取目录句柄；`rel_path` 的每一级
    // 子目录与最终文件都相对句柄、以 O_NOFOLLOW 打开/创建，应用放的链接既不会被跟随、
    // 校验后再换链也无效。
    let staging_dir = match layout.checked_maker_staging_dir(draft_id) {
        Ok(p) => p,
        Err(e) => return stage_write_error(&format!("暂存目录不可用：{e}")),
    };

    let resolved = match resolve_staging_path(&staging_dir, rel_path) {
        Ok(p) => p,
        Err(e) => return stage_write_error(&e),
    };

    let write = || -> Result<(), String> {
        let id = crate::dirfd::identity_of_real_dir(&staging_dir)?;
        let mut dir = crate::dirfd::DirHandle::open_expecting(&staging_dir, id)?;
        let mut names: Vec<String> = Path::new(rel_path)
            .components()
            .filter_map(|c| match c {
                std::path::Component::Normal(n) => Some(n.to_string_lossy().into_owned()),
                _ => None,
            })
            .collect();
        let file = names.pop().ok_or_else(|| "rel_path 不能为空".to_string())?;
        for d in &names {
            dir = dir.open_subdir_creating(d)?;
        }
        dir.write_file_replacing(&file, content.as_bytes())
    };
    if let Err(e) = write() {
        return stage_write_error(&format!("写入暂存文件失败：{e}"));
    }

    serde_json::json!({ "ok": true, "path": resolved.display().to_string() })
}

/// `__host_maker_install__` 的真正实现（T5，执行期决策"方案 B"，见
/// `docs/superpowers/plans/2026-07-18-p4-maker-flagship-onboarding.md`"执行期
/// 决策：T5 安装权限确认 seam"一节）：`params` 形状 `{draft_id}`。
///
/// **本分支自身绝不安装任何东西**——它只做校验 + 登记，真正的安装动作发生在
/// 用户确认之后，由 [`resolve_install`] 执行（生产经 Tauri 命令
/// `maker_respond_install_confirm`；测试直接调用 `resolve_install`）。这是
/// P1 安装权限确认在 Rust 侧没有 seam 这一发现之后新引入的确认点：Maker 生成
/// 的草稿是模型输出、未经用户过目，不能像旧 `install_app` 命令那样"前端按钮
/// 点了就直接装"——但 Rust 侧当时也没有能弹 UI 等待用户决策的机制，所以这里
/// 采用与 Task7/Task15 MCP 写确认同款的"登记 pending → 返回 confirm_id → 等
/// 外部世界（前端/测试）决定 allow/deny → 再消费"模式，而不是在这里同步阻塞
/// 等待。
///
/// 步骤：
/// 1. 取 `draft_id`（缺失/类型错误 → `{ok:false,error}`，不注册）。
/// 2. 复用 `validate_draft_id_is_single_safe_segment`（`stage_write` 同款
///    guard，见该函数文档）——恶意 `draft_id` 必须在拼 `maker_staging_dir`
///    之前就被拒绝，这里不是新逃逸面，但也不能因为是新分支就漏掉这道防线。
/// 3. `pkg::load_and_validate(&staging_dir)`：复用 P1 的包校验逻辑（不复制、
///    不弱化）——`package.json` 缺失/解析失败/缺 `superagent-app` 关键字/
///    UI 或 permissions 文件缺失等任何一种非法草稿，都在这一步被拒绝，回
///    `{ok:false,error}`，**不注册任何 pending**（fail-closed：非法草稿连
///    "待确认"的资格都没有）。
/// 4. 校验通过：`manager.register_pending_install(staging_dir, false)`——
///    `trusted` 硬编码为 `false`：Maker 输出是未受信来源，绝不能借这条 seam
///    绕过 P2 的受限模式/沙盒（`install::install_or_upgrade` 会像对待任何
///    第三方包一样对待它）。返回
///    `{pending_confirm:true, confirm_id, message}`——与 MCP 写确认的
///    wire 形状一致（见 `mcp_socket.rs::encode_result` 的 `PendingConfirm`
///    编码），`mcp_transport.ts` 客户端已经能解这个形状，不需要为 Maker 安装
///    再单独定义一套协议。
fn handle_install(
    params: serde_json::Value,
    layout: &DataLayout,
    manager: &McpManager,
) -> serde_json::Value {
    let draft_id = match params.get("draft_id").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return install_error("缺少或类型错误的 draft_id 参数（应为字符串）"),
    };

    if let Err(e) = validate_draft_id_is_single_safe_segment(draft_id) {
        return install_error(&e);
    }

    let staging_dir = layout.maker_staging_dir(draft_id);
    let manifest = match pkg::load_and_validate(&staging_dir) {
        Ok(m) => m,
        Err(e) => return install_error(&e.to_string()),
    };

    let confirm_id = manager.register_pending_install(staging_dir, false);
    serde_json::json!({
        "pending_confirm": true,
        "confirm_id": confirm_id,
        "message": format!("「{}」请求安装，请确认权限后继续", manifest.superagent.display_name),
    })
}

fn install_error(msg: &str) -> serde_json::Value {
    serde_json::json!({ "ok": false, "error": msg })
}

/// `__host_maker_preview__` 的真正实现（T6，本任务）：`params` 形状 `{draft_id}`。
///
/// Preview = 把暂存草稿当作**未受信第三方包**（`trusted=false`——与 `handle_install`
/// 写死 `trusted=false` 同一理由：Maker 输出未经用户审阅，绝不能免检 P2 受限
/// 模式/沙盒），经与其余 untrusted 应用完全同一条路径拉起一个临时 pi 会话跑到
/// ready，验证草稿本身没有坏到连沙盒里都起不来。**本分支自身绝不安装任何
/// 东西、绝不碰 registry**——这是与 `handle_install`（登记 pending、等用户确认后
/// 才真正安装）的关键区别：preview 甚至不产生"待确认"状态，跑完/失败都不留下
/// 任何持久化副作用（`session_mgr::spawn_preview_session` 自己负责清理它在
/// `staging_dir` 里创建的临时子目录，见该函数文档）。
///
/// 步骤（严格按此顺序）：
/// 1. 取 `draft_id`（缺失/类型错误 → `{ok:false,error}`）。
/// 2. 复用 `validate_draft_id_is_single_safe_segment`（`stage_write`/`install`
///    同款 guard）——恶意 `draft_id` 必须在拼 `maker_staging_dir` 之前就被拒绝。
/// 3. `pkg::load_and_validate(&staging_dir)`：fail-closed 前置校验——损坏/不完整
///    的草稿（缺 `package.json`/`superagent` 块/UI 文件等）在这里就被拒绝，不
///    浪费一次沙盒会话去跑一个注定打不开的包（与 `handle_install` 同款前置校验，
///    见该函数文档）。
/// 4. 校验通过：`session_mgr::spawn_preview_session(layout, draft_id)`——真正拉起
///    沙盒预览会话并驱动到 ready；`Ok(())` 回 `{ok:true}`，`Err(e)` 回
///    `{ok:false,error:e}`。
async fn handle_preview(params: serde_json::Value, layout: &DataLayout) -> serde_json::Value {
    let draft_id = match params.get("draft_id").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return preview_error("缺少或类型错误的 draft_id 参数（应为字符串）"),
    };

    if let Err(e) = validate_draft_id_is_single_safe_segment(draft_id) {
        return preview_error(&e);
    }

    // 草稿目录对预览应用可写，路径经 `checked_maker_staging_dir` 校验（I-b），不先规范化再用。
    let staging_dir = match layout.checked_maker_staging_dir(draft_id) {
        Ok(p) => p,
        Err(e) => return preview_error(&format!("暂存目录不可用：{e}")),
    };
    if let Err(e) = pkg::load_and_validate(&staging_dir) {
        return preview_error(&e.to_string());
    }

    match session_mgr::spawn_preview_session(layout, draft_id).await {
        Ok(()) => serde_json::json!({ "ok": true }),
        Err(e) => preview_error(&e),
    }
}

fn preview_error(msg: &str) -> serde_json::Value {
    serde_json::json!({ "ok": false, "error": msg })
}

/// 按 `confirm_id` 续行或丢弃一次 [`handle_install`] 登记的待确认 Maker 安装
/// （T5，`__host_maker_install__` 分支自身不安装，这是唯一真正把字节落到
/// `packages/<app_id>` 并写入 registry 的入口）。抽成自由函数（而非直接嵌在
/// Tauri 命令里）是为了让测试不必经过 `tauri::AppHandle`——生产由
/// `lib.rs::maker_respond_install_confirm` 命令调用，测试直接调用本函数。
///
/// - `confirm_id` 未知/已被消费过 → `Err`（`take_pending_install` 返回
///   `None`）——不区分"从未存在"和"已经被消费过一次"，两者对调用方而言都是
///   "这个确认已经不可用了"。
/// - `allow=false`（用户拒绝）→ `Ok(None)`，`pending` 已被
///   `take_pending_install` 移除、草稿目录原样留在暂存区，不做任何安装
///   副作用。
/// - `allow=true` → 委托 `install::install_or_upgrade`（P1 已验证过的安装
///   事务：校验 + 复制 + npm --ignore-scripts + 原子落位 + 建应用数据区 +
///   写 registry，任一步失败整体回滚），`trusted` 用登记时锁定的值
///   （`handle_install` 写死的 `false`——Maker 输出不免检），成功返回
///   `Ok(Some(app))`。
pub fn resolve_install(
    manager: &McpManager,
    layout: &DataLayout,
    registry: &RegistryStore,
    confirm_id: &str,
    allow: bool,
) -> Result<Option<registry::InstalledApp>, String> {
    let pending = manager
        .take_pending_install(confirm_id)
        .ok_or_else(|| "unknown or expired confirm_id".to_string())?;

    if !allow {
        return Ok(None);
    }

    let app = install::install_or_upgrade(&pending.draft_dir, layout, registry, pending.trusted)?;
    Ok(Some(app))
}

/// 前端可见的一条待确认 Maker 安装（P4 T5b，新增 Tauri 命令 `list_pending_installs`
/// 的返回元素）：`display_name` + 权限人话预览，供确认面渲染"「<app>」请求安装 +
/// 权限清单"。`#[derive(Serialize)]`（字段名不 rename，序列化即 snake_case，
/// 同 `registry::InstalledApp`/`lib.rs::InstallPreview` 既有前端契约的命名习惯
/// 一致）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct PendingInstallView {
    pub confirm_id: String,
    pub display_name: String,
    pub permissions: Vec<String>,
}

/// 列出当前所有待确认的 Maker 安装，供前端确认面渲染（P4 T5b）——新增 Tauri
/// 命令 `list_pending_installs` 的实现，`lib.rs` 里的命令函数只是把
/// `AppState.mcp` 传进来薄薄转发一层。**只读查询，不消费、不移除任何
/// pending**（见 `McpManager::pending_install_dirs` 文档）——真正的消费入口
/// 唯一是 [`resolve_install`]。
///
/// 对每个 `(confirm_id, draft_dir)` 重新执行一遍
/// `pkg::load_and_validate(draft_dir)` + `permissions::load` +
/// `CapabilityRegistry::render_human`（P6-A，`trusted=false`）：草稿仍在暂存区磁盘上（`handle_install` 分支
/// 校验通过之后才会登记 pending，登记到被消费之间没有任何步骤会删除/修改该
/// 目录），比起让 `PendingInstall` 额外缓存一份清单内容，现读现解更不容易
/// 和磁盘状态产生第二份不一致的真源。
///
/// 一个 pending 目录如果在登记之后被外部破坏到无法通过
/// `load_and_validate`/`permissions::load`（理论上不该发生——暂存区只由
/// Maker 经 `__host_maker_stage_write__` 写，且 `handle_install` 登记前已经
/// 校验过一次），这里选择静默跳过该条目，而不是让整个列表查询失败：一条脏
/// 数据不该拖累其它合法 pending 的可见性；该条目仍原样留在
/// `McpManager::pending_installs` 里（未被移除），只是这次查询看不到它。
pub fn list_pending_installs(manager: &McpManager) -> Vec<PendingInstallView> {
    manager
        .pending_install_dirs()
        .into_iter()
        .filter_map(|(confirm_id, draft_dir)| {
            let manifest = pkg::load_and_validate(&draft_dir).ok()?;
            let perms = permissions::load(&draft_dir, &manifest.superagent.permissions).ok()?;
            // Maker 草稿一律按 trusted=false 呈现（草稿尚未安装，是否会被授予信任由用户
            // 在确认时决定）；trusted 本身也不影响非特权能力的 `declared()` 判定。
            let id = crate::capability::CallerIdentity::installing(&manifest.app_id(), false);
            Some(PendingInstallView {
                confirm_id,
                display_name: manifest.superagent.display_name,
                permissions: crate::capabilities::builtin().render_human(&perms, &id),
            })
        })
        .collect()
}

/// `__host_maker_install_skill__` 的真正实现（Task7，P6-B spec §5 末段"Maker
/// 可为应用生成技能"）：`params` 形状 `{draft_id}`。走与 [`handle_install`]
/// （应用安装）完全同款的"登记 pending → 等外部世界确认 → 再消费"模式，只是
/// 目标换成技能自己的安装门——`skills::SkillStore::preflight`/
/// `install_from_dir`（`skills.rs`），而不是 `pkg::load_and_validate` +
/// `install::install_or_upgrade` 那条应用安装门。Maker 现在能生成两种产物
/// （应用 / 技能），两种产物各自复用自己已经存在、已经过审的安装门，本函数
/// 不新增任何一条校验逻辑。
///
/// **本分支自身绝不安装任何技能**——真正的落盘动作发生在用户确认之后，由
/// [`resolve_install_skill`] 执行（生产经 Tauri 命令
/// `skill_respond_install_confirm`；测试直接调用 `resolve_install_skill`）。
///
/// 步骤：
/// 1. 取 `draft_id`（缺失/类型错误 → `{ok:false,error}`，不注册）。
/// 2. 复用 `validate_draft_id_is_single_safe_segment`（`stage_write`/
///    `install`/`preview` 四个分支共用的同一道 guard）。
/// 3. `skills::SkillStore::preflight(&staging_dir, trusted=false, known_tools)`：
///    `known_tools` 取自 `crate::known_tools(manager)`（宿主已知工具名全集，
///    与 `preview_skill`/`install_skill_from_path` 等既有技能命令用的是同一个
///    函数）。`preflight` 内部已完整覆盖"找不到 SKILL.md"/"frontmatter 非法"/
///    "name 校验失败"/"扫描超限或含符号链接"/"allowed-tools 声明了宿主不认识
///    的工具名"/"不受信来源命中 High 危险模式（`HighRiskUntrusted`——直接拒
///    装，不进 pending）"/"重名"——任一失败都 `{ok:false,error}`，不注册任何
///    pending（fail-closed，与 `handle_install` 的 `pkg::load_and_validate`
///    前置校验同一哲学）。
/// 4. 校验通过：`SkillStore::register_pending_skill_install` 持久化登记（简报
///    明确要求，与应用安装确认的进程内存表刻意不同，见
///    `skills::PendingSkillInstall` 文档）。返回
///    `{pending_confirm:true, confirm_id, meta, scan}`——`meta`/`scan` 直接
///    带回去，供前端「技能」页确认弹窗渲染 frontmatter 摘要 + 扫描发现
///    （`SkillMetaSummary` 复用的同一张信息），不需要再发一次查询。
fn handle_install_skill(
    params: serde_json::Value,
    layout: &DataLayout,
    manager: &McpManager,
) -> serde_json::Value {
    let draft_id = match params.get("draft_id").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return install_error("缺少或类型错误的 draft_id 参数（应为字符串）"),
    };

    if let Err(e) = validate_draft_id_is_single_safe_segment(draft_id) {
        return install_error(&e);
    }

    let staging_dir = layout.maker_staging_dir(draft_id);
    let known = crate::known_tools(manager);
    let store = skills::SkillStore::new(layout.clone());

    let (meta, scan) = match store.preflight(&staging_dir, false, &known) {
        Ok(v) => v,
        Err(e) => return install_error(&e.to_string()),
    };

    let confirm_id =
        match store.register_pending_skill_install(staging_dir, meta.clone(), scan.clone()) {
            Ok(id) => id,
            Err(e) => return install_error(&e),
        };

    serde_json::json!({
        "pending_confirm": true,
        "confirm_id": confirm_id,
        "meta": meta,
        "scan": scan,
    })
}

/// 按 `confirm_id` 续行或丢弃一次 [`handle_install_skill`] 登记的待确认
/// Maker 生成技能安装（Task7）——`handle_install_skill` 分支自身不安装，这是
/// 唯一真正把技能落到 `skills/<id>/` 并写进 `skills-index.json` 的入口。抽成
/// 自由函数（而非直接嵌在 Tauri 命令里）是为了让测试不必经过
/// `tauri::AppHandle`——同 [`resolve_install`] 的手法。
///
/// - `confirm_id` 未知/已被消费过 → `Err`（`take_pending_skill_install` 返回
///   `Ok(None)`）——不区分"从未存在"和"已经被消费过一次"。
/// - `allow=false`（用户拒绝）→ `Ok(None)`，**并清理暂存目录**
///   （`std::fs::remove_dir_all`，best-effort，忽略错误）。这是与
///   [`resolve_install`]（应用安装确认，deny 时暂存目录原样留在
///   `maker-staging/` 下不清理）刻意不同的一点——批次 D 简报明确要求技能这条
///   路径 deny 时清理暂存目录，这里照办；两条路径行为不一致是有意的产品决策，
///   不是疏漏。
/// - `allow=true` → 委托 `store.install_from_dir(&pending.draft_dir, ...,
///   trusted=false, known_tools, now)`——`trusted` 硬编码为 `false`：Maker
///   输出是未受信来源，与 [`handle_install`]/[`handle_preview`] 同一理由，
///   绝不能借这条 seam 绕过安装门的 untrusted 分支（`install_from_dir` 内部
///   会再跑一遍 `preflight`，包括 untrusted 的 High 危险模式拒装）。
pub fn resolve_install_skill(
    store: &skills::SkillStore,
    confirm_id: &str,
    allow: bool,
    known_tools: &[String],
    now: i64,
) -> Result<Option<skills::InstalledSkill>, String> {
    let pending = store
        .take_pending_skill_install(confirm_id)?
        .ok_or_else(|| "unknown or expired confirm_id".to_string())?;

    if !allow {
        let _ = std::fs::remove_dir_all(&pending.draft_dir);
        return Ok(None);
    }

    let source = skills::SkillSource {
        kind: skills::SkillSourceKind::Maker,
        url: None,
        sha256: None,
    };
    store
        .install_from_dir(&pending.draft_dir, source, false, known_tools, now)
        .map(Some)
        .map_err(|e| e.to_string())
}

/// 前端可见的一条待确认 Maker 生成技能安装（Task7）：与前端契约
/// `list_pending_skill_installs()` 的元素形状逐字对齐——`confirm_id`/`meta`/
/// `scan` 三个下划线字段名（`src/lib/skills.ts::PendingSkillInstall` 已按这个
/// 形状写好）。**不带 `draft_dir`**——那是宿主内部路径，没有理由暴露给前端
/// （同 [`PendingInstallView`] 不带 `draft_dir` 的理由）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct PendingSkillInstallView {
    pub confirm_id: String,
    pub meta: skills::SkillMeta,
    pub scan: skills::ScanReport,
}

/// 列出当前所有待确认的 Maker 生成技能安装，供前端「技能」页确认面渲染
/// （Task7）。只读查询——不消费、不移除（真正的消费入口唯一是
/// [`resolve_install_skill`]）。持久化在 `skills-index.json`（不同于
/// [`list_pending_installs`] 依赖的进程内存 `McpManager::pending_installs`），
/// 所以这里只需要一个 `SkillStore`，不需要 `&McpManager`。
pub fn list_pending_skill_installs(
    store: &skills::SkillStore,
) -> Result<Vec<PendingSkillInstallView>, String> {
    Ok(store
        .list_pending_skill_installs()?
        .into_iter()
        .map(|p| PendingSkillInstallView {
            confirm_id: p.confirm_id,
            meta: p.meta,
            scan: p.scan,
        })
        .collect())
}

/// `draft_id` 必须是文件名意义上的"单一路径段"：不含 `/`/`\`（防止拼出多级路径
/// 逃出 `maker_staging_dir` 的单层结构）、不含 `..`（防止父目录跳跃）、不含 NUL
/// （防止截断类注入）、非空、不等于 `.`（单点会让 `root.join(".")` 坍缩回 root
/// 本身，丧失按 draft 隔离的语义）、不含 `:`（防止 Windows 盘符相对路径逃逸，
/// 见下）。与 `resolve_staging_path` 对 `rel_path` 的纯词法拒绝思路一致，但
/// `draft_id` 语义上只应该是一段，所以直接拒绝任何路径分隔符，比 `rel_path`
/// 那种"允许多级子路径但拒绝 `..`"的规则更严格。
///
/// 为什么额外拒绝 `:`：`maker_staging_dir(draft_id)` 是 `maker_staging_root().join(draft_id)`，
/// 和 `resolve_staging_path` 里 `canonical_base.join(rel)` 是同一个"Windows
/// join 替换"风险点（见该函数文档的详细展开）——`draft_id = "C:evil"` 这类
/// 有盘符前缀但没有根的路径，不含 `/`、`\`、`..`、NUL，会完整通过上面的检查；
/// 但按 `PathBuf::push` 的文档语义，`base.join("C:evil")` 会整体替换掉 base，
/// 结果是裸的 `C:evil`，`create_dir_all` 会在暂存根之外创建目录——`resolve_staging_path`
/// 用"canonicalize base 后前缀校验"堵住了这个洞，但 `maker_staging_dir` 本身
/// 在这里被调用时目录还不存在，没法 canonicalize，所以必须在词法层面直接拒绝
/// 一切可能触发这个替换的输入。`:` 同时也覆盖了 NTFS 备用数据流（ADS，如
/// `"name:stream"`）这一相关的类似输入形态。
///
/// 拒绝完 `/`、`\`、`..`、`.`、`:`、NUL、空字符串之后，能通过这里的 `draft_id`
/// 在任何平台上都是一个真正安全的单一路径段——`root.join(draft_id)` 不可能
/// 逃逸：它不含分隔符（不能拼出多级路径）、不含 `..`/`.`（不能是相对导航）、
/// 不含 `:`（不能触发 Windows 的 join 替换语义或 ADS）、不含 NUL（不能截断）、
/// 非空（不会坍缩成 no-op join）。
fn validate_draft_id_is_single_safe_segment(draft_id: &str) -> Result<(), String> {
    if draft_id.is_empty() {
        return Err("draft_id 不能为空".to_string());
    }
    if draft_id == "."
        || draft_id.contains('/')
        || draft_id.contains('\\')
        || draft_id.contains("..")
        || draft_id.contains(':')
        || draft_id.contains('\0')
    {
        return Err(format!(
            "draft_id 必须是单一安全路径段（不含 `/`、`\\`、`..`、`.`、`:`、NUL）：{draft_id}"
        ));
    }
    Ok(())
}

fn stage_write_error(msg: &str) -> serde_json::Value {
    serde_json::json!({ "ok": false, "error": msg })
}

/// 权限声明 vs 实际使用的最小启发式核对（Task7，P4，spec §15）。
///
/// **纯启发式，不是硬门**：返回值只是给用户看的提示字符串列表，调用方绝不能据此
/// 拒绝安装、拦截 preview 或阻塞任何流程——真正兜底安全的是 `pkg::load_and_validate`
/// \+ `handle_install` 的 `trusted=false` + P2 沙盒，这个函数解决的是另一个更软的
/// 问题："Maker 生成的包声明了一项权限，但 persona/H5 里完全看不出用它干什么"，
/// 这种"声明了但可能是模型瞎写/抄模板凑数"的情况值得在确认弹窗里提醒用户一句，
/// 但不构成拒绝安装的理由——用户仍可能就是想要这项权限留着以备后用。
///
/// 判定方法很粗：把 `persona` 与 `ui_html` 拼接后整体转小写，对 `Permissions`
/// 里**声明了**的每个类别，检查这段文本里是否出现了该类别对应关键词集合中的
/// 任意一个；一个都没出现就产出一条告警。这天然接受两种误差：假阳性（权限确实
/// 用到了，只是用的措辞不在关键词集合里）和假阴性（文本里刚好提到了关键词，但
/// 其实是无关的泛泛而谈）——启发式方法故意接受这个代价，不追求精确匹配。
///
/// 类别 → 关键词映射（按 `Permissions` 结构体字段声明顺序逐一检查，告警也按此
/// 顺序产出，保证确定性 —— 相同输入永远得到相同顺序的结果）：
/// - `filesystem`（旧字段 `filesystem.read`/`filesystem.write` 任一非空）：
///   filesystem / 文件 / read / write / 读取 / 写入
/// - `ui.connectSrc`（`domains()`，界面 CSP `connect-src` 非空；P6-A 起 `network.domains`
///   已更名为 `ui.connectSrc`，`mcp` 字段已删除，改由 `connectors` 表达）：
///   network / 网络 / http / api / url
/// - `agents.call`（声明调用的其他应用列表非空）：调用 / call / agent / 应用
/// - `system.notifications`：通知 / notify / notification / 提醒
/// - `system.schedule` 或 `scheduledTasks` 非空（二者语义上都是"定时能力"，合并
///   成一条告警，避免同一件事重复刷两条）：schedule / 定时 / cron / 每天 / 提醒 /
///   计划任务
/// - `connectors`（P3 新增；`category` 是自由字符串，不是封闭枚举——逐条检查：
///   已知类别名复用上面同名关键词集合；未知类别退化为直接检查该类别字符串本身
///   是否出现在文本里——"类别名本身出现在文本里"是对任意自由字符串类别成立的
///   最起码的使用信号，不需要为每个可能出现的自定义 category 都预先定义关键词）
pub fn minimal_permission_warnings(
    perms: &Permissions,
    persona: &str,
    ui_html: &str,
) -> Vec<String> {
    let haystack = format!("{persona}\n{ui_html}").to_lowercase();
    let mentions = |keywords: &[&str]| -> bool {
        keywords
            .iter()
            .any(|k| haystack.contains(&k.to_lowercase()))
    };

    let mut warnings = Vec::new();

    if (!perms.filesystem.read.is_empty() || !perms.filesystem.write.is_empty())
        && !mentions(&["filesystem", "文件", "read", "write", "读取", "写入"])
    {
        warnings.push("声明了文件系统权限，但 persona/H5 中未见相关使用痕迹".to_string());
    }

    if !perms.domains().is_empty() && !mentions(&["network", "网络", "http", "api", "url"]) {
        warnings.push("声明了网络访问权限，但 persona/H5 中未见相关使用痕迹".to_string());
    }

    if !perms.agents.call.is_empty() && !mentions(&["调用", "call", "agent", "应用"]) {
        warnings.push("声明了调用其他应用的权限，但 persona/H5 中未见相关使用痕迹".to_string());
    }

    if perms.system.notifications && !mentions(&["通知", "notify", "notification", "提醒"]) {
        warnings.push("声明了系统通知权限，但 persona/H5 中未见相关使用痕迹".to_string());
    }

    if (perms.system.schedule || !perms.scheduled_tasks.is_empty())
        && !mentions(&["schedule", "定时", "cron", "每天", "提醒", "计划任务"])
    {
        warnings.push("声明了定时任务权限，但 persona/H5 中未见相关使用痕迹".to_string());
    }

    for conn in &perms.connectors {
        let category_lower = conn.category.to_lowercase();
        let matched = match category_lower.as_str() {
            "filesystem" => mentions(&["filesystem", "文件", "read", "write", "读取", "写入"]),
            "network" => mentions(&["network", "网络", "http", "api", "url"]),
            _ => haystack.contains(&category_lower),
        };
        if !matched {
            warnings.push(format!(
                "声明了连接器「{}」，但 persona/H5 中未见相关使用痕迹",
                conn.category
            ));
        }
    }

    warnings
}

#[cfg(test)]
mod tests {
    /// I-b：草稿暂存目录被换成链接 / 草稿内子目录被换成链接后再 stage_write → 拒绝、
    /// 受害目录无写入；正常写入照常。
    #[test]
    fn stage_write_rejects_symlinked_staging_and_subdir_ib() {
        let tmp = tempfile::tempdir().unwrap();
        let layout = DataLayout::new(tmp.path().to_path_buf());
        let victim = tmp.path().join("victim");
        std::fs::create_dir_all(&victim).unwrap();
        let ok = |d: &str, rel: &str| {
            handle_stage_write(
                serde_json::json!({"draft_id": d, "rel_path": rel, "content": "X"}),
                &layout,
            )
        };
        assert_eq!(
            ok("good", "agent/persona.md")["ok"],
            serde_json::json!(true)
        );
        // 暂存目录本身是链接
        std::os::unix::fs::symlink(&victim, tmp.path().join("maker-staging/bad")).unwrap();
        assert_eq!(ok("bad", "package.json")["ok"], serde_json::json!(false));
        // 草稿内子目录是链接（预览会话里的应用可以这样放）
        let st = layout.checked_maker_staging_dir("sub").unwrap();
        std::os::unix::fs::symlink(&victim, st.join("agent")).unwrap();
        assert_eq!(
            ok("sub", "agent/persona.md")["ok"],
            serde_json::json!(false)
        );
        // 目标文件本身是指向受害文件的链接：被替换而不是被跟随
        std::fs::write(victim.join("f"), "VICTIM").unwrap();
        std::os::unix::fs::symlink(victim.join("f"), st.join("package.json")).unwrap();
        assert_eq!(ok("sub", "package.json")["ok"], serde_json::json!(true));
        assert_eq!(std::fs::read_to_string(victim.join("f")).unwrap(), "VICTIM");
        assert_eq!(std::fs::read_dir(&victim).unwrap().count(), 1);
    }

    use super::*;

    /// Ok 场景：`agent/persona.md` 的父目录 `agent/` 在 tempdir 里尚不存在，
    /// `resolve_staging_path` 仍应成功解析（`stage_write` 在此之后才会 mkdir -p）。
    #[test]
    fn resolves_relative_path_with_nonexistent_parent() {
        let dir = tempfile::tempdir().unwrap();
        let resolved = resolve_staging_path(dir.path(), "agent/persona.md").unwrap();
        let canonical_base = std::fs::canonicalize(dir.path()).unwrap();
        assert!(resolved.starts_with(&canonical_base));
        assert_eq!(resolved, canonical_base.join("agent/persona.md"));
    }

    /// Ok 场景：另一条合法相对路径，同样验证落在 canonicalize 后的 dir 内。
    #[test]
    fn resolves_another_relative_path() {
        let dir = tempfile::tempdir().unwrap();
        let resolved = resolve_staging_path(dir.path(), "ui/index.html").unwrap();
        let canonical_base = std::fs::canonicalize(dir.path()).unwrap();
        assert!(resolved.starts_with(&canonical_base));
        assert_eq!(resolved, canonical_base.join("ui/index.html"));
    }

    /// 拒绝以 `..` 开头的相对路径逃逸。
    #[test]
    fn rejects_dotdot_prefix_escape() {
        let dir = tempfile::tempdir().unwrap();
        assert!(resolve_staging_path(dir.path(), "../escape").is_err());
    }

    /// 拒绝绝对路径（以 `/` 开头）。
    #[test]
    fn rejects_absolute_path() {
        let dir = tempfile::tempdir().unwrap();
        assert!(resolve_staging_path(dir.path(), "/etc/x").is_err());
    }

    /// 拒绝深层路径中夹带的 `..`（即使净效果仍可能落在目录内，也一律拒绝——
    /// 纯词法检查，不做"净位移"计算）。
    #[test]
    fn rejects_deep_path_containing_dotdot_component() {
        let dir = tempfile::tempdir().unwrap();
        assert!(resolve_staging_path(dir.path(), "a/../../b").is_err());
    }

    /// 拒绝包含双引号的路径（防 SBPL/shell 注入字符）。
    #[test]
    fn rejects_path_with_quote_character() {
        let dir = tempfile::tempdir().unwrap();
        assert!(resolve_staging_path(dir.path(), "a\"b").is_err());
    }

    /// 拒绝包含反斜线的路径。
    #[test]
    fn rejects_path_with_backslash_character() {
        let dir = tempfile::tempdir().unwrap();
        assert!(resolve_staging_path(dir.path(), "a\\b").is_err());
    }

    /// 拒绝包含 NUL 字节的路径。
    #[test]
    fn rejects_path_with_nul_byte() {
        let dir = tempfile::tempdir().unwrap();
        assert!(resolve_staging_path(dir.path(), "a\0b").is_err());
    }

    /// 拒绝空字符串。
    #[test]
    fn rejects_empty_path() {
        let dir = tempfile::tempdir().unwrap();
        assert!(resolve_staging_path(dir.path(), "").is_err());
    }

    /// base（staging_dir）本身不存在、无法 canonicalize 时应返回 Err，而不是
    /// 静默回退——这是"暂存目录必须已存在"这一前置假设被打破时的正确表现
    /// （与 sandbox.rs::build_profile 对 app_data 的处理同规格：必须成功
    /// canonicalize，不做绝对路径回退）。
    #[test]
    fn rejects_when_base_cannot_be_canonicalized() {
        let missing = std::path::PathBuf::from("/tmp/does-not-exist-maker-staging-t2/nope");
        assert!(resolve_staging_path(&missing, "agent/persona.md").is_err());
    }

    /// `Component::CurDir`（路径中的 `.` 分量）应被 `lexically_normalize` 折叠掉，
    /// 使含 `./` 的合法路径与不含它的等价路径解析到同一个 `PathBuf`。
    #[test]
    fn resolves_path_with_curdir_component() {
        let dir = tempfile::tempdir().unwrap();
        let with_curdir = resolve_staging_path(dir.path(), "agent/./persona.md").unwrap();
        let without_curdir = resolve_staging_path(dir.path(), "agent/persona.md").unwrap();
        assert_eq!(with_curdir, without_curdir);
    }

    /// Windows 专属回归测试：`"C:foo"` 是"盘符相对路径"（有前缀、无根），
    /// `Path::is_absolute()` 对它返回 `false`，且它不含 `..` 分量或危险字符，
    /// 因此会完整通过第 1 步的纯词法拒绝。到第 3 步 `canonical_base.join(rel)`
    /// 时，按 `PathBuf::push` 的文档语义，这类"有前缀无根"的 path 会整体替换
    /// 掉 `self`，join 结果不是 `canonical_base` 下的子路径，而是裸的
    /// `"C:foo"`——只有最终的 `starts_with(&canonical_base)` 前缀校验能拦下它。
    /// 本测试锁定这一行为：该前缀校验在 Windows 上是真正会被触发的防线，不是
    /// 死代码。
    #[cfg(windows)]
    #[test]
    fn rejects_windows_drive_relative_path() {
        let dir = tempfile::tempdir().unwrap();
        let canonical_base = std::fs::canonicalize(dir.path()).unwrap();
        assert!(resolve_staging_path(&canonical_base, "C:foo").is_err());
    }

    // --- Task7: minimal_permission_warnings（声明未用 → 提示，非硬门）---

    /// 声明 `system.schedule: true` 但 persona/H5 都没有任何“定时”相关字样
    /// → 应产出至少一条告警，且告警文案里能看到“定时”二字（弱断言：只锁定
    /// 语义信号，不锁定措辞本身，避免文案微调就打断测试）。
    #[test]
    fn warns_when_schedule_declared_but_no_schedule_words() {
        let perms: Permissions =
            serde_json::from_str(r#"{ "system": { "schedule": true } }"#).unwrap();
        let warnings =
            minimal_permission_warnings(&perms, "我是一个天气助手", "<div>今天天气</div>");
        assert!(!warnings.is_empty());
        assert!(warnings.iter().any(|w| w.contains("定时")));
    }

    /// 声明了 `connectors:[{category:"filesystem"}]`，且 persona/H5 文本里
    /// 确实出现了 filesystem/read/write 一类字样 → 不应该有关于文件系统的告警。
    #[test]
    fn no_warning_when_filesystem_connector_declared_and_used() {
        let perms: Permissions =
            serde_json::from_str(r#"{ "connectors": [{"category": "filesystem"}] }"#).unwrap();
        let warnings = minimal_permission_warnings(
            &perms,
            "我会读取你下载目录里的文件并帮你整理",
            "<div>write summary</div>",
        );
        assert!(warnings.is_empty());
    }

    /// 空权限声明 → 无论 persona/H5 写什么，都不应该产出任何告警（没有声明就
    /// 没有“声明未用”这回事）。
    #[test]
    fn empty_permissions_yield_no_warnings() {
        let perms: Permissions = serde_json::from_str("{}").unwrap();
        let warnings = minimal_permission_warnings(&perms, "随便写点什么", "<div>随便</div>");
        assert!(warnings.is_empty());
    }

    /// 声明了文件系统连接器，但 persona/H5 全文都没提过文件系统相关字样
    /// → 应该告警。
    #[test]
    fn warns_when_filesystem_connector_declared_but_unused() {
        let perms: Permissions =
            serde_json::from_str(r#"{ "connectors": [{"category": "filesystem"}] }"#).unwrap();
        let warnings =
            minimal_permission_warnings(&perms, "我是一个聊天机器人", "<div>hello</div>");
        assert!(!warnings.is_empty());
    }

    /// 声明 `system.schedule: true` 且 persona 里确实提到了定时相关字样 →
    /// 不应该有关于定时任务的告警。
    #[test]
    fn no_warning_when_schedule_declared_and_used() {
        let perms: Permissions =
            serde_json::from_str(r#"{ "system": { "schedule": true } }"#).unwrap();
        let warnings = minimal_permission_warnings(
            &perms,
            "我会帮你创建每天的定时提醒",
            "<div>schedule</div>",
        );
        assert!(warnings.is_empty());
    }
}
