//! P6-F：按需采样进程树资源（RSS / CPU%）并按「根 pid」归属到宿主 / 主助手 / 应用 / MCP。
//!
//! 只在资源面板打开、前端轮询 `resource_report` 时才采样，不起常驻后台循环。
//! 归属逻辑全是纯函数（`descendants` / `aggregate`），便于不碰真实进程就能测。

use crate::idle::AppActivity;
use std::collections::{HashMap, HashSet};

#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct ProcSample {
    pub pid: u32,
    pub ppid: Option<u32>,
    pub name: String,
    pub rss_bytes: u64,
    pub cpu_percent: f32,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct GroupUsage {
    pub label: String,
    pub root_pids: Vec<u32>,
    pub proc_count: usize,
    pub rss_bytes: u64,
    pub cpu_percent: f32,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct AppUsage {
    pub app_id: String,
    /// 前台会话子树 + 名下全部后台会话子树。
    pub usage: GroupUsage,
    /// 定时任务 / 被调方会话个数。
    pub background_sessions: usize,
    pub opened_at: Option<i64>,
    pub last_activity_at: Option<i64>,
    pub idle_secs: Option<i64>,
    pub in_turn: bool,
    /// 不会被回收的原因（中文短语）；T3 填，本任务恒为 None。
    pub exempt: Option<String>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct ResourceReport {
    pub sampled_at: i64,
    /// 第一次采样没有差值基准，CPU 无意义，前端显示「—」。
    pub cpu_ready: bool,
    /// 只含宿主进程本身。
    pub host: GroupUsage,
    /// 主助手会话子树。
    pub main: Option<GroupUsage>,
    /// 按 rss_bytes 降序。
    pub apps: Vec<AppUsage>,
    /// label = server id。
    pub mcp_servers: Vec<GroupUsage>,
    /// 宿主其余子孙（Maker 预览会话等）。
    pub other: GroupUsage,
    pub total_rss_bytes: u64,
    pub total_cpu_percent: f32,
    pub total_proc_count: usize,
}

pub struct AppRoots {
    pub app_id: String,
    pub fg_pid: Option<u32>,
    pub bg_pids: Vec<u32>,
    pub activity: Option<AppActivity>,
}

/// 含 root 自身；root 不在表里返回空；用 visited 集合防环（pid 复用可能造出环）。
pub fn descendants(table: &[(u32, Option<u32>)], root: u32) -> Vec<u32> {
    if !table.iter().any(|(p, _)| *p == root) {
        return Vec::new();
    }
    let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
    for (pid, ppid) in table {
        if let Some(pp) = ppid {
            children.entry(*pp).or_default().push(*pid);
        }
    }
    let mut visited: HashSet<u32> = HashSet::new();
    let mut out = Vec::new();
    let mut stack = vec![root];
    while let Some(p) = stack.pop() {
        if !visited.insert(p) {
            continue;
        }
        out.push(p);
        if let Some(kids) = children.get(&p) {
            stack.extend(kids.iter().copied());
        }
    }
    out
}

/// 归属规则：按 main -> apps（按 app_id 排序）-> mcp 的顺序认领各自根的子树，
/// 每个 pid 只归属一次（先到先得）；host 组只含宿主 pid；other = 宿主子孙 - 宿主 - 已认领；
/// total = 宿主整棵子树。根 pid 不在表里（进程刚退出）时该组 proc_count=0，应用行照常列出。
pub fn aggregate(
    table: &[ProcSample],
    cpu_ready: bool,
    host_pid: u32,
    main_pid: Option<u32>,
    apps: &[AppRoots],
    mcp: &[(String, u32)],
    now: i64,
) -> ResourceReport {
    let pairs: Vec<(u32, Option<u32>)> = table.iter().map(|p| (p.pid, p.ppid)).collect();
    let by_pid: HashMap<u32, &ProcSample> = table.iter().map(|p| (p.pid, p)).collect();
    let mut claimed: HashSet<u32> = HashSet::new();

    // 认领各根的子树（已被先到者认领的 pid 跳过），汇总成一组。
    let group = |label: &str, roots: &[u32], claimed: &mut HashSet<u32>| -> GroupUsage {
        let mut rss = 0u64;
        let mut cpu = 0f32;
        let mut n = 0usize;
        for r in roots {
            for pid in descendants(&pairs, *r) {
                if !claimed.insert(pid) {
                    continue;
                }
                if let Some(p) = by_pid.get(&pid) {
                    rss += p.rss_bytes;
                    cpu += p.cpu_percent;
                    n += 1;
                }
            }
        }
        GroupUsage {
            label: label.to_string(),
            root_pids: roots.to_vec(),
            proc_count: n,
            rss_bytes: rss,
            cpu_percent: cpu,
        }
    };

    // host 只含宿主进程本身。
    let mut host = GroupUsage {
        label: "host".to_string(),
        root_pids: vec![host_pid],
        proc_count: 0,
        rss_bytes: 0,
        cpu_percent: 0.0,
    };
    if let Some(p) = by_pid.get(&host_pid) {
        host.proc_count = 1;
        host.rss_bytes = p.rss_bytes;
        host.cpu_percent = p.cpu_percent;
        claimed.insert(host_pid);
    }

    let main = main_pid.map(|m| group("main", &[m], &mut claimed));

    let mut sorted: Vec<&AppRoots> = apps.iter().collect();
    sorted.sort_by(|a, b| a.app_id.cmp(&b.app_id));
    let mut app_rows: Vec<AppUsage> = sorted
        .into_iter()
        .map(|a| {
            let mut roots: Vec<u32> = a.fg_pid.into_iter().collect();
            roots.extend(a.bg_pids.iter().copied());
            let usage = group(&a.app_id, &roots, &mut claimed);
            AppUsage {
                app_id: a.app_id.clone(),
                usage,
                background_sessions: a.bg_pids.len(),
                opened_at: a.activity.as_ref().map(|x| x.opened_at),
                last_activity_at: a.activity.as_ref().map(|x| x.last_activity_at),
                idle_secs: a.activity.as_ref().map(|x| now - x.last_activity_at),
                in_turn: a.activity.as_ref().is_some_and(|x| x.in_turn),
                exempt: None,
            }
        })
        .collect();
    app_rows.sort_by_key(|a| std::cmp::Reverse(a.usage.rss_bytes));

    let mcp_servers: Vec<GroupUsage> = mcp
        .iter()
        .map(|(id, pid)| group(id, &[*pid], &mut claimed))
        .collect();

    // other = 宿主子孙 - 已认领（宿主本身已认领）；total = 宿主整棵子树。
    let tree = descendants(&pairs, host_pid);
    let mut other = GroupUsage {
        label: "other".to_string(),
        root_pids: Vec::new(),
        proc_count: 0,
        rss_bytes: 0,
        cpu_percent: 0.0,
    };
    let (mut total_rss, mut total_cpu) = (0u64, 0f32);
    for pid in &tree {
        let Some(p) = by_pid.get(pid) else { continue };
        total_rss += p.rss_bytes;
        total_cpu += p.cpu_percent;
        if claimed.insert(*pid) {
            other.root_pids.push(*pid);
            other.proc_count += 1;
            other.rss_bytes += p.rss_bytes;
            other.cpu_percent += p.cpu_percent;
        }
    }

    ResourceReport {
        sampled_at: now,
        cpu_ready,
        host,
        main,
        apps: app_rows,
        mcp_servers,
        other,
        total_rss_bytes: total_rss,
        total_cpu_percent: total_cpu,
        total_proc_count: tree.len(),
    }
}

pub struct ResourceSampler {
    sys: sysinfo::System,
    refreshed_once: bool,
}

impl ResourceSampler {
    /// 空表，构造很便宜。
    pub fn new() -> Self {
        Self {
            sys: sysinfo::System::new(),
            refreshed_once: false,
        }
    }

    /// 刷新全表并转成 `ProcSample`；返回 (表, cpu_ready)，
    /// cpu_ready = 本次之前已刷新过至少一次（CPU% 需要两次刷新之间的差值）。
    pub fn sample(&mut self) -> (Vec<ProcSample>, bool) {
        use sysinfo::{ProcessRefreshKind, ProcessesToUpdate};
        self.sys.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            ProcessRefreshKind::nothing().with_memory().with_cpu(),
        );
        let ready = self.refreshed_once;
        self.refreshed_once = true;
        let table = self
            .sys
            .processes()
            .iter()
            .map(|(pid, p)| ProcSample {
                pid: pid.as_u32(),
                ppid: p.parent().map(|x| x.as_u32()),
                name: p.name().to_string_lossy().into_owned(),
                rss_bytes: p.memory(),
                cpu_percent: p.cpu_usage(),
            })
            .collect();
        (table, ready)
    }
}

impl Default for ResourceSampler {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ps(pid: u32, ppid: Option<u32>, rss: u64) -> ProcSample {
        ProcSample {
            pid,
            ppid,
            name: format!("p{pid}"),
            rss_bytes: rss,
            cpu_percent: 1.0,
        }
    }

    #[test]
    fn descendants_collects_subtree_only() {
        let t = [
            (1, None),
            (10, Some(1)),
            (11, Some(10)),
            (12, Some(10)),
            (20, Some(1)),
            (21, Some(20)),
        ];
        let mut d = descendants(&t, 10);
        d.sort();
        assert_eq!(d, vec![10, 11, 12]);
    }

    #[test]
    fn descendants_survives_cycle_and_missing_root() {
        let t = [(5, Some(6)), (6, Some(5))];
        let mut d = descendants(&t, 5);
        d.sort();
        assert_eq!(d, vec![5, 6]);
        assert!(descendants(&t, 99).is_empty());
    }

    fn fixture() -> Vec<ProcSample> {
        vec![
            ps(1, None, 100),
            ps(10, Some(1), 10),
            ps(11, Some(10), 11),
            ps(20, Some(1), 20),
            ps(21, Some(20), 21),
            ps(22, Some(21), 22),
            ps(30, Some(1), 30),
            ps(40, Some(1), 40),
            ps(50, Some(1), 50),
        ]
    }

    fn app_a(activity: Option<AppActivity>) -> AppRoots {
        AppRoots {
            app_id: "a".into(),
            fg_pid: Some(20),
            bg_pids: vec![30],
            activity,
        }
    }

    #[test]
    fn aggregate_attributes_each_pid_once() {
        let r = aggregate(
            &fixture(),
            true,
            1,
            Some(10),
            &[app_a(None)],
            &[("fs".to_string(), 40)],
            1000,
        );
        let a = &r.apps[0];
        assert_eq!(a.usage.proc_count, 4);
        assert_eq!(a.usage.rss_bytes, 20 + 21 + 22 + 30);
        assert_eq!(a.background_sessions, 1);
        assert_eq!(r.mcp_servers[0].label, "fs");
        assert_eq!(r.mcp_servers[0].proc_count, 1);
        assert_eq!(r.other.proc_count, 1);
        assert_eq!(r.other.rss_bytes, 50);
        assert_eq!(r.host.proc_count, 1);
        assert_eq!(r.main.as_ref().unwrap().proc_count, 2);
        assert_eq!(r.total_proc_count, 9);
        assert_eq!(
            r.total_rss_bytes,
            100 + 10 + 11 + 20 + 21 + 22 + 30 + 40 + 50
        );
    }

    #[test]
    fn aggregate_lists_app_with_dead_root_as_zero() {
        let dead = AppRoots {
            app_id: "z".into(),
            fg_pid: Some(999),
            bg_pids: vec![],
            activity: None,
        };
        let r = aggregate(&fixture(), true, 1, None, &[dead], &[], 1000);
        assert_eq!(r.apps.len(), 1);
        assert_eq!(r.apps[0].usage.proc_count, 0);
    }

    #[test]
    fn aggregate_idle_secs_from_activity() {
        let act = AppActivity {
            opened_at: 50,
            last_activity_at: 100,
            in_turn: false,
        };
        let r = aggregate(&fixture(), true, 1, None, &[app_a(Some(act))], &[], 400);
        assert_eq!(r.apps[0].idle_secs, Some(300));
        assert_eq!(r.apps[0].opened_at, Some(50));
        let r2 = aggregate(&fixture(), true, 1, None, &[app_a(None)], &[], 400);
        assert_eq!(r2.apps[0].idle_secs, None);
    }

    #[cfg(unix)]
    #[test]
    fn sampler_sees_real_child_process() {
        let mut child = std::process::Command::new("sleep")
            .arg("5")
            .spawn()
            .unwrap();
        let pid = child.id();
        let mut s = ResourceSampler::new();
        let (table, _) = s.sample();
        let me = std::process::id();
        let row = table.iter().find(|p| p.pid == pid).expect("子进程在表里");
        assert_eq!(row.ppid, Some(me));
        assert!(row.rss_bytes > 0);
        let pairs: Vec<(u32, Option<u32>)> = table.iter().map(|p| (p.pid, p.ppid)).collect();
        assert!(descendants(&pairs, me).contains(&pid));
        let _ = child.kill();
        let _ = child.wait();
    }

    #[test]
    fn cpu_ready_only_from_second_sample() {
        let mut s = ResourceSampler::new();
        assert!(!s.sample().1);
        assert!(s.sample().1);
    }
}
