//! P6-C：`ApprovalStore`——把写操作的"暂存调用"与"总是允许"放行规则从
//! `McpManager` 的进程内存表（`pending`/`always_allow`，重启即丢，见
//! `docs/superpowers/specs/2026-09-02-p6c-approval-trust-design.md` §1）
//! 搬到持久化存储：`DataLayout::approvals_dir()` 下两个整份 JSON 文件
//! （`staged.json`/`rules.json`），落盘模式与 `scheduler.rs::TaskRegistry`
//! 同款——「读全量 -> 内存改 -> 写 `.tmp` -> `rename`」原子替换，用进程内
//! `static MUTEX` 串行化同一时刻的读改写（本模块只服务单个 host 进程内的并发
//! 调用者，不处理多进程并发写同一份文件）。
//!
//! `ApprovalStore` 本身不缓存任何数据（同 `TaskRegistry`/`NotificationStore`
//! 的"无内存态、现读现写"风格）——每次调用都是一次完整的文件读改写，`layout`
//! 是唯一字段；这也是 `staged_persists_across_fresh_store` 测试要验证的
//! 属性：同一个 `DataLayout` 根目录上现造的第二个 `ApprovalStore` 实例必须
//! 看到第一个实例落盘的数据。
//!
//! fail-closed 两处（spec §6 不变量）：
//! - `is_allowed`：`rules.json` 读取/解析失败一律按"未放行"（`false`），不
//!   panic、不放宽——见 `is_allowed_is_false_when_rules_file_is_corrupt`。
//! - `take`：先从文件里删除匹配项、写盘成功后才把它返回给调用方
//!   （at-most-once）；找不到匹配项返回 `Ok(None)`，调用方据此判断"这条暂存
//!   已经被别的请求消费过/根本不存在"，不执行。

use crate::paths::DataLayout;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

/// `SystemTime` -> unix 秒，钳制到 `UNIX_EPOCH` 之前的异常值为 0（不 panic/
/// 不回绕）。`mcp.rs::host_mcp_call`/`notifications.rs`/`scheduler.rs` 三处都
/// 需要把墙钟/`Clock::now()` 换算成本模块用的 `i64` unix 秒——集中放这里，避免
/// 三份各自重写一遍 `duration_since(UNIX_EPOCH)` 的样板代码（同 `audit.rs`/
/// `vault.rs` 已有的同款转换，但那两处各自内联、量级小；这里刻意抽出来是因为
/// 三个新调用点都在本次改动里新增，值得共用一处定义）。
pub(crate) fn unix_secs(t: std::time::SystemTime) -> i64 {
    t.duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// 当前墙钟时间的 unix 秒——生产路径专用（`host_mcp_call`/`create_confirm`/
/// `respond_staged` 的 `add_rule` 都需要"现在"这个时间戳，但它们本身不经过
/// `scheduler::Clock` 注入——`Clock` trait 是调度到期判定专用的确定性时钟，
/// 不是全局时间源）。测试要确定性时间请直接调 `stage`/`add_rule`/
/// `expire_older_than` 并传入手写的 `i64` 常量，不要依赖这个函数。
pub(crate) fn unix_now() -> i64 {
    unix_secs(std::time::SystemTime::now())
}

/// 暂存调用的 TTL：超过这个时长仍未被验收（允许/拒绝）的暂存调用，调度器
/// tick（Task4 `expire_staged`）会把它当作过期拒绝处理并审计 `expired`。
pub const STAGED_TTL_SECS: i64 = 24 * 3600;

/// 一条暂存待批的写调用："谁"（`app_id`）在"哪个连接器的哪个工具"
/// （`server`/`tool`）上想执行"什么参数"（`args`），何时登记
/// （`created_at`，unix 秒）。`id` 就是 `mcp_socket.rs` 回给模型的
/// `confirm_id`——`stage`/`take` 全靠它做一一对应。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StagedCall {
    pub id: String,
    pub app_id: String,
    pub server: String,
    pub tool: String,
    pub args: serde_json::Value,
    pub created_at: i64,
}

/// 一条"总是允许"放行规则：`(app_id, server, tool)` 三元组精确作用域——三者
/// 全等才命中（`is_allowed`），不做前缀/通配匹配。`created_at` 只是记录用
/// （审计/界面展示"何时放行的"），不参与匹配逻辑。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct ApprovalRule {
    pub app_id: String,
    pub server: String,
    pub tool: String,
    pub created_at: i64,
}

/// 进程内单把互斥锁：串行化对 `staged.json`/`rules.json` 的所有读改写
/// （同一进程内的所有 `ApprovalStore` 实例，不论指向哪个 `DataLayout` 根，
/// 共用这一把锁——比 `scheduler.rs::registry_lock_for` 的按路径分片更保守，
/// 但本模块的调用量级远低于需要细粒度并发的场景，简单更要紧）。
static MUTEX: Mutex<()> = Mutex::new(());

/// 生成一个跨进程重启也几乎不可能撞车的 id：`"<prefix>-<纳秒时间戳>-<进程内
/// 单调计数器>"`——`notifications.rs::next_notification_id`（该模式最早的
/// 引入处，见其文档"进程级单调计数器 + 纳秒时间戳拼出的 ID：跨多个各自现造的
/// XxxStore 实例也能保证互不相同，不需要引入 UUID 依赖"）与本模块原先各自维护
/// 一份「时间戳+计数器」拼接逻辑之间的共享抽出，供多处持久化 id 生成统一调用：
/// - 下方 `stage`（`ApprovalStore` 自己的暂存调用 id，取代原先内联的
///   `next_staged_id(now)`——原实现的 `now` 由调用方传入，`stage` 内部一处
///   使用；改用本函数后 id 本身不再依赖调用方传入的时间，`created_at` 字段仍
///   由调用方单独记录，语义不变）；
/// - `skills::SkillStore::register_pending_skill_install`（P6-B 批次 D 合并前
///   审查 C1：pending 技能安装表持久化跨进程重启，但此前生成 `confirm_id` 用
///   的是裸 `AtomicU64`——没有时间戳分量，进程重启后计数器归零、又从同一个
///   id 开始 mint，与持久化数据的生存期不匹配，见该处调用点的文档）；
/// - `lib.rs` 市场下载临时目录命名（取代裸 `.market-dl-{now}`，同一秒内并发
///   安装会撞同一个目录名，见该处调用点的文档）。
///
/// 纳秒时间戳保证跨进程重启几乎不可能撞车（两次进程启动精确落在同一纳秒的
/// 概率可忽略）；进程内单调计数器保证同一进程内、`SystemTime` 精度粗于 1ns
/// 的平台上多次调用仍不会撞。两者组合不需要引入 `uuid`/`rand` 依赖。
pub(crate) fn fresh_id(prefix: &str) -> String {
    static COUNTER: OnceLock<AtomicU64> = OnceLock::new();
    let counter = COUNTER.get_or_init(|| AtomicU64::new(0));
    let n = counter.fetch_add(1, Ordering::SeqCst);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("{prefix}-{nanos}-{n}")
}

/// 暂存调用与放行规则的持久化存储（P6-C，取代 `McpManager` 里的
/// `pending`/`always_allow` 两张内存表）。见模块文档。
pub struct ApprovalStore {
    layout: DataLayout,
}

impl ApprovalStore {
    pub fn new(layout: DataLayout) -> Self {
        Self { layout }
    }

    fn staged_path(&self) -> std::path::PathBuf {
        self.layout.approvals_dir().join("staged.json")
    }

    fn rules_path(&self) -> std::path::PathBuf {
        self.layout.approvals_dir().join("rules.json")
    }

    /// 读整份 `staged.json`：文件不存在视为空列表（首次运行，不是错误）；
    /// 存在但读取/解析失败才是真错误（`Err`）。
    fn load_staged(&self) -> Result<Vec<StagedCall>, String> {
        load_json_file(&self.staged_path())
    }

    /// 原子写：先写 `.json.tmp` 再 `rename`，同 `scheduler.rs::TaskRegistry::save`。
    fn save_staged(&self, items: &[StagedCall]) -> Result<(), String> {
        save_json_file(&self.staged_path(), items)
    }

    fn load_rules(&self) -> Result<Vec<ApprovalRule>, String> {
        load_json_file(&self.rules_path())
    }

    fn save_rules(&self, items: &[ApprovalRule]) -> Result<(), String> {
        save_json_file(&self.rules_path(), items)
    }

    /// 登记一条暂存调用，返回它的 id（`stg-<now>-<计数器>`）。`now` 由调用方
    /// 传入（不直接读墙钟）——同 `scheduler.rs::Clock` 的注入式时钟哲学，保证
    /// TTL/过期相关测试的确定性。
    pub fn stage(
        &self,
        app_id: &str,
        server: &str,
        tool: &str,
        args: serde_json::Value,
        now: i64,
    ) -> Result<String, String> {
        let _guard = MUTEX.lock().expect("ApprovalStore mutex poisoned");
        let mut staged = self.load_staged()?;
        let id = fresh_id("stg");
        staged.push(StagedCall {
            id: id.clone(),
            app_id: app_id.to_string(),
            server: server.to_string(),
            tool: tool.to_string(),
            args,
            created_at: now,
        });
        self.save_staged(&staged)?;
        Ok(id)
    }

    /// 原子取出：命中 → 先从文件删除、写盘成功后才返回 `Some`（at-most-once，
    /// 并发两次 `take` 同一个 `id` 只有一次能拿到 `Some`，因为整个"读-删-写"
    /// 过程持有 `MUTEX`）；未命中（已被消费/id 不存在）→ `Ok(None)`。
    pub fn take(&self, id: &str) -> Result<Option<StagedCall>, String> {
        let _guard = MUTEX.lock().expect("ApprovalStore mutex poisoned");
        let mut staged = self.load_staged()?;
        let Some(pos) = staged.iter().position(|s| s.id == id) else {
            return Ok(None);
        };
        let item = staged.remove(pos);
        self.save_staged(&staged)?;
        Ok(Some(item))
    }

    /// 列出暂存调用；`app_id` 为 `Some` 时只返回该应用的。
    pub fn list_staged(&self, app_id: Option<&str>) -> Result<Vec<StagedCall>, String> {
        let _guard = MUTEX.lock().expect("ApprovalStore mutex poisoned");
        let staged = self.load_staged()?;
        Ok(match app_id {
            Some(a) => staged.into_iter().filter(|s| s.app_id == a).collect(),
            None => staged,
        })
    }

    /// 取出并从文件删除全部到期项（`now - created_at >= ttl_secs`），返回被
    /// 取出的那些——调用方（Task4 `expire_staged`）负责逐条 ack 通知、审计
    /// `expired`。未到期项原样留在文件里。
    pub fn expire_older_than(&self, now: i64, ttl_secs: i64) -> Result<Vec<StagedCall>, String> {
        let _guard = MUTEX.lock().expect("ApprovalStore mutex poisoned");
        let mut staged = self.load_staged()?;
        let mut expired = Vec::new();
        staged.retain(|s| {
            if now - s.created_at >= ttl_secs {
                expired.push(s.clone());
                false
            } else {
                true
            }
        });
        if !expired.is_empty() {
            self.save_staged(&staged)?;
        }
        Ok(expired)
    }

    /// 登记一条放行规则（幂等：同一 `(app_id, server, tool)` 三元组重复调用
    /// 只保留一条，`created_at` 取最新一次调用的值）。
    pub fn add_rule(&self, app_id: &str, server: &str, tool: &str, now: i64) -> Result<(), String> {
        let _guard = MUTEX.lock().expect("ApprovalStore mutex poisoned");
        let mut rules = self.load_rules()?;
        rules.retain(|r| !(r.app_id == app_id && r.server == server && r.tool == tool));
        rules.push(ApprovalRule {
            app_id: app_id.to_string(),
            server: server.to_string(),
            tool: tool.to_string(),
            created_at: now,
        });
        self.save_rules(&rules)
    }

    /// 撤销一条放行规则；返回是否真的删掉了一条（`false` = 本就不存在，
    /// 幂等）。
    pub fn remove_rule(&self, app_id: &str, server: &str, tool: &str) -> Result<bool, String> {
        let _guard = MUTEX.lock().expect("ApprovalStore mutex poisoned");
        let mut rules = self.load_rules()?;
        let before = rules.len();
        rules.retain(|r| !(r.app_id == app_id && r.server == server && r.tool == tool));
        let removed = rules.len() != before;
        if removed {
            self.save_rules(&rules)?;
        }
        Ok(removed)
    }

    /// 列出放行规则；`app_id` 为 `Some` 时只返回该应用的。
    pub fn list_rules(&self, app_id: Option<&str>) -> Result<Vec<ApprovalRule>, String> {
        let _guard = MUTEX.lock().expect("ApprovalStore mutex poisoned");
        let rules = self.load_rules()?;
        Ok(match app_id {
            Some(a) => rules.into_iter().filter(|r| r.app_id == a).collect(),
            None => rules,
        })
    }

    /// 卸载应用时的清理入口（Task5 `connectors::on_uninstall`）：删掉该
    /// `app_id` 名下全部放行规则与暂存调用，返回删掉的规则条数与被丢弃的暂存
    /// 调用（调用方据此逐条审计 `rejected`）。只影响该 `app_id`，其它应用的
    /// 记录原样保留；幂等（该 app 本就没有任何记录时返回 `(0, [])`）。
    pub fn remove_all_for_app(&self, app_id: &str) -> Result<(usize, Vec<StagedCall>), String> {
        let _guard = MUTEX.lock().expect("ApprovalStore mutex poisoned");

        let mut rules = self.load_rules()?;
        let before = rules.len();
        rules.retain(|r| r.app_id != app_id);
        let removed_rules = before - rules.len();
        if removed_rules > 0 {
            self.save_rules(&rules)?;
        }

        let mut staged = self.load_staged()?;
        let mut dropped = Vec::new();
        staged.retain(|s| {
            if s.app_id == app_id {
                dropped.push(s.clone());
                false
            } else {
                true
            }
        });
        if !dropped.is_empty() {
            self.save_staged(&staged)?;
        }

        Ok((removed_rules, dropped))
    }

    /// 查 `(app_id, server, tool)` 是否已被放行规则覆盖。fail-closed：
    /// `rules.json` 读取/解析失败一律按 `false`（未放行），绝不因为存储层
    /// 故障而误放行一次写操作——见 `is_allowed_is_false_when_rules_file_is_corrupt`。
    pub fn is_allowed(&self, app_id: &str, server: &str, tool: &str) -> bool {
        let _guard = MUTEX.lock().expect("ApprovalStore mutex poisoned");
        match self.load_rules() {
            Ok(rules) => rules
                .iter()
                .any(|r| r.app_id == app_id && r.server == server && r.tool == tool),
            Err(_) => false,
        }
    }
}

/// 读整份 JSON 数组文件：文件不存在 → 空列表（首次运行，不是错误）；存在但
/// 读取失败或解析失败 → `Err`（供 `is_allowed` 等 fail-closed 调用方区分
/// "本来就没有" 和 "存储损坏"）。
fn load_json_file<T: for<'de> Deserialize<'de>>(path: &std::path::Path) -> Result<Vec<T>, String> {
    match std::fs::read_to_string(path) {
        Ok(s) => serde_json::from_str(&s).map_err(|e| format!("{} 解析失败：{e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(format!("{} 读取失败：{e}", path.display())),
    }
}

/// 原子写：先写 `.json.tmp` 再 `rename`，同 `scheduler.rs::TaskRegistry::save`。
pub(crate) fn save_json_file<T: Serialize + ?Sized>(
    path: &std::path::Path,
    items: &T,
) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let tmp = path.with_extension("json.tmp");
    let body = serde_json::to_string_pretty(items).map_err(|e| e.to_string())?;
    std::fs::write(&tmp, body).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, path).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_store() -> (tempfile::TempDir, ApprovalStore) {
        let tmp = tempfile::tempdir().unwrap();
        let layout = DataLayout::new(tmp.path().to_path_buf());
        let store = ApprovalStore::new(layout);
        (tmp, store)
    }

    #[test]
    fn stage_then_take_is_at_most_once() {
        let (_tmp, store) = temp_store();
        let id = store
            .stage("app1", "srv1", "tool1", serde_json::json!({"a": 1}), 1000)
            .expect("stage 应成功");

        let first = store.take(&id).expect("take 应成功");
        assert!(first.is_some(), "第一次 take 应拿到暂存调用");

        let second = store.take(&id).expect("take 应成功");
        assert!(
            second.is_none(),
            "第二次 take 同一个 id 应为 None（at-most-once）"
        );

        assert!(
            store
                .list_staged(None)
                .expect("list_staged 应成功")
                .is_empty(),
            "取出后 list_staged 应为空"
        );
    }

    #[test]
    fn staged_persists_across_fresh_store() {
        let tmp = tempfile::tempdir().unwrap();
        let layout = DataLayout::new(tmp.path().to_path_buf());
        let store1 = ApprovalStore::new(layout.clone());
        store1
            .stage("app1", "srv1", "tool1", serde_json::json!({}), 1000)
            .expect("stage 应成功");

        let store2 = ApprovalStore::new(layout);
        let staged = store2.list_staged(None).expect("list_staged 应成功");
        assert_eq!(
            staged.len(),
            1,
            "同一 tempdir 建的第二个 ApprovalStore 应看到已落盘的暂存调用"
        );
        assert_eq!(staged[0].tool, "tool1");
    }

    #[test]
    fn expire_returns_and_removes_only_old_items() {
        let (_tmp, store) = temp_store();
        store
            .stage("app1", "srv1", "old", serde_json::json!({}), 800)
            .expect("stage 应成功");
        store
            .stage("app1", "srv1", "new", serde_json::json!({}), 950)
            .expect("stage 应成功");

        let expired = store
            .expire_older_than(1000, 100)
            .expect("expire_older_than 应成功");
        assert_eq!(expired.len(), 1, "只有 created_at=800 的那条应到期");
        assert_eq!(expired[0].tool, "old");

        let remaining = store.list_staged(None).expect("list_staged 应成功");
        assert_eq!(remaining.len(), 1, "created_at=950 的那条应保留");
        assert_eq!(remaining[0].tool, "new");
    }

    #[test]
    fn rules_exact_scope_and_revoke() {
        let (_tmp, store) = temp_store();
        store
            .add_rule("a", "s", "t", 1000)
            .expect("add_rule 应成功");

        assert!(store.is_allowed("a", "s", "t"), "精确三元组命中应放行");
        assert!(!store.is_allowed("a", "s", "t2"), "tool 不同不应命中");
        assert!(!store.is_allowed("b", "s", "t"), "app_id 不同不应命中");
        assert!(
            !store.is_allowed("a", "s2", "t"),
            "server 不同不应命中（Review A：此前无任何测试变化 server 字段，去掉 server 比对也能全绿）"
        );

        let removed = store
            .remove_rule("a", "s", "t")
            .expect("remove_rule 应成功");
        assert!(removed, "撤销一条真实存在的规则应返回 true");
        assert!(
            !store.is_allowed("a", "s", "t"),
            "撤销后下一次调用应立即回到暂存（不再放行）"
        );

        let removed_again = store
            .remove_rule("a", "s", "t")
            .expect("remove_rule 应成功");
        assert!(
            !removed_again,
            "重复撤销同一条应返回 false（幂等，不是错误）"
        );
    }

    #[test]
    fn remove_all_for_app_clears_rules_and_staged_of_that_app_only() {
        let (_tmp, store) = temp_store();
        store
            .add_rule("a", "s", "t1", 1000)
            .expect("add_rule 应成功");
        store
            .add_rule("b", "s", "t1", 1000)
            .expect("add_rule 应成功");
        store
            .stage("a", "s", "t2", serde_json::json!({}), 1000)
            .expect("stage 应成功");
        store
            .stage("b", "s", "t2", serde_json::json!({}), 1000)
            .expect("stage 应成功");

        let (removed_rules, dropped_staged) = store
            .remove_all_for_app("a")
            .expect("remove_all_for_app 应成功");
        assert_eq!(removed_rules, 1, "只应删掉 app a 的一条规则");
        assert_eq!(dropped_staged.len(), 1, "只应丢弃 app a 的一条暂存调用");
        assert_eq!(dropped_staged[0].app_id, "a");

        assert!(store
            .list_rules(Some("a"))
            .expect("list_rules 应成功")
            .is_empty());
        assert!(store
            .list_staged(Some("a"))
            .expect("list_staged 应成功")
            .is_empty());

        assert_eq!(
            store
                .list_rules(Some("b"))
                .expect("list_rules 应成功")
                .len(),
            1,
            "app b 的规则不受影响"
        );
        assert_eq!(
            store
                .list_staged(Some("b"))
                .expect("list_staged 应成功")
                .len(),
            1,
            "app b 的暂存不受影响"
        );
    }

    #[test]
    fn is_allowed_is_false_when_rules_file_is_corrupt() {
        let tmp = tempfile::tempdir().unwrap();
        let layout = DataLayout::new(tmp.path().to_path_buf());
        std::fs::create_dir_all(layout.approvals_dir()).expect("建目录应成功");
        std::fs::write(
            layout.approvals_dir().join("rules.json"),
            "not valid json{{{",
        )
        .expect("写入损坏文件应成功");

        let store = ApprovalStore::new(layout);
        assert!(
            !store.is_allowed("a", "s", "t"),
            "rules.json 损坏时 is_allowed 应 fail-closed 返回 false，不能 panic"
        );
    }
}
