// P6-A Task8 迁移：本文件曾覆盖 session_mgr 里手写的 MCP/桥注入自由函数
// （`mcp_launch_extras`/`bridge_aware_launch_extras`/`agent_launch_extras`/
// `task_mcp_injection`）——四条会话路径改走 `CapabilityRegistry::launch` 之后，
// 这些函数已被删除（连同它们各自的单测）。本文件port 每条用例，改为直接调
// `capabilities::builtin().launch(...)`（生产的 `open_app_after_acquire`/
// `run_headless_session`/`spawn_call_session` 现在也是调这同一个方法），断言
// 与 legacy 相同的属性；两条测 `bridge_aware_launch_extras` 本身的用例
// （"无激活桥原样返回 base"/"base=None 但有桥激活时构造最小 socket 注入"）随其
// 主体一起删除——这两条属性现在由 `capability.rs` 的 registry merge 语义测试
// （`launch_merges_only_declared_and_dedups_tools_and_bridges` 等）覆盖。
//
// 覆盖内容（对应 legacy 分组）：
// 1. connectors 能力：读写/只读/无连接器/未匹配 category —— 四种场景。
// 2. task-mode 等价：从磁盘装一份真实清单/权限（同 `run_headless_session`/
//    `spawn_task_session` 内部读取方式），断言 registry.launch 的产出与前台
//    完全一致（本就是同一个方法，这里钉住"读盘 -> 权限 -> 调用"这条链路本身
//    没有走样）。
// 3. Maker 身份路径：router（MAKER_APP_ID）身份下 maker/router/call 桥 vs 非
//    router 身份下的桥隔离。
// 4. agents.call 桥隔离（原 P5 T4 分组）。
use std::collections::BTreeMap;
use std::path::Path;
use super_agent_os::capabilities;
use super_agent_os::capability::{CallerIdentity, LaunchCtx};
use super_agent_os::maker::MAKER_APP_ID;
use super_agent_os::mcp::McpManager;
use super_agent_os::paths::DataLayout;
use super_agent_os::permissions::{Access, ConnectorReq, Permissions};

fn mock_server_config_with_category(
    id: &str,
    category: &str,
) -> super_agent_os::vault::ServerConfig {
    super_agent_os::vault::ServerConfig {
        id: id.to_string(),
        category: category.to_string(),
        command: env!("CARGO_BIN_EXE_mock_mcp_server").to_string(),
        args: vec![],
        env: BTreeMap::new(),
        transport: "stdio".into(),
        trust: Default::default(),
    }
}

// ---------------------------------------------------------------------------
// 1. connectors 能力：读写/只读/无连接器/未匹配 category（原 mcp_launch_extras 三场景）
// ---------------------------------------------------------------------------

#[tokio::test]
async fn readwrite_connector_injects_both_tools_socket_and_extension() {
    let manager = McpManager::new();
    let cfg = mock_server_config_with_category("mcp9-fs-rw", "filesystem");
    manager
        .ensure_server(&cfg)
        .await
        .expect("ensure_server 应成功");

    let perms = Permissions {
        connectors: vec![ConnectorReq {
            category: "filesystem".to_string(),
            access: Access::ReadWrite,
        }],
        ..Default::default()
    };
    let tmp = tempfile::tempdir().unwrap();
    let layout = DataLayout::new(tmp.path().to_path_buf());
    let hosttools = Path::new("/opt/hosttools");
    let socket = layout.mcp_socket_path("app-rw");

    let ctx = LaunchCtx {
        app_id: "app-rw",
        trusted: false,
        sandboxed: true,
        materialize: true,
        layout: &layout,
        hosttools_dir: hosttools,
        socket_path: &socket,
        mcp: &manager,
    };
    let c = capabilities::builtin()
        .launch(&perms, &CallerIdentity::installing("app-rw", false), &ctx)
        .expect("读写授权应成功计算贡献");

    let tools_json = c
        .env
        .iter()
        .find(|(k, _)| k == "SUPERAGENT_MCP_TOOLS")
        .map(|(_, v)| v.clone())
        .expect("应含 SUPERAGENT_MCP_TOOLS");
    let parsed: serde_json::Value = serde_json::from_str(&tools_json).expect("应是合法 JSON");
    let arr = parsed.as_array().expect("应是数组");
    assert_eq!(arr.len(), 2, "读写授权应含两个工具，实际：{arr:?}");
    for entry in arr {
        assert!(
            entry["server"].is_string(),
            "每条应带 server，实际：{entry:?}"
        );
        assert!(entry["tool"].is_string(), "每条应带 tool，实际：{entry:?}");
        assert!(entry["name"].is_string(), "每条应带 name，实际：{entry:?}");
        assert!(
            entry.get("inputSchema").is_some(),
            "每条应带 inputSchema，实际：{entry:?}"
        );
    }
    assert!(
        arr.iter().any(|e| e["tool"] == "read_file"),
        "实际：{arr:?}"
    );
    assert!(
        arr.iter().any(|e| e["tool"] == "write_file"),
        "实际：{arr:?}"
    );

    let socket_env = c
        .env
        .iter()
        .find(|(k, _)| k == "SUPERAGENT_MCP_SOCKET")
        .map(|(_, v)| v.clone())
        .expect("应含 SUPERAGENT_MCP_SOCKET");
    assert_eq!(socket_env, socket.to_string_lossy().to_string());

    assert!(
        c.bridges.contains(&"mcp_bridge.ts"),
        "应把 mcp-bridge 扩展加进 bridges，实际：{:?}",
        c.bridges
    );
    assert!(c.needs_socket);
}

/// 只读连接器：Task6 的可见性隔离必须 carry 到注入层——注入的
/// SUPERAGENT_MCP_TOOLS 只应含 read_file，绝不能含 write_file。
#[tokio::test]
async fn read_only_connector_injects_only_read_tool() {
    let manager = McpManager::new();
    let cfg = mock_server_config_with_category("mcp9-fs-ro", "filesystem");
    manager
        .ensure_server(&cfg)
        .await
        .expect("ensure_server 应成功");

    let perms = Permissions {
        connectors: vec![ConnectorReq {
            category: "filesystem".to_string(),
            access: Access::Read,
        }],
        ..Default::default()
    };
    let tmp = tempfile::tempdir().unwrap();
    let layout = DataLayout::new(tmp.path().to_path_buf());
    let hosttools = Path::new("/opt/hosttools");
    let socket = layout.mcp_socket_path("app-ro");

    let ctx = LaunchCtx {
        app_id: "app-ro",
        trusted: false,
        sandboxed: true,
        materialize: true,
        layout: &layout,
        hosttools_dir: hosttools,
        socket_path: &socket,
        mcp: &manager,
    };
    let c = capabilities::builtin()
        .launch(&perms, &CallerIdentity::installing("app-ro", false), &ctx)
        .expect("只读授权应成功计算贡献");

    let tools_json = c
        .env
        .iter()
        .find(|(k, _)| k == "SUPERAGENT_MCP_TOOLS")
        .map(|(_, v)| v.clone())
        .expect("应含 SUPERAGENT_MCP_TOOLS");
    let parsed: serde_json::Value = serde_json::from_str(&tools_json).expect("应是合法 JSON");
    let arr = parsed.as_array().expect("应是数组");

    assert_eq!(
        arr.len(),
        1,
        "只读授权只应看到 1 个工具（可见性隔离），实际：{arr:?}"
    );
    assert_eq!(arr[0]["tool"], "read_file");
    assert!(
        !arr.iter().any(|e| e["tool"] == "write_file"),
        "只读授权绝不能把 write_file 注入进去，实际：{arr:?}"
    );

    // M-6：只读隔离必须同时在最终 `--tools` 白名单面（`contribution.tools`，
    // `assemble_launch_plan` 拿去与清单声明取并集的那份）上成立，不能只在
    // `SUPERAGENT_MCP_TOOLS` env 这一份 JSON 上成立——两者理应同源，但分别断言
    // 才能防止未来两处实现分叉时只有一处被测出来。
    assert!(
        c.tools.contains(&"mcp__mcp9-fs-ro__read_file".to_string()),
        "{:?}",
        c.tools
    );
    assert!(
        !c.tools.iter().any(|t| t.ends_with("__write_file")),
        "只读授权的 --tools 面也绝不能含 write_file，实际：{:?}",
        c.tools
    );
}

/// 空连接器（或声明的 category 无任何已连接 server）→ authorized_tools 为空 →
/// 不注入任何依赖 socket 的东西（`connectors` 能力贡献 `LaunchContribution::default()`）——
/// `ui_emit` 恒贡献的 `ui_emit.ts`/`__host_ui_emit__` 不受影响，但不属于本用例断言范围。
#[tokio::test]
async fn no_connectors_injects_nothing() {
    let manager = McpManager::new();
    let cfg = mock_server_config_with_category("mcp9-fs-empty", "filesystem");
    manager
        .ensure_server(&cfg)
        .await
        .expect("ensure_server 应成功");

    let perms = Permissions::default();
    let tmp = tempfile::tempdir().unwrap();
    let layout = DataLayout::new(tmp.path().to_path_buf());
    let hosttools = Path::new("/opt/hosttools");
    let socket = layout.mcp_socket_path("app-empty");

    let ctx = LaunchCtx {
        app_id: "app-empty",
        trusted: false,
        sandboxed: true,
        materialize: true,
        layout: &layout,
        hosttools_dir: hosttools,
        socket_path: &socket,
        mcp: &manager,
    };
    let c = capabilities::builtin()
        .launch(
            &perms,
            &CallerIdentity::installing("app-empty", false),
            &ctx,
        )
        .unwrap();

    assert!(
        !c.env.iter().any(|(k, _)| k == "SUPERAGENT_MCP_TOOLS"),
        "无连接器时不应注入 SUPERAGENT_MCP_TOOLS，实际：{:?}",
        c.env
    );
    assert!(
        !c.env.iter().any(|(k, _)| k == "SUPERAGENT_MCP_SOCKET"),
        "无连接器/无其它需要 socket 的能力时不应注入 SUPERAGENT_MCP_SOCKET，实际：{:?}",
        c.env
    );
    assert!(!c.bridges.contains(&"mcp_bridge.ts"));
    assert!(!c.needs_socket);
}

/// 声明的 category 没有任何已连接的 server 匹配（同上一条但更贴近"声明了
/// connector 但对应服务未连接"的现实场景）→ 同样不注入。
#[tokio::test]
async fn unmatched_category_connector_injects_nothing() {
    let manager = McpManager::new();
    let cfg = mock_server_config_with_category("mcp9-fs-unmatched", "filesystem");
    manager
        .ensure_server(&cfg)
        .await
        .expect("ensure_server 应成功");

    let perms = Permissions {
        connectors: vec![ConnectorReq {
            category: "calendar".to_string(),
            access: Access::Read,
        }],
        ..Default::default()
    };
    let tmp = tempfile::tempdir().unwrap();
    let layout = DataLayout::new(tmp.path().to_path_buf());
    let hosttools = Path::new("/opt/hosttools");
    let socket = layout.mcp_socket_path("app-unmatched");

    let ctx = LaunchCtx {
        app_id: "app-unmatched",
        trusted: false,
        sandboxed: true,
        materialize: true,
        layout: &layout,
        hosttools_dir: hosttools,
        socket_path: &socket,
        mcp: &manager,
    };
    let c = capabilities::builtin()
        .launch(
            &perms,
            &CallerIdentity::installing("app-unmatched", false),
            &ctx,
        )
        .unwrap();

    assert!(
        !c.env.iter().any(|(k, _)| k == "SUPERAGENT_MCP_TOOLS"),
        "未匹配的 category 应贡献零授权工具，不应注入，实际：{:?}",
        c.env
    );
    assert!(!c.needs_socket);
}

// ---------------------------------------------------------------------------
// 2. task-mode 等价：`run_headless_session`/`spawn_task_session` 内部读磁盘上的
// 清单/权限文件后同样调 `capabilities::builtin().launch(...)`——这里从磁盘装一份
// 真实清单/权限（同该内核的读取方式），钉住"读盘 -> 权限解析 -> 调用注册表"这条
// 链路本身产出与直接构造 Permissions（用例1）完全一致的结果。
// ---------------------------------------------------------------------------

/// 往 `layout.packages_dir(app_id)` 写一份最小合法清单 + 指定的 permissions.json
/// 内容（同 `pkg.rs`/`scheduler_it.rs` 同名助手同规格）。
fn write_task_app_package(layout: &DataLayout, app_id: &str, permissions_json: &str) {
    let dir = layout.packages_dir(app_id);
    std::fs::create_dir_all(dir.join("ui")).unwrap();
    std::fs::write(dir.join("ui/index.html"), "<html></html>").unwrap();
    std::fs::write(dir.join("permissions.json"), permissions_json).unwrap();
    let pkg = serde_json::json!({
        "name": app_id,
        "version": "1.0.0",
        "keywords": ["pi-package", "superagent-app"],
        "engines": { "superagent-host": ">=1.0.0, <2.0.0" },
        "superagent": {
            "schemaVersion": 1,
            "displayName": app_id,
            "category": "life",
            "ui": "ui/index.html",
            "permissions": "permissions.json",
        }
    });
    std::fs::write(
        dir.join("package.json"),
        serde_json::to_string_pretty(&pkg).unwrap(),
    )
    .unwrap();
}

fn load_perms_from_disk(layout: &DataLayout, app_id: &str) -> Permissions {
    let pkg_dir = layout.packages_dir(app_id);
    let manifest = super_agent_os::pkg::load_and_validate(&pkg_dir).expect("清单应能解析");
    super_agent_os::permissions::load(&pkg_dir, &manifest.superagent.permissions)
        .expect("权限应能解析")
}

/// 读写连接器：task-mode 从磁盘装的权限应得到与前台（用例1）完全一样的三件套——
/// SUPERAGENT_MCP_TOOLS（两个工具）+ SUPERAGENT_MCP_SOCKET（与 `open_app` 给前台
/// 监听器 bind 的路径**同一个** `layout.mcp_socket_path(app_id)`）+ mcp-bridge 扩展。
#[tokio::test]
async fn task_mode_readwrite_connector_gets_same_tools_socket_and_extension_as_foreground() {
    let tmp = tempfile::tempdir().unwrap();
    let layout = DataLayout::new(tmp.path().to_path_buf());
    write_task_app_package(
        &layout,
        "app-t17b-rw",
        r#"{"connectors":[{"category":"filesystem","access":"readwrite"}]}"#,
    );

    let manager = McpManager::new();
    let cfg = mock_server_config_with_category("mcp17b-fs-rw", "filesystem");
    manager
        .ensure_server(&cfg)
        .await
        .expect("ensure_server 应成功");

    let perms = load_perms_from_disk(&layout, "app-t17b-rw");
    let socket_path = layout.mcp_socket_path("app-t17b-rw");
    let hosttools = Path::new("/opt/hosttools");
    let ctx = LaunchCtx {
        app_id: "app-t17b-rw",
        trusted: false,
        sandboxed: true,
        materialize: true,
        layout: &layout,
        hosttools_dir: hosttools,
        socket_path: &socket_path,
        mcp: &manager,
    };
    let c = capabilities::builtin()
        .launch(
            &perms,
            &CallerIdentity::installing("app-t17b-rw", false),
            &ctx,
        )
        .unwrap();

    let tools_json = c
        .env
        .iter()
        .find(|(k, _)| k == "SUPERAGENT_MCP_TOOLS")
        .map(|(_, v)| v.clone())
        .expect("应含 SUPERAGENT_MCP_TOOLS");
    let parsed: serde_json::Value = serde_json::from_str(&tools_json).expect("应是合法 JSON");
    let arr = parsed.as_array().expect("应是数组");
    assert_eq!(arr.len(), 2, "读写授权应含两个工具，实际：{arr:?}");
    assert!(arr.iter().any(|e| e["tool"] == "read_file"));
    assert!(arr.iter().any(|e| e["tool"] == "write_file"));

    let socket_env = c
        .env
        .iter()
        .find(|(k, _)| k == "SUPERAGENT_MCP_SOCKET")
        .map(|(_, v)| v.clone())
        .expect("应含 SUPERAGENT_MCP_SOCKET");
    assert_eq!(socket_env, socket_path.to_string_lossy().to_string());

    assert!(
        c.bridges.contains(&"mcp_bridge.ts"),
        "应把 mcp-bridge 扩展加进 bridges，实际：{:?}",
        c.bridges
    );
}

/// 只读连接器：Task6 的可见性隔离必须 carry 到 task-mode 注入层——同前台一样只应
/// 看到 read_file，绝不能含 write_file。
#[tokio::test]
async fn task_mode_read_only_connector_injects_only_read_tool_isolation_carried() {
    let tmp = tempfile::tempdir().unwrap();
    let layout = DataLayout::new(tmp.path().to_path_buf());
    write_task_app_package(
        &layout,
        "app-t17b-ro",
        r#"{"connectors":[{"category":"filesystem","access":"read"}]}"#,
    );

    let manager = McpManager::new();
    let cfg = mock_server_config_with_category("mcp17b-fs-ro", "filesystem");
    manager
        .ensure_server(&cfg)
        .await
        .expect("ensure_server 应成功");

    let perms = load_perms_from_disk(&layout, "app-t17b-ro");
    let socket_path = layout.mcp_socket_path("app-t17b-ro");
    let hosttools = Path::new("/opt/hosttools");
    let ctx = LaunchCtx {
        app_id: "app-t17b-ro",
        trusted: false,
        sandboxed: true,
        materialize: true,
        layout: &layout,
        hosttools_dir: hosttools,
        socket_path: &socket_path,
        mcp: &manager,
    };
    let c = capabilities::builtin()
        .launch(
            &perms,
            &CallerIdentity::installing("app-t17b-ro", false),
            &ctx,
        )
        .unwrap();

    let tools_json = c
        .env
        .iter()
        .find(|(k, _)| k == "SUPERAGENT_MCP_TOOLS")
        .map(|(_, v)| v.clone())
        .expect("应含 SUPERAGENT_MCP_TOOLS");
    let parsed: serde_json::Value = serde_json::from_str(&tools_json).expect("应是合法 JSON");
    let arr = parsed.as_array().expect("应是数组");
    assert_eq!(
        arr.len(),
        1,
        "只读授权只应看到 1 个工具（可见性隔离），实际：{arr:?}"
    );
    assert_eq!(arr[0]["tool"], "read_file");
    assert!(!arr.iter().any(|e| e["tool"] == "write_file"));
}

/// 该 app 没有声明任何 connector（permissions.json 里 connectors 缺省为空数组）
/// → 不注入 MCP 相关的东西，与迁移前 Task12 行为一致。
#[tokio::test]
async fn task_mode_no_connectors_injects_nothing_unchanged_from_task12() {
    let tmp = tempfile::tempdir().unwrap();
    let layout = DataLayout::new(tmp.path().to_path_buf());
    write_task_app_package(&layout, "app-t17b-none", "{}");

    let manager = McpManager::new();
    let cfg = mock_server_config_with_category("mcp17b-fs-empty", "filesystem");
    manager
        .ensure_server(&cfg)
        .await
        .expect("ensure_server 应成功");

    let perms = load_perms_from_disk(&layout, "app-t17b-none");
    let socket_path = layout.mcp_socket_path("app-t17b-none");
    let hosttools = Path::new("/opt/hosttools");
    let ctx = LaunchCtx {
        app_id: "app-t17b-none",
        trusted: false,
        sandboxed: true,
        materialize: true,
        layout: &layout,
        hosttools_dir: hosttools,
        socket_path: &socket_path,
        mcp: &manager,
    };
    let c = capabilities::builtin()
        .launch(
            &perms,
            &CallerIdentity::installing("app-t17b-none", false),
            &ctx,
        )
        .unwrap();

    assert!(
        !c.env.iter().any(|(k, _)| k == "SUPERAGENT_MCP_TOOLS"),
        "无 connector 声明时不应注入，实际：{:?}",
        c.env
    );
    assert!(!c.needs_socket);
}

/// 该 app 根本没有装过包（`layout.packages_dir` 下没有 package.json）——理论上
/// 不应在生产路径发生（`open_app` 打开该 app 时已经校验过清单），但
/// `run_headless_session`（`spawn_task_session`/`spawn_call_session` 共用的内核）
/// 必须优雅地退化为默认权限而不是 panic——见其文档"与前台刻意不同"一节。这里
/// 直接钉住 `pkg::load_and_validate` 在这种场景下确实是 `Err`（前置条件），以及
/// 内核遇到这种情况时会用的同一条降级路径（`Permissions::default()`）不会让
/// `registry.launch` panic、也不会产生任何依赖 socket 的贡献——与
/// `tests/call_session_it.rs::spawn_call_session_runs_callee_and_returns_text`
/// （被调方从未装过包，走完整 `spawn_call_session` 也不 panic）互为端到端补充。
#[tokio::test]
async fn task_mode_missing_package_manifest_falls_back_without_panicking() {
    let tmp = tempfile::tempdir().unwrap();
    let layout = DataLayout::new(tmp.path().to_path_buf());
    let manager = McpManager::new();

    let pkg_dir = layout.packages_dir("app-never-installed");
    assert!(
        super_agent_os::pkg::load_and_validate(&pkg_dir).is_err(),
        "前置：确实没有装过包"
    );

    let socket_path = layout.mcp_socket_path("app-never-installed");
    let hosttools = Path::new("/opt/hosttools");
    let ctx = LaunchCtx {
        app_id: "app-never-installed",
        trusted: false,
        sandboxed: true,
        materialize: true,
        layout: &layout,
        hosttools_dir: hosttools,
        socket_path: &socket_path,
        mcp: &manager,
    };
    let c = capabilities::builtin()
        .launch(
            &Permissions::default(),
            &CallerIdentity::installing("app-never-installed", false),
            &ctx,
        )
        .expect("默认权限下不应出错");

    assert!(!c.env.iter().any(|(k, _)| k == "SUPERAGENT_MCP_TOOLS"));
    assert!(!c.needs_socket);
}

// ---------------------------------------------------------------------------
// Round 2 review（I-1 覆盖半边）：直接单测 `session_mgr::headless_contribution` 本身
// ——它是 headless 会话（task-mode/`spawn_task_session`、call-mode/`spawn_call_session`）
// 唯一计算 `LaunchContribution` 的地方（见其文档"唯一计算点"一节）。上面 2. 这一节的
// 用例都是经 `capabilities::builtin().launch(...)` 间接验证"读盘 -> 权限 -> 调用注册表"
// 这条链路，不直接触碰 `headless_contribution` 这一个函数本身；这里补上直接调用，
// 钉住它的三条属性：`Some(socket)` 时贡献原样生效、`None` 时剥离依赖 socket 的字段但
// 保留 sandbox 路径、`depth` 确实传导到 `CallerIdentity`（不会被内部悄悄归零）。
// ---------------------------------------------------------------------------

fn headless_contribution_app(id: &str) -> super_agent_os::registry::InstalledApp {
    super_agent_os::registry::InstalledApp {
        app_id: id.into(),
        name: id.into(),
        version: "1.0.0".into(),
        display_name: id.into(),
        category: "life".into(),
        icon: None,
        trusted: false,
        domains: vec![],
    }
}

/// `Some(socket_path)`：贡献原样生效——`SUPERAGENT_MCP_TOOLS`（两个工具）、
/// `SUPERAGENT_MCP_SOCKET` 等于传入的路径、`mcp_bridge.ts` 桥、`mcp__<srv>__read_file`/
/// `__write_file` 两个工具名、`needs_socket == true`。
#[tokio::test]
async fn headless_contribution_some_socket_injects_mcp_tools_and_bridge() {
    let tmp = tempfile::tempdir().unwrap();
    let layout = DataLayout::new(tmp.path().to_path_buf());
    write_task_app_package(
        &layout,
        "app-hc-rw",
        r#"{"connectors":[{"category":"filesystem","access":"readwrite"}]}"#,
    );
    let app = headless_contribution_app("app-hc-rw");

    let manager = McpManager::new();
    let cfg = mock_server_config_with_category("mcp-hc-rw", "filesystem");
    manager
        .ensure_server(&cfg)
        .await
        .expect("ensure_server 应成功");

    let socket_path = layout.mcp_socket_path(&app.app_id);
    let (_manifest, c) = super_agent_os::session_mgr::headless_contribution(
        &layout,
        &capabilities::builtin(),
        &app,
        0,
        Path::new("/ht"),
        Some(&socket_path),
        &manager,
    )
    .expect("应成功算出贡献");

    let tools_json = c
        .env
        .iter()
        .find(|(k, _)| k == "SUPERAGENT_MCP_TOOLS")
        .map(|(_, v)| v.clone())
        .expect("应含 SUPERAGENT_MCP_TOOLS");
    let parsed: serde_json::Value = serde_json::from_str(&tools_json).expect("应是合法 JSON");
    let arr = parsed.as_array().expect("应是数组");
    assert_eq!(arr.len(), 2, "读写授权应含两个工具，实际：{arr:?}");

    let socket_env = c
        .env
        .iter()
        .find(|(k, _)| k == "SUPERAGENT_MCP_SOCKET")
        .map(|(_, v)| v.clone())
        .expect("应含 SUPERAGENT_MCP_SOCKET");
    assert_eq!(socket_env, socket_path.to_string_lossy().to_string());

    assert!(
        c.bridges.contains(&"mcp_bridge.ts"),
        "实际：{:?}",
        c.bridges
    );
    assert!(
        c.tools.contains(&"mcp__mcp-hc-rw__read_file".to_string()),
        "{:?}",
        c.tools
    );
    assert!(
        c.tools.contains(&"mcp__mcp-hc-rw__write_file".to_string()),
        "{:?}",
        c.tools
    );
    assert!(c.needs_socket);
}

/// `None`：没有任何监听器在跑——剥掉一切依赖 socket 的贡献（`bridges`/`env`/`tools`/
/// `needs_socket`），只保留 `sandbox_read`/`sandbox_write`。与同一个 app 的 `Some` 调用
/// 结果比较相等（而不是断言"都是空 vec"）——这个夹具没有声明 `filesystem.read`/`write`，
/// 两次调用碰巧都是空，但断言相等钉住的是"剥离只动依赖 socket 的字段，不触碰 sandbox
/// 路径"这条属性本身，不依赖夹具恰好是空的偶然性。
#[tokio::test]
async fn headless_contribution_none_socket_strips_bridges_env_tools_keeps_sandbox_paths() {
    let tmp = tempfile::tempdir().unwrap();
    let layout = DataLayout::new(tmp.path().to_path_buf());
    write_task_app_package(
        &layout,
        "app-hc-none",
        r#"{"connectors":[{"category":"filesystem","access":"readwrite"}]}"#,
    );
    let app = headless_contribution_app("app-hc-none");

    let manager = McpManager::new();
    let cfg = mock_server_config_with_category("mcp-hc-none", "filesystem");
    manager
        .ensure_server(&cfg)
        .await
        .expect("ensure_server 应成功");

    let socket_path = layout.mcp_socket_path(&app.app_id);
    let (_m1, with_socket) = super_agent_os::session_mgr::headless_contribution(
        &layout,
        &capabilities::builtin(),
        &app,
        0,
        Path::new("/ht"),
        Some(&socket_path),
        &manager,
    )
    .unwrap();
    let (_m2, without_socket) = super_agent_os::session_mgr::headless_contribution(
        &layout,
        &capabilities::builtin(),
        &app,
        0,
        Path::new("/ht"),
        None,
        &manager,
    )
    .unwrap();

    assert!(
        without_socket.bridges.is_empty(),
        "实际：{:?}",
        without_socket.bridges
    );
    assert!(
        without_socket.env.is_empty(),
        "实际：{:?}",
        without_socket.env
    );
    assert!(
        without_socket.tools.is_empty(),
        "实际：{:?}",
        without_socket.tools
    );
    assert!(!without_socket.needs_socket);
    assert_eq!(without_socket.sandbox_read, with_socket.sandbox_read);
    assert_eq!(without_socket.sandbox_write, with_socket.sandbox_write);
}

/// `depth`：必须真的传到 `CallerIdentity`，不会被内部悄悄归零——用声明了
/// `agents.call` 的 app，在 `depth: 2` 下断言 `__host_call_agent__`/
/// `call_agent_bridge.ts` 仍然存在（`AgentsCallCapability::declared()` 本身不按
/// depth 门控，这里钉住的是"depth 确实送到了 identity 构造这一步、没有在半路被
/// 吞掉"这条穿线本身，为将来任何按 depth 门控的能力打好回归底）。
#[tokio::test]
async fn headless_contribution_depth_reaches_identity_without_breaking_declared() {
    let tmp = tempfile::tempdir().unwrap();
    let layout = DataLayout::new(tmp.path().to_path_buf());
    write_task_app_package(
        &layout,
        "app-hc-depth",
        r#"{"agents":{"call":["@superagent/summarizer"]}}"#,
    );
    let app = headless_contribution_app("app-hc-depth");
    let manager = McpManager::new();

    let socket_path = layout.mcp_socket_path(&app.app_id);
    let (_manifest, c) = super_agent_os::session_mgr::headless_contribution(
        &layout,
        &capabilities::builtin(),
        &app,
        2,
        Path::new("/ht"),
        Some(&socket_path),
        &manager,
    )
    .expect("应成功算出贡献");

    assert!(
        c.tools.contains(&"__host_call_agent__".to_string()),
        "{:?}",
        c.tools
    );
    assert!(
        c.bridges.contains(&"call_agent_bridge.ts"),
        "{:?}",
        c.bridges
    );
}

// ---------------------------------------------------------------------------
// 3. P4/P5：Maker 身份路径 —— 打开 Maker（trusted 内置 app，app_id == MAKER_APP_ID）
// 时，即便它没有声明任何 MCP connector，也必须注入 SUPERAGENT_MCP_SOCKET env +
// maker_bridge.ts 扩展；非 Maker app 在任何情况下都绝不能看到 maker_bridge。
// ---------------------------------------------------------------------------

/// Maker、无 connectors（当前实际预期场景）：connectors 能力本身不贡献任何东西，
/// 但 maker/router/agents.call 能力都对 router 身份 `declared()`，合起来仍产出
/// SUPERAGENT_MCP_SOCKET + maker_bridge.ts，不含 SUPERAGENT_MCP_TOOLS/mcp_bridge.ts
/// （Maker 没有 MCP 工具面）。
#[tokio::test]
async fn maker_without_connectors_still_gets_socket_and_maker_bridge() {
    let manager = McpManager::new();
    let tmp = tempfile::tempdir().unwrap();
    let layout = DataLayout::new(tmp.path().to_path_buf());
    let hosttools = Path::new("/opt/hosttools");
    let socket = layout.mcp_socket_path("maker");

    let ctx = LaunchCtx {
        app_id: MAKER_APP_ID,
        trusted: true,
        sandboxed: true,
        materialize: true,
        layout: &layout,
        hosttools_dir: hosttools,
        socket_path: &socket,
        mcp: &manager,
    };
    let c = capabilities::builtin()
        .launch(
            &Permissions::default(),
            &CallerIdentity::installing(MAKER_APP_ID, true),
            &ctx,
        )
        .expect("Maker 即便无 connectors 也应成功计算贡献");

    let socket_env = c
        .env
        .iter()
        .find(|(k, _)| k == "SUPERAGENT_MCP_SOCKET")
        .map(|(_, v)| v.clone())
        .expect("应含 SUPERAGENT_MCP_SOCKET");
    assert_eq!(socket_env, socket.to_string_lossy().to_string());
    assert!(
        c.bridges.contains(&"maker_bridge.ts"),
        "实际：{:?}",
        c.bridges
    );
    assert!(
        !c.env.iter().any(|(k, _)| k == "SUPERAGENT_MCP_TOOLS"),
        "Maker 无 connectors 时不应注入 SUPERAGENT_MCP_TOOLS，实际：{:?}",
        c.env
    );
    assert!(
        !c.bridges.contains(&"mcp_bridge.ts"),
        "Maker 无 connectors 时不应注入 mcp_bridge.ts，实际：{:?}",
        c.bridges
    );
}

/// 非 Maker、有 connectors、无 agents.call：应含 mcp_bridge.ts，绝不含 maker_bridge.ts
/// ——即便该 app 恰好也用到了 MCP socket 基础设施，也不应额外混入 Maker 专属的桥。
#[tokio::test]
async fn non_maker_app_with_connectors_gets_mcp_extras_but_not_maker_bridge() {
    let manager = McpManager::new();
    let cfg = mock_server_config_with_category("mcp9-maker-aware-fs", "filesystem");
    manager
        .ensure_server(&cfg)
        .await
        .expect("ensure_server 应成功");

    let perms = Permissions {
        connectors: vec![ConnectorReq {
            category: "filesystem".to_string(),
            access: Access::ReadWrite,
        }],
        ..Default::default()
    };
    let tmp = tempfile::tempdir().unwrap();
    let layout = DataLayout::new(tmp.path().to_path_buf());
    let hosttools = Path::new("/opt/hosttools");
    let socket = layout.mcp_socket_path("some-other-app");

    let ctx = LaunchCtx {
        app_id: "some-other-app",
        trusted: false,
        sandboxed: true,
        materialize: true,
        layout: &layout,
        hosttools_dir: hosttools,
        socket_path: &socket,
        mcp: &manager,
    };
    let c = capabilities::builtin()
        .launch(
            &perms,
            &CallerIdentity::installing("some-other-app", false),
            &ctx,
        )
        .expect("有 connectors 时应成功计算贡献");

    assert!(
        c.bridges.contains(&"mcp_bridge.ts"),
        "应含 mcp_bridge.ts，实际：{:?}",
        c.bridges
    );
    assert!(
        !c.bridges.contains(&"maker_bridge.ts"),
        "非 Maker app 绝不能看到 maker_bridge.ts，实际：{:?}",
        c.bridges
    );
}

/// Maker、恰好也声明了 MCP connectors：两套工具面（MCP + Maker）在同一个 socket 上共存。
#[tokio::test]
async fn maker_with_connectors_gets_both_mcp_bridge_and_maker_bridge() {
    let manager = McpManager::new();
    let cfg = mock_server_config_with_category("mcp9-maker-both-fs", "filesystem");
    manager
        .ensure_server(&cfg)
        .await
        .expect("ensure_server 应成功");

    let perms = Permissions {
        connectors: vec![ConnectorReq {
            category: "filesystem".to_string(),
            access: Access::ReadWrite,
        }],
        ..Default::default()
    };
    let tmp = tempfile::tempdir().unwrap();
    let layout = DataLayout::new(tmp.path().to_path_buf());
    let hosttools = Path::new("/opt/hosttools");
    let socket = layout.mcp_socket_path("maker-rw");

    let ctx = LaunchCtx {
        app_id: MAKER_APP_ID,
        trusted: true,
        sandboxed: true,
        materialize: true,
        layout: &layout,
        hosttools_dir: hosttools,
        socket_path: &socket,
        mcp: &manager,
    };
    let c = capabilities::builtin()
        .launch(
            &perms,
            &CallerIdentity::installing(MAKER_APP_ID, true),
            &ctx,
        )
        .expect("有 connectors 时应成功计算贡献");

    assert!(c.env.iter().any(|(k, _)| k == "SUPERAGENT_MCP_TOOLS"));
    assert!(
        c.bridges.contains(&"mcp_bridge.ts"),
        "实际：{:?}",
        c.bridges
    );
    assert!(
        c.bridges.contains(&"maker_bridge.ts"),
        "实际：{:?}",
        c.bridges
    );
}

// ---------------------------------------------------------------------------
// 4. agents.call 桥隔离（原 P5 T4 分组）——两条测 `bridge_aware_launch_extras`
// 本身的用例已随该函数删除（属性由 capability.rs 的 registry merge 语义覆盖）。
// ---------------------------------------------------------------------------

fn has_bridge(bridges: &[&'static str], file: &str) -> bool {
    bridges.iter().any(|b| b.ends_with(file))
}

/// 非 router、有 agents.call → 注入 call 桥，绝不注 maker 桥。
#[tokio::test]
async fn agents_call_third_party_with_call_gets_call_bridge_only() {
    let manager = McpManager::new();
    let tmp = tempfile::tempdir().unwrap();
    let layout = DataLayout::new(tmp.path().to_path_buf());
    let hosttools = Path::new("/opt/hosttools");
    let socket = layout.mcp_socket_path("caller");

    let mut perms = Permissions::default();
    perms.agents.call = vec!["@superagent/summarizer".to_string()];
    let ctx = LaunchCtx {
        app_id: "superagent__researcher",
        trusted: false,
        sandboxed: true,
        materialize: true,
        layout: &layout,
        hosttools_dir: hosttools,
        socket_path: &socket,
        mcp: &manager,
    };
    let c = capabilities::builtin()
        .launch(
            &perms,
            &CallerIdentity::installing("superagent__researcher", false),
            &ctx,
        )
        .expect("声明了 agents.call 应成功计算贡献");

    assert!(has_bridge(&c.bridges, "call_agent_bridge.ts"));
    assert!(
        !has_bridge(&c.bridges, "maker_bridge.ts"),
        "非 Maker 绝不注 maker 桥"
    );
}

/// 非 router、无 agents.call、无 connectors → 不产出任何依赖 socket 的贡献（什么都不注）。
#[tokio::test]
async fn agents_call_plain_third_party_gets_nothing() {
    let manager = McpManager::new();
    let tmp = tempfile::tempdir().unwrap();
    let layout = DataLayout::new(tmp.path().to_path_buf());
    let hosttools = Path::new("/opt/hosttools");
    let socket = layout.mcp_socket_path("plain");

    let ctx = LaunchCtx {
        app_id: "superagent__plain",
        trusted: false,
        sandboxed: true,
        materialize: true,
        layout: &layout,
        hosttools_dir: hosttools,
        socket_path: &socket,
        mcp: &manager,
    };
    let c = capabilities::builtin()
        .launch(
            &Permissions::default(),
            &CallerIdentity::installing("superagent__plain", false),
            &ctx,
        )
        .unwrap();

    assert!(!c.needs_socket);
    assert!(!has_bridge(&c.bridges, "call_agent_bridge.ts"));
    assert!(!has_bridge(&c.bridges, "maker_bridge.ts"));
    assert!(!has_bridge(&c.bridges, "mcp_bridge.ts"));
}

/// router（MAKER_APP_ID）→ maker 桥 + call 桥 + router 桥都注入（路由能力）。
#[tokio::test]
async fn agents_call_router_gets_maker_call_and_router_bridges() {
    let manager = McpManager::new();
    let tmp = tempfile::tempdir().unwrap();
    let layout = DataLayout::new(tmp.path().to_path_buf());
    let hosttools = Path::new("/opt/hosttools");
    let socket = layout.mcp_socket_path("superagent");

    let ctx = LaunchCtx {
        app_id: MAKER_APP_ID,
        trusted: true,
        sandboxed: true,
        materialize: true,
        layout: &layout,
        hosttools_dir: hosttools,
        socket_path: &socket,
        mcp: &manager,
    };
    let c = capabilities::builtin()
        .launch(
            &Permissions::default(),
            &CallerIdentity::installing(MAKER_APP_ID, true),
            &ctx,
        )
        .expect("router 即便无 connectors 也应成功计算贡献");

    assert!(has_bridge(&c.bridges, "maker_bridge.ts"));
    assert!(
        has_bridge(&c.bridges, "call_agent_bridge.ts"),
        "router 应有 call 桥用于路由"
    );
    assert!(
        has_bridge(&c.bridges, "router_bridge.ts"),
        "router 应有 list_agents 路由桥"
    );
}

/// 非 router（仅 agents.call）不应看到 router_bridge（list_agents 是路由专属）。
#[tokio::test]
async fn agents_call_third_party_has_no_router_bridge() {
    let manager = McpManager::new();
    let tmp = tempfile::tempdir().unwrap();
    let layout = DataLayout::new(tmp.path().to_path_buf());
    let hosttools = Path::new("/opt/hosttools");
    let socket = layout.mcp_socket_path("caller");

    let mut perms = Permissions::default();
    perms.agents.call = vec!["@superagent/summarizer".to_string()];
    let ctx = LaunchCtx {
        app_id: "superagent__researcher",
        trusted: false,
        sandboxed: true,
        materialize: true,
        layout: &layout,
        hosttools_dir: hosttools,
        socket_path: &socket,
        mcp: &manager,
    };
    let c = capabilities::builtin()
        .launch(
            &perms,
            &CallerIdentity::installing("superagent__researcher", false),
            &ctx,
        )
        .expect("有 agents.call 应成功计算贡献");

    assert!(
        !has_bridge(&c.bridges, "router_bridge.ts"),
        "非 router 绝不注 router 桥"
    );
}
