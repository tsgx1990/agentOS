// P3 Task15b【计划外·接线缺口】：调度器运行时接线。
//
// 缺口回顾：`Scheduler::tick`（Task12）与 `NotificationStore::record_task_result`
// （Task15）都已存在，但没有任何生产路径周期性调用 `tick`——只有应用打开时的
// `run_catch_up_for_app`（Task13）会跑一次。本任务补的是：
// 1. `tick` 的 running-only 边界（P3 §10）：一个 app 的定时任务只在它**当前
//    打开**时才由周期 tick 触发；关闭期间到期的任务不触发，留给它下次打开时
//    的 catch_up 补跑。
// 2. `tick` 结果 -> `NotificationStore` 的胶水（`scheduler::run_scheduler_tick_cycle`），
//    供 `lib.rs` 的后台周期循环（`SCHEDULER_TICK_INTERVAL`）调用。
//
// 本文件两条用例合起来钉住这两件事：
// - `open_app_due_task_fires_and_records_notification`：due 任务的 app 在
//   `is_app_open` 判定的打开集合里 -> 真的拉起 task-mode 会话（mock_pi 收到
//   prompt、走到 agent_end）、`mark_run` 生效、且在 `NotificationStore` 里能
//   查到一条对应的 `task_result` 通知。
// - `closed_app_due_task_does_not_fire_or_notify`：due 任务的 app 不在打开集合
//   里 -> 不触发任何 task-mode 会话（不需要 `SUPERAGENT_PI_BIN`，若真的触发了
//   会因为找不到 mock_pi 而在结果里体现为 errored 或直接改变 last_run，均可被
//   下面的断言捕获）、任务保持未 `mark_run`（仍可在下次该 app 打开时被
//   catch_up 补跑）、`NotificationStore` 里没有它的 `task_result` 通知。
//
// 全程直接构造 `RegistryStore`/`TaskRegistry`/`NotificationStore`（`upsert`/
// `register`/`McpManager::new()`），不碰 `vault::*`（真实 keychain 会导致
// `cargo test` 卡死，见 `scheduler_it.rs`/`progress.md` Task1 记录的教训）。
// `TestClock` 注入，全程不碰真实墙钟；`run_scheduler_tick_cycle` 直接调用一次
// （不睡 `SCHEDULER_TICK_INTERVAL`），验证"一次 tick 周期"本身的行为，不依赖
// 真实的 30s 后台循环。
use std::collections::HashSet;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};
use tokio::sync::Mutex;

use super_agent_os::mcp::McpManager;
use super_agent_os::notifications::{NotificationFilter, NotificationStore};
use super_agent_os::paths::DataLayout;
use super_agent_os::permissions::ScheduledTask;
use super_agent_os::registry::{InstalledApp, RegistryStore};
use super_agent_os::scheduler::{
    run_scheduler_tick_cycle, Clock, Scheduler, TaskRegistry, TestClock,
};

// `SUPERAGENT_PI_BIN`/`MOCK_PI_MODE` 是进程级全局状态，`cargo test` 默认并行跑
// 同一测试二进制内的多个测试线程——串行化访问，同 `scheduler_it.rs` 的既有惯例。
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

fn hosttools_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("hosttools")
}

/// 把一组静态字符串包成 `is_app_open` 谓词，模拟 `AppState::app_sessions` 的
/// key 集合——生产调用方（`lib.rs::start_scheduler_loop`）从真实的
/// `app_sessions` 取 key 构造同形状的闭包，这里直接给定一个固定集合即可，
/// 不需要真的起一个 tauri `AppState`/pi 会话。
fn open_set(ids: &[&str]) -> impl Fn(&str) -> bool + Send + Sync {
    let set: HashSet<String> = ids.iter().map(|s| s.to_string()).collect();
    move |id: &str| set.contains(id)
}

#[tokio::test]
async fn open_app_due_task_fires_and_records_notification() {
    let _guard = ENV_LOCK.lock().await;
    std::env::set_var("SUPERAGENT_PI_BIN", env!("CARGO_BIN_EXE_mock_pi"));

    let tmp = tempfile::tempdir().unwrap();
    let layout = DataLayout::new(tmp.path().to_path_buf());

    let apps = RegistryStore::new(layout.registry_path());
    apps.upsert(installed_app("app-open")).unwrap();

    let treg = TaskRegistry::new(&layout);
    treg.register("app-open", &[task("daily", "写个简报")])
        .unwrap();
    assert!(
        treg.all()[0].last_run.is_none(),
        "前置：新注册任务 last_run 应为 None"
    );

    let clock = TestClock::new(SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000));
    let scheduler = Scheduler::new(
        layout.clone(),
        apps,
        hosttools_dir(),
        McpManager::new(),
        Duration::ZERO,
    );
    let notifications = NotificationStore::new(layout.clone(), McpManager::new());
    let is_open = open_set(&["app-open"]);

    run_scheduler_tick_cycle(&scheduler, &clock, &notifications, &is_open).await;

    std::env::remove_var("SUPERAGENT_PI_BIN");

    // 任务真的被拉起（task-mode 会话跑完）：mark_run 生效，last_run 落到本轮 now。
    let all = TaskRegistry::new(&layout).all();
    assert_eq!(all.len(), 1);
    assert_eq!(
        all[0].last_run,
        Some(clock.now()),
        "打开的 app 的到期任务应被本轮 tick 拉起并 mark_run"
    );

    // 结果被转存进 NotificationStore：能查到一条对应的 task_result 通知。
    let notifs = notifications.list(&NotificationFilter {
        kind: Some("task_result".into()),
        ..Default::default()
    });
    assert_eq!(
        notifs.len(),
        1,
        "打开的 app 的任务结果应产生 1 条 task_result 通知"
    );
    assert_eq!(notifs[0].app_id, "app-open");
    assert!(notifs[0].title.contains("daily"));
    assert_eq!(
        notifs[0].body, "你好，世界",
        "通知正文应是 mock_pi 产出的助手文本"
    );
}

#[tokio::test]
async fn closed_app_due_task_does_not_fire_or_notify() {
    let _guard = ENV_LOCK.lock().await;
    // 刻意不设置 SUPERAGENT_PI_BIN：若 running-only 过滤失效、真的尝试拉起
    // task-mode 会话，会因为找不到 pi 可执行文件而报错——但更直接的证据是下面
    // 对 last_run 与通知列表的断言，不依赖这次"意外触发"具体失败成什么样子。

    let tmp = tempfile::tempdir().unwrap();
    let layout = DataLayout::new(tmp.path().to_path_buf());

    let apps = RegistryStore::new(layout.registry_path());
    apps.upsert(installed_app("app-closed")).unwrap();

    let treg = TaskRegistry::new(&layout);
    treg.register("app-closed", &[task("daily", "写个简报")])
        .unwrap();

    let clock = TestClock::new(SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000));
    let scheduler = Scheduler::new(
        layout.clone(),
        apps,
        hosttools_dir(),
        McpManager::new(),
        Duration::ZERO,
    );
    let notifications = NotificationStore::new(layout.clone(), McpManager::new());
    // "app-closed" 不在打开集合里——空集合，等价于"当前没有任何 app 打开"。
    let is_open = open_set(&[]);

    run_scheduler_tick_cycle(&scheduler, &clock, &notifications, &is_open).await;

    // 任务未被拉起：last_run 仍是 None（未被 mark_run 过），保持"到期未跑"状态，
    // 留给该 app 下次打开时的 catch_up 补跑。
    let all = TaskRegistry::new(&layout).all();
    assert_eq!(all.len(), 1);
    assert!(
        all[0].last_run.is_none(),
        "关闭的 app 的到期任务不该被本轮 tick 触发，last_run 应保持 None"
    );

    // 没有产生任何通知。
    let notifs = notifications.list(&NotificationFilter::default());
    assert!(
        notifs.is_empty(),
        "关闭的 app 不该产生任何通知（该任务的执行被 running-only 边界挡住）"
    );
}

/// 混合场景：同一轮 tick 里，打开的 app 的到期任务被触发+记录通知，关闭的 app
/// 的到期任务被挡住——证明过滤是逐任务的，不是"只要有一个 app 打开就全跑"或
/// "只要有一个 app 关闭就全不跑"这种全局开关式的粗糙实现。
#[tokio::test]
async fn mixed_open_and_closed_apps_only_open_ones_fire() {
    let _guard = ENV_LOCK.lock().await;
    std::env::set_var("SUPERAGENT_PI_BIN", env!("CARGO_BIN_EXE_mock_pi"));

    let tmp = tempfile::tempdir().unwrap();
    let layout = DataLayout::new(tmp.path().to_path_buf());

    let apps = RegistryStore::new(layout.registry_path());
    apps.upsert(installed_app("app-open")).unwrap();
    apps.upsert(installed_app("app-closed")).unwrap();

    let treg = TaskRegistry::new(&layout);
    treg.register("app-open", &[task("daily", "写个简报")])
        .unwrap();
    treg.register("app-closed", &[task("daily", "写个简报")])
        .unwrap();

    let clock = TestClock::new(SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000));
    let scheduler = Scheduler::new(
        layout.clone(),
        apps,
        hosttools_dir(),
        McpManager::new(),
        Duration::ZERO,
    );
    let notifications = NotificationStore::new(layout.clone(), McpManager::new());
    let is_open = open_set(&["app-open"]);

    run_scheduler_tick_cycle(&scheduler, &clock, &notifications, &is_open).await;

    std::env::remove_var("SUPERAGENT_PI_BIN");

    let all = TaskRegistry::new(&layout).all();
    let open_task = all.iter().find(|t| t.app_id == "app-open").unwrap();
    let closed_task = all.iter().find(|t| t.app_id == "app-closed").unwrap();
    assert_eq!(
        open_task.last_run,
        Some(clock.now()),
        "打开的 app 的任务应被触发"
    );
    assert!(
        closed_task.last_run.is_none(),
        "关闭的 app 的任务不该被触发"
    );

    let notifs = notifications.list(&NotificationFilter {
        kind: Some("task_result".into()),
        ..Default::default()
    });
    assert_eq!(
        notifs.len(),
        1,
        "只应有打开的那个 app 产生 1 条 task_result 通知"
    );
    assert_eq!(notifs[0].app_id, "app-open");
}
