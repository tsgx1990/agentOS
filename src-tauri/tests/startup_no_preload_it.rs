//! 「启动不预载应用会话」不变式（P6-F）。
//!
//! 这是源码层面的守卫，不是运行期证明：setup 闭包绑定 Wry 运行时、会建窗口并
//! 拉起真实 pi，没法在测试里直接跑。这里钉住两条最可能被回归破坏的结构事实：
//! 启动路径里没有任何「恢复上次打开的应用」，以及打开应用会话只有一个入口。
//!
//! 注意：主助手会话（`start_main_session` → `main_session_launch`）与 MCP server
//! 在启动时**会**起进程，这是设计如此；不变式只约束「应用会话」：启动时
//! `app_sessions` 为空，应用只在用户点开时才被打开。

use std::path::Path;

use super_agent_os::app_state::AppState;

const FORBIDDEN_IN_SETUP: [&str; 5] = [
    "open_app",
    "app_sessions",
    "spawn_app_session",
    "spawn_task_session",
    "run_catch_up_for_app",
];

#[test]
fn setup_block_never_opens_app_sessions() {
    let src = include_str!("../src/lib.rs");
    let start = src
        .find(".setup(|app| {")
        .expect("找不到 .setup(|app| { 锚点，lib.rs 结构变了，请更新本测试");
    let end = src[start..]
        .find(".run(tauri::generate_context!())")
        .expect("找不到 .run(tauri::generate_context!()) 锚点")
        + start;
    let block = &src[start..end];

    assert!(
        block.contains("start_main_session") && block.contains("start_scheduler_loop"),
        "截取到的 setup 片段缺少锚点，可能截空了"
    );
    for word in FORBIDDEN_IN_SETUP {
        assert!(
            !block.contains(word),
            "setup 里出现了 `{word}`：打开应用会话只能由用户触发，见 P6-F 不变式"
        );
    }
}

fn collect_rs(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    for entry in std::fs::read_dir(dir).expect("读取源码目录失败") {
        let path = entry.expect("目录项").path();
        if path.is_dir() {
            collect_rs(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn open_app_has_single_entry_point() {
    let mut files = Vec::new();
    collect_rs(
        Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/src")),
        &mut files,
    );
    assert!(!files.is_empty(), "没有读到任何源码文件");
    let mut hits = Vec::new();
    for f in &files {
        let text = std::fs::read_to_string(f).expect("读取源码失败");
        for (i, line) in text.lines().enumerate() {
            if line.contains("session_mgr::open_app(") {
                hits.push(format!("{}:{}", f.display(), i + 1));
            }
        }
    }
    assert_eq!(
        hits.len(),
        1,
        "session_mgr::open_app( 应当只在 lib.rs 的 open_app 命令里出现一次，实际 {hits:?}；\
         打开应用会话只能由用户触发，见 P6-F 不变式"
    );
}

#[tokio::test]
async fn default_app_state_has_no_app_sessions() {
    let state = AppState::default();
    assert!(state.app_sessions.lock().await.is_empty());
    assert!(state.activity.snapshot().is_empty());
    assert!(state.activity.dormant().is_empty());
}
