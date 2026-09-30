//! P3 定时任务调度：本文件只解决"时钟从哪来"+"注册了哪些任务、上次跑到哪"两件事，
//! 真正的 cron 触发/派发循环是 Task 11+ 在此基础上建。
//!
//! `Clock`：调度逻辑（含它的测试）禁止碰真实墙钟（项目级规定，见 audit.rs 保留期
//! 用「文件名日期」而非 mtime 的同一动机）——生产用 `SystemClock`，测试用
//! `TestClock`（`Mutex<SystemTime>`，手动 `advance`）。
//!
//! `TaskRegistry`：把每个 app 通过权限清单 `scheduledTasks` 声明的定时任务
//! （`permissions::ScheduledTask`）+ 运行状态（`last_run`）持久化到 host data
//! 目录下的单个 JSON 文件（`paths::DataLayout::scheduler_tasks_path`）。落盘模式
//! 与 `registry.rs::RegistryStore`（已装应用索引）同款：整份 `Vec<T>` 序列化，
//! 「读全量 -> 内存改 -> 写 `.tmp` -> `rename`」原子替换。**同一 host 进程内**
//! 的并发调用者（`Scheduler::tick` 在 `MAX_CONCURRENT_TASKS` 个并发 task-mode
//! 会话各自跑完后独立 `mark_run`）现在靠 `registry_lock_for` 提供的按路径共享
//! 锁串行化整个「读改写」，不再是丢更新的 last-writer-wins（历史 bug：两个
//! 几乎同时跑完的任务各自 load 到同一份旧文件，后 save 覆盖先 save，导致其中
//! 一个任务的 `last_run` 悄悄丢失、下一轮又被判到期重跑）。真正的多**进程**
//! 写者（多个独立 host 进程同时改同一份文件）仍不在本任务范围。

use crate::mcp::McpManager;
use crate::notifications::NotificationStore;
use crate::paths::DataLayout;
use crate::permissions::ScheduledTask;
use crate::registry::{InstalledApp, RegistryStore};
use crate::session_mgr;
use chrono::{DateTime, Local};
use cron::Schedule as CronSchedule;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, SystemTime};
use tokio::sync::Semaphore;

// ---------------------------------------------------------------------------
// Clock：注入式时钟
// ---------------------------------------------------------------------------

/// 调度器消费的时钟接口。生产用 `SystemClock`，测试用 `TestClock`——调度逻辑
/// 本身不得直接调用 `SystemTime::now()`，全部经这个 trait 注入，保证测试
/// 确定性（不依赖真实时间流逝）。
///
/// 显式要求 `Send + Sync`（Task13 新增约束，`SystemClock`/`TestClock` 两个既有
/// 实现天然都满足）：`run_catch_up_for_app` 被 `session_mgr::open_app` 包进
/// `tokio::spawn` 的后台任务里调用（不阻塞应用打开），其生成的 `Future` 要求
/// `Send`——若途中借用的 `&dyn Clock` 不是 `Send`（需要 `dyn Clock: Sync`），
/// 整个外层 `tokio::spawn` 的 future 就编译不过。不加这个约束，`Clock` 仍能
/// 在纯同步测试/`Scheduler::tick` 里正常工作，但会在"从另一个 tokio::spawn
/// 内部调用一个内部也 tokio::spawn 的 async fn 并借用 `&dyn Clock` 跨越那次
/// `.await`"这个新场景下编译失败。
pub trait Clock: Send + Sync {
    fn now(&self) -> SystemTime;
}

/// 生产实现：直接转发到 `SystemTime::now()`。
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> SystemTime {
        SystemTime::now()
    }
}

/// 测试替身：持有一个可手动推进的 `SystemTime`，测试里用 `advance` 模拟时间
/// 流逝，而不必真的 sleep 或依赖墙钟。
pub struct TestClock {
    now: Mutex<SystemTime>,
}

impl TestClock {
    pub fn new(start: SystemTime) -> Self {
        Self {
            now: Mutex::new(start),
        }
    }

    /// 把内部时钟向前推进 `d`。
    pub fn advance(&self, d: Duration) {
        let mut guard = self.now.lock().expect("TestClock mutex poisoned");
        *guard += d;
    }
}

impl Clock for TestClock {
    fn now(&self) -> SystemTime {
        *self.now.lock().expect("TestClock mutex poisoned")
    }
}

// ---------------------------------------------------------------------------
// RegisteredTask / TaskRegistry
// ---------------------------------------------------------------------------

/// 持久化的单条定时任务记录：`app_id` + 清单声明字段（`id`/`cron`/`prompt`/
/// `catch_up`）+ 运行状态（`last_run`，首次注册为 `None`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RegisteredTask {
    pub app_id: String,
    pub id: String,
    pub cron: String,
    pub prompt: String,
    pub catch_up: bool,
    #[serde(default)]
    pub last_run: Option<SystemTime>,
}

/// 进程内按落盘路径分片的锁表：同一个 `scheduler_tasks_path()` 对应同一把
/// `Mutex<()>`，谁拿到这把锁谁才能做 `load -> modify -> save`。
///
/// 关键点：锁必须按**路径**共享，不能挂在 `TaskRegistry` 实例自己头上——
/// `Scheduler::tick`（见下）里每个并发到期任务各自 `TaskRegistry::new(&layout)`
/// 现造一个新实例再 `mark_run`，这些实例互不知道对方存在；如果锁是
/// `TaskRegistry::new` 里现建的 `Mutex::new(())`，那就是各锁各的、等于没锁。
/// 用一张按路径查找的全局表，才能保证「同一份磁盘文件」的所有读改写调用者
/// （不管来自哪个 `TaskRegistry` 实例）串行在同一把锁上。
///
/// 只用 `Mutex<()>` 做互斥信号，不在锁表本身缓存任务数据——`TaskRegistry`
/// 仍然每次现读现写磁盘，改动范围只是"读改写整个过程持锁"，不改变落盘格式
/// 或原子写模式（`.json.tmp` + `rename` 不变）。
fn registry_lock_for(path: &std::path::Path) -> Arc<Mutex<()>> {
    static LOCKS: OnceLock<Mutex<HashMap<PathBuf, Arc<Mutex<()>>>>> = OnceLock::new();
    let table = LOCKS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut table = table.lock().expect("registry lock table mutex poisoned");
    table
        .entry(path.to_path_buf())
        .or_insert_with(|| Arc::new(Mutex::new(())))
        .clone()
}

/// per-app 定时任务注册表，落盘到 `DataLayout::scheduler_tasks_path()`
/// 指向的单个 host-global JSON 文件。
///
/// 并发安全：`register`/`deregister_app`/`mark_run`（写）与 `all`（读）全部
/// 在 `lock`（见 `registry_lock_for`）持有期间完成整个 `load -> modify(可选)
/// -> save`，串行化针对同一落盘路径的所有调用者——修复"两个几乎同时完成的
/// `mark_run` 各自 load 到同一份旧文件、后写覆盖先写"这个丢更新 bug（P3 §5.2
/// 并发调度场景下，`Scheduler::tick` 允许最多 `MAX_CONCURRENT_TASKS` 个
/// task-mode 会话并发跑，各自跑完后独立 `mark_run` 自己的 task）。这些方法
/// 全是同步文件 I/O、不 `async`，锁不会跨 `.await` 持有。
pub struct TaskRegistry {
    path: PathBuf,
    lock: Arc<Mutex<()>>,
}

impl TaskRegistry {
    pub fn new(layout: &DataLayout) -> Self {
        let path = layout.scheduler_tasks_path();
        let lock = registry_lock_for(&path);
        Self { path, lock }
    }

    fn load(&self) -> Vec<RegisteredTask> {
        std::fs::read_to_string(&self.path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    /// 原子写：先写 `.json.tmp` 再 `rename`，与 `registry.rs::RegistryStore::save`
    /// 同一套模式，避免进程中途崩溃留下半写文件。
    fn save(&self, tasks: &[RegisteredTask]) -> Result<(), String> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let tmp = self.path.with_extension("json.tmp");
        let body = serde_json::to_string_pretty(tasks).map_err(|e| e.to_string())?;
        std::fs::write(&tmp, body).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, &self.path).map_err(|e| e.to_string())
    }

    /// 注册某 app 声明的定时任务。语义：先移除该 app 之前持久化的全部记录，
    /// 再按传入的 `tasks` 逐条重建——cron/prompt/catch_up 一律取清单最新声明
    /// （应用更新后清单可能改了 cron，理应生效）。
    ///
    /// 唯一保留的旧状态是 `last_run`：若某个 task `id` 在重新注册前后都存在，
    /// 沿用旧记录里的 `last_run`（不清零）；否则（新增 task id）`last_run` 为
    /// `None`。理由：`last_run` 是"这个任务上次真的跑过没有"的运行事实，跟清单
    /// 声明（cron/prompt 等）是否变了无关——应用改了 prompt 文案不该导致同一个
    /// 定时任务被判定为"从没跑过"而立即重跑一次（尤其配合 `catch_up` 语义时
    /// 后果更明显）。不影响其他 app 的记录。
    pub fn register(&self, app_id: &str, tasks: &[ScheduledTask]) -> Result<(), String> {
        let _guard = self.lock.lock().expect("TaskRegistry lock poisoned");
        let mut all = self.load();
        let old_last_run: HashMap<String, Option<SystemTime>> = all
            .iter()
            .filter(|t| t.app_id == app_id)
            .map(|t| (t.id.clone(), t.last_run))
            .collect();
        all.retain(|t| t.app_id != app_id);
        for task in tasks {
            let last_run = old_last_run.get(&task.id).copied().flatten();
            all.push(RegisteredTask {
                app_id: app_id.to_string(),
                id: task.id.clone(),
                cron: task.cron.clone(),
                prompt: task.prompt.clone(),
                catch_up: task.catch_up,
                last_run,
            });
        }
        self.save(&all)
    }

    /// 移除某 app 的全部已注册任务（幂等：app 不存在时无事发生）。不影响其他
    /// app 的记录。
    pub fn deregister_app(&self, app_id: &str) -> Result<(), String> {
        let _guard = self.lock.lock().expect("TaskRegistry lock poisoned");
        let mut all = self.load();
        all.retain(|t| t.app_id != app_id);
        self.save(&all)
    }

    /// 把 `(app_id, task_id)` 对应任务的 `last_run` 设为 `at` 并持久化。
    /// 找不到该任务时 no-op（不报错——调用方多半是在 app 被卸载竞态后才收到
    /// 迟到的运行结果，静默忽略即可）。
    pub fn mark_run(&self, app_id: &str, task_id: &str, at: SystemTime) -> Result<(), String> {
        let _guard = self.lock.lock().expect("TaskRegistry lock poisoned");
        let mut all = self.load();
        if let Some(t) = all
            .iter_mut()
            .find(|t| t.app_id == app_id && t.id == task_id)
        {
            t.last_run = Some(at);
        }
        self.save(&all)
    }

    /// 加载全部已持久化的任务（跨 app）。同一把 `lock` 覆盖读路径——保证看到
    /// 的是某次完整 `save` 之后的一致文件，不会读到写到一半的中间状态。
    pub fn all(&self) -> Vec<RegisteredTask> {
        let _guard = self.lock.lock().expect("TaskRegistry lock poisoned");
        self.load()
    }
}

/// Task14b：应用打开时的接线点——把清单声明的 `scheduledTasks` 登记进
/// `TaskRegistry`，权限门为 `schedule_permitted`（调用方传 `perms.system.schedule`）。
///
/// 这是 Task10 的 `TaskRegistry::register()` 与 Task13 的
/// `run_catch_up_for_app()` 之间此前唯一缺失的一环：两者都已存在，但从没有任何
/// 生产路径真的调用过 `register()`——`open_app` 只调了 `run_catch_up_for_app`，
/// 导致 registry 永远是空的，整个调度器（`Scheduler::tick`/`catch_up`）形同虚设。
/// 调用方（`session_mgr::open_app_after_acquire`）必须在触发
/// `run_catch_up_for_app` 之前调用本函数，否则那次补跑会因为 registry 里还没有
/// 这个 app 的任务而查不到任何到期任务。
///
/// 参数刻意不是 `&Permissions` 整体，而是拆成 `schedule_permitted: bool` +
/// `tasks: &[ScheduledTask]`：调用方在拿到 `perms` 之后、调用本函数之前，还会把
/// `perms.connectors` 按值移出去喂给 `McpSocketListener::start`（Task9），移出
/// 一个字段之后就不能再整体借用 `&perms`（借用检查器会拒绝），但各个字段
/// （`perms.system.schedule`、`perms.scheduled_tasks`）仍可单独访问——拆参数正是
/// 为了避开这个借用限制，不是随意的风格选择。
///
/// 权限门（安全，P3 §10）：`schedule_permitted == false` 时是纯 no-op——不注册，
/// 也不主动摘除该 app 可能已有的旧记录（保持这次修复的改动最小；若清单声明了
/// `scheduledTasks` 但没有被用户确认授予 `system.schedule` 权限，这个 app 就不能
/// 拥有后台定时执行能力，哪怕它以前登记过）。
///
/// `schedule_permitted == true` 时无条件调用 `register()`（哪怕 `tasks` 为空）：
/// `register` 语义是"先移除该 app 的旧记录、再按传入列表整体重建"（Task10），
/// 传空列表等价于摘除该 app 全部任务——顺带处理了"该 app 更新后清单不再声明
/// 某个任务"的剪除，不需要额外分支。`last_run` 仍由 `register()` 自身按 task id
/// 保留（Task10），重复调用（每次 `open_app` 都会触发一次）是幂等的：重复打开
/// 同一个 app 不会产生重复任务，也不会清零已有的 `last_run`。
pub fn register_scheduled_tasks_if_permitted(
    layout: &DataLayout,
    app_id: &str,
    schedule_permitted: bool,
    tasks: &[ScheduledTask],
) -> Result<(), String> {
    if !schedule_permitted {
        return Ok(());
    }
    TaskRegistry::new(layout).register(app_id, tasks)
}

// ---------------------------------------------------------------------------
// cron 到期判定：next_fire + due_tasks + 抖动错峰
// ---------------------------------------------------------------------------
//
// 清单里的 `ScheduledTask.cron`（见 `permissions.rs`、`permissions.rs` 里
// `render_human` 附近的 fixture、以及本文件测试里复用的 `"0 9 * * *"` / `"* * * * *"`）
// 一律按**标准 5 段**写：`分 时 日 月 周`（POSIX/Vixie cron 惯例，没有秒）。
//
// 但 `cron` crate（0.12）的 `Schedule::from_str` 要求 **6 或 7 段**：
// `秒 分 时 日 月 周 [年]`——直接把 5 段清单字符串丢给它会解析失败。
// 所以这里落地时统一在清单声明前面补一个 `"0 "`（秒=0），把 5 段规范化成
// 6 段再喂给 `cron` crate；已经是 6/7 段的字符串（未来若清单改声明习惯）
// 原样透传，不重复加。判断依据是空白分隔的字段数，不是猜字符串长度。
fn normalize_cron_expr(expr: &str) -> String {
    match expr.split_whitespace().count() {
        5 => format!("0 {expr}"),
        _ => expr.to_string(),
    }
}

/// 计算 `cron`（清单里的 5 段写法，见上方 `normalize_cron_expr` 注释）在
/// `after` **之后**（严格 after，不含 after 本身）最近一次触发的绝对时刻。
///
/// 解析/计算按**本地时区**（`chrono::Local`）进行——`0 9 * * *` 指的是"本地
/// 时间 9 点"，不是 UTC 9 点（P3 设计 §10 明确要求"时区按本地时区显式处理"）。
///
/// cron 字符串不合法时返回 `Err`（不 panic）——调用方 `due_tasks` 据此把这类
/// 任务当作"这轮判不了到期"跳过，而不是让整个调度判定崩掉；这里选择"跳过"
/// 而非"整体 Err 向上传播"，因为一个 app 写错 cron 不该拖垮其他 app 的定时
/// 任务判定。
pub fn next_fire(cron_expr: &str, after: SystemTime) -> Result<SystemTime, String> {
    let normalized = normalize_cron_expr(cron_expr);
    let schedule = CronSchedule::from_str(&normalized)
        .map_err(|e| format!("unparseable cron `{cron_expr}` (normalized `{normalized}`): {e}"))?;

    let after_local: DateTime<Local> = after.into();
    let next_local = schedule.after(&after_local).next().ok_or_else(|| {
        format!("cron `{cron_expr}` has no upcoming fire time after {after_local}")
    })?;
    Ok(SystemTime::from(next_local))
}

/// FNV-1a（64 位）：只用来把 task id 摊平成一个确定性数字，不追求密码学强度。
/// 不用 `std::collections::hash_map::DefaultHasher`——它的具体算法"不保证跨
/// Rust 版本稳定"（标准库文档原话），这里要的是"同一个 id 永远算出同一个抖
/// 动量"这种可长期依赖的确定性，所以手写一个固定算法，不依赖任何未声明保证
/// 的实现细节，也不必新增 `rand` 之类的依赖。
fn fnv1a_hash(s: &str) -> u64 {
    const OFFSET_BASIS: u64 = 0xcbf29ce484222325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = OFFSET_BASIS;
    for byte in s.as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(PRIME);
    }
    hash
}

/// 由 `task_id` 派生一个 `[0, jitter]` 区间内的确定性抖动量，用于错峰：多个任务
/// 若 cron 表达式算出同一个触发时刻，各自加上不同（但每次都一样）的抖动后就不
/// 会真的同时触发。
///
/// 硬约束（P3 设计 §10）：抖动只能把触发时间**往后**推，绝不能让任务提前
/// 触发——所以返回值永远 `>= Duration::ZERO` 且 `<= jitter`，调用方直接把它
/// 加到 `next_fire` 算出的基准时刻上即可，不存在减法路径。
fn jitter_for(task_id: &str, jitter: Duration) -> Duration {
    let max_nanos = jitter.as_nanos();
    if max_nanos == 0 {
        return Duration::ZERO;
    }
    let nanos = (fnv1a_hash(task_id) as u128) % max_nanos;
    Duration::from_nanos(nanos as u64)
}

/// 返回当前"到期该跑"的任务列表：对每条已注册任务，以 `last_run`（`None`
/// 则用 `SystemTime::UNIX_EPOCH` 兜底——task 从没跑过时，视为从 1970 年就
/// "该跑而未跑"，所以新注册的任务立刻可到期，不必等到下一个自然 cron 周期
/// 边界）为基准算出 `next_fire`，叠加按 task id 派生的确定性抖动后，若
/// `<= clock.now()` 即判定为到期。
///
/// cron 解析失败的任务直接跳过（不 panic、不让调用方 `due_tasks` 整体出错）
/// ——见 `next_fire` 文档。
pub fn due_tasks(reg: &TaskRegistry, clock: &dyn Clock, jitter: Duration) -> Vec<RegisteredTask> {
    let now = clock.now();
    reg.all()
        .into_iter()
        .filter(|t| {
            let baseline = t.last_run.unwrap_or(SystemTime::UNIX_EPOCH);
            let Ok(base_fire) = next_fire(&t.cron, baseline) else {
                return false;
            };
            base_fire + jitter_for(&t.id, jitter) <= now
        })
        .collect()
}

/// P3 §10 running-only 边界：把 `due_tasks` 的结果按"该任务归属的 app 当前是否
/// 打开"收窄，只保留 `is_app_open` 返回 `true` 的那些——见 `Scheduler::tick`
/// 文档"running-only 边界"一节。
///
/// 抽成独立的纯函数（而非直接内联进 `tick`）：可以脱离 `TaskRegistry`/
/// `Semaphore`/`session_mgr::spawn_task_session` 单独单测——给一组
/// `RegisteredTask` + 一个假的 `is_app_open` 谓词，直接断言过滤结果，不需要
/// 起 tokio 运行时或 mock_pi 子进程（那部分端到端属性留给
/// `tests/scheduler_running_only_it.rs` 覆盖）。
fn filter_due_for_open_apps(
    due: Vec<RegisteredTask>,
    is_app_open: &(dyn Fn(&str) -> bool + Send + Sync),
) -> Vec<RegisteredTask> {
    due.into_iter().filter(|t| is_app_open(&t.app_id)).collect()
}

// ---------------------------------------------------------------------------
// 补跑（catch-up，Task13）：应用重新打开时，把它离线期间错过的定时任务补跑
// 一次——错过的自然周期不管有几个，只补最近一次，不按错过次数重复补。
// ---------------------------------------------------------------------------

/// 补跑选择逻辑（P3 §10）：从注册表里选出"至少错过一次触发窗口、且声明允许
/// 补跑"的任务——每条任务无论期间实际错过了多少个自然 cron 周期，在返回的
/// `Vec` 里最多只出现一次（不是"错过几次就补几次"）。
///
/// 判定与 `due_tasks` 同款（以 `last_run` 为基准算 `next_fire`，`None` 用
/// `UNIX_EPOCH` 兜底——同"从没跑过的任务立刻可到期"的语义），但有两处刻意
/// 不同：
/// 1. 先看 `catch_up` 声明——`false` 的任务无论多过期都直接排除，永不补跑；
///    这个检查排在算 `next_fire` 之前，短路掉不该补跑的任务。
/// 2. 不叠加 `due_tasks` 的确定性抖动错峰——补跑是"启动时发现漏跑了，立刻
///    补"，不是常规调度里为避免多任务同一时刻扎堆触发而做的错峰；调用方
///    （`run_catch_up_for_app`）本来就只处理单个刚打开的 app，用不上错峰。
///
/// "只补一次"这个属性由函数结构本身保证，不需要额外去重逻辑：对每个任务只
/// 算"基于 last_run 的下一个触发点"这一个值（`next_fire` 的定义就是"after
/// 之后最近一次"，不是"after 之后全部"），不会把错过期间跳过的每个自然周期
/// 都枚举出来；`reg.all()` 里每个 `(app_id, id)` 本来就只有一条记录，
/// `filter` 只会保留或丢弃整条，结果 `Vec` 里不可能出现同一个任务两次。
///
/// cron 解析失败的任务直接排除（不 panic），处理方式同 `due_tasks`。
pub fn catch_up(reg: &TaskRegistry, clock: &dyn Clock) -> Vec<RegisteredTask> {
    let now = clock.now();
    reg.all()
        .into_iter()
        .filter(|t| {
            if !t.catch_up {
                return false;
            }
            let baseline = t.last_run.unwrap_or(SystemTime::UNIX_EPOCH);
            let Ok(fire) = next_fire(&t.cron, baseline) else {
                return false;
            };
            fire <= now
        })
        .collect()
}

/// 把 `catch_up` 为某个刚打开的 `app` 选出的补跑任务，经与 `Scheduler::tick`
/// 完全同一条 task-mode 派发路径（`session_mgr::spawn_task_session` +
/// `TaskRegistry::mark_run`）跑掉，遵守同一个全局并发上限
/// `MAX_CONCURRENT_TASKS`——即便单个刚打开的 app 短时间内不太可能撞上限，
/// 也不为补跑另起一套并发策略。
///
/// 只处理**这一个** `app` 的任务（按 `app_id` 过滤 `catch_up` 的全量结果）：
/// 补跑发生在"某个 app 被打开"这个时间点，其余 app 此刻并未运行，它们各自
/// 的补跑留给各自下次被打开时触发——不在这里越权替未打开的 app 抢跑。
///
/// 返回值形状与 `Scheduler::tick` 完全一致（`Vec<TaskSessionResult>`，同一个
/// 与 Task15 通知中心之间的最小 seam；见该类型文档）——拉起失败时同样合成一条
/// `errored: true` 的结果，而不是让错误消失或让调用方整体失败。
///
/// 供 `session_mgr::open_app_after_acquire` 在应用打开后台调用一次；调用方
/// 应把这个 `async fn` 包进独立的 `tokio::spawn`（不要内联 `.await`），因为
/// 补跑可能触发若干次真实 LLM 调用、耗时不可控，不该拖慢"应用已打开，界面
/// 可以显示了"这个用户可感知的响应。
///
/// `mcp`（Task17b）：转发给 `session_mgr::spawn_task_session`，供其经
/// `session_mgr::headless_contribution` 算出该 app 的 `CapabilityRegistry::launch`
/// 贡献（同前台 `open_app` 的 `state.capabilities.launch`）。调用方
/// （`session_mgr::open_app_after_acquire`）传入的是与前台会话同一个
/// `AppState::mcp`（clone 只是浅拷贝 `Arc`），保证补跑的 task-mode 会话与刚
/// 打开的前台会话看到的是同一份已连接 server 状态。
pub async fn run_catch_up_for_app(
    layout: &DataLayout,
    hosttools_dir: &Path,
    mcp: &McpManager,
    app: &InstalledApp,
    clock: &dyn Clock,
) -> Vec<session_mgr::TaskSessionResult> {
    let treg = TaskRegistry::new(layout);
    let due: Vec<RegisteredTask> = catch_up(&treg, clock)
        .into_iter()
        .filter(|t| t.app_id == app.app_id)
        .collect();
    if due.is_empty() {
        return Vec::new();
    }

    // 同一轮补跑统一用同一个 now 做全部 mark_run 的时刻，与 `Scheduler::tick`
    // 同款处理（见该函数文档：语义上是"这一轮判定并派发"的时刻，不是"每条
    // 任务各自跑完"的时刻）。
    let now = clock.now();
    let sem = Arc::new(Semaphore::new(MAX_CONCURRENT_TASKS));
    let mut handles = Vec::new();
    for task in due {
        let sem = sem.clone();
        let layout = layout.clone();
        let hosttools_dir = hosttools_dir.to_path_buf();
        let mcp = mcp.clone();
        let app = app.clone();
        handles.push(tokio::spawn(async move {
            let _permit = sem
                .acquire_owned()
                .await
                .expect("semaphore 不会被 close：run_catch_up_for_app 未提供关闭接口");

            let outcome = session_mgr::spawn_task_session(
                &layout,
                &hosttools_dir,
                &mcp,
                &app,
                &task.id,
                &task.prompt,
            )
            .await;

            let treg = TaskRegistry::new(&layout);
            let _ = treg.mark_run(&app.app_id, &task.id, now);

            match outcome {
                Ok(r) => r,
                Err(e) => session_mgr::TaskSessionResult {
                    app_id: app.app_id.clone(),
                    task_id: task.id.clone(),
                    text: e,
                    errored: true,
                },
            }
        }));
    }

    let mut results = Vec::new();
    for h in handles {
        // 同 `Scheduler::tick`：某个 task-mode 会话对应的 tokio 任务本身 panic
        // 时跳过这一条，不让整个补跑批次崩掉。
        if let Ok(r) = h.await {
            results.push(r);
        }
    }
    results
}

// ---------------------------------------------------------------------------
// Scheduler：调度器执行核（Task12）——tick 一次 = 取到期 -> 并发闸门 -> 逐个
// task-mode 拉起 -> mark_run -> 收集结果
// ---------------------------------------------------------------------------

/// 同一时刻允许同时在跑的 task-mode 会话数上限（P3 §5.2"并发保护"）。命名常量，
/// 不是魔法数字——`tick` 内部据此建 `Semaphore(MAX_CONCURRENT_TASKS)`，多出的
/// 到期任务排队等前面的会话跑完释放名额，不是拒绝/丢弃。
pub const MAX_CONCURRENT_TASKS: usize = 2;

/// 生产环境默认抖动上限（喂给 `Scheduler::new` 的 `jitter` 参数）：多个 app 的
/// cron 表达式若算出同一触发时刻，各自按 task id 派生的确定性抖动（见
/// `jitter_for` 文档）在这个区间内错峰，不会真的同时触发。取值小于
/// `SCHEDULER_TICK_INTERVAL`（`lib.rs`，周期 tick 的触发间隔）——抖动只应把
/// 到期点推迟到"下一次 tick 也能发现"的范围内，不应大到需要等好几轮 tick
/// 才会被发现。命名常量，供生产 `start_scheduler_loop`（`lib.rs`）构造真正的
/// `Scheduler` 时使用；测试一律传 `Duration::ZERO`（不需要错峰、且抖动会让到期
/// 判定的断言依赖具体计算出的偏移量，见 `scheduler.rs` 已有测试的做法）。
pub const DEFAULT_JITTER: Duration = Duration::from_secs(20);

/// 调度器执行核：把"到期任务"接到"task-mode pi 拉起"。持有 `TaskRegistry`
/// 落盘所需的 `DataLayout`（不直接持有 `TaskRegistry` 本身——它是无内存状态的
/// 薄封装，每次现用现造更简单，见 `tick` 内部）、已装应用索引 `RegistryStore`
/// （查到期任务归属的 app 是否 trusted 等 spawn 所需字段）、`hosttools_dir`
/// （喂给 `session_mgr::build_launch`/`spawn_task_session`）、`mcp`（Task17b：
/// 转发给 `session_mgr::spawn_task_session` 算 MCP 授权注入，与前台 `open_app`
/// 用的是同一个 `AppState::mcp` 实例——生产调用方 `lib.rs::start_scheduler_loop`
/// 传的就是 `state.mcp.clone()`，不新起一份互不知情的连接池）、`jitter`（转发给
/// `due_tasks`，见该函数文档）。
pub struct Scheduler {
    layout: DataLayout,
    apps: RegistryStore,
    hosttools_dir: PathBuf,
    mcp: McpManager,
    jitter: Duration,
    /// 当前正在跑的 task-mode 会话数（观测用 gauge）。真正的并发上限由 `tick`
    /// 内部的 `Semaphore(MAX_CONCURRENT_TASKS)` 保证，这个计数器只是"闸门内到底
    /// 有几个在跑"的可观测证据——供测试断言并发确实生效（而不是意外串行）。
    in_flight: Arc<AtomicUsize>,
    /// 自本 `Scheduler` 创建以来观测到的 `in_flight` 最大值（观测用 gauge，只增
    /// 不减）。供并发上限测试断言"确实达到过 cap"，而不是"侥幸从未撞线、上限
    /// 形同虚设"。
    peak_in_flight: Arc<AtomicUsize>,
}

impl Scheduler {
    pub fn new(
        layout: DataLayout,
        apps: RegistryStore,
        hosttools_dir: PathBuf,
        mcp: McpManager,
        jitter: Duration,
    ) -> Self {
        Self {
            layout,
            apps,
            hosttools_dir,
            mcp,
            jitter,
            in_flight: Arc::new(AtomicUsize::new(0)),
            peak_in_flight: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// 当前正在跑的 task-mode 会话数，见字段文档。
    pub fn in_flight(&self) -> usize {
        self.in_flight.load(Ordering::SeqCst)
    }

    /// 自创建以来观测到的最大同时在跑 task-mode 会话数，见字段文档。
    pub fn peak_in_flight(&self) -> usize {
        self.peak_in_flight.load(Ordering::SeqCst)
    }

    /// 跑一轮调度：
    /// 1. `due_tasks` 取本轮到期任务；
    /// 2. **running-only 边界（P3 §10，Task15b）**：`filter_due_for_open_apps`
    ///    只保留 `is_app_open` 判定为"当前打开"的 app 的到期任务——一个 app 关闭
    ///    期间即便有任务到期，这里也绝不触发；那条任务保持未 `mark_run`（`due_tasks`
    ///    判定依据的 `last_run` 不变），留给它下次被打开时的 `run_catch_up_for_app`
    ///    补跑一次（Task13）。这是 v1 设计明确的边界："只在 app 运行时触发它的
    ///    任务，错过就交给 catch-up"，不是"任何时候到期都跑"。过滤后为空则直接
    ///    返回，不建 `Semaphore`/不 spawn；
    /// 3. 全局并发闸门 `Semaphore(MAX_CONCURRENT_TASKS)`：同刻到期再多，也只按
    ///    这个上限并发跑 task-mode 会话，多出的排队等释放（P3 §5.2）；
    /// 4. 对每条到期任务：先查 `RegistryStore` 拿其 `InstalledApp`——查不到（该
    ///    app 已卸载竞态）静默跳过，不 panic，同 `mark_run` 对未知任务"迟到静默
    ///    忽略"的哲学；查到则 `session_mgr::spawn_task_session` 拉起同权同沙盒
    ///    的 headless 会话跑 `prompt`，随后无论成败都 `mark_run` 落 `last_run`
    ///    （一次拉起失败不该让该任务卡在永远到期的状态、被下一轮 tick 无意义地
    ///    反复重试——故障 app 也不该拖垮调度器本身）；
    /// 5. 收集全部结果（含拉起失败时合成的 `errored: true` 结果）返回——这是
    ///    本任务与 Task15（通知中心）之间的最小 seam：调用方拿到
    ///    `Vec<TaskSessionResult>` 后逐条转存进 `NotificationStore`；生产环境的
    ///    调用方是 `run_scheduler_tick_cycle`（本文件下方，供 `lib.rs` 的后台
    ///    周期循环调用），本方法自身仍不碰 `NotificationStore`。
    ///
    /// `is_app_open`：由调用方传入的"该 app_id 当前是否有活跃会话"谓词——生产
    /// 调用方（`run_scheduler_tick_cycle`）用 `AppState::app_sessions` 的 key 集合
    /// 构造；测试可以传任意假谓词，不需要真的打开一个 app。要求
    /// `Send + Sync`：`tick` 内部把到期任务的拉起包进 `tokio::spawn`（见下方并发
    /// 闸门部分），若这个谓词要跨那个 spawn 边界被使用就需要这个约束——虽然当前
    /// 实现只在 spawn 之前、过滤阶段用它（不会真的跨 spawn 传递），但显式标注
    /// 这个约束更安全、也与本文件 `Clock: Send + Sync` 的既有先例一致，不依赖
    /// "恰好没跨边界用到"这种脆弱的偶然性。
    pub async fn tick(
        &self,
        clock: &dyn Clock,
        is_app_open: &(dyn Fn(&str) -> bool + Send + Sync),
    ) -> Vec<session_mgr::TaskSessionResult> {
        let treg = TaskRegistry::new(&self.layout);
        let due = filter_due_for_open_apps(due_tasks(&treg, clock, self.jitter), is_app_open);
        if due.is_empty() {
            return Vec::new();
        }

        // 本轮统一用同一个 `now`（tick 开始时取一次）作为全部 `mark_run` 的时刻——
        // 语义上是"这一轮判定到期并派发"的时刻，不是"每条任务各自跑完"的时刻；
        // 与 `due_tasks` 用同一个 `clock.now()` 保持一致（否则同一轮内不同任务
        // 各自 mark_run 不同的 now，会让"同一轮"这个概念失去意义）。
        let now = clock.now();
        let installed = self.apps.load();
        let sem = Arc::new(Semaphore::new(MAX_CONCURRENT_TASKS));

        let mut handles = Vec::new();
        for task in due {
            let Some(app) = installed.iter().find(|a| a.app_id == task.app_id).cloned() else {
                continue; // app 已卸载竞态：静默跳过，不 panic（同 mark_run 的哲学）
            };
            let sem = sem.clone();
            let layout = self.layout.clone();
            let hosttools_dir = self.hosttools_dir.clone();
            let mcp = self.mcp.clone();
            let in_flight = self.in_flight.clone();
            let peak_in_flight = self.peak_in_flight.clone();
            let task_id = task.id.clone();
            let app_id = task.app_id.clone();
            let prompt = task.prompt.clone();

            handles.push(tokio::spawn(async move {
                // 持有 permit 直到本任务的 task-mode 会话跑完（含 mark_run）——
                // `_permit` 在这个 async block 结尾才 drop，这正是"同时在跑不超过
                // MAX_CONCURRENT_TASKS"这个不变量的来源。
                let _permit = sem
                    .acquire_owned()
                    .await
                    .expect("semaphore 不会被 close：Scheduler 未提供关闭接口");

                let cur = in_flight.fetch_add(1, Ordering::SeqCst) + 1;
                peak_in_flight.fetch_max(cur, Ordering::SeqCst);

                let outcome = session_mgr::spawn_task_session(
                    &layout,
                    &hosttools_dir,
                    &mcp,
                    &app,
                    &task_id,
                    &prompt,
                )
                .await;

                in_flight.fetch_sub(1, Ordering::SeqCst);

                let treg = TaskRegistry::new(&layout);
                let _ = treg.mark_run(&app_id, &task_id, now);

                match outcome {
                    Ok(r) => r,
                    Err(e) => session_mgr::TaskSessionResult {
                        app_id,
                        task_id,
                        text: e,
                        errored: true,
                    },
                }
            }));
        }

        let mut results = Vec::new();
        for h in handles {
            // `h.await` 出 `Err`：该 task-mode 会话对应的 tokio 任务本身 panic 了——
            // 不让整个 tick 崩掉，跳过这一条（比让 panic 传播、崩掉整轮调度更安全；
            // 生产场景下这理论上只应发生在 `session_mgr::spawn_task_session` 内部
            // 逻辑 bug 时，故意不静默吞掉——`cargo test` 里 panic 仍会打印栈)。
            if let Ok(r) = h.await {
                results.push(r);
            }
        }
        results
    }
}

// ---------------------------------------------------------------------------
// Task15b：tick 周期 -> 通知中心 的最小胶水（生产后台循环调用，见
// `lib.rs::start_scheduler_loop`）——这是 Task12 `Scheduler::tick` 与 Task15
// `NotificationStore::record_task_result` 之间此前唯一缺失的一环：两者都已
// 存在，但从没有任何生产路径真的周期性调用过 `tick`（只有应用打开时的
// `run_catch_up_for_app` 会跑），导致运行期间到期的任务永远不会被发现，也永远
// 不会产生任何通知。
// ---------------------------------------------------------------------------

/// 一次"tick 周期"：跑一轮 `Scheduler::tick`（只对 `is_app_open` 判定为当前打开
/// 的 app 触发到期任务，见该方法文档"running-only 边界"一节），再把返回的
/// `Vec<TaskSessionResult>` 逐条转存进 `NotificationStore`
/// （`record_task_results`，best-effort——单条落盘失败不影响其余条，同
/// `audit::record` 调用方不应因一条审计写失败而中断业务逻辑的哲学），最后顺带
/// 清理到期未验收的暂存写调用（P6-C Task4 `NotificationStore::expire_staged`，
/// 复用同一个 tick 周期，不为 TTL 到期另开一条独立循环，见 spec §3"到期由
/// 调度器 tick 顺带清理"）——`clock.now()` 返回 `SystemTime`，`expire_staged`
/// 要的是 unix 秒，`approvals::unix_secs` 就是这一步换算，与 `TestClock` 注入
/// 的确定性时间保持一致（不直接读墙钟）。
///
/// 抽成独立函数（而非内联进 `lib.rs` 里那个 `loop { sleep(SCHEDULER_TICK_INTERVAL)
/// .await; ... }`）：测试可以直接调用它跑"一次周期"，注入 `TestClock` + 假的
/// `is_app_open` 谓词，断言 running-only 过滤与"结果→通知"这两段组合行为，
/// 不需要真的睡 30s 等墙钟——真实的周期循环本身没有独立值得测的逻辑（就是一个
/// `sleep` + 调用本函数），不应该、也不需要在测试里真的等待那个 sleep。
pub async fn run_scheduler_tick_cycle(
    scheduler: &Scheduler,
    clock: &dyn Clock,
    notifications: &NotificationStore,
    is_app_open: &(dyn Fn(&str) -> bool + Send + Sync),
) {
    let results = scheduler.tick(clock, is_app_open).await;
    let _ = notifications.record_task_results(&results);
    let _ = notifications.expire_staged(crate::approvals::unix_secs(clock.now()));
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use tempfile::tempdir;

    fn task(id: &str, cron: &str) -> ScheduledTask {
        ScheduledTask {
            id: id.to_string(),
            cron: cron.to_string(),
            prompt: format!("prompt-{id}"),
            catch_up: true,
        }
    }

    // ---- TaskRegistry: register + persistence ----

    #[test]
    fn register_two_tasks_all_returns_both_and_persists_across_fresh_registry() {
        let tmp = tempdir().unwrap();
        let layout = DataLayout::new(tmp.path().to_path_buf());

        let reg = TaskRegistry::new(&layout);
        reg.register(
            "app1",
            &[task("daily", "0 9 * * *"), task("weekly", "0 9 * * 1")],
        )
        .unwrap();

        let all = reg.all();
        assert_eq!(all.len(), 2);
        assert!(all
            .iter()
            .any(|t| t.app_id == "app1" && t.id == "daily" && t.cron == "0 9 * * *"));
        assert!(all
            .iter()
            .any(|t| t.app_id == "app1" && t.id == "weekly" && t.cron == "0 9 * * 1"));
        assert!(all.iter().all(|t| t.last_run.is_none()));

        // 新建一个指向同一目录的 TaskRegistry：必须读回同样的两条记录（持久化）。
        let reg2 = TaskRegistry::new(&layout);
        let all2 = reg2.all();
        assert_eq!(all2.len(), 2);
        assert_eq!(all2, all);
    }

    // ---- Task14b: register_scheduled_tasks_if_permitted（open_app 接线点）----

    #[test]
    fn open_app_with_schedule_permission_registers_declared_tasks_and_persists() {
        let tmp = tempdir().unwrap();
        let layout = DataLayout::new(tmp.path().to_path_buf());
        let tasks = [task("daily", "0 9 * * *"), task("weekly", "0 9 * * 1")];

        register_scheduled_tasks_if_permitted(&layout, "app1", true, &tasks).unwrap();

        // 直接对着同一份落盘文件新建一个 TaskRegistry：验证的是"真的写盘了"，
        // 不是同一个内存实例的缓存假象。
        let all = TaskRegistry::new(&layout).all();
        assert_eq!(all.len(), 2);
        assert!(all
            .iter()
            .any(|t| t.app_id == "app1" && t.id == "daily" && t.cron == "0 9 * * *"));
        assert!(all
            .iter()
            .any(|t| t.app_id == "app1" && t.id == "weekly" && t.cron == "0 9 * * 1"));
    }

    #[test]
    fn open_app_without_schedule_permission_registers_nothing() {
        let tmp = tempdir().unwrap();
        let layout = DataLayout::new(tmp.path().to_path_buf());
        let tasks = [task("daily", "0 9 * * *"), task("weekly", "0 9 * * 1")];

        // 清单声明了 scheduledTasks，但 system.schedule 不为 true（缺省/显式 false）
        // ——权限门必须挡住，registry 里不应出现这个 app 的任何任务。
        register_scheduled_tasks_if_permitted(&layout, "app1", false, &tasks).unwrap();

        let all = TaskRegistry::new(&layout).all();
        assert!(all.iter().all(|t| t.app_id != "app1"));
        assert!(all.is_empty());
    }

    #[test]
    fn reopening_scheduled_app_is_idempotent_and_preserves_last_run() {
        let tmp = tempdir().unwrap();
        let layout = DataLayout::new(tmp.path().to_path_buf());
        let tasks = [task("daily", "0 9 * * *"), task("weekly", "0 9 * * 1")];

        // 第一次打开：登记两条任务。
        register_scheduled_tasks_if_permitted(&layout, "app1", true, &tasks).unwrap();

        // 其中一条已经真的跑过一次（模拟调度器 tick 或 catch_up 跑完后 mark_run）。
        let ran_at = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        TaskRegistry::new(&layout)
            .mark_run("app1", "daily", ran_at)
            .unwrap();

        // 再次打开同一个 app（例如用户关闭又重新打开）：必须幂等——不产生重复
        // 任务，且已经跑过的那条 last_run 不被清零。
        register_scheduled_tasks_if_permitted(&layout, "app1", true, &tasks).unwrap();

        let all = TaskRegistry::new(&layout).all();
        assert_eq!(all.len(), 2); // 没有变成 4 条
        let daily = all.iter().find(|t| t.id == "daily").unwrap();
        assert_eq!(daily.last_run, Some(ran_at)); // last_run 沿用
        let weekly = all.iter().find(|t| t.id == "weekly").unwrap();
        assert!(weekly.last_run.is_none()); // 从没跑过的那条仍是 None
    }

    #[test]
    fn register_is_scoped_per_app() {
        let tmp = tempdir().unwrap();
        let layout = DataLayout::new(tmp.path().to_path_buf());
        let reg = TaskRegistry::new(&layout);

        reg.register("app1", &[task("daily", "0 9 * * *")]).unwrap();
        reg.register("app2", &[task("daily", "0 8 * * *")]).unwrap();

        let all = reg.all();
        assert_eq!(all.len(), 2);
        assert!(all
            .iter()
            .any(|t| t.app_id == "app1" && t.cron == "0 9 * * *"));
        assert!(all
            .iter()
            .any(|t| t.app_id == "app2" && t.cron == "0 8 * * *"));
    }

    #[test]
    fn re_register_same_task_id_preserves_last_run_but_updates_cron() {
        let tmp = tempdir().unwrap();
        let layout = DataLayout::new(tmp.path().to_path_buf());
        let reg = TaskRegistry::new(&layout);

        reg.register("app1", &[task("daily", "0 9 * * *")]).unwrap();
        let t = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
        reg.mark_run("app1", "daily", t).unwrap();

        // 重新注册同一个 task id，但 cron 变了（模拟应用更新了清单）。
        reg.register("app1", &[task("daily", "0 10 * * *")])
            .unwrap();

        let all = reg.all();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].cron, "0 10 * * *"); // cron 取最新声明
        assert_eq!(all[0].last_run, Some(t)); // last_run 沿用，不清零
    }

    // ---- TaskRegistry: mark_run ----

    #[test]
    fn mark_run_updates_last_run_and_persists() {
        let tmp = tempdir().unwrap();
        let layout = DataLayout::new(tmp.path().to_path_buf());
        let reg = TaskRegistry::new(&layout);
        reg.register("app1", &[task("daily", "0 9 * * *")]).unwrap();

        let t = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        reg.mark_run("app1", "daily", t).unwrap();

        let all = reg.all();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].last_run, Some(t));

        // 新建一个指向同一目录的 TaskRegistry：last_run 必须读回同一个值（持久化）。
        let reg2 = TaskRegistry::new(&layout);
        assert_eq!(reg2.all()[0].last_run, Some(t));
    }

    #[test]
    fn mark_run_unknown_task_is_noop() {
        let tmp = tempdir().unwrap();
        let layout = DataLayout::new(tmp.path().to_path_buf());
        let reg = TaskRegistry::new(&layout);
        reg.register("app1", &[task("daily", "0 9 * * *")]).unwrap();

        // 未知 task_id：不报错、不新增记录。
        reg.mark_run("app1", "no-such-task", SystemTime::now())
            .unwrap();
        assert_eq!(reg.all().len(), 1);
        assert!(reg.all()[0].last_run.is_none());
    }

    // ---- TaskRegistry: mark_run 并发（复现 + 验证丢更新 bug 已修） ----

    #[test]
    fn concurrent_mark_run_of_different_tasks_persists_both_last_run() {
        // 复现 P3 审阅发现的并发 bug：`Scheduler::tick` 在多线程 tokio 运行时上
        // 最多 `MAX_CONCURRENT_TASKS` 个到期任务并发跑 task-mode 会话，每个任务
        // 跑完后各自 `TaskRegistry::new(&layout)`（现造一个新实例，不是共享同一个
        // 引用）再 `mark_run` 自己的 `(app_id, task_id)`。若 load -> modify -> save
        // 不加锁串行化：两个几乎同时完成的 `mark_run` 各自 load 到同一份旧文件，
        // 各自只改自己那条记录再 save，后写的那次会把先写的那次整份覆盖掉——
        // 先写的任务 `last_run` 就悄悄丢失（读回 `None`），下一轮 tick 又把它判成
        // 到期，重跑一次（幽灵重跑：重复 LLM 调用 + 重复副作用）。
        //
        // 这里用两个真实 OS 线程（不是 tokio task——`mark_run` 本身是同步文件
        // I/O，不需要 tokio 运行时也能复现同一个竞态)，各自反复对同一个共享
        // registry 目录、但不同 task_id 调用 `mark_run`，不加任何人工同步（没有
        // sleep/barrier）——完全对应生产代码里"各自现造 TaskRegistry 实例、互不
        // 知道对方存在"的模式。重复整个「新目录 -> 注册两个任务 -> 并发 mark_run
        // -> 用一个全新 TaskRegistry 重新读盘校验」场景若干轮，只要有一轮任意一个
        // 任务的 last_run 读回 None 就说明丢更新复现了；修复前该断言在若干轮内
        // 几乎必然命中至少一次失败，修复后（load->modify->save 全程持锁）应
        // 稳定全绿。
        const ROUNDS: usize = 20;
        const ITERS_PER_THREAD: usize = 25;

        for round in 0..ROUNDS {
            let tmp = tempdir().unwrap();
            let layout = DataLayout::new(tmp.path().to_path_buf());
            let setup_reg = TaskRegistry::new(&layout);
            setup_reg
                .register(
                    "app1",
                    &[task("task-a", "0 9 * * *"), task("task-b", "0 9 * * *")],
                )
                .unwrap();

            let base = SystemTime::UNIX_EPOCH
                + Duration::from_secs(1_700_000_000 + round as u64 * 100_000);

            // 关键：两个线程各自 `TaskRegistry::new(&layout)`，不是同一个实例的
            // clone——对应 `Scheduler::tick` 里每个并发任务各自现造实例的真实场景。
            let layout_a = layout.clone();
            let base_a = base;
            let handle_a = std::thread::spawn(move || {
                let reg = TaskRegistry::new(&layout_a);
                for i in 0..ITERS_PER_THREAD {
                    reg.mark_run("app1", "task-a", base_a + Duration::from_secs(i as u64))
                        .unwrap();
                }
            });

            let layout_b = layout.clone();
            let base_b = base + Duration::from_secs(10_000);
            let handle_b = std::thread::spawn(move || {
                let reg = TaskRegistry::new(&layout_b);
                for i in 0..ITERS_PER_THREAD {
                    reg.mark_run("app1", "task-b", base_b + Duration::from_secs(i as u64))
                        .unwrap();
                }
            });

            handle_a.join().unwrap();
            handle_b.join().unwrap();

            // 用一个全新的 TaskRegistry 重新从磁盘读——不能复用还带着内存状态的
            // 旧引用，必须证明的是"落盘文件"里两条记录都在，不是内存幻觉。
            let fresh = TaskRegistry::new(&layout);
            let all = fresh.all();
            let a_last_run = all
                .iter()
                .find(|t| t.id == "task-a")
                .and_then(|t| t.last_run);
            let b_last_run = all
                .iter()
                .find(|t| t.id == "task-b")
                .and_then(|t| t.last_run);

            assert!(
                a_last_run.is_some(),
                "round {round}: task-a 的 last_run 并发 mark_run 后应持久化为 Some，实际读回 None \
                 （load->modify->save 丢更新：另一线程的 save 把它覆盖回了 None，对应任务会在下一轮 \
                 tick 被误判为到期而重跑）"
            );
            assert!(
                b_last_run.is_some(),
                "round {round}: task-b 的 last_run 并发 mark_run 后应持久化为 Some，实际读回 None"
            );
        }
    }

    // ---- TaskRegistry: deregister_app ----

    #[test]
    fn deregister_app_removes_only_that_apps_tasks_and_persists() {
        let tmp = tempdir().unwrap();
        let layout = DataLayout::new(tmp.path().to_path_buf());
        let reg = TaskRegistry::new(&layout);

        reg.register(
            "app1",
            &[task("daily", "0 9 * * *"), task("weekly", "0 9 * * 1")],
        )
        .unwrap();
        reg.register("app2", &[task("daily", "0 8 * * *")]).unwrap();

        reg.deregister_app("app1").unwrap();

        let all = reg.all();
        assert_eq!(all.len(), 1);
        assert!(all.iter().all(|t| t.app_id != "app1"));
        assert!(all.iter().any(|t| t.app_id == "app2"));

        // 新建一个指向同一目录的 TaskRegistry：app1 必须仍然不在（持久化）。
        let reg2 = TaskRegistry::new(&layout);
        assert_eq!(reg2.all().len(), 1);
        assert!(reg2.all().iter().all(|t| t.app_id != "app1"));
    }

    #[test]
    fn deregister_unknown_app_is_noop() {
        let tmp = tempdir().unwrap();
        let layout = DataLayout::new(tmp.path().to_path_buf());
        let reg = TaskRegistry::new(&layout);
        reg.register("app1", &[task("daily", "0 9 * * *")]).unwrap();

        reg.deregister_app("no-such-app").unwrap();
        assert_eq!(reg.all().len(), 1);
    }

    // ---- Clock / TestClock ----

    #[test]
    fn test_clock_now_returns_start() {
        let start = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
        let clock = TestClock::new(start);
        assert_eq!(clock.now(), start);
    }

    #[test]
    fn test_clock_advance_moves_now_forward_by_exact_duration() {
        let start = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
        let clock = TestClock::new(start);
        clock.advance(Duration::from_secs(3600));
        assert_eq!(clock.now(), start + Duration::from_secs(3600));
    }

    #[test]
    fn system_clock_now_is_close_to_real_now() {
        // 只验证 SystemClock 真的转发到 SystemTime::now()（在同一秒量级），
        // 不做任何依赖真实时间流逝的断言。
        let before = SystemTime::now();
        let got = SystemClock.now();
        let after = SystemTime::now();
        assert!(got >= before && got <= after);
    }

    // ---- next_fire：5 段清单 cron → 规范化 → cron crate，本地时区 ----

    #[test]
    fn next_fire_daily_9am_returns_expected_local_instant() {
        // 选一个明显不挨着任何真实 DST 切换周末的日期（6 月中旬），断言不受
        // 跑测试的机器时区/该时区当天是否切换夏令时影响。
        let after_local: DateTime<Local> = Local.with_ymd_and_hms(2026, 6, 15, 8, 0, 0).unwrap();
        let expected_local: DateTime<Local> = Local.with_ymd_and_hms(2026, 6, 15, 9, 0, 0).unwrap();

        let got = next_fire("0 9 * * *", SystemTime::from(after_local)).unwrap();

        assert_eq!(got, SystemTime::from(expected_local));
    }

    #[test]
    fn next_fire_is_strictly_after_the_given_instant_not_equal() {
        // after 本身恰好落在 cron 触发点上时，next_fire 必须跳到下一次，
        // 不能原地返回同一个时刻（否则 due_tasks 里 last_run==now 时会误判到期）。
        let at_9am: DateTime<Local> = Local.with_ymd_and_hms(2026, 6, 15, 9, 0, 0).unwrap();
        let next_day_9am: DateTime<Local> = Local.with_ymd_and_hms(2026, 6, 16, 9, 0, 0).unwrap();

        let got = next_fire("0 9 * * *", SystemTime::from(at_9am)).unwrap();

        assert_eq!(got, SystemTime::from(next_day_9am));
    }

    #[test]
    fn next_fire_normalizes_5_field_manifest_cron_by_prepending_seconds() {
        // 5 段（清单惯例）与手写等价 6 段（秒=0）算出同一个 next_fire，
        // 证明规范化确实在起作用，不是巧合过了。
        let after = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let five_field = next_fire("* * * * *", after).unwrap();
        let six_field = next_fire("0 * * * * *", after).unwrap();
        assert_eq!(five_field, six_field);
    }

    #[test]
    fn next_fire_unparseable_cron_returns_err_not_panic() {
        let after = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let result = next_fire("this is not a cron expression", after);
        assert!(result.is_err());
    }

    // ---- jitter：确定性、且绝不提前 ----

    #[test]
    fn jitter_never_fires_earlier_than_base_and_never_exceeds_configured_max() {
        let base = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        for id in ["task-a", "task-b", "a-fairly-long-task-identifier-123", ""] {
            for jitter_secs in [0u64, 1, 5, 30, 300] {
                let jitter = Duration::from_secs(jitter_secs);
                let jittered = base + jitter_for(id, jitter);
                assert!(
                    jittered >= base,
                    "jitter must never fire earlier than base (id={id}, jitter={jitter_secs}s)"
                );
                assert!(
                    jittered <= base + jitter,
                    "jitter must not exceed configured max (id={id}, jitter={jitter_secs}s)"
                );
            }
        }
    }

    #[test]
    fn jitter_is_deterministic_for_the_same_task_id() {
        // 同一个 id + 同一个 jitter 上限，反复算必须得到同一个抖动量——
        // 这是"错峰但测试仍确定性"的前提，不能用 rand。
        let jitter = Duration::from_secs(30);
        let a = jitter_for("same-task-id", jitter);
        let b = jitter_for("same-task-id", jitter);
        assert_eq!(a, b);
    }

    // ---- due_tasks：注入 TestClock，禁止碰墙钟 ----

    /// 把 `secs` 对齐到分钟边界（`secs - secs % 60`），用于构造
    /// "每分钟" cron 场景下好推理的时刻：本地时区偏移在现代时区里都是整分钟，
    /// 所以"UTC 时间戳是 60 的倍数" 等价于 "本地时间的秒数为 0"，不受
    /// 跑测试的机器时区影响。
    fn minute_boundary(secs: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(secs - secs % 60)
    }

    #[test]
    fn due_tasks_every_minute_cron_due_not_due_then_due_again_after_advance() {
        let tmp = tempdir().unwrap();
        let layout = DataLayout::new(tmp.path().to_path_buf());
        let reg = TaskRegistry::new(&layout);
        reg.register("app1", &[task("tick", "* * * * *")]).unwrap();

        let now = minute_boundary(1_700_000_000);
        let clock = TestClock::new(now);

        // last_run = now - 61s（上一次触发窗口早就过了却没跑）→ due。
        reg.mark_run("app1", "tick", now - Duration::from_secs(61))
            .unwrap();
        assert!(
            due_tasks(&reg, &clock, Duration::ZERO)
                .iter()
                .any(|t| t.id == "tick"),
            "last_run 61s 前应判定为到期"
        );

        // last_run = now（刚跑完）→ 不 due。
        reg.mark_run("app1", "tick", now).unwrap();
        assert!(
            due_tasks(&reg, &clock, Duration::ZERO)
                .iter()
                .all(|t| t.id != "tick"),
            "last_run==now 不该判定为到期"
        );

        // 推进 1 分钟 → 到下一个 cron 周期边界，重新到期。
        clock.advance(Duration::from_secs(60));
        assert!(
            due_tasks(&reg, &clock, Duration::ZERO)
                .iter()
                .any(|t| t.id == "tick"),
            "advance 60s 后应重新到期"
        );
    }

    #[test]
    fn due_tasks_task_with_no_last_run_uses_epoch_baseline_and_is_immediately_due() {
        let tmp = tempdir().unwrap();
        let layout = DataLayout::new(tmp.path().to_path_buf());
        let reg = TaskRegistry::new(&layout);
        reg.register("app1", &[task("fresh", "0 9 * * *")]).unwrap();
        assert!(reg.all()[0].last_run.is_none());

        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let clock = TestClock::new(now);

        assert!(due_tasks(&reg, &clock, Duration::ZERO)
            .iter()
            .any(|t| t.id == "fresh"));
    }

    #[test]
    fn due_tasks_skips_task_with_unparseable_cron_without_panicking() {
        let tmp = tempdir().unwrap();
        let layout = DataLayout::new(tmp.path().to_path_buf());
        let reg = TaskRegistry::new(&layout);
        reg.register(
            "app1",
            &[
                task("bad-cron", "not-a-cron-expression-at-all"),
                task("good-cron", "* * * * *"),
            ],
        )
        .unwrap();
        // 一个坏 cron 的任务 last_run 早就过期，验证它没有被误判为到期。
        reg.mark_run("app1", "good-cron", SystemTime::UNIX_EPOCH)
            .unwrap();

        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let clock = TestClock::new(now);

        // 不 panic；坏 cron 的任务被跳过（不出现在到期列表里），好 cron 的正常判到期。
        let due = due_tasks(&reg, &clock, Duration::ZERO);
        assert!(due.iter().all(|t| t.id != "bad-cron"));
        assert!(due.iter().any(|t| t.id == "good-cron"));
    }

    #[test]
    fn due_tasks_applies_deterministic_jitter_and_can_delay_due_past_base_fire() {
        // 抖动只会让"到期"变晚，不会变早：构造一个 base_fire 恰好等于 now 的
        // 场景，加上非零抖动后必须变成"还没到期"（因为 base_fire + jitter > now）。
        let tmp = tempdir().unwrap();
        let layout = DataLayout::new(tmp.path().to_path_buf());
        let reg = TaskRegistry::new(&layout);
        reg.register("app1", &[task("tick", "* * * * *")]).unwrap();

        let now = minute_boundary(1_700_000_000);
        let last_run = now - Duration::from_secs(60);
        reg.mark_run("app1", "tick", last_run).unwrap();
        let clock = TestClock::new(now);

        // base_fire（无抖动的下一次触发点）恰好等于 now：确认场景搭对了。
        assert_eq!(next_fire("* * * * *", last_run).unwrap(), now);

        // 零抖动：base_fire == now，到期。
        assert!(due_tasks(&reg, &clock, Duration::ZERO)
            .iter()
            .any(|t| t.id == "tick"));

        // "tick" 这个 id 在 3600s 抖动上限下算出的确定性抖动量约 1331.69s（fnv1a
        // 摘要 % 上限的固定结果，非随机）——远大于 0，把到期点推到 now 之后，
        // 所以同一个任务在大抖动下应变为"还没到期"。断言用真实计算出的抖动量，
        // 不是拍脑袋的假设值。
        let big_jitter = Duration::from_secs(3600);
        let offset = jitter_for("tick", big_jitter);
        assert!(offset > Duration::ZERO && offset <= big_jitter);
        assert!(now + offset > now); // 抖动后的触发点确实晚于 now。

        assert!(
            due_tasks(&reg, &clock, big_jitter)
                .iter()
                .all(|t| t.id != "tick"),
            "大抖动应把 tick 的到期点推到 now 之后，本轮不该判定为到期"
        );
    }

    // ---- filter_due_for_open_apps：running-only 边界（纯函数，无需 mock_pi）----

    #[test]
    fn filter_due_for_open_apps_keeps_only_tasks_whose_app_is_open() {
        let due = vec![
            RegisteredTask {
                app_id: "open-app".into(),
                id: "t1".into(),
                cron: "* * * * *".into(),
                prompt: "p1".into(),
                catch_up: true,
                last_run: None,
            },
            RegisteredTask {
                app_id: "closed-app".into(),
                id: "t2".into(),
                cron: "* * * * *".into(),
                prompt: "p2".into(),
                catch_up: true,
                last_run: None,
            },
        ];

        let is_open: &(dyn Fn(&str) -> bool + Send + Sync) = &|id: &str| id == "open-app";
        let filtered = filter_due_for_open_apps(due, is_open);

        assert_eq!(
            filtered.len(),
            1,
            "关闭的 app 的到期任务不该出现在过滤结果里"
        );
        assert_eq!(filtered[0].app_id, "open-app");
    }

    #[test]
    fn filter_due_for_open_apps_empty_when_no_app_open() {
        let due = vec![RegisteredTask {
            app_id: "closed-app".into(),
            id: "t1".into(),
            cron: "* * * * *".into(),
            prompt: "p".into(),
            catch_up: true,
            last_run: None,
        }];

        let is_open: &(dyn Fn(&str) -> bool + Send + Sync) = &|_: &str| false;
        assert!(filter_due_for_open_apps(due, is_open).is_empty());
    }

    #[test]
    fn filter_due_for_open_apps_all_pass_through_when_all_open() {
        let due = vec![
            RegisteredTask {
                app_id: "a".into(),
                id: "t1".into(),
                cron: "* * * * *".into(),
                prompt: "p".into(),
                catch_up: true,
                last_run: None,
            },
            RegisteredTask {
                app_id: "b".into(),
                id: "t2".into(),
                cron: "* * * * *".into(),
                prompt: "p".into(),
                catch_up: true,
                last_run: None,
            },
        ];

        let is_open: &(dyn Fn(&str) -> bool + Send + Sync) = &|_: &str| true;
        assert_eq!(filter_due_for_open_apps(due, is_open).len(), 2);
    }

    // ---- catch_up：应用启动补跑「错过的最近一次」，不是「错过几次补几次」 ----

    #[test]
    fn catch_up_missed_two_periods_returns_task_exactly_once_not_twice() {
        let tmp = tempdir().unwrap();
        let layout = DataLayout::new(tmp.path().to_path_buf());
        let reg = TaskRegistry::new(&layout);
        reg.register("app1", &[task("tick", "* * * * *")]).unwrap();

        let now = minute_boundary(1_700_000_000);
        // last_run = now - 2 个周期（120s）：错过了至少 2 次每分钟触发。
        reg.mark_run("app1", "tick", now - Duration::from_secs(120))
            .unwrap();
        let clock = TestClock::new(now);

        let caught_up = catch_up(&reg, &clock);
        assert_eq!(
            caught_up.len(),
            1,
            "错过 2 个周期也只应返回 1 条（补跑一次），不是按错过次数返回多条"
        );
        assert_eq!(caught_up[0].id, "tick");
    }

    #[test]
    fn catch_up_respects_catch_up_false_flag_never_caught_up() {
        let tmp = tempdir().unwrap();
        let layout = DataLayout::new(tmp.path().to_path_buf());
        let reg = TaskRegistry::new(&layout);
        reg.register(
            "app1",
            &[ScheduledTask {
                id: "tick".into(),
                cron: "* * * * *".into(),
                prompt: "prompt-tick".into(),
                catch_up: false,
            }],
        )
        .unwrap();

        let now = minute_boundary(1_700_000_000);
        // 同样错过 2 个周期，但 catch_up=false。
        reg.mark_run("app1", "tick", now - Duration::from_secs(120))
            .unwrap();
        let clock = TestClock::new(now);

        assert!(
            catch_up(&reg, &clock).is_empty(),
            "catch_up=false 的任务即便严重错过也不该被补跑"
        );
    }

    #[test]
    fn catch_up_excludes_task_that_just_ran_and_is_not_overdue() {
        let tmp = tempdir().unwrap();
        let layout = DataLayout::new(tmp.path().to_path_buf());
        let reg = TaskRegistry::new(&layout);
        reg.register("app1", &[task("tick", "* * * * *")]).unwrap();

        let now = minute_boundary(1_700_000_000);
        reg.mark_run("app1", "tick", now).unwrap(); // 刚跑完，未到期
        let clock = TestClock::new(now);

        assert!(
            catch_up(&reg, &clock).is_empty(),
            "last_run==now 说明这个任务的下一次触发点还在未来，不该被判定为需要补跑"
        );
    }

    #[test]
    fn catch_up_never_run_task_uses_epoch_baseline_and_is_caught_up() {
        let tmp = tempdir().unwrap();
        let layout = DataLayout::new(tmp.path().to_path_buf());
        let reg = TaskRegistry::new(&layout);
        reg.register("app1", &[task("fresh", "0 9 * * *")]).unwrap();
        assert!(reg.all()[0].last_run.is_none());

        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let clock = TestClock::new(now);

        let caught_up = catch_up(&reg, &clock);
        assert_eq!(
            caught_up.len(),
            1,
            "从没跑过的任务应以 epoch 兜底，立刻判定为需要补跑"
        );
        assert_eq!(caught_up[0].id, "fresh");
    }

    #[test]
    fn catch_up_skips_unparseable_cron_without_panicking() {
        let tmp = tempdir().unwrap();
        let layout = DataLayout::new(tmp.path().to_path_buf());
        let reg = TaskRegistry::new(&layout);
        reg.register(
            "app1",
            &[
                task("bad-cron", "not-a-cron-expression-at-all"),
                task("good-cron", "* * * * *"),
            ],
        )
        .unwrap();
        reg.mark_run("app1", "good-cron", SystemTime::UNIX_EPOCH)
            .unwrap();

        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let clock = TestClock::new(now);

        let caught_up = catch_up(&reg, &clock);
        assert!(
            caught_up.iter().all(|t| t.id != "bad-cron"),
            "坏 cron 不 panic，直接被排除"
        );
        assert!(caught_up.iter().any(|t| t.id == "good-cron"));
    }

    #[test]
    fn catch_up_only_returns_one_entry_per_task_across_multiple_apps() {
        // 多个 app 各自有一个严重错过的任务：确认 catch_up 是"逐任务只判一次"，
        // 不会因为多 app/多任务混在同一份注册表里而产生交叉污染或重复条目。
        let tmp = tempdir().unwrap();
        let layout = DataLayout::new(tmp.path().to_path_buf());
        let reg = TaskRegistry::new(&layout);
        reg.register("app1", &[task("tick", "* * * * *")]).unwrap();
        reg.register("app2", &[task("tick", "* * * * *")]).unwrap();

        let now = minute_boundary(1_700_000_000);
        reg.mark_run("app1", "tick", now - Duration::from_secs(600))
            .unwrap();
        reg.mark_run("app2", "tick", now).unwrap(); // app2 未到期
        let clock = TestClock::new(now);

        let caught_up = catch_up(&reg, &clock);
        assert_eq!(caught_up.len(), 1);
        assert_eq!(caught_up[0].app_id, "app1");
    }
}
