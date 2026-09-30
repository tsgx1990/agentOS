use crate::jsonl::FrameBuffer;
use crate::pi_bin::resolve_pi_bin;
use std::path::Path;
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{mpsc, Mutex};

#[derive(Debug, Clone)]
pub enum PiEvent {
    AssistantDelta(String),
    AgentEnded,
    AutoRetry {
        attempt: u32,
        max: u32,
        delay_ms: u64,
    },
    ProviderError(String),
    /// 应用通过宿主工具 `__host_ui_emit__` 主动上报的 UI 观察事件（事件名 + 任意载荷）。
    UiEmit {
        event: String,
        payload: serde_json::Value,
    },
    /// 任意工具（除 `__host_ui_emit__`，已归类为上面的 `UiEmit`）的 `tool_execution_end`：
    /// pi 核心自己在工具**真正执行完**之后产生的事件，`tool_name`/`args` 直接来自这个
    /// 事件本体——不经过、也不依赖 `hosttools/permission_gate.ts` 的 `tool_call` 钩子
    /// 上报。P2 审计接线（`session_mgr.rs`/`lib.rs` 的每会话事件循环）就是从这里调用
    /// `audit::record`，理由见 `docs/superpowers/spikes/2026-07-17-host-hook-priority.md`
    /// §证据6：`tool_call` 钩子的"批准"对最终执行参数不构成任何保证（同进程内后加载的
    /// 另一个包扩展可以在批准之后、执行之前原地改写 `event.input`，无 block、无异常、
    /// 无信号）——`permission_gate.ts` 因此只降级为 advisory/审计层，真正落盘审计的
    /// 参数来源必须是这个不可被它（或任何包扩展）操纵的、pi 核心自产的执行完成事件。
    ToolExecuted {
        tool_name: String,
        args: serde_json::Value,
        is_error: bool,
    },
    /// P3 Task18 修复：对 `{"type":"get_session_stats"}` RPC 命令的响应，携带
    /// pi 会话**累计**（非增量）的 token 用量 + 真实花费。
    ///
    /// Task16 曾假设用量随 `agent_end` 事件到达（`usage: {input_tokens,output_tokens}`），
    /// **该假设已用真实 pi v0.74.2 证伪**：手工读取 npm 包
    /// `@earendil-works/pi-coding-agent` 内的 `dist/core/agent-session.d.ts`
    /// 确认 `AgentEndEvent` 只有 `{type:"agent_end", messages}`，不带任何 `usage`
    /// 字段；真实用量数据在 `AgentSession.getSessionStats()`（`SessionStats` 类型），
    /// 经 RPC 输入命令 `{"type":"get_session_stats"}` 取得。已用真实 pi 二进制
    /// （keyless、`--no-session`、未发送任何 `prompt`，纯 session-local 查询）
    /// 实测抓到的响应线格式：
    /// `{"type":"response","command":"get_session_stats","success":true,"data":{"sessionId":..,"tokens":{"input":..,"output":..,"cacheRead":..,"cacheWrite":..,"total":..},"cost":..,...}}`
    /// ——`classify`/`parse_session_stats` 就是照这个实测形状解析，不再是"最佳猜测"。
    ///
    /// `input`/`output`/`cost` 均为该 pi 会话**当前累计值**（`SessionStats` 语义
    /// 如此，非增量）——消费点（`usage::UsageAccumulator::set_latest`）据此覆盖式
    /// 记录"最新值"，不能像 Task16 的 `Usage` 变体那样累加，否则会重复计数。
    SessionStats {
        input: u64,
        output: u64,
        cost: f64,
    },
    Other(serde_json::Value),
}

pub struct RpcSession {
    stdin: Mutex<ChildStdin>,
    child: Mutex<Child>,
    child_id: Option<u32>,
}

/// 纯函数：判断一行 pi stderr 输出是否是启动期扩展冲突/加载失败的诊断信号。
/// P0 阶段整段丢弃 stderr 内容，这类信息完全不可见；这里只做识别，落地为
/// `eprintln!` 诊断日志，不改变别的行为（仍然排空、不阻塞）。
pub fn is_startup_conflict(line: &str) -> bool {
    line.contains("conflicts with") || line.contains("Failed to load extension")
}

/// 防御性解析一条 `{"type":"get_session_stats"}` 命令的响应对象：必须是
/// `type:"response"` + `command:"get_session_stats"` + `success:true`，否则
/// 返回 `None`（既包括其它命令的响应——如 `bash`——也包括 `success:false` 的
/// 失败响应）。`data.tokens.input`/`data.tokens.output` 解不出数字同样返回
/// `None`；`data.cost` 缺失时防御性退化为 `0.0`（不因为这个次要字段缺失就
/// 丢掉整条已经解出 token 数的事件）。形状对齐 `PiEvent::SessionStats` 文档里
/// 记录的真实 pi 实测响应。
fn parse_session_stats(v: &serde_json::Value) -> Option<(u64, u64, f64)> {
    if v.get("type")?.as_str()? != "response" {
        return None;
    }
    if v.get("command")?.as_str()? != "get_session_stats" {
        return None;
    }
    if v.get("success").and_then(|s| s.as_bool()) != Some(true) {
        return None;
    }
    let tokens = v.get("data")?.get("tokens")?;
    let input = tokens.get("input")?.as_u64()?;
    let output = tokens.get("output")?.as_u64()?;
    let cost = v["data"]
        .get("cost")
        .and_then(|c| c.as_f64())
        .unwrap_or(0.0);
    Some((input, output, cost))
}

fn classify(v: &serde_json::Value) -> PiEvent {
    // 优先级高于下面按 `type` 的分派：`get_session_stats` 的响应本身 `type` 就是
    // "response"，不落在下面任何一个 `agent_end`/`tool_execution_end` 等事件
    // `type` 分支里，必须单独识别（否则会被 `_ => Other` 兜底吞掉）。
    if let Some((input, output, cost)) = parse_session_stats(v) {
        return PiEvent::SessionStats {
            input,
            output,
            cost,
        };
    }
    match v["type"].as_str() {
        Some("message_update") => {
            let ev = &v["assistantMessageEvent"];
            if ev["type"] == "text_delta" {
                if let Some(d) = ev["delta"].as_str() {
                    return PiEvent::AssistantDelta(d.to_string());
                }
            }
            PiEvent::Other(v.clone())
        }
        Some("agent_end") => PiEvent::AgentEnded,
        Some("auto_retry_start") => PiEvent::AutoRetry {
            attempt: v["attempt"].as_u64().unwrap_or(0) as u32,
            max: v["maxAttempts"].as_u64().unwrap_or(0) as u32,
            delay_ms: v["delayMs"].as_u64().unwrap_or(0),
        },
        Some("extension_error") => {
            PiEvent::ProviderError(v["error"].as_str().unwrap_or("").to_string())
        }
        // 宿主工具 `__host_ui_emit__` 执行结束 = 应用主动上报的 UI 观察事件；
        // 其余 tool_execution_end（任意其它工具调用真正执行完毕）归为 ToolExecuted，
        // 供宿主审计接线使用（见 PiEvent::ToolExecuted 文档注释）。
        Some("tool_execution_end") if v["toolName"] == "__host_ui_emit__" => PiEvent::UiEmit {
            event: v["args"]["event"].as_str().unwrap_or("").to_string(),
            payload: v["args"]["payload"].clone(),
        },
        Some("tool_execution_end") => PiEvent::ToolExecuted {
            tool_name: v["toolName"].as_str().unwrap_or("").to_string(),
            args: v["args"].clone(),
            is_error: v["isError"].as_bool().unwrap_or(false),
        },
        _ => PiEvent::Other(v.clone()),
    }
}

impl RpcSession {
    /// 薄封装：无额外启动参数（多应用会话见 `spawn_with`）。
    pub async fn spawn(
        session_dir: &Path,
        env: Vec<(String, String)>,
    ) -> Result<(Self, mpsc::Receiver<PiEvent>), String> {
        Self::spawn_with(session_dir, env, vec![]).await
    }

    /// 携带额外启动参数（`--append-system-prompt`/`-e` 宿主工具等）的完整版：
    /// 供多应用会话（每个打开的应用一个 pi 子进程）使用。
    ///
    /// 薄封装：把固定的 `resolve_pi_bin()` + `--mode rpc` 前缀拼进 argv，
    /// 转交给通用的 `spawn_wrapped`（P2 起，`open_app` 需要在 pi 外面再套一层
    /// `sandbox-exec`，`spawn_wrapped` 就是那层通用外壳，见其文档）。
    pub async fn spawn_with(
        session_dir: &Path,
        env: Vec<(String, String)>,
        extra_args: Vec<String>,
    ) -> Result<(Self, mpsc::Receiver<PiEvent>), String> {
        let bin = resolve_pi_bin().to_string_lossy().to_string();
        let mut argv = vec!["--mode".to_string(), "rpc".to_string()];
        argv.extend(extra_args);
        Self::spawn_wrapped(&bin, argv, session_dir, env, None).await
    }

    /// 通用外层 wrapper：直接 `Command::new(bin).args(&argv)` 起子进程——`bin`/`argv`
    /// 由调用方决定，可以是裸 `pi --mode rpc ...`（`spawn_with` 的用法），也可以是
    /// `/usr/bin/sandbox-exec -p <profile> -D... -- <pi> --mode rpc ...`（`open_app`
    /// 在 macOS 上把真实 pi 子进程关进 L2 沙盒的用法，见 `session_mgr::open_app`）。
    ///
    /// `cwd`：子进程的当前工作目录；`None` 时保留原行为（继承宿主进程 cwd，
    /// `spawn_with`/裸 pi 场景不需要管这个）。macOS 沙盒场景（`session_mgr::spawn_app_session`）
    /// 会传 `Some($APP_DATA)`——sandbox-exec 子进程若不显式指定 cwd 就会继承宿主 cwd，
    /// 而宿主 cwd 多半不在沙盒读白名单内，node 启动时 `uv_cwd()` 会 EPERM（手工探测
    /// 记录见 `.superpowers/sdd/task-5-report.md` §6）。
    ///
    /// env/stdio/stderr 排空/stdout 读循环/`kill_on_drop` 与原 `spawn_with` 完全一致——
    /// 这层 wrapper 只是把「起什么」（bin/argv）从「怎么接管这个子进程」（下面这些管道
    /// 处理逻辑）里剥离出来，不改变任何既有行为。
    pub async fn spawn_wrapped(
        bin: &str,
        argv: Vec<String>,
        session_dir: &Path,
        env: Vec<(String, String)>,
        cwd: Option<&Path>,
    ) -> Result<(Self, mpsc::Receiver<PiEvent>), String> {
        let mut cmd = Command::new(bin);
        cmd.args(&argv);
        if let Some(cwd) = cwd {
            cmd.current_dir(cwd);
        }
        cmd.env("PI_CODING_AGENT_SESSION_DIR", session_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // tokio 的 Command 默认不会在 drop 时杀掉子进程；一旦 RpcSession 被丢弃
            // （应用退出、错误路径、已知的双开竞态等）而没走到显式 kill()，就会留下
            // 孤儿 pi 进程。这里让 drop 兜底杀掉子进程。
            .kill_on_drop(true);
        for (k, v) in env {
            cmd.env(k, v);
        }
        let mut child = cmd.spawn().map_err(|e| format!("spawn pi 失败：{e}"))?;
        let child_id = child.id();
        let stdin = child.stdin.take().ok_or("无 stdin")?;
        let stdout = child.stdout.take().ok_or("无 stdout")?;
        let stderr = child.stderr.take();

        // stderr 按行读取排空：命中启动期扩展冲突/加载失败（`is_startup_conflict`）
        // 时落 `eprintln!` 诊断，其余行照常丢弃——仍然全程排空、不阻塞 pi 子进程
        // （P0 修过的 stderr-drain 挂起：管道缓冲区满会阻塞子进程写 stderr，
        // 这里用 `read_line` 持续读走，不会让缓冲区堆积到 >64KB）。
        if let Some(stderr) = stderr {
            tokio::spawn(async move {
                let mut reader = BufReader::new(stderr);
                let mut line = String::new();
                loop {
                    line.clear();
                    match reader.read_line(&mut line).await {
                        Ok(0) | Err(_) => break,
                        Ok(_) => {
                            if is_startup_conflict(&line) {
                                eprintln!("pi 启动诊断：{}", line.trim());
                            }
                        }
                    }
                }
            });
        }

        let (tx, rx) = mpsc::channel::<PiEvent>(256);
        tokio::spawn(async move {
            let mut reader = BufReader::new(stdout);
            let mut fb = FrameBuffer::default();
            let mut chunk = [0u8; 4096];
            loop {
                match reader.read(&mut chunk).await {
                    Ok(0) => break,
                    Ok(n) => {
                        for line in fb.push(&chunk[..n]) {
                            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) {
                                if tx.send(classify(&v)).await.is_err() {
                                    return;
                                }
                            }
                        }
                    }
                    Err(_) => break,
                }
            }
        });

        Ok((
            Self {
                stdin: Mutex::new(stdin),
                child: Mutex::new(child),
                child_id,
            },
            rx,
        ))
    }

    async fn send_line(&self, json: String) -> Result<(), String> {
        let mut stdin = self.stdin.lock().await;
        stdin
            .write_all(format!("{json}\n").as_bytes())
            .await
            .map_err(|e| format!("写 stdin 失败：{e}"))?;
        stdin.flush().await.map_err(|e| format!("flush 失败：{e}"))
    }

    pub async fn send_prompt(&self, text: &str) -> Result<(), String> {
        let cmd = serde_json::json!({ "type": "prompt", "message": text });
        self.send_line(cmd.to_string()).await
    }

    /// 发送一条 `type:"steer"` RPC 命令（`docs/rpc.md` "Steer #steer"，本机
    /// pi 0.74.2 与另一版 0.84.4 均有此命令）：不像 `send_prompt` 那样另起一轮
    /// 新对话，而是把 `text` 塞进**当前这轮**助手的处理队列——pi 在当前工具
    /// 调用执行完、下一次 LLM 调用前会先吞下这条 steer 消息；若 agent 当前处于
    /// 空闲态（没有正在进行的轮次），效果等价于普通 `prompt`。P6-C Task4 用它
    /// 把批准/拒绝一次暂存写调用的结果回送进仍然活着的发起会话（见
    /// `notifications::NotificationStore::respond_staged` 文档），不打断该会话
    /// 正在做的其它事情。
    pub async fn send_steer(&self, text: &str) -> Result<(), String> {
        let cmd = serde_json::json!({ "type": "steer", "message": text });
        self.send_line(cmd.to_string()).await
    }

    pub async fn abort(&self) -> Result<(), String> {
        self.send_line(r#"{"type":"abort"}"#.to_string()).await
    }

    /// 发送一条 `type:"bash"` RPC 命令（`docs/rpc.md` "Bash #bash"）：不经过模型/
    /// 工具调用，直接触发 pi 内建 bash 执行器（`session.executeBash`，见
    /// vendored `rpc-mode.js` 的 `case "bash"` 分支：`await session.executeBash(...)`
    /// 后原样 `success(id, "bash", result)`）跑一条 shell 命令——全程无 `prompt`/
    /// `steer`/`follow_up`，不需要任何模型 API key（P0 无 key 沙盒逃逸里程碑的
    /// crux，见 `tests/real_pi_bash_escape_it.rs` 顶部注释）。
    ///
    /// `id` 用于在多条依次下发的 bash 命令之间关联响应：响应对象原样回显同一个
    /// `id`（`{"id":..,"type":"response","command":"bash","success":true,"data":{...}}`），
    /// 调用方据此从 `PiEvent::Other` 事件流里挑出对应这条命令的响应，而不是被
    /// 中间可能夹杂的其它事件（如另一条命令的响应）误配对。
    pub async fn send_bash(&self, id: &str, command: &str) -> Result<(), String> {
        let cmd = serde_json::json!({ "id": id, "type": "bash", "command": command });
        self.send_line(cmd.to_string()).await
    }

    /// P3 Task18 修复：发送 `{"type":"get_session_stats"}`，查询该会话累计 token
    /// 用量 + 真实花费（`docs`：pi 无官方 rpc 文档页记录此命令，形状按
    /// `AgentSession.getSessionStats()`/`SessionStats` 类型 + 真实 pi 实测确认，
    /// 见 `PiEvent::SessionStats` 文档）。不带 `id`——真实 pi 响应在未收到请求带
    /// `id` 时同样不带 `id`（`success()` 包装函数对 `undefined` 的 `id` 直接省略
    /// 该字段），`classify`/`parse_session_stats` 只按 `command` 字段匹配，不需要
    /// 关联 `id`；调用方按"发一条、等一条响应"的节奏使用（每轮 `agent_end` 后
    /// 发一次），不存在需要靠 `id` 区分的并发多条在途请求。
    pub async fn send_get_session_stats(&self) -> Result<(), String> {
        self.send_line(r#"{"type":"get_session_stats"}"#.to_string())
            .await
    }

    pub fn child_id(&self) -> Option<u32> {
        self.child_id
    }

    pub async fn kill(&mut self) {
        let mut child = self.child.lock().await;
        let _ = child.start_kill();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn classify_ui_emit_from_tool_execution_end() {
        let v = serde_json::json!({
            "type": "tool_execution_end", "toolName": "__host_ui_emit__",
            "args": { "event": "items_update", "payload": { "items": ["买牛奶"] } }
        });
        match classify(&v) {
            PiEvent::UiEmit { event, payload } => {
                assert_eq!(event, "items_update");
                assert_eq!(payload["items"][0], "买牛奶");
            }
            _ => panic!("应分类为 UiEmit"),
        }
    }
    #[test]
    fn classify_other_tool_end_is_tool_executed() {
        // P2：非 __host_ui_emit__ 的 tool_execution_end 不再归为 Other 丢弃——
        // 归为 ToolExecuted 供宿主接线 audit::record（见 PiEvent::ToolExecuted 文档）。
        let v = serde_json::json!({
            "type": "tool_execution_end", "toolName": "write",
            "args": { "path": "/data/apps/x/notes.json" }
        });
        match classify(&v) {
            PiEvent::ToolExecuted {
                tool_name,
                args,
                is_error,
            } => {
                assert_eq!(tool_name, "write");
                assert_eq!(args["path"], "/data/apps/x/notes.json");
                assert!(!is_error);
            }
            other => panic!("应分类为 ToolExecuted，实际 {other:?}"),
        }
    }

    #[test]
    fn classify_tool_execution_end_carries_is_error() {
        let v = serde_json::json!({
            "type": "tool_execution_end", "toolName": "bash",
            "args": { "command": "false" }, "isError": true
        });
        match classify(&v) {
            PiEvent::ToolExecuted { is_error, .. } => assert!(is_error),
            other => panic!("应分类为 ToolExecuted，实际 {other:?}"),
        }
    }

    #[test]
    fn classify_agent_end_is_agent_ended() {
        // P3 Task18 修复回归钉子：真实 pi 的 agent_end 事件就是 `{type:"agent_end",messages}`，
        // 不带任何 usage 字段（Task16 的假设已证伪，见 PiEvent::SessionStats 文档）。
        // agent_end 必须始终分类为 AgentEnded，不会被 get_session_stats 响应解析分支误判。
        let v = serde_json::json!({ "type": "agent_end", "messages": [] });
        match classify(&v) {
            PiEvent::AgentEnded => {}
            other => panic!("应分类为 AgentEnded，实际 {other:?}"),
        }
    }

    // ---- P3 Task18 修复：PiEvent::SessionStats（get_session_stats 响应） ----

    #[test]
    fn classify_get_session_stats_response_is_session_stats_event() {
        // 形状取自真实 pi v0.74.2 的实测响应（keyless、--no-session、未发送任何
        // prompt/API key，纯 session-local 查询，见 PiEvent::SessionStats 文档）：
        // {"type":"response","command":"get_session_stats","success":true,
        //  "data":{"sessionId":"019f74e9-d0e8-7cf2-838b-dd4146728a9d","userMessages":0,
        //  "assistantMessages":0,"toolCalls":0,"toolResults":0,"totalMessages":0,
        //  "tokens":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"total":0},"cost":0}}
        let v = serde_json::json!({
            "type": "response", "command": "get_session_stats", "success": true,
            "data": {
                "sessionId": "019f74e9-d0e8-7cf2-838b-dd4146728a9d",
                "userMessages": 2, "assistantMessages": 2, "toolCalls": 1, "toolResults": 1, "totalMessages": 6,
                "tokens": { "input": 100, "output": 50, "cacheRead": 0, "cacheWrite": 0, "total": 150 },
                "cost": 0.01
            }
        });
        match classify(&v) {
            PiEvent::SessionStats {
                input,
                output,
                cost,
            } => {
                assert_eq!(input, 100);
                assert_eq!(output, 50);
                assert!((cost - 0.01).abs() < 1e-9);
            }
            other => panic!("应分类为 SessionStats，实际 {other:?}"),
        }
    }

    #[test]
    fn classify_get_session_stats_response_missing_cost_defaults_zero() {
        // cost 字段防御性缺失兜底：不能因为这个次要字段缺失就丢掉已解出的 token 数。
        let v = serde_json::json!({
            "type": "response", "command": "get_session_stats", "success": true,
            "data": { "tokens": { "input": 7, "output": 3 } }
        });
        match classify(&v) {
            PiEvent::SessionStats {
                input,
                output,
                cost,
            } => {
                assert_eq!(input, 7);
                assert_eq!(output, 3);
                assert_eq!(cost, 0.0);
            }
            other => panic!("应分类为 SessionStats，实际 {other:?}"),
        }
    }

    #[test]
    fn classify_other_response_commands_are_not_session_stats() {
        // 回归钉子：其它命令的响应（如 bash）不应被误判为 SessionStats。
        let v = serde_json::json!({
            "type": "response", "command": "bash", "success": true, "data": { "output": "ok" }
        });
        match classify(&v) {
            PiEvent::Other(_) => {}
            other => panic!("应分类为 Other，实际 {other:?}"),
        }
    }

    #[test]
    fn classify_failed_get_session_stats_response_is_other() {
        // success:false 的响应不应被解析成 SessionStats（没有可信的 data）。
        let v = serde_json::json!({
            "type": "response", "command": "get_session_stats", "success": false, "error": "boom"
        });
        match classify(&v) {
            PiEvent::Other(_) => {}
            other => panic!("应分类为 Other，实际 {other:?}"),
        }
    }

    #[test]
    fn detects_startup_conflict_lines() {
        assert!(is_startup_conflict(
            r#"Failed to load extension "x": Tool "bash" conflicts with y"#
        ));
        assert!(!is_startup_conflict("normal stderr noise"));
    }

    /// `cargo test --lib`（本模块所在）不像集成测试那样能拿到 `CARGO_BIN_EXE_<name>`——
    /// 该变量按 cargo 文档只在编译**集成测试/benchmark**时设置，`--lib` 单测拿不到
    /// （已实测确认：编译期 `env!` 直接报错未定义，运行期 `std::env::var` 也是
    /// `NotPresent`）。因此改从当前测试可执行文件的路径反推 mock_pi 的位置：单测二进制
    /// 位于 `target/<profile>/deps/`，其同级 `target/<profile>/` 目录下有本 crate 的其它
    /// bin 产物（含 mock_pi）。
    ///
    /// `cargo test --lib` 单独执行时（不同于 `cargo build`/完整 `cargo test`）不会
    /// 顺带构建同 crate 下的其它 bin 目标——实测在全新 `cargo clean` 后单独跑
    /// `cargo test -p super-agent-os --lib rpc::` 会因 mock_pi 不存在而失败。为了让
    /// 这条被固定要求的测试命令能独立、确定性地跑通（不依赖调用方先手动
    /// `cargo build` 过一次），这里按需触发一次 `cargo build --bin mock_pi`。
    fn mock_pi_path() -> std::path::PathBuf {
        let exe = std::env::current_exe().expect("current_exe");
        let deps_dir = exe.parent().expect("deps 目录");
        let profile_dir = deps_dir.parent().expect("profile 目录");
        let bin = profile_dir.join(format!("mock_pi{}", std::env::consts::EXE_SUFFIX));
        if !bin.exists() {
            let status = std::process::Command::new(env!("CARGO"))
                .args(["build", "--bin", "mock_pi"])
                .current_dir(env!("CARGO_MANIFEST_DIR"))
                .status()
                .expect("构建 mock_pi 失败");
            assert!(status.success(), "cargo build --bin mock_pi 失败");
        }
        bin
    }

    #[tokio::test]
    async fn abort_and_kill_do_not_panic() {
        std::env::set_var("SUPERAGENT_PI_BIN", mock_pi_path());
        let tmp = tempfile::tempdir().unwrap();
        let (mut session, _rx) = RpcSession::spawn_with(tmp.path(), vec![], vec![])
            .await
            .unwrap();
        session.abort().await.unwrap();
        assert!(session.child_id().is_some());
        session.kill().await; // 不应 panic
    }

    /// P3 Task18 修复：`send_get_session_stats` 发出的 `{"type":"get_session_stats"}`
    /// 能被 mock_pi（已扩展支持该命令，见 `bin/mock_pi.rs`）应答，响应经
    /// `classify`/`parse_session_stats` 正确解析为 `PiEvent::SessionStats`——覆盖
    /// "发命令 → 收到能被解析的响应事件" 这条完整链路，不只是纯函数级的 `classify`
    /// 单测。
    #[tokio::test]
    async fn send_get_session_stats_and_receive_response_via_mock_pi() {
        std::env::set_var("SUPERAGENT_PI_BIN", mock_pi_path());
        let tmp = tempfile::tempdir().unwrap();
        let (session, mut rx) = RpcSession::spawn_with(tmp.path(), vec![], vec![])
            .await
            .unwrap();
        session.send_get_session_stats().await.unwrap();
        let ev = tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv())
            .await
            .expect("应在超时前收到响应")
            .expect("通道不应提前关闭");
        match ev {
            PiEvent::SessionStats {
                input,
                output,
                cost,
            } => {
                assert_eq!(input, 100);
                assert_eq!(output, 50);
                assert!((cost - 0.01).abs() < 1e-9);
            }
            other => panic!("应收到 SessionStats 事件，实际 {other:?}"),
        }
    }
}
