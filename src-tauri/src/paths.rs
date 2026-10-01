use std::path::PathBuf;

/// `#[derive(Clone)]`（Task9b）：唯一字段是 `PathBuf`，克隆只是拷贝一段路径
/// 字符串。`mcp_socket::McpSocketListener::start` 需要一份可以移进长期存活的
/// `tokio::spawn` accept 循环里的持有型 `DataLayout`（供 `host_mcp_call` 落
/// 审计记录用），调用方（`session_mgr::open_app_after_acquire`）手头的
/// `layout` 后面还要接着用，只能 `.clone()` 一份出去，不能移交所有权。
#[derive(Clone)]
pub struct DataLayout {
    root: PathBuf,
}

impl DataLayout {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    pub fn session_dir(&self, app_id: &str) -> PathBuf {
        self.root.join("sessions").join(app_id)
    }

    pub fn audit_dir(&self) -> PathBuf {
        self.root.join("audit")
    }

    pub fn ensure(&self, app_id: &str) -> std::io::Result<PathBuf> {
        let sdir = self.session_dir(app_id);
        std::fs::create_dir_all(&sdir)?;
        std::fs::create_dir_all(self.audit_dir())?;
        Ok(sdir)
    }

    /// 返回应用程序包目录路径: root/packages/<app_id>
    pub fn packages_dir(&self, app_id: &str) -> PathBuf {
        self.root.join("packages").join(app_id)
    }

    /// 返回应用程序数据目录路径: root/apps/<app_id>
    pub fn app_data_dir(&self, app_id: &str) -> PathBuf {
        self.root.join("apps").join(app_id)
    }

    /// 返回应用程序状态文件路径: root/state/<app_id>.json
    pub fn state_path(&self, app_id: &str) -> PathBuf {
        self.root.join("state").join(format!("{app_id}.json"))
    }

    /// 返回全局注册表文件路径: root/registry.json
    pub fn registry_path(&self) -> PathBuf {
        self.root.join("registry.json")
    }

    /// 返回应用程序的 pi agent home 目录路径: root/agenthome/<app_id>
    /// （对应 spawn_with 时设置的 PI_CODING_AGENT_DIR 环境变量）
    pub fn agent_home_dir(&self, app_id: &str) -> PathBuf {
        self.root.join("agenthome").join(app_id)
    }

    /// 返回该应用 MCP 桥接的宿主监听 socket 路径: root/mcp/<app_id>/mcp.sock
    /// （P6-A：`session_mgr::assemble_launch_plan` 把它经 `SUPERAGENT_MCP_SOCKET`
    /// 注入给该 app 的 pi 子进程——由 `CapabilityRegistry::launch` 算出的
    /// `LaunchContribution.env` 携带这一条，见 `capabilities::connectors`/
    /// `notifications`/`agents_call`/`maker`/`router` 各自的 `launch` 实现——供
    /// `mcp_transport.ts` 的 `hostMcpCall` 连接。真正在这个路径上 `bind`/`listen`
    /// 的宿主端是 `mcp_socket::McpSocketListener`，本函数只负责推导路径，不创建
    /// 目录、不 bind。）
    pub fn mcp_socket_path(&self, app_id: &str) -> PathBuf {
        self.root.join("mcp").join(app_id).join("mcp.sock")
    }

    /// 返回定时任务注册表持久化文件路径: root/scheduler/tasks.json
    /// （Task10：`scheduler::TaskRegistry` 落盘的整份 `Vec<RegisteredTask>`，
    /// host-global 单文件——不是 per-app，与 `registry_path()` 已装应用索引
    /// 同一套「整份 JSON + 原子写」模式，只是记录内容不同。）
    pub fn scheduler_tasks_path(&self) -> PathBuf {
        self.root.join("scheduler").join("tasks.json")
    }

    /// 返回通知中心按日滚动的日志目录: root/notifications
    /// （Task15：`notifications::NotificationStore` 落盘 `<date>.jsonl`，host-global
    /// ——不是 per-app（每条记录用 `app_id` 字段标出归属），与 `audit_dir()` 同一套
    /// 「按日滚动 + size cap + 保留期」治理模式，只是记录内容/schema 不同。）
    pub fn notifications_dir(&self) -> PathBuf {
        self.root.join("notifications")
    }

    /// 返回审批中心持久化目录: root/approvals（P6-C）——`approvals::ApprovalStore`
    /// 落盘 `staged.json`（暂存待批的写调用）与 `rules.json`（"总是允许"放行
    /// 规则）两个整份 JSON 文件，host-global（不是 per-app，每条记录自带
    /// `app_id` 标出归属），同 `scheduler_tasks_path()` 的「读全量 -> 内存改 ->
    /// 写 `.tmp` -> `rename`」原子替换模式。
    pub fn approvals_dir(&self) -> PathBuf {
        self.root.join("approvals")
    }

    /// 返回 maker 暂存区根目录路径: root/maker-staging
    /// （Task1（P4）：Maker subagent 生成应用到暂存区，所有草稿共用的根目录。）
    pub fn maker_staging_root(&self) -> PathBuf {
        self.root.join("maker-staging")
    }

    /// 返回某草稿的 maker 暂存目录路径: root/maker-staging/<draft_id>
    /// （Task1（P4）：Maker subagent 为每个草稿生成独立暂存目录。）
    pub fn maker_staging_dir(&self, draft_id: &str) -> PathBuf {
        self.maker_staging_root().join(draft_id)
    }

    /// 返回一次 agent 间调用（call_agent）临时会话专用的**调用域** unix socket
    /// 路径: `root/callbus/<nonce>/call.sock`（P5 §1.4）。
    ///
    /// 与 per-app 的 `mcp_socket_path`（按 app_id 确定性推导、由 `open_app` 以
    /// depth=0 绑定）刻意分开：被调方临时会话需要一个**绑定了正确嵌套深度**的
    /// 监听器，且不能复用被调方 per-app socket——那条可能正被被调方前台交互会话
    /// 占用，且 `McpSocketListener::start` "先删旧文件再 bind" 会误删前台监听器
    /// 的 socket 文件。`nonce` 由 `session_mgr::spawn_call_session` 用单调原子计数器
    /// 生成（`<callee_app_id>-d<depth>-<seq>`，非时间来源、并发唯一——同一 callee 同一
    /// depth 被并发调起也不会撞同一 socket 文件），会话结束即 `stop()` + 清目录。
    pub fn call_socket_path(&self, nonce: &str) -> PathBuf {
        self.root.join("callbus").join(nonce).join("call.sock")
    }

    /// 发布产出根目录: `root/published/`——`publish::export_package` 把包导到这里，
    /// `publish::merge_index_entry` 在这里维护一份 `index.json`；整个目录就是"可以
    /// 直接推到 GitHub 精选市场仓库"的东西（真实推送是手工里程碑）。
    pub fn published_dir(&self) -> PathBuf {
        self.root.join("published")
    }

    /// 某已发布应用的导出目录: `root/published/<app_id>/`。
    pub fn published_app_dir(&self, app_id: &str) -> PathBuf {
        self.published_dir().join(app_id)
    }

    /// 技能库根目录: `root/skills`（P6-B）——`skills::SkillStore::install_from_dir`
    /// 把已装技能的目录复制到这下面，每个技能一个子目录（`skill_dir`）。
    pub fn skills_root(&self) -> PathBuf {
        self.root.join("skills")
    }

    /// 某个已装技能的目录: `root/skills/<id>/`——`id` 是 frontmatter `name` 经
    /// `skills::validate_name` NFKC 归一化后的值（spec 裁决 1：id 用 name，不强制
    /// 与来源目录同名）。
    pub fn skill_dir(&self, id: &str) -> PathBuf {
        self.skills_root().join(id)
    }

    /// 技能清单 + 授予表持久化文件: `root/skills-index.json`（host-global 单文件，
    /// 同 `scheduler_tasks_path()`/`approvals_dir()` 的"整份 JSON + 原子写"模式）。
    pub fn skills_index_path(&self) -> PathBuf {
        self.root.join("skills-index.json")
    }

    /// 自定义 provider 配置: `root/providers.json`（host-global 单文件，只存非密钥配置，
    /// 密钥在系统钥匙串；同 `skills_index_path()` 的"整份 JSON + 原子写"模式）。
    pub fn providers_path(&self) -> PathBuf {
        self.root.join("providers.json")
    }

    /// 全局默认模型 / 应用级模型覆盖: `root/model-overrides.json`（host-global 单文件，
    /// 同 `skills_index_path()` 的"整份 JSON + 原子写"模式）。
    pub fn model_overrides_path(&self) -> PathBuf {
        self.root.join("model-overrides.json")
    }

    /// 取得该应用的一个「应用可写」私有目录（`kind` 为 `apps` / `agenthome` / `sessions`
    /// 之一），返回**已规范化的字面路径**：`<canonical(root)>/<kind>/<app_id>`。
    ///
    /// 这三个目录都在沙盒里对应用可写，应用可以把其中某个目录改名、再放一个指向别处的
    /// 符号链接顶替。宿主在沙盒外面对这些路径做任何写操作、或把它们授权给下一次沙盒，
    /// 都必须先经过这里：
    /// - 已存在但不是真目录（符号链接、普通文件）→ Err，绝不跟随；
    /// - 不存在才创建；
    /// - 最后确认 `canonicalize == 预期字面路径`，不等 → Err。
    ///
    /// 数据根本身由宿主控制（应用不可写），每次在这里规范化一次，保证返回的路径可直接
    /// 交给 `sandbox::build_profile`（它要求可写路径「规范化后等于自身」）。
    pub fn private_dir(&self, kind: &str, app_id: &str) -> Result<PathBuf, String> {
        if !matches!(kind, "apps" | "agenthome" | "sessions") {
            return Err(format!("未知的私有目录类别：{kind}"));
        }
        if app_id.is_empty()
            || app_id == "."
            || app_id == ".."
            || app_id.contains(['/', '\\', '\0'])
        {
            return Err(format!("非法的应用 id：{app_id:?}"));
        }
        std::fs::create_dir_all(&self.root).map_err(|e| format!("无法创建数据根：{e}"))?;
        let root =
            std::fs::canonicalize(&self.root).map_err(|e| format!("无法规范化数据根：{e}"))?;
        let parent = root.join(kind);
        std::fs::create_dir_all(&parent).map_err(|e| e.to_string())?;
        let expected = parent.join(app_id);
        let not_real = |what: &str| {
            format!(
                "私有目录不是真实目录（{what}，可能被替换成了符号链接或文件），拒绝使用：{}",
                expected.display()
            )
        };
        match std::fs::symlink_metadata(&expected) {
            Ok(m) if m.file_type().is_dir() => {}
            Ok(m) if m.file_type().is_symlink() => return Err(not_real("符号链接")),
            Ok(_) => return Err(not_real("非目录")),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                match std::fs::create_dir(&expected) {
                    Ok(()) => {}
                    // 并发创建：再确认一次它是真目录。
                    Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                        let m = std::fs::symlink_metadata(&expected).map_err(|e| e.to_string())?;
                        if !m.file_type().is_dir() {
                            return Err(not_real("非真实目录"));
                        }
                    }
                    Err(e) => return Err(e.to_string()),
                }
            }
            Err(e) => return Err(e.to_string()),
        }
        let canon = std::fs::canonicalize(&expected).map_err(|e| e.to_string())?;
        if canon != expected {
            return Err(not_real("规范化后路径不一致"));
        }
        Ok(expected)
    }

    /// 创建应用程序所需的目录结构
    /// 包括: apps/<app_id>, sessions/<app_id>（经 `private_dir`，不跟随链接）, state 父目录
    pub fn ensure_app(&self, app_id: &str) -> std::io::Result<()> {
        self.private_dir("apps", app_id)
            .map_err(std::io::Error::other)?;
        self.private_dir("sessions", app_id)
            .map_err(std::io::Error::other)?;
        if let Some(p) = self.state_path(app_id).parent() {
            std::fs::create_dir_all(p)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn session_dir_is_per_app() {
        let layout = DataLayout::new(std::path::PathBuf::from("/data"));
        assert_eq!(
            layout.session_dir("main"),
            std::path::PathBuf::from("/data/sessions/main")
        );
        assert_eq!(layout.audit_dir(), std::path::PathBuf::from("/data/audit"));
    }

    #[test]
    fn ensure_creates_dirs() {
        let tmp = tempdir().unwrap();
        let layout = DataLayout::new(tmp.path().to_path_buf());
        let sdir = layout.ensure("main").unwrap();
        assert!(sdir.is_dir());
        assert!(layout.audit_dir().is_dir());
    }

    #[test]
    fn call_socket_path_is_per_nonce_under_callbus() {
        let l = DataLayout::new(std::path::PathBuf::from("/data"));
        assert_eq!(
            l.call_socket_path("superagent__summarizer-d1"),
            std::path::PathBuf::from("/data/callbus/superagent__summarizer-d1/call.sock")
        );
        // 不同 nonce → 不同目录（不会相互覆盖 socket 文件）
        assert_ne!(
            l.call_socket_path("a-d1").parent(),
            l.call_socket_path("b-d1").parent()
        );
    }

    #[test]
    fn published_paths_shapes() {
        let l = DataLayout::new(std::path::PathBuf::from("/data"));
        assert_eq!(
            l.published_dir(),
            std::path::PathBuf::from("/data/published")
        );
        assert_eq!(
            l.published_app_dir("superagent__researcher"),
            std::path::PathBuf::from("/data/published/superagent__researcher")
        );
    }

    #[test]
    fn p1_paths_shapes() {
        let l = DataLayout::new(std::path::PathBuf::from("/data"));
        assert_eq!(
            l.packages_dir("superagent__todo-notes"),
            std::path::PathBuf::from("/data/packages/superagent__todo-notes")
        );
        assert_eq!(
            l.app_data_dir("superagent__todo-notes"),
            std::path::PathBuf::from("/data/apps/superagent__todo-notes")
        );
        assert_eq!(
            l.state_path("superagent__todo-notes"),
            std::path::PathBuf::from("/data/state/superagent__todo-notes.json")
        );
        assert_eq!(
            l.registry_path(),
            std::path::PathBuf::from("/data/registry.json")
        );
    }

    #[test]
    fn ensure_app_creates_dirs() {
        let tmp = tempdir().unwrap();
        let l = DataLayout::new(tmp.path().to_path_buf());
        l.ensure_app("app1").unwrap();
        assert!(l.app_data_dir("app1").is_dir());
        assert!(l.session_dir("app1").is_dir());
        assert!(l.state_path("app1").parent().unwrap().is_dir());
    }

    #[test]
    fn scheduler_tasks_path_is_deterministic_and_host_global() {
        let l = DataLayout::new(std::path::PathBuf::from("/data"));
        assert_eq!(
            l.scheduler_tasks_path(),
            std::path::PathBuf::from("/data/scheduler/tasks.json")
        );
        // 不含 app_id 分量——host-global 单文件，多次调用同一结果。
        assert_eq!(l.scheduler_tasks_path(), l.scheduler_tasks_path());
    }

    #[test]
    fn mcp_socket_path_is_per_app_and_deterministic() {
        let l = DataLayout::new(std::path::PathBuf::from("/data"));
        assert_eq!(
            l.mcp_socket_path("superagent__todo-notes"),
            std::path::PathBuf::from("/data/mcp/superagent__todo-notes/mcp.sock")
        );
        // 同一个 app_id 两次调用必须给出同一个路径（确定性，不含随机成分）。
        assert_eq!(l.mcp_socket_path("app1"), l.mcp_socket_path("app1"));
    }

    #[test]
    fn notifications_dir_is_host_global_and_deterministic() {
        let l = DataLayout::new(std::path::PathBuf::from("/data"));
        assert_eq!(
            l.notifications_dir(),
            std::path::PathBuf::from("/data/notifications")
        );
        assert_eq!(l.notifications_dir(), l.notifications_dir());
    }

    #[test]
    fn approvals_dir_is_host_global_and_deterministic() {
        let l = DataLayout::new(std::path::PathBuf::from("/data"));
        assert_eq!(
            l.approvals_dir(),
            std::path::PathBuf::from("/data/approvals")
        );
        assert_eq!(l.approvals_dir(), l.approvals_dir());
    }

    #[test]
    fn maker_staging_root_is_host_global() {
        let l = DataLayout::new(std::path::PathBuf::from("/data"));
        assert_eq!(
            l.maker_staging_root(),
            std::path::PathBuf::from("/data/maker-staging")
        );
        assert_eq!(l.maker_staging_root(), l.maker_staging_root());
    }

    #[test]
    fn maker_staging_dir_is_per_draft_and_under_root() {
        let l = DataLayout::new(std::path::PathBuf::from("/data"));
        let staging_dir = l.maker_staging_dir("draft-abc");
        // Check it ends with maker-staging/draft-abc
        assert!(staging_dir.ends_with("maker-staging/draft-abc"));
        // Check it is under data root
        assert_eq!(
            staging_dir,
            std::path::PathBuf::from("/data/maker-staging/draft-abc")
        );
    }

    #[test]
    fn maker_staging_dir_different_drafts_are_different() {
        let l = DataLayout::new(std::path::PathBuf::from("/data"));
        let dir1 = l.maker_staging_dir("draft-abc");
        let dir2 = l.maker_staging_dir("draft-xyz");
        assert_ne!(dir1, dir2);
    }

    #[test]
    fn skills_paths_shapes() {
        let l = DataLayout::new(std::path::PathBuf::from("/data"));
        assert_eq!(l.skills_root(), std::path::PathBuf::from("/data/skills"));
        assert_eq!(
            l.skill_dir("good-skill"),
            std::path::PathBuf::from("/data/skills/good-skill")
        );
        assert_eq!(
            l.skills_index_path(),
            std::path::PathBuf::from("/data/skills-index.json")
        );
        // skill_dir 建在 skills_root 之下（同 maker_staging_dir 的分层约束）。
        assert_eq!(l.skill_dir("x"), l.skills_root().join("x"));
    }

    #[test]
    fn maker_staging_dir_built_on_maker_staging_root() {
        let l = DataLayout::new(std::path::PathBuf::from("/data"));
        let staging_root = l.maker_staging_root();
        let staging_dir = l.maker_staging_dir("draft-abc");
        // The staging_dir should be staging_root joined with the draft_id
        assert_eq!(staging_dir, staging_root.join("draft-abc"));
    }

    // --- private_dir（C1 宿主侧收口） ---------------------------------------

    #[test]
    fn private_dir_creates_real_dir_and_returns_canonical_literal() {
        let tmp = tempdir().unwrap();
        let l = DataLayout::new(tmp.path().to_path_buf());
        for kind in ["apps", "agenthome", "sessions"] {
            let p = l.private_dir(kind, "a").unwrap();
            assert!(p.is_dir());
            assert_eq!(std::fs::canonicalize(&p).unwrap(), p);
            assert!(p.ends_with(format!("{kind}/a")));
        }
        // 幂等
        assert!(l.private_dir("apps", "a").is_ok());
    }

    #[test]
    fn private_dir_rejects_symlink_and_file_without_touching_target() {
        let tmp = tempdir().unwrap();
        let l = DataLayout::new(tmp.path().to_path_buf());
        let victim = tmp.path().join("victim");
        std::fs::create_dir_all(&victim).unwrap();
        for kind in ["apps", "agenthome", "sessions"] {
            std::fs::create_dir_all(tmp.path().join(kind)).unwrap();
            std::os::unix::fs::symlink(&victim, tmp.path().join(kind).join("a")).unwrap();
            let e = l.private_dir(kind, "a").unwrap_err();
            assert!(e.contains("符号链接"), "{e}");
            // 普通文件
            std::fs::write(tmp.path().join(kind).join("f"), b"x").unwrap();
            assert!(l.private_dir(kind, "f").is_err());
        }
        assert_eq!(std::fs::read_dir(&victim).unwrap().count(), 0);
    }

    #[test]
    fn private_dir_rejects_bad_kind_and_app_id() {
        let tmp = tempdir().unwrap();
        let l = DataLayout::new(tmp.path().to_path_buf());
        assert!(l.private_dir("packages", "a").is_err());
        for bad in ["", ".", "..", "a/b", "../x"] {
            assert!(l.private_dir("apps", bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn ensure_app_does_not_follow_swapped_link() {
        let tmp = tempdir().unwrap();
        let l = DataLayout::new(tmp.path().to_path_buf());
        let victim = tmp.path().join("victim");
        std::fs::create_dir_all(&victim).unwrap();
        std::fs::create_dir_all(tmp.path().join("sessions")).unwrap();
        std::os::unix::fs::symlink(&victim, tmp.path().join("sessions/a")).unwrap();
        assert!(l.ensure_app("a").is_err());
    }
}
