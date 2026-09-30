//! P5 发布（§11.6 机械化内核）：把一个已装应用导出成"可以直接推到 GitHub 精选市场
//! 仓库"的形态——导出包目录 + 一条市场索引条目。
//!
//! 本模块只做机械化的"打包 + 生成条目"这一段：
//! - `build_entry`：从 registry 记录 + 权限清单生成一条 `market::MarketEntry`（权限
//!   摘要复用 `CapabilityRegistry::render_human`（P6-A），与安装确认弹窗看到的是同一套人话）。
//! - `export_package`：把 `packages/<app_id>` 复制到 `published/<app_id>/`（复用
//!   `install::copy_dir`——只拷常规文件/目录，跳过符号链接）。
//! - `merge_index_entry`：把条目 upsert 进 `published/index.json`（同 `name` 覆盖）。
//!
//! **真实推送到 GitHub（git push / API 上传）是手工里程碑**（凭据边界，需用户 GitHub
//! 权限），不在码内自动执行——`published/` 目录备好后由用户自己推。

use crate::market::MarketEntry;
use crate::paths::DataLayout;
use crate::permissions::Permissions;
use crate::registry::InstalledApp;
use std::path::PathBuf;

/// 从 registry 记录 + 权限清单生成一条市场条目（纯函数，可测）。
/// `source` 由调用方给（发布产出目录相对路径 / 未来的 git URL）。
/// `description` v1 留空——registry 记录里没有描述字段，未来可从 manifest 补。
pub fn build_entry(app: &InstalledApp, perms: &Permissions, source: String) -> MarketEntry {
    MarketEntry {
        name: app.name.clone(),
        display_name: app.display_name.clone(),
        version: app.version.clone(),
        category: app.category.clone(),
        icon: app.icon.clone(),
        description: String::new(),
        source,
        // trusted 不影响非特权能力的 `declared()` 判定（见 `capability.rs`），发布摘要
        // 按第三方/未知处理即可，与 `lib.rs::preview_install`/`maker.rs::list_pending_installs`
        // 用同一套调用惯例。
        permissions: crate::capabilities::builtin().render_human(
            perms,
            &crate::capability::CallerIdentity::installing(&app.app_id, false),
        ),
        // P6-B Task 6：`kind`/`download_url`/`sha256`/`size`/`author` 是技能市场
        // 条目专用字段（`kind: "skill"`）——`publish_app`/`build_entry` 发布的是
        // **应用**，恒为默认的 `"app"` + 全空，技能条目由
        // `lib.rs::skill_market_install` 走另一条路径写进索引，不经这里。
        kind: crate::market::kind_app(),
        download_url: None,
        sha256: None,
        size: None,
        author: None,
    }
}

/// 把 `packages/<app_id>` 导出到 `published/<app_id>/`。源是**已安装的包目录**（本就
/// 只含发行内容，不含运行期产物如 settings.json/session 历史——那些在 apps/ 与
/// agent home，不在 packages/）。已存在旧导出时先清掉，保证是干净最新版。
pub fn export_package(app_id: &str, layout: &DataLayout) -> Result<PathBuf, String> {
    let src = layout.packages_dir(app_id);
    if !src.is_dir() {
        return Err(format!("应用 {app_id} 未安装（找不到包目录 {src:?}）"));
    }
    let dst = layout.published_app_dir(app_id);
    let _ = std::fs::remove_dir_all(&dst); // 幂等：清旧导出
    crate::install::copy_dir(&src, &dst).map_err(|e| e.to_string())?;
    Ok(dst)
}

/// 把一条条目 upsert 进 `published/index.json`（同 `name` 覆盖，不重复）。文件不存在
/// 就新建。这份 index.json 就是"可推到精选市场仓库"的索引。
pub fn merge_index_entry(layout: &DataLayout, entry: &MarketEntry) -> Result<(), String> {
    let dir = layout.published_dir();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let path = dir.join("index.json");

    let mut entries: Vec<MarketEntry> = match std::fs::read_to_string(&path) {
        Ok(raw) => crate::market::parse_index(&raw).unwrap_or_default(),
        Err(_) => Vec::new(),
    };
    entries.retain(|e| e.name != entry.name);
    entries.push(entry.clone());

    let index = serde_json::json!({ "entries": entries });
    let text = serde_json::to_string_pretty(&index).map_err(|e| e.to_string())?;
    std::fs::write(&path, text).map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::permissions::Permissions;

    fn app(app_id: &str) -> InstalledApp {
        InstalledApp {
            app_id: app_id.into(),
            name: format!("@superagent/{}", app_id.trim_start_matches("superagent__")),
            version: "1.2.3".into(),
            display_name: "演示应用".into(),
            category: "automation".into(),
            icon: None,
            trusted: false,
            domains: vec![],
        }
    }

    #[test]
    fn build_entry_carries_metadata_and_human_permissions() {
        let a = app("superagent__researcher");
        let perms: Permissions =
            serde_json::from_str(r#"{ "agents": { "call": ["@superagent/summarizer"] } }"#)
                .unwrap();
        let entry = build_entry(&a, &perms, "superagent__researcher".to_string());
        assert_eq!(entry.name, "@superagent/researcher");
        assert_eq!(entry.version, "1.2.3");
        assert_eq!(entry.category, "automation");
        assert_eq!(entry.source, "superagent__researcher");
        assert!(
            entry.permissions.iter().any(|p| p.contains("调用其他应用")),
            "权限摘要应含人话的调用声明，实际：{:?}",
            entry.permissions
        );
    }

    #[test]
    fn export_package_copies_package_dir_and_errors_when_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let layout = DataLayout::new(tmp.path().to_path_buf());

        // 未装 → Err。
        assert!(export_package("superagent__nope", &layout).is_err());

        // 造一个 packages/<app_id>（含 package.json）。
        let pkg = layout.packages_dir("superagent__x");
        std::fs::create_dir_all(pkg.join("ui")).unwrap();
        std::fs::write(pkg.join("package.json"), r#"{"name":"@superagent/x"}"#).unwrap();
        std::fs::write(pkg.join("ui/index.html"), "<html></html>").unwrap();

        let out = export_package("superagent__x", &layout).unwrap();
        assert_eq!(out, layout.published_app_dir("superagent__x"));
        assert!(out.join("package.json").is_file(), "导出应含 package.json");
        assert!(
            out.join("ui/index.html").is_file(),
            "导出应递归含子目录文件"
        );
    }

    #[test]
    fn merge_index_entry_creates_then_upserts_by_name() {
        let tmp = tempfile::tempdir().unwrap();
        let layout = DataLayout::new(tmp.path().to_path_buf());
        let a = app("superagent__x");
        let perms = Permissions::default();

        let e1 = build_entry(&a, &perms, "superagent__x".to_string());
        merge_index_entry(&layout, &e1).unwrap();

        let path = layout.published_dir().join("index.json");
        let entries = crate::market::parse_index(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].version, "1.2.3");

        // 同 name 再发布一个新版本 → 覆盖，不重复。
        let mut a2 = app("superagent__x");
        a2.version = "2.0.0".into();
        let e2 = build_entry(&a2, &perms, "superagent__x".to_string());
        merge_index_entry(&layout, &e2).unwrap();

        let entries = crate::market::parse_index(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(entries.len(), 1, "同 name 应覆盖，不新增");
        assert_eq!(entries[0].version, "2.0.0");
    }
}
