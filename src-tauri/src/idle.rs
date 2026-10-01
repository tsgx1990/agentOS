//! P6-F：应用会话的活动跟踪与（后续任务追加的）空闲回收。

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::PathBuf;

use crate::app_state::AppState;
use crate::notifications::NotificationStore;
use crate::paths::DataLayout;

/// 一个已打开应用会话的活动快照（时间均为墙钟 unix 秒）。
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct AppActivity {
    pub opened_at: i64,
    pub last_activity_at: i64,
    pub in_turn: bool,
}

/// 每应用的打开时刻 / 最近活动 / 是否在回合中 / 休眠集合。
///
/// 用 `std::sync::Mutex`：纯内存、临界区极短，guard 绝不跨 `.await`。
#[derive(Default)]
pub struct ActivityTracker {
    inner: std::sync::Mutex<TrackerInner>,
}

#[derive(Default)]
struct TrackerInner {
    live: HashMap<String, AppActivity>,
    dormant: HashSet<String>,
}

impl ActivityTracker {
    fn lock(&self) -> std::sync::MutexGuard<'_, TrackerInner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// 应用会话打开：插入 `{now, now, false}`，并清掉休眠标记。
    pub fn on_open(&self, app_id: &str, now: i64) {
        let mut g = self.lock();
        g.dormant.remove(app_id);
        g.live.insert(
            app_id.to_string(),
            AppActivity {
                opened_at: now,
                last_activity_at: now,
                in_turn: false,
            },
        );
    }

    /// 会话崩溃后被自动重启：保留 `opened_at`，回合状态清零；条目不存在则等同 `on_open`。
    pub fn on_restart(&self, app_id: &str, now: i64) {
        let mut g = self.lock();
        match g.live.get_mut(app_id) {
            Some(a) => {
                a.last_activity_at = now;
                a.in_turn = false;
            }
            None => {
                drop(g);
                self.on_open(app_id, now);
            }
        }
    }

    /// 只更新已存在的条目——关闭后迟到的事件不得把条目复活。
    pub fn touch(&self, app_id: &str, now: i64) {
        if let Some(a) = self.lock().live.get_mut(app_id) {
            a.last_activity_at = now;
        }
    }

    pub fn begin_turn(&self, app_id: &str, now: i64) {
        if let Some(a) = self.lock().live.get_mut(app_id) {
            a.in_turn = true;
            a.last_activity_at = now;
        }
    }

    pub fn end_turn(&self, app_id: &str, now: i64) {
        if let Some(a) = self.lock().live.get_mut(app_id) {
            a.in_turn = false;
            a.last_activity_at = now;
        }
    }

    /// 移除 live 条目，不动休眠集合。
    pub fn on_close(&self, app_id: &str) {
        self.lock().live.remove(app_id);
    }

    pub fn mark_dormant(&self, app_id: &str) {
        self.lock().dormant.insert(app_id.to_string());
    }

    /// 休眠中的应用 id（已排序）。
    pub fn dormant(&self) -> Vec<String> {
        let mut v: Vec<String> = self.lock().dormant.iter().cloned().collect();
        v.sort();
        v
    }

    pub fn get(&self, app_id: &str) -> Option<AppActivity> {
        self.lock().live.get(app_id).cloned()
    }

    pub fn snapshot(&self) -> Vec<(String, AppActivity)> {
        let mut v: Vec<(String, AppActivity)> = self
            .lock()
            .live
            .iter()
            .map(|(k, a)| (k.clone(), a.clone()))
            .collect();
        v.sort_by(|a, b| a.0.cmp(&b.0));
        v
    }
}

impl ActivityTracker {
    /// 关闭前在锁内复核：条目存在、!in_turn、now - last_activity_at >= timeout。
    pub fn still_idle(&self, app_id: &str, now: i64, timeout_secs: i64) -> bool {
        match self.lock().live.get(app_id) {
            Some(a) => !a.in_turn && now - a.last_activity_at >= timeout_secs,
            None => false,
        }
    }

    /// 卸载用：live 与 dormant 都删。
    pub fn forget(&self, app_id: &str) {
        let mut g = self.lock();
        g.live.remove(app_id);
        g.dormant.remove(app_id);
    }
}

pub const DEFAULT_IDLE_TIMEOUT_SECS: i64 = 900;
/// 下限 5 分钟，防止误设成几秒把正在用的应用关掉。
pub const MIN_IDLE_TIMEOUT_SECS: i64 = 300;
pub const MAX_IDLE_TIMEOUT_SECS: i64 = 86_400;

fn default_true() -> bool {
    true
}

fn default_timeout() -> i64 {
    DEFAULT_IDLE_TIMEOUT_SECS
}

/// 空闲回收策略（`<root>/idle-policy.json`）。
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct IdlePolicy {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_timeout")]
    pub timeout_secs: i64,
    #[serde(default)]
    pub exempt_apps: BTreeSet<String>,
}

impl Default for IdlePolicy {
    fn default() -> Self {
        Self {
            enabled: true,
            timeout_secs: DEFAULT_IDLE_TIMEOUT_SECS,
            exempt_apps: BTreeSet::new(),
        }
    }
}

pub struct IdlePolicyStore {
    path: PathBuf,
}

impl IdlePolicyStore {
    pub fn new(layout: &DataLayout) -> Self {
        Self {
            path: layout.idle_policy_path(),
        }
    }

    /// 文件缺失或损坏 → 默认值（不报错）；越界的 timeout 视同损坏。
    pub fn load(&self) -> IdlePolicy {
        std::fs::read(&self.path)
            .ok()
            .and_then(|b| serde_json::from_slice::<IdlePolicy>(&b).ok())
            .filter(|p| (MIN_IDLE_TIMEOUT_SECS..=MAX_IDLE_TIMEOUT_SECS).contains(&p.timeout_secs))
            .unwrap_or_default()
    }

    /// timeout 越界 → Err 且不动文件；否则写临时文件再 rename。
    pub fn save(&self, p: &IdlePolicy) -> Result<(), String> {
        if !(MIN_IDLE_TIMEOUT_SECS..=MAX_IDLE_TIMEOUT_SECS).contains(&p.timeout_secs) {
            return Err(format!(
                "空闲时长须在 {MIN_IDLE_TIMEOUT_SECS}–{MAX_IDLE_TIMEOUT_SECS} 秒之间"
            ));
        }
        crate::approvals::save_json_file(&self.path, p)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Exempt {
    Maker,
    PolicyOff,
    UserExempt,
    InTurn,
    PendingApproval,
    BackgroundSession,
}

impl Exempt {
    pub fn label(&self) -> &'static str {
        match self {
            Exempt::Maker => "内置 Maker",
            Exempt::PolicyOff => "空闲回收已关闭",
            Exempt::UserExempt => "已设为不休眠",
            Exempt::InTurn => "正在回复",
            Exempt::PendingApproval => "有待批调用",
            Exempt::BackgroundSession => "后台任务运行中",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    Keep(Option<Exempt>),
    Recycle { idle_secs: i64 },
}

/// 纯函数。判定顺序：Maker → 策略关闭 → 用户豁免 → 回复中 → 待批 → 后台会话 → 未超时 → 回收。
/// 豁免原因与是否超时无关地返回（面板据此显示「不会休眠：原因」）。
pub fn idle_verdict(
    app_id: &str,
    act: &AppActivity,
    policy: &IdlePolicy,
    now: i64,
    has_pending: bool,
    has_background: bool,
) -> Verdict {
    if app_id == crate::maker::MAKER_APP_ID {
        return Verdict::Keep(Some(Exempt::Maker));
    }
    if !policy.enabled {
        return Verdict::Keep(Some(Exempt::PolicyOff));
    }
    if policy.exempt_apps.contains(app_id) {
        return Verdict::Keep(Some(Exempt::UserExempt));
    }
    if act.in_turn {
        return Verdict::Keep(Some(Exempt::InTurn));
    }
    if has_pending {
        return Verdict::Keep(Some(Exempt::PendingApproval));
    }
    if has_background {
        return Verdict::Keep(Some(Exempt::BackgroundSession));
    }
    let idle = now - act.last_activity_at;
    if idle < policy.timeout_secs {
        return Verdict::Keep(None);
    }
    Verdict::Recycle { idle_secs: idle }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    pub app_id: String,
    pub idle_secs: i64,
    pub timeout_secs: i64,
    pub root_pid: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct Recycled {
    pub app_id: String,
    pub idle_secs: i64,
    pub freed_rss_bytes: Option<u64>,
}

/// 该应用是否有待批调用；读失败按「有」处理（宁可不回收）。
pub fn has_pending_approval(layout: &DataLayout, app_id: &str) -> bool {
    crate::approvals::ApprovalStore::new(layout.clone())
        .list_staged(Some(app_id))
        .map(|v| !v.is_empty())
        .unwrap_or(true)
}

/// 出现过后台会话的应用 id（不论是否拿到 pid）——回收计划与资源面板共用这一口径。
pub fn background_app_ids(sessions: &[crate::session_mgr::HeadlessSession]) -> HashSet<String> {
    sessions.iter().map(|h| h.app_id.clone()).collect()
}

/// 只读：列出该回收的应用。
pub async fn plan_idle_recycle(state: &AppState, layout: &DataLayout, now: i64) -> Vec<Candidate> {
    let policy = IdlePolicyStore::new(layout).load();
    let background = background_app_ids(&crate::session_mgr::running_headless_sessions());
    // 现取各应用根 pid（崩溃重启后会变）；guard 在本语句内释放。
    let pids: HashMap<String, Option<u32>> = {
        let g = state.app_sessions.lock().await;
        g.iter().map(|(id, s)| (id.clone(), s.child_id())).collect()
    };
    let mut out = Vec::new();
    for (id, act) in state.activity.snapshot() {
        let Some(root_pid) = pids.get(&id).copied() else {
            continue;
        };
        let pending = has_pending_approval(layout, &id);
        if let Verdict::Recycle { idle_secs } =
            idle_verdict(&id, &act, &policy, now, pending, background.contains(&id))
        {
            out.push(Candidate {
                app_id: id,
                idle_secs,
                timeout_secs: policy.timeout_secs,
                root_pid,
            });
        }
    }
    out
}

/// 对每个候选：锁内复核仍空闲 → `close_app_in` → 成功则标休眠并发通知。
pub async fn recycle(
    state: &AppState,
    notifications: &NotificationStore,
    picks: Vec<(Candidate, Option<u64>)>,
    now: i64,
) -> Vec<Recycled> {
    let mut done = Vec::new();
    for (c, rss) in picks {
        if !state.activity.still_idle(&c.app_id, now, c.timeout_secs) {
            continue;
        }
        if !crate::session_mgr::close_app_in(state, &c.app_id).await {
            continue;
        }
        state.activity.mark_dormant(&c.app_id);
        let mut body = format!("空闲 {} 分钟，已自动关闭", c.idle_secs / 60);
        if let Some(b) = rss {
            body.push_str(&format!("，释放约 {} MB", b / (1024 * 1024)));
        }
        body.push_str("。点应用卡片即可重新打开。");
        if let Err(e) = notifications.add("update", &c.app_id, "已休眠", &body) {
            eprintln!("休眠通知写入失败：{e}");
        }
        done.push(Recycled {
            app_id: c.app_id,
            idle_secs: c.idle_secs,
            freed_rss_bytes: rss,
        });
    }
    done
}

/// 墙钟 unix 秒。
pub fn now_secs() -> i64 {
    crate::approvals::unix_secs(std::time::SystemTime::now())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tracker_open_touch_close_lifecycle() {
        let t = ActivityTracker::default();
        t.on_open("a", 100);
        assert_eq!(
            t.get("a"),
            Some(AppActivity {
                opened_at: 100,
                last_activity_at: 100,
                in_turn: false
            })
        );
        t.touch("a", 150);
        assert_eq!(t.get("a").unwrap().last_activity_at, 150);
        t.on_close("a");
        assert_eq!(t.get("a"), None);
        t.touch("a", 200);
        assert_eq!(t.get("a"), None, "关闭后迟到的事件不得复活条目");
    }

    #[test]
    fn begin_end_turn_toggles_in_turn() {
        let t = ActivityTracker::default();
        t.on_open("a", 100);
        t.begin_turn("a", 120);
        let a = t.get("a").unwrap();
        assert!(a.in_turn);
        assert_eq!(a.last_activity_at, 120);
        t.end_turn("a", 130);
        let a = t.get("a").unwrap();
        assert!(!a.in_turn);
        assert_eq!(a.last_activity_at, 130);
    }

    #[test]
    fn on_restart_keeps_opened_at_and_clears_in_turn() {
        let t = ActivityTracker::default();
        t.on_open("a", 100);
        t.begin_turn("a", 120);
        t.on_restart("a", 300);
        assert_eq!(
            t.get("a"),
            Some(AppActivity {
                opened_at: 100,
                last_activity_at: 300,
                in_turn: false
            })
        );
    }

    #[test]
    fn dormant_cleared_by_open() {
        let t = ActivityTracker::default();
        t.mark_dormant("a");
        assert_eq!(t.dormant(), vec!["a".to_string()]);
        t.on_open("a", 1);
        assert!(t.dormant().is_empty());
    }

    fn base_act() -> AppActivity {
        AppActivity {
            opened_at: 0,
            last_activity_at: 0,
            in_turn: false,
        }
    }

    #[test]
    fn verdict_matrix() {
        let pol = IdlePolicy::default();
        let v = |id: &str, act: &AppActivity, p: &IdlePolicy, now: i64, pend: bool, bg: bool| {
            idle_verdict(id, act, p, now, pend, bg)
        };
        let a = base_act();
        assert_eq!(
            v("x", &a, &pol, 1000, false, false),
            Verdict::Recycle { idle_secs: 1000 }
        );
        assert_eq!(v("x", &a, &pol, 800, false, false), Verdict::Keep(None));
        assert_eq!(
            v("x", &a, &pol, DEFAULT_IDLE_TIMEOUT_SECS, false, false),
            Verdict::Recycle { idle_secs: 900 },
            "恰好等于阈值即回收"
        );
        assert_eq!(
            v(crate::maker::MAKER_APP_ID, &a, &pol, 1000, false, false),
            Verdict::Keep(Some(Exempt::Maker))
        );
        let off = IdlePolicy {
            enabled: false,
            ..IdlePolicy::default()
        };
        assert_eq!(
            v("x", &a, &off, 1000, false, false),
            Verdict::Keep(Some(Exempt::PolicyOff))
        );
        let ex = IdlePolicy {
            exempt_apps: ["x".to_string()].into_iter().collect(),
            ..IdlePolicy::default()
        };
        assert_eq!(
            v("x", &a, &ex, 1000, false, false),
            Verdict::Keep(Some(Exempt::UserExempt))
        );
        let turn = AppActivity {
            in_turn: true,
            ..base_act()
        };
        assert_eq!(
            v("x", &turn, &pol, 1000, false, false),
            Verdict::Keep(Some(Exempt::InTurn))
        );
        assert_eq!(
            v("x", &a, &pol, 1000, true, false),
            Verdict::Keep(Some(Exempt::PendingApproval))
        );
        assert_eq!(
            v("x", &a, &pol, 1000, false, true),
            Verdict::Keep(Some(Exempt::BackgroundSession))
        );
    }

    #[test]
    fn verdict_order_combinations() {
        let pol = IdlePolicy::default();
        let turn = AppActivity {
            in_turn: true,
            ..base_act()
        };
        assert_eq!(
            idle_verdict(crate::maker::MAKER_APP_ID, &turn, &pol, 1000, false, false),
            Verdict::Keep(Some(Exempt::Maker))
        );
        let both = IdlePolicy {
            enabled: false,
            exempt_apps: ["x".to_string()].into_iter().collect(),
            ..IdlePolicy::default()
        };
        assert_eq!(
            idle_verdict("x", &base_act(), &both, 1000, false, false),
            Verdict::Keep(Some(Exempt::PolicyOff))
        );
        assert_eq!(
            idle_verdict("x", &turn, &pol, 1000, true, false),
            Verdict::Keep(Some(Exempt::InTurn))
        );
        assert_eq!(
            idle_verdict("x", &base_act(), &pol, 1000, true, true),
            Verdict::Keep(Some(Exempt::PendingApproval))
        );
    }

    #[test]
    fn background_ids_include_sessions_without_pid() {
        use crate::session_mgr::HeadlessSession;
        let ids = background_app_ids(&[
            HeadlessSession {
                app_id: "a".into(),
                pid: None,
                started_at: 0,
            },
            HeadlessSession {
                app_id: "b".into(),
                pid: Some(7),
                started_at: 0,
            },
        ]);
        assert!(ids.contains("a") && ids.contains("b") && ids.len() == 2);
    }

    #[test]
    fn policy_store_defaults_on_missing_or_corrupt() {
        let tmp = tempfile::tempdir().unwrap();
        let layout = DataLayout::new(tmp.path().to_path_buf());
        let store = IdlePolicyStore::new(&layout);
        assert_eq!(store.load(), IdlePolicy::default());
        std::fs::write(layout.idle_policy_path(), "{oops").unwrap();
        assert_eq!(store.load(), IdlePolicy::default());
    }

    #[test]
    fn policy_store_roundtrip_and_rejects_out_of_range() {
        let tmp = tempfile::tempdir().unwrap();
        let layout = DataLayout::new(tmp.path().to_path_buf());
        let store = IdlePolicyStore::new(&layout);
        let p = IdlePolicy {
            timeout_secs: 600,
            ..IdlePolicy::default()
        };
        store.save(&p).unwrap();
        assert_eq!(store.load(), p);
        let before = std::fs::read(layout.idle_policy_path()).unwrap();
        for bad in [10, 100_000] {
            let q = IdlePolicy {
                timeout_secs: bad,
                ..IdlePolicy::default()
            };
            assert!(store.save(&q).is_err());
            assert_eq!(std::fs::read(layout.idle_policy_path()).unwrap(), before);
        }
    }

    #[test]
    fn still_idle_rejects_recent_touch_and_in_turn() {
        let t = ActivityTracker::default();
        assert!(!t.still_idle("a", 2000, 900), "无条目不回收");
        t.on_open("a", 0);
        assert!(t.still_idle("a", 900, 900));
        t.touch("a", 890);
        assert!(!t.still_idle("a", 960, 900));
        t.touch("a", 0);
        t.begin_turn("a", 0);
        assert!(!t.still_idle("a", 5000, 900));
    }

    #[test]
    fn forget_clears_live_and_dormant() {
        let t = ActivityTracker::default();
        t.on_open("a", 0);
        t.mark_dormant("a");
        t.forget("a");
        assert_eq!(t.get("a"), None);
        assert!(t.dormant().is_empty());
    }
}
