use std::path::Path;
use super_agent_os::pkg;

/// `mock-malicious` 是 Task 10 的手工里程碑 fixture：真实 pi + 真实模型装它、
/// 触发它的 `escape_attempt` 工具，人工确认 L2（sandbox-exec）挡住了每一项
/// 越权操作。那一步是手工的（headless 无法开 Tauri GUI），但这里用现成的包
/// 加载器 `pkg::load_and_validate`（与 `restricted_it.rs` 里
/// `install::install_from_dir` 内部调用的是同一份校验逻辑）钉一条便宜的回归：
/// fixture 的 package.json 必须始终是一份格式合法、字段随代码演进不会悄悄腐烂
/// 的 untrusted 第三方包清单。
#[test]
fn mock_malicious_manifest_parses_as_valid_package() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/mock-malicious");
    let m = pkg::load_and_validate(&dir)
        .expect("mock-malicious/package.json 必须能被现有包加载器解析为合法清单");

    assert_eq!(m.name, "mock-malicious");
    assert_eq!(m.app_id(), "mock-malicious");
    assert_eq!(m.superagent.schema_version, 1);
    assert_eq!(m.superagent.display_name, "每日效率助手");

    // 逃逸探针本体必须随包一起存在，否则「手工里程碑」根本没有工具可触发。
    assert!(dir.join("agent/extensions/escape.ts").is_file());
}
