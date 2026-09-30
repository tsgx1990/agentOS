// P3 Task18（收官任务）：里程碑自动化验证 —— 用两个真实格式的第三方应用
// fixture（`notes-reader` / `notes-writer`）证明"连接（MCP 桥）+ 调度（定时
// 任务）"这两条各自独立完工的 P3 半区确实**组合**在一起工作，而不是分别测过
// 就假设它们能拼上。
//
// 场景设定：
// - App A = `notes-reader`：untrusted 第三方包，只声明 `connectors:
//   [{category:"filesystem", access:"read"}]`——一个只应该能"看笔记"的应用。
// - App B = `notes-writer`：untrusted 第三方包，声明
//   `connectors:[{category:"filesystem", access:"readwrite"}]` +
//   `system.schedule:true` + 一条 `scheduledTasks`（"笔记摘要"任务，id
//   `notes-digest`）——既能读写笔记，也有一个每天定时生成摘要的后台任务。
//
// 两个 fixture 装的是**同一个** filesystem MCP server（`mock_mcp_server`，
// Task4 的替身，暴露 `read_file`(Danger::Read) / `write_file`(Danger::Write)
// 两个工具）：这正是"一个连接器被两个 app 按各自声明的 access 共享、且互相
// 隔离"这条 P3 核心主张的最小验证场景。
//
// 全程直接构造 `McpManager`/`RegistryStore`/`TaskRegistry`（`ensure_server`/
// `upsert`/`register`），**不**碰 `vault::*` 生产自由函数——那些会触达真实
// macOS 交互式钥匙串提示，曾在 Task1 让 `cargo test` 无限挂起（见
// `progress.md` Task1"跨任务约束"记录，`mcp_manager_it.rs`/`scheduler_it.rs`
// 等既有集成测试全部遵守同一条禁令）。`ServerConfig` 手工构造，`command` 指向
// 真实编译出的 `mock_mcp_server` bin（`CARGO_BIN_EXE_` 环境变量）。
//
// 两个 fixture 本身走**真实安装路径**（`install::install_from_dir`，同
// `restricted_it.rs` 的做法）落位进临时 `DataLayout`，再用生产的
// `permissions::load`/`scheduler::register_scheduled_tasks_if_permitted` 读出
// 它们的权限声明——不是在测试里手工拼一份"看起来像"的 `ConnectorReq`/
// `ScheduledTask`，而是让这两个 fixture 的 `package.json`/`permissions.json`
// 真正被现有的包加载器/权限解析器/安装器解析、校验、落盘一遍。
//
// 四个焦点测试（覆盖 Task18 brief 的三条硬性主张）：
// 1. `fixtures_notes_reader_and_notes_writer_parse_and_install_...`：两个
//    fixture 本身是合法、能装、字段符合预期的第三方包（回归钉子，同
//    `e2e_milestone.rs::mock_malicious_manifest_parses_as_valid_package`/
//    `mock_malicious_fixture_it.rs` 的做法）。
// 2. `shared_filesystem_connector_is_isolated_per_app_by_declared_access`：
//    共享同一个已连接 filesystem server，App A 的 `authorized_tools` 只有
//    `read_file`，App B 两个都有——一个 server、两份互相隔离的可见性。
// 3a/3b：写确认门 + 隔离——App B 写操作 → `PendingConfirm`（未执行）→ 通知
//    中心 `respond_confirm(allow=true)` 续行才真正执行；App A 尝试写操作 →
//    `Denied`（对它整个不可见/不可调），全程走真实的宿主 unix socket 监听器
//    （`McpSocketListener`，同 `mcp_socket_it.rs` 的线协议），不是绕过传输层
//    直接调内部 API。
// 4. `scheduled_notes_digest_fires_while_app_open_and_produces_task_result_...`：
//    App B 清单里的 `notes-digest` 定时任务被真实注册进 `TaskRegistry`，
//    `TestClock` 拨到到期之后、模拟"App B 当前打开"（`is_app_open` 判真 +
//    该 app 已有前台 `McpSocketListener`，同 Task17b/`scheduler_running_only_it.rs`
//    的"打开"语义），`run_scheduler_tick_cycle` 拉起 task-mode 会话
//    （mock_pi）跑到 `agent_end`，产出一条 `task_result` 通知；且前台连接器
//    在定时任务跑完后依然可用——证明"调度"确实是在"连接"已经接好的地基上跑，
//    不是两条互不知情的平行流水线。
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};
use tokio::sync::Mutex;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use super_agent_os::install;
use super_agent_os::mcp::{Danger, McpManager};
use super_agent_os::mcp_socket::McpSocketListener;
use super_agent_os::notifications::{NotificationFilter, NotificationStore};
use super_agent_os::paths::DataLayout;
use super_agent_os::permissions::{self, Access};
use super_agent_os::registry::{InstalledApp, RegistryStore};
use super_agent_os::scheduler::{
    register_scheduled_tasks_if_permitted, run_scheduler_tick_cycle, Clock, Scheduler,
    TaskRegistry, TestClock,
};
use super_agent_os::vault::ServerConfig;

// `SUPERAGENT_PI_BIN` 是进程级全局状态（`std::env::set_var`/`remove_var`），
// `cargo test` 默认并行跑同一测试二进制内的多个测试线程——本文件只有一个测试
// 触碰它（Task4：`scheduled_notes_digest_...`），但仍上锁，同
// `scheduler_it.rs`/`e2e_milestone.rs` 的既有惯例，防止未来添加同类测试时
// 悄悄踩踏。
static ENV_LOCK: std::sync::LazyLock<Mutex<()>> = std::sync::LazyLock::new(|| Mutex::new(()));

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn hosttools_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("hosttools")
}

fn temp_layout() -> (tempfile::TempDir, DataLayout) {
    let tmp = tempfile::tempdir().unwrap();
    let layout = DataLayout::new(tmp.path().to_path_buf());
    (tmp, layout)
}

fn mock_fs_server_config(id: &str) -> ServerConfig {
    ServerConfig {
        id: id.to_string(),
        category: "filesystem".to_string(),
        command: env!("CARGO_BIN_EXE_mock_mcp_server").to_string(),
        args: vec![],
        env: BTreeMap::new(),
        transport: "stdio".into(),
        trust: Default::default(),
    }
}

/// 把 `tests/fixtures/<name>` 走真实安装路径（`install::install_from_dir`，同
/// `restricted_it.rs` 的做法）落位进 `layout`——不是手工拼一份"看起来像"的
/// `InstalledApp`，而是让这两个 fixture 真的经过包加载器校验/复制/原子落位/
/// registry 落盘一遍，证明它们是货真价实、装得进去的第三方包。
fn install_fixture(layout: &DataLayout, registry: &RegistryStore, name: &str) -> InstalledApp {
    let src = fixtures_dir().join(name);
    install::install_from_dir(&src, layout, registry, false)
        .unwrap_or_else(|e| panic!("fixture `{name}` 应能作为合法第三方包被安装，实际：{e}"))
}

/// 手写的假客户端：复刻 `mcp_transport.ts::hostMcpCall` 的线协议——每次调用
/// 新开一条连接，写一行 JSON 请求，读一行 JSON 响应后关闭连接（同
/// `mcp_socket_it.rs` 的做法，这里不重复该文件已覆盖的编码细节测试，只用它
/// 驱动本文件的组合场景）。
async fn fake_client_call(
    socket_path: &Path,
    server: &str,
    tool: &str,
    args: serde_json::Value,
) -> serde_json::Value {
    let stream = tokio::net::UnixStream::connect(socket_path)
        .await
        .unwrap_or_else(|e| panic!("连接 {socket_path:?} 应成功：{e}"));
    let (r, mut w) = stream.into_split();

    let req = serde_json::json!({
        "method": "__host_mcp_call__",
        "params": { "server": server, "tool": tool, "args": args }
    });
    w.write_all(format!("{req}\n").as_bytes())
        .await
        .expect("写请求应成功");

    let mut reader = BufReader::new(r);
    let mut line = String::new();
    reader.read_line(&mut line).await.expect("应能读到一行响应");
    serde_json::from_str(&line).unwrap_or_else(|e| panic!("响应应是合法 JSON，实际 {line:?}：{e}"))
}

// ---------------------------------------------------------------------------
// 1. fixture 本身：合法、可安装、权限声明符合预期（回归钉子）
// ---------------------------------------------------------------------------

#[test]
fn fixtures_notes_reader_and_notes_writer_parse_and_install_with_declared_connectors_and_schedule()
{
    let (_tmp, layout) = temp_layout();
    let registry = RegistryStore::new(layout.registry_path());

    let reader = install_fixture(&layout, &registry, "notes-reader");
    let writer = install_fixture(&layout, &registry, "notes-writer");

    assert!(!reader.trusted, "notes-reader 是未标记 trusted 的第三方包");
    assert!(!writer.trusted, "notes-writer 是未标记 trusted 的第三方包");
    assert_eq!(reader.app_id, "notes-reader");
    assert_eq!(writer.app_id, "notes-writer");
    assert_eq!(reader.display_name, "笔记查看器");
    assert_eq!(writer.display_name, "笔记摘要助手");

    let perms_a = permissions::load(&layout.packages_dir(&reader.app_id), "permissions.json")
        .expect("notes-reader 的 permissions.json 应能被解析");
    assert_eq!(perms_a.connectors.len(), 1);
    assert_eq!(perms_a.connectors[0].category, "filesystem");
    assert_eq!(
        perms_a.connectors[0].access,
        Access::Read,
        "App A 只应声明只读访问"
    );
    assert!(!perms_a.system.schedule, "App A 不应声明调度权限");
    assert!(
        perms_a.scheduled_tasks.is_empty(),
        "App A 不应声明任何定时任务"
    );

    let perms_b = permissions::load(&layout.packages_dir(&writer.app_id), "permissions.json")
        .expect("notes-writer 的 permissions.json 应能被解析");
    assert_eq!(perms_b.connectors.len(), 1);
    assert_eq!(perms_b.connectors[0].category, "filesystem");
    assert_eq!(
        perms_b.connectors[0].access,
        Access::ReadWrite,
        "App B 应声明读写访问"
    );
    assert!(perms_b.system.schedule, "App B 应声明调度权限");
    assert_eq!(perms_b.scheduled_tasks.len(), 1);
    assert_eq!(perms_b.scheduled_tasks[0].id, "notes-digest");
    assert!(
        perms_b.scheduled_tasks[0].prompt.contains("摘要"),
        "定时任务 prompt 应是笔记摘要类描述，实际：{}",
        perms_b.scheduled_tasks[0].prompt
    );
    assert!(
        perms_b.scheduled_tasks[0].catch_up,
        "省略 catchUp 应落回默认 true"
    );
}

// ---------------------------------------------------------------------------
// 2. 共享连接器 + 可见性隔离：一个已连接 filesystem server，两个 app 各自
//    `authorized_tools` 结果按 access 收窄，互不相同。
// ---------------------------------------------------------------------------

#[tokio::test]
async fn shared_filesystem_connector_is_isolated_per_app_by_declared_access() {
    let manager = McpManager::new();
    let cfg = mock_fs_server_config("notes-fs-shared");
    manager
        .ensure_server(&cfg)
        .await
        .expect("filesystem MCP server 应能连接成功");

    let (_tmp, layout) = temp_layout();
    let registry = RegistryStore::new(layout.registry_path());
    let reader = install_fixture(&layout, &registry, "notes-reader");
    let writer = install_fixture(&layout, &registry, "notes-writer");

    let perms_a =
        permissions::load(&layout.packages_dir(&reader.app_id), "permissions.json").unwrap();
    let perms_b =
        permissions::load(&layout.packages_dir(&writer.app_id), "permissions.json").unwrap();

    let authed_a = manager.authorized_tools(&perms_a.connectors);
    assert_eq!(
        authed_a.len(),
        1,
        "只读 App A 应恰好看到 1 个工具，实际：{authed_a:?}"
    );
    assert_eq!(authed_a[0].tool, "read_file");
    assert!(
        authed_a.iter().all(|t| t.danger == Danger::Read),
        "只读 App A 绝不应看到任何 Danger::Write 工具，实际：{authed_a:?}"
    );
    assert!(
        !authed_a.iter().any(|t| t.tool == "write_file"),
        "write_file 对只读 App A 必须完全不可见，实际：{authed_a:?}"
    );

    let authed_b = manager.authorized_tools(&perms_b.connectors);
    assert_eq!(
        authed_b.len(),
        2,
        "读写 App B 应看到两个工具，实际：{authed_b:?}"
    );
    assert!(authed_b.iter().any(|t| t.tool == "read_file"));
    assert!(authed_b.iter().any(|t| t.tool == "write_file"));

    // 两个 app 用的是同一个 server 实例（spawn_count 仍是 1），不是各自连了
    // 一份互不知情的 MCP server——这才是"共享一个连接器"的实质证据。
    assert_eq!(
        manager.spawn_count(),
        1,
        "App A/App B 应共享同一份已连接的 filesystem MCP server，不应各自触发 spawn"
    );
}

// ---------------------------------------------------------------------------
// 3a. App B 写操作：PendingConfirm（未执行）-> 通知中心可见 ->
//     respond_confirm(allow=true) 续行才真正执行。全程走真实宿主 socket。
// ---------------------------------------------------------------------------

#[tokio::test]
async fn notes_writer_write_file_is_pending_then_executes_only_after_respond_confirm_allow() {
    let manager = McpManager::new();
    let cfg = mock_fs_server_config("notes-fs-confirm");
    manager
        .ensure_server(&cfg)
        .await
        .expect("filesystem MCP server 应能连接成功");

    let (_tmp, layout) = temp_layout();
    let registry = RegistryStore::new(layout.registry_path());
    let writer = install_fixture(&layout, &registry, "notes-writer");
    let perms_b =
        permissions::load(&layout.packages_dir(&writer.app_id), "permissions.json").unwrap();

    let socket_path = layout.mcp_socket_path(&writer.app_id);
    let listener = McpSocketListener::start(
        manager.clone(),
        layout.clone(),
        writer.app_id.clone(),
        perms_b.connectors,
        socket_path.clone(),
    )
    .expect("App B 的前台 MCP 监听器应能成功 bind");

    let resp = fake_client_call(
        &socket_path,
        &cfg.id,
        "write_file",
        serde_json::json!({ "path": "/notes/today.md", "content": "今日摘要" }),
    )
    .await;

    assert!(
        resp.get("error").is_none(),
        "授权的写操作不应走 error 分支，实际：{resp:?}"
    );
    let result = resp.get("result").expect("PendingConfirm 应落在 result 里");
    assert_eq!(result["pending_confirm"], serde_json::json!(true));
    let confirm_id = result["confirm_id"]
        .as_str()
        .expect("应带 confirm_id 字段")
        .to_string();
    assert_eq!(
        manager.call_count(&cfg.id),
        0,
        "确认之前，写操作绝不应被真正执行"
    );

    let notifications = NotificationStore::new(layout.clone(), manager.clone());
    let pending = notifications.list(&NotificationFilter {
        kind: Some("confirm_request".into()),
        app_id: Some(writer.app_id.clone()),
        ..Default::default()
    });
    assert_eq!(
        pending.len(),
        1,
        "应在通知中心看到这条待确认，实际：{pending:?}"
    );
    assert_eq!(pending[0].id, confirm_id, "通知 id 必须就是 confirmId");
    assert!(!pending[0].acked);

    let outcome = notifications
        .respond_confirm(&confirm_id, true, false)
        .await
        .expect("respond_confirm(allow=true) 应成功续行执行");
    assert!(outcome.is_some(), "allow=true 应返回被续行调用的结果");
    assert_eq!(
        manager.call_count(&cfg.id),
        1,
        "respond_confirm(allow=true) 之后，写操作应恰好被执行一次"
    );

    let acked = notifications.list(&NotificationFilter {
        kind: Some("confirm_request".into()),
        app_id: Some(writer.app_id.clone()),
        ..Default::default()
    });
    assert!(
        acked[0].acked,
        "respond_confirm 之后，对应的待确认通知应被标记已读"
    );

    listener.stop().await;
}

// ---------------------------------------------------------------------------
// 3b. App A（只读）尝试写操作：对它不可见/不可调，Denied，绝不执行、绝不产生
//     待确认通知——与 3a 的 App B 形成对照，证明隔离不只是"看不见"，调用路径
//     本身也被挡住。
// ---------------------------------------------------------------------------

#[tokio::test]
async fn notes_reader_write_file_attempt_is_denied_and_never_executed() {
    let manager = McpManager::new();
    let cfg = mock_fs_server_config("notes-fs-denied");
    manager
        .ensure_server(&cfg)
        .await
        .expect("filesystem MCP server 应能连接成功");

    let (_tmp, layout) = temp_layout();
    let registry = RegistryStore::new(layout.registry_path());
    let reader = install_fixture(&layout, &registry, "notes-reader");
    let perms_a =
        permissions::load(&layout.packages_dir(&reader.app_id), "permissions.json").unwrap();

    let socket_path = layout.mcp_socket_path(&reader.app_id);
    let listener = McpSocketListener::start(
        manager.clone(),
        layout.clone(),
        reader.app_id.clone(),
        perms_a.connectors,
        socket_path.clone(),
    )
    .expect("App A 的前台 MCP 监听器应能成功 bind");

    // 只读 app 的合法读取仍应正常工作——隔离不是"这个 app 干脆连不上 MCP"。
    let read_resp = fake_client_call(
        &socket_path,
        &cfg.id,
        "read_file",
        serde_json::json!({ "path": "/notes/a.md" }),
    )
    .await;
    assert!(
        read_resp.get("error").is_none(),
        "只读 app 的 read_file 应正常成功，实际：{read_resp:?}"
    );
    assert_eq!(manager.call_count(&cfg.id), 1);

    // 写操作必须被拒绝——不是卡在待确认，是 Denied（unauthorized）。
    let write_resp = fake_client_call(
        &socket_path,
        &cfg.id,
        "write_file",
        serde_json::json!({ "path": "/notes/a.md", "content": "x" }),
    )
    .await;
    let error = write_resp
        .get("error")
        .expect("只读 app 请求写操作应走 error/Denied 分支");
    assert!(error.is_string() && !error.as_str().unwrap().is_empty());
    assert!(
        write_resp.get("result").is_none(),
        "Denied 不应同时带 result，实际：{write_resp:?}"
    );
    assert_eq!(
        manager.call_count(&cfg.id),
        1,
        "写操作绝不应被执行，call_count 应仍停在前面那次 read_file"
    );

    let notifications = NotificationStore::new(layout.clone(), manager.clone());
    let confirm_notifs = notifications.list(&NotificationFilter {
        kind: Some("confirm_request".into()),
        ..Default::default()
    });
    assert!(
        confirm_notifs.is_empty(),
        "未授权拒绝不应产生任何待确认通知，实际：{confirm_notifs:?}"
    );

    listener.stop().await;
}

// ---------------------------------------------------------------------------
// 4. 定时笔记摘要：App B 清单里的 scheduledTasks -> 真实注册进 TaskRegistry ->
//    TestClock 拨过到期时刻、App B "打开"（is_app_open 判真 + 已有前台 MCP
//    监听器）-> run_scheduler_tick_cycle 拉起 task-mode 会话（mock_pi）跑到
//    agent_end -> 产出 task_result 通知；且前台连接器在任务跑完后依然可用。
// ---------------------------------------------------------------------------

#[tokio::test]
async fn scheduled_notes_digest_fires_while_app_open_and_produces_task_result_notification() {
    let _guard = ENV_LOCK.lock().await;
    std::env::set_var("SUPERAGENT_PI_BIN", env!("CARGO_BIN_EXE_mock_pi"));

    let manager = McpManager::new();
    let cfg = mock_fs_server_config("notes-fs-digest");
    manager
        .ensure_server(&cfg)
        .await
        .expect("filesystem MCP server 应能连接成功");

    let (_tmp, layout) = temp_layout();
    let registry = RegistryStore::new(layout.registry_path());
    let writer = install_fixture(&layout, &registry, "notes-writer");
    let perms_b =
        permissions::load(&layout.packages_dir(&writer.app_id), "permissions.json").unwrap();

    // 模拟 open_app 的两件事：①按清单权限注册 scheduledTasks（Task14b 的生产
    // 函数，非测试自己拼一份任务）；②为该 app 起前台 MCP 监听器（Task9b/9c）
    // ——running-only 边界下"打开"意味着这两者同时成立。
    register_scheduled_tasks_if_permitted(
        &layout,
        &writer.app_id,
        perms_b.system.schedule,
        &perms_b.scheduled_tasks,
    )
    .expect("应能按 system.schedule 权限注册清单里的定时任务");

    let socket_path = layout.mcp_socket_path(&writer.app_id);
    let listener = McpSocketListener::start(
        manager.clone(),
        layout.clone(),
        writer.app_id.clone(),
        perms_b.connectors,
        socket_path.clone(),
    )
    .expect("前台 MCP 监听器应能成功 bind");

    let treg = TaskRegistry::new(&layout);
    let registered = treg.all();
    assert_eq!(
        registered.len(),
        1,
        "notes-digest 任务应已从清单注册进任务表"
    );
    assert_eq!(registered[0].id, "notes-digest");
    assert!(
        registered[0].last_run.is_none(),
        "前置：新注册任务 last_run 应为 None"
    );

    // last_run=None -> epoch 兜底 -> 立刻可到期（同 scheduler.rs 已有测试
    // 的既定行为），不需要真的等到清单里 "0 8 * * *" 那个自然时刻。
    let clock = TestClock::new(SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000));
    let scheduler = Scheduler::new(
        layout.clone(),
        registry,
        hosttools_dir(),
        manager.clone(),
        Duration::ZERO,
    );
    let notifications = NotificationStore::new(layout.clone(), manager.clone());

    let open_app_id = writer.app_id.clone();
    let is_open = move |id: &str| id == open_app_id.as_str();

    run_scheduler_tick_cycle(&scheduler, &clock, &notifications, &is_open).await;

    std::env::remove_var("SUPERAGENT_PI_BIN");

    let all = TaskRegistry::new(&layout).all();
    assert_eq!(all.len(), 1);
    assert_eq!(
        all[0].last_run,
        Some(clock.now()),
        "打开的 App B 的到期任务应被本轮 tick 拉起并 mark_run"
    );

    let task_notifs = notifications.list(&NotificationFilter {
        kind: Some("task_result".into()),
        app_id: Some(writer.app_id.clone()),
        ..Default::default()
    });
    assert_eq!(
        task_notifs.len(),
        1,
        "应产出恰好 1 条 task_result 通知，实际：{task_notifs:?}"
    );
    assert!(
        task_notifs[0].title.contains("notes-digest"),
        "通知标题应带上任务 id，实际：{}",
        task_notifs[0].title
    );
    assert_eq!(
        task_notifs[0].body, "你好，世界",
        "通知正文应是 task-mode 会话（mock_pi）产出的助手文本"
    );

    // 前台连接器在定时任务跑完之后依然可用——证明"调度"是在"连接"已经接好的
    // 地基上跑（task-mode 复用同一个前台监听器，不是另起炉灶/顶替它）。
    let still_alive = fake_client_call(
        &socket_path,
        &cfg.id,
        "read_file",
        serde_json::json!({ "path": "/notes/a.md" }),
    )
    .await;
    assert!(
        still_alive.get("result").is_some(),
        "定时任务跑完后前台连接器仍应可正常处理请求，实际：{still_alive:?}"
    );

    listener.stop().await;
}
