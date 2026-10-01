//! P6-F：应用会话的活动跟踪与（后续任务追加的）空闲回收。

use std::collections::{HashMap, HashSet};

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
}
