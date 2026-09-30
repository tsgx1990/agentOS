use serde::{Deserialize, Serialize};
use std::path::Path;

// 文件系统权限
#[derive(Debug, Default, Clone, Deserialize, Serialize)]
pub struct FsPerm {
    #[serde(default)]
    pub read: Vec<String>,
    #[serde(default)]
    pub write: Vec<String>,
}

/// 界面（预制 H5）允许 `connect-src` 访问的网址——只约束 WebView CSP，不代表 agent 子进程可出网
/// （agent 出网走连接器）。原字段名 `network.domains` 因会被误读为「agent 可联网」而更名。
#[derive(Debug, Default, Clone, Deserialize, Serialize)]
pub struct UiPerm {
    #[serde(default, rename = "connectSrc")]
    pub connect_src: Vec<String>,
}

// 应用调用权限
#[derive(Debug, Default, Clone, Deserialize, Serialize)]
pub struct AgentsPerm {
    #[serde(default)]
    pub call: Vec<String>,
}

// 系统权限
#[derive(Debug, Default, Clone, Deserialize, Serialize)]
pub struct SystemPerm {
    #[serde(default)]
    pub notifications: bool,
    #[serde(default)]
    pub schedule: bool,
}

// 连接器访问级别：只读 / 读写。清单省略时缺省为只读（最小权限原则）。
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Access {
    #[default]
    Read,
    ReadWrite,
}

// 应用声明的连接器（MCP 服务）访问请求，Task 6 消费
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ConnectorReq {
    pub category: String,
    #[serde(default)]
    pub access: Access,
}

fn default_catch_up() -> bool {
    true
}

// 清单声明的定时任务，Task 10 消费
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ScheduledTask {
    pub id: String,
    pub cron: String,
    pub prompt: String,
    #[serde(default = "default_catch_up", rename = "catchUp")]
    pub catch_up: bool,
}

/// 技能授予权限（P6-B）：只有声明 `skills.allow = true` 的应用才可能被授予/加载
/// 技能——`skills` 能力（`capabilities/skills.rs`）的 `declared()` 只读这一个
/// 字段。省略时默认 `false`（最小权限原则，同其它 `*Perm` 结构）。
#[derive(Debug, Default, Clone, Deserialize, Serialize, PartialEq)]
pub struct SkillsPerm {
    #[serde(default)]
    pub allow: bool,
}

// 权限配置
//
// F4（review）：新增 `Serialize`——`tests/capability_invariants_it.rs` 的
// `every_permissions_field_is_owned_by_some_capability` 现在直接
// `serde_json::to_value(Permissions::default())` 读真实顶层字段集合去和
// `capabilities::builtin()` 对账，不再靠两份手写列表互相打勾（那种写法下新增
// 字段可以完全不触碰任何一份列表，测试照样全绿——2026-08-10 缺陷的确切形状）。
#[derive(Debug, Default, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Permissions {
    #[serde(default)]
    pub filesystem: FsPerm,
    #[serde(default)]
    pub ui: UiPerm,
    #[serde(default)]
    pub agents: AgentsPerm,
    #[serde(default)]
    pub system: SystemPerm,
    #[serde(default)]
    pub connectors: Vec<ConnectorReq>,
    #[serde(default, rename = "scheduledTasks")]
    pub scheduled_tasks: Vec<ScheduledTask>,
    #[serde(default)]
    pub skills: SkillsPerm,
}

impl Permissions {
    /// 供 WebView CSP `connect-src` 使用（`install.rs`/`scheme.rs` 既有调用点不变）。
    pub fn domains(&self) -> &[String] {
        &self.ui.connect_src
    }
}

/// 解析权限清单 JSON 文本（`load` 的纯函数部分，供测试与安装预览复用）。
/// 未知字段给定向提示：`mcp`/`network` 是已删除/改名的旧字段，单独指路；
/// 其余未知字段报出字段名与可用字段清单。
pub fn parse(raw: &str) -> Result<Permissions, String> {
    serde_json::from_str::<Permissions>(raw).map_err(|e| {
        let msg = e.to_string();
        if msg.contains("unknown field `mcp`") {
            "权限清单字段 `mcp` 已废弃，请改用 `connectors`（例：{\"connectors\":[{\"category\":\"calendar\",\"access\":\"read\"}]}）".to_string()
        } else if msg.contains("unknown field `network`") {
            "权限清单字段 `network.domains` 已更名为 `ui.connectSrc`（仅约束界面 CSP；agent 出网请声明连接器）".to_string()
        } else if let Some(rest) = msg.strip_prefix("unknown field `") {
            let name = rest.split('`').next().unwrap_or("?");
            format!("权限清单含未知字段 `{name}`，可用字段：filesystem / ui / agents / system / connectors / scheduledTasks / skills")
        } else {
            format!("权限清单解析失败：{msg}")
        }
    })
    .and_then(|p| {
        for s in p.filesystem.read.iter().chain(p.filesystem.write.iter()) {
            crate::capabilities::filesystem::validate_spec(s)?;
        }
        Ok(p)
    })
}

/// 从指定目录加载权限配置
pub fn load(dir: &Path, rel: &str) -> Result<Permissions, String> {
    let raw = std::fs::read_to_string(dir.join(rel)).map_err(|e| e.to_string())?;
    parse(&raw)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{
      "filesystem": { "read": ["$DOWNLOADS", "$APP_DATA"], "write": ["$APP_DATA"] },
      "ui": { "connectSrc": ["api.amap.com", "*.booking.com"] },
      "agents": { "call": ["@superagent/flight-search"] },
      "system": { "notifications": true, "schedule": false }
    }"#;

    #[test]
    fn parses_and_exposes_ui_connect_src_as_domains() {
        let p = parse(SAMPLE).unwrap();
        assert_eq!(
            p.domains(),
            &["api.amap.com".to_string(), "*.booking.com".to_string()]
        );
        assert!(p.system.notifications);
        assert!(!p.system.schedule);
    }

    #[test]
    fn empty_permissions_ok() {
        let p = parse("{}").unwrap();
        assert!(p.domains().is_empty());
        assert!(p.filesystem.write.is_empty());
    }

    #[test]
    fn legacy_mcp_field_is_rejected_with_pointer_to_connectors() {
        let err = parse(r#"{ "mcp": ["calendar"] }"#).unwrap_err();
        assert!(err.contains("`mcp` 已废弃"), "{err}");
        assert!(err.contains("connectors"), "{err}");
    }

    #[test]
    fn legacy_network_field_is_rejected_with_pointer_to_ui_connect_src() {
        let err = parse(r#"{ "network": { "domains": ["a.com"] } }"#).unwrap_err();
        assert!(
            err.contains("`network.domains` 已更名为 `ui.connectSrc`"),
            "{err}"
        );
    }

    #[test]
    fn unknown_field_is_rejected() {
        let err = parse(r#"{ "telepathy": true }"#).unwrap_err();
        assert!(err.contains("未知字段"), "{err}");
        assert!(err.contains("telepathy"), "{err}");
    }

    // --- P3 Task 2: connectors + system.schedule + scheduledTasks 解析 ---

    const P3_SAMPLE: &str = r#"{
      "connectors": [{"category": "filesystem", "access": "readwrite"}],
      "system": { "schedule": true },
      "scheduledTasks": [{"id": "daily-digest", "cron": "0 9 * * *", "prompt": "整理今天的待办"}]
    }"#;

    #[test]
    fn connectors_and_schedule_parse_full() {
        let p: Permissions = serde_json::from_str(P3_SAMPLE).unwrap();
        assert_eq!(p.connectors.len(), 1);
        assert_eq!(p.connectors[0].category, "filesystem");
        assert_eq!(p.connectors[0].access, Access::ReadWrite);
        assert!(p.system.schedule);
        assert_eq!(p.scheduled_tasks.len(), 1);
        assert_eq!(p.scheduled_tasks[0].id, "daily-digest");
        assert_eq!(p.scheduled_tasks[0].cron, "0 9 * * *");
        assert_eq!(p.scheduled_tasks[0].prompt, "整理今天的待办");
        assert!(p.scheduled_tasks[0].catch_up); // 省略 -> 默认 true
    }

    #[test]
    fn connector_access_defaults_to_read() {
        let json = r#"{ "connectors": [{"category": "filesystem"}] }"#;
        let p: Permissions = serde_json::from_str(json).unwrap();
        assert_eq!(p.connectors[0].access, Access::Read);
    }

    #[test]
    fn scheduled_task_catch_up_defaults_true() {
        let json = r#"{ "scheduledTasks": [{"id": "x", "cron": "* * * * *", "prompt": "p"}] }"#;
        let p: Permissions = serde_json::from_str(json).unwrap();
        assert!(p.scheduled_tasks[0].catch_up);
    }

    // --- P6-A Task 6: filesystem 路径变量校验 ---

    #[test]
    fn filesystem_specs_are_validated_at_parse() {
        assert!(parse(r#"{ "filesystem": { "read": ["$HOME"] } }"#)
            .unwrap_err()
            .contains("$HOME"));
        assert!(parse(r#"{ "filesystem": { "write": ["/tmp"] } }"#)
            .unwrap_err()
            .contains("绝对路径"));
        assert!(
            parse(r#"{ "filesystem": { "read": ["$DOWNLOADS/a"], "write": ["$APP_DATA"] } }"#)
                .is_ok()
        );
    }

    #[test]
    fn legacy_permissions_without_new_fields_still_parse() {
        // 旧 P1 权限清单不含 connectors/scheduledTasks，须仍可解析且新增字段落回默认值（向后兼容）
        let p: Permissions = serde_json::from_str(SAMPLE).unwrap();
        assert!(p.connectors.is_empty());
        assert!(p.scheduled_tasks.is_empty());
    }
}
