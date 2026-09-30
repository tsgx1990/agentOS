//! 真实 pi（本机 PATH 或 SUPERAGENT_PI_BIN）：注册表合成的 --tools 白名单必须让桥工具真的处于激活集。
//! 需要本机 pi；`cargo test --test real_pi_tools_allowlist_it -- --ignored`。
use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};

fn hosttools() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("hosttools")
}
fn probe() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/rpc-probe/active_tools_probe.ts")
}

/// 从 pi `--mode rpc` 的一行 stdout 里取出 `ACTIVE_TOOLS_PROBE:` 探针 payload（若这行是）。
///
/// 踩坑记录：计划草稿里原先设想的手工切片方案——在整行原文里 `find("ACTIVE_TOOLS_PROBE:")`
/// 定位起点，再用 `rfind('}')` 找终点、`replace("\\\"", "\"")` 反转义——用真实 pi 输出验证
/// 后发现是错的：真实的 `extension_ui_request` 事件形状是
/// `{"type":"extension_ui_request","id":"...","method":"notify",
///   "message":"ACTIVE_TOOLS_PROBE:{...探针 JSON...}","notifyType":"info"}`——
/// `message` 字段之后还跟着 `notifyType` 字段，所以整行里最后一个 `}` 是**外层**事件
/// 对象的收尾，不是探针 payload 自己的收尾；`rfind('}')` 会把 `,"notifyType":"info"}`
/// 这段尾巴也吞进去，交给 `serde_json::from_str` 解析必然报 "trailing characters"
/// （已用本机真实 pi 0.74.2 复现）。
///
/// 正确做法（与 `real_pi_bash_escape_it.rs` 的 bonus 用例
/// `TOOL_REGISTRATION_PROBE_RESULT` 同款手法）：先把整行当整体 JSON 解析一次——
/// `message` 是 JSON 字符串字段，serde_json 解析外层对象时已经把里面的 `\"` 转义
/// 还原成了普通 Rust `String`，剩下只需要在这个已反转义的字符串上找前缀、剥掉它，
/// 再把余下部分整体喂给 `serde_json::from_str`（它就是这一个 JSON 值，没有多余尾巴）。
fn parse_probe_line(line: &str) -> Option<serde_json::Value> {
    let v: serde_json::Value = serde_json::from_str(line).ok()?;
    if v["type"] != "extension_ui_request" || v["method"] != "notify" {
        return None;
    }
    let msg = v["message"].as_str()?;
    let rest = msg.strip_prefix("ACTIVE_TOOLS_PROBE:")?;
    serde_json::from_str(rest).ok()
}

#[test]
#[ignore = "手工里程碑自动化：依赖真实 pi 装在本机(或 PATH)，用 `cargo test -- --ignored` 显式跑"]
fn registry_tools_allowlist_activates_bridge_tools_in_real_pi() {
    let pi = super_agent_os::pi_bin::resolve_pi_bin();
    let tmp = tempfile::tempdir().unwrap();
    let tools = "read,__host_ui_emit__,__host_notify__"; // 模拟注册表合成结果（ui_emit + notifications）
    let mut child = Command::new(&pi)
        .args([
            "--mode",
            "rpc",
            "--no-session",
            "--offline",
            "--tools",
            tools,
            "-e",
            hosttools().join("ui_emit.ts").to_str().unwrap(),
            "-e",
            hosttools().join("notify_bridge.ts").to_str().unwrap(),
            "-e",
            probe().to_str().unwrap(),
        ])
        .env("PI_CODING_AGENT_DIR", tmp.path())
        .env("SUPERAGENT_MCP_SOCKET", tmp.path().join("never.sock"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(b"{\"type\":\"get_state\"}\n")
        .unwrap();
    let reader = BufReader::new(child.stdout.take().unwrap());
    let mut found = None;
    for line in reader.lines().take(50) {
        let line = line.unwrap();
        if let Some(payload) = parse_probe_line(&line) {
            found = Some(payload);
            break;
        }
    }
    let _ = child.kill();
    let _ = child.wait();
    let payload = found.expect("探针未上报");
    let active: Vec<String> = payload["active"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    assert!(
        active.contains(&"__host_notify__".to_string()),
        "{active:?}"
    );
    assert!(
        active.contains(&"__host_ui_emit__".to_string()),
        "{active:?}"
    );
}

#[test]
#[ignore = "手工里程碑自动化：依赖真实 pi 装在本机(或 PATH)，用 `cargo test -- --ignored` 显式跑"]
fn tool_missing_from_allowlist_is_not_even_registered_in_real_pi() {
    // 对照组：白名单不含 __host_notify__ → getAllTools 也不含它（坐实 spec §1.3 缺陷形状）。
    let pi = super_agent_os::pi_bin::resolve_pi_bin();
    let tmp = tempfile::tempdir().unwrap();
    let mut child = Command::new(&pi)
        .args([
            "--mode",
            "rpc",
            "--no-session",
            "--offline",
            "--tools",
            "read",
            "-e",
            hosttools().join("notify_bridge.ts").to_str().unwrap(),
            "-e",
            probe().to_str().unwrap(),
        ])
        .env("PI_CODING_AGENT_DIR", tmp.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(b"{\"type\":\"get_state\"}\n")
        .unwrap();
    let reader = BufReader::new(child.stdout.take().unwrap());
    let mut all: Option<Vec<String>> = None;
    for line in reader.lines().take(50) {
        let line = line.unwrap();
        if let Some(payload) = parse_probe_line(&line) {
            all = Some(
                payload["all"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|x| x.as_str().unwrap().to_string())
                    .collect(),
            );
            break;
        }
    }
    let _ = child.kill();
    let _ = child.wait();
    assert!(!all.unwrap().contains(&"__host_notify__".to_string()));
}
