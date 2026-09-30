// P5 T7 互联 e2e（里程碑：两个应用协作完成一个任务）。
//
// 端到端把 P5 互联的三层接起来：安装（install_builtin_sample_core 装 researcher +
// summarizer 到 packages/）→ socket 分发（researcher 的前台监听器收 __host_call_agent__）
// → 被调方会话（spawn_call_session 拉起 summarizer 的 mock_pi 会话回传文本）。
//
// 测试硬件直接连 researcher 的 socket、发 __host_call_agent__ 帧，模拟"researcher 的
// 模型自己决定调 summarizer"（真实模型在自由对话里做这个决定是手工里程碑，headless
// 测不了——同 P4 e2e 里硬件扮演 Maker 直接发 maker 帧）。
//
// SUPERAGENT_PI_BIN/MOCK_PI_MODE 是进程级全局，用 ENV_LOCK 序列化（惯例见
// scheduler_it.rs / maker_socket_it.rs 头部注释）。

use std::path::{Path, PathBuf};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::Mutex;

use super_agent_os::maker::MAKER_APP_ID;
use super_agent_os::mcp::McpManager;
use super_agent_os::mcp_socket::McpSocketListener;
use super_agent_os::paths::DataLayout;
use super_agent_os::registry::RegistryStore;
use super_agent_os::{call_bus, install_builtin_sample_core};

static ENV_LOCK: std::sync::LazyLock<Mutex<()>> = std::sync::LazyLock::new(|| Mutex::new(()));

fn real_samples_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../samples")
}

fn hosttools_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("hosttools")
}

async fn fake_client_send(
    socket_path: &Path,
    method: &str,
    params: serde_json::Value,
) -> serde_json::Value {
    let stream = tokio::net::UnixStream::connect(socket_path)
        .await
        .unwrap_or_else(|e| panic!("连接 {socket_path:?} 应成功：{e}"));
    let (r, mut w) = stream.into_split();
    let req = serde_json::json!({ "method": method, "params": params });
    w.write_all(format!("{req}\n").as_bytes())
        .await
        .expect("写请求应成功");
    let mut reader = BufReader::new(r);
    let mut line = String::new();
    reader.read_line(&mut line).await.expect("应能读到一行响应");
    serde_json::from_str(&line).unwrap_or_else(|e| panic!("响应应是合法 JSON，实际 {line:?}：{e}"))
}

/// 里程碑：researcher 经 __host_call_agent__ 调 summarizer，summarizer 的会话真的被
/// 拉起、回传文本——"两个应用协作完成一个任务"端到端跑通。
///
/// P6-A：`process_request` 改走 `CapabilityRegistry::dispatch` 之后，
/// `__host_call_agent__` 要先过 `agents.call` 能力的 `declared()` 闸——它读的
/// 是**监听器绑定时那份 `Permissions`**（`start_with_identity` 参数），不再
/// 是旧手写分支那样完全不看、放行到 `call_bus::handle_call_agent` 内部才凭
/// 自己重新读盘的 `researcher` `agents.call`。这里改用 `start_with_identity`
/// 并真的加载 researcher 装好之后的 `permissions.json`——与生产路径
/// `session_mgr::open_app_after_acquire` 的做法一致（`crate::pkg::load_and_validate`
/// + `crate::permissions::load`），而不是继续用只绑 `connectors` 的
/// `start_with` 薄封装（那样 `agents.call` 恒为空，声明闸会先一步拒绝，根本
/// 到不了这条测试真正想钉住的"两个应用协作端到端"这件事）。
#[tokio::test]
async fn researcher_calls_summarizer_end_to_end_and_gets_result() {
    let _guard = ENV_LOCK.lock().await;
    std::env::set_var("SUPERAGENT_PI_BIN", env!("CARGO_BIN_EXE_mock_pi"));

    let tmp = tempfile::tempdir().unwrap();
    let layout = DataLayout::new(tmp.path().to_path_buf());
    let reg = RegistryStore::new(layout.registry_path());

    // 装协作样例对儿（进 packages/，含各自 permissions.json——handle_call_agent 要读
    // researcher 的 agents.call）。
    let researcher =
        install_builtin_sample_core("researcher", &real_samples_dir(), &layout, &reg).unwrap();
    install_builtin_sample_core("summarizer", &real_samples_dir(), &layout, &reg).unwrap();

    // 真实加载 researcher 装好之后的清单权限——与生产路径
    // `session_mgr::open_app_after_acquire` 同一手法，供下面绑给监听器。
    let researcher_pkg_dir = layout.packages_dir(&researcher.app_id);
    let researcher_manifest = super_agent_os::pkg::load_and_validate(&researcher_pkg_dir)
        .expect("researcher 清单应能解析");
    let researcher_perms = super_agent_os::permissions::load(
        &researcher_pkg_dir,
        &researcher_manifest.superagent.permissions,
    )
    .expect("researcher 权限清单应能解析");

    // researcher 的前台交互监听器（调用链顶端 depth=0，绑定 hosttools 以启用 call bus，
    // 绑定真实身份 + 真实 agents.call 权限）。
    let socket_path = layout.mcp_socket_path("superagent__researcher");
    let listener = McpSocketListener::start_with_identity(
        McpManager::new(),
        layout.clone(),
        super_agent_os::capability::CallerIdentity {
            app_id: "superagent__researcher".to_string(),
            trusted: researcher.trusted,
            depth: 0,
        },
        researcher_perms,
        socket_path.clone(),
        Some(hosttools_dir()),
        std::sync::Arc::new(super_agent_os::capabilities::builtin()),
    )
    .expect("researcher 监听器应能 bind");

    // 模拟 researcher 的模型调 summarizer。
    let resp = fake_client_send(
        &socket_path,
        "__host_call_agent__",
        serde_json::json!({ "target": "@superagent/summarizer", "prompt": "把这些要点精简成 3 条" }),
    )
    .await;

    std::env::remove_var("SUPERAGENT_PI_BIN");
    listener.stop().await;

    assert_eq!(
        resp["ok"],
        serde_json::json!(true),
        "协作应成功，实际：{resp:?}"
    );
    assert_eq!(
        resp["text"],
        serde_json::json!("你好，世界"),
        "应回传被调方 summarizer 会话的助手文本，实际：{resp:?}"
    );
}

/// 安全：researcher 调一个已装但**未在其 agents.call 里声明**的应用（这里用内置
/// superagent/Maker）→ 未授权拒绝，绝不拉起被调方。
///
/// P6-A（code review I-1 修复）：跟 happy-path 测试一样，用
/// `start_with_identity` 绑定 researcher 真实装好之后的 `permissions.json`
/// （`agents.call: ["@superagent/summarizer"]`，不含这里要调的 `superagent`），
/// 而不是只绑 `connectors` 恒空 `agents.call` 的 `start_with`——否则请求会在
/// `AgentsCallCapability::declared()` 这道前置闸就被拒（`unauthorized: 该应用
/// 未声明能力 agents.call`），根本到不了 `call_bus::authorize_call` 的白名单
/// 分支，这条测试就测不到它声称要测的东西（已装但未声明 → not-permitted）。
#[tokio::test]
async fn researcher_cannot_call_undeclared_installed_app() {
    let _guard = ENV_LOCK.lock().await;
    std::env::set_var("SUPERAGENT_PI_BIN", env!("CARGO_BIN_EXE_mock_pi"));

    let tmp = tempfile::tempdir().unwrap();
    let layout = DataLayout::new(tmp.path().to_path_buf());
    let reg = RegistryStore::new(layout.registry_path());
    let researcher =
        install_builtin_sample_core("researcher", &real_samples_dir(), &layout, &reg).unwrap();
    // 装 Maker（superagent）作为"已装但 researcher 没声明调它"的目标。
    super_agent_os::seed_builtin_maker(&real_samples_dir(), &layout, &reg).unwrap();

    // 真实加载 researcher 装好之后的清单权限——与 happy-path 测试、生产路径
    // `session_mgr::open_app_after_acquire` 同一手法。researcher 的
    // `agents.call` 只含 `@superagent/summarizer`，不含 `superagent`。
    let researcher_pkg_dir = layout.packages_dir(&researcher.app_id);
    let researcher_manifest = super_agent_os::pkg::load_and_validate(&researcher_pkg_dir)
        .expect("researcher 清单应能解析");
    let researcher_perms = super_agent_os::permissions::load(
        &researcher_pkg_dir,
        &researcher_manifest.superagent.permissions,
    )
    .expect("researcher 权限清单应能解析");

    let socket_path = layout.mcp_socket_path("superagent__researcher");
    let listener = McpSocketListener::start_with_identity(
        McpManager::new(),
        layout.clone(),
        super_agent_os::capability::CallerIdentity {
            app_id: "superagent__researcher".to_string(),
            trusted: researcher.trusted,
            depth: 0,
        },
        researcher_perms,
        socket_path.clone(),
        Some(hosttools_dir()),
        std::sync::Arc::new(super_agent_os::capabilities::builtin()),
    )
    .expect("监听器应能 bind");

    let resp = fake_client_send(
        &socket_path,
        "__host_call_agent__",
        serde_json::json!({ "target": "superagent", "prompt": "x" }),
    )
    .await;

    std::env::remove_var("SUPERAGENT_PI_BIN");
    listener.stop().await;

    assert_eq!(resp["ok"], serde_json::json!(false));
    assert!(
        resp["error"].as_str().unwrap_or("").contains("未声明调用"),
        "错误信息应是 call_bus::authorize_call 白名单拒绝的原文，实际：{resp:?}"
    );
    let audits = super_agent_os::audit::query(
        &layout,
        &super_agent_os::audit::AuditFilter {
            app_id: Some("superagent__researcher".into()),
            tool: Some("__host_call_agent__".into()),
            ..Default::default()
        },
    );
    assert!(
        audits.iter().any(|a| a.verdict == "not-permitted"),
        "应落 call_bus 自己写的 not-permitted 审计（证明真的走到了 authorize_call），实际：{audits:?}"
    );
}

/// 深度闸（直接单测 IO 版，避免搭多层 mock_pi 嵌套链）：depth 到 MAX 时直接拒，
/// 不产生任何 spawn 副作用。
#[tokio::test]
async fn handle_call_agent_denies_at_max_depth_without_spawning() {
    let tmp = tempfile::tempdir().unwrap();
    let layout = DataLayout::new(tmp.path().to_path_buf());
    let reg = RegistryStore::new(layout.registry_path());
    install_builtin_sample_core("summarizer", &real_samples_dir(), &layout, &reg).unwrap();

    // caller = router（豁免白名单），只让深度闸能拒；depth = MAX。
    let resp = call_bus::handle_call_agent(
        MAKER_APP_ID,
        &serde_json::json!({ "target": "@superagent/summarizer", "prompt": "x" }),
        &layout,
        &McpManager::new(),
        &hosttools_dir(),
        call_bus::MAX_CALL_DEPTH,
    )
    .await;

    assert_eq!(resp["ok"], serde_json::json!(false));
    assert!(
        resp["error"].as_str().unwrap_or("").contains("深度"),
        "depth 到顶应被拒，实际：{resp:?}"
    );
}
