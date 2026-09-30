use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// 已装应用记录（包括元数据：app_id / name / version / display_name / category / icon / trusted / domains）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstalledApp {
    pub app_id: String,
    pub name: String,
    pub version: String,
    pub display_name: String,
    pub category: String,
    #[serde(default)]
    pub icon: Option<String>,
    #[serde(default)]
    pub trusted: bool,
    #[serde(default)]
    pub domains: Vec<String>,
}

/// 已装应用索引存储（串行原子写 .json.tmp + rename）
pub struct RegistryStore {
    path: PathBuf,
}

impl RegistryStore {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    /// 从 registry.json 加载所有已装应用；损坏当空返回
    pub fn load(&self) -> Vec<InstalledApp> {
        std::fs::read_to_string(&self.path)
            .ok()
            .and_then(|s| serde_json::from_str::<Vec<InstalledApp>>(&s).ok())
            .unwrap_or_default()
    }

    /// 内部原子写：先写 .tmp，再 rename
    fn save(&self, apps: &[InstalledApp]) -> Result<(), String> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let tmp = self.path.with_extension("json.tmp");
        let body = serde_json::to_string_pretty(apps).map_err(|e| e.to_string())?;
        std::fs::write(&tmp, body).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, &self.path).map_err(|e| e.to_string())
    }

    /// 插入或更新应用（同 app_id 覆盖，不新增）
    pub fn upsert(&self, app: InstalledApp) -> Result<(), String> {
        let mut apps = self.load();
        if let Some(existing) = apps.iter_mut().find(|a| a.app_id == app.app_id) {
            *existing = app;
        } else {
            apps.push(app);
        }
        self.save(&apps)
    }

    /// 删除应用（幂等）
    pub fn remove(&self, app_id: &str) -> Result<(), String> {
        let mut apps = self.load();
        apps.retain(|a| a.app_id != app_id);
        self.save(&apps)
    }

    /// 按 app_id 获取单个应用
    pub fn get(&self, app_id: &str) -> Option<InstalledApp> {
        self.load().into_iter().find(|a| a.app_id == app_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn app(id: &str, cat: &str) -> InstalledApp {
        InstalledApp {
            app_id: id.into(),
            name: id.into(),
            version: "1.0.0".into(),
            display_name: id.into(),
            category: cat.into(),
            icon: None,
            trusted: false,
            domains: vec![],
        }
    }

    #[test]
    fn upsert_list_get_remove_roundtrip() {
        let d = tempdir().unwrap();
        let s = RegistryStore::new(d.path().join("registry.json"));
        assert!(s.load().is_empty());
        s.upsert(app("a", "life")).unwrap();
        s.upsert(app("b", "info")).unwrap();
        assert_eq!(s.load().len(), 2);
        assert_eq!(s.get("a").unwrap().category, "life");
        // upsert 同 id = 覆盖，不新增
        let mut a2 = app("a", "info");
        a2.version = "2.0.0".into();
        s.upsert(a2).unwrap();
        assert_eq!(s.load().len(), 2);
        assert_eq!(s.get("a").unwrap().version, "2.0.0");
        s.remove("a").unwrap();
        assert!(s.get("a").is_none());
        assert_eq!(s.load().len(), 1);
    }

    #[test]
    fn corrupt_registry_reads_as_empty() {
        let d = tempdir().unwrap();
        let p = d.path().join("registry.json");
        std::fs::write(&p, "not json").unwrap();
        let s = RegistryStore::new(p);
        assert!(s.load().is_empty()); // 损坏当空，可从 packages/ 重建（后续任务）
    }
}
