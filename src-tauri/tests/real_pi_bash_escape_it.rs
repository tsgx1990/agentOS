// P0 "sandbox-escape" 手工里程碑的无 key、无 GUI 自动化证明（headless、keyless）。
//
// 背景：`tests/mock_malicious_fixture_it.rs` 顶部注释记录了这个里程碑原本的形态——
// 真实 pi + 真实模型装 `mock-malicious`，人工触发 `escape_attempt`，人工确认三类
// 越权操作被真实 `/usr/bin/sandbox-exec` 挡住。那份验证需要（a）一个真实模型 API
// key 让模型自己决定调用这个工具，（b）一个能看输出的 GUI。本文件在这两个都没有
// 的前提下，把能自动化验证的部分钉成测试。
//
// **Crux（已在此前调研阶段确认，穷举自 vendored 包
// `dist/modes/rpc/rpc-types.d.ts` 的 `RpcCommand` 联合类型 + `dist/modes/rpc/
// rpc-mode.js` 的 `handleCommand` 实现本体，非仅读 `docs/rpc.md` 散文描述）**：
// RPC 输入命令集合是 `prompt` / `steer` / `follow_up` / `abort` / `new_session` /
// `get_state` / `set_model` / `cycle_model` / `get_available_models` /
// `set_thinking_level` / `cycle_thinking_level` / `set_steering_mode` /
// `set_follow_up_mode` / `compact` / `set_auto_compaction` / `set_auto_retry` /
// `abort_retry` / `bash` / `abort_bash` / `get_session_stats` / `export_html` /
// `switch_session` / `fork` / `clone` / `get_fork_messages` /
// `get_last_assistant_text` / `set_session_name` / `get_messages` /
// `get_commands`——**没有任何"按名字直接调用一个已注册工具"的命令**，工具只能由
// 模型在一次 `prompt`/`steer`/`follow_up` 触发的回合里自己决定调用。
//
// 但其中 `type:"bash"` 是个例外：服务端实现（`rpc-mode.js`）是
// `case "bash": { const result = await session.executeBash(command.command);
// return success(id, "bash", result); }`——`session.executeBash` 直接调用本地
// shell 执行器（`executeBashWithOperations`/`createLocalBashOperations`，见
// `dist/core/bash-executor.d.ts`/`agent-session.js`），全程不经过模型、不经过
// 工具调用/权限门（不产生 `tool_execution_end` 事件——`docs/rpc.md` "Bash #bash"
// 原文："This message does NOT emit an event"），因此**不需要任何 API key**。
// 请求/响应精确形状：
//   请求：`{"id"?:string,"type":"bash","command":string}`
//   响应：`{"id"?:string,"type":"response","command":"bash","success":true,
//          "data":{"output":string,"exitCode":number|undefined,
//                   "cancelled":bool,"truncated":bool,"fullOutputPath"?:string}}`
//   （`success:false` 时是 `{"id"?,"type":"response","command":string,
//     "success":false,"error":string}`）
// 完成信号就是这一条 `response` 本身——`bash` 不像 `prompt` 那样流式发
// `message_update`/`agent_end`；`id`（若请求带了）原样回显，供多条依次下发的
// `bash` 命令互相关联响应。
//
// 因此：真实 pi 的 `bash` 能力本质是"当前 node 进程发起的一次本地子进程
// exec/shell"——如果把整棵 pi/node 进程树包进真实 `/usr/bin/sandbox-exec`
// （`session_mgr::sandboxed_argv`/`sandbox::build_profile` 生产路径，与
// `open_app`/`spawn_app_session` 用的同一份，不是重新拼一份等价逻辑），
// `bash` 命令跑的越权操作应该被同一套 SBPL 挡住——这与
// `tests/sandbox_escape_it.rs`（直接 `/bin/sh -c` 跑越权操作）证明的是同一条
// 沙盒边界，区别只是这次越权操作是**真实 pi 在其真实 RPC 主循环里**转发出去的，
// 而不是测试自己拼的 shell 命令。
//
// **残留（本文件无法覆盖，需要真人 + 真 key + GUI 才能补齐的那一环）**：
// mock-malicious 的 `escape_attempt` 第三方**工具**要被**调用**，仍然只能由模型
// 在一次 `prompt` 回合里自己决定——上面穷举过的 27 个 `RpcCommand` 变体里没有
// 任何一个能"按名字直接调用一个已注册工具"。这是模型决策本身的问题，不是沙盒
// 边界问题，无法也不需要在无 key 前提下证明。本文件证明的是更强的邻近命题：
// "如果 escape_attempt 被调用了，它试图做的那三类越权操作会被同一个沙盒挡住"——
// 用真实 pi 的 `bash` 执行器在同一份生产沙盒 profile 下做完全同款的三类越权操作。
//
// 曾探索过、已确认不可行的旁路：能否从**另一个**同进程内的伴生扩展
// monkey-patch `pi.registerTool` 来截获 `escape.ts` 注册的 `execute` 闭包、绕过
// 模型直接调用——不行：每个扩展拿到的 `pi`（ExtensionAPI）实例互相隔离，一个
// 扩展重新赋值 `pi.registerTool` 不会影响另一个扩展看到的 `registerTool` 调用。
// 这本身是一个值得记的正面发现：pi 的扩展隔离阻止了"伴生扩展劫持另一个扩展的
// 工具执行"这条路，但也意味着它不能被用来当作绕过模型回合的手段。

#![cfg(target_os = "macos")]

use std::path::PathBuf;
use std::time::Duration;

use super_agent_os::rpc::{PiEvent, RpcSession};
use super_agent_os::session_mgr::sandboxed_argv;

/// 解析真实 pi 二进制路径：优先 `SUPERAGENT_PI_BIN` 环境变量（CI 用它指向
/// `scripts/fetch-pi.sh` 下载的独立二进制），没设或指向的文件不存在则退化到 `PATH`
/// 查找；两者都没有返回 `None`——调用方据此打印诊断后提前返回（不是 panic），因为不是
/// 所有开发机/CI 都装了真实 pi（本文件全部测试因此标 `#[ignore]`，见各测试注释）。
fn real_pi_bin() -> Option<PathBuf> {
    if let Some(from_env) = std::env::var_os("SUPERAGENT_PI_BIN").map(PathBuf::from) {
        if from_env.is_file() {
            return Some(from_env);
        }
    }
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        let cand = dir.join("pi");
        if cand.is_file() {
            return Some(cand);
        }
    }
    None
}

/// 一个独立沙盒化真实 pi 会话：`$APP_DATA`（唯一可写 subpath）+ pi 专属的
/// agent 配置目录/session 目录（均落在 `$APP_DATA` 内，见 `spawn` 文档）。
struct SandboxedPi {
    // 只是为了在整个测试期间保活这个临时目录（RAII），从不直接读它——真正用到
    // 的是下面 canonicalize 过的 `app_data_canon`。
    _app_data: tempfile::TempDir,
    app_data_canon: PathBuf,
    session: RpcSession,
    rx: tokio::sync::mpsc::Receiver<PiEvent>,
}

impl SandboxedPi {
    /// 生产同款调用：`trusted=false`（未受信第三方应用）、`mcp_socket=None`
    /// （本里程碑不涉及 MCP）——直接复用 `session_mgr::sandboxed_argv`
    /// （与 `spawn_app_session` 调用的同一个生产函数，不是重新拼一份等价逻辑）。
    ///
    /// `PI_CODING_AGENT_DIR`/session 目录都显式落在 `$APP_DATA` 内：不这样做的话
    /// 真实 pi 会尝试读/写 `~/.pi/agent/...`（宿主真实 home 目录，不在
    /// `BASE_PROFILE` 只读白名单内的路径），造成一个与"越权是否被挡"完全无关的
    /// 启动期失败，掩盖本文件真正要证明的东西。
    async fn spawn(extra_args: Vec<String>) -> Result<Self, String> {
        let pi_bin = real_pi_bin()
            .ok_or("本机未找到真实 pi 二进制（SUPERAGENT_PI_BIN 与 PATH 均未命中）")?;
        std::env::set_var("SUPERAGENT_PI_BIN", &pi_bin);

        let app_data = tempfile::tempdir().map_err(|e| e.to_string())?;
        let app_data_canon = std::fs::canonicalize(app_data.path()).map_err(|e| e.to_string())?;
        let agent_dir = app_data_canon.join("pi_agent_dir");
        let session_dir = app_data_canon.join("session_dir");
        std::fs::create_dir_all(&agent_dir).map_err(|e| e.to_string())?;
        std::fs::create_dir_all(&session_dir).map_err(|e| e.to_string())?;

        let mut args = vec!["--no-session".to_string(), "--offline".to_string()];
        args.extend(extra_args);

        let (bin, argv) = sandboxed_argv(&app_data_canon, false, &args, None, &[], &[])?;
        if bin != "/usr/bin/sandbox-exec" {
            return Err(format!("sandboxed_argv 未返回真实 sandbox-exec：{bin}"));
        }

        let env = vec![(
            "PI_CODING_AGENT_DIR".to_string(),
            agent_dir.to_string_lossy().to_string(),
        )];
        let (session, rx) =
            RpcSession::spawn_wrapped(&bin, argv, &session_dir, env, Some(&app_data_canon)).await?;

        Ok(Self {
            _app_data: app_data,
            app_data_canon,
            session,
            rx,
        })
    }

    /// 发一条 `type:"bash"` 命令并等待带同一个 `id` 的 `response`；超时/流结束
    /// 返回 `None`。中间夹杂的其它事件（如另一条命令的响应）只跳过，不影响判定。
    async fn bash(
        &mut self,
        id: &str,
        command: &str,
        timeout: Duration,
    ) -> Option<serde_json::Value> {
        self.session.send_bash(id, command).await.ok()?;
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                return None;
            }
            match tokio::time::timeout(remaining, self.rx.recv()).await {
                Ok(Some(PiEvent::Other(v))) => {
                    if v["type"] == "response" && v["command"] == "bash" && v["id"] == id {
                        return Some(v);
                    }
                }
                Ok(Some(_)) => continue,
                Ok(None) => return None,
                Err(_) => return None,
            }
        }
    }

    async fn kill(mut self) {
        self.session.kill().await;
    }
}

/// 前置：证明真实 pi（不带任何第三方扩展）能在生产 `sandboxed_argv` 未受信
/// profile 下正常起 rpc 会话并响应一条 `type:"bash"` 命令——排除"下面的越权测试
/// 失败是因为沙盒把 pi 自己都挡死了"这种误判可能性。全程只发 `bash`（不触发模型、
/// 不需要 API key）。
#[tokio::test]
#[ignore = "手工里程碑自动化：依赖真实 pi 装在本机(或 PATH)，用 `cargo test -- --ignored` 显式跑"]
async fn real_pi_bare_starts_and_responds_to_bash_under_production_untrusted_sandbox() {
    let mut sb = match SandboxedPi::spawn(vec![]).await {
        Ok(sb) => sb,
        Err(e) => {
            eprintln!("跳过 real_pi_bare_starts_and_responds_to_bash：{e}");
            return;
        }
    };

    let resp = sb
        .bash("preflight-echo", "echo ready", Duration::from_secs(20))
        .await
        .expect(
            "真实 pi 在生产未受信沙盒 profile 下应能响应 type:bash 命令\
             （若超时：先看是不是扩展/启动失败，检查 stderr 诊断）",
        );
    assert_eq!(resp["success"], true, "bash 命令应成功：{resp}");
    let output = resp["data"]["output"].as_str().unwrap_or("");
    assert!(
        output.contains("ready"),
        "bash 输出应包含 echo 的内容：{resp}"
    );
    assert_eq!(
        resp["data"]["exitCode"].as_i64(),
        Some(0),
        "echo 应成功退出：{resp}"
    );

    sb.kill().await;
}

/// **主证明（本里程碑的头条结果）**：真实 pi 在 `--mode rpc` 下，于**生产**
/// untrusted-app 沙盒 profile（`session_mgr::sandboxed_argv(app_data,
/// trusted=false, ...)`）下，收到 `type:"bash"` 命令后：
///
/// 0. 正对照：在 `$APP_DATA` 内写文件 —— 应成功（证明真实 pi 的 bash 执行器本身
///    工作正常，后面的失败是沙盒挡的，不是 pi 的 bash 坏了）。
/// 1. 写 `$APP_DATA` 外的 `/tmp` —— 应被拒（非零退出码）。
/// 2. 读取既非 `$APP_DATA` 也非 `runtime_paths` 的内容 —— 应被拒（非零退出码，
///    且内容不出现在 bash 输出里）。特意不用 `/etc/hosts`：那条路径在
///    `BASE_PROFILE` 下本来就允许读（node/dyld 启动需要），用它会把"沙盒本来就
///    该放行的路径"误判成"沙盒没挡住"。
/// 3. 联网（`nc` 直连字面 IP，避免 DNS 解析失败/curl 缺失等环境因素造成假阳性，
///    与 `sandbox_escape_it.rs::network_denied` 同一手法）—— 应被拒。
///
/// 全程不发送任何 `prompt`/`steer`/`follow_up`——没有任何模型回合，不需要任何
/// API key。宿主视角（子进程之外）额外核验：`/tmp` 下不应残留任何越权写入的
/// 标记文件——不只信任子进程自述的 `exitCode`。
#[tokio::test]
#[ignore = "手工里程碑自动化：依赖真实 pi 装在本机(或 PATH)，用 `cargo test -- --ignored` 显式跑"]
async fn real_pi_bash_escape_attempts_blocked_under_production_untrusted_sandbox() {
    let mut sb = match SandboxedPi::spawn(vec![]).await {
        Ok(sb) => sb,
        Err(e) => {
            eprintln!("跳过 real_pi_bash_escape_attempts_blocked：{e}");
            return;
        }
    };

    let marker = format!(
        "superagent-escaped-{}-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
        std::process::id()
    );
    let escape_path = format!("/tmp/{marker}");

    // 独立于 $APP_DATA/runtime_paths 的秘密文件——既非 app_data 也非 pi/node
    // 安装前缀，其内容读取应被拒（见 sandbox.rs 文档与
    // sandbox_escape_it.rs::read_other_content_denied 同款用例）。
    let secret_dir = tempfile::tempdir().unwrap();
    let secret_path = secret_dir.path().join("secret.txt");
    std::fs::write(&secret_path, "top-secret-do-not-leak").unwrap();

    let positive_control_path = sb.app_data_canon.join("bash_positive_control.txt");

    // --- 0) 正对照：$APP_DATA 内写应成功 ------------------------------------
    let ctrl = sb
        .bash(
            "ctrl-write-inside",
            &format!(
                "echo positive-control-ok > {}",
                positive_control_path.display()
            ),
            Duration::from_secs(20),
        )
        .await
        .expect("正对照命令应收到响应");
    assert_eq!(
        ctrl["data"]["exitCode"].as_i64(),
        Some(0),
        "$APP_DATA 内写应成功（正对照，证明真实 pi 的 bash 执行器本身工作正常）：{ctrl}"
    );

    // --- 1) 写 $APP_DATA 外的 /tmp —— 应被拒 --------------------------------
    let write_outside = sb
        .bash(
            "escape-write-outside",
            &format!("echo escaped > {escape_path}"),
            Duration::from_secs(20),
        )
        .await
        .expect("write_outside 命令应收到响应");
    assert_ne!(
        write_outside["data"]["exitCode"].as_i64(),
        Some(0),
        "写 $APP_DATA 外(/tmp)应被沙盒拒绝，非零退出：{write_outside}"
    );

    // --- 2) 读取沙盒外内容 —— 应被拒 ----------------------------------------
    let read_other = sb
        .bash(
            "escape-read-other",
            &format!("cat {}", secret_path.display()),
            Duration::from_secs(20),
        )
        .await
        .expect("read_other 命令应收到响应");
    assert_ne!(
        read_other["data"]["exitCode"].as_i64(),
        Some(0),
        "读取沙盒外内容应被拒，非零退出：{read_other}"
    );
    let read_output = read_other["data"]["output"].as_str().unwrap_or("");
    assert!(
        !read_output.contains("top-secret-do-not-leak"),
        "读取应被拒的内容不应出现在 bash 输出里：{read_other}"
    );

    // --- 3) 联网 —— 应被拒 ---------------------------------------------------
    let network = sb
        .bash(
            "escape-network",
            "nc -w2 -z 1.1.1.1 80",
            Duration::from_secs(20),
        )
        .await
        .expect("network 命令应收到响应");
    assert_ne!(
        network["data"]["exitCode"].as_i64(),
        Some(0),
        "联网应被拒，非零退出：{network}"
    );

    // --- 宿主视角核验（不只信任子进程自述）——必须在 `sb.kill()` 之前做：
    // `SandboxedPi::kill` 按值消费 `self`，会一并 drop 掉 `_app_data: TempDir`
    // 字段，其 `Drop` 实现会递归删除整个 `$APP_DATA` 临时目录（含这里要核验的
    // 正对照文件）——踩过的坑：曾经先 kill 再核验，导致目录已被清空，
    // 误判成"沙盒挡住了合法的 $APP_DATA 内写"。
    assert!(
        !std::path::Path::new(&escape_path).exists(),
        "宿主核验：/tmp 下不应出现越权写入的标记文件（实际存在说明沙盒没挡住）"
    );
    assert!(
        std::fs::read_to_string(&positive_control_path)
            .map(|s| s.contains("positive-control-ok"))
            .unwrap_or(false),
        "宿主核验：正对照写入应确实落在 $APP_DATA 内"
    );

    sb.kill().await;
}

/// Step 4（bonus，keyless）：真实 pi 在同一份生产 untrusted 沙盒 profile 下，
/// 加载真实第三方扩展 `mock-malicious/escape.ts`（原样字节拷贝，未改一个字节），
/// 证明其 `escape_attempt` 工具确实被注册——全程不发送任何 prompt/steer/
/// follow_up，即没有任何模型回合、不需要任何 API key。
///
/// RPC 协议里没有任何"列出已注册工具"的命令（见文件顶部穷举），唯一能在无模型
/// 回合前提下内省工具注册情况的办法，是用一个测试专用的伴生"探针"扩展
/// （`tests/fixtures/rpc-probe/tool_registration_probe.ts`）：它在 `session_start`
/// 钩子（pi 完成所有 `-e` 扩展的工厂函数之后、且早于任何 prompt 处理之前触发）里
/// 调用 `pi.getAllTools()` 检查 `escape_attempt` 是否已注册、其 `sourceInfo.path`
/// 是否确实指向 escape.ts，再用 `ctx.ui.notify()`（fire-and-forget，RPC 模式下
/// 原样序列化成 stdout 上的 `extension_ui_request` 事件）上报。
///
/// 踩坑记录：escape.ts 物理文件在 `tests/fixtures/mock-malicious/agent/
/// extensions/` 下（仓库路径），既不在 `$APP_DATA`（`sandboxed_argv` 的唯一
/// WRITE 根）也不在 `runtime_paths`（pi/node 安装前缀）——生产 `sandboxed_argv`
/// 现状（`read_paths` 硬编码传空）下，pi 读不到这个路径的 `-e` 扩展
/// （`fs.existsSync` 类检查因 EPERM 返回 false，pi 报 "Extension path does not
/// exist"）。为了不让这个与本测试无关的变量掩盖真正要证明的东西，这里把
/// escape.ts**原样字节拷贝**（不改一个字节）到 `$APP_DATA` 内再 `-e` 加载。
///
/// 残留：见文件顶部——工具被**调用**仍然只能由模型决定，这条测试只证明"加载 +
/// 注册"这一环。
#[tokio::test]
#[ignore = "手工里程碑自动化：依赖真实 pi 装在本机(或 PATH)，用 `cargo test -- --ignored` 显式跑"]
async fn real_pi_loads_mock_malicious_extension_and_registers_escape_attempt_under_sandbox() {
    let repo_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let escape_ts_src = repo_root.join("tests/fixtures/mock-malicious/agent/extensions/escape.ts");
    let probe_ts_src = repo_root.join("tests/fixtures/rpc-probe/tool_registration_probe.ts");
    assert!(
        escape_ts_src.is_file(),
        "mock-malicious 的 escape.ts fixture 必须存在"
    );
    assert!(probe_ts_src.is_file(), "探针扩展必须存在");

    let pi_bin = match real_pi_bin() {
        Some(p) => p,
        None => {
            eprintln!("跳过 real_pi_loads_mock_malicious_extension：本机未找到真实 pi 二进制");
            return;
        }
    };
    std::env::set_var("SUPERAGENT_PI_BIN", &pi_bin);

    let app_data = tempfile::tempdir().unwrap();
    let app_data_canon = std::fs::canonicalize(app_data.path()).unwrap();
    let agent_dir = app_data_canon.join("pi_agent_dir");
    let session_dir = app_data_canon.join("session_dir");
    std::fs::create_dir_all(&agent_dir).unwrap();
    std::fs::create_dir_all(&session_dir).unwrap();

    let ext_dir = app_data_canon.join("ext");
    std::fs::create_dir_all(&ext_dir).unwrap();
    let escape_ts = ext_dir.join("escape.ts");
    let probe_ts = ext_dir.join("tool_registration_probe.ts");
    std::fs::copy(&escape_ts_src, &escape_ts).expect("拷贝真实 escape.ts 不应失败");
    std::fs::copy(&probe_ts_src, &probe_ts).expect("拷贝探针扩展不应失败");

    let extra_args = vec![
        "--no-session".to_string(),
        "--offline".to_string(),
        "-e".to_string(),
        escape_ts.to_string_lossy().to_string(),
        "-e".to_string(),
        probe_ts.to_string_lossy().to_string(),
    ];
    let (bin, argv) = sandboxed_argv(&app_data_canon, false, &extra_args, None, &[], &[])
        .expect("sandboxed_argv 不应失败");
    assert_eq!(bin, "/usr/bin/sandbox-exec");

    let env = vec![(
        "PI_CODING_AGENT_DIR".to_string(),
        agent_dir.to_string_lossy().to_string(),
    )];
    let (session, mut rx) =
        RpcSession::spawn_wrapped(&bin, argv, &session_dir, env, Some(&app_data_canon))
            .await
            .expect("真实 pi 应能在生产未受信沙盒 profile 下 spawn 成功（加载两个 -e 扩展）");

    // 关键：从头到尾不发送 prompt/steer/follow_up——探针的 session_start 钩子在
    // pi 完成所有扩展工厂函数之后自动触发，不需要我们发任何命令去"启动"它。
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    let mut payload: Option<serde_json::Value> = None;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        match tokio::time::timeout(remaining, rx.recv()).await {
            Ok(Some(PiEvent::Other(v))) => {
                if v["type"] == "extension_ui_request" && v["method"] == "notify" {
                    if let Some(msg) = v["message"].as_str() {
                        if let Some(rest) = msg.strip_prefix("TOOL_REGISTRATION_PROBE_RESULT:") {
                            payload = serde_json::from_str(rest).ok();
                            break;
                        }
                    }
                }
            }
            Ok(Some(_)) => continue,
            Ok(None) => break,
            Err(_) => break,
        }
    }

    let mut session = session;
    session.kill().await;

    let payload = payload.expect(
        "应在 20s 内收到探针的 TOOL_REGISTRATION_PROBE_RESULT notify\
         （若超时：先看是不是扩展加载失败，检查 stderr/extension_error 诊断）",
    );
    eprintln!("TOOL_REGISTRATION_PROBE_RESULT payload: {payload}");
    assert_eq!(
        payload["escapeAttemptRegistered"], true,
        "escape_attempt 工具应已被真实 pi 注册：{payload}"
    );
    let source_path = payload["escapeAttemptSourcePath"]
        .as_str()
        .expect("sourceInfo.path 应是字符串");
    assert_eq!(
        std::path::Path::new(source_path),
        escape_ts.as_path(),
        "escape_attempt 的 sourceInfo.path 应精确指向拷贝进沙盒的那份 escape.ts"
    );
}
