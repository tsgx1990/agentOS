//! P6-A 能力插件内核：一类能力 = 一个对象（声明 / 呈现 / 启动期 / 调用期 / 装卸）。
//! 见 docs/superpowers/specs/2026-09-02-p6a-capability-plugin-core-design.md §2。
use crate::mcp::McpManager;
use crate::paths::DataLayout;
use crate::permissions::Permissions;
use crate::registry::InstalledApp;
use serde_json::Value;
use std::path::{Path, PathBuf};

pub const NO_EXTRA_PERMISSIONS: &str = "仅在自己的数据区内活动，无额外权限";

/// 调用方身份——**只**从 `McpSocketListener` 绑定值或安装流程构造，从不解析 wire。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallerIdentity {
    pub app_id: String,
    pub trusted: bool,
    pub depth: u32,
}
impl CallerIdentity {
    pub fn is_router(&self) -> bool {
        self.app_id == crate::maker::MAKER_APP_ID
    }
    /// 安装/预览/诊断场景用：深度 0。
    pub fn installing(app_id: &str, trusted: bool) -> Self {
        Self {
            app_id: app_id.to_string(),
            trusted,
            depth: 0,
        }
    }
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct LaunchContribution {
    pub extra_args: Vec<String>,
    pub env: Vec<(String, String)>,
    pub bridges: Vec<&'static str>,
    pub tools: Vec<String>,
    pub sandbox_read: Vec<PathBuf>,
    pub sandbox_write: Vec<PathBuf>,
    pub needs_socket: bool,
}
impl LaunchContribution {
    fn absorb(&mut self, other: LaunchContribution) {
        self.extra_args.extend(other.extra_args);
        for (k, v) in other.env {
            if !self.env.iter().any(|(ek, _)| ek == &k) {
                self.env.push((k, v));
            }
        }
        for b in other.bridges {
            if !self.bridges.contains(&b) {
                self.bridges.push(b);
            }
        }
        for t in other.tools {
            if !self.tools.contains(&t) {
                self.tools.push(t);
            }
        }
        for p in other.sandbox_read {
            if !self.sandbox_read.contains(&p) {
                self.sandbox_read.push(p);
            }
        }
        for p in other.sandbox_write {
            if !self.sandbox_write.contains(&p) {
                self.sandbox_write.push(p);
            }
        }
        self.needs_socket |= other.needs_socket;
    }
}

#[derive(Clone, Copy)]
pub struct LaunchCtx<'a> {
    pub app_id: &'a str,
    pub trusted: bool,
    pub sandboxed: bool,
    /// F1（review）：仅三条真实 spawn 路径（`session_mgr::open_app_after_acquire`/
    /// `session_mgr::headless_contribution`）传 `true`——这时才允许 `filesystem`
    /// 能力的 `launch` 真的 `create_dir_all` 落地写目录。`describe()`（供
    /// `preview_install`/`app_capabilities` 只读诊断复用）强制传 `false`：只做
    /// 校验/包含性复核，绝不能仅仅因为用户打开了预览面板或安装对话框就在磁盘上
    /// 建出 `~/Desktop/导出` 这类目录——那是「安装」被点击之后才该发生的副作用。
    pub materialize: bool,
    pub layout: &'a DataLayout,
    pub hosttools_dir: &'a Path,
    pub socket_path: &'a Path,
    pub mcp: &'a McpManager,
}
pub struct CallCtx<'a> {
    pub layout: &'a DataLayout,
    pub mcp: &'a McpManager,
    pub hosttools_dir: Option<&'a Path>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Enforcement {
    Launch,
    Sandbox,
    HostMethod,
    InstallHook,
    UiCsp,
}

#[async_trait::async_trait]
pub trait Capability: Send + Sync {
    fn key(&self) -> &'static str;
    fn declared(&self, perms: &Permissions, identity: &CallerIdentity) -> bool;
    fn render_human(&self, perms: &Permissions) -> Vec<String>;
    fn launch(
        &self,
        perms: &Permissions,
        ctx: &LaunchCtx<'_>,
    ) -> Result<LaunchContribution, String>;
    fn methods(&self) -> &'static [&'static str] {
        &[]
    }
    async fn handle(
        &self,
        _method: &str,
        _params: Value,
        _identity: &CallerIdentity,
        _perms: &Permissions,
        _ctx: &CallCtx<'_>,
    ) -> Value {
        Value::Null
    }
    fn on_install(
        &self,
        _app: &InstalledApp,
        _perms: &Permissions,
        _layout: &DataLayout,
    ) -> Result<(), String> {
        Ok(())
    }
    fn on_uninstall(&self, _app_id: &str, _layout: &DataLayout) -> Result<(), String> {
        Ok(())
    }
    fn enforcement(&self) -> &'static [Enforcement];
    /// 特权能力（maker/router）为 true：不呈现、不参与「字段全覆盖」检查。默认 false。
    fn privileged(&self) -> bool {
        false
    }
}

#[derive(Debug, serde::Serialize)]
pub struct CapabilityReport {
    pub key: String,
    pub declared: bool,
    pub human: Vec<String>,
    pub enforcement: Vec<Enforcement>,
    pub tools: Vec<String>,
    /// F1（review）：`declared` 能力的 `launch(...)` 若报错（此前 `describe` 用
    /// `unwrap_or_default()` 吞掉，`tools` 静默变空、调用方无从得知原因），改为
    /// 把错误原样带出——`preview_install`/`app_capabilities` 的调用方（安装对话框/
    /// 诊断面板）能把真实原因呈现给用户，而不是一个看起来"这条能力没有任何工具"
    /// 的假象。serde 默认序列化 `None -> null`。
    pub error: Option<String>,
}

pub struct CapabilityRegistry {
    caps: Vec<Box<dyn Capability>>,
}
impl CapabilityRegistry {
    pub fn new(caps: Vec<Box<dyn Capability>>) -> Self {
        Self { caps }
    }
    pub fn iter(&self) -> impl Iterator<Item = &dyn Capability> {
        self.caps.iter().map(|c| c.as_ref())
    }

    pub fn render_human(&self, perms: &Permissions, identity: &CallerIdentity) -> Vec<String> {
        let mut out: Vec<String> = self
            .caps
            .iter()
            .filter(|c| !c.privileged() && c.declared(perms, identity))
            .flat_map(|c| c.render_human(perms))
            .collect();
        if out.is_empty() {
            out.push(NO_EXTRA_PERMISSIONS.to_string());
        }
        out
    }

    pub fn launch(
        &self,
        perms: &Permissions,
        identity: &CallerIdentity,
        ctx: &LaunchCtx<'_>,
    ) -> Result<LaunchContribution, String> {
        let mut acc = LaunchContribution::default();
        for c in &self.caps {
            if c.declared(perms, identity) {
                acc.absorb(
                    c.launch(perms, ctx)
                        .map_err(|e| format!("能力 {} 启动期贡献失败：{e}", c.key()))?,
                );
            }
        }
        Ok(acc)
    }

    /// 找到认领 `method` 的能力：未声明 → 拒绝（调用方负责审计）；无人认领 → 明确错误。
    pub async fn dispatch(
        &self,
        method: &str,
        params: Value,
        identity: &CallerIdentity,
        perms: &Permissions,
        ctx: &CallCtx<'_>,
    ) -> Value {
        match self.caps.iter().find(|c| c.methods().contains(&method)) {
            None => serde_json::json!({ "ok": false, "error": format!("未知的宿主方法 {method}") }),
            Some(c) if !c.declared(perms, identity) => serde_json::json!({
                "ok": false, "error": format!("unauthorized: 该应用未声明能力 {}", c.key()),
            }),
            Some(c) => c.handle(method, params, identity, perms, ctx).await,
        }
    }

    pub fn on_install(
        &self,
        app: &InstalledApp,
        perms: &Permissions,
        layout: &DataLayout,
    ) -> Result<(), String> {
        let id = CallerIdentity::installing(&app.app_id, app.trusted);
        for c in &self.caps {
            if c.declared(perms, &id) {
                c.on_install(app, perms, layout)
                    .map_err(|e| format!("能力 {} 安装钩子失败：{e}", c.key()))?;
            }
        }
        Ok(())
    }

    pub fn on_uninstall(&self, app_id: &str, layout: &DataLayout) -> Vec<String> {
        self.caps
            .iter()
            .filter_map(|c| {
                c.on_uninstall(app_id, layout)
                    .err()
                    .map(|e| format!("{}: {e}", c.key()))
            })
            .collect()
    }

    /// F1（review）：无论调用方传的 `ctx.materialize` 是什么，`describe` 一律在本地
    /// 强制改成 `false` 再喂给每个能力的 `launch`——诊断/预览是只读操作，绝不能因为
    /// 走了这条路径就让 `filesystem` 能力的 `launch` 真的 `create_dir_all`。
    /// `LaunchCtx` 全部字段都是 `Copy`（引用 + `bool`），`LaunchCtx { materialize:
    /// false, ..*ctx }` 是一次纯值拷贝，不借用 `ctx` 之外的任何东西。
    pub fn describe(
        &self,
        perms: &Permissions,
        identity: &CallerIdentity,
        ctx: &LaunchCtx<'_>,
    ) -> Vec<CapabilityReport> {
        let ctx = LaunchCtx {
            materialize: false,
            ..*ctx
        };
        self.caps
            .iter()
            .map(|c| {
                let declared = c.declared(perms, identity);
                let (tools, error) = if declared {
                    match c.launch(perms, &ctx) {
                        Ok(l) => (l.tools, None),
                        Err(e) => (vec![], Some(e)),
                    }
                } else {
                    (vec![], None)
                };
                CapabilityReport {
                    key: c.key().to_string(),
                    declared,
                    human: if declared {
                        c.render_human(perms)
                    } else {
                        vec![]
                    },
                    enforcement: c.enforcement().to_vec(),
                    tools,
                    error,
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fake {
        key: &'static str,
        declared: bool,
        tools: Vec<&'static str>,
        bridge: &'static str,
        methods: &'static [&'static str],
    }

    #[async_trait::async_trait]
    impl Capability for Fake {
        fn key(&self) -> &'static str {
            self.key
        }
        fn declared(&self, _p: &Permissions, _i: &CallerIdentity) -> bool {
            self.declared
        }
        fn render_human(&self, _p: &Permissions) -> Vec<String> {
            vec![format!("人话:{}", self.key)]
        }
        fn launch(
            &self,
            _p: &Permissions,
            _c: &LaunchCtx<'_>,
        ) -> Result<LaunchContribution, String> {
            Ok(LaunchContribution {
                bridges: vec![self.bridge],
                tools: self.tools.iter().map(|s| s.to_string()).collect(),
                env: vec![("SHARED".to_string(), "1".to_string())],
                needs_socket: true,
                ..Default::default()
            })
        }
        fn methods(&self) -> &'static [&'static str] {
            self.methods
        }
        async fn handle(
            &self,
            method: &str,
            _p: Value,
            id: &CallerIdentity,
            _perms: &Permissions,
            _c: &CallCtx<'_>,
        ) -> Value {
            serde_json::json!({ "handled": method, "by": id.app_id })
        }
        fn enforcement(&self) -> &'static [Enforcement] {
            &[Enforcement::Launch, Enforcement::HostMethod]
        }
    }

    fn ctx_parts() -> (DataLayout, McpManager, tempfile::TempDir) {
        let tmp = tempfile::tempdir().unwrap();
        (
            DataLayout::new(tmp.path().to_path_buf()),
            McpManager::new(),
            tmp,
        )
    }

    fn reg() -> CapabilityRegistry {
        CapabilityRegistry::new(vec![
            Box::new(Fake {
                key: "a",
                declared: true,
                tools: vec!["t1", "shared"],
                bridge: "a.ts",
                methods: &["__host_a__"],
            }),
            Box::new(Fake {
                key: "b",
                declared: true,
                tools: vec!["t2", "shared"],
                bridge: "b.ts",
                methods: &["__host_b__"],
            }),
            Box::new(Fake {
                key: "c",
                declared: false,
                tools: vec!["t3"],
                bridge: "c.ts",
                methods: &["__host_c__"],
            }),
        ])
    }

    #[test]
    fn launch_merges_only_declared_and_dedups_tools_and_bridges() {
        let (layout, mcp, _t) = ctx_parts();
        let hosttools = Path::new("/ht");
        let sock = Path::new("/sock");
        let ctx = LaunchCtx {
            app_id: "x",
            trusted: false,
            sandboxed: true,
            materialize: true,
            layout: &layout,
            hosttools_dir: hosttools,
            socket_path: sock,
            mcp: &mcp,
        };
        let id = CallerIdentity::installing("x", false);
        let c = reg().launch(&Permissions::default(), &id, &ctx).unwrap();
        assert_eq!(c.bridges, vec!["a.ts", "b.ts"]); // c 未声明不贡献
        assert_eq!(c.tools, vec!["t1", "shared", "t2"]); // 去重保序
        assert_eq!(c.env.len(), 1); // a、b 都贡献同一个 SHARED key，按 key 去重只留第一个
        assert!(c.needs_socket);
    }

    #[test]
    fn render_human_concatenates_declared_and_falls_back_when_empty() {
        let id = CallerIdentity::installing("x", false);
        let lines = reg().render_human(&Permissions::default(), &id);
        assert_eq!(lines, vec!["人话:a", "人话:b"]);
        let none = CapabilityRegistry::new(vec![Box::new(Fake {
            key: "z",
            declared: false,
            tools: vec![],
            bridge: "z.ts",
            methods: &["__host_z__"],
        })]);
        assert_eq!(
            none.render_human(&Permissions::default(), &id),
            vec![NO_EXTRA_PERMISSIONS.to_string()]
        );
    }

    #[tokio::test]
    async fn dispatch_routes_declared_denies_undeclared_and_rejects_unknown() {
        let (layout, mcp, _t) = ctx_parts();
        let ctx = CallCtx {
            layout: &layout,
            mcp: &mcp,
            hosttools_dir: None,
        };
        let id = CallerIdentity {
            app_id: "x".into(),
            trusted: false,
            depth: 0,
        };
        let r = reg();
        let ok = r
            .dispatch(
                "__host_b__",
                Value::Null,
                &id,
                &Permissions::default(),
                &ctx,
            )
            .await;
        assert_eq!(ok["handled"], "__host_b__");
        let denied = r
            .dispatch(
                "__host_c__",
                Value::Null,
                &id,
                &Permissions::default(),
                &ctx,
            )
            .await;
        assert_eq!(denied["ok"], false);
        assert!(denied["error"].as_str().unwrap().contains("未声明能力 c"));
        let unknown = r
            .dispatch(
                "__host_nope__",
                Value::Null,
                &id,
                &Permissions::default(),
                &ctx,
            )
            .await;
        assert!(unknown["error"]
            .as_str()
            .unwrap()
            .contains("未知的宿主方法"));
    }

    #[test]
    fn describe_reports_every_capability_with_declared_flag() {
        let (layout, mcp, _t) = ctx_parts();
        let ctx = LaunchCtx {
            app_id: "x",
            trusted: false,
            sandboxed: true,
            materialize: true,
            layout: &layout,
            hosttools_dir: Path::new("/ht"),
            socket_path: Path::new("/s"),
            mcp: &mcp,
        };
        let id = CallerIdentity::installing("x", false);
        let reports = reg().describe(&Permissions::default(), &id, &ctx);
        assert_eq!(reports.len(), 3);
        assert!(!reports.iter().find(|r| r.key == "c").unwrap().declared);
        assert_eq!(
            reports.iter().find(|r| r.key == "a").unwrap().tools,
            vec!["t1", "shared"]
        );
        assert!(reports.iter().all(|r| r.error.is_none()), "{reports:?}");
    }

    /// F1（review）：`declared` 为真但 `launch` 报错的能力——`describe` 不得
    /// `unwrap_or_default()` 吞掉这个错误（此前 `tools` 会静默变空，调用方无从
    /// 得知原因）；也不能 panic。
    struct FailingLaunch;
    #[async_trait::async_trait]
    impl Capability for FailingLaunch {
        fn key(&self) -> &'static str {
            "boom"
        }
        fn declared(&self, _p: &Permissions, _i: &CallerIdentity) -> bool {
            true
        }
        fn render_human(&self, _p: &Permissions) -> Vec<String> {
            vec!["人话:boom".into()]
        }
        fn launch(
            &self,
            _p: &Permissions,
            _c: &LaunchCtx<'_>,
        ) -> Result<LaunchContribution, String> {
            Err("boom".to_string())
        }
        fn enforcement(&self) -> &'static [Enforcement] {
            &[Enforcement::Launch]
        }
    }

    #[test]
    fn describe_captures_launch_error_instead_of_swallowing_it() {
        let (layout, mcp, _t) = ctx_parts();
        let ctx = LaunchCtx {
            app_id: "x",
            trusted: false,
            sandboxed: true,
            materialize: true,
            layout: &layout,
            hosttools_dir: Path::new("/ht"),
            socket_path: Path::new("/s"),
            mcp: &mcp,
        };
        let id = CallerIdentity::installing("x", false);
        let reg = CapabilityRegistry::new(vec![Box::new(FailingLaunch)]);
        let reports = reg.describe(&Permissions::default(), &id, &ctx);
        assert_eq!(reports.len(), 1);
        assert!(reports[0].declared);
        assert_eq!(reports[0].error, Some("boom".to_string()));
        assert!(reports[0].tools.is_empty());
    }
}
