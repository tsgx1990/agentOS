// base profile 启动验证：真实 sandbox-exec 下跑 mock_pi，断言不被信号杀死（如 SIGABRT）。
// 仅 macOS 运行——依赖系统自带 /usr/bin/sandbox-exec。
#![cfg(target_os = "macos")]
use super_agent_os::pi_bin::{install_prefix, resolve_node_bin};
use super_agent_os::sandbox::{build_profile, sandbox_exec_argv};

#[test]
fn mock_pi_starts_under_base_profile() {
    let app_data = tempfile::tempdir().unwrap();
    let sp = build_profile(app_data.path(), &[], &[], &[], true, None).unwrap();
    // 在沙盒里跑 mock_pi（读一行 stdin 空 → 退出）——关键是不能 SIGABRT
    let inner = vec![env!("CARGO_BIN_EXE_mock_pi").to_string()];
    let argv = sandbox_exec_argv(&sp, &inner);
    let mut cmd = std::process::Command::new("/usr/bin/sandbox-exec");
    cmd.args(&argv).stdin(std::process::Stdio::piped());
    let mut child = cmd.spawn().expect("sandbox-exec spawn");
    drop(child.stdin.take()); // EOF → mock_pi 主循环结束退出
    let status = child.wait().unwrap();
    // SIGABRT(信号6) 会是 None code / signal 6；正常应 success 或干净退出
    assert!(
        status.success() || status.code().is_some(),
        "mock_pi 在 base profile 下异常终止(可能 base allow 集不足致 SIGABRT): {status:?}"
    );
}

/// P2 真实运行时加固任务的 crux：证明**真实 node**（不是 mock_pi 这种无自身路径/cwd
/// 依赖的纯 Rust 二进制）能在沙盒内干净启动。这是本任务两个修复的联合验证：
///
/// 1. `build_profile` 的 `runtime_paths`（渲染出 `file-read*` + `process-exec*`）——
///    没有它，`execvp` node 自身二进制（装在如 `~/.nvm/...` 这类不在 `BASE_PROFILE`
///    只读白名单内的路径）会直接 EPERM。
/// 2. `.current_dir($APP_DATA)`（`session_mgr::spawn_app_session` 的修复，此处手工
///    复现同等效果）——没有它，node 启动时 `uv_cwd()`（即 `getcwd()`）会因继承的
///    宿主 cwd 不在读白名单内而 EPERM，进程直接崩在启动期。
///
/// 依赖真实 `node` 存在于本机 `PATH`——不是所有开发机/CI 都装了 node，因此标记
/// `#[ignore]`（手工里程碑，类比 `.superpowers/sdd/task-5-report.md` 记录的
/// "real-node-boot" 待办），用 `cargo test -- --ignored` 显式跑。跑的时候若本机
/// 确实没有 node，测试体内部会打印诊断并提前返回（不是 panic），避免在偶然装了
/// 该套件但没装 node 的机器上把 `--ignored` 跑成一个误导性的红。
#[test]
#[ignore = "手工里程碑：依赖真实 node 装在本机 PATH 上，用 `cargo test -- --ignored` 显式跑"]
fn node_starts_under_sandbox() {
    let node = match resolve_node_bin(None) {
        Some(n) => n,
        None => {
            eprintln!("跳过 node_starts_under_sandbox：PATH 上未找到 node");
            return;
        }
    };
    let node_real = std::fs::canonicalize(&node).expect("canonicalize 真实 node 二进制路径");
    let prefix = install_prefix(&node_real)
        .expect("install_prefix 不应对真实 node 安装路径触发下限保护(路径太浅)")
        .expect("应能从 node 真实路径推导出安装前缀目录");

    let app_data = tempfile::tempdir().unwrap();
    // deny_network=true：本用例只验证启动，不涉及网络。
    let sp = build_profile(app_data.path(), &[], &[], &[prefix], true, None).unwrap();
    let inner = vec![
        node_real.to_string_lossy().to_string(),
        "-e".to_string(),
        "process.exit(0)".to_string(),
    ];
    let argv = sandbox_exec_argv(&sp, &inner);
    let status = std::process::Command::new("/usr/bin/sandbox-exec")
        .args(&argv)
        // 手工复现 session_mgr::spawn_app_session 的 cwd=$APP_DATA 修复。
        .current_dir(app_data.path())
        .status()
        .expect("spawn sandbox-exec 失败");
    assert!(
        status.success(),
        "真实 node 应能在沙盒(runtime_paths 放行自身安装前缀 + cwd=$APP_DATA)内干净启动，实际: {status:?}"
    );
}
