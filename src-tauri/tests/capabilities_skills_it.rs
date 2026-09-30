//! P6-B Task 3：能力 `skills`（清单字段 `skills.allow`、`launch()` 贡献
//! `--skill <dir>`、所有应用的启动基线都带 `--no-skills`、卸载钩子清空该应用的
//! 全部技能授予）。spec §4/§8，计划 Task 3 Step 1。

use std::path::{Path, PathBuf};
use super_agent_os::capabilities;
use super_agent_os::capability::{CallerIdentity, LaunchCtx};
use super_agent_os::mcp::McpManager;
use super_agent_os::paths::DataLayout;
use super_agent_os::permissions::{parse, Permissions};
use super_agent_os::registry::InstalledApp;
use super_agent_os::session_mgr::assemble_launch_plan;
use super_agent_os::skills::{SkillSource, SkillSourceKind, SkillStore};
use super_agent_os::{app_tool_set_core, known_tools};

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/skills")
        .join(name)
}

fn local_source() -> SkillSource {
    SkillSource {
        kind: SkillSourceKind::Local,
        url: None,
        sha256: None,
    }
}

fn fake_app(id: &str, trusted: bool) -> InstalledApp {
    InstalledApp {
        app_id: id.into(),
        name: id.into(),
        version: "1.0.0".into(),
        display_name: id.into(),
        category: "life".into(),
        icon: None,
        trusted,
        domains: vec![],
    }
}

fn manifest_min() -> super_agent_os::pkg::Manifest {
    serde_json::from_value(serde_json::json!({
        "name": "@t/x", "version": "1.0.0", "keywords": ["superagent-app"],
        "engines": {"superagent-host": ">=1.0 <2.0"},
        "superagent": {"schemaVersion": 1, "displayName": "x", "category": "life",
                        "ui": "ui/index.html", "permissions": "permissions.json"}
    }))
    .unwrap()
}

fn launch_ctx<'a>(
    layout: &'a DataLayout,
    mcp: &'a McpManager,
    app_id: &'a str,
    sock: &'a Path,
    trusted: bool,
    materialize: bool,
) -> LaunchCtx<'a> {
    LaunchCtx {
        app_id,
        trusted,
        sandboxed: true,
        materialize,
        layout,
        hosttools_dir: Path::new("/ht"),
        socket_path: sock,
        mcp,
    }
}

// ---------------------------------------------------------------------------
// permissions.rs：`skills.allow` 解析与默认值
// ---------------------------------------------------------------------------

#[test]
fn permissions_v2_parses_skills_allow_and_defaults_false() {
    assert!(
        !parse("{}").unwrap().skills.allow,
        "省略 skills 字段应默认 false"
    );
    let p = parse(r#"{ "skills": { "allow": true } }"#).unwrap();
    assert!(p.skills.allow);
}

// ---------------------------------------------------------------------------
// --no-skills 基线：所有应用无条件带上，不含 --skill 因为未声明能力
// ---------------------------------------------------------------------------

#[test]
fn undeclared_app_gets_no_skill_args_but_always_no_skills() {
    let tmp = tempfile::tempdir().unwrap();
    let layout = DataLayout::new(tmp.path().to_path_buf());
    let mcp = McpManager::new();
    let sock = layout.mcp_socket_path("app-a");
    let ctx = launch_ctx(&layout, &mcp, "app-a", &sock, false, true);

    let perms = Permissions::default(); // skills.allow == false（未声明）
    let contribution = capabilities::builtin()
        .launch(&perms, &CallerIdentity::installing("app-a", false), &ctx)
        .expect("默认权限下不应出错");
    assert!(
        !contribution.extra_args.iter().any(|a| a == "--skill"),
        "未声明 skills 能力不应贡献任何 --skill：{:?}",
        contribution.extra_args
    );

    let plan = assemble_launch_plan(
        &fake_app("app-a", false),
        &manifest_min(),
        &contribution,
        &layout,
        Path::new("/ht"),
        true,
    );
    assert!(
        plan.extra_args.iter().any(|a| a == "--no-skills"),
        "所有应用都应带 --no-skills（与是否声明 skills 能力无关）：{:?}",
        plan.extra_args
    );
    assert!(
        !plan.extra_args.iter().any(|a| a == "--skill"),
        "{:?}",
        plan.extra_args
    );
}

// ---------------------------------------------------------------------------
// declared：只贡献已授予且已启用的技能
// ---------------------------------------------------------------------------

#[test]
fn declared_app_gets_only_enabled_grants() {
    let tmp = tempfile::tempdir().unwrap();
    let layout = DataLayout::new(tmp.path().to_path_buf());
    let store = SkillStore::new(layout.clone());
    let known_tools = vec!["bash".to_string(), "python".to_string()];
    store
        .install_from_dir(
            &fixture("good-skill"),
            local_source(),
            true,
            &known_tools,
            1000,
        )
        .expect("good-skill 应装成功");
    store
        .install_from_dir(
            &fixture("script-skill"),
            local_source(),
            true,
            &known_tools,
            1000,
        )
        .expect("script-skill 应装成功");
    store
        .grant("app-b", "good-skill", &known_tools)
        .expect("授予 good-skill 应成功");
    store
        .grant("app-b", "script-skill", &known_tools)
        .expect("授予 script-skill 应成功");
    store
        .set_enabled("app-b", "script-skill", false)
        .expect("禁用 script-skill 应成功"); // 授予仍在，只是禁用

    let mcp = McpManager::new();
    let sock = layout.mcp_socket_path("app-b");
    let ctx = launch_ctx(&layout, &mcp, "app-b", &sock, false, true);
    let mut perms = Permissions::default();
    perms.skills.allow = true;
    let contribution = capabilities::builtin()
        .launch(&perms, &CallerIdentity::installing("app-b", false), &ctx)
        .expect("应成功算出贡献");

    let skill_flag_count = contribution
        .extra_args
        .iter()
        .filter(|a| *a == "--skill")
        .count();
    assert_eq!(
        skill_flag_count, 1,
        "两条授予、一条禁用 -> 恰一个 --skill：{:?}",
        contribution.extra_args
    );
    let good_dir = layout.skill_dir("good-skill");
    assert!(
        contribution
            .extra_args
            .iter()
            .any(|a| a == &good_dir.to_string_lossy().to_string()),
        "{:?}",
        contribution.extra_args
    );
    assert!(
        !contribution
            .extra_args
            .iter()
            .any(|a| a.contains("script-skill")),
        "禁用的技能不应出现在 --skill 参数里：{:?}",
        contribution.extra_args
    );
    assert_eq!(
        contribution.sandbox_read,
        vec![good_dir],
        "sandbox_read 应恰好包含已启用技能的目录"
    );
}

// ---------------------------------------------------------------------------
// describe()（materialize=false）只读，不落盘
// ---------------------------------------------------------------------------

#[test]
fn describe_does_not_touch_disk() {
    let tmp = tempfile::tempdir().unwrap();
    let layout = DataLayout::new(tmp.path().to_path_buf());
    let mcp = McpManager::new();
    let sock = layout.mcp_socket_path("app-c");
    let ctx = launch_ctx(&layout, &mcp, "app-c", &sock, false, false); // materialize=false

    let mut perms = Permissions::default();
    perms.skills.allow = true;
    let reg = capabilities::builtin();
    let cap = reg.iter().find(|c| c.key() == "skills").unwrap();
    let contribution = cap.launch(&perms, &ctx).expect("只读路径不应出错");
    assert!(contribution.extra_args.is_empty(), "未装任何技能，理应为空");
    assert!(
        !tmp.path().join("skills").exists(),
        "describe()（materialize=false）不应创建 skills/ 目录"
    );
    assert!(
        !tmp.path().join("skills-index.json").exists(),
        "describe()（materialize=false）不应创建 skills-index.json"
    );
}

// ---------------------------------------------------------------------------
// 卸载钩子：清空该应用的全部技能授予，技能库本身不受影响
// ---------------------------------------------------------------------------

#[test]
fn uninstall_hook_clears_grants() {
    let tmp = tempfile::tempdir().unwrap();
    let layout = DataLayout::new(tmp.path().to_path_buf());
    let store = SkillStore::new(layout.clone());
    let known_tools: Vec<String> = vec![];
    store
        .install_from_dir(
            &fixture("script-skill"),
            local_source(),
            true,
            &known_tools,
            1000,
        )
        .expect("script-skill 应装成功");
    store
        .grant("app-d", "script-skill", &known_tools)
        .expect("授予应成功");
    assert_eq!(store.grants_for("app-d").unwrap().len(), 1);

    let reg = capabilities::builtin();
    let cap = reg.iter().find(|c| c.key() == "skills").unwrap();
    cap.on_uninstall("app-d", &layout).expect("卸载钩子应成功");

    assert!(
        store.grants_for("app-d").unwrap().is_empty(),
        "卸载钩子应清空该应用的全部技能授予"
    );
    assert_eq!(
        store.list().unwrap().len(),
        1,
        "技能库本身不应被卸载钩子影响"
    );
}

// ---------------------------------------------------------------------------
// 审查修复轮 2 Important 1：能装 ≠ 能授予（spec §8 裁决，见 lib.rs::known_tools
// 文档）——声明 bash 的技能用真实 known_tools() 应能装；grant 给未放宽应用应被拒，
// grant 给 trusted 应用应放行。
// ---------------------------------------------------------------------------

fn manifest_with_tools(tools: &[&str]) -> super_agent_os::pkg::Manifest {
    serde_json::from_value(serde_json::json!({
        "name": "@t/x", "version": "1.0.0", "keywords": ["superagent-app"],
        "engines": {"superagent-host": ">=1.0 <2.0"},
        "superagent": {"schemaVersion": 1, "displayName": "x", "category": "life",
                        "ui": "ui/index.html", "permissions": "permissions.json",
                        "tools": tools}
    }))
    .unwrap()
}

#[test]
fn skill_declaring_bash_installs_but_grant_requires_relaxed_app() {
    let tmp = tempfile::tempdir().unwrap();
    let layout = DataLayout::new(tmp.path().to_path_buf());
    let store = SkillStore::new(layout.clone());
    let mcp = McpManager::new();

    // 用真实 known_tools()（不用手写桩列表）——bash 在 PI_BUILTIN_TOOLS 里，
    // pi 认识这个名字，所以声明 allowed-tools: bash 的技能应该能装。
    let tools = known_tools(&mcp);
    assert!(
        tools.iter().any(|t| t == "bash"),
        "known_tools() 应包含 bash（PI_BUILTIN_TOOLS）：{tools:?}"
    );
    let installed = store
        .install_from_dir(&fixture("bash-only-skill"), local_source(), true, &tools, 0)
        .expect("声明 allowed-tools: bash 的技能应能安装（pi 认识 bash 这个名字）");

    let perms = Permissions::default();
    let manifest = manifest_with_tools(&["bash"]);
    let registry = capabilities::builtin();

    // 未放宽应用（trusted=false, sandboxed=false）：即便清单声明了 tools=["bash"]，
    // resolve_tools 的非 relaxed 分支会把它从最终工具集里过滤掉——grant 应该被拒，
    // 错误信息含 "bash"。
    let sock_a = layout.mcp_socket_path("app-untrusted");
    let identity_a = CallerIdentity::installing("app-untrusted", false);
    let ctx_a = LaunchCtx {
        app_id: "app-untrusted",
        trusted: false,
        sandboxed: false,
        materialize: false,
        layout: &layout,
        hosttools_dir: Path::new("/ht"),
        socket_path: &sock_a,
        mcp: &mcp,
    };
    let app_tools_untrusted = app_tool_set_core(
        &manifest,
        &perms,
        false,
        false,
        &identity_a,
        &registry,
        &ctx_a,
    );
    assert!(
        !app_tools_untrusted.iter().any(|t| t == "bash"),
        "未放宽应用的最终工具集不该含 bash：{app_tools_untrusted:?}"
    );
    let err = store
        .grant("app-untrusted", &installed.meta.id, &app_tools_untrusted)
        .expect_err("未放宽应用不该被授予需要 bash 的技能");
    assert!(
        err.to_string().contains("bash"),
        "错误信息应提到 bash：{err}"
    );

    // trusted 应用：relaxed 分支放行清单声明的 bash——grant 应该成功。
    let sock_b = layout.mcp_socket_path("app-trusted");
    let identity_b = CallerIdentity::installing("app-trusted", true);
    let ctx_b = LaunchCtx {
        app_id: "app-trusted",
        trusted: true,
        sandboxed: false,
        materialize: false,
        layout: &layout,
        hosttools_dir: Path::new("/ht"),
        socket_path: &sock_b,
        mcp: &mcp,
    };
    let app_tools_trusted = app_tool_set_core(
        &manifest,
        &perms,
        true,
        false,
        &identity_b,
        &registry,
        &ctx_b,
    );
    assert!(
        app_tools_trusted.iter().any(|t| t == "bash"),
        "trusted 应用的最终工具集应含 bash：{app_tools_trusted:?}"
    );
    store
        .grant("app-trusted", &installed.meta.id, &app_tools_trusted)
        .expect("trusted 应用应能被授予声明了 bash 的技能");
}
