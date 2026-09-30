// P3 Task12 里程碑：调度器执行核（`scheduler::Scheduler::tick`）——到期任务→
// 全局并发闸门→task-mode（headless）pi 拉起→mark_run→结果收集。
//
// 三条用例合起来钉住本任务的完整安全/功能属性：
// 1. `tick_launches_task_mode_session_drives_mock_pi_and_marks_run`：单个到期
//    任务端到端走通——`tick` 真的拉起一个 task-mode 会话，`mock_pi` 收到 prompt
//    并回 `agent_end`，`mark_run` 被调（`last_run` 更新），且产出一条结果（本任务
//    与 Task15 通知中心之间的最小 seam）。
// 2. `tick_batches_task_mode_sessions_under_concurrency_cap`：3 个同时到期任务、
//    `MAX_CONCURRENT_TASKS=2`→分批跑，断言同时在跑数从未超过上限、且确实达到过
//    上限（证明并发闸门真的生效，不是侥幸串行）。
// 3. `task_mode_launch_reuses_p2_sandbox_wrapping_argv0_is_sandbox_exec`（仅
//    macOS）：断言 task-mode 拉起复用的是 P2 唯一的沙盒包裹入口
//    `session_mgr::sandboxed_argv`，其产出的可执行文件字面是 `/usr/bin/sandbox-exec`
//    ——同 `sandbox_pipe_it.rs`/`sandbox_escape_it.rs` 的做法：直接断言"这条真实
//    生产函数产出的可执行文件"，而不是试图在运行期从进程外部窥探已 fork 出去的
//    子进程 argv[0]（macOS 没有稳定、无需 root 的这类反射 API）。结合用例1（走的
//    正是 `spawn_task_session`→`spawn_app_session`，其 macOS 分支无条件调用这个
//    同一个函数，见 `session_mgr.rs` 代码——没有第二条分支可选），两条用例合起来
//    证明"task-mode 后台任务与前台交互会话同权同沙盒"这条安全不变式。
//
// 全程直接构造 `RegistryStore`/`TaskRegistry`（`upsert`/`register`），不碰
// `vault::*`——`vault` 的生产自由函数会撞真实 macOS 交互式钥匙串提示，曾在 Task1
// 令 `cargo test` 无限卡死（见 `progress.md` Task1 记录的"跨任务约束"）。
// `TestClock`：全程不碰真实墙钟（`SystemTime::now()`），与 `scheduler.rs` 已有
// 测试同规格。
//
// Task17b 补充两条用例（文件尾部）：
// 4. `tick_task_mode_session_reuses_foreground_mcp_listener_for_app_with_connectors`：
//    该 app 声明了 filesystem 只读连接器，且已经像 `open_app` 那样为它 bind 好了
//    前台 `McpSocketListener`——`tick` 拉起的 task-mode 会话不应起第二个监听器
//    （否则同路径 bind 会 `AddrInUse`/或把前台监听器的 socket 文件误删），且前台
//    监听器在 tick 跑完后仍然可用（真实走一次 MCP 往返验证）。
// 5.（仅 macOS）`task_mode_mcp_injection_sandbox_profile_carries_socket_allow`：
//    task-mode 内核（P6-A：`run_headless_session` 经 `CapabilityRegistry::launch`
//    算出的贡献）决定的 socket 路径喂给 `sandboxed_argv` 后，沙盒 profile/argv 确实
//    带上 Task9c 的 MCP socket 窄放行——与用例3（`mcp_socket=None` 分支）互补，合起来
//    证明"有 MCP 时窄放行确实带上、没 MCP 时沙盒包裹本身不变"。
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};
use tokio::sync::Mutex;

use super_agent_os::mcp::McpManager;
use super_agent_os::mcp_socket::McpSocketListener;
use super_agent_os::paths::DataLayout;
use super_agent_os::permissions::{Access, ConnectorReq, ScheduledTask};
use super_agent_os::registry::{InstalledApp, RegistryStore};
use super_agent_os::scheduler::{
    run_catch_up_for_app, Clock, Scheduler, TaskRegistry, TestClock, MAX_CONCURRENT_TASKS,
};
use super_agent_os::vault::ServerConfig;

// `SUPERAGENT_PI_BIN`/`MOCK_PI_MODE` 是进程级全局状态（`std::env::set_var`/
// `remove_var`），`cargo test` 默认并行跑同一测试二进制内的多个测试线程——不
// 序列化访问会让本文件的两个 `#[tokio::test]` 互相踩环境变量（已在
// `e2e_milestone.rs`/`sandbox_pipe_it.rs`/`pi_bin.rs` 反复踩过、注释过的教训：
// mock_pi 拿到错的 `MOCK_PI_MODE` 后不会输出期望的事件，它处理完一行 stdin 后仍
// 阻塞等下一行，`rx.recv()` 永远等不到 `None`，测试挂起而非快速失败）。
static ENV_LOCK: std::sync::LazyLock<Mutex<()>> = std::sync::LazyLock::new(|| Mutex::new(()));

fn installed_app(id: &str) -> InstalledApp {
    InstalledApp {
        app_id: id.into(),
        name: id.into(),
        version: "1.0.0".into(),
        display_name: id.into(),
        category: "life".into(),
        icon: None,
        trusted: false,
        domains: vec![],
    }
}

fn task(id: &str, prompt: &str) -> ScheduledTask {
    ScheduledTask {
        id: id.into(),
        cron: "* * * * *".into(),
        prompt: prompt.into(),
        catch_up: true,
    }
}

/// dev 树里 hosttools 的真实路径——只用于拼 `-e <path>/ui_emit.ts` 之类的字符串
/// 参数（`mock_pi` 完全不读 argv，见 `mock_pi.rs`），不要求文件真的存在。
fn hosttools_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("hosttools")
}

#[tokio::test]
async fn tick_launches_task_mode_session_drives_mock_pi_and_marks_run() {
    let _guard = ENV_LOCK.lock().await;
    std::env::set_var("SUPERAGENT_PI_BIN", env!("CARGO_BIN_EXE_mock_pi"));

    let tmp = tempfile::tempdir().unwrap();
    let layout = DataLayout::new(tmp.path().to_path_buf());
    let layout_for_assertions = layout.clone();

    let apps = RegistryStore::new(layout.registry_path());
    apps.upsert(installed_app("app1")).unwrap();

    let treg = TaskRegistry::new(&layout);
    treg.register("app1", &[task("daily", "写个简报")]).unwrap();
    assert!(
        treg.all()[0].last_run.is_none(),
        "前置：新注册任务 last_run 应为 None"
    );

    // last_run=None → epoch 兜底 → 立刻可到期（同 scheduler.rs 已有的
    // `due_tasks_task_with_no_last_run_uses_epoch_baseline_and_is_immediately_due`）。
    let clock = TestClock::new(SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000));

    let scheduler = Scheduler::new(
        layout,
        apps,
        hosttools_dir(),
        McpManager::new(),
        Duration::ZERO,
    );
    let results = scheduler.tick(&clock, &|_: &str| true).await;

    std::env::remove_var("SUPERAGENT_PI_BIN");

    assert_eq!(results.len(), 1, "1 个到期任务应产出 1 条结果");
    assert_eq!(results[0].app_id, "app1");
    assert_eq!(results[0].task_id, "daily");
    assert!(
        !results[0].errored,
        "mock_pi 默认 mode 正常走完，不应标记为出错"
    );
    assert_eq!(
        results[0].text, "你好，世界",
        "task-mode 会话应把 mock_pi 的助手文本收全"
    );

    let all = TaskRegistry::new(&layout_for_assertions).all();
    assert_eq!(all.len(), 1);
    assert_eq!(
        all[0].last_run,
        Some(clock.now()),
        "mark_run 应把 last_run 落到本轮 tick 的 now"
    );
}

/// P3 Task13：`run_catch_up_for_app` 走的是 `Scheduler::tick`（用例1）已经端到端
/// 钉住的同一条 task-mode 派发路径（`session_mgr::spawn_task_session`），只是
/// 触发时机不同（应用打开时补跑一次，不是常规 tick 循环）——这里再走一遍
/// mock_pi，证明补跑真的拉起了会话（收到 prompt、走到 `agent_end`）且真的
/// `mark_run` 了（`last_run` 落到调用时的 `now`），不是被静默丢弃。
#[tokio::test]
async fn catch_up_dispatches_missed_task_through_task_mode_path_and_marks_run() {
    let _guard = ENV_LOCK.lock().await;
    std::env::set_var("SUPERAGENT_PI_BIN", env!("CARGO_BIN_EXE_mock_pi"));

    let tmp = tempfile::tempdir().unwrap();
    let layout = DataLayout::new(tmp.path().to_path_buf());
    let app = installed_app("app1");

    let treg = TaskRegistry::new(&layout);
    treg.register("app1", &[task("daily", "补跑一下")]).unwrap();

    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    // 模拟"app 关闭期间错过了不止一次触发"：last_run 设在很久以前——
    // `* * * * *` 每分钟一次，错过的自然周期数远大于 1。
    treg.mark_run("app1", "daily", now - Duration::from_secs(1_000_000))
        .unwrap();
    let clock = TestClock::new(now);

    let results =
        run_catch_up_for_app(&layout, &hosttools_dir(), &McpManager::new(), &app, &clock).await;

    std::env::remove_var("SUPERAGENT_PI_BIN");

    assert_eq!(results.len(), 1, "1 个错过的任务应产出 1 条补跑结果");
    assert_eq!(results[0].app_id, "app1");
    assert_eq!(results[0].task_id, "daily");
    assert!(!results[0].errored);
    assert_eq!(
        results[0].text, "你好，世界",
        "补跑应真的拉起 task-mode 会话跑到 agent_end"
    );

    let all = TaskRegistry::new(&layout).all();
    assert_eq!(all.len(), 1);
    assert_eq!(
        all[0].last_run,
        Some(now),
        "补跑应像 Scheduler::tick 一样调 mark_run，把 last_run 落到调用时的 now"
    );
}

/// 补充：任务本身没有错过任何触发窗口（刚跑完）时，`run_catch_up_for_app`
/// 不应该拉起任何 task-mode 会话——用 `last_run` 不产生 mock_pi 依赖即可验证
/// （不到期就不会走到 `spawn_task_session`，不需要 `SUPERAGENT_PI_BIN`）。
#[tokio::test]
async fn catch_up_wiring_is_noop_when_task_is_not_overdue() {
    let _guard = ENV_LOCK.lock().await;

    let tmp = tempfile::tempdir().unwrap();
    let layout = DataLayout::new(tmp.path().to_path_buf());
    let app = installed_app("app1");

    let treg = TaskRegistry::new(&layout);
    treg.register("app1", &[task("daily", "补跑一下")]).unwrap();

    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    treg.mark_run("app1", "daily", now).unwrap(); // 刚跑完，未到期
    let clock = TestClock::new(now);

    let results =
        run_catch_up_for_app(&layout, &hosttools_dir(), &McpManager::new(), &app, &clock).await;

    assert!(
        results.is_empty(),
        "未到期的任务不该被补跑，也就不会产出任何结果"
    );
    let all = TaskRegistry::new(&layout).all();
    assert_eq!(
        all[0].last_run,
        Some(now),
        "mark_run 不该被多余地调用，last_run 应保持原值"
    );
}

#[tokio::test]
async fn tick_batches_task_mode_sessions_under_concurrency_cap() {
    let _guard = ENV_LOCK.lock().await;
    std::env::set_var("SUPERAGENT_PI_BIN", env!("CARGO_BIN_EXE_mock_pi"));
    std::env::set_var("MOCK_PI_MODE", "slow");

    let tmp = tempfile::tempdir().unwrap();
    let layout = DataLayout::new(tmp.path().to_path_buf());

    let apps = RegistryStore::new(layout.registry_path());
    let treg = TaskRegistry::new(&layout);
    for id in ["app1", "app2", "app3"] {
        apps.upsert(installed_app(id)).unwrap();
        treg.register(id, &[task("t", "慢任务")]).unwrap();
    }

    let clock = TestClock::new(SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000));
    let scheduler = Scheduler::new(
        layout,
        apps,
        hosttools_dir(),
        McpManager::new(),
        Duration::ZERO,
    );

    let results = scheduler.tick(&clock, &|_: &str| true).await;

    std::env::remove_var("MOCK_PI_MODE");
    std::env::remove_var("SUPERAGENT_PI_BIN");

    assert_eq!(
        results.len(),
        3,
        "3 个到期任务分批跑完，最终都应产出结果（不丢弃）"
    );
    assert!(
        results.iter().all(|r| !r.errored),
        "慢任务也应正常走到 agent_end，不应出错"
    );

    assert!(
        scheduler.peak_in_flight() <= MAX_CONCURRENT_TASKS,
        "并发闸门必须生效：同时在跑的 task-mode 会话数({})不能超过上限({})",
        scheduler.peak_in_flight(),
        MAX_CONCURRENT_TASKS
    );
    assert_eq!(
        scheduler.peak_in_flight(),
        MAX_CONCURRENT_TASKS,
        "3 个 due 任务、cap={} 时应真的达到过上限并发——否则无法排除\"意外串行、\
         并发闸门形同虚设\"这个反面可能",
        MAX_CONCURRENT_TASKS
    );
}

/// 仅 macOS：断言 task-mode 拉起复用的是 P2 唯一沙盒包裹入口，见文件头部文档。
#[cfg(target_os = "macos")]
#[test]
fn task_mode_launch_reuses_p2_sandbox_wrapping_argv0_is_sandbox_exec() {
    use super_agent_os::session_mgr::sandboxed_argv;

    let tmp = tempfile::tempdir().unwrap();
    let layout = DataLayout::new(tmp.path().to_path_buf());
    layout.ensure_app("app1").unwrap();
    let app_data_dir = layout.app_data_dir("app1");

    // 与 `spawn_task_session` 对该 app 实际调用 `spawn_app_session`（进而调用
    // `sandboxed_argv`）时会传入的参数同形状：`trusted=false`（`installed_app`
    // 的默认值）、`extra_args` 任意（这里传空，不影响 bin 的选择——`sandboxed_argv`
    // 内 `bin` 恒为 `/usr/bin/sandbox-exec`，与 extra_args 内容无关）、
    // `mcp_socket=None`——这里的 `app1` 在这个全新 tempdir 里没有安装任何真实包
    // （没有 `package.json`），`spawn_task_session` 内部的 headless 内核
    // （P6-A：`run_headless_session` 经 `CapabilityRegistry::launch`）读清单会
    // 失败而退化为默认权限，算出的贡献 `needs_socket` 为假（见该函数文档"与
    // 前台刻意不同"一节），等价于此处显式传 `None`；断言的是"argv0 恒为
    // sandbox-exec"这条与 mcp_socket 取值无关的属性——有 MCP 时窄放行确实带上
    // 这一半由下面 `task_mode_mcp_injection_sandbox_profile_carries_socket_allow`
    // 补完。
    let (bin, argv) =
        sandboxed_argv(&app_data_dir, false, &[], None, &[], &[]).expect("sandboxed_argv 不应失败");

    assert_eq!(
        bin, "/usr/bin/sandbox-exec",
        "task-mode 拉起必须复用 P2 的 sandbox-exec 包裹路径，不得是裸 spawn（安全不变式）"
    );
    let sep = argv
        .iter()
        .position(|a| a == "--")
        .expect("sandbox-exec argv 必须有 -- 分隔符");
    assert!(
        argv.len() > sep + 1,
        "分隔符 -- 之后必须跟着被包裹的目标命令（pi/mock_pi）"
    );
}

/// 仅 macOS：task-mode 内核（P6-A：`run_headless_session` 经
/// `CapabilityRegistry::launch` 算出的贡献）决定的 socket 路径喂给
/// `sandboxed_argv` 后，task-mode 的沙盒 profile/argv 必须带上 Task9c 的 MCP
/// socket 窄放行——与上一条用例（`mcp_socket=None` 分支）互补，合起来证明
/// "没有 MCP 时沙盒包裹字节不变、有 MCP 时窄放行确实带上"。
///
/// 用真实 `UnixListener::bind` 而不是普通文件占位——同 `sandbox.rs` 已有的
/// `build_profile_with_mcp_socket_path_adds_param_and_allow` 测试做法：
/// `build_profile` 内部要 `canonicalize` 这个路径，必须真实存在；用真实 bind
/// 出来的 socket 也更贴近"这是 `open_app` 早先起的前台监听器"这个真实场景。
#[cfg(target_os = "macos")]
#[test]
fn task_mode_mcp_injection_sandbox_profile_carries_socket_allow() {
    use super_agent_os::session_mgr::sandboxed_argv;

    let tmp = tempfile::tempdir().unwrap();
    let layout = DataLayout::new(tmp.path().to_path_buf());
    layout.ensure_app("app1").unwrap();
    let app_data_dir = layout.app_data_dir("app1");

    let socket_path = layout.mcp_socket_path("app1");
    std::fs::create_dir_all(socket_path.parent().unwrap()).unwrap();
    let _listener = std::os::unix::net::UnixListener::bind(&socket_path)
        .expect("模拟前台 McpSocketListener 已经 bind 好的路径");

    let (bin, argv) = sandboxed_argv(&app_data_dir, false, &[], Some(&socket_path), &[], &[])
        .expect("sandboxed_argv 不应失败");

    assert_eq!(bin, "/usr/bin/sandbox-exec");
    let canon = std::fs::canonicalize(&socket_path)
        .unwrap()
        .to_string_lossy()
        .to_string();
    assert!(
        argv.iter().any(|a| a == &format!("-DMCP_SOCK={canon}")),
        "task-mode 沙盒 argv 应含 MCP socket 窄放行参数，实际：{argv:?}"
    );
    let profile = argv
        .iter()
        .find(|a| a.contains("network-outbound"))
        .expect("应有一个 argv 参数（-p 后的 profile 文本）包含窄放行规则");
    assert!(
        profile.contains(r#"(allow network-outbound (literal (param "MCP_SOCK")))"#),
        "profile 应含 MCP_SOCK 的 network-outbound literal 放行，实际：{profile}"
    );
}

/// 往 `layout.packages_dir(app_id)` 写一份最小合法清单 + 指定的 permissions.json
/// 内容——同 `session_mgr_mcp_it.rs` 里的同名助手，供下面的端到端用例安装一个
/// 声明了 connectors 的真实 app（`spawn_task_session` 内部的 headless 内核
/// `run_headless_session` 需要真的读到这两个文件才能算出非空的 MCP 贡献）。
fn write_task_app_package(layout: &DataLayout, app_id: &str, permissions_json: &str) {
    let dir = layout.packages_dir(app_id);
    std::fs::create_dir_all(dir.join("ui")).unwrap();
    std::fs::write(dir.join("ui/index.html"), "<html></html>").unwrap();
    std::fs::write(dir.join("permissions.json"), permissions_json).unwrap();
    let pkg = serde_json::json!({
        "name": app_id,
        "version": "1.0.0",
        "keywords": ["pi-package", "superagent-app"],
        "engines": { "superagent-host": ">=1.0.0, <2.0.0" },
        "superagent": {
            "schemaVersion": 1,
            "displayName": app_id,
            "category": "life",
            "ui": "ui/index.html",
            "permissions": "permissions.json",
        }
    });
    std::fs::write(
        dir.join("package.json"),
        serde_json::to_string_pretty(&pkg).unwrap(),
    )
    .unwrap();
}

/// Task17b 端到端主张：一个声明了 filesystem 连接器的 app，若已经像 `open_app`
/// 那样有一个前台 `McpSocketListener` bind 在 `layout.mcp_socket_path(app_id)`
/// 上（模拟 running-only 边界保证的"该 app 当前打开"状态），`Scheduler::tick`
/// 拉起的 task-mode 会话：
/// 1. 不应因为"也想要 MCP"而在同一路径上再起一个监听器（`McpSocketListener::start`
///    的"先删旧文件再 bind"清理逻辑一旦被第二次触发，会把前台监听器正在用的
///    socket 文件删掉——这里显式地验证前台监听器在 tick 跑完后依然可用，间接
///    证明了这一点没有发生）；
/// 2. mock_pi 端到端仍然正常跑完、`mark_run` 生效——加了 MCP 注入之后 task-mode
///    派发路径本身不受影响。
#[tokio::test]
async fn tick_task_mode_session_reuses_foreground_mcp_listener_for_app_with_connectors() {
    let _guard = ENV_LOCK.lock().await;
    std::env::set_var("SUPERAGENT_PI_BIN", env!("CARGO_BIN_EXE_mock_pi"));

    let tmp = tempfile::tempdir().unwrap();
    let layout = DataLayout::new(tmp.path().to_path_buf());

    let manager = McpManager::new();
    let cfg = ServerConfig {
        id: "t17b-fs".to_string(),
        category: "filesystem".to_string(),
        command: env!("CARGO_BIN_EXE_mock_mcp_server").to_string(),
        args: vec![],
        env: BTreeMap::new(),
        transport: "stdio".into(),
        trust: Default::default(),
    };
    manager
        .ensure_server(&cfg)
        .await
        .expect("ensure_server 应成功");

    write_task_app_package(
        &layout,
        "app1",
        r#"{"connectors":[{"category":"filesystem","access":"read"}]}"#,
    );

    // 模拟 open_app 早先为这个（"当前打开"的）app bind 好的前台 MCP 监听器——
    // running-only 边界保证 tick 只会对这样的 app 派发到期任务。
    let socket_path = layout.mcp_socket_path("app1");
    let listener = McpSocketListener::start(
        manager.clone(),
        layout.clone(),
        "app1".to_string(),
        vec![ConnectorReq {
            category: "filesystem".to_string(),
            access: Access::Read,
        }],
        socket_path.clone(),
    )
    .expect("前台监听器应能成功 bind");

    let apps = RegistryStore::new(layout.registry_path());
    apps.upsert(installed_app("app1")).unwrap();
    let treg = TaskRegistry::new(&layout);
    treg.register("app1", &[task("daily", "写个简报")]).unwrap();

    let clock = TestClock::new(SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000));
    let scheduler = Scheduler::new(
        layout.clone(),
        apps,
        hosttools_dir(),
        manager,
        Duration::ZERO,
    );

    let results = scheduler.tick(&clock, &|_: &str| true).await;

    std::env::remove_var("SUPERAGENT_PI_BIN");

    assert_eq!(results.len(), 1, "1 个到期任务应产出 1 条结果");
    assert!(
        !results[0].errored,
        "加了 MCP 注入后 task-mode 派发不应受影响，仍应正常跑完"
    );
    assert_eq!(results[0].text, "你好，世界");

    let all = TaskRegistry::new(&layout).all();
    assert_eq!(all[0].last_run, Some(clock.now()));

    // 前台监听器在 tick 跑完后依然可用——若 task-mode 曾经错误地在同一路径上
    // 再起一个监听器（先删文件再 bind），这里的连接会失败或收不到合法响应。
    let stream = tokio::net::UnixStream::connect(&socket_path)
        .await
        .expect("前台监听器应仍在监听，未被 task-mode 误删/顶替");
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let (r, mut w) = stream.into_split();
    let req = serde_json::json!({
        "method": "__host_mcp_call__",
        "params": { "server": "t17b-fs", "tool": "read_file", "args": {} }
    });
    w.write_all(format!("{req}\n").as_bytes())
        .await
        .expect("写请求应成功");
    let mut reader = BufReader::new(r);
    let mut line = String::new();
    reader.read_line(&mut line).await.expect("应能读到一行响应");
    let resp: serde_json::Value = serde_json::from_str(&line).expect("响应应是合法 JSON");
    assert!(
        resp.get("result").is_some(),
        "前台监听器应仍能正常处理请求，实际：{resp}"
    );

    listener.stop().await;
}
