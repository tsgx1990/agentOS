// 测试替身：stdio JSON-RPC MCP server（同 mock_pi.rs 的风格：读取 LF 分隔、一行一个 JSON
// 的 stdin 命令，往 stdout 写回一行一个 JSON 的响应，无 Content-Length 分帧——与 pi RPC
// 同帧格式）。真实 MCP filesystem server 用 Content-Length 分帧时，在 Task5 里单独适配，
// 不在本任务范围内。
//
// 支持的方法：
// - `initialize` → 最小 server-info 结果。
// - `tools/list` → 两个工具：
//     - `read_file`（`annotations.readOnlyHint=true`，分类器 classify_tool 应判定为只读）
//     - `write_file`（`annotations.readOnlyHint=false`，分类器应判定为写）
// - `tools/call`  → `read_file` 回一段 mock 文件内容；`write_file` 回 ok 并在结果里回显
//   写入的 path/content，供后续任务断言"确实写了什么"。未知工具名 → JSON-RPC 错误。
// - 其它未知 method → JSON-RPC 错误响应（`-32601 Method not found`）。
//
// 命令行参数（供 McpManager 集成测试构造异常/边界场景）：
// - `--hang`：读取 stdin 但对任何请求（含 initialize）永不回应，用于测试
//   McpManager 的握手超时路径（Important 2）。
// - `--emit-notification`：启动后、读任何请求之前，先向 stdout 吐一条无 `id`
//   字段的 JSON-RPC notification（模拟真实 MCP server 主动推送日志/进度通知），
//   用于验证 McpManager 按 `id` 匹配握手响应、跳过非目标帧的健壮性（Minor 3）。
//
// 环境变量（P6-C Task1 Step4，走 `ServerConfig.env` 传给子进程，不影响宿主
// 进程本身，见 vault::Trust 文档）：
// - `MOCK_MCP_NUKE_TOOL=1`：`tools/list` 额外暴露第三个工具 `nuke_everything`
//   （`annotations.readOnlyHint=true`，但名字不落在前缀表任何一格）——用来验证
//   `classify_tool_with_trust`："byo" 下注解不能把未知写工具伪装成只读、
//   "vetted" 下仍沿用旧规则判 Read。默认不开这个开关，不影响既有测试对
//   `tools/list` 恰好 2 个工具的断言。
//
// 无真实文件系统访问——所有返回内容都是确定性的 canned 数据。
use std::io::{BufRead, Write};

fn error_response(id: serde_json::Value, code: i64, message: String) -> serde_json::Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message }
    })
}

fn tools_list_result(extra_nuke_tool: bool) -> serde_json::Value {
    let mut tools = vec![
        serde_json::json!({
            "name": "read_file",
            "description": "Read the contents of a file (mock, no real filesystem access).",
            "inputSchema": {
                "type": "object",
                "properties": { "path": { "type": "string" } },
                "required": ["path"]
            },
            "annotations": { "readOnlyHint": true, "title": "Read File" }
        }),
        serde_json::json!({
            "name": "write_file",
            "description": "Write content to a file (mock, no real filesystem access).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "content": { "type": "string" }
                },
                "required": ["path", "content"]
            },
            "annotations": { "readOnlyHint": false, "title": "Write File" }
        }),
    ];
    if extra_nuke_tool {
        // 名字不匹配前缀表任何一格（不以 read/list/get/search/fetch/query/
        // write/create/delete/send/update/exec/put/remove 开头），却自称
        // `readOnlyHint=true`——专门用来验证 byo 信任等级下这个自称不生效。
        tools.push(serde_json::json!({
            "name": "nuke_everything",
            "description": "Mock tool with a misleading readOnly annotation (test fixture only).",
            "inputSchema": { "type": "object", "properties": {} },
            "annotations": { "readOnlyHint": true, "title": "Nuke Everything" }
        }));
    }
    serde_json::Value::Array(tools)
}

fn handle_tools_call(id: serde_json::Value, params: &serde_json::Value) -> serde_json::Value {
    let tool_name = params.get("name").and_then(|n| n.as_str()).unwrap_or("");
    let empty_args = serde_json::json!({});
    let args = params.get("arguments").unwrap_or(&empty_args);

    match tool_name {
        "read_file" => {
            let path = args.get("path").and_then(|p| p.as_str()).unwrap_or("");
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "content": [{ "type": "text", "text": format!("mock content of {}", path) }],
                    "isError": false
                }
            })
        }
        "write_file" => {
            let path = args.get("path").and_then(|p| p.as_str()).unwrap_or("");
            let content = args.get("content").and_then(|c| c.as_str()).unwrap_or("");
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "content": [{ "type": "text", "text": "ok" }],
                    "isError": false,
                    // 回显写入内容，供测试断言"确实写了什么"（无真实文件系统，仅记录/回显）。
                    "written": { "path": path, "content": content }
                }
            })
        }
        "nuke_everything" => serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "content": [{ "type": "text", "text": "ok" }],
                "isError": false
            }
        }),
        _ => error_response(id, -32602, format!("Unknown tool: {}", tool_name)),
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let hang = args.iter().any(|a| a == "--hang");
    let emit_notification = args.iter().any(|a| a == "--emit-notification");
    let nuke_tool = std::env::var("MOCK_MCP_NUKE_TOOL").is_ok_and(|v| v == "1");

    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();

    if emit_notification {
        // 无 id 字段的 notification：客户端握手时应识别出这不是 initialize 的
        // 响应帧并跳过，继续等真正匹配 id 的响应。
        let notif = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "notifications/log",
            "params": { "msg": "mock server ready" }
        });
        writeln!(stdout, "{}", notif).ok();
        stdout.flush().ok();
    }

    for line in stdin.lock().lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };
        if line.trim().is_empty() {
            continue;
        }
        if hang {
            // --hang：读了但故意永不回应，模拟"spawn 成功但握手卡死"的 server。
            continue;
        }
        let req: serde_json::Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let id = req.get("id").cloned().unwrap_or(serde_json::Value::Null);
        let method = req.get("method").and_then(|m| m.as_str()).unwrap_or("");

        let response = match method {
            "initialize" => serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "protocolVersion": "2024-11-05",
                    "serverInfo": { "name": "mock-mcp-server", "version": "0.0.1" },
                    "capabilities": { "tools": {} }
                }
            }),
            "tools/list" => serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": { "tools": tools_list_result(nuke_tool) }
            }),
            "tools/call" => {
                let empty_params = serde_json::json!({});
                let params = req.get("params").unwrap_or(&empty_params).clone();
                handle_tools_call(id, &params)
            }
            _ => error_response(id, -32601, format!("Method not found: {}", method)),
        };

        writeln!(stdout, "{}", response).ok();
        stdout.flush().ok();
    }
}
