use crate::capability::{CallerIdentity, CapabilityRegistry, LaunchContribution, LaunchCtx};
use crate::mcp::McpManager;
use crate::model_overrides::ModelLaunch;
use crate::paths::DataLayout;
use crate::registry::{InstalledApp, RegistryStore};
use std::path::{Path, PathBuf};
use tauri::{Emitter, Manager};

/// 一次应用会话启动所需的确定性参数：额外 CLI 参数 + 额外环境变量 + 沙盒放行路径。
/// `--model`/`--tools`/各能力贡献的桥/env 由 `assemble_launch_plan` 在 `build_launch`
/// 的基础上追加（P6-A：四条会话路径统一走这一个拼装函数，见其文档）。
/// `sandbox_read`/`sandbox_write`（P6-A 新增）：`filesystem` 能力清单声明并展开后的
/// 额外只读/可写目录，原样转给 `spawn_app_session` 换取 `sandbox::build_profile` 的
/// 放行——`$APP_DATA` 本身已隐含默认可写，不出现在这两个集合里。
pub struct LaunchPlan {
    pub extra_args: Vec<String>,
    pub env: Vec<(String, String)>,
    pub sandbox_read: Vec<PathBuf>,
    pub sandbox_write: Vec<PathBuf>,
}

/// THE single source of truth for "does this app run under an OS-level sandbox".
/// 这是唯一一处判断——本文件里两处必须永远一致、否则会产生安全漏洞的地方，
/// 都必须调用这个函数，不得各自重新写一份 `cfg!(target_os = "macos")`：
/// (a) `spawn_app_session`（及其内部 `sandboxed_argv`）决定是否真的把 pi 子
///     进程包进 `/usr/bin/sandbox-exec`；
/// (b) `open_app_after_acquire` 算出的 `sandboxed`，喂给
///     `build_settings_json`/`resolve_tools`，决定是否放宽第三方扩展/工具
///     限制（放宽的前提正是「OS 沙盒已经是真正的硬边界」）。
///
/// 两处一旦独立漂移（例如以后加 Linux bubblewrap 支持或某个「per-app 跳过
/// 沙盒」调试开关时只改了其中一处），失败模式是沉默且严重的：`sandboxed=true`
/// 但 pi 实际未被 OS 沙盒包住——untrusted 第三方应用会被当作「有沙盒兜底」放行
/// extensions + bash/联网工具，而真实进程完全没有 OS 边界，正是 P1 用
/// `extensions:[]` + SAFE_TOOLS 白名单堵死的那个数据外泄口子。
///
/// 下面紧跟的 `const _` 是编译期一致性钉子：把这个函数的返回值与它在两处
/// 编译期分支选择（`#[cfg(target_os = "macos")]` / `#[cfg(not(...))]`，见
/// `spawn_app_session`）里假定的字面量钉在一起，两者一旦不一致直接编译失败，
/// 而不是留到运行时才暴露成一个安全漏洞。
pub(crate) const fn sandboxing_available() -> bool {
    cfg!(target_os = "macos")
}

const _: () = assert!(sandboxing_available() == cfg!(target_os = "macos"));

/// 构建该应用的受限 settings.json：`packages` 只含该应用自己一个包。
/// 第一方（trusted）省略 `extensions` 键 = 全加载，`sandboxed` 取值不影响。
/// 第三方（untrusted）：
/// - `sandboxed == false`（非 L2 平台）：P1 行为，`extensions: []` = 受限
///   （不加载第三方扩展/钩子，防止未经审计的代码在宿主内跑——宿主没有别的
///   边界能挡这段代码）。
/// - `sandboxed == true`（该 app 的 pi 确实被 `sandbox-exec` 包住，见
///   `sandboxing_available()` 的文档注释）：省略 `extensions` 键 = 加载。此时
///   OS 级沙盒才是真正的硬边界，宿主侧再挡 extensions 意义不大，且第三方应用
///   需要它才能提供完整功能（自定义工具/钩子）。
pub fn build_settings_json(
    app: &InstalledApp,
    layout: &DataLayout,
    sandboxed: bool,
) -> serde_json::Value {
    let source = layout
        .packages_dir(&app.app_id)
        .to_string_lossy()
        .to_string();
    let mut entry =
        serde_json::json!({ "source": source, "skills": ["skills/*"], "prompts": ["prompts/*"] });
    if !app.trusted && !sandboxed {
        entry["extensions"] = serde_json::json!([]); // 受限：不加载第三方扩展/钩子
    }
    serde_json::json!({ "packages": [entry] })
}

/// 构建该应用 pi 子进程的启动计划里"与清单/能力贡献无关"的确定性部分：
/// - `--append-system-prompt <persona>`：注入该应用的人格/系统提示词
/// - `-e permission_gate.ts`：应用与宿主通信的权限门控桥——所有应用无条件加载，
///   不属于任何"能力"（P6-A：`ui_emit.ts` 桥改由 `ui_emit` 能力经
///   `LaunchContribution.bridges` 贡献，见 `capabilities::ui_emit`；不再在这里
///   硬编码，是"一类能力 = 一处贡献点"这条 P6-A 设计的直接体现）。
/// - env：`PI_CODING_AGENT_DIR`（agent home）、`SUPERAGENT_APP_DATA`（应用数据目录）、
///   `SUPERAGENT_READ_PATHS`（P1 阶段读路径为空，宿主 gate 只放行 APP_DATA）、
///   `SUPERAGENT_APP_ID`（P2 新增：`permission_gate.ts` 上报 advisory 审计事件时
///   需要知道自己是哪个 app，见该文件 `{app_id, tool, args, verdict}` 的 payload）
///
/// `--model`/`--tools`/各能力贡献的桥与 env 由 `assemble_launch_plan`（紧接本函数
/// 之后）在此基础上追加——三条真正的会话路径（交互 `open_app_after_acquire`、
/// headless `run_headless_session` 内核共用于 task-mode/call）统一走
/// `assemble_launch_plan` → 本函数。
///
/// - `--no-skills`：P6-B spec §4/裁决 2——关掉 pi 自身对 `~/.agents/skills`/
///   `PI_CODING_AGENT_DIR` 的技能自动发现，**无条件**加给经本函数拼装的三条会话
///   路径，与该应用是否声明 `skills` 能力无关（那条能力只负责"声明了就能看见
///   宿主授予的集合"，见 `capabilities::skills`）。放在这个确定性基座里而不是
///   能力贡献里，是因为它是一条"堵泄漏"的底线，不该因为应用没声明 `skills.allow`
///   就被跳过——未声明的应用如果没有这一条，反而会看见用户真实主目录下的任意
///   技能，是清单权限模型要防的那类越权。**第四条路径** `spawn_preview_session`
///   不经本函数（见 `assemble_launch_plan` 文档"预览不读清单/权限，不需要任何
///   桥"），因此不能靠这里的调用兜底——它在自己的 `preview_extra_args` 里
///   独立追加同一个 `"--no-skills"`（审查修复轮 1 Important：此前遗漏，预览
///   会话会看见真实主目录下的任意技能，同一个泄漏口子在第四条路径上重演）。
///
/// - **沙盒可写基座**：`sandbox_write` 恒含该应用自己的 `agent_home_dir`
///   （`PI_CODING_AGENT_DIR`）与 `session_dir`（`PI_CODING_AGENT_SESSION_DIR`）。pi 启动时要在
///   agent home 里建 `trust.json`/`settings.json` 的锁、读写凭据存储、加载 `models.json`，
///   会话文件写在会话目录；这两处都不在 `$APP_DATA`（`<root>/apps/<id>`）里，不放行的话真实
///   安装（数据根在用户目录下）里沙盒应用的 pi 在启动阶段就崩溃。放行是安全的：这两个目录
///   是该应用私有的（路径按 app_id 隔离，不含数据根与其它应用的目录）；宿主每次拉起前都会
///   重写 `settings.json`/`models.json`，应用篡改它们只影响它自己、越不出沙盒；`models.json`
///   里只有 `${VAR}` 引用，没有密钥字面值。`assemble_launch_plan` 合并能力贡献时保留这两项。
/// - **沙盒只读基座**：`sandbox_read` 恒含该应用的 `packages_dir` 与 `hosttools_dir`，理由见函数体内注释。
pub fn build_launch(
    app: &InstalledApp,
    layout: &DataLayout,
    hosttools_dir: &Path,
) -> Result<LaunchPlan, String> {
    // 三个应用可写目录一律经 `private_dir` 取得（拒绝符号链接/非目录、返回规范化字面路径）：
    // 应用能在自己的目录里换链接，宿主不能把「拉起那一刻由应用可控的文件系统状态解析出来的
    // 路径」授权给沙盒（C1）。校验失败 → Err，调用方拒绝启动。
    let agent_home = layout.private_dir("agenthome", &app.app_id)?;
    let session_dir = layout.private_dir("sessions", &app.app_id)?;
    let app_data = layout.private_dir("apps", &app.app_id)?;
    let pkg = layout.packages_dir(&app.app_id);
    let s = |p: PathBuf| p.to_string_lossy().to_string();
    let extra_args = vec![
        "--append-system-prompt".into(),
        s(pkg.join("agent/persona.md")),
        "-e".into(),
        s(hosttools_dir.join("permission_gate.ts")),
        "--no-skills".into(),
    ];
    let env = vec![
        ("PI_CODING_AGENT_DIR".into(), s(agent_home.clone())),
        ("SUPERAGENT_APP_DATA".into(), s(app_data)),
        ("SUPERAGENT_READ_PATHS".into(), "[]".to_string()), // P1 读路径为空（gate 只放行 APP_DATA）
        ("SUPERAGENT_APP_ID".into(), app.app_id.clone()),
    ];
    Ok(LaunchPlan {
        extra_args,
        env,
        // 只读基座：该应用自己的包目录（persona.md 等，应用自己的只读内容）与宿主的
        // hosttools 目录（`-e` 加载的权限闸/各能力桥，宿主自己的公开扩展代码）。
        // 它们都不在 `$APP_DATA` 里；不放行时 pi 读不到 persona（只警告），`-e` 扩展
        // 则被静默跳过——沙盒应用里权限闸与全部桥悄悄缺席。只读放行不越权：不含数据根与
        // 其它应用的目录。
        sandbox_read: vec![pkg.clone(), hosttools_dir.to_path_buf()],
        // 该应用自己的 agent home 与会话目录（见函数文档「沙盒可写基座」）。
        sandbox_write: vec![agent_home, session_dir],
    })
}

/// 纯函数：把 `build_launch` 的确定性基座 + 该应用清单 + `CapabilityRegistry::launch`
/// 算出的贡献拼成最终 `LaunchPlan`（P6-A 核心：不起进程、不 bind socket）。四条会话
/// 路径——`open_app_after_acquire`（交互会话）、`run_headless_session`（headless 内核，
/// `spawn_task_session`/`spawn_call_session` 共用）——统一走这一个拼装点，不再各自
/// 手写一份；`spawn_preview_session` 刻意不接入（见其文档"不做的事"一节，预览不读
/// 清单/权限，不需要任何桥）。
///
/// 规则（spec §7.5，修复本任务标题里那个"--tools 白名单把桥工具过滤掉"的生产缺陷）：
/// - `extra_args` = `build_launch(app).extra_args`（persona + permission_gate.ts，
///   不再含 ui_emit.ts——它现在由 `ui_emit` 能力经 `contribution.bridges` 贡献）
///   + `["--model", m]`（若清单声明了 model）
///   + `["--tools", <去重保序的工具列表>.join(",")]`：`resolve_tools(manifest.superagent.tools,
///     app.trusted, sandboxed)` 与 `contribution.tools`（各能力贡献的桥工具名，如
///     `__host_call_agent__`/`mcp__<server>__<tool>`）取并集——此前 `--tools` 只由
///     清单声明算出，桥注入的工具名从不在其中，pi 自己的工具白名单会把这些工具
///     直接拒绝，等于桥白注入了（本任务修的确认缺陷）。
///   + `contribution.extra_args`
///   + 对每个 `contribution.bridges` 追加一对 `["-e", hosttools_dir/<桥文件>]`；
/// - `env` = `build_launch.env` + `contribution.env`；
/// - `sandbox_read`/`sandbox_write` = `build_launch` 基座（只读：应用包目录 + hosttools；可写：该应用自己的 agent home 与会话目录）
///   ∪ `contribution.sandbox_read`/`sandbox_write`，去重保序（当前唯一贡献者是 `filesystem`
///   能力，见其 `launch` 实现）。
pub fn assemble_launch_plan(
    app: &InstalledApp,
    manifest: &crate::pkg::Manifest,
    contribution: &LaunchContribution,
    layout: &DataLayout,
    hosttools_dir: &Path,
    sandboxed: bool,
    model: &ModelLaunch,
) -> Result<LaunchPlan, String> {
    let mut plan = build_launch(app, layout, hosttools_dir)?;
    // 模型选择（应用覆盖 > 全局默认 > 清单默认）与按所选 provider 最小注入的密钥环境变量，
    // 由调用方经 `resolve_model_launch` 算好后传入。这是产生启动计划的唯一拼装点，
    // 交互与 headless 两条路径一起受益（此前应用会话的 plan.env 里没有任何密钥）。
    plan.extra_args.extend(model.args.iter().cloned());
    plan.env.extend(model.env.iter().cloned());
    let mut tools = resolve_tools(&manifest.superagent.tools, app.trusted, sandboxed);
    for t in &contribution.tools {
        if !tools.contains(t) {
            tools.push(t.clone());
        }
    }
    // F2（review）防御带 + 残留收口（P6-A 终审残留）：`tools` 到这里已经是
    // `resolve_tools`（清单声明部分——trusted/sandboxed 放宽路径下 declared 原样
    // 放行，见其文档）与 `contribution.tools`（各能力贡献的桥工具名，如
    // `capabilities::connectors::is_safe_name` 挡过的 MCP 工具名）合并后的整表。
    // 此前这道逗号过滤只施于合并 `contribution.tools` 的那个循环内部，`resolve_tools`
    // 来的清单声明部分完全没过滤——下面的 `debug_assert!` 检查的却是合并后的
    // 整表，一旦清单自己声明了一个带逗号的工具名（写错/被篡改），relaxed 分支下
    // 原样放行进 `tools`，这条自检会直接 panic：不是"防住了脏名字"，是"崩在了
    // 自检本身上"。改成对合并后的整个列表统一过滤一遍（无论来自哪个来源），
    // `debug_assert!` 才真的是"确认过滤生效"而不是一颗随时炸的雷。整条丢弃
    // （不是转义/替换），并原样报告丢弃了谁——防止一个脏名字打破下面
    // `tools.join(",")` 的逗号分隔契约，把额外工具名（如 `bash`）偷偷注入 pi 的
    // `--tools` 白名单，这正是 P1 用 `extensions:[]` + SAFE_TOOLS 白名单要堵死的
    // 那个数据外泄口子的同一形状。
    tools.retain(|t| {
        let ok = !t.contains(',');
        if !ok {
            eprintln!(
                "assemble_launch_plan：丢弃含逗号的工具名 {t:?}（打破 --tools 分隔契约，拒绝注入白名单）"
            );
        }
        ok
    });
    // 自检：上面这条防御带确实对合并后的整表生效——拼出的最终 --tools 列表不该
    // 再含任何逗号。release 编译期整体裁掉，不产生运行时开销；debug/test 下用来
    // 钉住这个不变式，一旦有改动让脏名字漏网就立刻炸给开发者看，而不是留到 pi
    // 子进程侧才炸。
    debug_assert!(
        tools.iter().all(|t| !t.contains(',')),
        "assemble_launch_plan 拼出的 --tools 列表仍含逗号：防御带失效，是缺陷不是配置问题"
    );
    plan.extra_args.push("--tools".into());
    plan.extra_args.push(tools.join(","));
    plan.extra_args
        .extend(contribution.extra_args.iter().cloned());
    for b in &contribution.bridges {
        plan.extra_args.push("-e".into());
        plan.extra_args
            .push(hosttools_dir.join(b).to_string_lossy().to_string());
    }
    plan.env.extend(contribution.env.iter().cloned());
    // 合并去重而不是覆盖：基座里的 agent home / 会话目录必须保留。
    for p in &contribution.sandbox_read {
        if !plan.sandbox_read.contains(p) {
            plan.sandbox_read.push(p.clone());
        }
    }
    for p in &contribution.sandbox_write {
        if !plan.sandbox_write.contains(p) {
            plan.sandbox_write.push(p.clone());
        }
    }
    Ok(plan)
}

/// 两条真实会话路径（交互 / headless）共用：读 providers.json、model-overrides.json 与钥匙串，
/// 算出该应用的 `ModelLaunch`（--provider/--model 参数、最小注入的密钥环境变量、models.json）。
fn resolve_model_launch(
    layout: &DataLayout,
    app_id: &str,
    manifest: &crate::pkg::Manifest,
) -> Result<ModelLaunch, String> {
    resolve_model_launch_with(layout, app_id, manifest, crate::secrets::read_key)
}

/// `resolve_model_launch` 的可注入钥匙串版本（测试用）。
fn resolve_model_launch_with(
    layout: &DataLayout,
    app_id: &str,
    manifest: &crate::pkg::Manifest,
    lookup_key: impl Fn(&str) -> Option<String>,
) -> Result<ModelLaunch, String> {
    let custom = crate::providers::ProvidersStore::new(layout.providers_path()).list()?;
    let overrides =
        crate::model_overrides::OverridesStore::new(layout.model_overrides_path()).load()?;
    let eff = crate::model_overrides::resolve(
        Some(app_id),
        &overrides,
        manifest.superagent.model.as_deref(),
        |id| crate::providers::is_known(id, &custom),
    );
    Ok(crate::model_overrides::model_launch(
        &eff, &custom, lookup_key, true,
    ))
}

/// 退避重启用：重新解析模型选择（应用覆盖 / 全局默认 / 钥匙串里的最新密钥）、
/// 重写 agent home（settings.json / models.json），并**复用与首次启动同一个拼装函数**
/// `assemble_launch_plan` 重新算出完整启动计划——模型 env/args 与能力贡献的相对顺序、
/// 去重规则、沙盒读写白名单都与首次启动逐项一致（不再「基础参数 + 末尾追加模型参数」
/// 另写一份），且三个应用可写目录在每次重启时重新经 `private_dir` 校验：应用若在崩溃前
/// 把自己的目录换成了符号链接，这里直接 Err，不会带着被偷换的路径重新授权。
/// 首次启动沿用的 env 与参数是启动那一刻的快照，用户之后改了默认、换了密钥、删了自定义
/// 服务，崩溃重启就会带着过期值起会话，所以每次重启都要重新走一遍这条路径。
/// 解析失败（配置文件损坏）或目录校验失败返回 Err。
#[allow(clippy::too_many_arguments)]
fn relaunch_plan_with(
    layout: &DataLayout,
    app: &InstalledApp,
    manifest: &crate::pkg::Manifest,
    contribution: &LaunchContribution,
    hosttools_dir: &Path,
    sandboxed: bool,
    settings: &serde_json::Value,
    lookup_key: impl Fn(&str) -> Option<String>,
) -> Result<LaunchPlan, String> {
    let ml = resolve_model_launch_with(layout, &app.app_id, manifest, lookup_key)?;
    write_agent_home(layout, &app.app_id, settings, ml.models_json.as_ref())?;
    assemble_launch_plan(
        app,
        manifest,
        contribution,
        layout,
        hosttools_dir,
        sandboxed,
        &ml,
    )
}

/// 写 settings.json；`models_json` 为 Some 则写 models.json，None 则删除旧的 models.json
/// （NotFound 忽略）——防止撤销自定义 provider 后旧文件残留。models.json 里只有
/// `${环境变量名}` 引用，没有密钥字面值。
///
/// agent home 是应用可写的目录（应用能在里面放符号链接、甚至把整个目录换成链接），而本函数
/// 由宿主在沙盒外执行：目录先经 `DataLayout::private_dir` 校验，随后立刻以
/// `O_DIRECTORY|O_NOFOLLOW` 打开成目录句柄并核对身份（`open_agent_home`），之后临时文件创建、
/// 改名覆盖、删除全部相对这个句柄做（`dirfd::DirHandle`），校验之后路径再被换成链接也影响不到
/// 写入位置（C2 / I-a）。
fn write_agent_home(
    layout: &DataLayout,
    app_id: &str,
    settings: &serde_json::Value,
    models_json: Option<&serde_json::Value>,
) -> Result<(), String> {
    let home = open_agent_home(layout, app_id)?;
    write_agent_home_files(&home, settings, models_json)
}

/// 校验 agent home（`private_dir`）→ 记下身份 → 打开目录句柄并核对身份。
fn open_agent_home(layout: &DataLayout, app_id: &str) -> Result<crate::dirfd::DirHandle, String> {
    let path = layout.private_dir("agenthome", app_id)?;
    let id = crate::dirfd::identity_of_real_dir(&path)?;
    crate::dirfd::DirHandle::open_expecting(&path, id)
}

/// `write_agent_home` 在目录句柄里的写文件部分。
fn write_agent_home_files(
    home: &crate::dirfd::DirHandle,
    settings: &serde_json::Value,
    models_json: Option<&serde_json::Value>,
) -> Result<(), String> {
    home.write_file_replacing(
        "settings.json",
        serde_json::to_string_pretty(settings)
            .map_err(|e| e.to_string())?
            .as_bytes(),
    )?;
    match models_json {
        Some(v) => home.write_file_replacing(
            "models.json",
            serde_json::to_string_pretty(v)
                .map_err(|e| e.to_string())?
                .as_bytes(),
        ),
        // unlinkat 对符号链接只删链接本身。
        None => home.remove_file_if_exists("models.json"),
    }
}

/// 根据 `tool_execution_end` 的 `isError` 计算审计动词：`"allow"` = 工具真正执行成功
/// （能走到这个事件本身就说明没有在 gate/沙盒层被拒——那类拒绝根本不会产生此事件），
/// `"error"` = 工具执行了但自身报错（例如 bash 命令非零退出），不是权限拒绝。抽成纯
/// 函数只是为了可单测；真正调用点见 `open_app` 内 per-app 事件循环对
/// `PiEvent::ToolExecuted` 的处理。
pub(crate) fn audit_verdict_for_tool_execution(is_error: bool) -> &'static str {
    if is_error {
        "error"
    } else {
        "allow"
    }
}

/// pi 0.84.4 内建工具名全集，出处 `packages/coding-agent` 的 tools 目录——"pi 这个
/// 运行时认不认识这个工具名"。与 `SAFE_TOOLS` 分开维护、语义完全不同，不要合并：
/// `SAFE_TOOLS` 是宿主给未放宽（非 trusted 且非 sandboxed）应用的工具白名单（"能不能
/// 用"，见 `resolve_tools` 文档），这个常量只回答"pi 认识哪些工具名字"（"能不能装"，
/// 见 `lib.rs::known_tools` 文档、spec §8 裁决：声明 `bash` 的技能可以安装，只是对
/// 未放宽的应用不可授予）。`bash` 在这里，但不在 `SAFE_TOOLS` 里——这正是两个常量要
/// 分开维护的原因。
pub const PI_BUILTIN_TOOLS: &[&str] = &["read", "write", "edit", "ls", "grep", "find", "bash"];

/// P1 安全工具集：宿主自有 __host_ui_emit__ + 受 permission_gate 路径约束的文件工具；
/// **不含 bash**（其参数在 P1 无法被约束，是任意读 + 联网外泄通道）、不含任何联网工具。
const SAFE_TOOLS: &[&str] = &[
    "__host_ui_emit__",
    "read",
    "write",
    "edit",
    "ls",
    "grep",
    "find",
];

/// 解析该应用最终的 --tools 白名单。
/// - 第一方（trusted）：信任其声明；声明为空则给 SAFE_TOOLS。`sandboxed` 取值不影响。
/// - 第三方（untrusted）：
///   - `sandboxed == false`（非 L2 平台）：P1 白名单，声明与 SAFE_TOOLS 取交集
///     （永不放开 bash/联网——宿主没有 OS 级沙盒兜底，bash 的参数在 P1/无沙盒下
///     无法被约束，是任意读 + 联网外泄通道）；声明为空则给 SAFE_TOOLS。
///   - `sandboxed == true`（该 app 的 pi 确实被 `sandbox-exec` 包住）：放宽为声明
///     原样（可含 bash/联网工具）——OS 级沙盒是真正的边界，兜底 exfil，宿主侧
///     再裁剪意义不大。
///
/// 无论如何都确保 __host_ui_emit__ 在内（UI 推送必需）。
pub fn resolve_tools(declared: &[String], trusted: bool, sandboxed: bool) -> Vec<String> {
    // trusted 与「已被 OS 沙盒包住」都不需要宿主侧再裁剪工具集。
    let relaxed = trusted || sandboxed;
    let mut tools: Vec<String> = if declared.is_empty() {
        SAFE_TOOLS.iter().map(|s| s.to_string()).collect()
    } else if relaxed {
        declared.to_vec()
    } else {
        declared
            .iter()
            .filter(|t| SAFE_TOOLS.contains(&t.as_str()))
            .cloned()
            .collect()
    };
    if !tools.iter().any(|t| t == "__host_ui_emit__") {
        tools.push("__host_ui_emit__".into());
    }
    tools
}

/// 决定该次 spawn 实际用的 (bin, argv)：macOS 上把真实 pi 子进程关进 L2 沙盒
/// （design D1/D2/§6）——bin 固定为 `/usr/bin/sandbox-exec`，内层命令是
/// `<pi> --mode rpc <extra_args...>`，`deny_network = !trusted`（第三方受限应用
/// 禁网，可信/第一方应用放行全部出站——P2 无域名级过滤，`render_profile` 的
/// `!deny_network` 分支已实现放行）。
///
/// `read_paths`/`write_paths`（P6-A）：`filesystem` 能力清单声明并展开后的额外
/// 只读/可写目录（`$APP_DATA` 本身已经是隐含默认可写区，不出现在这两个集合里）
/// ——原样转给 `build_profile` 的同名参数。调用方（`spawn_app_session`）现在传的
/// 是 `LaunchPlan.sandbox_read`/`sandbox_write`——`assemble_launch_plan` 把
/// `capabilities::filesystem::FilesystemCapability::launch` 经
/// `CapabilityRegistry::launch` 算出的 `LaunchContribution.sandbox_read`/
/// `sandbox_write` 原样透传到这里（见 `assemble_launch_plan`/`open_app_after_acquire`/
/// `run_headless_session` 三处调用点），不再是硬编码的空 `vec![]`。
///
/// `runtime_paths` 则**不**是空——`crate::pi_bin::runtime_install_dirs()` 解析出
/// 真实 pi/node 自身的安装前缀（nvm 布局下是 `versions/node/vX/`），喂给
/// `build_profile` 的 `runtime_paths` 换来 read+exec 放行。这是本任务修的
/// crux：没有这条，`BASE_PROFILE` 只覆盖系统只读路径，真实 pi/node 装在
/// `~/.nvm/...` 这类用户目录下，`execvp` 自身就先 EPERM（见
/// `.superpowers/sdd/task-5-report.md` §6 记录的手工探测）。
/// `mcp_socket`（Task 9c）：`Some(path)` 时——调用方（`spawn_app_session`）只应在
/// 确认 `CapabilityRegistry::launch` 算出的贡献真的 `needs_socket`（且监听器 bind
/// 成功——交互会话见 `open_app_after_acquire` 里 `injected_mcp_socket` 的赋值点；
/// headless 会话见 `run_headless_session` 里 `mcp_socket_for_sandbox` 的赋值点）
/// 时才传 `Some`——把该 app 自己的 MCP unix socket 路径喂给 `build_profile`，换来
/// 一条窄放行：这个 `deny_network=true` 的受限 profile 下，pi 子进程仍然能连接
/// 它自己的这一个 socket（否则宿主 MCP/桥对受限第三方 app 完全不可达）。`None`
/// 时不放行任何 socket。
/// `pub`（非 `spawn_app_session`/`open_app_after_acquire` 内部调用所必需的可见性）
/// 是刻意的：Task12 `tests/scheduler_it.rs` 需要从集成测试里直接调用**这一个**
/// 生产函数本身，断言其返回的 `bin` 字面是 `"/usr/bin/sandbox-exec"`，来证明
/// task-mode（headless 定时任务）拉起复用的是与交互会话完全同一条 P2 沙盒包裹
/// 路径——而不是重新推导一遍等价逻辑再自我验证。与 `sandbox::build_profile`/
/// `sandbox::sandbox_exec_argv` 已经是 `pub`（供 `sandbox_pipe_it.rs`/
/// `sandbox_escape_it.rs` 直接调用）同一个理由。
#[cfg(target_os = "macos")]
pub fn sandboxed_argv(
    app_data_dir: &Path,
    trusted: bool,
    extra_args: &[String],
    mcp_socket: Option<&Path>,
    read_paths: &[std::path::PathBuf],
    write_paths: &[std::path::PathBuf],
) -> Result<(String, Vec<String>), String> {
    let runtime_paths = crate::pi_bin::runtime_install_dirs()?;
    let sp = crate::sandbox::build_profile(
        app_data_dir,
        read_paths,
        write_paths,
        &runtime_paths,
        !trusted,
        mcp_socket,
    )?;
    let mut inner = vec![crate::pi_bin::resolve_pi_bin()
        .to_string_lossy()
        .to_string()];
    inner.push("--mode".into());
    inner.push("rpc".into());
    inner.extend(extra_args.iter().cloned());
    Ok((
        "/usr/bin/sandbox-exec".to_string(),
        crate::sandbox::sandbox_exec_argv(&sp, &inner),
    ))
}

/// 起该应用的 pi 子进程：macOS 经 `sandbox-exec` 包一层 L2 沙盒（`sandboxed_argv`）；
/// 其余平台（L2 不可用，design §5/§10）原样调用 `RpcSession::spawn_with`——P1 行为，
/// 直接起裸 pi，不沙盒。
///
/// `open_app` 的首次启动与看护循环的退避重启都必须走同一个函数，否则会出现
/// “崩溃重启一次就跑到沙盒外”的口子——这也是为什么这里单独抽出来而不是把
/// sandbox-exec 包装逻辑写死在 `open_app_after_acquire` 里一次性调用。
///
/// 下面两个分支的选择必须是编译期 `#[cfg]`（而非运行时 `if`）：`sandboxed_argv`
/// 用到 macOS-only API，非 macOS 编译单元里这些符号根本不存在，没法在运行时
/// 二选一。但「选哪个分支」这件事在语义上仍然必须和 `sandboxing_available()`
/// ——本文件 THE single source of truth——保持一致，不能各自独立判断；每个分支
/// 内的 `debug_assert!` 就是把这层语义耦合钉在代码里，防止未来有人只改
/// `sandboxing_available()` 或只改这里其中一个分支就让两者悄悄分裂。
///
/// `#[allow(clippy::too_many_arguments)]`：P6-A 加了 `read_paths`/`write_paths`
/// 两个参数（凑到 8 个，过 clippy 默认阈值 7）——本任务的定位是纯签名穿线
/// （见任务说明"session_mgr.rs 改动只是签名穿线"），把这些参数捆成一个 struct
/// 属于会动到调用方语义的重构，留给之后专门整理调用惯例的任务，这里不顺手做。
#[allow(clippy::too_many_arguments)]
async fn spawn_app_session(
    session_dir: &Path,
    app_data_dir: &Path,
    trusted: bool,
    env: Vec<(String, String)>,
    extra_args: Vec<String>,
    mcp_socket: Option<std::path::PathBuf>,
    read_paths: Vec<std::path::PathBuf>,
    write_paths: Vec<std::path::PathBuf>,
) -> Result<
    (
        crate::rpc::RpcSession,
        tokio::sync::mpsc::Receiver<crate::rpc::PiEvent>,
    ),
    String,
> {
    #[cfg(target_os = "macos")]
    {
        debug_assert!(
            sandboxing_available(),
            "macos 分支必须与 sandboxing_available() 一致"
        );
        let (bin, argv) = sandboxed_argv(
            app_data_dir,
            trusted,
            &extra_args,
            mcp_socket.as_deref(),
            &read_paths,
            &write_paths,
        )?;
        // cwd = $APP_DATA（Task 5 已把它做成可读+可写的 subpath）：sandbox-exec 子进程
        // 若不显式指定 cwd，会继承宿主进程当前工作目录——那多半不在沙盒的读白名单内，
        // node 启动时 `uv_cwd()`（本质是 getcwd()）就会 EPERM，整个进程直接崩在启动期
        // （见 task-5-report.md §6 手工探测记录）。$APP_DATA 已经是沙盒内确定可读的路径，
        // 拿它当 cwd 是最省事的修法，不需要额外放行别的目录。
        crate::rpc::RpcSession::spawn_wrapped(&bin, argv, session_dir, env, Some(app_data_dir))
            .await
    }
    #[cfg(not(target_os = "macos"))]
    {
        debug_assert!(
            !sandboxing_available(),
            "非 macos 分支必须与 sandboxing_available() 一致"
        );
        let _ = (app_data_dir, trusted, mcp_socket, read_paths, write_paths); // 非 macOS：L2 不可用，原样走 P1，未用到的参数避免警告
        crate::rpc::RpcSession::spawn_with(session_dir, env, extra_args).await
    }
}

/// 打开（或复用已打开的）应用会话：
/// 1. 若该应用已有活跃会话，幂等返回其已分配的界面槽位（不重复占用并发闸门名额）；
/// 2. 否则先占并发闸门名额（满则拒绝），写受限 `settings.json`，拼装启动计划
///    （含读清单追加的 `--model`/`--tools`），spawn 该应用自己的 pi 子进程；
/// 3. 分配一个界面槽位，并起一个 per-app 事件转发 + 退避重启循环
///    （事件名带 `:<appId>` 后缀，仿 P0 `start_main_session`）。
///
/// gate 名额一旦 `try_acquire()` 成功，第 2/3 步中任何一步失败都必须把 gate 与
/// （若已分配）界面槽位还回去，否则该 app_id 的一次打字错误/未安装等错误就会
/// 永久占掉一个并发名额；见 `open_app_after_acquire` 与下方的错误路径清理。
pub async fn open_app(app: tauri::AppHandle, app_id: String) -> Result<usize, String> {
    let state = app.state::<crate::app_state::AppState>();
    {
        // gate 与 app_sessions/slots 短暂嵌套加锁（仅判断 + try_acquire，无 .await
        // 跨锁持有的阻塞操作），与 close_app 的顺序加锁不构成循环等待。
        let mut gate = state.gate.lock().await;
        if state.app_sessions.lock().await.contains_key(&app_id) {
            // 幂等提前返回：应用已打开时直接复用其槽位，不重复占用并发闸门名额。
            // 注意：此判断与下面的 try_acquire 合起来并不是一次原子操作——两个并发的
            // open_app(同一 app_id) 理论上都可能先后通过这里的 contains_key 检查（都
            // 还没插入 app_sessions），再各自 acquire+spawn，造成重复消耗闸门名额与
            // 孤儿子进程。这是 P1 单用户场景下的已知 Minor，留给 P2 硬化处理；不在此
            // 引入跨 await 的加锁去堵它，避免与 close_app 产生新的锁序风险。
            return state
                .slots
                .lock()
                .unwrap()
                .slot_for_app(&app_id)
                .ok_or_else(|| "会话在但无槽位".to_string());
        }
        if !gate.try_acquire() {
            return Err("同时打开的应用已达上限，请先关闭一个".into());
        }
    }

    // gate 已 acquire；后续任一步失败都必须释放 gate + 已占槽位，否则并发名额永久泄漏
    // （原实现在这之后用 `?` 提前返回，会跳过 gate.release()）。
    match open_app_after_acquire(&app, &app_id).await {
        Ok(slot) => Ok(slot),
        Err(e) => {
            state.slots.lock().unwrap().release_app(&app_id); // 未分配槽位时是 no-op
            state.gate.lock().await.release();
            Err(e)
        }
    }
}

/// `open_app` 在成功 `try_acquire()` 之后的全部可失败工作：读注册表、写受限
/// settings.json、拼装启动计划、spawn 子进程、分配界面槽位、起 per-app 转发任务。
/// 任一步出错都只需把 `Err` 原样传回 `open_app`，由它统一做 gate/槽位回收，
/// 这里本身不做任何 gate/槽位清理。
async fn open_app_after_acquire(app: &tauri::AppHandle, app_id: &str) -> Result<usize, String> {
    let state = app.state::<crate::app_state::AppState>();
    let app_id = app_id.to_string();

    let root = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let layout = DataLayout::new(root);
    let reg = RegistryStore::new(layout.registry_path());
    let record = reg.get(&app_id).ok_or("应用未安装")?;

    // CRITICAL SECURITY INVARIANT：`sandboxed` 必须和 `spawn_app_session` 里决定
    // 是否真的把 pi 包进 `/usr/bin/sandbox-exec` 的判断永远一致——这里不再自己
    // 重新写一份 `cfg!(target_os = "macos")`，而是调用 `sandboxing_available()`
    // （定义见文件上方，THE single source of truth，其文档注释里详述了两处一旦
    // 独立漂移会造成的失败模式）。两处都从同一个函数取值，就不可能出现
    // `sandboxed=true` 但 pi 实际未被 OS 沙盒包住的情况：untrusted 第三方代码
    // 会被 `build_settings_json`/`resolve_tools` 当作「有沙盒兜底」放行
    // extensions + bash/联网工具，而真实进程完全没有 OS 边界——这正是 P1 用
    // `extensions:[]` + SAFE_TOOLS 白名单要堵死的那个数据外泄口子。
    //
    // 注（Minor）：这里只按平台判断，没有再与「该 app 是否声明要走沙盒」相与
    // （P2 spec 设想的 `<该 app 走沙盒>` per-app 那个 conjunct）——是有意省略，
    // 不是漏写：当前 macOS 上每一个 app 都被无条件包进 sandbox-exec（见
    // `spawn_app_session`），没有 per-app 级别的开关可关；等以后真的加了
    // per-app 跳过沙盒的调试开关，才需要把它接进 `sandboxing_available()`
    // 或在这里另行相与。
    let sandboxed = sandboxing_available();

    // hosttools 目录走真实资源解析（生产打包内为 resource_dir()/hosttools，dev
    // 回退到仓库内 src-tauri/hosttools）。
    let hosttools = crate::hosttools_dir(app);

    // P6-A：读该 app 的清单 + 权限——`?` 直接失败，不再像迁移前那样静默跳过整段
    // 启动计划拼装（旧 `if let Ok(m) = ...`）。安装期（`maker::resolve_install`/
    // `install_builtin_sample_core` 等）已经跑过 `pkg::load_and_validate` +
    // `permissions::load` 校验——一个已经在 registry 里的 app_id 走到这里时若清单/
    // 权限文件读不出来，只可能是包在磁盘上被破坏/被外部篡改；"假装它什么权限都
    // 没有、继续拼一个残缺的启动计划"比直接报错更危险（会用一份没有任何能力贡献、
    // 但清单其实可能声明了 connectors/agents.call 的"错误的最小权限"启动，而不是
    // 让用户看到一个明确的"这个应用坏了"错误）。
    let manifest =
        crate::pkg::load_and_validate(&layout.packages_dir(&app_id)).map_err(|e| e.to_string())?;
    let perms = crate::permissions::load(
        &layout.packages_dir(&app_id),
        &manifest.superagent.permissions,
    )?;

    // 写受限 settings.json（packages 只含该应用自己一个包；sandboxed 决定第三方
    // 是否加载 extensions，见 build_settings_json 文档）与按所选 provider 生成的
    // models.json（清单要先读到才能算出有效模型，所以挪到这里）。
    let model_launch = resolve_model_launch(&layout, &app_id, &manifest)?;
    write_agent_home(
        &layout,
        &app_id,
        &build_settings_json(&record, &layout, sandboxed),
        model_launch.models_json.as_ref(),
    )?;
    layout.ensure_app(&app_id).map_err(|e| e.to_string())?;

    let identity = CallerIdentity {
        app_id: app_id.clone(),
        trusted: record.trusted,
        depth: 0,
    };
    let socket_path = layout.mcp_socket_path(&app_id);
    let ctx = LaunchCtx {
        app_id: &app_id,
        trusted: record.trusted,
        sandboxed,
        // F1（review）：这是三条真实 spawn 路径之一——真的要拉起子进程，`filesystem`
        // 能力的 `launch` 必须真的把清单声明的写目录落地（否则子进程启动后第一次
        // 写文件就会因为目录不存在而失败）。见 `LaunchCtx::materialize` 文档。
        materialize: true,
        layout: &layout,
        hosttools_dir: &hosttools,
        socket_path: &socket_path,
        mcp: &state.mcp,
    };
    // P6-A：`--tools`/`--model`/各宿主桥/env 全部由注册表统一算出
    // （`CapabilityRegistry::launch` 遍历全部内置能力，merge 各自 `declared()` 为真
    // 的那些贡献，见 `capability.rs` 文档）——不再一个个手写 mcp/maker/call/router/
    // notify 的判断分支。
    let mut contribution = state.capabilities.launch(&perms, &identity, &ctx)?;

    // Task9c：只有当下面的 MCP 监听器真的 bind 成功才置为 Some，喂给
    // spawn_app_session -> sandboxed_argv -> build_profile，换来"受限 pi 能连自己
    // 的 MCP socket"这一条窄放行；没有任何依赖 socket 的贡献时保持 None。
    let mut injected_mcp_socket: Option<PathBuf> = None;
    if contribution.needs_socket {
        // P3 Task9b 不变式：`SUPERAGENT_MCP_SOCKET` 指向的路径必须真的有宿主端在
        // 监听，否则任何桥的每次调用都会连接失败——"注入桥/env"和"起监听器"必须
        // 绑在一起，以监听器 bind 成功为准。绑定该 app 完整身份（`record.trusted`）
        // 与完整清单权限（`perms.clone()`，不只 `connectors`），以及进程级唯一的
        // `state.capabilities`（P6-A 各能力如 `notifications` 的限速窗口依赖单实
        // 例，不能让每个监听器各自新建一份，见 `AppState::capabilities` 文档）。
        // `perms` 传 `.clone()` 而非按值移入：下面 `register_scheduled_tasks_if_permitted`
        // 还要读 `perms.system.schedule`/`&perms.scheduled_tasks`。
        match crate::mcp_socket::McpSocketListener::start_with_identity(
            state.mcp.clone(),
            layout.clone(),
            identity.clone(),
            perms.clone(),
            socket_path.clone(),
            Some(hosttools.clone()),
            state.capabilities.clone(),
        ) {
            Ok(mcp_listener) => {
                state
                    .mcp_sockets
                    .lock()
                    .await
                    .insert(app_id.clone(), mcp_listener);
                injected_mcp_socket = Some(socket_path.clone());
            }
            Err(e) => {
                // P3 不变式：绝不注入一个指向"没人监听"的 socket 的桥——bind 失败
                // 就把这次贡献里所有依赖 socket 的部分（桥/env/tools/needs_socket）
                // 整体丢弃，只保留与 socket 无关的 `sandbox_read`/`sandbox_write`
                // （`filesystem` 能力的贡献，不依赖 MCP socket）。
                eprintln!(
                    "app {app_id} 的 MCP socket 监听器启动失败，本次启动不注入任何依赖 socket 的桥/工具：{e}"
                );
                contribution = LaunchContribution {
                    sandbox_read: contribution.sandbox_read,
                    sandbox_write: contribution.sandbox_write,
                    ..Default::default()
                };
            }
        }
    }

    let plan = assemble_launch_plan(
        &record,
        &manifest,
        &contribution,
        &layout,
        &hosttools,
        sandboxed,
        &model_launch,
    )?;

    // Task14b：按 system.schedule 权限门控，把该 app 清单声明的 scheduledTasks
    // 登记进 TaskRegistry（补上 Task10 register()/Task13 run_catch_up_for_app() 之间
    // 此前缺失的接线，见 `scheduler::register_scheduled_tasks_if_permitted` 文档）。
    // 必须放在下面 tokio::spawn 里的 run_catch_up_for_app 之前——否则那次补跑会
    // 因为 registry 里还没有这个 app 的任务而查不到任何到期任务。
    if let Err(e) = crate::scheduler::register_scheduled_tasks_if_permitted(
        &layout,
        &app_id,
        perms.system.schedule,
        &perms.scheduled_tasks,
    ) {
        eprintln!("app {app_id} 的定时任务注册失败，跳过：{e}");
    }

    // 沙盒外宿主取得的、已校验为真实目录的规范化字面路径（C1）。
    let session_dir = layout.private_dir("sessions", &app_id)?;
    let app_data_dir = layout.private_dir("apps", &app_id)?;
    // Task9b：若上面已经为该 app 起了 MCP socket 监听器，这里任何一步失败都必须
    // 把它一并 stop() 掉（中止 accept 循环 + 删 socket 文件）——否则一次 spawn
    // 失败/无空闲槽位就会在磁盘上留一个没有对应存活会话、却仍在监听的孤儿 socket。
    let (session, rx) = match spawn_app_session(
        &session_dir,
        &app_data_dir,
        record.trusted,
        plan.env.clone(),
        plan.extra_args.clone(),
        injected_mcp_socket.clone(),
        plan.sandbox_read.clone(),
        plan.sandbox_write.clone(),
    )
    .await
    {
        Ok(pair) => pair,
        Err(e) => {
            if let Some(listener) = state.mcp_sockets.lock().await.remove(&app_id) {
                listener.stop().await;
            }
            return Err(e);
        }
    };

    // 先分配界面槽位、分配成功后才登记进 app_sessions：若槽位分配失败，直接 kill 掉
    // 刚 spawn 出来的子进程并原样返回错误，绝不让它以“已登记但未分配槽位”的状态留在
    // app_sessions 里——否则该 app_id 会被上面的幂等检查判定为“已打开”而卡死，且
    // 之后 close_app 对它做清理时会对 gate 多做一次 release（等于凭空多放出一个
    // 并发名额），比不释放更糟。
    //
    // 注意：`assign` 的结果先落到独立的 `let` 语句里，不直接放进 `match` 的
    // 判断表达式——`match <expr> { ... }` 里 `<expr>` 中产生的临时对象（这里是
    // std::sync::MutexGuard）生命周期会延伸到整个 match 语句结束，若直接
    // `match state.slots.lock().unwrap().assign(...) { None => { ... .await ... } }`
    // 就会在 `session.kill().await` 期间仍持有这把同步锁的 guard——而
    // std::sync::MutexGuard 不是 Send，跨 `.await` 悬挂点持有会让这个异步任务
    // 无法编译（或不可发送）。改成先落 `let` 绑定，guard 在该语句结束时就已释放。
    let assigned_slot = state.slots.lock().unwrap().assign(&app_id);
    let slot = match assigned_slot {
        Some(slot) => slot,
        None => {
            let mut session = session;
            session.kill().await;
            if let Some(listener) = state.mcp_sockets.lock().await.remove(&app_id) {
                listener.stop().await;
            }
            return Err("无空闲界面槽位".into());
        }
    };
    // P6-D Task5：新会话的用量从零开始（总量与按模型拆分一起清零）；放在事件循环
    // 启动之前，避免清零晚于首条用量事件。
    state.usage.reset_app(&app_id).await;
    state
        .app_sessions
        .lock()
        .await
        .insert(app_id.clone(), session);
    state.activity.on_open(&app_id, crate::idle::now_secs());

    // Task13：应用打开后台触发一次错过任务补跑（P3 §10）——只补这一个刚打开
    // 的 app 的任务（`run_catch_up_for_app` 内部按 app_id 过滤），其余 app 此刻
    // 未运行，各自的补跑留给各自下次被打开时触发。包进独立的 `tokio::spawn`
    // 而不是内联 `.await`：补跑可能触发若干次真实 LLM 调用（task-mode
    // headless 会话），耗时不可控，绝不能拖慢本函数把已打开的界面槽位交还
    // 调用方——用户能感知到的"应用已打开"不该被后台补跑卡住。结果目前丢弃
    // 不做任何事：与 `Scheduler::tick` 同一个 `Vec<TaskSessionResult>` 形状，
    // 是留给 Task15 通知中心的最小 seam（见该类型文档），本任务不建通知 UI。
    {
        let layout = layout.clone();
        let hosttools = hosttools.clone();
        let record = record.clone();
        // Task17b：`state.mcp` 与本函数上面 `state.capabilities.launch(&perms, ...)`
        // 用的是同一个连接池实例（clone 只是浅拷贝内部 Arc，见 `McpManager` 文档）——
        // 补跑触发的 task-mode 会话与刚起的前台会话共享同一份已连接 server 状态，不会
        // 产生第二套互不知情的 MCP 连接。
        let mcp = state.mcp.clone();
        tokio::spawn(async move {
            let clock = crate::scheduler::SystemClock;
            let _ =
                crate::scheduler::run_catch_up_for_app(&layout, &hosttools, &mcp, &record, &clock)
                    .await;
        });
    }

    // per-app 事件转发 + 退避重启看护（仿 P0 start_main_session）：
    // 事件名统一带 `:<appId>` 后缀，UiEmit → `ui-emit:<appId>`。
    let id = app_id.clone();
    // 重启不复用首次启动的快照：每次重启由 `relaunch_plan_with` 重新解析模型选择、重写
    // agent home，并用与首次启动同一个 `assemble_launch_plan` 重新拼装完整启动计划。
    let restart_settings = build_settings_json(&record, &layout, sandboxed);
    let restart_manifest = manifest;
    let restart_contribution = contribution;
    let restart_record = record.clone();
    let restart_hosttools = hosttools.clone();
    let restart_layout = layout.clone();
    let restart_trusted = record.trusted;
    let restart_mcp_socket = injected_mcp_socket;
    let app = app.clone();
    // `layout` 从这里开始不再被本函数其余部分使用，整个移入下面的事件循环闭包——
    // 只用于 PiEvent::ToolExecuted 分支调用 audit::record（见该分支注释）。
    tokio::spawn(async move {
        use crate::rpc::PiEvent;
        let mut backoff = crate::supervisor::Backoff::new();
        let mut rx = rx;
        loop {
            while let Some(ev) = rx.recv().await {
                // P6-F：任何事件都算活动（不依赖变体，新增变体自动覆盖）。
                app.state::<crate::app_state::AppState>()
                    .activity
                    .touch(&id, crate::idle::now_secs());
                match ev {
                    PiEvent::AssistantDelta(d) => {
                        let _ = app.emit(&format!("assistant-delta:{id}"), d);
                    }
                    PiEvent::AgentEnded => {
                        app.state::<crate::app_state::AppState>()
                            .activity
                            .end_turn(&id, crate::idle::now_secs());
                        let _ = app.emit(&format!("assistant-done:{id}"), ());
                        // P3 Task18 修复：每轮结束后查一次该 app 会话的累计用量
                        // （`PiEvent::SessionStats` 分支据此更新 `usage`）。fire-and-forget——
                        // 写 stdin 失败（会话正在重启/已挂）不应影响本轮已经正常完成的
                        // assistant-done 通知。
                        if let Some(session) = app
                            .state::<crate::app_state::AppState>()
                            .app_sessions
                            .lock()
                            .await
                            .get(&id)
                        {
                            let _ = session.send_get_session_stats().await;
                        }
                    }
                    PiEvent::UiEmit { event, payload } => {
                        let _ = app.emit(
                            &format!("ui-emit:{id}"),
                            serde_json::json!({ "event": event, "payload": payload }),
                        );
                    }
                    PiEvent::ProviderError(msg) => {
                        let verdict = crate::byok::classify_error(&msg);
                        if let Some((kind, m)) = crate::byok::frontend_payload(&verdict) {
                            let _ = app.emit(
                                &format!("agent-error:{id}"),
                                serde_json::json!({ "kind": kind, "message": m }),
                            );
                        }
                    }
                    PiEvent::AutoRetry {
                        attempt,
                        max,
                        delay_ms,
                    } => {
                        let _ = app.emit(
                            &format!("retry-status:{id}"),
                            serde_json::json!({ "attempt": attempt, "max": max, "delayMs": delay_ms }),
                        );
                    }
                    // P2 审计接线：tool_name/args 直接来自 pi 核心自产的 tool_execution_end
                    // 事件本体（rpc::classify），不经过、不依赖 hosttools/permission_gate.ts
                    // 的 tool_call 钩子上报——理由见 PiEvent::ToolExecuted 的文档注释
                    // （该钩子的批准对最终执行参数不构成任何保证）。best-effort：审计写入
                    // 失败不应影响该应用会话本身的运行。
                    PiEvent::ToolExecuted {
                        tool_name,
                        args,
                        is_error,
                    } => {
                        let verdict = audit_verdict_for_tool_execution(is_error);
                        let _ = crate::audit::record(
                            &layout,
                            &id,
                            &tool_name,
                            &args.to_string(),
                            verdict,
                        );
                    }
                    // P3 Task18 修复：对上面 AgentEnded 分支发出的 get_session_stats
                    // 查询的响应，记录该 app 会话的最新累计用量，供 `app_usage` 命令
                    // 查询（见 `usage::UsageAccumulator`/`rpc::PiEvent::SessionStats`
                    // 文档）。`set_latest` 是覆盖不是累加——不需要在这里补发
                    // assistant-done，AgentEnded 现在正常触发，不会被这个分支抢走分类。
                    PiEvent::SessionStats {
                        input,
                        output,
                        cost,
                    } => {
                        app.state::<crate::app_state::AppState>()
                            .usage
                            .set_latest(&id, input, output, cost)
                            .await;
                    }
                    // P6-D Task5：按 (provider, model) 累加（每条 message_end 是不同的消息）。
                    PiEvent::AssistantUsage {
                        provider,
                        model,
                        input,
                        output,
                        cost,
                    } => {
                        app.state::<crate::app_state::AppState>()
                            .usage
                            .add_message(&id, &provider, &model, input, output, cost)
                            .await;
                    }
                    PiEvent::Other(_) => {}
                }
            }
            // rx 关闭 = 该应用的 pi 子进程退出。若已被 close_app 主动关闭（app_sessions
            // 中已移除该 app_id），说明是正常关闭，不再重启，直接结束转发任务；
            // 否则视为异常退出，按退避策略尝试重启。
            let still_open = app
                .state::<crate::app_state::AppState>()
                .app_sessions
                .lock()
                .await
                .contains_key(&id);
            if !still_open {
                break;
            }
            match backoff.next_delay() {
                Some(delay) => {
                    tokio::time::sleep(delay).await;
                    // 每次重启都重新校验三个应用可写目录（C1）并重新拼装完整计划；任何一步
                    // 失败都不拉起、不带旧快照硬起，走与 spawn 失败相同的退避路径并通知界面。
                    let relaunch = relaunch_plan_with(
                        &restart_layout,
                        &restart_record,
                        &restart_manifest,
                        &restart_contribution,
                        &restart_hosttools,
                        sandboxed,
                        &restart_settings,
                        crate::secrets::read_key,
                    )
                    .and_then(|plan| {
                        let sd = restart_layout.private_dir("sessions", &id)?;
                        let ad = restart_layout.private_dir("apps", &id)?;
                        Ok((plan, sd, ad))
                    });
                    let (plan, restart_session_dir, restart_app_data_dir) = match relaunch {
                        Ok(v) => v,
                        Err(e) => {
                            let _ = app.emit(
                                &format!("agent-error:{id}"),
                                serde_json::json!({
                                    "kind": "model_config",
                                    "message": format!("应用 {id} 重启失败（模型配置或私有目录校验未通过）：{e}")
                                }),
                            );
                            continue;
                        }
                    };
                    match spawn_app_session(
                        &restart_session_dir,
                        &restart_app_data_dir,
                        restart_trusted,
                        plan.env,
                        plan.extra_args,
                        restart_mcp_socket.clone(),
                        plan.sandbox_read,
                        plan.sandbox_write,
                    )
                    .await
                    {
                        Ok((s, new_rx)) => {
                            // 重启后是新的 pi 会话（累计值从零起），两份用量一起清零。
                            app.state::<crate::app_state::AppState>()
                                .usage
                                .reset_app(&id)
                                .await;
                            app.state::<crate::app_state::AppState>()
                                .app_sessions
                                .lock()
                                .await
                                .insert(id.clone(), s);
                            app.state::<crate::app_state::AppState>()
                                .activity
                                .on_restart(&id, crate::idle::now_secs());
                            rx = new_rx;
                            backoff.reset();
                        }
                        Err(_) => continue,
                    }
                }
                None => {
                    let _ = app.emit(
                        &format!("agent-error:{id}"),
                        serde_json::json!({
                            "kind": "faulted",
                            "message": format!("应用 {id} 多次异常退出，请检查配置")
                        }),
                    );
                    break;
                }
            }
        }
    });
    Ok(slot)
}

/// 关闭应用会话：kill 该应用的 pi 子进程、移除会话记录，并仅在确实移除到了一个
/// 存活会话时才释放界面槽位与并发闸门名额——对一个从未打开/已关闭的 app_id 调用
/// close_app 绝不能去释放别的存活应用正占用着的 gate 名额。
///
/// 注意：`app_sessions` 的锁只在 `remove` 这一条语句内持有（不跨 `s.kill().await`），
/// 避免像 `if let Some(mut s) = state.app_sessions.lock().await.remove(&app_id) { s.kill().await; }`
/// 那样把整个 if-let 语句块的作用域内都持有 MutexGuard（Rust 的 if-let 场景值临时对象
/// 生命周期会延伸到整个语句块），从而在 kill 期间不必要地阻塞其他任务对 app_sessions 的加锁。
pub async fn close_app_in(state: &crate::app_state::AppState, app_id: &str) -> bool {
    let removed = state.app_sessions.lock().await.remove(app_id);
    if let Some(mut s) = removed {
        // 活动记录必须在摘除会话之后、第一次 await 之前同步删掉：后面的 kill / 停监听
        // 都会让出执行权，用户可能在这个窗口里重开同一应用，那时新会话的记录不能被误删。
        state.activity.on_close(app_id);
        s.kill().await;
        // Task9b：该 app 若有 MCP socket 监听器在跑，一并停掉（中止 accept 循环 +
        // 删 socket 文件）——同 app_sessions 的 per-app 资源生命周期模式，放在
        // Some 分支里同样是为了"只对确实关掉了一个存活会话的 app_id 做清理"。
        if let Some(listener) = state.mcp_sockets.lock().await.remove(app_id) {
            listener.stop().await;
        }
        // slots.release_app 本身是无副作用的扫描型 no-op（该 app_id 未占槽位时安全），
        // 放进 Some 分支只是为了和 gate.release() 保持对称：两者都只应在“确实移除到
        // 了一个会话”时才发生，避免对一个根本没打开过的 app_id 释放别人的名额。
        state.slots.lock().unwrap().release_app(app_id);
        state.gate.lock().await.release();
        true
    } else {
        false
    }
}

/// 关闭应用会话（命令入口）：`close_app_in` 的薄封装，一切关闭（含空闲回收）都走它。
pub async fn close_app(app: tauri::AppHandle, app_id: String) -> Result<(), String> {
    let state = app.state::<crate::app_state::AppState>();
    close_app_in(&state, &app_id).await;
    Ok(())
}

/// P6-C Task4：把一条批准/拒绝暂存写调用的回执文案回送进 `app_id` 仍然活着的
/// 发起会话（`notifications::NotificationStore::respond_staged` 的 `deliver`
/// 回调最终落到这里）——用 pi RPC 的 `steer` 命令（`RpcSession::send_steer`），
/// 不打断该会话正在处理的其它事情，也不像 `send_prompt` 那样另起一轮新对话。
///
/// 返回是否真的投递成功：`app_sessions` 里没有这个 `app_id`（会话已关闭/从未
/// 打开）或 `send_steer` 本身失败（子进程已死但尚未被 `close_app` 清理等）都
/// 算"未投递"，返回 `false`——调用方据此决定要不要落一条 `update` 通知兜底
/// （见 Global Constraints"回送失败不影响执行结果与审计"）。不返回 `Err`：
/// 投递失败本就是这个函数要处理的正常分支之一，不是异常。
pub async fn steer_app_session(
    state: &crate::app_state::AppState,
    app_id: &str,
    text: &str,
) -> bool {
    let guard = state.app_sessions.lock().await;
    match guard.get(app_id) {
        Some(session) => {
            let ok = session.send_steer(text).await.is_ok();
            if ok {
                state.activity.touch(app_id, crate::idle::now_secs());
            }
            ok
        }
        None => false,
    }
}

// ---------------------------------------------------------------------------
// Task12：headless task-mode 会话——定时任务到点后台拉起
// ---------------------------------------------------------------------------

/// 一个正在运行的后台（headless）会话：定时任务或被调方会话。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HeadlessSession {
    pub app_id: String,
    pub pid: Option<u32>,
    /// 登记时刻（unix 秒）。磁盘清理据此判断哪些会话文件属于这个仍在运行的会话。
    pub started_at: i64,
}

static HEADLESS: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashMap<u64, HeadlessSession>>,
> = std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::HashMap::new()));
static HEADLESS_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// RAII 登记：构造时写入登记表，Drop 时移除。`run_headless_session` 是定时任务与
/// 被调方会话的唯一生产点，登记放在这里（进程级静态表，`spawn_call_session` 签名不动）。
#[doc(hidden)]
pub struct HeadlessGuard(u64);

impl HeadlessGuard {
    #[doc(hidden)]
    pub fn register(app_id: &str, pid: Option<u32>) -> Self {
        let id = HEADLESS_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        HEADLESS.lock().unwrap_or_else(|e| e.into_inner()).insert(
            id,
            HeadlessSession {
                app_id: app_id.to_string(),
                pid,
                started_at: crate::idle::now_secs(),
            },
        );
        Self(id)
    }
}

impl Drop for HeadlessGuard {
    fn drop(&mut self) {
        HEADLESS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.0);
    }
}

/// 当前正在运行的后台会话（按 app_id、pid 排序）。
pub fn running_headless_sessions() -> Vec<HeadlessSession> {
    let mut v: Vec<HeadlessSession> = HEADLESS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .values()
        .cloned()
        .collect();
    v.sort_by(|a, b| (&a.app_id, a.pid).cmp(&(&b.app_id, b.pid)));
    v
}

/// `spawn_task_session` 的产出：headless task-mode 会话跑到 `agent_end` 后的
/// 结果——拼接的最终助手文本 + 这次会话过程中是否出现过供应商侧错误。
///
/// 这是本任务与 Task15（通知中心，尚未建）之间的最小 seam：`scheduler::Scheduler
/// ::tick` 把每条到期任务的这个结果原样收集进一个 `Vec` 作为返回值交给调用方；
/// 未来 Task15 只需把这个 `Vec` 逐条转成 `Notification{kind: task_result, ...}`
/// 写进 `NotificationStore`——本任务不建通知 UI/存储，只把"结果"这个值稳定地
/// 产出到调用方手里。
#[derive(Debug, Clone, PartialEq)]
pub struct TaskSessionResult {
    pub app_id: String,
    pub task_id: String,
    /// 该次会话产生的全部 `AssistantDelta` 拼接文本（可能为空——任务型 prompt
    /// 也可能只触发工具调用、不产出可读文本）。若拉起本身失败（`spawn_app_session`
    /// 返回 `Err`），这里承载错误信息，供通知中心至少能展示"这次任务失败了/为什么"。
    pub text: String,
    /// 是否出错：拉起失败，或会话过程中出现过 `ProviderError`（鉴权/限流等供应商
    /// 侧错误信号）——不等价于"prompt 本身执行失败"，只是"这次跑得不干净"。
    pub errored: bool,
}

/// P6-A：`run_headless_session` 在该 app 没有真实安装包时（清单/权限读取失败）
/// 退化用的占位清单——只填 `assemble_launch_plan` 真正读到的两个字段
/// （`superagent.model`/`superagent.tools`，均取空/`None`），其余字段是不影响
/// 拼装结果的占位值。存在的理由：`spawn_call_session`/`spawn_task_session` 的
/// 被调方/该 app 记录（`InstalledApp`）本身不保证真的落过盘（见
/// `tests/call_session_it.rs`：只传一个 `InstalledApp`，不写任何包文件也应
/// 能跑通最小 headless 会话）——与前台 `open_app_after_acquire`"读失败即
/// `Err`"刻意不同，headless 内核容忍这一更宽松的调用场景。
fn minimal_manifest_for(app: &InstalledApp) -> crate::pkg::Manifest {
    crate::pkg::Manifest {
        name: app.name.clone(),
        version: app.version.clone(),
        keywords: vec![],
        engines: crate::pkg::Engines {
            superagent_host: String::new(),
        },
        superagent: crate::pkg::SuperagentField {
            schema_version: 1,
            display_name: app.display_name.clone(),
            category: app.category.clone(),
            ui: String::new(),
            permissions: String::new(),
            subagents: vec![],
            model: None,
            tools: vec![],
        },
    }
}

/// 单一计算点：读该 app 包目录下的清单/权限（读失败退化为 `minimal_manifest_for` +
/// 默认空 `Permissions`，见下方"与前台刻意不同"一节），按 `depth` 构造
/// `CallerIdentity`，经 `registry.launch` 算出 `LaunchContribution`。
///
/// `socket_path`：`Some` 时——这个路径上已经/将要有一个宿主监听器在跑，
/// `registry.launch` 算出的贡献原样生效（是否真的把这条 socket 放进沙盒窄放行，
/// 由调用方按 `contribution.needs_socket` 再判断一次，见 `run_headless_session`
/// 文档）。`None` 时——没有任何监听器，剥掉一切依赖 socket 的贡献（`bridges`/
/// `env`/`tools`/`needs_socket`），只保留 `sandbox_read`/`sandbox_write`（不依赖
/// socket）。
///
/// **本函数是 headless 会话唯一算一次 `LaunchContribution` 的地方**（P6-A review
/// I-1 修复）：此前 `spawn_call_session` 自己算一遍贡献只为决定要不要绑调用域
/// 监听器，`run_headless_session` 内部又独立重算一遍决定实际注入什么——两次输入
/// 本该一致，却只靠约定维持，且已经真的分叉过一次：`spawn_call_session` 那份用
/// 真实 `depth` 构造 `CallerIdentity`，`run_headless_session` 内部那份硬编码
/// `depth: 0`。翻转其中一次注册表 `declared()` 判断的结果（例如某能力按深度门控）
/// 就会让"决定绑不绑监听器"与"实际注入什么"这两件事互相不认识对方算出的答案——
/// 轻则监听器白绑/漏绑，重则给一个没有监听器的 socket 注入了桥。现在
/// `spawn_task_session`/`spawn_call_session` 都只调用本函数一次，把返回的
/// `(manifest, contribution)` 原样传给 `run_headless_session`，不存在第二次计算
/// 可以分叉。
///
/// `pub`：`tests/session_mgr_mcp_it.rs` 直接单测本函数三条属性——
/// `headless_contribution_some_socket_injects_mcp_tools_and_bridge`（`Some` 时贡献
/// 原样生效）、`headless_contribution_none_socket_strips_bridges_env_tools_keeps_sandbox_paths`
/// （`None` 时剥离依赖 socket 的字段、保留 sandbox 路径）、
/// `headless_contribution_depth_reaches_identity_without_breaking_declared`
/// （`depth` 确实传到 `CallerIdentity`，不会被内部悄悄归零）——不是给外部模块常规
/// 调用的公共 API 面，生产调用点只有 `spawn_task_session`/`spawn_call_session` 两处。
pub fn headless_contribution(
    layout: &DataLayout,
    registry: &CapabilityRegistry,
    app: &InstalledApp,
    depth: u32,
    hosttools_dir: &Path,
    socket_path: Option<&Path>,
    mcp: &McpManager,
) -> Result<(crate::pkg::Manifest, LaunchContribution), String> {
    let pkg_dir = layout.packages_dir(&app.app_id);
    let loaded_manifest = crate::pkg::load_and_validate(&pkg_dir).ok();
    let perms = loaded_manifest
        .as_ref()
        .and_then(|m| crate::permissions::load(&pkg_dir, &m.superagent.permissions).ok())
        .unwrap_or_default();
    let manifest = loaded_manifest.unwrap_or_else(|| minimal_manifest_for(app));

    let identity = CallerIdentity {
        app_id: app.app_id.clone(),
        trusted: app.trusted,
        depth,
    };
    let fallback_socket = layout.mcp_socket_path(&app.app_id);
    let sock = socket_path.unwrap_or(&fallback_socket);
    let ctx = LaunchCtx {
        app_id: &app.app_id,
        trusted: app.trusted,
        sandboxed: sandboxing_available(),
        // F1（review）：`headless_contribution` 是 task-mode/call 两条真实 spawn
        // 路径唯一算 `LaunchContribution` 的地方——同样要真的落地写目录，见
        // `LaunchCtx::materialize` 文档。
        materialize: true,
        layout,
        hosttools_dir,
        socket_path: sock,
        mcp,
    };
    let mut contribution = registry.launch(&perms, &identity, &ctx)?;
    if socket_path.is_none() {
        // 没有任何监听器在跑：剥掉一切依赖 socket 的贡献，只留 sandbox_read/write。
        contribution = LaunchContribution {
            sandbox_read: contribution.sandbox_read,
            sandbox_write: contribution.sandbox_write,
            ..Default::default()
        };
    }
    Ok((manifest, contribution))
}

/// 拉起该 app 的 headless task-mode pi 会话，把 `prompt` 跑到 `agent_end` 后
/// 收掉会话，返回 `TaskSessionResult`。调用方：`scheduler::Scheduler::tick`
/// （每条到期定时任务一次）。
///
/// **安全不变式（P3 §8/§10）**：定时/后台任务必须和该 app 的前台交互会话跑在
/// 同一个沙盒/权限边界下——绝不能是一次裸的、未沙盒化的 pi 调用。本函数经
/// `run_headless_session` 复用 `spawn_app_session`——与 `open_app_after_acquire`
/// 调的是同一个函数、同一段代码：macOS 上它内部无条件走 `sandboxed_argv`
/// （`sandboxing_available()` 是唯一真理源）。建 `settings.json`/算 `--model`/
/// `--tools`/各能力贡献同样复用 `build_settings_json`/`assemble_launch_plan`——
/// 与交互会话同一套函数、同一个 `sandboxed` 取值，不另起一套判断。
///
/// **MCP/桥工具面（P6-A）**：与前台交互会话经**同一个** `CapabilityRegistry::launch`
/// 算出贡献——经 `headless_contribution` 读该 app 的清单/权限，见其文档。**不**在
/// 这里起第二个 `McpSocketListener`：running-only 边界（Task15b：`Scheduler::tick`
/// 只对 `is_app_open` 判定为当前打开的 app 触发到期任务）保证本函数被调用时，若
/// 该 app 需要 socket，`open_app` 早先起的前台监听器已经在同一个
/// `layout.mcp_socket_path(app_id)` 上跑着，task-mode 会话直接复用它连接（同一个
/// app_id，socket 是 per-app 而非 per-session）；`registry` 因此传新建的一份
/// `capabilities::builtin()`——scheduler 没有 `AppState` 可拿，这份与前台
/// `AppState.capabilities` 是不同实例，`notifications` 等能力内部的限速窗口不与
/// 前台会话共享（可接受的取舍：task-mode 会话本身生命周期短、不常触发限速）。
///
/// 不分配界面槽位、不登记进 `AppState::app_sessions`、不起退避重启看护——headless
/// 任务型会话没有交互 UI，也不需要长期存活：跑完 `agent_end`（或拉起失败）就
/// `session.kill()` 收尾，不属于"打开的应用会话"这个概念，不占用并发闸门/界面槽位
/// （那两者是 P1/P2 为交互会话设计的资源，task-mode 有自己独立的并发上限，见
/// `scheduler::MAX_CONCURRENT_TASKS`）。
pub async fn spawn_task_session(
    layout: &DataLayout,
    hosttools_dir: &Path,
    mcp: &McpManager,
    app: &InstalledApp,
    task_id: &str,
    prompt: &str,
) -> Result<TaskSessionResult, String> {
    let registry = crate::capabilities::builtin();
    let socket_path = layout.mcp_socket_path(&app.app_id);
    let (manifest, contribution) = headless_contribution(
        layout,
        &registry,
        app,
        0,
        hosttools_dir,
        Some(&socket_path),
        mcp,
    )?;
    let (text, errored) = run_headless_session(
        layout,
        hosttools_dir,
        app,
        prompt,
        manifest,
        contribution,
        Some(&socket_path),
    )
    .await?;

    Ok(TaskSessionResult {
        app_id: app.app_id.clone(),
        task_id: task_id.to_string(),
        text,
        errored,
    })
}

/// 结构守卫（P6-A review round-2，I-1 结构化那一半）：`socket_path` 为 `None`
/// （没有任何监听器在跑）而 `needs_socket` 为真（贡献里至少一个能力的桥/env 依赖
/// socket）——这个组合绝不该发生，一旦发生就是"要给一个没人监听的 socket 注入桥"，
/// 直接 `Err`，绝不静默放行。纯函数、不做 IO：`run_headless_session` 会真的 spawn
/// 子进程，不方便直接单测这条判断本身，拆成这一个纯函数单独可测（见
/// `session_mgr::tests` 里的两条 `check_socket_invariant_*`）。
fn check_socket_invariant(socket_path: Option<&Path>, needs_socket: bool) -> Result<(), String> {
    if socket_path.is_none() && needs_socket {
        return Err(
            "内部错误：启动贡献需要宿主 socket，但没有绑定监听器（拒绝注入指向空气的桥）"
                .to_string(),
        );
    }
    Ok(())
}

/// 公共 headless 会话内核——`spawn_task_session`（定时任务）与 `spawn_call_session`
/// （P5 agent 互联）共用，避免两份"建 settings/home、拼启动计划、沙盒 spawn、
/// send_prompt、收 AssistantDelta 到 AgentEnded、kill"逻辑漂移。
///
/// P6-A review I-1 修复：本函数不再自己读清单/权限、不再自己调
/// `registry.launch`——`manifest`/`contribution` 由调用方经 `headless_contribution`
/// **算好一次**后原样传入（见该函数文档"唯一计算点"一节），本函数只管把它们拼成
/// `LaunchPlan`（`assemble_launch_plan`，与前台 `open_app_after_acquire` 同一个
/// 拼装函数）并拉起沙盒子进程，不持有 `mcp`/`registry`，也就不可能再在内部悄悄
/// 重新计算出一份不同的贡献。
///
/// `socket_path`：是否真的把这条路径放进沙盒的 MCP socket 窄放行，看
/// `contribution.needs_socket`（无依赖 socket 的贡献时不放行，最小权限——即便
/// 调用方传了 `Some`，贡献本身不需要 socket 也不会放行）。
///
/// 沙盒决策（`app.trusted`、`app_data_dir`）与该 app 的前台交互会话完全同规则喂给
/// `spawn_app_session`——`trusted`/权限只取自 `app` 自己的记录，本函数没有任何
/// "调用方"概念，结构上就不可能把别的应用的权限带进这次启动（P5 §8"权限绝不随
/// 调用链放大"在这里体现为：签名里根本没有调用方参数）。
///
/// 返回 `(text, errored)`：`text` 是全部 `AssistantDelta` 拼接；`errored` 表示过程中
/// 出现过 `ProviderError`（供应商侧错误信号）——不含"拉起失败"（那走 `Err`）。
async fn run_headless_session(
    layout: &DataLayout,
    hosttools_dir: &Path,
    app: &InstalledApp,
    prompt: &str,
    manifest: crate::pkg::Manifest,
    contribution: LaunchContribution,
    socket_path: Option<&Path>,
) -> Result<(String, bool), String> {
    use crate::rpc::PiEvent;

    // P6-A review round-2（I-1 结构化）：把"没有监听器却贡献需要 socket"这条组合
    // 钉死为一个显式错误，不再只靠调用方（`spawn_task_session`/`spawn_call_session`）
    // 按 `contribution.needs_socket` 算对该传 `Some` 还是 `None` 这条约定维持——
    // 即便未来某个调用点/某次重构算错了，本函数也会在拉起子进程、写任何桥/env 之前
    // 直接拒绝，而不是把指向"没人监听"的 `mcp_bridge.ts`/`SUPERAGENT_MCP_SOCKET` 等
    // 贡献悄悄注入进子进程（P3 不变式：绝不注入指向空气的桥）。
    check_socket_invariant(socket_path, contribution.needs_socket)?;

    let sandboxed = sandboxing_available();

    let model_launch = resolve_model_launch(layout, &app.app_id, &manifest)?;
    write_agent_home(
        layout,
        &app.app_id,
        &build_settings_json(app, layout, sandboxed),
        model_launch.models_json.as_ref(),
    )?;
    layout.ensure_app(&app.app_id).map_err(|e| e.to_string())?;

    let mcp_socket_for_sandbox = if contribution.needs_socket {
        socket_path.map(|p| p.to_path_buf())
    } else {
        None
    };

    let plan = assemble_launch_plan(
        app,
        &manifest,
        &contribution,
        layout,
        hosttools_dir,
        sandboxed,
        &model_launch,
    )?;

    let session_dir = layout.private_dir("sessions", &app.app_id)?;
    let app_data_dir = layout.private_dir("apps", &app.app_id)?;

    let (mut session, mut rx) = spawn_app_session(
        &session_dir,
        &app_data_dir,
        app.trusted,
        plan.env,
        plan.extra_args,
        mcp_socket_for_sandbox,
        plan.sandbox_read,
        plan.sandbox_write,
    )
    .await?;

    // P6-F：登记为后台会话，活到函数返回（覆盖 `?` 提前返回与正常 kill 收尾）。
    // 必须具名绑定——`let _ =` 会当场 drop。
    let _headless = HeadlessGuard::register(&app.app_id, session.child_id());

    session.send_prompt(prompt).await?;

    let mut text = String::new();
    let mut errored = false;
    while let Some(ev) = rx.recv().await {
        match ev {
            PiEvent::AssistantDelta(d) => text.push_str(&d),
            PiEvent::ProviderError(_) => errored = true,
            PiEvent::AgentEnded => break,
            _ => {}
        }
    }
    session.kill().await;

    Ok((text, errored))
}

/// 拉起被调方（callee）的 headless pi 会话执行一次 agent 间调用（P5 §1），把 `prompt`
/// 跑到 `agent_end` 后收掉会话，返回 `call_bus::CallResult`。调用方：
/// `call_bus::handle_call_agent`（socket 分发的 `__host_call_agent__` 分支）。
///
/// **权限不放大（P5 §8 核心）**：被调方运行时的一切约束都取自 `callee` 自己的记录
/// （`callee.trusted`）与它自己的清单（经 `run_headless_session` → `build_settings_json`/
/// `assemble_launch_plan`/`spawn_app_session` 全部按 `callee` 计算）——调用方的
/// 身份/权限从不进入本函数（签名里就没有调用方参数）。
///
/// `depth`：本次被调会话的调用深度（= 调用方 depth + 1），已由 `handle_call_agent`
/// 的深度闸校验过在 `MAX_CALL_DEPTH` 之内。
pub async fn spawn_call_session(
    layout: &DataLayout,
    hosttools_dir: &Path,
    mcp: &McpManager,
    callee: &InstalledApp,
    prompt: &str,
    depth: u32,
) -> Result<crate::call_bus::CallResult, String> {
    // 载入被调方自己的权限——供下面 `start_with_identity` 绑定监听器用（监听器需要
    // 绑定完整 `Permissions`，不只是 `registry.launch` 算出的派生 `LaunchContribution`，
    // 见 `McpSocketListener::start_with_identity` 文档"app 身份/权限绝不来自线上请求"）。
    // 读失败退化为默认空——被调方仍能以最小会话跑，只是拿不到 MCP/嵌套能力；绝不
    // panic。被调方运行时的一切约束都取自它自己（权限不放大）。
    let pkg_dir = layout.packages_dir(&callee.app_id);
    let callee_perms = crate::pkg::load_and_validate(&pkg_dir)
        .ok()
        .and_then(|m| crate::permissions::load(&pkg_dir, &m.superagent.permissions).ok())
        .unwrap_or_default();

    // 调用域 socket：独立于被调方 per-app socket，绑定本次调用深度 `depth`，服务被调方
    // 临时会话的全部宿主调用（MCP + 嵌套 call_agent）。`nonce` 用单调原子计数器保证
    // 并发唯一——同一 callee 同一 depth 可能被不同调用方并发调起，若沿用 spec §1.4 初稿的
    // `<app_id>-d<depth>` 会撞同一 socket 文件（`start_with` "先删旧文件再 bind" 会互删）；
    // 计数器是非时间、单调、并发安全的更稳来源（对初稿的实现期改进）。
    static CALL_NONCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let seq = CALL_NONCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let nonce = format!("{}-d{}-{}", callee.app_id, depth, seq);
    let call_socket_path = layout.call_socket_path(&nonce);

    // P6-A review I-1 修复：贡献只经 `headless_contribution` 算一次——它的返回值
    // `contribution` 原样喂给下面"要不要绑调用域监听器"的判断和
    // `run_headless_session`，不再各自重算一遍（此前 `run_headless_session` 内部
    // 会用同一套输入独立重新调一次 `registry.launch`，两次本该一致却只靠约定
    // 维持，且 `depth` 已经真的分叉过——见 `headless_contribution` 文档）。
    //
    // `registry` 在这里**不能**是 `AppState.capabilities`：本函数没有 `AppState`
    // （也不该为此改签名，见调用点 `handle_call_agent` 文档），故在函数内新建
    // 一份 `Arc::new(capabilities::builtin())`——调用域会话本就是这次调用专属、
    // 随本函数返回即 `stop()` 的短命监听器（`nonce` 保证不同调用各有独立 socket
    // 路径），像 `notifications` 那样的限速窗口不与该 app 的前台监听器共享，
    // 代价是同一 app 若既有前台会话又在做被调调用，两边限速各算各的——调用域
    // 会话本身生命周期极短、不常触发限速，这一点不共享属可接受的取舍。
    let registry = std::sync::Arc::new(crate::capabilities::builtin());
    let (manifest, contribution) = headless_contribution(
        layout,
        &registry,
        callee,
        depth,
        hosttools_dir,
        Some(&call_socket_path),
        mcp,
    )?;

    // 仅当确有注入（MCP 授权面非空 或 需要桥）时才起调用域监听器；否则最小会话，无 socket。
    let listener = if contribution.needs_socket {
        Some(
            crate::mcp_socket::McpSocketListener::start_with_identity(
                mcp.clone(),
                layout.clone(),
                CallerIdentity {
                    app_id: callee.app_id.clone(),
                    trusted: callee.trusted,
                    depth,
                },
                callee_perms.clone(),
                call_socket_path.clone(),
                Some(hosttools_dir.to_path_buf()),
                registry.clone(),
            )
            .map_err(|e| format!("调用域 socket 监听器启动失败：{e}"))?,
        )
    } else {
        None
    };

    // `contribution.needs_socket` 为假时给 `run_headless_session` 传 `None`——没有
    // 监听器在跑，不应让沙盒 profile 白放行一个用不上的 socket 路径（虽然
    // `run_headless_session` 内部本就会按 `contribution.needs_socket` 再门控一次，
    // 这里显式传 `None` 是让调用点本身的意图也诚实：`listener` 为 `None` 时压根
    // 没有 socket 这回事）。
    let socket_for_headless: Option<&Path> = if contribution.needs_socket {
        Some(&call_socket_path)
    } else {
        None
    };
    let result = run_headless_session(
        layout,
        hosttools_dir,
        callee,
        prompt,
        manifest,
        contribution,
        socket_for_headless,
    )
    .await;

    // 无论成功失败，收掉调用域监听器 + best-effort 清 callbus/<nonce>/ 目录（残留仅磁盘
    // 占用，非安全洞——同 P4 拒绝草稿无 GC 的已知取舍）。
    if let Some(l) = listener {
        l.stop().await;
    }
    if let Some(dir) = call_socket_path.parent() {
        let _ = std::fs::remove_dir_all(dir);
    }

    let (text, errored) = result?;
    Ok(crate::call_bus::CallResult {
        ok: !errored,
        text,
        error: if errored {
            Some("被调应用会话执行出错".to_string())
        } else {
            None
        },
    })
}

// ---------------------------------------------------------------------------
// Task6（P4 Maker）：预览会话——暂存草稿的沙盒试跑，不占用界面槽位/registry
// ---------------------------------------------------------------------------

/// T6 预览会话专用的两个隐藏子目录名，落在**草稿暂存目录**（`staging_dir`）
/// 内部，而不是 host root 下另开一块。这是刻意的：`spawn_preview_session` 把
/// `staging_dir` 本身当作这次预览的沙盒 WRITE 根（见该函数文档"为什么
/// `app_data_dir` 就是 `staging_dir`"一节）——若把 agent home / session 存储
/// 目录放在这个 WRITE 根之外，真实 pi（非 `mock_pi`，`mock_pi` 完全不碰这两个
/// 环境变量指向的路径）在受限沙盒下尝试往里面写 settings.json/session 历史时
/// 会被直接 EPERM 拒绝——与 `tests/real_pi_bash_escape_it.rs::SandboxedPi::spawn`
/// 把 `pi_agent_dir`/`session_dir` 都放进 `app_data_canon` 内部同一个理由（该
/// 文件同款注释有详细展开：不这样做真实 pi 会尝试读/写宿主真实 `~/.pi/agent/...`，
/// 那不在 `BASE_PROFILE` 只读白名单内）。
///
/// **必须在会话结束（成功或失败）后清理**：这两个隐藏目录若一直留在
/// `staging_dir` 里，用户之后对同一 `draft_id` 调用 `__host_maker_install__`
/// 时，`install::copy_dir` 会把它们（settings.json/session 历史等纯运行期产物）
/// 原样递归复制进最终安装包目录，污染发行给最终用户的包。`spawn_preview_session`
/// 因此在返回前（无论 `Ok`/`Err`）都会尽最大努力 `remove_dir_all` 这两个子目录，
/// 让 `staging_dir` 恢复到调用前的干净状态——这就是 T6 brief 里
/// "会话跑完/关闭即收"这条要求在本函数里的落地方式（见该函数文档"预览会话
/// 追踪选择"一节）。
const PREVIEW_AGENT_HOME_SUBDIR: &str = ".superagent-preview-agent-home";
const PREVIEW_SESSION_SUBDIR: &str = ".superagent-preview-session";

/// 尽最大努力清理 T6 预览会话在 `staging_dir` 内留下的两个隐藏临时子目录。
/// `remove_dir_all` 对不存在的路径返回 `Err`，这里全部吞掉——清理是"锦上添花"
/// 的收尾动作，它自身失败不应该覆盖/掩盖调用方真正关心的 spawn/ready 结果。
fn cleanup_preview_scratch_dirs(layout: &DataLayout, draft_id: &str) {
    // 预览期间草稿应用对暂存目录可写，可能已把它换成链接：清理前重新经
    // `checked_maker_staging_dir` 校验，不合格就什么都不删（不能删到链接目标里去）。
    let Ok(staging_dir) = layout.checked_maker_staging_dir(draft_id) else {
        return;
    };
    // 假设：这两个固定名字是 host 内部的运行期暂存名（点前缀），不会与草稿自己的
    // 文件撞名——不对草稿内容做存在性检查/改名让路，撞了就是静默吞掉草稿那份同名目录。
    for sub in [PREVIEW_AGENT_HOME_SUBDIR, PREVIEW_SESSION_SUBDIR] {
        // 子目录若被换成链接，`remove_dir_all` 只删链接本身（不跟随）。
        let _ = std::fs::remove_dir_all(staging_dir.join(sub));
    }
}

/// 预览会话专用、与清单/权限无关的固定 `extra_args`：抽成纯函数（审查修复轮 1
/// Important）方便直接单测断言，不必像 `tests/maker_preview_it.rs` 第 1 层证明
/// 那样起真实子进程走 macOS-only 的 `sandboxed_argv` 集成测试。
/// - `["--tools", resolve_tools(&[], false, sandboxed).join(",")]`：声明为空 →
///   恒定 `SAFE_TOOLS`，不含 bash（见 `resolve_tools` 文档）。
/// - `"--no-skills"`：`spawn_preview_session` 不经 `build_launch`/
///   `assemble_launch_plan`（见下方文档"复用而非重新实现沙盒包裹"一节），因此
///   拿不到 `build_launch` 里那条无条件加的 `--no-skills`——此前遗漏，预览会话
///   会看见用户真实主目录下 `~/.agents/skills` 的任意技能，与 `build_launch`
///   文档"堵泄漏的底线"那段要防的越权是同一个口子。这里独立补上，保证四条会话
///   路径（交互/task-mode/call/preview）没有一条漏掉这条底线。
fn preview_extra_args(sandboxed: bool) -> Vec<String> {
    vec![
        "--tools".to_string(),
        resolve_tools(&[], false, sandboxed).join(","),
        "--no-skills".to_string(),
    ]
}

/// T6（P4 Maker）：把 Maker 暂存草稿（`staging_dir`）当作**未受信第三方包**
/// （`trusted=false`——Maker 输出未经用户审阅，预览阶段与真正安装后一样绝不能
/// 免检）拉起一个临时 pi 预览会话，验证它确实能在 P2 沙盒下跑到 ready（收到
/// 第一个 `agent_end`），然后立刻收掉会话。调用方：`maker::handle_preview`
/// （`__host_maker_preview__` 分支）。
///
/// **复用而非重新实现沙盒包裹**：本函数唯一的"拉起子进程"动作就是下面这一行
/// `spawn_app_session(...)`调用——与 `open_app_after_acquire`（交互式前台会话）
/// `spawn_task_session`（headless task-mode）调用的是同一个私有函数，
/// macOS 上无条件经 `sandboxed_argv`（`sandboxing_available()` 是唯一真理源，
/// 见该函数文档头部"两处必须永远一致"的说明）包一层真实 `/usr/bin/sandbox-exec`。
/// 本函数不重新判断"要不要沙盒"，也不重新拼一份 argv。
///
/// **为什么 `app_data_dir` 就是 `staging_dir` 本身**：T6 brief 明确要求"拉起一个
/// 预览 pi 会话...点指向暂存草稿目录"——`spawn_app_session` 的 `app_data_dir`
/// 参数正是沙盒 profile 的唯一 WRITE 根（同时因 `render_profile` 给 WRITE 根
/// 追加对应的 `file-read*` 规则而天然可读），直接传 `staging_dir` 让草稿目录下
/// 的全部文件（`package.json`/`ui/`/`agent/`...）在沙盒内可读可写，且不需要
/// 额外的 `read_paths` 放行（`sandboxed_argv` 目前硬编码 `read_paths=&[]`，见其
/// 文档）。
///
/// **不做的事（刻意简化，已知 concern，见 task-6-report.md）**：不读取该草稿的
/// `package.json`/`permissions.json` 拼出与安装后完全一致的完整启动计划
/// （`--model`/声明的 `--tools`/`--append-system-prompt <persona>`/MCP 注入）——
/// 预览分支只需要证明"这份草稿能在沙盒里跑起来"这一件事，不需要它声明的完整
/// 能力面；`--tools` 因此固定用 `resolve_tools(&[], false, sandboxed)`
/// （声明为空 → 恒定 SAFE_TOOLS，不含 bash，见该函数文档）。真实生产实现若要把
/// 预览做成与安装后的应用会话功能对等（如 UI 走独立预览槽 iframe 实际渲染该
/// 草稿的 `ui/index.html`），需要在此基础上补齐清单驱动的启动计划——留给后续
/// 任务。
///
/// **驱动到 ready 的手法（review 修复：改无 key RPC 往返，不再发 prompt）**：
/// 最初实现发一条内容无关紧要的探测 `prompt`、等 `PiEvent::AgentEnded`——review
/// 指出这在真实 pi + 真实 API key 下是**一次计费的模型轮次**：预览只需要证明
/// "沙盒化子进程确实启动、且它的 RPC 通道能来回通信"，不需要真的驱动一轮模型
/// 对话，不应该花用户的 BYOK 额度。现在改发 `{"type":"get_session_stats"}`
/// （`RpcSession::send_get_session_stats`）——这条命令是 session-local 查询
/// （`AgentSession.getSessionStats()`），已用真实 pi 二进制实测确认 keyless、
/// 不经过模型 API（见 `rpc::PiEvent::SessionStats` 文档："keyless、`--no-session`、
/// 未发送任何 `prompt`"），`mock_pi` 也早在 P3 Task18 用量里程碑里就无条件响应
/// 它（与 `MOCK_PI_MODE` 无关，见 `src-tauri/src/bin/mock_pi.rs`）。等到第一个
/// `PiEvent::SessionStats { .. }` 响应即视为"该草稿确实能在沙盒里跑起来、且它的
/// RPC 通道能正常来回通信"——不再要求跑完一整轮 `agent_start`/`agent_end`。
/// `PiEvent::ProviderError`（`extension_error`，如启动期扩展冲突）视为"未 ready"
/// 并携带错误原因；20s 超时同样视为"未 ready"——预览是一次用户在等待结果的
/// 同步操作，不能无限期挂起。
///
/// 20s 的选择：与本文件其它手工里程碑测试（`tests/real_pi_bash_escape_it.rs`）
/// 给真实 pi 的超时预算一致；`mock_pi` 场景下这个超时几乎不可能被触及
/// （canned 事件流是同步、立即写出的）。
pub async fn spawn_preview_session(layout: &DataLayout, draft_id: &str) -> Result<(), String> {
    use crate::rpc::PiEvent;

    let sandboxed = sandboxing_available();
    // 沙盒要求可写路径「规范化后等于自身」（见 `sandbox::build_profile`）。暂存目录及其两个
    // 子目录对（上一次预览里的）草稿应用可写，应用可以把它们换成链接，所以**不**对路径先规范化
    // 再用（那会把链接目标授权成可写根）：与 `private_dir` 同一套校验——真目录、非链接、
    // 规范化等于由已规范化数据根按字面推出的预期路径，不满足就拒绝。
    let staging_dir_buf = layout.checked_maker_staging_dir(draft_id)?;
    let staging_dir = staging_dir_buf.as_path();
    let agent_home = crate::paths::ensure_real_dir(&staging_dir.join(PREVIEW_AGENT_HOME_SUBDIR))?;
    let preview_session_dir =
        crate::paths::ensure_real_dir(&staging_dir.join(PREVIEW_SESSION_SUBDIR))?;

    let env = vec![(
        "PI_CODING_AGENT_DIR".to_string(),
        agent_home.to_string_lossy().to_string(),
    )];
    let extra_args = preview_extra_args(sandboxed);

    let spawn_result = spawn_app_session(
        &preview_session_dir,
        staging_dir,
        false,
        env,
        extra_args,
        None,
        vec![],
        vec![],
    )
    .await;

    let (mut session, mut rx) = match spawn_result {
        Ok(pair) => pair,
        Err(e) => {
            cleanup_preview_scratch_dirs(layout, draft_id);
            return Err(format!("预览会话启动失败：{e}"));
        }
    };

    if let Err(e) = session.send_get_session_stats().await {
        session.kill().await;
        cleanup_preview_scratch_dirs(layout, draft_id);
        return Err(format!("预览会话探测 get_session_stats 发送失败：{e}"));
    }

    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(20);
    let mut ready = false;
    let mut provider_error: Option<String> = None;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        match tokio::time::timeout(remaining, rx.recv()).await {
            Ok(Some(PiEvent::SessionStats { .. })) => {
                ready = true;
                break;
            }
            Ok(Some(PiEvent::ProviderError(msg))) => provider_error = Some(msg),
            Ok(Some(_)) => continue,
            Ok(None) => break, // 通道关闭 = 子进程已退出
            Err(_) => break,   // 单次 recv 超时；外层 deadline 判断会在下一圈退出循环
        }
    }

    session.kill().await;
    cleanup_preview_scratch_dirs(layout, draft_id);

    if ready {
        Ok(())
    } else {
        Err(provider_error.unwrap_or_else(|| "预览会话在超时前未能进入 ready 状态".to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::DataLayout;
    use crate::registry::InstalledApp;
    use std::path::Path;

    #[test]
    fn headless_guard_registers_until_drop() {
        let id = "t-headless-guard";
        let guard = HeadlessGuard::register(id, Some(4242));
        let found: Vec<_> = running_headless_sessions()
            .into_iter()
            .filter(|h| h.app_id == id)
            .collect();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].pid, Some(4242));
        drop(guard);
        assert!(running_headless_sessions().iter().all(|h| h.app_id != id));
    }

    fn app(id: &str, trusted: bool) -> InstalledApp {
        InstalledApp {
            app_id: id.into(),
            name: id.into(),
            version: "1.0.0".into(),
            display_name: id.into(),
            category: "life".into(),
            icon: None,
            trusted,
            domains: vec![],
        }
    }

    // ---- build_settings_json：sandboxed × trusted 四象限 ----

    #[test]
    fn settings_untrusted_not_sandboxed_restricted_p1_regression() {
        // P1 回归钉子：非沙盒平台（sandboxed=false）时 untrusted 必须字节对齐地保持
        // Task 6 之前的行为——写 extensions:[]（不加载第三方扩展/钩子）。
        let layout = DataLayout::new(Path::new("/data").to_path_buf());
        let v = build_settings_json(&app("x", false), &layout, false);
        assert_eq!(v["packages"][0]["extensions"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn settings_untrusted_sandboxed_loads_extensions() {
        // L2 OS 沙盒在场（sandboxed=true）：即便 untrusted 也不再写 extensions:[]，
        // 即加载第三方扩展/钩子——OS 沙盒本身就是硬边界。
        let layout = DataLayout::new(Path::new("/data").to_path_buf());
        let v = build_settings_json(&app("x", false), &layout, true);
        assert!(v["packages"][0].get("extensions").is_none());
    }

    #[test]
    fn settings_trusted_not_sandboxed_full() {
        let layout = DataLayout::new(Path::new("/data").to_path_buf());
        let v = build_settings_json(&app("x", true), &layout, false);
        assert!(v["packages"][0].get("extensions").is_none());
    }

    #[test]
    fn settings_trusted_sandboxed_full_unchanged() {
        // 第一方（trusted）无论 sandboxed 与否行为都不变：全加载。
        let layout = DataLayout::new(Path::new("/data").to_path_buf());
        let v = build_settings_json(&app("x", true), &layout, true);
        assert!(v["packages"][0].get("extensions").is_none());
    }

    fn safe_tools_vec() -> Vec<String> {
        SAFE_TOOLS.iter().map(|s| s.to_string()).collect()
    }

    // ---- resolve_tools：sandboxed × trusted 四象限 ----

    #[test]
    fn resolve_tools_trusted_nonempty_declared_as_is_plus_host_ui_emit() {
        let declared = vec!["bash".to_string(), "read".to_string()];
        for sandboxed in [false, true] {
            let tools = resolve_tools(&declared, true, sandboxed);
            // 信任第一方：原样保留其声明（含 bash），不做交集裁剪；sandboxed 值不影响结果。
            assert!(tools.contains(&"bash".to_string()));
            assert!(tools.contains(&"read".to_string()));
            // 但必须补上 __host_ui_emit__（UI 推送必需，声明里没有）。
            assert!(tools.contains(&"__host_ui_emit__".to_string()));
        }
    }

    #[test]
    fn resolve_tools_trusted_empty_gives_safe_tools() {
        for sandboxed in [false, true] {
            assert_eq!(resolve_tools(&[], true, sandboxed), safe_tools_vec());
        }
    }

    #[test]
    fn resolve_tools_untrusted_not_sandboxed_intersects_safe_tools_strips_bash_p1_regression() {
        // P1 回归钉子：非沙盒平台（sandboxed=false）时 untrusted 必须字节对齐地保持
        // Task 6 之前的行为——声明与 SAFE_TOOLS 取交集，bash/联网类工具被裁掉。
        let declared = vec!["bash".to_string(), "read".to_string(), "curl".to_string()];
        let tools = resolve_tools(&declared, false, false);
        assert!(!tools.contains(&"bash".to_string()));
        assert!(!tools.contains(&"curl".to_string()));
        assert!(tools.contains(&"read".to_string()));
        assert!(tools.contains(&"__host_ui_emit__".to_string()));
    }

    #[test]
    fn resolve_tools_untrusted_sandboxed_relaxed_keeps_bash() {
        // L2 OS 沙盒在场（sandboxed=true）：untrusted 也放宽到声明原样，可含 bash/联网
        // 工具——OS 沙盒兜底 exfil，不再需要宿主侧白名单裁剪。
        let declared = vec!["bash".to_string(), "read".to_string(), "curl".to_string()];
        let tools = resolve_tools(&declared, false, true);
        assert!(tools.contains(&"bash".to_string()));
        assert!(tools.contains(&"curl".to_string()));
        assert!(tools.contains(&"read".to_string()));
        assert!(tools.contains(&"__host_ui_emit__".to_string()));
    }

    #[test]
    fn resolve_tools_untrusted_empty_gives_safe_tools() {
        for sandboxed in [false, true] {
            assert_eq!(resolve_tools(&[], false, sandboxed), safe_tools_vec());
        }
    }

    #[test]
    fn resolve_tools_always_includes_host_ui_emit() {
        // 声明非空且不含 __host_ui_emit__ 的情况下，trusted/untrusted × sandboxed 四条
        // 分支都要补齐。
        for sandboxed in [false, true] {
            assert!(resolve_tools(&["ls".to_string()], true, sandboxed)
                .iter()
                .any(|t| t == "__host_ui_emit__"));
            assert!(resolve_tools(&["grep".to_string()], false, sandboxed)
                .iter()
                .any(|t| t == "__host_ui_emit__"));
        }
    }

    #[test]
    fn preview_extra_args_includes_no_skills() {
        // 审查修复轮 1 Important 的回归测试：`spawn_preview_session` 不经
        // `build_launch`（那里的 `--no-skills` 覆盖不到它），必须自己独立带上
        // 同一条参数——否则预览会话会看见用户真实主目录下的任意技能。
        for sandboxed in [false, true] {
            let args = preview_extra_args(sandboxed);
            assert!(
                args.iter().any(|a| a == "--no-skills"),
                "preview_extra_args(sandboxed={sandboxed}) 应含 --no-skills：{args:?}"
            );
            assert!(
                args.iter().any(|a| a == "--tools"),
                "不应因为补 --no-skills 丢了原有的 --tools：{args:?}"
            );
        }
    }

    #[test]
    fn build_launch_sandbox_write_is_exactly_own_agent_home_and_session_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let layout = DataLayout::new(tmp.path().to_path_buf());
        let root = std::fs::canonicalize(tmp.path()).unwrap();
        let plan = build_launch(&app("a", false), &layout, Path::new("/ht")).unwrap();
        // 写白名单是经 `private_dir` 取得的规范化字面路径。
        assert_eq!(
            plan.sandbox_write,
            vec![
                layout.private_dir("agenthome", "a").unwrap(),
                layout.private_dir("sessions", "a").unwrap()
            ]
        );
        // 绝不含数据根、其它应用的目录，也不含 $APP_DATA 之外的共享目录。
        for p in &plan.sandbox_write {
            assert_ne!(p, &root);
            assert!(!p.ends_with("b"), "{p:?}");
            assert!(!p.starts_with(root.join("apps")), "{p:?}");
        }
        // 只读基座恰为该应用自己的包目录与 hosttools，不含数据根或其它应用目录。
        assert_eq!(
            plan.sandbox_read,
            vec![layout.packages_dir("a"), PathBuf::from("/ht")]
        );
        for p in &plan.sandbox_read {
            assert_ne!(p, &root);
            assert!(!p.ends_with("b"), "{p:?}");
            assert!(!p.starts_with(root.join("apps")), "{p:?}");
        }
    }

    /// C1（I1-2）：应用把自己的 sessions/agenthome/apps 目录各换成指向受害目录的符号链接后，
    /// 从生产启动计划出发（`build_launch`/`assemble_launch_plan`，以及再往下到
    /// `sandboxed_argv`）一律拒绝；受害目录无任何写入。
    #[test]
    fn launch_plan_rejects_swapped_private_dirs_c1() {
        for kind in ["sessions", "agenthome", "apps"] {
            let tmp = tempfile::tempdir().unwrap();
            let layout = DataLayout::new(tmp.path().to_path_buf());
            let rec = app("a", false);
            build_launch(&rec, &layout, Path::new("/ht")).expect("正常目录应通过");
            let victim = tmp.path().join("victim");
            std::fs::create_dir_all(&victim).unwrap();
            let dir = layout.private_dir(kind, "a").unwrap();
            std::fs::rename(&dir, tmp.path().join(format!("{kind}-old"))).unwrap();
            std::os::unix::fs::symlink(&victim, &dir).unwrap();
            let e = build_launch(&rec, &layout, Path::new("/ht"))
                .err()
                .expect("build_launch 应拒绝");
            assert!(e.contains("符号链接"), "{kind}: {e}");
            assert!(assemble_launch_plan(
                &rec,
                &manifest_min(),
                &LaunchContribution::default(),
                &layout,
                Path::new("/ht"),
                true,
                &ModelLaunch::default(),
            )
            .is_err());
            // 即便有人绕过 private_dir 直接把链接路径交给 sandboxed_argv，内核层也拒绝。
            #[cfg(target_os = "macos")]
            {
                let r = sandboxed_argv(
                    &layout.app_data_dir("a"),
                    false,
                    &[],
                    None,
                    &[],
                    &[layout.agent_home_dir("a"), layout.session_dir("a")],
                );
                assert!(r.is_err(), "{kind}: sandboxed_argv 应拒绝符号链接可写路径");
            }
            assert_eq!(std::fs::read_dir(&victim).unwrap().count(), 0, "{kind}");
        }
    }

    #[test]
    fn assemble_keeps_base_sandbox_write_and_merges_contribution_without_dups() {
        let tmp = tempfile::tempdir().unwrap();
        let layout = DataLayout::new(tmp.path().to_path_buf());
        let home = layout.private_dir("agenthome", "a").unwrap();
        let sess = layout.private_dir("sessions", "a").unwrap();
        let extra = PathBuf::from("/data/shared/x");
        let contribution = LaunchContribution {
            sandbox_read: vec![PathBuf::from("/r")],
            sandbox_write: vec![extra.clone(), sess.clone()],
            ..Default::default()
        };
        let plan = assemble_launch_plan(
            &fake_app("a", false),
            &manifest_min(),
            &contribution,
            &layout,
            Path::new("/ht"),
            true,
            &ModelLaunch::default(),
        )
        .unwrap();
        assert_eq!(plan.sandbox_write, vec![home, sess, extra]);
        assert_eq!(
            plan.sandbox_read,
            vec![
                layout.packages_dir("a"),
                PathBuf::from("/ht"),
                PathBuf::from("/r")
            ]
        );
    }

    #[test]
    fn launch_has_hosttools_persona_env() {
        let tmp = tempfile::tempdir().unwrap();
        let layout = DataLayout::new(tmp.path().to_path_buf());
        let plan = build_launch(&app("x", true), &layout, Path::new("/ht")).unwrap();
        assert!(plan
            .extra_args
            .windows(2)
            .any(|w| w[0] == "-e" && w[1].ends_with("permission_gate.ts")));
        assert!(plan
            .extra_args
            .iter()
            .any(|a| a == "--append-system-prompt"));
        assert!(plan.env.iter().any(|(k, _)| k == "PI_CODING_AGENT_DIR"));
        assert!(plan.env.iter().any(|(k, _)| k == "SUPERAGENT_APP_DATA"));
        // P6-A：ui_emit.ts 桥不再由 build_launch 直接写死——改由 `ui_emit` 能力经
        // `LaunchContribution.bridges` 贡献，`assemble_launch_plan` 才会把它拼进
        // extra_args（见下方 launch_plan_tools_include_registry_contributed_tool_names）。
        assert!(!plan.extra_args.iter().any(|a| a.ends_with("ui_emit.ts")));
    }

    // ---- assemble_launch_plan：--tools 白名单必须与各能力贡献的桥工具名取并集
    // （spec §7.5 回归钉子——此前桥注入的工具名从不出现在 --tools 里，pi 自己的
    // 工具白名单会把这些工具直接拒绝，等于桥白注入了） ----

    fn tools_arg(plan: &LaunchPlan) -> Vec<String> {
        let i = plan
            .extra_args
            .iter()
            .position(|a| a == "--tools")
            .expect("必有 --tools");
        plan.extra_args[i + 1]
            .split(',')
            .map(|s| s.to_string())
            .collect()
    }
    fn fake_app(id: &str, trusted: bool) -> InstalledApp {
        InstalledApp {
            app_id: id.into(),
            name: id.into(),
            version: "1.0.0".into(),
            display_name: id.into(),
            category: "life".into(),
            icon: None,
            trusted,
            domains: vec![],
        }
    }
    fn manifest_min() -> crate::pkg::Manifest {
        serde_json::from_value(serde_json::json!({
        "name": "@t/x", "version": "1.0.0", "keywords": ["superagent-app"], "engines": {"superagent-host": ">=1.0 <2.0"},
        "superagent": {"schemaVersion": 1, "displayName": "x", "category": "life", "ui": "ui/index.html", "permissions": "permissions.json"}
    })).unwrap()
    }

    #[test]
    fn launch_plan_tools_include_registry_contributed_tool_names() {
        let tmp = tempfile::tempdir().unwrap();
        let layout = DataLayout::new(tmp.path().to_path_buf());
        let contribution = LaunchContribution {
            tools: vec![
                "mcp__fs1__read_file".into(),
                "__host_call_agent__".into(),
                "__host_notify__".into(),
            ],
            bridges: vec!["ui_emit.ts", "mcp_bridge.ts"],
            ..Default::default()
        };
        let plan = assemble_launch_plan(
            &fake_app("a", false),
            &manifest_min(),
            &contribution,
            &layout,
            Path::new("/ht"),
            true,
            &ModelLaunch::default(),
        )
        .unwrap();
        let tools = tools_arg(&plan);
        for t in [
            "read",
            "mcp__fs1__read_file",
            "__host_call_agent__",
            "__host_notify__",
        ] {
            assert!(tools.contains(&t.to_string()), "{tools:?}");
        }
        assert!(plan
            .extra_args
            .windows(2)
            .any(|w| w[0] == "-e" && w[1].ends_with("mcp_bridge.ts")));
        assert!(plan
            .extra_args
            .windows(2)
            .any(|w| w[0] == "-e" && w[1].ends_with("ui_emit.ts")));
        assert!(plan
            .extra_args
            .windows(2)
            .any(|w| w[0] == "-e" && w[1].ends_with("permission_gate.ts")));
    }

    #[test]
    fn launch_plan_tools_without_contribution_equal_resolve_tools_exactly() {
        let tmp = tempfile::tempdir().unwrap();
        let layout = DataLayout::new(tmp.path().to_path_buf());
        let plan = assemble_launch_plan(
            &fake_app("a", false),
            &manifest_min(),
            &LaunchContribution::default(),
            &layout,
            Path::new("/ht"),
            true,
            &ModelLaunch::default(),
        )
        .unwrap();
        assert_eq!(tools_arg(&plan), resolve_tools(&[], false, true));
        assert!(!plan.extra_args.iter().any(|a| a.ends_with("mcp_bridge.ts")));
    }

    #[test]
    fn launch_plan_dedups_tool_already_in_safe_tools() {
        let tmp = tempfile::tempdir().unwrap();
        let layout = DataLayout::new(tmp.path().to_path_buf());
        let contribution = LaunchContribution {
            tools: vec!["__host_ui_emit__".into(), "read".into()],
            ..Default::default()
        };
        let plan = assemble_launch_plan(
            &fake_app("a", true),
            &manifest_min(),
            &contribution,
            &layout,
            Path::new("/ht"),
            true,
            &ModelLaunch::default(),
        )
        .unwrap();
        let tools = tools_arg(&plan);
        assert_eq!(tools.iter().filter(|t| *t == "read").count(), 1);
        assert_eq!(tools.iter().filter(|t| *t == "__host_ui_emit__").count(), 1);
    }

    // ---- F2（review）：--tools 拼装的防御带——任何单个工具名一旦含逗号就整条
    // 丢弃，绝不能让它打破 --tools 的逗号分隔契约、把额外工具名（如 bash）注入
    // pi 的白名单 ----

    #[test]
    fn launch_plan_drops_tool_names_containing_a_comma_defensively() {
        let tmp = tempfile::tempdir().unwrap();
        let layout = DataLayout::new(tmp.path().to_path_buf());
        // "x,bash" 模拟一个撒谎/写错的第三方 MCP server 上报的工具名——即便
        // connectors 能力自己的 is_safe_name 校验因为某种原因被绕过/遗漏，
        // assemble_launch_plan 也必须是最后一道防线。
        let contribution = LaunchContribution {
            tools: vec!["ok".into(), "x,bash".into()],
            ..Default::default()
        };
        let plan = assemble_launch_plan(
            &fake_app("a", false),
            &manifest_min(),
            &contribution,
            &layout,
            Path::new("/ht"),
            true,
            &ModelLaunch::default(),
        )
        .unwrap();
        let tools = tools_arg(&plan);
        assert!(tools.contains(&"ok".to_string()), "{tools:?}");
        assert!(
            !tools.iter().any(|t| t == "bash"),
            "脏工具名不得被逗号拆开注入白名单：{tools:?}"
        );
        assert!(!tools.iter().any(|t| t == "x,bash"), "{tools:?}");
    }

    /// P6-A 终审残留收口：此前逗号过滤只施于 `contribution.tools` 合并进来的
    /// 那部分（上面那条测试钉住的就是这条路径），`resolve_tools(&manifest.
    /// superagent.tools, ...)` 算出来的清单声明部分从未经过同一道过滤——
    /// `debug_assert!` 检查的却是合并后的整表，trusted/sandboxed 放宽路径下
    /// （`resolve_tools` 把 `declared.to_vec()` 原样放行）一个清单自己声明的
    /// 脏名字会让这条 `debug_assert!` 在 debug/test 构建下直接 panic——不是防
    /// 住了脏名字，是崩在了自检本身上。这条测试模拟"清单自己写错/被篡改"
    /// （trusted=true，走 relaxed 分支，declared 原样进 tools），钉住修复后
    /// 的行为：不 panic，且脏名字不出现在 --tools 里。
    #[test]
    fn launch_plan_drops_comma_tool_names_declared_in_manifest_without_panicking() {
        let tmp = tempfile::tempdir().unwrap();
        let layout = DataLayout::new(tmp.path().to_path_buf());
        let manifest: crate::pkg::Manifest = serde_json::from_value(serde_json::json!({
            "name": "@t/x", "version": "1.0.0", "keywords": ["superagent-app"], "engines": {"superagent-host": ">=1.0 <2.0"},
            "superagent": {
                "schemaVersion": 1, "displayName": "x", "category": "life",
                "ui": "ui/index.html", "permissions": "permissions.json",
                "tools": ["ok", "evil,bash"]
            }
        }))
        .unwrap();
        let plan = assemble_launch_plan(
            &fake_app("a", true),
            &manifest,
            &LaunchContribution::default(),
            &layout,
            Path::new("/ht"),
            true,
            &ModelLaunch::default(),
        )
        .unwrap();
        let tools = tools_arg(&plan);
        assert!(tools.contains(&"ok".to_string()), "{tools:?}");
        assert!(
            !tools.iter().any(|t| t == "bash"),
            "清单里带逗号的工具名不得被拆开注入白名单：{tools:?}"
        );
        assert!(!tools.iter().any(|t| t == "evil,bash"), "{tools:?}");
    }

    // ---- check_socket_invariant（P6-A review round-2 结构守卫）----

    #[test]
    fn check_socket_invariant_rejects_needs_socket_without_socket_path() {
        assert!(check_socket_invariant(None, true).is_err());
    }

    #[test]
    fn check_socket_invariant_allows_every_other_combination() {
        assert!(check_socket_invariant(None, false).is_ok());
        let p = Path::new("/tmp/x.sock");
        assert!(check_socket_invariant(Some(p), true).is_ok());
        assert!(check_socket_invariant(Some(p), false).is_ok());
    }

    #[test]
    fn launch_env_carries_app_id_for_gate_audit_payload() {
        // P2 新增：permission_gate.ts 需要 SUPERAGENT_APP_ID 才能在它上报的
        // advisory 审计事件里带上 app_id（见 build_launch 文档）。
        let tmp = tempfile::tempdir().unwrap();
        let layout = DataLayout::new(tmp.path().to_path_buf());
        let plan = build_launch(&app("my-app", true), &layout, Path::new("/ht")).unwrap();
        assert!(plan
            .env
            .iter()
            .any(|(k, v)| k == "SUPERAGENT_APP_ID" && v == "my-app"));
    }

    // ---- 模型启动注入 ----

    #[test]
    fn launch_plan_appends_model_launch_args_and_env() {
        let tmp = tempfile::tempdir().unwrap();
        let layout = DataLayout::new(tmp.path().to_path_buf());
        let ml = ModelLaunch {
            args: vec![
                "--provider".into(),
                "deepseek".into(),
                "--model".into(),
                "deepseek-v4-flash".into(),
            ],
            env: vec![("DEEPSEEK_API_KEY".into(), "sk-test-fake".into())],
            models_json: None,
        };
        let plan = assemble_launch_plan(
            &fake_app("a", false),
            &manifest_min(),
            &LaunchContribution::default(),
            &layout,
            Path::new("/ht"),
            true,
            &ml,
        )
        .unwrap();
        let pos = |x: &str| plan.extra_args.iter().position(|a| a == x).unwrap();
        assert_eq!(plan.extra_args[pos("--provider") + 1], "deepseek");
        assert_eq!(plan.extra_args[pos("--model") + 1], "deepseek-v4-flash");
        assert!(pos("--provider") < pos("--tools") && pos("--model") < pos("--tools"));
        assert!(plan
            .env
            .iter()
            .any(|(k, v)| k == "DEEPSEEK_API_KEY" && v == "sk-test-fake"));
    }

    #[test]
    fn launch_plan_with_default_model_launch_has_no_model_flag() {
        let tmp = tempfile::tempdir().unwrap();
        let layout = DataLayout::new(tmp.path().to_path_buf());
        let plan = assemble_launch_plan(
            &fake_app("a", false),
            &manifest_min(),
            &LaunchContribution::default(),
            &layout,
            Path::new("/ht"),
            true,
            &ModelLaunch::default(),
        )
        .unwrap();
        assert!(!plan
            .extra_args
            .iter()
            .any(|a| a == "--model" || a == "--provider"));
    }

    #[test]
    fn resolve_model_launch_reads_files_end_to_end() {
        let tmp = tempfile::tempdir().unwrap();
        let layout = DataLayout::new(tmp.path().to_path_buf());
        std::fs::write(
            layout.providers_path(),
            serde_json::json!({"version": 1, "custom": [{
                "id": "custom-mock", "display": "Mock",
                "base_url": "http://127.0.0.1:9/v1",
                "api": "openai-completions", "models": ["m1"]
            }]})
            .to_string(),
        )
        .unwrap();
        std::fs::write(
            layout.model_overrides_path(),
            serde_json::json!({"version": 1, "global": null,
                "apps": {"a": {"provider": "custom-mock", "model": "m1"}}})
            .to_string(),
        )
        .unwrap();
        let key = |_: &str| Some("sk-test-fake".to_string());
        let ml = resolve_model_launch_with(&layout, "a", &manifest_min(), key).unwrap();
        assert_eq!(ml.args, ["--provider", "custom-mock", "--model", "m1"]);
        assert!(ml
            .env
            .iter()
            .any(|(k, v)| k == "SUPERAGENT_KEY_CUSTOM_MOCK" && v == "sk-test-fake"));
        assert!(ml.models_json.is_some());
        // 另一个没有覆盖的应用：清单无 model 且无全局默认 -> 无参数。
        let ml = resolve_model_launch_with(&layout, "b", &manifest_min(), key).unwrap();
        assert!(ml.args.is_empty());
        // 损坏的文件报错并带上修复指引。
        std::fs::write(layout.model_overrides_path(), "{ bad").unwrap();
        let e = resolve_model_launch_with(&layout, "a", &manifest_min(), key).unwrap_err();
        assert!(
            e.contains("model-overrides.json") && e.contains("修复或删除"),
            "{e}"
        );
    }

    #[test]
    fn relaunch_inputs_reresolve_model_and_rewrite_agent_home() {
        let tmp = tempfile::tempdir().unwrap();
        let layout = DataLayout::new(tmp.path().to_path_buf());
        std::fs::write(
            layout.providers_path(),
            serde_json::json!({"version": 1, "custom": [{
                "id": "custom-mock", "display": "Mock",
                "base_url": "http://127.0.0.1:9/v1",
                "api": "openai-completions", "models": ["m1"]
            }]})
            .to_string(),
        )
        .unwrap();
        let set_override = |v: serde_json::Value| {
            std::fs::write(layout.model_overrides_path(), v.to_string()).unwrap();
        };
        let key = |_: &str| Some("sk-test-fake".to_string());
        let settings = serde_json::json!({"packages": []});
        let contribution = LaunchContribution {
            extra_args: vec!["--cx".into()],
            env: vec![("CX_ENV".into(), "1".into())],
            ..Default::default()
        };
        let rec = app("a", false);
        let ht = Path::new("/ht");
        let relaunch = || {
            relaunch_plan_with(
                &layout,
                &rec,
                &manifest_min(),
                &contribution,
                ht,
                true,
                &settings,
                key,
            )
        };
        let home = layout.agent_home_dir("a");
        let pos = |v: &[String], x: &str| v.iter().position(|a| a == x).unwrap();

        // 首次：覆盖到自定义 provider -> 带模型参数、密钥 env，并写出 models.json。
        set_override(serde_json::json!({"version": 1, "global": null,
            "apps": {"a": {"provider": "custom-mock", "model": "m1"}}}));
        let plan = relaunch().unwrap();
        // M3：与首次启动同一个拼装函数——整份计划逐项相等，而不是「基础参数 + 末尾追加」。
        let ml = resolve_model_launch_with(&layout, "a", &manifest_min(), key).unwrap();
        let first =
            assemble_launch_plan(&rec, &manifest_min(), &contribution, &layout, ht, true, &ml)
                .unwrap();
        assert_eq!(plan.extra_args, first.extra_args);
        assert_eq!(plan.env, first.env);
        assert_eq!(plan.sandbox_read, first.sandbox_read);
        assert_eq!(plan.sandbox_write, first.sandbox_write);
        // 顺序：persona/gate -> 模型参数 -> --tools -> 能力贡献的参数。
        let a = &plan.extra_args;
        assert!(pos(a, "-e") < pos(a, "--provider"));
        assert!(pos(a, "--model") < pos(a, "--tools"));
        assert!(pos(a, "--tools") < pos(a, "--cx"));
        assert_eq!(a[pos(a, "--provider") + 1], "custom-mock");
        assert_eq!(a[pos(a, "--model") + 1], "m1");
        assert!(plan.env.contains(&("CX_ENV".to_string(), "1".to_string())));
        assert!(plan
            .env
            .iter()
            .any(|(k, v)| k == "SUPERAGENT_KEY_CUSTOM_MOCK" && v == "sk-test-fake"));
        assert!(home.join("models.json").exists());

        // 用户之后把覆盖改成原生 provider：再次重启必须拿到新结果（新参数、不再带自定义密钥、
        // models.json 被清掉），而不是沿用首次启动的快照。
        set_override(serde_json::json!({"version": 1, "global": null,
            "apps": {"a": {"provider": "deepseek", "model": "deepseek-v4-flash"}}}));
        let plan = relaunch().unwrap();
        let a = &plan.extra_args;
        assert_eq!(a[pos(a, "--provider") + 1], "deepseek");
        assert_eq!(a[pos(a, "--model") + 1], "deepseek-v4-flash");
        assert!(!plan
            .env
            .iter()
            .any(|(k, _)| k == "SUPERAGENT_KEY_CUSTOM_MOCK"));
        assert!(!home.join("models.json").exists());

        // 配置损坏：返回 Err（调用方走退避失败路径），不静默沿用旧值。
        std::fs::write(layout.model_overrides_path(), "{ bad").unwrap();
        assert!(relaunch().is_err());
    }

    /// C1：应用在崩溃前把自己的 sessions/agenthome/apps 目录换成指向受害目录的符号链接，
    /// 退避重启走的 `relaunch_plan_with` 必须拒绝（不重新授权），且受害目录无任何写入。
    #[test]
    fn relaunch_rejects_swapped_private_dirs_and_leaves_victim_untouched() {
        for kind in ["sessions", "agenthome", "apps"] {
            let tmp = tempfile::tempdir().unwrap();
            let layout = DataLayout::new(tmp.path().to_path_buf());
            let rec = app("a", false);
            let settings = serde_json::json!({"packages": []});
            let ht = Path::new("/ht");
            let call = || {
                relaunch_plan_with(
                    &layout,
                    &rec,
                    &manifest_min(),
                    &LaunchContribution::default(),
                    ht,
                    true,
                    &settings,
                    |_: &str| None,
                )
            };
            call().expect("正常目录应能重启");
            let victim = tmp.path().join("victim");
            std::fs::create_dir_all(&victim).unwrap();
            let dir = layout.private_dir(kind, "a").unwrap();
            std::fs::rename(&dir, tmp.path().join(format!("{kind}-old"))).unwrap();
            std::os::unix::fs::symlink(&victim, &dir).unwrap();
            let e = call().err().expect("应被拒绝");
            assert!(e.contains("符号链接"), "{kind}: {e}");
            assert_eq!(std::fs::read_dir(&victim).unwrap().count(), 0, "{kind}");
        }
    }

    #[test]
    fn write_agent_home_removes_stale_models_json() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("agenthome").join("a");
        std::fs::create_dir_all(&home).unwrap();
        let settings = serde_json::json!({"packages": []});
        let models = serde_json::json!({"providers": {}});
        let h = test_handle(&home);
        write_agent_home_files(&h, &settings, Some(&models)).unwrap();
        assert!(home.join("models.json").exists());
        assert!(home.join("settings.json").exists());
        write_agent_home_files(&h, &settings, None).unwrap();
        assert!(!home.join("models.json").exists());
        assert!(home.join("settings.json").exists());
        // 本就没有时也不报错
        write_agent_home_files(&h, &settings, None).unwrap();
    }

    #[test]
    fn write_agent_home_does_not_follow_symlinks_c2() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("agenthome").join("a");
        std::fs::create_dir_all(&home).unwrap();
        let victim_s = tmp.path().join("victim-settings");
        let victim_m = tmp.path().join("victim-models");
        std::fs::write(&victim_s, "VICTIM-S").unwrap();
        std::fs::write(&victim_m, "VICTIM-M").unwrap();
        std::os::unix::fs::symlink(&victim_s, home.join("settings.json")).unwrap();
        std::os::unix::fs::symlink(&victim_m, home.join("models.json")).unwrap();
        let settings = serde_json::json!({"packages": []});
        let models = serde_json::json!({"providers": {}});
        let h = test_handle(&home);
        write_agent_home_files(&h, &settings, Some(&models)).unwrap();
        assert_eq!(std::fs::read_to_string(&victim_s).unwrap(), "VICTIM-S");
        assert_eq!(std::fs::read_to_string(&victim_m).unwrap(), "VICTIM-M");
        for n in ["settings.json", "models.json"] {
            let m = std::fs::symlink_metadata(home.join(n)).unwrap();
            assert!(m.file_type().is_file(), "{n} 应变为普通文件");
        }
        let got: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(home.join("settings.json")).unwrap())
                .unwrap();
        assert_eq!(got, settings);
        // models.json 为链接时删除只删链接本身
        std::fs::remove_file(home.join("models.json")).unwrap();
        std::os::unix::fs::symlink(&victim_m, home.join("models.json")).unwrap();
        write_agent_home_files(&h, &settings, None).unwrap();
        assert!(!home.join("models.json").exists());
        assert_eq!(std::fs::read_to_string(&victim_m).unwrap(), "VICTIM-M");
    }

    fn test_handle(dir: &Path) -> crate::dirfd::DirHandle {
        let id = crate::dirfd::identity_of_real_dir(dir).unwrap();
        crate::dirfd::DirHandle::open_expecting(dir, id).unwrap()
    }

    /// I-a：拿到目录句柄之后、写入之前，应用把目录改名并在原位放一个指向受害目录的链接。
    /// 句柄指向的仍是原目录：文件写进被改名的原目录，受害目录里什么都没有。
    #[test]
    fn write_agent_home_via_handle_survives_swap_after_open_ia() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("agenthome").join("a");
        let moved = tmp.path().join("agenthome").join("a-moved");
        let victim = tmp.path().join("victim");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&victim).unwrap();
        std::fs::write(home.join("models.json"), "OLD").unwrap();
        let h = test_handle(&home);
        // 竞态窗口：校验、打开之后，路径被换成链接。
        std::fs::rename(&home, &moved).unwrap();
        std::os::unix::fs::symlink(&victim, &home).unwrap();
        let settings = serde_json::json!({"packages": []});
        let models = serde_json::json!({"providers": {}});
        write_agent_home_files(&h, &settings, Some(&models)).unwrap();
        assert_eq!(
            std::fs::read_dir(&victim).unwrap().count(),
            0,
            "受害目录被写入"
        );
        assert!(moved.join("settings.json").exists());
        // 删除 models.json 同样只作用在原目录。
        std::fs::write(victim.join("models.json"), "VICTIM").unwrap();
        write_agent_home_files(&h, &settings, None).unwrap();
        assert!(!moved.join("models.json").exists());
        assert_eq!(
            std::fs::read_to_string(victim.join("models.json")).unwrap(),
            "VICTIM"
        );
    }

    /// I-a：校验记下身份之后、打开之前被换成链接 / 换成别的真目录 → 打开被拒。
    #[test]
    fn open_dir_handle_rejects_swap_before_open_ia() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("a");
        let victim = tmp.path().join("victim");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&victim).unwrap();
        let id = crate::dirfd::identity_of_real_dir(&home).unwrap();
        // 换成链接
        std::fs::rename(&home, tmp.path().join("a-moved")).unwrap();
        std::os::unix::fs::symlink(&victim, &home).unwrap();
        assert!(crate::dirfd::DirHandle::open_expecting(&home, id).is_err());
        // 换成另一个真目录（身份不同）
        std::fs::remove_file(&home).unwrap();
        std::fs::create_dir(&home).unwrap();
        assert!(crate::dirfd::DirHandle::open_expecting(&home, id).is_err());
        assert_eq!(std::fs::read_dir(&victim).unwrap().count(), 0);
    }

    /// C2：agent home 目录本身被换成指向受害目录的符号链接时，`write_agent_home` 拒绝，
    /// 受害目录里不会出现 settings.json / models.json。
    #[test]
    fn write_agent_home_rejects_symlinked_home_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let layout = DataLayout::new(tmp.path().to_path_buf());
        let victim = tmp.path().join("victim");
        std::fs::create_dir_all(&victim).unwrap();
        std::fs::create_dir_all(tmp.path().join("agenthome")).unwrap();
        std::os::unix::fs::symlink(&victim, tmp.path().join("agenthome/a")).unwrap();
        let settings = serde_json::json!({"packages": []});
        let models = serde_json::json!({"providers": {}});
        assert!(write_agent_home(&layout, "a", &settings, Some(&models)).is_err());
        assert_eq!(std::fs::read_dir(&victim).unwrap().count(), 0);
    }

    /// I-b：暂存目录（或其 agent home / session 子目录）被换成指向受害目录的链接后发起预览
    /// → 拒绝、受害目录无写入。校验发生在拉起子进程之前，不需要真实 pi。
    #[tokio::test]
    async fn preview_rejects_symlinked_staging_and_subdirs_ib() {
        let tmp = tempfile::tempdir().unwrap();
        let layout = DataLayout::new(tmp.path().to_path_buf());
        let victim = tmp.path().join("victim");
        std::fs::create_dir_all(&victim).unwrap();
        // 1) 草稿暂存目录本身是链接
        std::fs::create_dir_all(tmp.path().join("maker-staging")).unwrap();
        std::os::unix::fs::symlink(&victim, tmp.path().join("maker-staging/d1")).unwrap();
        assert!(spawn_preview_session(&layout, "d1").await.is_err());
        // 2) agent home / session 子目录是链接
        for sub in [PREVIEW_AGENT_HOME_SUBDIR, PREVIEW_SESSION_SUBDIR] {
            let st = layout.checked_maker_staging_dir("d2").unwrap();
            for other in [PREVIEW_AGENT_HOME_SUBDIR, PREVIEW_SESSION_SUBDIR] {
                let _ = std::fs::remove_file(st.join(other));
                let _ = std::fs::remove_dir_all(st.join(other));
            }
            std::os::unix::fs::symlink(&victim, st.join(sub)).unwrap();
            let e = spawn_preview_session(&layout, "d2").await.unwrap_err();
            assert!(e.contains("符号链接") || e.contains("真实目录"), "{e}");
        }
        assert_eq!(std::fs::read_dir(&victim).unwrap().count(), 0);
    }

    /// M-2：预览期间暂存目录被换成链接后，清理不得删到链接目标里。
    #[test]
    fn cleanup_preview_scratch_dirs_does_not_follow_swapped_staging_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let layout = DataLayout::new(tmp.path().to_path_buf());
        // 正常：两个子目录被清掉。
        let st = layout.checked_maker_staging_dir("d").unwrap();
        for sub in [PREVIEW_AGENT_HOME_SUBDIR, PREVIEW_SESSION_SUBDIR] {
            std::fs::create_dir_all(st.join(sub)).unwrap();
        }
        cleanup_preview_scratch_dirs(&layout, "d");
        assert!(!st.join(PREVIEW_AGENT_HOME_SUBDIR).exists());
        assert!(!st.join(PREVIEW_SESSION_SUBDIR).exists());
        // 暂存目录被换成指向受害目录的链接：受害目录里的同名子目录不能被删。
        let victim = tmp.path().join("victim");
        for sub in [PREVIEW_AGENT_HOME_SUBDIR, PREVIEW_SESSION_SUBDIR] {
            std::fs::create_dir_all(victim.join(sub)).unwrap();
        }
        std::fs::remove_dir_all(&st).unwrap();
        std::os::unix::fs::symlink(&victim, &st).unwrap();
        cleanup_preview_scratch_dirs(&layout, "d");
        for sub in [PREVIEW_AGENT_HOME_SUBDIR, PREVIEW_SESSION_SUBDIR] {
            assert!(victim.join(sub).exists(), "{sub} 被误删");
        }
    }

    // ---- audit_verdict_for_tool_execution ----

    #[test]
    fn audit_verdict_is_allow_when_not_error() {
        assert_eq!(audit_verdict_for_tool_execution(false), "allow");
    }

    #[test]
    fn audit_verdict_is_error_when_tool_reports_error() {
        assert_eq!(audit_verdict_for_tool_execution(true), "error");
    }
}
