// 冒烟测试：spawn 编译出的 mock_mcp_server（Rust bin，同 mock_pi 风格），
// 走一次 stdio JSON-RPC（LF 分隔一行一个 JSON，与 pi RPC 同帧格式），
// 断言 tools/list 里能看到 read_file（只读）+ write_file（写）两个工具，
// 并覆盖 initialize / tools/call / 未知 method 的基本行为。
//
// Task5 的 McpManager 会用同样的 spawn+JSON-RPC 握手方式接入这个 mock server；
// 真实 filesystem server 的 Content-Length 分帧在 Task5 里单独适配，不在本任务范围。
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

// cargo 把集成测试依赖的 bin 放在 deps 同级；用 CARGO_BIN_EXE_ 提供的路径（同 rpc_it.rs 里
// mock_pi 的用法）。
fn mock_mcp_server_path() -> String {
    env!("CARGO_BIN_EXE_mock_mcp_server").to_string()
}

fn spawn_server() -> (Child, ChildStdin, BufReader<ChildStdout>) {
    let mut child = Command::new(mock_mcp_server_path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("failed to spawn mock_mcp_server");
    let stdin = child.stdin.take().expect("stdin should be piped");
    let stdout = child.stdout.take().expect("stdout should be piped");
    (child, stdin, BufReader::new(stdout))
}

fn send_line(stdin: &mut ChildStdin, line: &str) {
    writeln!(stdin, "{}", line).expect("write request line");
    stdin.flush().expect("flush stdin");
}

fn read_response(reader: &mut BufReader<ChildStdout>) -> serde_json::Value {
    let mut line = String::new();
    reader.read_line(&mut line).expect("read response line");
    assert!(
        !line.trim().is_empty(),
        "expected a non-empty response line"
    );
    serde_json::from_str(&line).expect("response line should be valid JSON")
}

#[test]
fn tools_list_returns_read_file_and_write_file_with_danger_annotations() {
    let (mut child, mut stdin, mut reader) = spawn_server();

    send_line(
        &mut stdin,
        r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#,
    );
    let resp = read_response(&mut reader);

    assert_eq!(resp["id"], 1);
    let tools = resp["result"]["tools"]
        .as_array()
        .expect("result.tools should be an array");
    let names: Vec<&str> = tools.iter().filter_map(|t| t["name"].as_str()).collect();
    assert!(
        names.contains(&"read_file"),
        "expected read_file in {:?}",
        names
    );
    assert!(
        names.contains(&"write_file"),
        "expected write_file in {:?}",
        names
    );

    let read_tool = tools
        .iter()
        .find(|t| t["name"] == "read_file")
        .expect("read_file tool present");
    assert_eq!(read_tool["annotations"]["readOnlyHint"], true);

    let write_tool = tools
        .iter()
        .find(|t| t["name"] == "write_file")
        .expect("write_file tool present");
    assert_eq!(write_tool["annotations"]["readOnlyHint"], false);

    child.kill().ok();
}

#[test]
fn initialize_returns_minimal_server_info() {
    let (mut child, mut stdin, mut reader) = spawn_server();

    send_line(
        &mut stdin,
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize"}"#,
    );
    let resp = read_response(&mut reader);

    assert_eq!(resp["id"], 1);
    assert!(resp["result"]["serverInfo"]["name"].is_string());

    child.kill().ok();
}

#[test]
fn tools_call_read_file_returns_mock_content() {
    let (mut child, mut stdin, mut reader) = spawn_server();

    send_line(
        &mut stdin,
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"read_file","arguments":{"path":"/tmp/x.txt"}}}"#,
    );
    let resp = read_response(&mut reader);

    let text = resp["result"]["content"][0]["text"]
        .as_str()
        .expect("content[0].text should be a string");
    assert!(
        text.contains("/tmp/x.txt"),
        "expected mock content to reference requested path, got {:?}",
        text
    );

    child.kill().ok();
}

#[test]
fn tools_call_write_file_echoes_written_content() {
    let (mut child, mut stdin, mut reader) = spawn_server();

    send_line(
        &mut stdin,
        r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"write_file","arguments":{"path":"/tmp/x.txt","content":"hello"}}}"#,
    );
    let resp = read_response(&mut reader);

    assert_eq!(resp["result"]["isError"], false);
    assert_eq!(resp["result"]["written"]["path"], "/tmp/x.txt");
    assert_eq!(resp["result"]["written"]["content"], "hello");

    child.kill().ok();
}

#[test]
fn unknown_method_returns_json_rpc_error() {
    let (mut child, mut stdin, mut reader) = spawn_server();

    send_line(
        &mut stdin,
        r#"{"jsonrpc":"2.0","id":4,"method":"bogus/method"}"#,
    );
    let resp = read_response(&mut reader);

    assert_eq!(resp["id"], 4);
    assert!(resp.get("result").is_none());
    assert_eq!(resp["error"]["code"], -32601);

    child.kill().ok();
}

#[test]
fn unknown_tool_returns_json_rpc_error() {
    let (mut child, mut stdin, mut reader) = spawn_server();

    send_line(
        &mut stdin,
        r#"{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"delete_everything","arguments":{}}}"#,
    );
    let resp = read_response(&mut reader);

    assert_eq!(resp["id"], 5);
    assert!(resp.get("error").is_some());

    child.kill().ok();
}
