// Task13（P4，本分支收官任务）：mock Maker 端到端集成测试。
//
// 测试硬件本身扮演 Maker——不经过任何真实模型/真实 pi 决策，直接经
// `__host_maker_*__` socket 帧驱动（与 `tests/maker_socket_it.rs` 的假客户端
// 手法完全一致）——headless、keyless（全程用编译期产物 `mock_pi` 替身，不碰
// 真实 Anthropic key/真实 keychain），证明 P4 spec §7 描述的整条数据流管道
// 真的接在一起：
//
//   stage_write × 4（经绑定 `MAKER_APP_ID` 的 socket 监听器，逐个文件）
//     → 落盘到 `DataLayout::maker_staging_dir(draft_id)`（T1/T2/T4）
//   → `__host_maker_preview__`（经 socket，T3/T6）→ 该草稿以
//     `trusted=false` 经与其余第三方应用完全同一条路径拉起临时 pi 会话，
//     真实 P2 沙盒（macOS：`/usr/bin/sandbox-exec`）跑到 ready（keyless
//     `get_session_stats`），**不**进入已装 registry
//   → `__host_maker_install__`（经 socket，T3/T5）→ 只登记 pending、
//     `{pending_confirm, confirm_id}`，此刻仍**未安装**
//   → `maker::resolve_install(..., allow=true)`（T5，唯一真正的安装入口，
//     生产由 Tauri 命令 `maker_respond_install_confirm` 调用，本测试直接调用
//     自由函数——不需要 `tauri::AppHandle`）→ 委托 P1
//     `install::install_or_upgrade`（`trusted=false`）→ registry 落地、
//     `packages/<app_id>` 通过 `pkg::load_and_validate`
//   → `session_mgr::spawn_task_session`（与 `open_app_after_acquire`/
//     `spawn_preview_session` 调用的是同一个私有 `spawn_app_session`，见
//     `session_mgr.rs` 文档"两处必须永远一致"）拉起这个刚装好的应用的
//     headless 会话，真的跑到 `agent_end`——证明它不只是"registry 里多了一条
//     记录"，是真的**可打开**的应用。
//
// 复用（而非重新发明）既有测试已经验证过的手法：
// - `tests/maker_socket_it.rs`：假客户端（连一次/写一行 JSON/读一行 JSON/关
//   连接），与 `mcp_transport.ts::hostMcpCall` 线协议字节对齐。
// - `tests/maker_it.rs`：最小合法草稿的形状（`package.json` 含
//   `pi-package`/`superagent-app` 关键字、`schemaVersion:1`、UI/permissions
//   文件真实存在——真实 P1 格式，无 `AGENT.md`）；`resolve_install`/
//   `list_pending_installs` 的调用方式。
// - `tests/maker_preview_it.rs`：`SUPERAGENT_PI_BIN`/`ENV_LOCK` 惯例、
//   `sandboxed_argv` 直接断言"这次沙盒包裹返回的 bin 字面是
//   `/usr/bin/sandbox-exec`"的低层证明手法。
// - `tests/scheduler_it.rs`：`session_mgr::spawn_task_session` 是 task-mode
//   headless 会话与前台交互会话共享的同一条 `spawn_app_session` 派发路径，
//   本测试借它证明"刚装好的应用可打开"，不需要 `tauri::AppHandle`。
//
// 集成测试的每个文件是独立编译单元，本仓库目前没有 `tests/common` 共享模块——
// 下面几个小工具函数（`fake_client_send` 等）是从 `maker_socket_it.rs`/
// `maker_it.rs` 原样复制一份，而不是新增一个共享模块：这一小段重复比引入新的
// 跨文件依赖风险更小。

use std::path::{Path, PathBuf};
use tokio::sync::Mutex;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use super_agent_os::maker::{self, MAKER_APP_ID};
use super_agent_os::mcp::McpManager;
use super_agent_os::mcp_socket::McpSocketListener;
use super_agent_os::paths::DataLayout;
use super_agent_os::pkg;
use super_agent_os::registry::RegistryStore;
use super_agent_os::session_mgr;

#[cfg(target_os = "macos")]
use super_agent_os::session_mgr::sandboxed_argv;

// `std::env::set_var`/`remove_var` 改变进程全局状态（`SUPERAGENT_PI_BIN`）；本
// 文件目前只有一个 `#[tokio::test]`，但仍按同文件其它 P4 测试
// （`maker_preview_it.rs`/`e2e_mock.rs`）同款惯例声明这把锁——防将来有人在本
// 文件里再加一个同样要碰这个环境变量的测试时，悄悄引入 P3/P4 已经多次踩过、
// 也已经多次注释过的并行竞争坑。
static ENV_LOCK: std::sync::LazyLock<Mutex<()>> = std::sync::LazyLock::new(|| Mutex::new(()));

fn temp_layout() -> (tempfile::TempDir, DataLayout, McpManager) {
    let tmp = tempfile::tempdir().unwrap();
    let layout = DataLayout::new(tmp.path().to_path_buf());
    (tmp, layout, McpManager::new())
}

fn maker_socket_path(tmp: &tempfile::TempDir) -> PathBuf {
    tmp.path().join(MAKER_APP_ID).join("mcp.sock")
}

/// 手写假客户端：连一次、写一行 `{method,params}` JSON 请求、读一行 JSON 响应、
/// 关连接——与 `tests/maker_socket_it.rs::fake_client_send` 完全同规格，与
/// `mcp_transport.ts::hostMcpCall` 的线协议字节对齐。
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

// 最小合法 Maker 草稿的四个文件——与 `tests/maker_it.rs::write_valid_maker_draft`/
// `tests/maker_preview_it.rs` 同规格的真实 P1 格式（`package.json` 含
// `pi-package`+`superagent-app` 关键字、`schemaVersion:1`、UI/permissions 文件
// 真实存在，无 `AGENT.md`），区别只是这里由测试硬件经 `__host_maker_stage_write__`
// 逐个文件通过 socket "写"出来，而不是直接 `std::fs::write` 抄近道——这正是本
// 测试要证明的"Maker subagent 真的能通过这三个工具，把一个包一点点攒出来"这件事。
const PACKAGE_JSON: &str = r#"{
  "name": "@superagent/maker-e2e-demo", "version": "1.0.0",
  "keywords": ["pi-package", "superagent-app"],
  "engines": { "superagent-host": ">=1.0.0, <2.0.0" },
  "superagent": { "schemaVersion": 1, "displayName": "E2E演示应用",
    "category": "life", "ui": "ui/index.html", "permissions": "permissions.json" }
}"#;
const PERMISSIONS_JSON: &str = "{}";
const PERSONA_MD: &str =
    "你是一个由 Maker 生成的端到端演示应用，负责证明生成→预览→安装管道真的接在一起。";
const UI_HTML: &str = "<html><body>maker e2e demo</body></html>";

/// THE P4 milestone：headless、keyless，测试硬件扮演 Maker，驱动
/// 生成（stage_write）→预览（sandbox）→标准安装（confirm-then-install）
/// →可打开 这条完整管道。
#[tokio::test]
async fn maker_generate_preview_install_and_open_pipeline_end_to_end() {
    let _guard = ENV_LOCK.lock().await;
    std::env::set_var("SUPERAGENT_PI_BIN", env!("CARGO_BIN_EXE_mock_pi"));

    let (tmp, layout, manager) = temp_layout();
    let registry = RegistryStore::new(layout.registry_path());
    let socket_path = maker_socket_path(&tmp);

    // Maker 是特权内置 app：它的 socket 监听器绑定的 app_id 就是
    // `maker::MAKER_APP_ID`——只有绑在这个 app_id 上的监听器，`__host_maker_*__`
    // 方法才会被路由到真正的 handler：P6-A 起门控是
    // `capabilities::maker::MakerCapability::declared`（`id.is_router()`），由
    // `capability::CapabilityRegistry::dispatch`（`mcp_socket.rs::process_request`
    // 调用它）统一执行，见 `tests/maker_socket_it.rs` 对非 Maker app_id 越权尝试
    // 的回归覆盖——本测试不重复那部分安全回归，只验证"作为 Maker 本身，整条合法
    // 管道确实通"。
    let listener = McpSocketListener::start(
        manager.clone(),
        layout.clone(),
        MAKER_APP_ID.to_string(),
        vec![],
        socket_path.clone(),
    )
    .expect("Maker 的 socket 监听器应能成功 bind");

    let draft_id = "draft-e2e-flagship";

    // ---------------------------------------------------------------------
    // 阶段 1：stage_write × 4（经 socket）——把最小合法包逐个文件写进暂存目录
    // ---------------------------------------------------------------------
    let files: [(&str, &str); 4] = [
        ("package.json", PACKAGE_JSON),
        ("permissions.json", PERMISSIONS_JSON),
        ("agent/persona.md", PERSONA_MD),
        ("ui/index.html", UI_HTML),
    ];
    for (rel_path, content) in files {
        let resp = fake_client_send(
            &socket_path,
            "__host_maker_stage_write__",
            serde_json::json!({ "draft_id": draft_id, "rel_path": rel_path, "content": content }),
        )
        .await;
        assert_eq!(
            resp["ok"],
            serde_json::json!(true),
            "stage_write({rel_path}) 应成功，实际：{resp:?}"
        );
        let path_field = resp["path"]
            .as_str()
            .unwrap_or_else(|| panic!("stage_write({rel_path}) 应带 path 字段，实际：{resp:?}"));
        let written = std::fs::read_to_string(path_field)
            .unwrap_or_else(|e| panic!("{rel_path} 应已落盘到 {path_field}：{e}"));
        assert_eq!(written, content, "{rel_path} 落盘内容应与请求一致");
    }

    let staging_dir = layout.maker_staging_dir(draft_id);
    assert!(
        staging_dir.join("package.json").is_file(),
        "package.json 应已落盘"
    );
    assert!(
        staging_dir.join("permissions.json").is_file(),
        "permissions.json 应已落盘"
    );
    assert!(
        staging_dir.join("agent/persona.md").is_file(),
        "agent/persona.md 应已落盘"
    );
    assert!(
        staging_dir.join("ui/index.html").is_file(),
        "ui/index.html 应已落盘"
    );

    // ---------------------------------------------------------------------
    // 阶段 2：__host_maker_preview__（经 socket）——真实 P2 沙盒跑到 ready，
    // 绝不触碰已装 registry
    // ---------------------------------------------------------------------
    let preview_resp = fake_client_send(
        &socket_path,
        "__host_maker_preview__",
        serde_json::json!({ "draft_id": draft_id }),
    )
    .await;
    assert_eq!(
        preview_resp["ok"],
        serde_json::json!(true),
        "合法草稿的预览应成功，实际：{preview_resp:?}"
    );
    assert!(
        registry.load().is_empty(),
        "预览绝不能把草稿写入已装应用 registry"
    );
    assert!(
        preview_resp.get("pending_confirm").is_none(),
        "预览不应产生任何 pending_confirm 状态"
    );

    #[cfg(target_os = "macos")]
    {
        // 直接证明：这次预览唯一会调用到的沙盒 argv 构建函数——`sandboxed_argv`
        // （`session_mgr::spawn_preview_session` 内部调用的同一个生产函数）——对
        // 这份暂存目录真的返回 `/usr/bin/sandbox-exec`。与
        // `tests/maker_preview_it.rs` 第 1 层证明同规格，这里断言的是本次 e2e
        // 自己驱动出来的暂存目录，不是复用那个文件已经验证过的结果。
        let canonical_staging_dir = std::fs::canonicalize(&staging_dir)
            .expect("stage_write 之后暂存目录应已存在，可 canonicalize");
        let (bin, _argv) = sandboxed_argv(&canonical_staging_dir, false, &[], None, &[], &[])
            .expect("sandboxed_argv 不应失败");
        assert_eq!(
            bin, "/usr/bin/sandbox-exec",
            "预览必须经真实 /usr/bin/sandbox-exec 包裹，实际 bin={bin}"
        );
    }

    // ---------------------------------------------------------------------
    // 阶段 3：__host_maker_install__（经 socket）——只登记 pending，绝不安装
    // ---------------------------------------------------------------------
    let install_resp = fake_client_send(
        &socket_path,
        "__host_maker_install__",
        serde_json::json!({ "draft_id": draft_id }),
    )
    .await;
    assert_eq!(
        install_resp["pending_confirm"],
        serde_json::json!(true),
        "合法草稿的安装请求应返回 pending_confirm，实际：{install_resp:?}"
    );
    let confirm_id = install_resp["confirm_id"]
        .as_str()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| panic!("应返回非空 confirm_id，实际：{install_resp:?}"))
        .to_string();
    assert!(
        registry.load().is_empty(),
        "确认前，Maker 生成的应用不应已被安装"
    );

    // T5b 前端确认面查询：应能看到这条刚登记的 pending，展示"「E2E演示应用」请求安装"。
    let pending_list = maker::list_pending_installs(&manager);
    assert_eq!(
        pending_list.len(),
        1,
        "应能看到刚登记的 pending install，实际：{pending_list:?}"
    );
    assert_eq!(pending_list[0].confirm_id, confirm_id);
    assert_eq!(pending_list[0].display_name, "E2E演示应用");

    // ---------------------------------------------------------------------
    // 阶段 4：resolve_install(..., allow=true)——唯一真正把字节落到
    // packages/<app_id> 并写入 registry 的入口（生产由 Tauri 命令
    // `maker_respond_install_confirm` 调用；本测试直接调用同一个自由函数）
    // ---------------------------------------------------------------------
    let installed = maker::resolve_install(&manager, &layout, &registry, &confirm_id, true)
        .expect("allow=true 且 confirm_id 合法应成功")
        .expect("allow=true 应返回 Some(app)");

    assert_eq!(installed.app_id, "superagent__maker-e2e-demo");
    assert!(!installed.trusted, "Maker 输出必须 trusted=false，绝不免检");

    let from_registry = registry
        .get(&installed.app_id)
        .expect("确认后应已在 registry 中");
    assert_eq!(from_registry.app_id, installed.app_id);
    assert!(!from_registry.trusted);

    let installed_dir = layout.packages_dir(&installed.app_id);
    pkg::load_and_validate(&installed_dir).expect("已装包目录应能通过 load_and_validate");

    // 已被消费的 pending 不应再出现在前端确认面查询里。
    assert!(
        maker::list_pending_installs(&manager).is_empty(),
        "已消费的 pending install 不应再出现在列表里"
    );

    // ---------------------------------------------------------------------
    // 阶段 5：openability——刚装好的应用不只是"registry 里多了一条记录"，
    // 它真的能被拉起、跑到 agent_end（`session_mgr::spawn_task_session` 与
    // `open_app_after_acquire`/`spawn_preview_session` 共享同一个私有
    // `spawn_app_session`，macOS 上无条件经 `sandboxed_argv` 包一层真实
    // `/usr/bin/sandbox-exec`——见 `session_mgr.rs` "两处必须永远一致"文档）。
    // ---------------------------------------------------------------------
    // hosttools_dir 不需要真实存在：它只被拼进 `-e <path>`/`--append-system-prompt
    // <path>` 这类字符串参数里，`mock_pi` 完全不解析自己的 CLI 参数（只读
    // stdin），不会因为这些路径不存在而失败。
    let hosttools_dir = tmp.path().join("hosttools-not-a-real-dir");
    let task_result = session_mgr::spawn_task_session(
        &layout,
        &hosttools_dir,
        &manager,
        &installed,
        "e2e-openability-probe",
        "ping",
    )
    .await
    .expect("刚装好的应用应能被拉起——同 open_app 一致的 spawn_app_session 路径");

    assert_eq!(task_result.app_id, installed.app_id);
    assert!(
        !task_result.errored,
        "拉起刚装好的应用不应报错，实际：{task_result:?}"
    );

    #[cfg(target_os = "macos")]
    {
        // 与阶段2同规格的直接证明，但这次针对的是**安装后的包目录**本身（而非
        // 暂存目录）：刚装好的应用被打开时，走的是与草稿预览完全一致的真实 OS
        // 沙盒边界，不是巧合。
        let canonical_installed_dir =
            std::fs::canonicalize(&installed_dir).expect("安装后的包目录应已存在，可 canonicalize");
        let (bin, _argv) = sandboxed_argv(&canonical_installed_dir, false, &[], None, &[], &[])
            .expect("sandboxed_argv 不应失败");
        assert_eq!(
            bin, "/usr/bin/sandbox-exec",
            "已装应用被打开时必须经真实 /usr/bin/sandbox-exec 包裹，实际 bin={bin}"
        );
    }

    listener.stop().await;
    std::env::remove_var("SUPERAGENT_PI_BIN");
}
