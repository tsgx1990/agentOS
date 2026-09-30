// 越权测试套件（P2 里程碑）：用真实 /usr/bin/sandbox-exec 跑受限 profile，
// 断言退出码（沙盒拒绝表现为非零退出/信号，不是 stdout 内容）。
// 仅 macOS 运行——依赖系统自带 /usr/bin/sandbox-exec。
#![cfg(target_os = "macos")]
use std::process::{Command, Stdio};
use super_agent_os::sandbox::{build_profile, sandbox_exec_argv};

/// 在受限 profile 下经真实 sandbox-exec 跑 `/bin/sh -c '<sh>'`，返回退出状态。
/// stdout/stderr 丢弃：沙盒拒绝的判据是退出码/信号，不是命令输出内容。
fn run_in_sandbox(
    app_data: &std::path::Path,
    deny_net: bool,
    sh: &str,
) -> std::process::ExitStatus {
    run_in_sandbox_with_mcp(app_data, deny_net, None, sh)
}

/// 同 `run_in_sandbox`，多一个可选的 `mcp_socket` 参数（Task 9c）：`Some(path)`
/// 时给 profile 加上"允许连接这一个 unix socket"的窄放行——用于本文件下方新增
/// 的 MCP socket 窄放行用例；`None` 时与 `run_in_sandbox` 完全等价。
fn run_in_sandbox_with_mcp(
    app_data: &std::path::Path,
    deny_net: bool,
    mcp_socket: Option<&std::path::Path>,
    sh: &str,
) -> std::process::ExitStatus {
    let sp = build_profile(app_data, &[], &[], &[], deny_net, mcp_socket).unwrap();
    let argv = sandbox_exec_argv(&sp, &["/bin/sh".into(), "-c".into(), sh.into()]);
    Command::new("/usr/bin/sandbox-exec")
        .args(&argv)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap()
}

/// P6-A：带额外只读/可写放行的变体。路径必须是调用方已 canonicalize 过的形式——`build_profile`
/// 内部对 `write_paths` 的 `canonicalize_or_fallback` 是幂等的，但 shell 命令里拼的路径字符串
/// 必须与放行规则用的是同一种（已规范化的）形式，否则会撞上 `write_inside_app_data_allowed`
/// 文档里记录的"`/var/...` 与其符号链接解析后的 `/private/var/...` 不是同一字符串"这条坑
/// （沙盒对 `subpath` 的匹配是对实际用到的路径字符串做前缀比较，不做符号链接解析）。
fn run_in_sandbox_with_paths(
    app_data: &std::path::Path,
    read_paths: &[std::path::PathBuf],
    write_paths: &[std::path::PathBuf],
    sh: &str,
) -> std::process::ExitStatus {
    let sp = build_profile(app_data, read_paths, write_paths, &[], true, None).unwrap();
    let argv = sandbox_exec_argv(&sp, &["/bin/sh".into(), "-c".into(), sh.into()]);
    Command::new("/usr/bin/sandbox-exec")
        .args(&argv)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap()
}

/// 用例 1：在 $APP_DATA 内写文件 —— 应被允许（exit success）。
///
/// **踩坑记录**：macOS 上 `tempfile::tempdir()` 落在 `$TMPDIR`（`/var/folders/...`），
/// 而 `/var` 是指向 `/private/var` 的符号链接。`build_profile` 对 `app_data` 做了
/// canonicalize（这是必须的安全行为，防 app_data 路径本身经符号链接被偷换），
/// 于是 WRITE 参数落地为 `/private/var/folders/...`。若这里 shell 命令仍用未
/// canonicalize 的原始路径（`/var/folders/...`）去写，会撞上一个已知的 macOS
/// sandbox 行为：`(subpath ...)` 是按路径前缀比较，不会把 `/var/...` 和它符号
/// 链接解析后等价的 `/private/var/...` 当同一路径处理，导致本应允许的写被误拒
/// （已用裸 sandbox-exec 手工复现，与 build_profile 逻辑无关）。因此这里统一用
/// canonicalize 后的路径构造 shell 命令，与 WRITE 参数的路径形式保持一致——这也
/// 正是真实系统里调用方应有的用法（app_data 只应有一种规范路径形式在流转）。
#[test]
fn write_inside_app_data_allowed() {
    let d = tempfile::tempdir().unwrap();
    let canon = std::fs::canonicalize(d.path()).unwrap();
    let s = run_in_sandbox(
        d.path(),
        true,
        &format!("echo hi > {}/ok.txt", canon.display()),
    );
    assert!(s.success(), "$APP_DATA 内写应允许");
}

/// 用例 2：在 $APP_DATA 外（另一个独立临时目录）写文件 —— 应被拒（非 success）。
#[test]
fn write_outside_denied() {
    let d = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    let s = run_in_sandbox(
        d.path(),
        true,
        &format!("echo x > {}/evil.txt", out.path().display()),
    );
    assert!(!s.success(), "$APP_DATA 外写应被拒");
}

/// 用例 3：禁网 profile 下尝试联网 —— 应被拒（非 success）。
///
/// 用直连字面 IP 的原始 TCP 连接（`nc -w2 -z <ip> <port>`）而非 curl+域名：
/// 避免因 DNS 解析失败/curl 缺失等环境因素造成假阳性——失败必须是因为沙盒拒绝了
/// network syscall，而不是命令本身或域名解析出问题。
/// （已人工核验：同一 nc 尝试在沙盒外能连通[exit 0]，证明命令/网络本身没问题，
/// 沙盒内失败[Operation not permitted]确系拒绝生效。**附加发现**：手工核验还发现
/// 当前 `render_profile` 在 `deny_network=false` 分支是空操作——BASE_PROFILE 本身
/// 没有任何 `(allow network*)` 规则，`deny default` 已把网络隐式拒绝，所以现状是
/// 网络访问其实*始终*被拒，与 deny_network 参数无关；即"受信任应用可放行网络"
/// [design 第 72/83 行] 尚未实现。这不影响本用例——本用例固定 deny_network=true，
/// 断言仍然成立——但意味着这条测试暂不能反向证明"deny_network 参数真的控制了
/// 网络放行"，只能证明"deny_network=true 时网络确实被拒"。已在任务报告中记录为
/// concern，留待后续任务补 `(allow network-outbound)` 分支。）
#[test]
fn network_denied() {
    let d = tempfile::tempdir().unwrap();
    let s = run_in_sandbox(d.path(), true, "nc -w2 -z 1.1.1.1 80");
    assert!(!s.success(), "禁网 profile 下联网应失败");
}

/// 用例 4：在 $APP_DATA 内建软链指向外部目录，经软链写外部 —— 应被拒
/// （canonicalize 防护：write_root 在 build_profile 里被定死为 app_data 的真实规范路径，
/// 软链解析后的真实目标不在该 subpath 内，内核 vnode 级检查会拒绝）。
#[test]
fn symlink_escape_denied() {
    let d = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    let link = d.path().join("escape");
    std::os::unix::fs::symlink(out.path(), &link).unwrap();
    let s = run_in_sandbox(
        d.path(),
        true,
        &format!("echo x > {}/evil.txt", link.display()),
    );
    assert!(
        !s.success(),
        "经 $APP_DATA 内软链写外部应被拒(canonicalize 防护)"
    );
}

/// 用例 5（P2 真实运行时加固任务新增）：在 $APP_DATA 内写一个文件，然后在**另一个**
/// 全新的沙盒子进程里把它读回来（`cat`）—— 应被允许。
///
/// 这条区别于用例 1（只证明"能写进去"）：`render_profile` 给 write_root 追加的
/// `(allow file-read* (subpath (param "WRITE")))` 目前只在字符串层面被单测覆盖过
/// （`sandbox.rs::render_write_root_also_readable` 只 `assert!(p.contains(...))`），
/// 从未在真实 `sandbox-exec` 下跑过——本用例补上运行时证明：读回自己刚写的数据
/// 这条路径确实在内核层面放行，不是"字符串里有这行规则但实际不生效"。
#[test]
fn read_back_own_app_data_allowed() {
    let d = tempfile::tempdir().unwrap();
    let canon = std::fs::canonicalize(d.path()).unwrap();
    let write = run_in_sandbox(
        d.path(),
        true,
        &format!("echo hello-readback > {}/data.txt", canon.display()),
    );
    assert!(write.success(), "前置：$APP_DATA 内写应先能成功");

    // 全新的沙盒子进程（新 sandbox-exec 调用），只做读，证明"读回自己刚写的数据"
    // 这条规则本身生效，而不是复用同一进程内的文件描述符侥幸读到。
    let read = run_in_sandbox(d.path(), true, &format!("cat {}/data.txt", canon.display()));
    assert!(
        read.success(),
        "应能在新的沙盒进程里读回 $APP_DATA 内自己写的文件"
    );
}

/// 用例 6（P2 真实运行时加固任务新增）：`deny_network=false`（可信应用）的 profile 下，
/// 真实联网应被允许 —— 补 T4 手工核验记录的 gap（`render_profile` 的 `!deny_network`
/// 分支此前是空操作，本任务已在 `render_profile`/`build_profile` 里修好，见
/// `render_no_deny_network_when_allowed` 的字符串级断言）：这里是运行时证明，
/// 用真实 `sandbox-exec` + 真实 TCP 连接验证 `(allow network*)` 确实到达内核，
/// 不只是字符串里有这一行。
///
/// 若当前环境完全没有出站网络，这条会假失败——已用 `nc` 在沙盒外手工核验过本机
/// 可连通 1.1.1.1:80（见 `network_denied` 用例同款直连字面 IP 的做法，避免 DNS
/// 解析失败/curl 缺失等环境因素造成假阳性）。如果之后换到一个确实无出站网络的
/// CI 环境导致本用例 flaky，应比照 `network_denied` 的做法加 `#[ignore]` 并写明原因，
/// 而不是让它间歇性变红。
#[test]
fn network_allowed_when_trusted() {
    let d = tempfile::tempdir().unwrap();
    let s = run_in_sandbox(d.path(), false, "nc -w2 -z 1.1.1.1 80");
    assert!(
        s.success(),
        "deny_network=false(可信应用) profile 下联网应被允许"
    );
}

/// 用例 7（权限收窄 review 新增）：读取既不在 `$APP_DATA` 也不在 `read_paths`/
/// `runtime_paths` 内的路径的**内容** —— 应被拒（非 success）。
///
/// 这条钉住本次收窄（用祖先目录 `literal` 元数据放行取代全局
/// `(allow file-read-metadata)`）之后依然成立的核心边界：新方案只放行了
/// `app_data`/`read_paths`/`runtime_paths` 各自祖先目录的**元数据**（stat/lstat，
/// 用于满足 node `fs.realpathSync` 的祖先 lstat 需求），从未放行任意路径的**内容**
/// 读取——沙盒外新建的一个完全独立的临时目录（既不是 `$APP_DATA` 也不在
/// `read_paths` 里，`run_in_sandbox` 固定传空 `read_paths`/`runtime_paths`）里的文件，
/// 其内容必须读不到。对应威胁模型第 4 条（不能读其他路径的内容）——放宽后的读
/// 元数据面让这条更值得钉一个回归用例。
#[test]
fn read_other_content_denied() {
    let d = tempfile::tempdir().unwrap();
    let secret_dir = tempfile::tempdir().unwrap();
    let secret = secret_dir.path().join("secret.txt");
    std::fs::write(&secret, "top-secret").unwrap();
    let s = run_in_sandbox(d.path(), true, &format!("cat {}", secret.display()));
    assert!(
        !s.success(),
        "$APP_DATA/read_paths 之外路径的内容读取应被拒(即使其祖先目录元数据可见)"
    );
}

// --- MCP socket 窄放行（Task 9c 新增，见 task-9c-report.md 手工核验记录） -----
//
// 背景：untrusted 第三方 app 的 pi 子进程在 macOS 上被 `sandbox-exec` 包住，
// `deny_network=true` 时整条 `(deny network*)` 挡死一切出站网络——这也挡住了它
// 连接 Task9/9b 起的宿主 MCP 桥 unix socket（`SUPERAGENT_MCP_SOCKET`），导致
// MCP 桥对受限 app 完全不可达。本任务给 `build_profile` 加一个可选的
// `mcp_socket_path`：非 None 时追加一条只按 `literal` 匹配这一个 socket 路径的
// `network-outbound` 窄放行——`(deny network*)` 之外仅此一个新增可达点。
//
// 三条用例合起来钉住这条改动的完整安全属性：(a) 该 socket 确实可连
// （否则整个改动是摆设）；(b) 只有它可连，同目录下别的 socket 依旧不可连
// （否则"窄"这个字不成立，等价于放行了整个目录/放行了所有 unix socket）；
// (c) 任意 TCP 出站依旧被拒（否则等价于重新打开了 network*，不是"窄"放行）。
//
// 均用真实 `std::os::unix::net::UnixListener::bind` 起监听器（而不是普通文件）
// ——`network-outbound` 的 `literal` 匹配针对的是 `connect(2)` 这个网络操作本身，
// 用一个真实绑定的 socket 而非任意文件更贴近生产环境（Task9b 的
// `McpSocketListener::start`）。`nc -U -w2 <path> </dev/null` 是本文件唯一验证过
// 在真实 sandbox-exec 下能正确反映"unix socket connect 成功/失败"的命令形态
// （手工核验记录：`nc -U -z` 在这台机器的 BSD nc 实现下对 unix socket 恒返回失败，
// 与沙盒无关，是命令本身的问题，见 task-9c-report.md）。

/// 用例 8（正）：`deny_network=true` 的受限 profile 若带上这一个 socket 路径的
/// 窄放行，沙盒内进程应能真的连接它——证明窄放行本身端到端生效，不只是字符串
/// 里有这一行。
#[test]
fn mcp_socket_allowed_connect_succeeds() {
    let d = tempfile::tempdir().unwrap();
    let sock_dir = tempfile::tempdir().unwrap();
    // 先 canonicalize 目录、再拼文件名：与 write_inside_app_data_allowed 同样的
    // 理由——`$TMPDIR` 落在 `/var/folders/...`（`/var` 是 `/private/var` 的符号
    // 链接），`network-outbound` 的 `literal` 匹配同 `subpath` 一样是按路径前缀/
    // 字面量精确比较，不会把两种拼法当同一路径，必须统一用规范化后的形式。
    let sock_dir_canon = std::fs::canonicalize(sock_dir.path()).unwrap();
    let sock_path = sock_dir_canon.join("mcp.sock");
    let _listener = std::os::unix::net::UnixListener::bind(&sock_path).unwrap();

    let s = run_in_sandbox_with_mcp(
        d.path(),
        true,
        Some(&sock_path),
        &format!("nc -U -w2 {} < /dev/null", sock_path.display()),
    );
    assert!(
        s.success(),
        "受限(deny_network=true) profile 下应能连接自己被窄放行的 MCP unix socket(真实 sandbox-exec)"
    );
}

/// 用例 9（负·narrowness，安全属性核心）：同一个 profile 只放行了一个 socket
/// 路径时，连接同目录下**另一个**真实存在且在监听的 unix socket 应被拒——证明
/// 放行是按单条路径字面量生效，不是按目录/按"任意 unix socket"生效。
#[test]
fn mcp_socket_narrow_denies_other_socket() {
    let d = tempfile::tempdir().unwrap();
    let sock_dir = tempfile::tempdir().unwrap();
    let sock_dir_canon = std::fs::canonicalize(sock_dir.path()).unwrap();
    let allowed_path = sock_dir_canon.join("mcp.sock");
    let other_path = sock_dir_canon.join("other.sock");
    let _allowed_listener = std::os::unix::net::UnixListener::bind(&allowed_path).unwrap();
    let _other_listener = std::os::unix::net::UnixListener::bind(&other_path).unwrap();

    let s = run_in_sandbox_with_mcp(
        d.path(),
        true,
        Some(&allowed_path),
        &format!("nc -U -w2 {} < /dev/null", other_path.display()),
    );
    assert!(
        !s.success(),
        "profile 只放行了 allowed_path，连接同目录下另一个真实在监听的 unix socket(other_path) 应被拒(否则窄放行名不副实)"
    );
}

/// 用例 10（负·未放宽 network*）：带 MCP socket 窄放行的 profile 下，任意 TCP
/// 出站（同 `network_denied` 用例的直连字面 IP 手法）依旧应被拒——证明这条窄放行
/// 没有连带重新打开 `network*`。
#[test]
fn mcp_socket_allow_does_not_reopen_general_network() {
    let d = tempfile::tempdir().unwrap();
    let sock_dir = tempfile::tempdir().unwrap();
    let sock_dir_canon = std::fs::canonicalize(sock_dir.path()).unwrap();
    let sock_path = sock_dir_canon.join("mcp.sock");
    let _listener = std::os::unix::net::UnixListener::bind(&sock_path).unwrap();

    let s = run_in_sandbox_with_mcp(d.path(), true, Some(&sock_path), "nc -w2 -z 1.1.1.1 80");
    assert!(
        !s.success(),
        "带 MCP socket 窄放行的 profile 下，任意 TCP 出站(1.1.1.1:80)依旧应被拒——窄放行不应重新打开 network*"
    );
}

// --- P6-A：filesystem 能力的多写根 / 只读根越权测试 ---------------------------
//
// **fixture 必须落在 `$HOME` 下，不能用裸 `tempfile::tempdir()`**（P6-A 代码评审 Finding I2）：
// `BASE_PROFILE` 本身已经无条件放行 `(subpath "/private/var")` 的 file-read*（系统工具链启动
// 需要，见该常量定义处），而 `tempfile::tempdir()` 落在 `$TMPDIR`（`/var/folders/...`，`/var`
// 是指向 `/private/var` 的符号链接）。评审用真实 sandbox-exec 手工核验过：把这两条新用例的
// fixture 放在裸 tempdir 下时，"声明只读目录可读"与"未声明目录读被拒"这两条断言无论
// `read_paths` 传不传都得到相同结果——前者是因为不管有没有声明都会撞上 `/private/var` 的
// 全局放行，后者是因为（照搬 `read_other_content_denied` 的手法）用了未 canonicalize 的
// `/var/...` 字符串、根本没匹配上任何规则——两条断言测的都不是 `read_paths` 机制本身，是假阳性/
// 假阴性的巧合。`$HOME`（如 `/Users/<user>`）不在 `BASE_PROFILE` 任何一条 subpath 规则里，
// 这里用 `tempfile::Builder::tempdir_in($HOME)` 换一个不会被系统规则污染、又能保留
// `tempfile::TempDir` 的 Drop 自动清理（含断言失败 panic 时也清理，因为本 crate 不设
// `panic = "abort"`）的落地点。

/// 声明只读目录 → 该目录下文件可读（cat 退出 0，唯一由 `read_paths` 放行才能成立的断言）；
/// 只读放行不授予写（echo 重定向不产生文件，唯一从一开始就能真正判别的断言）；未声明目录
/// 读仍被拒（cat 非零退出，同样只有落在 `$HOME` 下才真正判别——见上方 fixture 说明）。
#[test]
fn declared_read_path_is_readable_but_not_writable_and_undeclared_stays_denied() {
    let home = dirs::home_dir().expect("测试环境需要可解析的 $HOME");
    let root = tempfile::Builder::new()
        .prefix(".superagent-escape-test-")
        .tempdir_in(&home)
        .expect("在 $HOME 下建 fixture 目录失败");
    let root = std::fs::canonicalize(root.path()).unwrap();

    let app_data = root.join("app_data");
    std::fs::create_dir_all(&app_data).unwrap();
    let app_data = std::fs::canonicalize(&app_data).unwrap();
    // 用一个 fixture 目录扮演「$DOWNLOADS」，避免污染真实下载目录；变量展开本身在 filesystem.rs 单测里验。
    let downloads = root.join("downloads");
    std::fs::create_dir_all(&downloads).unwrap();
    let downloads = std::fs::canonicalize(&downloads).unwrap();
    let probe = downloads.join("probe.txt");
    std::fs::write(&probe, "secret").unwrap();
    let undeclared = root.join("documents");
    std::fs::create_dir_all(&undeclared).unwrap();
    let undeclared = std::fs::canonicalize(&undeclared).unwrap();
    std::fs::write(undeclared.join("x.txt"), "x").unwrap();

    let read_ok = run_in_sandbox_with_paths(
        &app_data,
        std::slice::from_ref(&downloads),
        &[],
        &format!("cat {}", probe.display()),
    );
    assert!(read_ok.success(), "声明的只读目录必须可读");

    let write_target = downloads.join("escaped.txt");
    let _ = run_in_sandbox_with_paths(
        &app_data,
        std::slice::from_ref(&downloads),
        &[],
        &format!("echo x > {}", write_target.display()),
    );
    assert!(!write_target.exists(), "只读放行不得允许写");

    let denied = run_in_sandbox_with_paths(
        &app_data,
        std::slice::from_ref(&downloads),
        &[],
        &format!("cat {}", undeclared.join("x.txt").display()),
    );
    assert!(!denied.success(), "未声明目录读必须被拒");
}

/// 声明可写子目录 → 子目录内可写（文件出现）；其父目录仍不可写。fixture 同上放在 `$HOME` 下
/// （与上一条用例统一做法，虽然本用例即使在裸 tempdir 下也已能真正判别——`BASE_PROFILE` 的
/// `/private/var` 放行只覆盖读，不覆盖写）。
#[test]
fn declared_write_subpath_is_writable_but_parent_is_not() {
    let home = dirs::home_dir().expect("测试环境需要可解析的 $HOME");
    let root = tempfile::Builder::new()
        .prefix(".superagent-escape-test-")
        .tempdir_in(&home)
        .expect("在 $HOME 下建 fixture 目录失败");
    let root = std::fs::canonicalize(root.path()).unwrap();

    let app_data = root.join("app_data");
    std::fs::create_dir_all(&app_data).unwrap();
    let app_data = std::fs::canonicalize(&app_data).unwrap();
    let desktop = root.join("desktop");
    std::fs::create_dir_all(&desktop).unwrap();
    let desktop = std::fs::canonicalize(&desktop).unwrap();
    let sub = desktop.join("export");
    std::fs::create_dir_all(&sub).unwrap();

    let inside = sub.join("out.txt");
    let st = run_in_sandbox_with_paths(
        &app_data,
        &[],
        std::slice::from_ref(&sub),
        &format!("echo ok > {}", inside.display()),
    );
    assert!(st.success() && inside.exists(), "声明的可写子目录必须可写");

    let outside = desktop.join("escaped.txt");
    let _ = run_in_sandbox_with_paths(
        &app_data,
        &[],
        std::slice::from_ref(&sub),
        &format!("echo x > {}", outside.display()),
    );
    assert!(!outside.exists(), "父目录不得可写");
}
