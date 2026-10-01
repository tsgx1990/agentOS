//! 磁盘占用报告与缓存清理（P6-F）。
//!
//! 威胁模型：`sessions/<id>` 与 `maker-staging/<draft>` 在沙盒里对应用可写，应用可以在里面放
//! 符号链接、把子目录换成指向别处的链接。本模块在宿主（沙盒外）遍历与删除，所以一律
//! **不跟随符号链接**：目录只通过 `dirfd::DirHandle`（`O_DIRECTORY | O_NOFOLLOW` 的相对句柄）
//! 进入，目录项的元数据用 `AT_SYMLINK_NOFOLLOW` 读，删除链接只删链接本身。

use crate::dirfd::{identity_of_real_dir, DirHandle, EntryInfo, EntryKind};
use crate::paths::DataLayout;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub const SESSIONS_WARN_BYTES: u64 = 200 * 1024 * 1024;
pub const STAGING_MIN_AGE: std::time::Duration = std::time::Duration::from_secs(24 * 3600);

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct AppDisk {
    pub app_id: String,
    pub sessions_bytes: u64,
    pub data_bytes: u64,
    pub agent_home_bytes: u64,
    pub sessions_over_threshold: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct DiskReport {
    pub root_bytes: u64,
    pub audit_bytes: u64,
    pub notifications_bytes: u64,
    pub maker_staging_bytes: u64,
    pub main_sessions_bytes: u64,
    /// 有目录层级过深而没统计到（数值偏小）。
    pub incomplete: bool,
    pub apps: Vec<AppDisk>,
    pub threshold_bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct ClearReport {
    pub freed_bytes: u64,
    pub removed_files: usize,
    pub removed_drafts: usize,
    pub kept_active: Vec<String>,
    /// 因目录本身是符号链接 / 被换链而拒绝处理的目录（相对数据根的名字），一律没有动它。
    pub refused: Vec<String>,
}

/// 目录层级上限（每层占一个文件描述符，也防止被构造的深目录拖垮栈）。
/// 超过的部分：`dir_size` 标记「未完整统计」，清理时计入 `refused`，不静默略过。
const MAX_DEPTH: usize = 64;

/// 运行中会话的启动时刻留的余量：会话文件的 mtime 可能略早于我们记下的启动时刻。
pub const SESSION_START_MARGIN: std::time::Duration = std::time::Duration::from_secs(10);

/// 运行中的应用 → 其最早一个运行中会话的启动时刻。`None` 表示拿不到启动时刻，
/// 该目录一个文件都不删（保守）。
pub type RunningSessions = HashMap<String, Option<std::time::SystemTime>>;

/// 不跟随链接地打开 `path` 为目录句柄；根本身是链接 / 非目录 → Err。
fn open_real_dir(path: &Path) -> Result<DirHandle, String> {
    let id = identity_of_real_dir(path)?;
    DirHandle::open_expecting(path, id)
}

/// 返回 (字节数, 是否完整统计)。
fn tree_size(h: &DirHandle, depth: usize) -> (u64, bool) {
    let Ok(entries) = h.entries() else {
        return (0, true);
    };
    let mut total = 0u64;
    let mut complete = true;
    for e in entries {
        match e.kind {
            EntryKind::File => total = total.saturating_add(e.size),
            EntryKind::Dir if depth >= MAX_DEPTH => complete = false,
            EntryKind::Dir => {
                if let Ok(sub) = h.open_subdir(&e.name) {
                    let (n, c) = tree_size(&sub, depth + 1);
                    total = total.saturating_add(n);
                    complete &= c;
                }
            }
            // 符号链接与其它类型不计入：不跟随，也不把目标的大小算进来。
            _ => {}
        }
    }
    (total, complete)
}

/// 同 `dir_size`，另返回「是否完整统计」（有目录超过层级上限而没算进去 → false）。
pub fn dir_size_checked(path: &Path) -> (u64, bool) {
    match std::fs::symlink_metadata(path) {
        Ok(m) if m.file_type().is_file() => (m.len(), true),
        Ok(m) if m.file_type().is_dir() => match open_real_dir(path) {
            Ok(h) => tree_size(&h, 0),
            Err(_) => (0, true),
        },
        _ => (0, true),
    }
}

/// 递归求和；不跟随符号链接（目录只经 `O_NOFOLLOW` 句柄进入）；路径不存在 → 0；单项读错误跳过。
/// 超过层级上限的子目录不计入这里的数值；要知道有没有漏算用 `dir_size_checked`
/// （`disk_report` 用它，漏算时把 `incomplete` 置位并把该应用的会话提示置为超阈值）。
pub fn dir_size(path: &Path) -> u64 {
    dir_size_checked(path).0
}

pub fn disk_report(layout: &DataLayout, app_ids: &[String]) -> DiskReport {
    disk_report_with_threshold(layout, app_ids, SESSIONS_WARN_BYTES)
}

pub fn disk_report_with_threshold(
    layout: &DataLayout,
    app_ids: &[String],
    threshold: u64,
) -> DiskReport {
    let mut incomplete = false;
    let mut size = |p: PathBuf| -> (u64, bool) {
        let (n, c) = dir_size_checked(&p);
        incomplete |= !c;
        (n, c)
    };
    let mut apps: Vec<AppDisk> = Vec::new();
    for id in app_ids {
        let (sessions_bytes, sessions_complete) = size(layout.session_dir(id));
        apps.push(AppDisk {
            app_id: id.clone(),
            sessions_bytes,
            data_bytes: size(layout.app_data_dir(id)).0,
            agent_home_bytes: size(layout.agent_home_dir(id)).0,
            // 层级过深导致没算全，按「超阈值」提示：不能让应用靠深目录躲过提示。
            sessions_over_threshold: sessions_bytes > threshold || !sessions_complete,
        });
    }
    apps.sort_by(|a, b| {
        (b.sessions_bytes.saturating_add(b.data_bytes))
            .cmp(&a.sessions_bytes.saturating_add(a.data_bytes))
            .then_with(|| a.app_id.cmp(&b.app_id))
    });
    let root_bytes = size(layout.root_dir()).0;
    let audit_bytes = size(layout.audit_dir()).0;
    let notifications_bytes = size(layout.notifications_dir()).0;
    let maker_staging_bytes = size(layout.maker_staging_root()).0;
    let main_sessions_bytes = size(layout.session_dir("main")).0;
    DiskReport {
        root_bytes,
        audit_bytes,
        notifications_bytes,
        maker_staging_bytes,
        main_sessions_bytes,
        apps,
        threshold_bytes: threshold,
        incomplete,
    }
}

#[derive(Default)]
struct Tally {
    freed: u64,
    files: usize,
    /// 遇到超过层级上限的子目录（没有处理）。
    too_deep: bool,
}

/// 删掉句柄目录下的东西（递归）。`keep(entry)` 为真的**普通文件**保留。
/// 链接只删链接本身；子目录经 `O_NOFOLLOW` 句柄进入（打不开就整项跳过），清空后顺手删掉。
fn clear_tree(h: &DirHandle, keep: &dyn Fn(&EntryInfo) -> bool, tally: &mut Tally, depth: usize) {
    let Ok(entries) = h.entries() else { return };
    for e in entries {
        match e.kind {
            EntryKind::Dir => {
                if depth >= MAX_DEPTH {
                    tally.too_deep = true;
                    continue;
                }
                if let Ok(sub) = h.open_subdir(&e.name) {
                    clear_tree(&sub, keep, tally, depth + 1);
                    let _ = h.remove_empty_dir(&e.name);
                }
            }
            EntryKind::File => {
                if keep(&e) {
                    continue;
                }
                if h.remove_file_if_exists(&e.name).is_ok() {
                    tally.freed = tally.freed.saturating_add(e.size);
                    tally.files += 1;
                }
            }
            // 符号链接 / 套接字等：只删目录项本身（unlinkat 对链接不跟随），不计字节。
            EntryKind::Symlink | EntryKind::Other => {
                let _ = h.remove_file_if_exists(&e.name);
            }
        }
    }
}

/// 整棵树里最新的 mtime（含目录自身、各级目录项；不跟随链接）。
/// 超过层级上限 → `too_deep` 置位（调用方按「不确定，不删」处理）。
fn tree_latest(
    h: &DirHandle,
    own: std::time::SystemTime,
    depth: usize,
    too_deep: &mut bool,
) -> std::time::SystemTime {
    let mut latest = own;
    for e in h.entries().unwrap_or_default() {
        latest = latest.max(e.mtime);
        if e.kind == EntryKind::Dir {
            if depth >= MAX_DEPTH {
                *too_deep = true;
            } else if let Ok(sub) = h.open_subdir(&e.name) {
                latest = latest.max(tree_latest(&sub, e.mtime, depth + 1, too_deep));
            }
        }
    }
    latest
}

fn valid_target(id: &str) -> bool {
    !(id.is_empty() || id == "." || id == ".." || id.contains(['/', '\\', '\0']))
}

/// 清缓存。
/// target=None：清 `<root>/sessions/` 下每个子目录（含 main）+ maker-staging；
/// target=Some(id)：只清 sessions/<id>，不动草稿。id 含 '/'、'\\'、".." 或为空 → Err。
///
/// `running`：运行中的应用 → 其**最早**一个运行中会话的启动时刻。同一应用的交互会话与后台会话
/// 共用 `sessions/<id>`，各自在写自己的会话文件，所以运行中的目录保留所有 mtime 不早于
/// 「最早启动时刻 - SESSION_START_MARGIN」的文件，只删更早的；启动时刻为 `None`（拿不到）→
/// 该目录一个文件都不删。不在 running 里的整目录内容全删（目录本身留着）。
/// 草稿：maker-staging/<draft> 不在 pending_drafts 里、且**整棵树**最大 mtime 早于
/// now - STAGING_MIN_AGE 才删（`stage_write` 改嵌套文件不更新草稿根目录的 mtime）。
/// 只遍历 sessions/ 与 maker-staging/ 两处。
///
/// 这两处的子目录对应用可写：全程不跟随符号链接（见模块说明）。某个 sessions/<id> 本身是链接
/// 或被换成别的东西 → 全量清理时跳过并记入 `refused`，指定目标时直接 Err。
/// 目录层级超过上限的部分不处理，同样记入 `refused`。
pub fn clear_caches(
    layout: &DataLayout,
    target: Option<&str>,
    running: &RunningSessions,
    pending_drafts: &[PathBuf],
    now: std::time::SystemTime,
) -> Result<ClearReport, String> {
    if let Some(id) = target {
        if !valid_target(id) {
            return Err(format!("非法的目标：{id:?}"));
        }
    }
    let mut report = ClearReport {
        freed_bytes: 0,
        removed_files: 0,
        removed_drafts: 0,
        kept_active: Vec::new(),
        refused: Vec::new(),
    };
    let mut tally = Tally::default();
    let sessions_root = layout.root_dir().join("sessions");

    let clear_session =
        |id: &str, tally: &mut Tally, report: &mut ClearReport| -> Result<(), String> {
            let dir = sessions_root.join(id);
            let h = open_real_dir(&dir)?;
            match running.get(id) {
                None => clear_tree(&h, &|_| false, tally, 0),
                Some(None) => report.kept_active.push(id.to_string()),
                Some(Some(start)) => {
                    report.kept_active.push(id.to_string());
                    let cutoff = start
                        .checked_sub(SESSION_START_MARGIN)
                        .unwrap_or(std::time::UNIX_EPOCH);
                    clear_tree(&h, &|e| e.mtime >= cutoff, tally, 0);
                }
            }
            if std::mem::take(&mut tally.too_deep) {
                report
                    .refused
                    .push(format!("sessions/{id}（目录层级过深，部分内容未处理）"));
            }
            Ok(())
        };

    match target {
        Some(id) => match std::fs::symlink_metadata(sessions_root.join(id)) {
            Ok(_) => clear_session(id, &mut tally, &mut report)
                .map_err(|e| format!("拒绝处理 sessions/{id}：{e}"))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.to_string()),
        },
        None => {
            // sessions/ 本身由宿主控制；仍然经句柄枚举。
            match open_real_dir(&sessions_root) {
                Ok(root_h) => {
                    let mut ids: Vec<std::ffi::OsString> = root_h
                        .entries()
                        .unwrap_or_default()
                        .into_iter()
                        .map(|e| e.name)
                        .collect();
                    ids.sort();
                    for id in ids {
                        // 非 UTF-8 的目录名不可能是合法应用 id，也不在 running 里：按普通目录清。
                        let label = id.to_string_lossy().into_owned();
                        let Some(id_str) = id.to_str() else {
                            if let Ok(sub) = root_h.open_subdir(&id) {
                                clear_tree(&sub, &|_| false, &mut tally, 0);
                            } else {
                                report.refused.push(format!("sessions/{label}"));
                            }
                            continue;
                        };
                        if clear_session(id_str, &mut tally, &mut report).is_err() {
                            report.refused.push(format!("sessions/{label}"));
                        }
                    }
                }
                Err(_) if std::fs::symlink_metadata(&sessions_root).is_err() => {}
                Err(_) => report.refused.push("sessions".to_string()),
            }
            clear_staging(layout, pending_drafts, now, &mut tally, &mut report);
        }
    }
    report.freed_bytes = tally.freed;
    report.removed_files = tally.files;
    Ok(report)
}

fn is_pending(staging_root: &Path, name: &std::ffi::OsStr, pending: &[PathBuf]) -> bool {
    let canon_root = std::fs::canonicalize(staging_root).ok();
    pending.iter().any(|p| {
        p.file_name().is_some_and(|n| n == name)
            && p.parent().is_some_and(|par| {
                par == staging_root || canon_root.as_deref().is_some_and(|c| par == c)
            })
    })
}

fn clear_staging(
    layout: &DataLayout,
    pending: &[PathBuf],
    now: std::time::SystemTime,
    tally: &mut Tally,
    report: &mut ClearReport,
) {
    let root = layout.maker_staging_root();
    let h = match open_real_dir(&root) {
        Ok(h) => h,
        Err(_) if std::fs::symlink_metadata(&root).is_err() => return,
        Err(_) => {
            report.refused.push("maker-staging".to_string());
            return;
        }
    };
    let cutoff = now
        .checked_sub(STAGING_MIN_AGE)
        .unwrap_or(std::time::UNIX_EPOCH);
    for e in h.entries().unwrap_or_default() {
        if is_pending(&root, &e.name, pending) {
            continue;
        }
        let label = format!("maker-staging/{}", e.name.to_string_lossy());
        match e.kind {
            EntryKind::Dir => {
                let Ok(sub) = h.open_subdir(&e.name) else {
                    report.refused.push(label);
                    continue;
                };
                let mut deep = false;
                let latest = tree_latest(&sub, e.mtime, 0, &mut deep);
                if deep {
                    report
                        .refused
                        .push(format!("{label}（目录层级过深，未处理）"));
                    continue;
                }
                if latest > cutoff {
                    continue;
                }
                clear_tree(&sub, &|_| false, tally, 0);
                if h.remove_empty_dir(&e.name).is_ok() {
                    report.removed_drafts += 1;
                }
            }
            _ if e.mtime > cutoff => {}
            EntryKind::File => {
                if h.remove_file_if_exists(&e.name).is_ok() {
                    tally.freed = tally.freed.saturating_add(e.size);
                    tally.files += 1;
                }
            }
            // 草稿位置上的符号链接：只删链接本身。
            EntryKind::Symlink | EntryKind::Other => {
                let _ = h.remove_file_if_exists(&e.name);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{Duration, SystemTime};

    fn write(p: &Path, n: usize) {
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, vec![b'x'; n]).unwrap();
    }

    fn layout() -> (tempfile::TempDir, DataLayout) {
        let td = tempfile::tempdir().unwrap();
        let l = DataLayout::new(td.path().to_path_buf());
        (td, l)
    }

    fn set_age(p: &Path, ago: Duration) {
        let f = fs::File::options().write(true).open(p).unwrap();
        f.set_modified(SystemTime::now() - ago).unwrap();
    }

    fn no_run() -> RunningSessions {
        HashMap::new()
    }

    fn run_since(id: &str, ago: Duration) -> RunningSessions {
        HashMap::from([(id.to_string(), Some(SystemTime::now() - ago))])
    }

    #[cfg(unix)]
    #[test]
    fn dir_size_sums_nested_and_ignores_symlinks() {
        let (td, _l) = layout();
        let d = td.path().join("d");
        write(&d.join("a"), 10);
        write(&d.join("sub/b"), 20);
        let victim = tempfile::tempdir().unwrap();
        write(&victim.path().join("big"), 1024 * 1024);
        std::os::unix::fs::symlink(victim.path().join("big"), d.join("link_file")).unwrap();
        // 链接子目录：目标里的内容也不能计入
        std::os::unix::fs::symlink(victim.path(), d.join("link_dir")).unwrap();
        assert_eq!(dir_size(&d), 30);
        assert_eq!(dir_size(&td.path().join("missing")), 0);
        // 根本身是链接 → 不跟随
        std::os::unix::fs::symlink(victim.path(), td.path().join("rootlink")).unwrap();
        assert_eq!(dir_size(&td.path().join("rootlink")), 0);
    }

    #[test]
    fn disk_report_flags_sessions_over_threshold() {
        let (td, l) = layout();
        write(&td.path().join("sessions/a/f"), 150);
        write(&td.path().join("sessions/b/f"), 50);
        write(&td.path().join("sessions/main/f"), 7);
        write(&td.path().join("apps/b/d"), 500);
        let r = disk_report_with_threshold(&l, &["a".into(), "b".into()], 100);
        assert_eq!(r.threshold_bytes, 100);
        assert_eq!(r.main_sessions_bytes, 7);
        let a = r.apps.iter().find(|x| x.app_id == "a").unwrap();
        let b = r.apps.iter().find(|x| x.app_id == "b").unwrap();
        assert!(a.sessions_over_threshold);
        assert!(!b.sessions_over_threshold);
        assert_eq!(b.data_bytes, 500);
        assert_eq!(r.apps[0].app_id, "b");
        assert!(r.root_bytes >= 150 + 50 + 7 + 500);
    }

    #[test]
    fn clear_caches_keeps_files_newer_than_running_start() {
        let (td, l) = layout();
        let a = td.path().join("sessions/a");
        write(&a.join("f1"), 100);
        write(&a.join("f2"), 40);
        set_age(&a.join("f1"), Duration::from_secs(3600));
        write(&td.path().join("sessions/b/g1"), 5);
        write(&td.path().join("sessions/b/g2"), 6);
        let running = run_since("a", Duration::from_secs(600));
        let r = clear_caches(&l, None, &running, &[], SystemTime::now()).unwrap();
        assert!(!a.join("f1").exists());
        assert!(a.join("f2").exists());
        assert!(!td.path().join("sessions/b/g1").exists());
        assert!(!td.path().join("sessions/b/g2").exists());
        assert!(td.path().join("sessions/b").is_dir());
        assert_eq!(r.freed_bytes, 100 + 5 + 6);
        assert_eq!(r.removed_files, 3);
        assert_eq!(r.kept_active, vec!["a".to_string()]);
        assert!(r.refused.is_empty());
    }

    #[test]
    fn clear_caches_never_touches_protected_dirs() {
        let (td, l) = layout();
        let files = [
            "apps/x/data.bin",
            "audit/2026-01-01.jsonl",
            "notifications/2026-01-01.jsonl",
            "registry.json",
            "packages/x/f",
            "agenthome/x/f",
            "state/x.json",
            "approvals/f",
            "skills/s/f",
            "skills-index.json",
        ];
        for (i, f) in files.iter().enumerate() {
            let p = td.path().join(f);
            write(&p, 3 + i);
        }
        write(&td.path().join("sessions/a/old"), 9);
        clear_caches(
            &l,
            None,
            &no_run(),
            &[],
            SystemTime::now() + Duration::from_secs(99 * 3600),
        )
        .unwrap();
        for (i, f) in files.iter().enumerate() {
            assert_eq!(
                fs::read(td.path().join(f)).unwrap(),
                vec![b'x'; 3 + i],
                "{f}"
            );
        }
        assert!(!td.path().join("sessions/a/old").exists());
    }

    #[test]
    fn clear_caches_drafts_respect_pending_and_age() {
        let (td, l) = layout();
        write(&td.path().join("maker-staging/orphan/f"), 8);
        write(&td.path().join("maker-staging/pending/f"), 8);
        let pending = vec![td.path().join("maker-staging/pending")];
        let now = SystemTime::now();
        let r = clear_caches(&l, None, &no_run(), &pending, now).unwrap();
        assert_eq!(r.removed_drafts, 0);
        assert!(td.path().join("maker-staging/orphan/f").exists());
        let r = clear_caches(
            &l,
            None,
            &no_run(),
            &pending,
            now + Duration::from_secs(48 * 3600),
        )
        .unwrap();
        assert_eq!(r.removed_drafts, 1);
        assert_eq!(r.freed_bytes, 8);
        assert!(!td.path().join("maker-staging/orphan").exists());
        assert!(td.path().join("maker-staging/pending/f").exists());
    }

    #[test]
    fn clear_caches_rejects_path_traversal_target() {
        let (td, l) = layout();
        write(&td.path().join("apps/keep"), 4);
        write(&td.path().join("sessions/a/f"), 4);
        for bad in ["../apps", "a/b", "", "..", "a\\b"] {
            assert!(
                clear_caches(&l, Some(bad), &no_run(), &[], SystemTime::now()).is_err(),
                "{bad:?}"
            );
        }
        assert!(td.path().join("apps/keep").exists());
        assert!(td.path().join("sessions/a/f").exists());
    }

    #[test]
    fn clear_caches_single_target_leaves_others() {
        let (td, l) = layout();
        write(&td.path().join("sessions/a/f"), 4);
        write(&td.path().join("sessions/b/f"), 4);
        write(&td.path().join("maker-staging/d/f"), 4);
        let r = clear_caches(
            &l,
            Some("a"),
            &no_run(),
            &[],
            SystemTime::now() + Duration::from_secs(99 * 3600),
        )
        .unwrap();
        assert_eq!(r.removed_files, 1);
        assert!(!td.path().join("sessions/a/f").exists());
        assert!(td.path().join("sessions/b/f").exists());
        assert!(td.path().join("maker-staging/d/f").exists());
    }

    #[test]
    fn clear_caches_keeps_both_live_files_of_two_sessions_same_app() {
        // 同一应用的交互会话与后台会话各有一个活文件，都晚于最早启动时刻 → 都保留。
        let (td, l) = layout();
        let a = td.path().join("sessions/a");
        write(&a.join("interactive"), 10);
        write(&a.join("background"), 20);
        write(&a.join("stale"), 30);
        set_age(&a.join("interactive"), Duration::from_secs(300));
        set_age(&a.join("background"), Duration::from_secs(5));
        set_age(&a.join("stale"), Duration::from_secs(7200));
        let running = run_since("a", Duration::from_secs(600));
        let r = clear_caches(&l, None, &running, &[], SystemTime::now()).unwrap();
        assert!(a.join("interactive").exists());
        assert!(a.join("background").exists());
        assert!(!a.join("stale").exists());
        assert_eq!(r.removed_files, 1);
        assert_eq!(r.freed_bytes, 30);
    }

    #[test]
    fn clear_caches_unknown_start_time_deletes_nothing() {
        let (td, l) = layout();
        let a = td.path().join("sessions/main");
        write(&a.join("old"), 10);
        set_age(&a.join("old"), Duration::from_secs(99999));
        let running = HashMap::from([("main".to_string(), None)]);
        let r = clear_caches(&l, None, &running, &[], SystemTime::now()).unwrap();
        assert!(a.join("old").exists());
        assert_eq!(r.removed_files, 0);
        assert_eq!(r.kept_active, vec!["main".to_string()]);
    }

    #[test]
    fn clear_caches_keeps_draft_with_recent_nested_file() {
        // 顶层目录很旧，但子目录里有新文件（stage_write 不更新草稿根目录 mtime）→ 不删。
        let (td, l) = layout();
        let d = td.path().join("maker-staging/d");
        write(&d.join("sub/new.txt"), 5);
        write(&d.join("old.txt"), 5);
        set_age(&d.join("old.txt"), Duration::from_secs(99 * 3600));
        // 目录自身 mtime 调旧
        let dd = fs::File::open(&d).unwrap();
        dd.set_modified(SystemTime::now() - Duration::from_secs(99 * 3600))
            .unwrap();
        let sd = fs::File::open(d.join("sub")).unwrap();
        sd.set_modified(SystemTime::now() - Duration::from_secs(99 * 3600))
            .unwrap();
        // 25 小时后看：new.txt 才 25 小时前……用 now=实际现在 + 1h，仍不满 24h
        let r = clear_caches(
            &l,
            None,
            &no_run(),
            &[],
            SystemTime::now() + Duration::from_secs(3600),
        )
        .unwrap();
        assert_eq!(r.removed_drafts, 0);
        assert!(d.join("sub/new.txt").exists());
    }

    fn deep_dir(base: &Path, levels: usize) -> PathBuf {
        let mut p = base.to_path_buf();
        for _ in 0..levels {
            p.push("d");
        }
        fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn deep_trees_are_reported_not_silently_skipped() {
        let (td, l) = layout();
        let deep = deep_dir(&td.path().join("sessions/a"), MAX_DEPTH + 6);
        write(&deep.join("big"), 1000);
        write(&td.path().join("sessions/a/top"), 10);
        // 统计：标记未完整，且该应用被提示为超阈值
        let (n, complete) = dir_size_checked(&td.path().join("sessions/a"));
        assert_eq!(n, 10);
        assert!(!complete);
        let r = disk_report_with_threshold(&l, &["a".into()], 1_000_000);
        assert!(r.incomplete);
        assert!(r.apps[0].sessions_over_threshold);
        // 清理：能处理的处理，超深的计入 refused
        let rep = clear_caches(&l, None, &no_run(), &[], SystemTime::now()).unwrap();
        assert!(!td.path().join("sessions/a/top").exists());
        assert!(rep.refused.iter().any(|x| x.starts_with("sessions/a")));
        assert!(deep.join("big").exists());
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_file_names_are_sized_and_cleared() {
        use std::os::unix::ffi::OsStrExt;
        let (td, l) = layout();
        let dir = td.path().join("sessions/a");
        fs::create_dir_all(&dir).unwrap();
        let name = std::ffi::OsStr::from_bytes(b"bad-\xff\xfe.jsonl");
        // 部分文件系统（如 APFS）拒绝非 UTF-8 文件名：此时无从构造，跳过。
        if fs::write(dir.join(name), vec![b'x'; 50]).is_err() {
            return;
        }
        assert_eq!(dir_size(&dir), 50);
        let r = clear_caches(&l, None, &no_run(), &[], SystemTime::now()).unwrap();
        assert_eq!(r.freed_bytes, 50);
        assert!(fs::symlink_metadata(dir.join(name)).is_err());
    }

    // ---- R2：应用可写目录里的符号链接 ----

    #[cfg(unix)]
    fn victim() -> (tempfile::TempDir, PathBuf) {
        let v = tempfile::tempdir().unwrap();
        let p = v.path().to_path_buf();
        write(&p.join("secret.txt"), 11);
        write(&p.join("nested/deep.txt"), 13);
        (v, p)
    }

    #[cfg(unix)]
    fn snapshot(p: &Path) -> Vec<(String, Vec<u8>)> {
        fn walk(base: &Path, p: &Path, out: &mut Vec<(String, Vec<u8>)>) {
            for e in fs::read_dir(p).unwrap().flatten() {
                let path = e.path();
                let rel = path.strip_prefix(base).unwrap().display().to_string();
                if path.is_dir() {
                    out.push((rel, vec![]));
                    walk(base, &path, out);
                } else {
                    out.push((rel, fs::read(&path).unwrap()));
                }
            }
        }
        let mut out = Vec::new();
        walk(p, p, &mut out);
        out.sort();
        out
    }

    #[cfg(unix)]
    #[test]
    fn clear_caches_removes_links_inside_session_without_following() {
        let (td, l) = layout();
        let (_v, vp) = victim();
        let before = snapshot(&vp);
        let s = td.path().join("sessions/a");
        write(&s.join("old"), 3);
        std::os::unix::fs::symlink(vp.join("secret.txt"), s.join("link_file")).unwrap();
        std::os::unix::fs::symlink(&vp, s.join("link_dir")).unwrap();
        // 运行中的应用（保留最新文件）与不在运行的都要验
        for running in [no_run(), run_since("a", Duration::from_secs(0))] {
            clear_caches(&l, None, &running, &[], SystemTime::now()).unwrap();
            assert_eq!(snapshot(&vp), before);
            assert!(
                fs::symlink_metadata(s.join("link_file")).is_err(),
                "文件链接应被删"
            );
            assert!(
                fs::symlink_metadata(s.join("link_dir")).is_err(),
                "目录链接应被删"
            );
            // 为第二轮重新布置
            write(&s.join("old"), 3);
            std::os::unix::fs::symlink(vp.join("secret.txt"), s.join("link_file")).unwrap();
            std::os::unix::fs::symlink(&vp, s.join("link_dir")).unwrap();
        }
        assert_eq!(snapshot(&vp), before);
    }

    #[cfg(unix)]
    #[test]
    fn clear_caches_refuses_session_dir_that_is_a_symlink() {
        let (td, l) = layout();
        let (_v, vp) = victim();
        let before = snapshot(&vp);
        fs::create_dir_all(td.path().join("sessions")).unwrap();
        std::os::unix::fs::symlink(&vp, td.path().join("sessions/a")).unwrap();
        write(&td.path().join("sessions/b/f"), 2);
        // 全量清理：跳过并体现在报告里，其它目录照常处理
        let r = clear_caches(&l, None, &no_run(), &[], SystemTime::now()).unwrap();
        assert_eq!(r.refused, vec!["sessions/a".to_string()]);
        assert_eq!(snapshot(&vp), before);
        assert!(!td.path().join("sessions/b/f").exists());
        // 指定目标：直接报错
        assert!(clear_caches(&l, Some("a"), &no_run(), &[], SystemTime::now()).is_err());
        assert_eq!(snapshot(&vp), before);
        // 链接本身没被当目录处理，也没被删
        assert!(fs::symlink_metadata(td.path().join("sessions/a"))
            .unwrap()
            .file_type()
            .is_symlink());
    }

    #[cfg(unix)]
    #[test]
    fn clear_caches_does_not_follow_swapped_staging_draft() {
        let (td, l) = layout();
        let (_v, vp) = victim();
        let before = snapshot(&vp);
        fs::create_dir_all(td.path().join("maker-staging")).unwrap();
        std::os::unix::fs::symlink(&vp, td.path().join("maker-staging/draft")).unwrap();
        let r = clear_caches(
            &l,
            None,
            &no_run(),
            &[],
            SystemTime::now() + Duration::from_secs(99 * 3600),
        )
        .unwrap();
        assert_eq!(snapshot(&vp), before);
        assert!(fs::symlink_metadata(td.path().join("maker-staging/draft")).is_err());
        assert_eq!(r.freed_bytes, 0);
    }

    #[cfg(unix)]
    #[test]
    fn clear_caches_refuses_staging_root_that_is_a_symlink() {
        let (td, l) = layout();
        let (_v, vp) = victim();
        let before = snapshot(&vp);
        std::os::unix::fs::symlink(&vp, td.path().join("maker-staging")).unwrap();
        let r = clear_caches(
            &l,
            None,
            &no_run(),
            &[],
            SystemTime::now() + Duration::from_secs(99 * 3600),
        )
        .unwrap();
        assert_eq!(r.refused, vec!["maker-staging".to_string()]);
        assert_eq!(snapshot(&vp), before);
    }
}
