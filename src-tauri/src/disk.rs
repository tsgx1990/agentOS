//! 磁盘占用报告与缓存清理（P6-F）。
//!
//! 威胁模型：`sessions/<id>` 与 `maker-staging/<draft>` 在沙盒里对应用可写，应用可以在里面放
//! 符号链接、把子目录换成指向别处的链接。本模块在宿主（沙盒外）遍历与删除，所以一律
//! **不跟随符号链接**：目录只通过 `dirfd::DirHandle`（`O_DIRECTORY | O_NOFOLLOW` 的相对句柄）
//! 进入，目录项的元数据用 `AT_SYMLINK_NOFOLLOW` 读，删除链接只删链接本身。

use crate::dirfd::{identity_of_real_dir, DirHandle, EntryKind};
use crate::paths::DataLayout;
use std::collections::HashSet;
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

const MAX_DEPTH: usize = 64;

/// 不跟随链接地打开 `path` 为目录句柄；根本身是链接 / 非目录 → Err。
fn open_real_dir(path: &Path) -> Result<DirHandle, String> {
    let id = identity_of_real_dir(path)?;
    DirHandle::open_expecting(path, id)
}

fn tree_size(h: &DirHandle, depth: usize) -> u64 {
    let Ok(entries) = h.entries() else { return 0 };
    let mut total = 0u64;
    for e in entries {
        match e.kind {
            EntryKind::File => total = total.saturating_add(e.size),
            EntryKind::Dir if depth < MAX_DEPTH => {
                if let Ok(sub) = h.open_subdir(&e.name) {
                    total = total.saturating_add(tree_size(&sub, depth + 1));
                }
            }
            // 符号链接与其它类型不计入：不跟随，也不把目标的大小算进来。
            _ => {}
        }
    }
    total
}

/// 递归求和；不跟随符号链接（目录只经 `O_NOFOLLOW` 句柄进入）；路径不存在 → 0；单项读错误跳过。
pub fn dir_size(path: &Path) -> u64 {
    match std::fs::symlink_metadata(path) {
        Ok(m) if m.file_type().is_file() => m.len(),
        Ok(m) if m.file_type().is_dir() => match open_real_dir(path) {
            Ok(h) => tree_size(&h, 0),
            Err(_) => 0,
        },
        _ => 0,
    }
}

pub fn disk_report(layout: &DataLayout, app_ids: &[String]) -> DiskReport {
    disk_report_with_threshold(layout, app_ids, SESSIONS_WARN_BYTES)
}

pub fn disk_report_with_threshold(
    layout: &DataLayout,
    app_ids: &[String],
    threshold: u64,
) -> DiskReport {
    let mut apps: Vec<AppDisk> = app_ids
        .iter()
        .map(|id| {
            let sessions_bytes = dir_size(&layout.session_dir(id));
            AppDisk {
                app_id: id.clone(),
                sessions_bytes,
                data_bytes: dir_size(&layout.app_data_dir(id)),
                agent_home_bytes: dir_size(&layout.agent_home_dir(id)),
                sessions_over_threshold: sessions_bytes > threshold,
            }
        })
        .collect();
    apps.sort_by(|a, b| {
        (b.sessions_bytes.saturating_add(b.data_bytes))
            .cmp(&a.sessions_bytes.saturating_add(a.data_bytes))
            .then_with(|| a.app_id.cmp(&b.app_id))
    });
    DiskReport {
        root_bytes: dir_size(&layout.root_dir()),
        audit_bytes: dir_size(&layout.audit_dir()),
        notifications_bytes: dir_size(&layout.notifications_dir()),
        maker_staging_bytes: dir_size(&layout.maker_staging_root()),
        main_sessions_bytes: dir_size(&layout.session_dir("main")),
        apps,
        threshold_bytes: threshold,
    }
}

#[derive(Default)]
struct Tally {
    freed: u64,
    files: usize,
}

/// 删掉句柄目录下的东西。`keep` 是顶层要保留的那个文件名。
/// 链接只删链接本身；子目录经 `O_NOFOLLOW` 句柄进入（打不开就整项跳过），清空后顺手删掉。
fn clear_tree(h: &DirHandle, keep: Option<&str>, tally: &mut Tally, depth: usize) {
    let Ok(entries) = h.entries() else { return };
    for e in entries {
        if keep == Some(e.name.as_str()) && e.kind == EntryKind::File {
            continue;
        }
        match e.kind {
            EntryKind::Dir => {
                if depth >= MAX_DEPTH {
                    continue;
                }
                if let Ok(sub) = h.open_subdir(&e.name) {
                    clear_tree(&sub, None, tally, depth + 1);
                    let _ = h.remove_empty_dir(&e.name);
                }
            }
            EntryKind::File => {
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

fn valid_target(id: &str) -> bool {
    !(id.is_empty() || id == "." || id == ".." || id.contains(['/', '\\', '\0']))
}

/// 清缓存。
/// target=None：清 `<root>/sessions/` 下每个子目录（含 main）+ maker-staging；
/// target=Some(id)：只清 sessions/<id>，不动草稿。id 含 '/'、'\\'、".." 或为空 → Err。
/// running 里的目录保留 mtime 最新的那个顶层文件，其余普通文件删除，空子目录顺手删；
/// 不在 running 里的整目录内容全删（目录本身留着）。
/// 草稿：maker-staging/<draft> 不在 pending_drafts 里、且 mtime 早于 now - STAGING_MIN_AGE 才删。
/// 只遍历 sessions/ 与 maker-staging/ 两处。
///
/// 这两处的子目录对应用可写：全程不跟随符号链接（见模块说明）。某个 sessions/<id> 本身是链接
/// 或被换成别的东西 → 全量清理时跳过并记入 `refused`，指定目标时直接 Err。
pub fn clear_caches(
    layout: &DataLayout,
    target: Option<&str>,
    running: &HashSet<String>,
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
            let keep = if running.contains(id) {
                h.entries()
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|e| e.kind == EntryKind::File)
                    .max_by_key(|e| e.mtime)
                    .map(|e| e.name)
            } else {
                None
            };
            if running.contains(id) {
                report.kept_active.push(id.to_string());
            }
            clear_tree(&h, keep.as_deref(), tally, 0);
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
                    let mut ids: Vec<String> = root_h
                        .entries()
                        .unwrap_or_default()
                        .into_iter()
                        .map(|e| e.name)
                        .collect();
                    ids.sort();
                    for id in ids {
                        if clear_session(&id, &mut tally, &mut report).is_err() {
                            report.refused.push(format!("sessions/{id}"));
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

fn is_pending(staging_root: &Path, name: &str, pending: &[PathBuf]) -> bool {
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
        if is_pending(&root, &e.name, pending) || e.mtime > cutoff {
            continue;
        }
        match e.kind {
            EntryKind::Dir => {
                let Ok(sub) = h.open_subdir(&e.name) else {
                    report.refused.push(format!("maker-staging/{}", e.name));
                    continue;
                };
                clear_tree(&sub, None, tally, 0);
                if h.remove_empty_dir(&e.name).is_ok() {
                    report.removed_drafts += 1;
                }
            }
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

    fn no_run() -> HashSet<String> {
        HashSet::new()
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
    fn clear_caches_keeps_newest_file_of_running_sessions() {
        let (td, l) = layout();
        let a = td.path().join("sessions/a");
        write(&a.join("f1"), 100);
        write(&a.join("f2"), 40);
        set_age(&a.join("f1"), Duration::from_secs(3600));
        write(&td.path().join("sessions/b/g1"), 5);
        write(&td.path().join("sessions/b/g2"), 6);
        let running: HashSet<String> = ["a".to_string()].into();
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
        for running in [no_run(), ["a".to_string()].into()] {
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
