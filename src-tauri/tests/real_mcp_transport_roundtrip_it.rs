// P3 里程碑验收：真实 MCP 传输往返（headless & keyless）。
//
// 现有自动化测试各自只验证半条链路：
// - `tests/mcp_socket_it.rs` 用一个**手写复刻协议的假客户端**去打真实的
//   `McpSocketListener`——验证了监听器这一侧，但从未跑过真实的
//   `hosttools/mcp_transport.ts::hostMcpCall`。
// - `hosttools/mcp_bridge.test.ts` 整体 `vi.mock` 掉了 `mcp_transport.ts`——
//   验证了扩展逻辑这一侧，但从未连过真实 socket。
//
// 本文件补上中间缺的那一段：真实 TS 客户端 ↔ 真实 unix socket ↔ 真实
// `McpSocketListener` ↔ 真实 `McpManager::host_mcp_call` ↔ 真实
// `mock_mcp_server`，端到端跑一遍，证明两侧讲的是同一套线协议。
//
// 无 key、无模型：`hostMcpCall`（见 `hosttools/mcp_transport.ts` 头部注释）
// 本身只需要一个 socket 路径（`SUPERAGENT_MCP_SOCKET`），不碰 pi 子进程、不碰
// 任何 vault/keychain——`ServerConfig` 在本文件里直接手工构造（同
// `mcp_socket_it.rs`/`mcp_manager_it.rs` 的一贯做法，避免碰真实 keychain 卡死
// `cargo test`）。
//
// 跑法：真实 TS 客户端源码本身完全不改——用仓库根 `node_modules/.bin/esbuild`
// （`npm ci` 产物，`mcp_transport.ts` 只 import 内置的 `node:net`，打包对运行时
// 行为是恒等变换）把 `mcp_transport.ts` 打包成一个自包含 ESM bundle，再用一个
// 几行的 `.mjs` 驱动脚本 `import()` 这个 bundle、调用 `hostMcpCall`、把
// resolve/reject 的结果编码成一行 JSON 打到 stdout。Rust 测试 spawn 真实的
// `node` 子进程跑这个驱动脚本、解析它的 stdout——断言的是真实 TS 代码的真实
// 运行结果，不是 Rust 侧重新实现一遍协议自己验证自己。
//
// `#[ignore]`：需要本机 `node`（PATH 可达）与仓库根 `npm ci` 产物
// `node_modules/.bin/esbuild`——CI 环境可能没装，不放进默认 `cargo test`。
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use super_agent_os::mcp::McpManager;
use super_agent_os::mcp_socket::McpSocketListener;
use super_agent_os::paths::DataLayout;
use super_agent_os::permissions::{Access, ConnectorReq};
use super_agent_os::vault::ServerConfig;

fn temp_layout() -> (tempfile::TempDir, DataLayout) {
    let tmp = tempfile::tempdir().unwrap();
    let layout = DataLayout::new(tmp.path().to_path_buf());
    (tmp, layout)
}

fn mock_server_config_with_category(id: &str, category: &str) -> ServerConfig {
    ServerConfig {
        id: id.to_string(),
        category: category.to_string(),
        command: env!("CARGO_BIN_EXE_mock_mcp_server").to_string(),
        args: vec![],
        env: BTreeMap::new(),
        transport: "stdio".into(),
        trust: Default::default(),
    }
}

/// `CARGO_MANIFEST_DIR` 是 `<仓库根>/src-tauri`，仓库根（放 `node_modules/`、
/// `hosttools` 的上一级）就是它的父目录。
fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("src-tauri 应有父目录（仓库根）")
        .to_path_buf()
}

/// 用仓库根已装好的 esbuild 把真实的 `hosttools/mcp_transport.ts` 打包成一个
/// 自包含 ESM bundle（该文件只 import 内置的 `node:net`，打包是恒等变换，不
/// 会改变 `hostMcpCall` 的任何运行时行为）。返回 bundle 文件路径。
fn bundle_real_mcp_transport(out_dir: &Path) -> PathBuf {
    let esbuild = repo_root().join("node_modules/.bin/esbuild");
    assert!(
        esbuild.exists(),
        "找不到 {esbuild:?}——先在仓库根跑一次 `npm ci`（esbuild 是根 devDependency）"
    );
    let ts_file = repo_root().join("src-tauri/hosttools/mcp_transport.ts");
    assert!(ts_file.exists(), "找不到真实的 {ts_file:?}");

    let out_file = out_dir.join("mcp_transport.bundle.mjs");
    let status = Command::new(&esbuild)
        .arg(&ts_file)
        .arg("--bundle")
        .arg("--format=esm")
        .arg("--platform=node")
        .arg(format!("--outfile={}", out_file.display()))
        .status()
        .unwrap_or_else(|e| panic!("esbuild 应能执行：{e}"));
    assert!(status.success(), "esbuild 打包真实 mcp_transport.ts 应成功");
    out_file
}

/// 几行的 node 驱动脚本：`import()` 打包后的真实 `hostMcpCall`，调用一次，把
/// resolve 的结果或 reject 的错误编码成一行 JSON 打到 stdout。脚本本身不含
/// 任何 MCP 线协议逻辑——协议逻辑全部来自被 `import()` 进来的真实
/// `mcp_transport.ts` 代码，这里只是给它接上一个可从命令行驱动的外壳。
const DRIVER_JS: &str = r#"
import { pathToFileURL } from "node:url";

const [, , bundlePath, server, tool, argsJson] = process.argv;
const { hostMcpCall } = await import(pathToFileURL(bundlePath).href);
const args = JSON.parse(argsJson);

try {
  const result = await hostMcpCall({ server, tool, args });
  process.stdout.write(JSON.stringify({ ok: true, result }) + "\n");
} catch (e) {
  process.stdout.write(JSON.stringify({ ok: false, error: String(e && e.message ? e.message : e) }) + "\n");
}
"#;

fn write_driver(out_dir: &Path) -> PathBuf {
    let path = out_dir.join("driver.mjs");
    std::fs::write(&path, DRIVER_JS).expect("写驱动脚本应成功");
    path
}

/// 真正 spawn 一个真实 `node` 子进程跑驱动脚本：`SUPERAGENT_MCP_SOCKET` 经 env
/// 注入（与生产环境 `session_mgr` 给 pi 子进程注入这个变量的方式完全一致，
/// 唯一区别是这次的"子进程"是驱动脚本而不是 pi）。同步阻塞调用（`node` 进程
/// 跑完即返回），调用方必须放进 `tokio::task::spawn_blocking`——否则会在
/// `#[tokio::test]` 默认的单线程 runtime 上把这次阻塞式子进程等待和处理这次
/// socket 连接的 accept/handle_conn 异步任务锁死在同一条 OS 线程上，谁都等不到
/// 谁（node 子进程连上 socket 等回应，Rust 这侧的 accept 循环却因为当前线程被
/// 这次同步 `Command::output()` 占住而永远排不上调度）。
fn call_real_ts_host_mcp(
    driver: PathBuf,
    bundle: PathBuf,
    socket_path: PathBuf,
    server: String,
    tool: String,
    args: serde_json::Value,
) -> serde_json::Value {
    let output = Command::new("node")
        .arg(&driver)
        .arg(&bundle)
        .arg(&server)
        .arg(&tool)
        .arg(args.to_string())
        .env("SUPERAGENT_MCP_SOCKET", &socket_path)
        .output()
        .unwrap_or_else(|e| panic!("spawn 真实 node 子进程应成功（PATH 里应有 node）：{e}"));

    assert!(
        output.status.success(),
        "node 驱动进程应正常退出，stdout={}, stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let last_line = stdout
        .lines()
        .next_back()
        .unwrap_or_else(|| panic!("node 驱动脚本应至少打印一行 JSON，实际 stdout={stdout:?}"));
    serde_json::from_str(last_line)
        .unwrap_or_else(|e| panic!("驱动脚本输出应是合法 JSON，实际 {last_line:?}：{e}"))
}

/// 场景 1（headline）：只读授权的真实 TS 客户端对已授权的 `read_file` 发起
/// 调用——真实 `hostMcpCall` 经真实 unix socket 连上真实 `McpSocketListener`，
/// 转发给真实 `McpManager::host_mcp_call`，由真实 `mock_mcp_server` 执行，结果
/// 按线协议编码回去，真实 TS 客户端的 `hostMcpCall` Promise 真的 resolve 出
/// mock server 的 read_file 内容——这是 TS 侧的真实返回值，不是 Rust 侧自证
/// 协议形状对。
#[tokio::test]
#[ignore = "需要本机 node（PATH 可达）+ 仓库根 `npm ci` 产物 esbuild；不放进默认 cargo test"]
async fn real_ts_client_read_file_roundtrips_through_real_listener_and_mock_server() {
    let tmp = tempfile::tempdir().unwrap();
    let bundle = bundle_real_mcp_transport(tmp.path());
    let driver = write_driver(tmp.path());

    let manager = McpManager::new();
    let cfg = mock_server_config_with_category("real-ts-fs-read", "filesystem");
    manager
        .ensure_server(&cfg)
        .await
        .expect("ensure_server 应成功");
    let (_layout_tmp, layout) = temp_layout();
    let socket_path = tmp.path().join("app-real-ts-read").join("mcp.sock");

    let connectors = vec![ConnectorReq {
        category: "filesystem".to_string(),
        access: Access::Read,
    }];
    let listener = McpSocketListener::start(
        manager.clone(),
        layout,
        "app-real-ts-read".to_string(),
        connectors,
        socket_path.clone(),
    )
    .expect("监听器应能成功 bind");

    let response = tokio::task::spawn_blocking({
        let driver = driver.clone();
        let bundle = bundle.clone();
        let socket_path = socket_path.clone();
        let server = cfg.id.clone();
        move || {
            call_real_ts_host_mcp(
                driver,
                bundle,
                socket_path,
                server,
                "read_file".to_string(),
                serde_json::json!({ "path": "/tmp/a.txt" }),
            )
        }
    })
    .await
    .expect("blocking node 调用任务不应 panic");

    assert_eq!(
        response["ok"],
        serde_json::json!(true),
        "真实 TS 客户端应 resolve 而非 reject，实际：{response:?}"
    );
    let result = &response["result"];
    let text = result["content"][0]["text"].as_str().unwrap_or("");
    assert!(
        text.contains("mock content of /tmp/a.txt"),
        "真实 TS 客户端应拿回真实 mock server 的 read_file 内容，实际：{result:?}"
    );
    assert_eq!(
        manager.call_count(&cfg.id),
        1,
        "read_file 应确实经这条真实链路被执行了一次"
    );

    listener.stop().await;
}

/// 场景 2：读写授权的真实 TS 客户端对 `write_file`（`Danger::Write`）发起调用
/// → 应得到 `PendingConfirm` 编码，真实 TS 客户端按 `mcp_transport.ts` 的解码
/// 规则（含 `result` 字段 → resolve）拿到 `{pending_confirm:true, confirm_id,
/// message}`——不是 error（不会被 reject），也不撞 `Ok(read_file)` 的形状。
/// 工具本身在这个分支绝不应被真正执行。
#[tokio::test]
#[ignore = "需要本机 node（PATH 可达）+ 仓库根 `npm ci` 产物 esbuild；不放进默认 cargo test"]
async fn real_ts_client_write_file_decodes_pending_confirm_shape_from_real_listener() {
    let tmp = tempfile::tempdir().unwrap();
    let bundle = bundle_real_mcp_transport(tmp.path());
    let driver = write_driver(tmp.path());

    let manager = McpManager::new();
    let cfg = mock_server_config_with_category("real-ts-fs-write", "filesystem");
    manager
        .ensure_server(&cfg)
        .await
        .expect("ensure_server 应成功");
    let (_layout_tmp, layout) = temp_layout();
    let socket_path = tmp.path().join("app-real-ts-write").join("mcp.sock");

    let connectors = vec![ConnectorReq {
        category: "filesystem".to_string(),
        access: Access::ReadWrite,
    }];
    let listener = McpSocketListener::start(
        manager.clone(),
        layout,
        "app-real-ts-write".to_string(),
        connectors,
        socket_path.clone(),
    )
    .expect("监听器应能成功 bind");

    let response = tokio::task::spawn_blocking({
        let driver = driver.clone();
        let bundle = bundle.clone();
        let socket_path = socket_path.clone();
        let server = cfg.id.clone();
        move || {
            call_real_ts_host_mcp(
                driver,
                bundle,
                socket_path,
                server,
                "write_file".to_string(),
                serde_json::json!({ "path": "/tmp/x", "content": "y" }),
            )
        }
    })
    .await
    .expect("blocking node 调用任务不应 panic");

    assert_eq!(
        response["ok"],
        serde_json::json!(true),
        "PendingConfirm 绝不应让真实 TS 客户端 reject，实际：{response:?}"
    );
    let result = &response["result"];
    assert_eq!(result["pending_confirm"], serde_json::json!(true));
    assert!(result["confirm_id"].as_str().is_some_and(|s| !s.is_empty()));
    // P6-C 裁决2：回执如实说"待批"，不模拟成功——断言新文案的两个关键词
    // （"暂存"+"审批中心"），不再断言笼统的"确认"二字。
    let message = result["message"].as_str().unwrap_or("");
    assert!(
        message.contains("暂存") && message.contains("审批中心"),
        "应带如实说明「待批」+ 指向审批中心的提示文案，实际：{result:?}"
    );
    assert!(
        result.get("content").is_none(),
        "PendingConfirm 解码结果不应和 Ok(read_file) 撞形状，实际：{result:?}"
    );
    assert_eq!(
        manager.call_count(&cfg.id),
        0,
        "PendingConfirm 分支下写操作绝不应被真正执行"
    );

    listener.stop().await;
}

/// 场景 3：只读授权的真实 TS 客户端调用未授权的 `write_file` → host 侧二次
/// 授权复核 Denied，编码进 `error` 字段；真实 TS 客户端按解码规则
/// `reject(new Error(String(parsed.error)))`——驱动脚本的 `catch` 分支真的被
/// 走到，证明 reject 路径同样讲得通这套线协议，不止 resolve 路径。
#[tokio::test]
#[ignore = "需要本机 node（PATH 可达）+ 仓库根 `npm ci` 产物 esbuild；不放进默认 cargo test"]
async fn real_ts_client_unauthorized_write_file_rejects_with_error_from_real_listener() {
    let tmp = tempfile::tempdir().unwrap();
    let bundle = bundle_real_mcp_transport(tmp.path());
    let driver = write_driver(tmp.path());

    let manager = McpManager::new();
    let cfg = mock_server_config_with_category("real-ts-fs-denied", "filesystem");
    manager
        .ensure_server(&cfg)
        .await
        .expect("ensure_server 应成功");
    let (_layout_tmp, layout) = temp_layout();
    let socket_path = tmp.path().join("app-real-ts-denied").join("mcp.sock");

    // 只读授权：write_file 未在授权范围内。
    let connectors = vec![ConnectorReq {
        category: "filesystem".to_string(),
        access: Access::Read,
    }];
    let listener = McpSocketListener::start(
        manager.clone(),
        layout,
        "app-real-ts-denied".to_string(),
        connectors,
        socket_path.clone(),
    )
    .expect("监听器应能成功 bind");

    let response = tokio::task::spawn_blocking({
        let driver = driver.clone();
        let bundle = bundle.clone();
        let socket_path = socket_path.clone();
        let server = cfg.id.clone();
        move || {
            call_real_ts_host_mcp(
                driver,
                bundle,
                socket_path,
                server,
                "write_file".to_string(),
                serde_json::json!({ "path": "/tmp/x", "content": "y" }),
            )
        }
    })
    .await
    .expect("blocking node 调用任务不应 panic");

    assert_eq!(
        response["ok"],
        serde_json::json!(false),
        "未授权调用应让真实 TS 客户端的 hostMcpCall reject，实际：{response:?}"
    );
    assert_eq!(
        response["error"],
        serde_json::json!("unauthorized"),
        "reject 出的 Error.message 应就是宿主编码的原始 reason，实际：{response:?}"
    );
    assert_eq!(
        manager.call_count(&cfg.id),
        0,
        "未授权的写操作绝不应被真正执行"
    );

    listener.stop().await;
}
