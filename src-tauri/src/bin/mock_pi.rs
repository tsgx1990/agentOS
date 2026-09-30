// 测试替身：读取 stdin 的 JSONL 命令，对 prompt 输出 canned 事件流。
// 支持通过环境变量 MOCK_PI_MODE 切换行为：normal | auth_error | rate_limit | crash | ui_emit
use std::io::{BufRead, Write};

/// P3 Task18 修复：`get_session_stats` 响应的第 N 次调用返回的 (input, output, cost)。
/// 真实 pi 的 `SessionStats` 是**累计**值（同一会话里多轮对话后数字只增不减），
/// 这里第二次及以后调用返回比第一次更高的数字，模拟"第二轮对话后累计用量更高"，
/// 供 `get_session_stats_after_turn_reflects_latest_cumulative_not_summed`
/// （`tests/e2e_mock.rs`）验证消费端存的是"最新值"而不是把两次响应加总。
fn session_stats_for_call(call: u64) -> (u64, u64, f64) {
    if call == 0 {
        (100, 50, 0.01)
    } else {
        (250, 120, 0.025)
    }
}

/// 组装一条 `get_session_stats` 响应，字段形状照抄真实 pi v0.74.2 的实测响应
/// （keyless、`--no-session`、未发送任何 prompt/API key 时抓到的原始 JSON，见
/// `rpc::PiEvent::SessionStats` 文档）：
/// `{"type":"response","command":"get_session_stats","success":true,"data":{"sessionId":..,"tokens":{"input":..,"output":..,"cacheRead":..,"cacheWrite":..,"total":..},"cost":..,...}}`
fn session_stats_response(call: u64) -> String {
    let (input, output, cost) = session_stats_for_call(call);
    serde_json::json!({
        "type": "response",
        "command": "get_session_stats",
        "success": true,
        "data": {
            "sessionId": "mock-session",
            "userMessages": call + 1,
            "assistantMessages": call + 1,
            "toolCalls": 0,
            "toolResults": 0,
            "totalMessages": (call + 1) * 2,
            "tokens": { "input": input, "output": output, "cacheRead": 0, "cacheWrite": 0, "total": input + output },
            "cost": cost
        }
    })
    .to_string()
}

/// P6-C Task4：若设了 `MOCK_PI_STDIN_LOG=<file>`，把每一行收到的原始 stdin
/// 追加进该文件（`tests/p6c_approvals_it.rs::approved_result_is_steered_into_live_session`
/// 用它断言 `RpcSession::send_steer` 确实把一条 `{"type":"steer",...}` 命令
/// 写到了这个 mock 子进程的 stdin——不用真实 pi，靠这个文件断言"steer 确实被
/// 发送了"，而不是靠猜测/sleep 竞态）。best-effort（写失败不影响本身的应答
/// 逻辑，同其余测试替身对辅助落盘失败的一贯态度：这是诊断通道，不是主逻辑）。
/// 追加发生在应答之前——同一个单线程循环里先落盘、后写 stdout，测试侧读到
/// steer 的响应事件时，日志文件里对应那一行必然已经写完，不需要额外的
/// sleep/轮询来避免竞态。
fn log_stdin_line(line: &str) {
    if let Ok(path) = std::env::var("MOCK_PI_STDIN_LOG") {
        use std::io::Write as _;
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            let _ = writeln!(f, "{line}");
        }
    }
}

fn main() {
    let mode = std::env::var("MOCK_PI_MODE").unwrap_or_else(|_| "normal".into());
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    let mut session_stats_calls: u64 = 0;
    for line in stdin.lock().lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };
        if line.trim().is_empty() {
            continue;
        }
        log_stdin_line(&line);
        let cmd: serde_json::Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        // `get_session_stats` 与 `MOCK_PI_MODE` 无关，任何模式下都应答——真实 pi 里
        // 这是一条与"这轮对话怎么回复"完全正交的 session-local 查询命令。
        if cmd["type"] == "get_session_stats" {
            let resp = session_stats_response(session_stats_calls);
            session_stats_calls += 1;
            writeln!(stdout, "{resp}").ok();
            stdout.flush().ok();
            continue;
        }
        // P6-C Task4：`{"type":"steer",...}` 与 `MOCK_PI_MODE` 无关，任何模式
        // 下都应答——真实 pi 的 `docs/rpc.md` #steer 一节：成功入队后回
        // `{"type":"response","command":"steer","success":true}`，不产生
        // `agent_start`/`agent_end` 这类完整轮次事件（steer 只是把消息塞进
        // 当前轮的处理队列，不是另起一轮）。
        if cmd["type"] == "steer" {
            writeln!(
                stdout,
                r#"{{"type":"response","command":"steer","success":true}}"#
            )
            .ok();
            stdout.flush().ok();
            continue;
        }
        if cmd["type"] == "prompt" {
            match mode.as_str() {
                "crash" => {
                    std::process::exit(1);
                }
                "auth_error" => {
                    writeln!(stdout, r#"{{"type":"agent_start"}}"#).ok();
                    writeln!(stdout, r#"{{"type":"extension_error","error":"401 {{\"error\":{{\"type\":\"authentication_error\"}}}}"}}"#).ok();
                    writeln!(stdout, r#"{{"type":"agent_end","willRetry":false}}"#).ok();
                }
                "rate_limit" => {
                    writeln!(stdout, r#"{{"type":"agent_start"}}"#).ok();
                    writeln!(stdout, r#"{{"type":"auto_retry_start","attempt":1,"maxAttempts":3,"delayMs":2000,"errorMessage":"429 rate_limit"}}"#).ok();
                    writeln!(
                        stdout,
                        r#"{{"type":"auto_retry_end","success":true,"attempt":2}}"#
                    )
                    .ok();
                    writeln!(stdout, r#"{{"type":"agent_end","willRetry":false}}"#).ok();
                }
                // ui_emit：模拟应用通过宿主工具 __host_ui_emit__ 主动上报 UI 观察事件
                // （tool_execution_end + toolName:"__host_ui_emit__"），供 e2e 里程碑验证
                // rpc::classify 能把它归类为 PiEvent::UiEmit。
                "ui_emit" => {
                    writeln!(stdout, r#"{{"type":"agent_start"}}"#).ok();
                    writeln!(stdout, r#"{{"type":"message_update","message":{{}},"assistantMessageEvent":{{"type":"text_delta","contentIndex":0,"delta":"好的","partial":{{}}}}}}"#).ok();
                    writeln!(stdout, r#"{{"type":"tool_execution_end","toolName":"__host_ui_emit__","args":{{"event":"items_update","payload":{{"items":[{{"text":"买牛奶","done":false}}]}}}}}}"#).ok();
                    writeln!(stdout, r#"{{"type":"agent_end","willRetry":false}}"#).ok();
                }
                // slow：agent_start 后先真 sleep 一段（模拟一次耗时较长的 task-mode
                // 会话），再回文本 + agent_end——供 P3 Task12 调度器并发上限测试
                // （3 个同时到期任务、cap=2）制造可观测的重叠窗口：若无这段延迟，
                // 三次拉起+收尾在毫秒级完成，测试将永远看不到"确实有 2 个同时在跑"，
                // 沦为侥幸从未撞线的假阳性通过。sleep 发生在这个独立子进程里，不是
                // 调度器/测试自身的逻辑在碰墙钟（scheduler.rs 头部注释里禁止碰墙钟
                // 的是调度到期判定本身，即 Clock trait 那部分，不含这类测试替身的
                // 模拟耗时）。
                "slow" => {
                    writeln!(stdout, r#"{{"type":"agent_start"}}"#).ok();
                    stdout.flush().ok();
                    std::thread::sleep(std::time::Duration::from_millis(250));
                    writeln!(stdout, r#"{{"type":"message_update","message":{{}},"assistantMessageEvent":{{"type":"text_delta","contentIndex":0,"delta":"慢速完成","partial":{{}}}}}}"#).ok();
                    writeln!(stdout, r#"{{"type":"agent_end","willRetry":false}}"#).ok();
                }
                // tool_audit：模拟一次普通工具（非 __host_ui_emit__）真正执行完毕，
                // 供 P2 审计接线的 e2e 验证——rpc::classify 应把它归类为
                // PiEvent::ToolExecuted，而不是 P1 时代直接丢弃的 Other。
                "tool_audit" => {
                    writeln!(stdout, r#"{{"type":"agent_start"}}"#).ok();
                    writeln!(stdout, r#"{{"type":"tool_execution_end","toolName":"write","args":{{"path":"/data/apps/x/notes.json"}},"isError":false}}"#).ok();
                    writeln!(stdout, r#"{{"type":"agent_end","willRetry":false}}"#).ok();
                }
                _ => {
                    writeln!(stdout, r#"{{"type":"agent_start"}}"#).ok();
                    writeln!(stdout, r#"{{"type":"message_update","message":{{}},"assistantMessageEvent":{{"type":"text_delta","contentIndex":0,"delta":"你好","partial":{{}}}}}}"#).ok();
                    writeln!(stdout, r#"{{"type":"message_update","message":{{}},"assistantMessageEvent":{{"type":"text_delta","contentIndex":0,"delta":"，世界","partial":{{}}}}}}"#).ok();
                    writeln!(stdout, r#"{{"type":"agent_end","willRetry":false}}"#).ok();
                }
            }
            stdout.flush().ok();
        }
    }
}
