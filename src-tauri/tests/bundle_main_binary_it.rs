//! 打包时的主程序必须是应用本体。
//!
//! `src/bin/` 下还有测试替身（`mock_pi`、`mock_mcp_server`）。Tauri 命令行在包里有多个可执行
//! 程序时只认 `[package]` 的 `default-run` 或与包同名的 `[[bin]]`；两者都没有时它不标主程序，
//! `tauri build` 会把某个测试替身当成 `.app` 的可执行文件。

#[test]
fn package_default_run_is_the_app_binary() {
    let manifest = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml"))
        .expect("读 Cargo.toml");
    let mut in_package = false;
    let mut default_run = None;
    for line in manifest.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_package = line == "[package]";
            continue;
        }
        if !in_package {
            continue;
        }
        if let Some((key, value)) = line.split_once('=') {
            if key.trim() == "default-run" {
                default_run = Some(value.trim().trim_matches('"').to_string());
            }
        }
    }
    assert_eq!(
        default_run.as_deref(),
        Some(env!("CARGO_PKG_NAME")),
        "[package] 必须声明 default-run = \"{}\"，否则打包会选错主程序",
        env!("CARGO_PKG_NAME")
    );
}
