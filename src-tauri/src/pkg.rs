// 包清单解析、校验、规范化
use serde::Deserialize;
use std::path::Path;

pub const HOST_API_VERSION: &str = "1.0.0";
const SUPPORTED_SCHEMA: u32 = 1;

#[derive(Debug, thiserror::Error)]
pub enum PkgError {
    #[error("读取 package.json 失败：{0}")]
    Io(String),
    #[error("package.json 解析失败：{0}")]
    Parse(String),
    #[error("缺少 superagent-app 关键字，不是合法的 Super Agent 应用")]
    MissingKeyword,
    #[error("不支持的包格式版本 {0}，请升级宿主")]
    UnsupportedSchema(u32),
    #[error("此应用要求宿主 {required}，当前宿主为 {host}，请升级宿主")]
    IncompatibleHost { required: String, host: String },
    #[error("engines.superagent-host 版本区间非法：{0}")]
    BadEngine(String),
    #[error("清单声明的 UI 文件不存在")]
    MissingUi,
    #[error("清单声明的 permissions 文件不存在")]
    MissingPermissions,
    #[error("清单路径非法(不得为绝对路径或含 ..)：{0}")]
    UnsafePath(String),
}

#[derive(Debug, Deserialize)]
pub struct Manifest {
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub keywords: Vec<String>,
    pub engines: Engines,
    pub superagent: SuperagentField,
}

#[derive(Debug, Deserialize)]
pub struct Engines {
    #[serde(rename = "superagent-host")]
    pub superagent_host: String,
}

#[derive(Debug, Deserialize)]
pub struct SuperagentField {
    #[serde(rename = "schemaVersion")]
    pub schema_version: u32,
    #[serde(rename = "displayName")]
    pub display_name: String,
    pub category: String,
    pub ui: String,
    pub permissions: String,
    #[serde(default)]
    pub subagents: Vec<String>,
    /// 该应用希望使用的模型（可选）；`open_app` 若存在则追加 `--model <model>`。
    #[serde(default)]
    pub model: Option<String>,
    /// 该应用希望启用的工具列表（可选）；`open_app` 若非空则追加 `--tools <逗号拼接>`。
    #[serde(default)]
    pub tools: Vec<String>,
}

impl Manifest {
    pub fn app_id(&self) -> String {
        normalize_app_id(&self.name)
    }
}

/// 校验清单里声明的相对路径是否安全：不得是绝对路径，也不得含 `..` 上跳，
/// 防止恶意包通过 ui/permissions 字段逃出自己的包目录读取宿主任意文件。
fn is_safe_rel(p: &str) -> bool {
    let path = std::path::Path::new(p);
    !path.is_absolute()
        && path
            .components()
            .all(|c| !matches!(c, std::path::Component::ParentDir))
}

pub fn normalize_app_id(name: &str) -> String {
    let mut out = String::new();
    for c in name.chars() {
        match c {
            '@' => {}                  // 去掉作用域前缀符
            '/' => out.push_str("__"), // 作用域分隔 → 双下划线
            c if c.is_ascii_alphanumeric() || c == '-' || c == '_' => out.push(c),
            _ => out.push('-'),
        }
    }
    out
}

pub fn load_and_validate(dir: &Path) -> Result<Manifest, PkgError> {
    let raw = std::fs::read_to_string(dir.join("package.json"))
        .map_err(|e| PkgError::Io(e.to_string()))?;
    let m: Manifest = serde_json::from_str(&raw).map_err(|e| PkgError::Parse(e.to_string()))?;

    // 1. keywords 必须同时含 "pi-package" 与 "superagent-app"，二者缺一即非法应用（spec §4）
    if !m.keywords.iter().any(|k| k == "pi-package")
        || !m.keywords.iter().any(|k| k == "superagent-app")
    {
        return Err(PkgError::MissingKeyword);
    }
    // 2. schemaVersion 必须匹配当前宿主支持的清单格式版本，避免解析未来/未知格式
    if m.superagent.schema_version != SUPPORTED_SCHEMA {
        return Err(PkgError::UnsupportedSchema(m.superagent.schema_version));
    }
    // 3. engines.superagent-host 必须是合法的 semver 版本区间
    let req = semver::VersionReq::parse(&m.engines.superagent_host)
        .map_err(|e| PkgError::BadEngine(e.to_string()))?;
    let host =
        semver::Version::parse(HOST_API_VERSION).map_err(|e| PkgError::BadEngine(e.to_string()))?;
    // 4. 当前宿主版本必须落在该区间内，否则拒绝加载不兼容的包
    if !req.matches(&host) {
        return Err(PkgError::IncompatibleHost {
            required: m.engines.superagent_host.clone(),
            host: HOST_API_VERSION.to_string(),
        });
    }
    // 5. ui/permissions 声明的路径必须是包目录内的相对路径，防止第三方包借由
    //    绝对路径或 ".." 逃出沙箱读取宿主任意文件（先查路径合法性，再查文件是否存在）
    if !is_safe_rel(&m.superagent.ui) {
        return Err(PkgError::UnsafePath(m.superagent.ui.clone()));
    }
    if !is_safe_rel(&m.superagent.permissions) {
        return Err(PkgError::UnsafePath(m.superagent.permissions.clone()));
    }
    // 6. UI 入口文件必须真实存在
    if !dir.join(&m.superagent.ui).is_file() {
        return Err(PkgError::MissingUi);
    }
    // 7. permissions 声明文件必须真实存在
    if !dir.join(&m.superagent.permissions).is_file() {
        return Err(PkgError::MissingPermissions);
    }
    Ok(m)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn write_pkg(dir: &std::path::Path, json: &str) {
        fs::write(dir.join("package.json"), json).unwrap();
        fs::write(dir.join("permissions.json"), "{}").unwrap();
        fs::create_dir_all(dir.join("ui")).unwrap();
        fs::write(dir.join("ui/index.html"), "<html></html>").unwrap();
    }

    const VALID: &str = r#"{
      "name": "@superagent/todo-notes", "version": "1.0.0",
      "keywords": ["pi-package", "superagent-app"],
      "engines": { "superagent-host": ">=1.0.0, <2.0.0" },
      "superagent": { "schemaVersion": 1, "displayName": "待办便签",
        "category": "life", "ui": "ui/index.html", "permissions": "permissions.json" }
    }"#;

    #[test]
    fn normalize_id() {
        assert_eq!(
            normalize_app_id("@superagent/todo-notes"),
            "superagent__todo-notes"
        );
        assert_eq!(normalize_app_id("plain-name"), "plain-name");
    }

    #[test]
    fn valid_pkg_loads() {
        let d = tempdir().unwrap();
        write_pkg(d.path(), VALID);
        let m = load_and_validate(d.path()).unwrap();
        assert_eq!(m.app_id(), "superagent__todo-notes");
        assert_eq!(m.superagent.display_name, "待办便签");
    }

    #[test]
    fn missing_keyword_rejected() {
        let d = tempdir().unwrap();
        write_pkg(d.path(), &VALID.replace("\"superagent-app\"", "\"other\""));
        assert!(matches!(
            load_and_validate(d.path()),
            Err(PkgError::MissingKeyword)
        ));
    }

    #[test]
    fn missing_pi_package_keyword_rejected() {
        let d = tempdir().unwrap();
        write_pkg(d.path(), &VALID.replace("\"pi-package\", ", ""));
        assert!(matches!(
            load_and_validate(d.path()),
            Err(PkgError::MissingKeyword)
        ));
    }

    #[test]
    fn unsafe_ui_path_rejected() {
        // 绝对路径逃逸
        let d = tempdir().unwrap();
        write_pkg(
            d.path(),
            &VALID.replace("\"ui/index.html\"", "\"/etc/passwd\""),
        );
        assert!(matches!(
            load_and_validate(d.path()),
            Err(PkgError::UnsafePath(_))
        ));

        // 含 ".." 上跳逃逸
        let d2 = tempdir().unwrap();
        write_pkg(
            d2.path(),
            &VALID.replace("\"ui/index.html\"", "\"../escape.html\""),
        );
        assert!(matches!(
            load_and_validate(d2.path()),
            Err(PkgError::UnsafePath(_))
        ));
    }

    #[test]
    fn unknown_schema_version_rejected() {
        let d = tempdir().unwrap();
        write_pkg(
            d.path(),
            &VALID.replace("\"schemaVersion\": 1", "\"schemaVersion\": 2"),
        );
        assert!(matches!(
            load_and_validate(d.path()),
            Err(PkgError::UnsupportedSchema(2))
        ));
    }

    #[test]
    fn incompatible_engine_rejected() {
        let d = tempdir().unwrap();
        write_pkg(d.path(), &VALID.replace(">=1.0.0, <2.0.0", ">=2.0.0"));
        assert!(matches!(
            load_and_validate(d.path()),
            Err(PkgError::IncompatibleHost { .. })
        ));
    }

    #[test]
    fn missing_ui_file_rejected() {
        let d = tempdir().unwrap();
        write_pkg(d.path(), VALID);
        std::fs::remove_file(d.path().join("ui/index.html")).unwrap();
        assert!(matches!(
            load_and_validate(d.path()),
            Err(PkgError::MissingUi)
        ));
    }
}
