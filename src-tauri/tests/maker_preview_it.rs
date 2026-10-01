// Task6（P4 Maker）集成测试：`__host_maker_preview__`——把 Maker 暂存草稿当作
// **未受信第三方包**（`trusted=false`）拉起一个临时 pi 会话，验证它能在 P2
// 真实 `/usr/bin/sandbox-exec` 沙盒下跑到 ready（收到 `agent_end`），且这个
// 预览过程绝不把草稿写进已装应用 registry。
//
// 仅 macOS 运行——依赖系统自带 `/usr/bin/sandbox-exec`，与
// `tests/sandbox_escape_it.rs`/`tests/sandbox_pipe_it.rs`/
// `tests/sandbox_startup_it.rs` 同规格的 `#![cfg(target_os = "macos")]` 门控。
// 不需要 `#[ignore]`——不像 `tests/real_pi_bash_escape_it.rs` 依赖本机装的真实
// pi 二进制/真实模型 API key，本文件全程用 `mock_pi`（编译期产物，
// `env!("CARGO_BIN_EXE_mock_pi")`）替身 pi，keyless、不碰真实 keychain。
//
// 两层证明合起来钉住 T6 brief 的两条 milestone 属性：
//
// 1.（低层、直接证明"沙盒包裹"）`preview_session_wraps_real_sandbox_exec_and_mock_pi_reaches_ready`：
//    直接调用 `session_mgr::sandboxed_argv(staging_dir, trusted=false, ...)`——
//    与 `session_mgr::spawn_preview_session` 内部唯一一次"拉起子进程"动作
//    （`spawn_app_session` -> macOS 分支 -> `sandboxed_argv`）调用的是同一个
//    生产函数，取同一组参数形状——断言其返回的 `bin` 字面是
//    `/usr/bin/sandbox-exec`，再真的用这份 argv 通过 `RpcSession::spawn_wrapped`
//    拉起一个真实的 `sandbox-exec` 子进程包 `mock_pi`，验证 mock_pi 确实能在
//    这层真实沙盒包裹下收到 prompt 并吐出 `agent_end`（"ready"）。
//    与 `tests/scheduler_it.rs`（Task12，验证 task-mode 复用同一条 `sandboxed_argv`
//    路径）同一手法，但额外做了真实 spawn + 端到端 JSONL 往返（`sandbox_pipe_it.rs`
//    同款手法），比只调用纯函数验证返回值更强一层。
// 2.（高层、经生产入口）`preview_via_maker_request_reaches_ok_true_and_registry_stays_empty`
//    + `preview_invalid_draft_rejected_without_touching_registry`：走真正的
//    `maker::handle_maker_request("__host_maker_preview__", ...)` 生产入口，
//    证明合法草稿预览成功（`{ok:true}`）且全程不触碰 registry；非法草稿
//    fail-closed，同样不触碰 registry。
//
// 读代码交叉核对：`session_mgr::spawn_preview_session` 的实现里，唯一一次拉起
// 子进程的调用就是 `spawn_app_session(..., trusted=false, ...)`——与
// `open_app_after_acquire`/`spawn_task_session` 调用的是同一个私有函数，不是
// 重新拼的一份等价逻辑；macOS 分支还有 `debug_assert!(sandboxing_available(), ...)`
// 钉住一致性。这与上面第 1 条测试合起来，构成"沙盒包裹被断言证明、不是假设"
// 的完整证据链。
#![cfg(target_os = "macos")]

use std::path::Path;
use std::time::Duration;
use tokio::sync::Mutex;

use super_agent_os::maker::handle_maker_request;
use super_agent_os::mcp::McpManager;
use super_agent_os::paths::DataLayout;
use super_agent_os::registry::RegistryStore;
use super_agent_os::rpc::{PiEvent, RpcSession};
use super_agent_os::session_mgr::sandboxed_argv;

// `SUPERAGENT_PI_BIN` 是进程级全局状态（`std::env::set_var`/`remove_var`），本文件
// 内多个 `#[tokio::test]` 默认并行跑在同一个测试二进制里——不序列化会在它上面
// 竞争、间歇性失败（`pi_bin.rs`/`e2e_mock.rs`/`scheduler_it.rs` 等文件已踩过、
// 并留下同款注释的教训）。
static ENV_LOCK: std::sync::LazyLock<Mutex<()>> = std::sync::LazyLock::new(|| Mutex::new(()));

fn temp_layout() -> (tempfile::TempDir, DataLayout, McpManager) {
    let tmp = tempfile::tempdir().unwrap();
    let layout = DataLayout::new(tmp.path().to_path_buf());
    (tmp, layout, McpManager::new())
}

/// 在 `layout.maker_staging_dir(draft_id)` 下写一份最小合法包——与
/// `tests/maker_it.rs::write_valid_maker_draft` 同规格（`install.rs`/`pkg.rs`
/// 测试用的合法包形状：`package.json` 含 `pi-package`+`superagent-app` 关键字、
/// `schemaVersion:1`、UI/permissions 文件真实存在）。
fn write_valid_maker_draft(layout: &DataLayout, draft_id: &str) {
    let dir = layout.maker_staging_dir(draft_id);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("package.json"),
        r#"{
          "name": "@superagent/maker-preview-demo", "version": "1.0.0",
          "keywords": ["pi-package", "superagent-app"],
          "engines": { "superagent-host": ">=1.0.0, <2.0.0" },
          "superagent": { "schemaVersion": 1, "displayName": "Maker预览示例",
            "category": "life", "ui": "ui/index.html", "permissions": "permissions.json" }
        }"#,
    )
    .unwrap();
    std::fs::write(dir.join("permissions.json"), "{}").unwrap();
    std::fs::create_dir_all(dir.join("ui")).unwrap();
    std::fs::write(
        dir.join("ui/index.html"),
        "<html><body>maker preview demo</body></html>",
    )
    .unwrap();
}

/// 用与 `session_mgr::spawn_preview_session` 内部完全同形状的 `resolve_tools(&[],
/// false, sandboxed)` 结果拼 `--tools`——`declared` 为空时该函数恒定返回
/// `SAFE_TOOLS`（不含 bash/联网），与是否 sandboxed 无关，见其文档。
const PREVIEW_EXTRA_ARGS_TOOLS: &str = "__host_ui_emit__,read,write,edit,ls,grep,find";

/// 第 1 层证明：`sandboxed_argv(staging_dir, trusted=false, ...)`——预览会话
/// 唯一会调用到的沙盒 argv 构建函数——真的返回 `/usr/bin/sandbox-exec`，且用
/// 这份 argv 真实拉起的 `mock_pi` 子进程确实能在这层沙盒包裹下收到 prompt、
/// 吐出 `agent_end`（"ready"）。不经过 `maker::handle_preview`（那是第 2 层
/// 生产入口测试），直接对齐 `session_mgr::spawn_preview_session` 内部会传的
/// 参数形状：`trusted=false`、`app_data_dir=staging_dir`、`mcp_socket=None`。
#[tokio::test]
async fn preview_session_wraps_real_sandbox_exec_and_mock_pi_reaches_ready() {
    let _guard = ENV_LOCK.lock().await;
    std::env::set_var("SUPERAGENT_PI_BIN", env!("CARGO_BIN_EXE_mock_pi"));

    let (_tmp, layout, _manager) = temp_layout();
    write_valid_maker_draft(&layout, "draft-preview-lowlevel");
    // 与生产同源：由已规范化的数据根按字面推出并校验（`checked_maker_staging_dir`），
    // 而不是对路径先 canonicalize 再用。
    let canonical_staging_dir = layout
        .checked_maker_staging_dir("draft-preview-lowlevel")
        .unwrap();

    let extra_args = vec!["--tools".to_string(), PREVIEW_EXTRA_ARGS_TOOLS.to_string()];

    let (bin, argv) = sandboxed_argv(&canonical_staging_dir, false, &extra_args, None, &[], &[])
        .expect("sandboxed_argv 不应失败");
    assert_eq!(
        bin, "/usr/bin/sandbox-exec",
        "预览会话（trusted=false）必须经真实 /usr/bin/sandbox-exec 包裹，实际 bin={bin}"
    );

    let (session, mut rx) = RpcSession::spawn_wrapped(
        &bin,
        argv,
        &canonical_staging_dir,
        vec![],
        Some(&canonical_staging_dir),
    )
    .await
    .expect("经真实 /usr/bin/sandbox-exec 包 mock_pi 应能 spawn 成功");

    session
        .send_prompt("preview-ready-probe")
        .await
        .expect("沙盒内 stdin 写入不应失败");

    let mut ready = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        match tokio::time::timeout(remaining, rx.recv()).await {
            Ok(Some(PiEvent::AgentEnded)) => {
                ready = true;
                break;
            }
            Ok(Some(_)) => continue,
            Ok(None) => break,
            Err(_) => break,
        }
    }

    let mut session = session;
    session.kill().await;
    std::env::remove_var("SUPERAGENT_PI_BIN");

    assert!(
        ready,
        "mock_pi 应在真实 sandbox-exec 包裹下收到 prompt 并跑到 ready（agent_end）"
    );
}

/// 第 2 层证明：走真正的生产入口 `handle_maker_request("__host_maker_preview__", ...)`。
/// 合法草稿 → `{ok:true}`，且预览全程不往 registry 写任何东西——预览与安装
/// （`__host_maker_install__`）不同，甚至不产生"待确认"的 pending 状态。
///
/// 顺带核验"会话跑完即收"（T6 brief 的预览会话追踪选择）：`spawn_preview_session`
/// 在 `staging_dir` 内创建的隐藏临时子目录（agent home / session 存储）应在
/// 函数返回前被清理干净——`staging_dir` 下的目录项集合应与预览前完全一致，
/// 不应残留任何多出来的隐藏目录（否则将来 `__host_maker_install__` 会把这些
/// 纯运行期产物一并复制进最终安装包，见 `session_mgr.rs` 文档记录的污染风险）。
#[tokio::test]
async fn preview_via_maker_request_reaches_ok_true_and_registry_stays_empty() {
    let _guard = ENV_LOCK.lock().await;
    std::env::set_var("SUPERAGENT_PI_BIN", env!("CARGO_BIN_EXE_mock_pi"));

    let (_tmp, layout, manager) = temp_layout();
    let registry = RegistryStore::new(layout.registry_path());
    write_valid_maker_draft(&layout, "draft-preview-e2e");
    let staging_dir = layout.maker_staging_dir("draft-preview-e2e");
    let entries_before = dir_entry_names(&staging_dir);

    let resp = handle_maker_request(
        "app-1",
        "__host_maker_preview__",
        serde_json::json!({ "draft_id": "draft-preview-e2e" }),
        &layout,
        &manager,
    )
    .await;

    std::env::remove_var("SUPERAGENT_PI_BIN");

    assert_eq!(
        resp["ok"],
        serde_json::json!(true),
        "合法草稿预览应成功，实际：{resp:?}"
    );

    // 预览不是安装：既不应该出现在已装 registry 里，也不应该像 install 分支那样
    // 留下任何 pending_confirm 状态。
    assert!(
        registry.load().is_empty(),
        "预览绝不能把草稿写入已装应用 registry"
    );
    assert!(
        resp.get("pending_confirm").is_none(),
        "预览不应产生任何 pending_confirm 状态"
    );

    let entries_after = dir_entry_names(&staging_dir);
    assert_eq!(
        entries_after, entries_before,
        "预览会话结束后 staging_dir 的目录项应恢复到预览前的样子（隐藏的 agent home/session \
         子目录必须被清理，否则会污染后续 install 的复制结果）"
    );
}

/// 非法草稿（缺 `superagent` 块）→ fail-closed，`{ok:false,error}`，同样不触碰
/// registry——preview 分支的前置校验（`pkg::load_and_validate`）与
/// `handle_install` 同款，理由见 `maker.rs::handle_preview` 文档。
#[tokio::test]
async fn preview_invalid_draft_rejected_without_touching_registry() {
    // 刻意不设置 SUPERAGENT_PI_BIN：若 fail-closed 顺序被打破、代码提前尝试拉起
    // 预览会话，默认 `resolve_pi_bin()` 会回退到裸名 "pi"——本机很可能没有能被
    // 直接 spawn 的 "pi" 在 PATH 上，若真的走到了 spawn 这一步会因为大概率报错
    // 而不是本测试期望的"因草稿本身非法而拒绝"间接暴露这个顺序 bug，而不是
    // 静默通过。
    let (_tmp, layout, manager) = temp_layout();
    let registry = RegistryStore::new(layout.registry_path());
    let dir = layout.maker_staging_dir("draft-preview-bad");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("package.json"),
        r#"{
          "name": "@superagent/maker-preview-bad", "version": "1.0.0",
          "keywords": ["pi-package", "superagent-app"],
          "engines": { "superagent-host": ">=1.0.0, <2.0.0" }
        }"#,
    )
    .unwrap();

    let resp = handle_maker_request(
        "app-1",
        "__host_maker_preview__",
        serde_json::json!({ "draft_id": "draft-preview-bad" }),
        &layout,
        &manager,
    )
    .await;

    assert_eq!(
        resp["ok"],
        serde_json::json!(false),
        "非法草稿应失败，实际：{resp:?}"
    );
    assert!(resp.get("error").is_some(), "非法草稿应带 error 字段");
    assert!(registry.load().is_empty(), "非法草稿预览不应写入 registry");
}

/// 恶意 `draft_id`（含 `..`）必须在拼 `maker_staging_dir` 之前被拒绝——同
/// `stage_write`/`install` 的同款 guard（`validate_draft_id_is_single_safe_segment`），
/// preview 分支复用而非重新实现。
#[tokio::test]
async fn preview_rejects_malicious_draft_id_without_spawning() {
    let (tmp, layout, manager) = temp_layout();
    let registry = RegistryStore::new(layout.registry_path());

    let resp = handle_maker_request(
        "app-1",
        "__host_maker_preview__",
        serde_json::json!({ "draft_id": "../evil" }),
        &layout,
        &manager,
    )
    .await;

    assert_eq!(
        resp["ok"],
        serde_json::json!(false),
        "恶意 draft_id 应失败，实际：{resp:?}"
    );
    assert!(resp.get("error").is_some());
    assert!(
        !tmp.path().join("evil").exists(),
        "不应在 host root 下创建 evil 目录/文件"
    );
    assert!(registry.load().is_empty());
}

fn dir_entry_names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
        .collect();
    names.sort();
    names
}
