// 安装事务：校验源包 → 复制到暂存区 → (可选) npm install --ignore-scripts →
// 原子 rename 落位 → 建应用数据区/状态文件 → 解析权限 → 写入 registry。
// 任一步失败即回滚（清理暂存与落位目录），registry 不留下失败条目。
use crate::paths::DataLayout;
use crate::permissions;
use crate::pkg::{self, Manifest};
use crate::registry::{InstalledApp, RegistryStore};
use std::path::Path;

/// 递归复制目录（供 T7 升级复用）。跳过符号链接：P1 阶段不复制，避免恶意包
/// 借软链接逃出目标目录访问宿主任意文件；L2 阶段再做更完整的加固。
pub(crate) fn copy_dir(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let ft = entry.file_type()?;
        let dst = to.join(entry.file_name());
        if ft.is_dir() {
            copy_dir(&entry.path(), &dst)?;
        } else if ft.is_file() {
            std::fs::copy(entry.path(), &dst)?;
        }
        // 符号链接：P1 不复制（避免逃逸；L2 加固在 P2），静默跳过
    }
    Ok(())
}

/// 判断包目录下 package.json 的 dependencies 是否非空
fn has_dependencies(dir: &Path) -> bool {
    std::fs::read_to_string(dir.join("package.json"))
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .and_then(|v| v.get("dependencies").cloned())
        .and_then(|d| d.as_object().map(|o| !o.is_empty()))
        .unwrap_or(false)
}

/// 安装期零代码执行：仅装依赖、不跑生命周期脚本
fn run_npm_ignore_scripts(dir: &Path) -> Result<(), String> {
    let status = std::process::Command::new("npm")
        .args(["install", "--ignore-scripts", "--omit=dev"])
        .current_dir(dir)
        .status()
        .map_err(|e| format!("npm 调用失败：{e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err("npm install 失败".into())
    }
}

/// 落位一个已校验的源目录到 packages/<app_id>，并建区 + 注册。
/// staging→rename 保证同盘原子；失败回滚。fresh only（app_id 已存在 → Err，升级见 Task 7）。
pub fn install_from_dir(
    src: &Path,
    layout: &DataLayout,
    registry: &RegistryStore,
    trusted: bool,
) -> Result<InstalledApp, String> {
    let m: Manifest = pkg::load_and_validate(src).map_err(|e| e.to_string())?;
    // 提前在源目录上加载并解析 permissions.json：pkg::load_and_validate 只检查该文件
    // 是否存在，不检查内容能否解析为合法 JSON。这是安装流程里最现实的可失败步骤，
    // 必须在任何文件系统副作用（staging/rename/建应用数据区/写 state/写 registry）
    // 之前完成，否则失败时会在 apps/、sessions/、state/ 下留下无法安全清理的残留
    // （不能无脑连带删除这些目录：用户若此前"卸载并保留数据"，重装失败时会把
    // 保留下来的用户数据一并删掉）。
    let perms = permissions::load(src, &m.superagent.permissions)?;
    let app_id = m.app_id();
    if registry.get(&app_id).is_some() {
        return Err(format!("应用 {app_id} 已安装，请使用升级"));
    }
    let staging = layout.packages_dir(&format!("{app_id}.installing"));
    let final_dir = layout.packages_dir(&app_id);
    let _ = std::fs::remove_dir_all(&staging);

    let result = (|| -> Result<InstalledApp, String> {
        if final_dir.exists() {
            return Err("目标目录已存在".into());
        }
        copy_dir(src, &staging).map_err(|e| format!("复制包失败：{e}"))?;
        if has_dependencies(&staging) {
            run_npm_ignore_scripts(&staging)?;
        }
        std::fs::rename(&staging, &final_dir).map_err(|e| format!("落位失败：{e}"))?;
        layout.ensure_app(&app_id).map_err(|e| e.to_string())?;
        let state = layout.state_path(&app_id);
        if !state.exists() {
            std::fs::write(&state, "{}").map_err(|e| e.to_string())?;
        }
        let rec = InstalledApp {
            app_id: app_id.clone(),
            name: m.name.clone(),
            version: m.version.clone(),
            display_name: m.superagent.display_name.clone(),
            category: m.superagent.category.clone(),
            icon: if src.join("icon.png").is_file() {
                Some("icon.png".into())
            } else {
                None
            },
            trusted,
            domains: perms.domains().to_vec(),
        };
        registry.upsert(rec.clone())?;
        // 装卸钩子（P6-A）：能力注册表按已声明的清单跑一遍 `on_install`——目前唯一有
        // 实际副作用的是 `system.schedule`（把 `scheduledTasks` 登记进 `TaskRegistry`，
        // 与 `open_app_after_acquire` 里的"打开时按最新清单重建"互补，见该函数文档）。
        // 失败走下面 `result.is_err()` 统一回滚，registry 条目也一并摘除。
        crate::capabilities::builtin().on_install(&rec, &perms, layout)?;
        Ok(rec)
    })();

    // 回滚清理 staging + final_dir + registry 条目：经过上面的重排，permissions 解析等
    // 现实可触发的失败已在任何目录创建之前拦截；此处闭包内剩余步骤（ensure_app / 写
    // state / registry.upsert / on_install 钩子）理论上仍可能因罕见的磁盘级错误失败，
    // 届时会在 apps/、sessions/、state/ 下留下空目录/文件。这些残留是无害的，且不可在
    // 此处一并强制删除 —— 否则会误删用户此前"卸载并保留数据"时特意保留下来的数据。
    // registry 条目则必须摘除：`on_install` 失败于 `registry.upsert` 之后，留着会让
    // 这个 app_id 挂着一条指向已被清空的 final_dir 的"幽灵"记录。
    if result.is_err() {
        let _ = std::fs::remove_dir_all(&staging);
        let _ = std::fs::remove_dir_all(&final_dir);
        let _ = registry.remove(&app_id);
    }
    result
}

/// 卸载应用：删除包目录、会话目录、agent home、状态文件，可选删除应用数据，摘除注册，
/// 摘除该 app 的全部已注册定时任务（Task14，见 `scheduler::TaskRegistry::deregister_app`）。
/// 尽力删除，收集错误后如实报告残留。成功返回 Ok(())；若有残留返回 Err(残留列表)。
pub fn uninstall_fs(
    app_id: &str,
    layout: &DataLayout,
    registry: &RegistryStore,
    keep_app_data: bool,
) -> Result<(), String> {
    let mut residue: Vec<String> = Vec::new();

    // 辅助函数：删除目录，收集错误。用 `symlink_metadata`（不跟随）判断：顶层若是符号链接
    // （应用可在自己的可写目录里把目录换成链接）只删链接本身，绝不递归进链接目标。
    let rm = |p: std::path::PathBuf, residue: &mut Vec<String>| {
        let r = match std::fs::symlink_metadata(&p) {
            Ok(m) if m.file_type().is_dir() => std::fs::remove_dir_all(&p),
            Ok(_) => std::fs::remove_file(&p),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
        };
        if let Err(e) = r {
            residue.push(format!("{}: {e}", p.display()));
        }
    };

    // 删除 packages/<app_id>
    rm(layout.packages_dir(app_id), &mut residue);

    // 删除 sessions/<app_id>
    rm(layout.session_dir(app_id), &mut residue);

    // 删除 agenthome/<app_id>（M4：此前卸载后残留；里面只有宿主每次拉起前重写的 settings.json /
    // models.json 与 pi 自己的凭据/信任存储）。无论是否保留应用数据都删。
    rm(layout.agent_home_dir(app_id), &mut residue);

    // 删除 state/<app_id>.json
    let state = layout.state_path(app_id);
    if state.exists() {
        if let Err(e) = std::fs::remove_file(&state) {
            residue.push(format!("{}: {e}", state.display()));
        }
    }

    // 根据 keep_app_data 决定是否删除 apps/<app_id>
    if !keep_app_data {
        rm(layout.app_data_dir(app_id), &mut residue);
    }

    // 摘除注册
    if let Err(e) = registry.remove(app_id) {
        residue.push(format!("registry: {e}"));
    }

    // 删除该应用在 model-overrides.json 里的模型覆盖；文件不存在则无事可做（不创建它）。
    let overrides = layout.model_overrides_path();
    if overrides.exists() {
        if let Err(e) = crate::model_overrides::OverridesStore::new(overrides).set_app(app_id, None)
        {
            residue.push(format!("model-overrides: {e}"));
        }
    }

    // 装卸钩子（P6-A）：能力注册表按 app_id 跑一遍 `on_uninstall`（不看清单——app_id
    // 已从 registry 摘除，清单是否仍能读到不重要，各能力自己决定要不要清理）。目前
    // 唯一有实际副作用的是 `system.schedule`（`TaskRegistry::deregister_app`，即
    // Task14 的原有摘除逻辑，现改走注册表统一入口）；`declared`/权限门在这里不适用
    // ——`CapabilityRegistry::on_uninstall` 对全部能力无条件调用，每个能力自己保证
    // 幂等 no-op（同原 `deregister_app` 语义）。
    for e in crate::capabilities::builtin().on_uninstall(app_id, layout) {
        residue.push(e);
    }

    // 如果有残留，返回 Err；否则返回 Ok
    if residue.is_empty() {
        Ok(())
    } else {
        Err(format!("卸载完成但有残留：{}", residue.join("；")))
    }
}

/// 安装或升级：app_id 未装 → 走全新安装；已装 → 走升级路径。
pub fn install_or_upgrade(
    src: &Path,
    layout: &DataLayout,
    registry: &RegistryStore,
    trusted: bool,
) -> Result<InstalledApp, String> {
    let m: Manifest = pkg::load_and_validate(src).map_err(|e| e.to_string())?;
    let app_id = m.app_id();
    match registry.get(&app_id) {
        None => install_from_dir(src, layout, registry, trusted),
        Some(existing) => upgrade(src, m, existing, layout, registry, trusted),
    }
}

/// 升级已装应用：新版本须 semver 严格大于旧版本，否则拒绝降级/同版本重装；
/// 第三方（!trusted）遇 schemaVersion 变化时拒绝——数据迁移脚本要等安全沙盒(P2)才能跑。
/// 摘除 registry 条目 → 备份旧 packages/<app_id> → install_from_dir 走 fresh 路径落新版；
/// 任一步失败都恢复已变更的状态（registry 条目 / 备份目录），确保失败路径一致可回滚。
/// apps/<app_id>、state/<app_id>.json 数据区全程不动。
fn upgrade(
    src: &Path,
    m: Manifest,
    existing: InstalledApp,
    layout: &DataLayout,
    registry: &RegistryStore,
    trusted: bool,
) -> Result<InstalledApp, String> {
    let app_id = m.app_id();
    let new_v = semver::Version::parse(&m.version).map_err(|e| e.to_string())?;
    let old_v = semver::Version::parse(&existing.version).map_err(|e| e.to_string())?;
    if new_v <= old_v {
        return Err(format!("新版本 {new_v} 不高于已装 {old_v}，拒绝降级/重装"));
    }
    if m.superagent.schema_version != 1 && !trusted {
        return Err("第三方应用的数据迁移需等安全沙盒(P2)就绪".into());
    }

    let final_dir = layout.packages_dir(&app_id);
    let backup = layout.packages_dir(&format!("{app_id}.bak"));
    let _ = std::fs::remove_dir_all(&backup);

    // 装卸钩子（P6-A）：旧包此刻仍在 final_dir（尚未 rename 到 backup），趁着还能读
    // 尽力读一遍旧清单/旧权限——`old_perms` 留到下面装新版失败回滚时，把旧版本的副
    // 作用（`on_install`）重新跑一遍，让"回滚回旧版本"真正等价于"旧版本仍在正常运行"。
    // 读旧清单/权限失败（理论上不该发生——它是当前已装且能正常运行的版本）只记日志，
    // 不阻断升级。**这里只读、不注销**——`on_uninstall` 调用点在下面，必须晚于所有
    // 可回滚的失败退出（见下方注释），不能挪到这里。
    let old_manifest = pkg::load_and_validate(&final_dir).ok();
    let old_perms = old_manifest
        .as_ref()
        .and_then(|om| permissions::load(&final_dir, &om.superagent.permissions).ok());
    if old_perms.is_none() {
        eprintln!("升级 {app_id}：读取旧版本清单/权限失败，跳过旧版本装卸钩子（不阻断升级）");
    }

    // 先摘 registry 条目，让 install_from_dir 走 fresh 路径；失败则未动任何文件，直接返回
    registry.remove(&app_id)?;
    // 备份旧包；若备份失败，恢复 registry 条目再返回
    if let Err(e) = std::fs::rename(&final_dir, &backup) {
        let _ = registry.upsert(existing);
        return Err(format!("升级失败(备份旧版)已回滚：{e}"));
    }

    // 装卸钩子（P6-A）：注销旧版本的能力副作用（目前只有 `system.schedule`，摘除
    // `TaskRegistry` 里已注册的定时任务）**必须晚于所有可回滚的失败退出点**——上面
    // `registry.remove`/`rename` 任一失败都是直接 `Err` 返回，旧版本文件和 registry
    // 条目原封不动；若在那之前就注销了钩子，这两条退出路径会把"旧版本还装着、但它的
    // 定时任务已经被摘掉，且没有任何恢复步骤"这个不一致状态留给用户（要等下一次
    // `open_app` 才会重建，是一段静默失效窗口）。放在这里（`rename` 成功之后、
    // `install_from_dir` 之前）是本函数里唯一"过了这个点就不会再有无恢复的 Err 早退"
    // 的位置：再往后只剩 `install_from_dir` 的 Ok/Err 两个分支，Err 分支已经有一整套
    // 恢复逻辑（rename 备份回来 → registry.upsert(existing) → 用 `old_perms` 重新执行
    // `on_install`），天然覆盖了注销之后唯一可能发生的失败退出，不需要给每个退出点
    // 各自补一份恢复。
    for e in crate::capabilities::builtin().on_uninstall(&app_id, layout) {
        eprintln!("升级 {app_id}：卸载旧版本能力钩子残留：{e}");
    }

    // 装新版；失败则恢复备份 + registry 旧条目 + 旧版本的能力钩子（尽力）
    match install_from_dir(src, layout, registry, trusted) {
        Ok(rec) => {
            let _ = std::fs::remove_dir_all(&backup);
            Ok(rec)
        }
        Err(e) => {
            let _ = std::fs::remove_dir_all(&final_dir);
            let _ = std::fs::rename(&backup, &final_dir);
            let _ = registry.upsert(existing.clone());
            if let Some(p) = &old_perms {
                if let Err(hook_err) =
                    crate::capabilities::builtin().on_install(&existing, p, layout)
                {
                    eprintln!("升级 {app_id} 回滚：重新执行旧版本安装钩子失败：{hook_err}");
                }
            }
            Err(format!("升级失败已回滚：{e}"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::DataLayout;
    use crate::permissions::ScheduledTask;
    use crate::registry::RegistryStore;
    use crate::scheduler::TaskRegistry;
    use std::fs;
    use tempfile::tempdir;

    fn make_pkg(src: &std::path::Path) {
        fs::write(
            src.join("package.json"),
            r#"{
          "name": "@superagent/todo-notes", "version": "1.0.0",
          "keywords": ["pi-package", "superagent-app"],
          "engines": { "superagent-host": ">=1.0.0, <2.0.0" },
          "superagent": { "schemaVersion": 1, "displayName": "待办便签",
            "category": "life", "ui": "ui/index.html", "permissions": "permissions.json" }
        }"#,
        )
        .unwrap();
        fs::write(
            src.join("permissions.json"),
            r#"{ "filesystem": { "write": ["$APP_DATA"] } }"#,
        )
        .unwrap();
        fs::create_dir_all(src.join("ui")).unwrap();
        fs::write(src.join("ui/index.html"), "<html><body>todo</body></html>").unwrap();
    }

    #[test]
    fn install_places_files_and_registers() {
        let root = tempdir().unwrap();
        let src = tempdir().unwrap();
        make_pkg(src.path());
        let layout = DataLayout::new(root.path().to_path_buf());
        let reg = RegistryStore::new(layout.registry_path());
        let rec = install_from_dir(src.path(), &layout, &reg, true).unwrap();
        assert_eq!(rec.app_id, "superagent__todo-notes");
        assert!(layout
            .packages_dir("superagent__todo-notes")
            .join("ui/index.html")
            .is_file());
        assert!(layout.app_data_dir("superagent__todo-notes").is_dir());
        assert!(layout.state_path("superagent__todo-notes").is_file());
        assert!(reg.get("superagent__todo-notes").unwrap().trusted);
    }

    #[test]
    fn invalid_pkg_leaves_no_residue() {
        let root = tempdir().unwrap();
        let src = tempdir().unwrap();
        make_pkg(src.path());
        fs::remove_file(src.path().join("ui/index.html")).unwrap(); // 破坏 UI 声明
        let layout = DataLayout::new(root.path().to_path_buf());
        let reg = RegistryStore::new(layout.registry_path());
        assert!(install_from_dir(src.path(), &layout, &reg, false).is_err());
        assert!(reg.load().is_empty());
        assert!(!layout.packages_dir("superagent__todo-notes").exists());
    }

    #[test]
    fn malformed_permissions_json_leaves_no_residue() {
        let root = tempdir().unwrap();
        let src = tempdir().unwrap();
        make_pkg(src.path());
        fs::write(src.path().join("permissions.json"), "{ not valid json").unwrap(); // 存在但解析失败
        let layout = DataLayout::new(root.path().to_path_buf());
        let reg = RegistryStore::new(layout.registry_path());
        assert!(install_from_dir(src.path(), &layout, &reg, false).is_err());
        assert!(!layout.packages_dir("superagent__todo-notes").exists());
        assert!(!layout.app_data_dir("superagent__todo-notes").exists());
        assert!(!layout.state_path("superagent__todo-notes").exists());
        assert!(reg.load().is_empty());
    }

    #[test]
    fn double_install_rejected() {
        let root = tempdir().unwrap();
        let src = tempdir().unwrap();
        make_pkg(src.path());
        let layout = DataLayout::new(root.path().to_path_buf());
        let reg = RegistryStore::new(layout.registry_path());
        install_from_dir(src.path(), &layout, &reg, true).unwrap();
        assert!(install_from_dir(src.path(), &layout, &reg, true).is_err());
    }

    #[test]
    fn uninstall_keep_vs_delete_app_data() {
        let root = tempdir().unwrap();
        let src = tempdir().unwrap();
        make_pkg(src.path());
        let layout = DataLayout::new(root.path().to_path_buf());
        let reg = RegistryStore::new(layout.registry_path());
        install_from_dir(src.path(), &layout, &reg, true).unwrap();
        let id = "superagent__todo-notes";
        // 保留数据
        uninstall_fs(id, &layout, &reg, true).unwrap();
        assert!(!layout.packages_dir(id).exists());
        assert!(reg.get(id).is_none());
        assert!(layout.app_data_dir(id).exists()); // 保留
        assert!(!layout.session_dir(id).exists()); // 删除（regardless of keep_app_data）
        assert!(!layout.state_path(id).exists()); // 删除（regardless of keep_app_data）
                                                  // 重装后删数据
        install_from_dir(src.path(), &layout, &reg, true).unwrap();
        uninstall_fs(id, &layout, &reg, false).unwrap();
        assert!(!layout.app_data_dir(id).exists());
    }

    fn scheduled_task(id: &str, cron: &str) -> ScheduledTask {
        ScheduledTask {
            id: id.to_string(),
            cron: cron.to_string(),
            prompt: "do it".to_string(),
            catch_up: true,
        }
    }

    #[test]
    fn uninstall_deregisters_the_apps_scheduled_tasks_and_persists() {
        let root = tempdir().unwrap();
        let src = tempdir().unwrap();
        make_pkg(src.path());
        let layout = DataLayout::new(root.path().to_path_buf());
        let reg = RegistryStore::new(layout.registry_path());
        install_from_dir(src.path(), &layout, &reg, true).unwrap();
        let id = "superagent__todo-notes";

        // 模拟安装时（清单 scheduledTasks）注册过定时任务——Task14 只管卸载侧摘除，
        // 不涉及安装侧接线，所以测试直接用 TaskRegistry::register 构造前置状态。
        let task_reg = TaskRegistry::new(&layout);
        task_reg
            .register(
                id,
                &[
                    scheduled_task("daily", "0 9 * * *"),
                    scheduled_task("weekly", "0 9 * * 1"),
                ],
            )
            .unwrap();
        assert_eq!(task_reg.all().len(), 2);

        uninstall_fs(id, &layout, &reg, false).unwrap();

        assert!(task_reg.all().is_empty());
        // 持久化：全新 TaskRegistry 指向同一目录重新读盘，仍然确认已移除。
        let fresh = TaskRegistry::new(&layout);
        assert!(fresh.all().is_empty());
    }

    #[test]
    fn uninstall_app_with_no_scheduled_tasks_is_noop_and_other_apps_untouched() {
        let root = tempdir().unwrap();
        let src = tempdir().unwrap();
        make_pkg(src.path());
        let layout = DataLayout::new(root.path().to_path_buf());
        let reg = RegistryStore::new(layout.registry_path());
        install_from_dir(src.path(), &layout, &reg, true).unwrap();
        let id = "superagent__todo-notes";

        // 另一个 app 的定时任务须完全不受影响。
        let task_reg = TaskRegistry::new(&layout);
        task_reg
            .register("other-app", &[scheduled_task("weekly", "0 9 * * 1")])
            .unwrap();

        // `id` 本身从未注册过定时任务——卸载须是干净的 no-op，不报错。
        uninstall_fs(id, &layout, &reg, false).unwrap();

        let all = task_reg.all();
        assert_eq!(all.len(), 1);
        assert!(all.iter().all(|t| t.app_id == "other-app"));
    }

    /// P6-C Task5：端到端——`uninstall_fs` 对全部能力无条件调 `on_uninstall`
    /// （见本文件上方 `on_uninstall`/`upgrade_or_reinstall` 调用点注释），`
    /// connectors` 能力借这条既有装卸钩子清空该 app 名下的放行规则与暂存
    /// 调用（spec §4「卸载即清」），不需要该 app 实际声明过 connectors 权限
    /// ——`todo-notes` 测试夹具本身只声明了 filesystem 写权限，装的时候不会
    /// 碰 `ApprovalStore`，这里直接在卸载前手工写入两张表模拟"用户此前批
    /// 过若干次连接器调用、还留了一条待批"的前置状态。
    #[test]
    fn uninstall_clears_the_apps_approval_rules_and_staged_calls() {
        let root = tempdir().unwrap();
        let src = tempdir().unwrap();
        make_pkg(src.path());
        let layout = DataLayout::new(root.path().to_path_buf());
        let reg = RegistryStore::new(layout.registry_path());
        install_from_dir(src.path(), &layout, &reg, true).unwrap();
        let id = "superagent__todo-notes";

        let store = crate::approvals::ApprovalStore::new(layout.clone());
        store.add_rule(id, "fs1", "read_file", 1).unwrap();
        store
            .stage(id, "fs1", "write_file", serde_json::json!({"path": "x"}), 1)
            .unwrap();
        // 另一个 app 的规则须完全不受影响。
        store.add_rule("other-app", "fs1", "read_file", 1).unwrap();

        uninstall_fs(id, &layout, &reg, false).unwrap();

        assert!(store.list_rules(Some(id)).unwrap().is_empty());
        assert!(store.list_staged(Some(id)).unwrap().is_empty());
        assert_eq!(store.list_rules(Some("other-app")).unwrap().len(), 1);
        // 持久化：全新 ApprovalStore 指向同一目录重新读盘，仍然确认已清空。
        let fresh = crate::approvals::ApprovalStore::new(layout.clone());
        assert!(fresh.list_rules(Some(id)).unwrap().is_empty());
    }

    #[test]
    fn uninstall_removes_the_apps_model_override_only() {
        use crate::model_overrides::{ModelChoice, OverridesStore};
        let root = tempdir().unwrap();
        let src = tempdir().unwrap();
        make_pkg(src.path());
        let layout = DataLayout::new(root.path().to_path_buf());
        let reg = RegistryStore::new(layout.registry_path());
        install_from_dir(src.path(), &layout, &reg, true).unwrap();
        let id = "superagent__todo-notes";
        let choice = || {
            Some(ModelChoice {
                provider: "deepseek".into(),
                model: "m".into(),
            })
        };
        let store = OverridesStore::new(layout.model_overrides_path());
        // 文件不存在时卸载不报错、也不凭空创建它。
        uninstall_fs(id, &layout, &reg, false).unwrap();
        assert!(!layout.model_overrides_path().exists());
        install_from_dir(src.path(), &layout, &reg, true).unwrap();
        store.set_app(id, choice()).unwrap();
        store.set_app("other-app", choice()).unwrap();
        uninstall_fs(id, &layout, &reg, false).unwrap();
        let f = store.load().unwrap();
        assert!(!f.apps.contains_key(id));
        assert!(f.apps.contains_key("other-app"));
    }

    #[test]
    fn upgrade_bumps_version_downgrade_rejected() {
        let root = tempdir().unwrap();
        let layout = DataLayout::new(root.path().to_path_buf());
        let reg = RegistryStore::new(layout.registry_path());
        let id = "superagent__todo-notes";

        let src1 = tempdir().unwrap();
        make_pkg(src1.path());
        install_or_upgrade(src1.path(), &layout, &reg, true).unwrap();
        assert_eq!(reg.get(id).unwrap().version, "1.0.0");

        // 写点应用数据，验证升级不动数据
        std::fs::write(layout.app_data_dir(id).join("keep.txt"), "x").unwrap();

        let src2 = tempdir().unwrap();
        make_pkg(src2.path());
        let p2 = std::fs::read_to_string(src2.path().join("package.json"))
            .unwrap()
            .replace("\"1.0.0\"", "\"1.1.0\"");
        std::fs::write(src2.path().join("package.json"), p2).unwrap();
        install_or_upgrade(src2.path(), &layout, &reg, true).unwrap();
        assert_eq!(reg.get(id).unwrap().version, "1.1.0");
        assert!(layout.app_data_dir(id).join("keep.txt").is_file()); // 数据保留

        // 降级/同版本拒绝
        assert!(install_or_upgrade(src1.path(), &layout, &reg, true).is_err());
    }

    #[test]
    fn upgrade_install_failure_rolls_back_to_old_version() {
        let root = tempdir().unwrap();
        let layout = DataLayout::new(root.path().to_path_buf());
        let reg = RegistryStore::new(layout.registry_path());
        let id = "superagent__todo-notes";

        // 先装 v1.0.0，并在 apps/<id> 下留一个标记文件
        let src1 = tempdir().unwrap();
        make_pkg(src1.path());
        install_or_upgrade(src1.path(), &layout, &reg, true).unwrap();
        assert_eq!(reg.get(id).unwrap().version, "1.0.0");
        std::fs::write(layout.app_data_dir(id).join("keep.txt"), "x").unwrap();

        // 构造 v1.1.0（高于旧版，过 semver 门）但 permissions.json 存在却解析失败，
        // 让 install_from_dir 在源目录校验阶段就失败（不产生任何落位副作用）
        let src2 = tempdir().unwrap();
        make_pkg(src2.path());
        let p2 = std::fs::read_to_string(src2.path().join("package.json"))
            .unwrap()
            .replace("\"1.0.0\"", "\"1.1.0\"");
        std::fs::write(src2.path().join("package.json"), p2).unwrap();
        std::fs::write(src2.path().join("permissions.json"), "{ not valid json").unwrap();

        assert!(install_or_upgrade(src2.path(), &layout, &reg, true).is_err());

        // 旧版本应完整回滚：registry 条目、packages 目录、apps 数据全部恢复
        assert_eq!(reg.get(id).unwrap().version, "1.0.0");
        assert!(layout.packages_dir(id).exists());
        assert!(layout.app_data_dir(id).join("keep.txt").is_file());
    }

    /// I-2 回归（Task9 review）：`upgrade()` 在"不可回头点"（`registry.remove` +
    /// `rename(final_dir, backup)` 都已成功）之后才会 `on_uninstall` 旧版本的能力钩子
    /// （摘掉已注册的定时任务），随即尝试 `install_from_dir` 装新版。若新版在这之后
    /// 失败（此处用一份带未知字段的 `permissions.json`，在 `install_from_dir` 内部
    /// `permissions::load` 阶段失败——早于任何落位副作用，但已经晚于 `upgrade()` 自己
    /// 的 registry.remove/rename），唯一恢复入口是 `install_from_dir`-失败分支：
    /// rename 备份回来 → `registry.upsert(existing)` → 用 `old_perms` 重新执行
    /// `on_install`。本测试断言这条链路真的把旧版本已经注册过的定时任务
    /// （`nightly`）恢复回 `TaskRegistry`，不是只恢复了文件和 registry 条目。
    #[test]
    fn upgrade_install_failure_restores_old_versions_scheduled_tasks() {
        let root = tempdir().unwrap();
        let layout = DataLayout::new(root.path().to_path_buf());
        let reg = RegistryStore::new(layout.registry_path());
        let id = "superagent__todo-notes";

        // 装 v1.0.0，清单声明 system.schedule + 一个定时任务 nightly。
        let src1 = tempdir().unwrap();
        make_pkg(src1.path());
        fs::write(
            src1.path().join("permissions.json"),
            r#"{ "system": { "schedule": true }, "scheduledTasks": [{ "id": "nightly", "cron": "0 22 * * *", "prompt": "p" }] }"#,
        ).unwrap();
        install_or_upgrade(src1.path(), &layout, &reg, true).unwrap();
        assert_eq!(reg.get(id).unwrap().version, "1.0.0");

        let before = TaskRegistry::new(&layout).all();
        assert!(
            before.iter().any(|t| t.app_id == id && t.id == "nightly"),
            "前置：安装 v1 后 nightly 应已注册进 TaskRegistry，实际：{before:?}"
        );

        // 构造 v1.1.0（高于旧版，过 semver 门），但 permissions.json 含未知字段，
        // 会在 install_from_dir 内部（pkg::load_and_validate 通过之后）的
        // permissions::load 阶段解析失败——晚于 upgrade() 自己的
        // registry.remove/rename（那两步已经成功），早于任何落位副作用。
        let src2 = tempdir().unwrap();
        make_pkg(src2.path());
        let p2 = fs::read_to_string(src2.path().join("package.json"))
            .unwrap()
            .replace("\"1.0.0\"", "\"1.1.0\"");
        fs::write(src2.path().join("package.json"), p2).unwrap();
        fs::write(
            src2.path().join("permissions.json"),
            r#"{ "telepathy": true }"#,
        )
        .unwrap();

        assert!(install_or_upgrade(src2.path(), &layout, &reg, true).is_err());

        // 旧版本文件/registry 条目应完整回滚（既有断言，同
        // upgrade_install_failure_rolls_back_to_old_version）。
        assert_eq!(reg.get(id).unwrap().version, "1.0.0");
        assert!(layout.packages_dir(id).exists());

        // 关键断言：nightly 定时任务也应恢复——不是只有文件和 registry 条目回来了，
        // 却把 on_uninstall 摘掉的定时任务落下了。
        let after = TaskRegistry::new(&layout).all();
        assert!(
            after.iter().any(|t| t.app_id == id && t.id == "nightly"),
            "升级失败回滚后 nightly 应被 on_install(existing, old_perms) 重新注册，实际：{after:?}"
        );
    }

    /// M4：卸载同时删除 agenthome/<id>（settings.json/models.json/凭据存储等宿主写的配置）；
    /// 顶层若被应用换成符号链接，只删链接本身，不跟随删除链接目标里的内容。
    #[test]
    fn uninstall_removes_agent_home_and_never_follows_top_level_link() {
        for as_link in [false, true] {
            let root = tempdir().unwrap();
            let layout = DataLayout::new(root.path().to_path_buf());
            let reg = RegistryStore::new(layout.registry_path());
            let id = "superagent__todo-notes";
            let src = tempdir().unwrap();
            make_pkg(src.path());
            install_or_upgrade(src.path(), &layout, &reg, true).unwrap();
            let victim = root.path().join("victim");
            fs::create_dir_all(&victim).unwrap();
            fs::write(victim.join("keep.txt"), "keep").unwrap();
            let home = layout.agent_home_dir(id);
            if as_link {
                fs::create_dir_all(home.parent().unwrap()).unwrap();
                std::os::unix::fs::symlink(&victim, &home).unwrap();
            } else {
                fs::create_dir_all(&home).unwrap();
                fs::write(home.join("settings.json"), "{}").unwrap();
            }
            uninstall_fs(id, &layout, &reg, false).unwrap();
            assert!(
                fs::symlink_metadata(&home).is_err(),
                "agenthome 应被删除（link={as_link}）"
            );
            assert_eq!(fs::read_to_string(victim.join("keep.txt")).unwrap(), "keep");
        }
    }
}
