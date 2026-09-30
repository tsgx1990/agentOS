//! system.notifications 能力：应用经 `__host_notify__` 向通知中心投递 `app_notice`。
//! 未声明 → 注册表拒绝；声明 → 截断 + 每应用每分钟 ≤ MAX_PER_MINUTE 条。
use crate::capability::*;
use crate::permissions::Permissions;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub struct NotificationsCapability {
    window: Mutex<HashMap<String, Vec<Instant>>>,
}
impl Default for NotificationsCapability {
    fn default() -> Self {
        Self {
            window: Mutex::new(HashMap::new()),
        }
    }
}
impl NotificationsCapability {
    pub fn new() -> Self {
        Self::default()
    }
}
pub const NOTIFY_METHOD: &str = "__host_notify__";
pub const NOTICE_KIND: &str = "app_notice";
pub const MAX_PER_MINUTE: usize = 10;
const TITLE_MAX: usize = 200;
const BODY_MAX: usize = 2000;

fn truncate(s: &str, max: usize) -> String {
    s.chars().take(max).collect()
}

#[async_trait::async_trait]
impl Capability for NotificationsCapability {
    fn key(&self) -> &'static str {
        "system.notifications"
    }
    fn declared(&self, p: &Permissions, _i: &CallerIdentity) -> bool {
        p.system.notifications
    }
    fn render_human(&self, _p: &Permissions) -> Vec<String> {
        vec!["向你发送通知".to_string()]
    }
    fn launch(&self, _p: &Permissions, ctx: &LaunchCtx<'_>) -> Result<LaunchContribution, String> {
        Ok(LaunchContribution {
            env: vec![(
                super::SOCKET_ENV.to_string(),
                ctx.socket_path.to_string_lossy().to_string(),
            )],
            bridges: vec!["notify_bridge.ts"],
            tools: vec![NOTIFY_METHOD.to_string()],
            needs_socket: true,
            ..Default::default()
        })
    }
    fn methods(&self) -> &'static [&'static str] {
        &[NOTIFY_METHOD]
    }
    async fn handle(
        &self,
        _m: &str,
        params: Value,
        id: &CallerIdentity,
        _p: &Permissions,
        ctx: &CallCtx<'_>,
    ) -> Value {
        let title = truncate(
            params
                .get("title")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim(),
            TITLE_MAX,
        );
        let body = truncate(
            params.get("body").and_then(|v| v.as_str()).unwrap_or(""),
            BODY_MAX,
        );
        if title.is_empty() {
            return serde_json::json!({ "ok": false, "error": "title 不能为空" });
        }
        {
            let mut w = self.window.lock().unwrap();
            let now = Instant::now();
            let hits = w.entry(id.app_id.clone()).or_default();
            hits.retain(|t| now.duration_since(*t) < Duration::from_secs(60));
            if hits.len() >= MAX_PER_MINUTE {
                return serde_json::json!({ "ok": false, "error": "rate_limited" });
            }
            hits.push(now);
        }
        let store =
            crate::notifications::NotificationStore::new(ctx.layout.clone(), ctx.mcp.clone());
        match store.add(NOTICE_KIND, &id.app_id, &title, &body) {
            Ok(n) => serde_json::json!({ "ok": true, "id": n.id }),
            Err(e) => serde_json::json!({ "ok": false, "error": format!("通知写入失败：{e}") }),
        }
    }
    fn enforcement(&self) -> &'static [Enforcement] {
        &[Enforcement::Launch, Enforcement::HostMethod]
    }
}
