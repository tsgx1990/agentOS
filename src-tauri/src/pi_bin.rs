use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// 进程级登记随包 pi 二进制的真实路径（P6-E）：生产打包内由
/// `lib.rs::setup()` 用 `resource_dir()/pi/pi` 算出后调用 `register_bundled_pi`
/// 登记一次；`OnceLock` 是进程级单例，天然满足"只登记一次、后来者不覆盖"。
static BUNDLED_PI: OnceLock<PathBuf> = OnceLock::new();

/// 登记随包 pi 二进制路径。`path` 必须真实存在且是文件才登记成功——调用方
/// （`lib.rs::setup()`）传来的路径来自 `resource_dir()` 拼接，dev/未打包环境下
/// 这个目录本就可能不存在，登记失败是预期路径，不是错误。已登记过时后续调用
/// 忽略（`OnceLock::set` 本身的语义）并返回 `false`——不会覆盖首次登记的值。
///
/// 单测只测"路径不存在 → false"这一条（见 `mod tests`）：`path.is_file()` 为
/// false 时函数在触碰 `BUNDLED_PI.set` 之前就返回，不会污染进程级状态；反过来，
/// 一个真的会调用 `.set()` 成功的单测会把 `BUNDLED_PI` 永久设成测试用的假路径，
/// 污染同一测试二进制里其它调用 `resolve_pi_bin()` 的用例（`OnceLock` 无法重置）。
/// 优先级/覆盖行为改由下面的纯函数 `resolve_pi_bin_with` 做穷尽单测。
pub fn register_bundled_pi(path: PathBuf) -> bool {
    if !path.is_file() {
        return false;
    }
    BUNDLED_PI.set(path).is_ok()
}

/// 纯函数：解析顺序的核心决策逻辑，四级来源按优先级排列：
/// ① `env`（`SUPERAGENT_PI_BIN` 环境变量，调用方已读好传入）
/// ② `bundled`（随包资源登记的路径，调用方已用 `register_bundled_pi` 校验过存在性）
/// ③ `dev_candidate`（开发检出里的 `src-tauri/binaries/pi/pi`，这里现查
///    `is_file()`——它没有像 bundled 那样经过前置注册校验）
/// ④ 裸名 `"pi"`，交给 PATH 解析
///
/// 拆成纯函数只是为了不依赖进程级 `OnceLock`/真实环境变量就能穷尽单测四级
/// 优先级；`resolve_pi_bin()` 只是把三个真实来源喂给它。
fn resolve_pi_bin_with(env: Option<&str>, bundled: Option<&Path>, dev_candidate: &Path) -> PathBuf {
    if let Some(p) = env {
        return PathBuf::from(p);
    }
    if let Some(b) = bundled {
        return b.to_path_buf();
    }
    if dev_candidate.is_file() {
        return dev_candidate.to_path_buf();
    }
    PathBuf::from("pi")
}

pub fn resolve_pi_bin() -> PathBuf {
    let env = std::env::var("SUPERAGENT_PI_BIN").ok();
    let dev_candidate = Path::new("src-tauri/binaries/pi/pi");
    resolve_pi_bin_with(
        env.as_deref(),
        BUNDLED_PI.get().map(PathBuf::as_path),
        dev_candidate,
    )
}

/// 纯函数部分：在给定的目录列表里找一个名为 `name` 的可执行文件，找不到返回 `None`。
/// 拆成纯函数是为了不依赖真实全局 `PATH` 环境变量就能单测（`find_in_path` 是它接读
/// 真实 `PATH` 的薄封装）。
fn find_in_dirs(name: &str, dirs: &[PathBuf]) -> Option<PathBuf> {
    for dir in dirs {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// 在 `PATH` 环境变量列出的目录里找一个名为 `name` 的可执行文件，找不到返回 `None`。
/// 是 `which <name>` 的最小内联实现（不引入额外依赖）。
fn find_in_path(name: &str) -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    let dirs: Vec<PathBuf> = std::env::split_paths(&path_var).collect();
    find_in_dirs(name, &dirs)
}

/// 读 `path` 文件头部（最多 512 字节，够放下任何现实的 shebang 行）找 `#!` 解释器：
/// `#!/usr/bin/env node` → 返回 `"node"`；`#!/abs/path/to/node` → 返回该绝对路径本身。
/// 不是文本文件/没有 shebang/读不到都返回 `None`（不当作错误——pi 也可能被打成原生二进制）。
fn shebang_interpreter(path: &Path) -> Option<String> {
    use std::io::Read;
    let mut f = std::fs::File::open(path).ok()?;
    let mut buf = [0u8; 512];
    let n = f.read(&mut buf).ok()?;
    let text = String::from_utf8_lossy(&buf[..n]);
    let first_line = text.lines().next()?;
    let rest = first_line.strip_prefix("#!")?.trim();
    let mut parts = rest.split_whitespace();
    let first = parts.next()?;
    if first.ends_with("env") {
        parts.next().map(|s| s.to_string())
    } else {
        Some(first.to_string())
    }
}

/// 解析真实沙盒内要跑的 node 解释器路径：
/// 1. `SUPERAGENT_NODE_BIN` 环境变量覆盖（测试/生产 sidecar 用）；
/// 2. 否则读 `pi_real`（`resolve_pi_bin()` canonicalize 后的真实文件）的 shebang——
///    `#!/usr/bin/env node` 这种给裸名，再去 `PATH` 里找；`#!/abs/path/node` 这种
///    直接是绝对路径，能读到即用；
/// 3. 上面都拿不到，退化为直接在 `PATH` 里找 `node`。
///
/// 找不到就是 `None`（不算错误——只影响 P2 的"多放行一个运行时目录"，找不到则
/// 该沙盒 profile 里就不含 node 的运行时放行，manual milestone 会在真实机器上暴露）。
pub fn resolve_node_bin(pi_real: Option<&Path>) -> Option<PathBuf> {
    if let Ok(p) = std::env::var("SUPERAGENT_NODE_BIN") {
        return Some(PathBuf::from(p));
    }
    if let Some(real) = pi_real {
        if let Some(interp) = shebang_interpreter(real) {
            if interp.contains('/') {
                let p = PathBuf::from(&interp);
                if p.is_absolute() && p.exists() {
                    return Some(p);
                }
            } else if let Some(found) = find_in_path(&interp) {
                return Some(found);
            }
        }
    }
    find_in_path("node")
}

/// install_prefix 的下限保护：拒绝把 `/` 或分量过少（< 3，即根 + 至多 1 个真实
/// 路径段）的路径当作"安装前缀"返回。这类过浅的前缀喂给 `sandbox::build_profile`
/// 的 `runtime_paths` 会渲染成 `(subpath (param "RT_i"))` 形式的
/// `file-read*`/`process-exec*`——`subpath("/")` 等价于放行整个文件系统读+执行，
/// `subpath("/bin")` 同样危险到接近全盘（覆盖系统全部基础二进制的读+exec）。
/// 真实安装目录（`/Users/x/.nvm/versions/node/vY` 这类）远比这深；触发下限说明
/// 推导本身出了问题（比如一个原生二进制真被摆在 `/bin/x` 或 `/lib/x`），此时应该
/// 拒绝生成 profile，而不是悄悄放行一条覆盖近乎整个文件系统的规则。
fn reject_shallow_prefix(prefix: PathBuf) -> Result<Option<PathBuf>, String> {
    if prefix.components().count() < 3 {
        return Err(format!(
            "install_prefix 推导出的前缀 {} 过浅（分量数 < 3，疑似非正常安装路径），\
             拒绝生成可能放行整个文件系统的 subpath 规则",
            prefix.display()
        ));
    }
    Ok(Some(prefix))
}

/// 从一个已 canonicalize 的真实文件路径推导它的"安装前缀"目录：向上找最近一个
/// 名为 `bin` 或 `lib` 的路径分量，返回其父目录。
///
/// nvm 布局下这精确落在 `~/.nvm/versions/node/vX.Y.Z/`——该目录同时持有
/// `bin/`（node 二进制本身）、`lib/node_modules/...`（pi 的 cli.js 与其全部依赖）、
/// `include/`——node 启动/require 自身模块都从这一整棵树下分页读取，只放行
/// `bin/` 或只放行 `lib/` 都不够，必须是两者共同的父目录。
///
/// 找不到 `bin`/`lib` 分量（比如某些非常规打包布局）时退化为文件的直接父目录——
/// 没那么精确，但优于完全不放行（沙盒仍会因该目录之外的路径被拒读/拒 exec）。
///
/// **下限保护**（权限收窄 review 新增）：无论走哪条分支推出的前缀，都要先过
/// `reject_shallow_prefix`——`/bin/x`、`/lib/x` 这类文件会让上面的 `bin`/`lib`
/// 分量匹配返回 `/`（其 parent），若不设下限会让调用方把 `subpath("/")` 塞进沙盒
/// profile，等价于放行整个文件系统的读 + exec。返回 `Err` 而不是 `Ok(None)`——
/// 这不是"正常的解析不出来"（那种情况本就该静默留空），而是"解析出了一个危险值"，
/// 必须让调用方（最终是 `sandbox::build_profile`）当作硬错误处理。
pub fn install_prefix(real: &Path) -> Result<Option<PathBuf>, String> {
    for ancestor in real.ancestors() {
        if let Some(name) = ancestor.file_name() {
            if name == "bin" || name == "lib" {
                if let Some(parent) = ancestor.parent() {
                    return reject_shallow_prefix(parent.to_path_buf());
                }
            }
        }
    }
    match real.parent() {
        Some(p) => reject_shallow_prefix(p.to_path_buf()),
        None => Ok(None),
    }
}

/// 汇总沙盒需要放行 read+exec 的运行时安装目录：`resolve_pi_bin()` 本身（canonicalize
/// 解开符号链接后的真实文件）与其 node 解释器，各自推导 `install_prefix` 后去重。
///
/// 供 `session_mgr::sandboxed_argv` 把这些目录喂给 `sandbox::build_profile` 的
/// `runtime_paths` 参数——解决真实 pi/node 在 `BASE_PROFILE` 下 `execvp` EPERM
/// （自身安装路径不在只读白名单内，见 task-5-report.md 记录的手工探测）。
///
/// 任何一步*解析不出来*（pi_bin/node 本就不存在、canonicalize 失败）都只是让返回的
/// `Vec` 缺一项，不返回 `Err`——找不到运行时目录不该阻塞非 macOS/未装 pi 的开发环境。
///
/// 但若 `install_prefix` 因下限保护（`reject_shallow_prefix`）返回 `Err`——即推出的
/// 前缀是 `/` 或过浅——这不是"解析不出来"，是"解析出了一个危险值"，用 `?` 原样
/// 向上传播（最终传到 `session_mgr::sandboxed_argv`/`sandbox::build_profile` 的
/// `Result`），不能静默吞掉继续用这个危险前缀。
pub fn runtime_install_dirs() -> Result<Vec<PathBuf>, String> {
    let mut dirs = Vec::new();
    let pi_bin = resolve_pi_bin();
    let pi_real = std::fs::canonicalize(&pi_bin).ok();
    if let Some(real) = &pi_real {
        if let Some(prefix) = install_prefix(real)? {
            if !dirs.contains(&prefix) {
                dirs.push(prefix);
            }
        }
    }
    if let Some(node_bin) = resolve_node_bin(pi_real.as_deref()) {
        if let Ok(node_real) = std::fs::canonicalize(&node_bin) {
            if let Some(prefix) = install_prefix(&node_real)? {
                if !dirs.contains(&prefix) {
                    dirs.push(prefix);
                }
            }
        }
    }
    Ok(dirs)
}

#[tauri::command]
pub async fn pi_version() -> Result<String, String> {
    let out = tokio::process::Command::new(resolve_pi_bin())
        .arg("--version")
        .output()
        .await
        .map_err(|e| format!("无法启动 pi：{e}"))?;
    if !out.status.success() {
        return Err(format!("pi --version 退出码非零：{}", out.status));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // `std::env::set_var`/`remove_var` 改变进程全局状态，但 cargo 默认在并行线程中运行测试。
    // 不序列化访问会导致这两个测试在 `SUPERAGENT_PI_BIN` 上竞争并间歇性失败
    // （已通过实验确认）。此锁保证确定性。
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn resolve_prefers_env_override() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("SUPERAGENT_PI_BIN", "/tmp/fake-pi");
        assert_eq!(resolve_pi_bin(), PathBuf::from("/tmp/fake-pi"));
        std::env::remove_var("SUPERAGENT_PI_BIN");
    }

    #[test]
    fn resolve_defaults_to_path_pi() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::remove_var("SUPERAGENT_PI_BIN");
        assert_eq!(resolve_pi_bin(), PathBuf::from("pi"));
    }

    // --- resolve_pi_bin_with（纯函数，四级解析顺序穷尽单测；不碰真实
    // env/OnceLock，见 register_bundled_pi 文档为什么这里必须走纯函数） -------

    #[test]
    fn resolve_with_env_wins_over_everything() {
        let dir = tempfile::tempdir().unwrap();
        let bundled = dir.path().join("bundled-pi");
        std::fs::write(&bundled, "").unwrap();
        let dev = dir.path().join("dev-pi");
        std::fs::write(&dev, "").unwrap();
        assert_eq!(
            resolve_pi_bin_with(Some("/env/pi"), Some(&bundled), &dev),
            PathBuf::from("/env/pi"),
            "① env 必须优先于 ②③④"
        );
    }

    #[test]
    fn resolve_with_bundled_wins_when_no_env() {
        let dir = tempfile::tempdir().unwrap();
        let bundled = dir.path().join("bundled-pi");
        std::fs::write(&bundled, "").unwrap();
        let dev = dir.path().join("dev-pi");
        std::fs::write(&dev, "").unwrap();
        assert_eq!(
            resolve_pi_bin_with(None, Some(&bundled), &dev),
            bundled,
            "② bundled 必须优先于 ③④（无 env 时）"
        );
    }

    #[test]
    fn resolve_with_dev_candidate_used_when_no_env_no_bundled() {
        let dir = tempfile::tempdir().unwrap();
        let dev = dir.path().join("dev-pi");
        std::fs::write(&dev, "").unwrap();
        assert_eq!(
            resolve_pi_bin_with(None, None, &dev),
            dev,
            "③ dev 候选存在时必须被选中（无 env/bundled 时）"
        );
    }

    #[test]
    fn resolve_with_bare_pi_when_nothing_else_available() {
        let dir = tempfile::tempdir().unwrap();
        let dev = dir.path().join("does-not-exist");
        assert_eq!(
            resolve_pi_bin_with(None, None, &dev),
            PathBuf::from("pi"),
            "④ 兜底裸名 pi（dev 候选不存在时）"
        );
    }

    // --- register_bundled_pi（只测"不存在的路径 → false"这一条，见函数文档
    // 为什么不能写一个会真的 `.set()` 成功的单测） --------------------------

    #[test]
    fn register_bundled_pi_returns_false_for_nonexistent_path() {
        assert!(!register_bundled_pi(PathBuf::from(
            "/definitely/does/not/exist/pi"
        )));
    }

    // --- install_prefix ---------------------------------------------------

    #[test]
    fn install_prefix_finds_bin_ancestor() {
        // nvm 布局：.../v20.19.5/bin/node → 前缀是 v20.19.5
        let p = Path::new("/x/versions/node/v20.19.5/bin/node");
        assert_eq!(
            install_prefix(p),
            Ok(Some(PathBuf::from("/x/versions/node/v20.19.5")))
        );
    }

    #[test]
    fn install_prefix_finds_lib_ancestor() {
        // nvm 布局：.../v20.19.5/lib/node_modules/@x/pi/dist/cli.js → 前缀同样是 v20.19.5
        let p = Path::new("/x/versions/node/v20.19.5/lib/node_modules/@x/pi/dist/cli.js");
        assert_eq!(
            install_prefix(p),
            Ok(Some(PathBuf::from("/x/versions/node/v20.19.5")))
        );
    }

    #[test]
    fn install_prefix_falls_back_to_parent_when_no_bin_or_lib() {
        // 非常规布局：路径分量里没有 bin/lib → 退化为直接父目录。
        let p = Path::new("/opt/tools/pi");
        assert_eq!(install_prefix(p), Ok(Some(PathBuf::from("/opt/tools"))));
    }

    // --- install_prefix 下限保护（权限收窄 review 新增） ---------------------

    #[test]
    fn install_prefix_rejects_root_prefix_from_top_level_bin() {
        // /bin/x → "bin" 分量的 parent 是 "/"：若不拒绝，会让 build_profile 生成
        // subpath("/")，等价于放行整个文件系统的读 + exec。
        let p = Path::new("/bin/x");
        assert!(
            install_prefix(p).is_err(),
            "前缀为 / 时必须 Err，而不是静默放行全盘"
        );
    }

    #[test]
    fn install_prefix_rejects_root_prefix_from_top_level_lib() {
        // /lib/x → 同上，"lib" 分量的 parent 同样是 "/"。
        let p = Path::new("/lib/x");
        assert!(
            install_prefix(p).is_err(),
            "前缀为 / 时必须 Err，而不是静默放行全盘"
        );
    }

    #[test]
    fn install_prefix_rejects_shallow_fallback_prefix() {
        // 没有 bin/lib 分量、直接父目录退化 的情形下，前缀过浅（分量数 < 3）同样要拒绝：
        // /a/x → 退化前缀 "/a"（分量数 2：Root + "a"）。
        let p = Path::new("/a/x");
        assert!(install_prefix(p).is_err(), "退化前缀过浅时也必须 Err");
    }

    #[test]
    fn install_prefix_accepts_two_segment_fallback_prefix() {
        // 现有回归基线：/opt/tools/pi 的退化前缀 "/opt/tools"（分量数 3）不应被下限拒绝，
        // 与 install_prefix_falls_back_to_parent_when_no_bin_or_lib 呼应，确认下限阈值
        // 没有误伤这条已有合法用例。
        let p = Path::new("/opt/tools/pi");
        assert_eq!(install_prefix(p), Ok(Some(PathBuf::from("/opt/tools"))));
    }

    // --- find_in_dirs（纯函数，不碰真实 PATH） -----------------------------

    #[test]
    fn find_in_dirs_finds_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("node");
        std::fs::write(&f, "").unwrap();
        assert_eq!(find_in_dirs("node", &[dir.path().to_path_buf()]), Some(f));
    }

    #[test]
    fn find_in_dirs_returns_none_when_absent() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(find_in_dirs("node", &[dir.path().to_path_buf()]), None);
    }

    // --- shebang_interpreter -------------------------------------------------

    #[test]
    fn shebang_interpreter_env_node_gives_bare_name() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("pi");
        std::fs::write(&script, "#!/usr/bin/env node\nconsole.log(1)\n").unwrap();
        assert_eq!(shebang_interpreter(&script), Some("node".to_string()));
    }

    #[test]
    fn shebang_interpreter_absolute_interpreter_path() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("pi");
        std::fs::write(&script, "#!/abs/path/to/node\n").unwrap();
        assert_eq!(
            shebang_interpreter(&script),
            Some("/abs/path/to/node".to_string())
        );
    }

    #[test]
    fn shebang_interpreter_none_when_no_shebang() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("pi");
        std::fs::write(&script, "not a script\n").unwrap();
        assert_eq!(shebang_interpreter(&script), None);
    }

    // --- resolve_node_bin ---------------------------------------------------

    #[test]
    fn resolve_node_bin_prefers_env_override() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("SUPERAGENT_NODE_BIN", "/tmp/fake-node");
        assert_eq!(
            resolve_node_bin(None),
            Some(PathBuf::from("/tmp/fake-node"))
        );
        std::env::remove_var("SUPERAGENT_NODE_BIN");
    }

    #[test]
    fn resolve_node_bin_uses_absolute_shebang_interpreter() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::remove_var("SUPERAGENT_NODE_BIN");
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("pi");
        let node = dir.path().join("node");
        std::fs::write(&node, "").unwrap();
        std::fs::write(&script, format!("#!{}\n", node.display())).unwrap();
        assert_eq!(resolve_node_bin(Some(&script)), Some(node));
    }

    // --- runtime_install_dirs -----------------------------------------------

    #[test]
    fn runtime_install_dirs_dedupes_when_pi_and_node_share_prefix() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let pi = bin.join("pi");
        let node = bin.join("node");
        std::fs::write(&pi, "#!/usr/bin/env node\n").unwrap();
        std::fs::write(&node, "").unwrap();
        std::env::set_var("SUPERAGENT_PI_BIN", &pi);
        std::env::set_var("SUPERAGENT_NODE_BIN", &node);
        let dirs = runtime_install_dirs().unwrap();
        std::env::remove_var("SUPERAGENT_PI_BIN");
        std::env::remove_var("SUPERAGENT_NODE_BIN");
        // pi 与 node 都在同一个 vX/bin/ 下 → install_prefix 都指向 vX 本身 → 去重后只有一条。
        assert_eq!(dirs, vec![std::fs::canonicalize(dir.path()).unwrap()]);
    }

    #[test]
    fn runtime_install_dirs_empty_when_pi_unresolvable() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("SUPERAGENT_PI_BIN", "/definitely/does/not/exist/pi");
        std::env::set_var("SUPERAGENT_NODE_BIN", "/definitely/does/not/exist/node");
        let dirs = runtime_install_dirs().unwrap();
        std::env::remove_var("SUPERAGENT_PI_BIN");
        std::env::remove_var("SUPERAGENT_NODE_BIN");
        assert!(
            dirs.is_empty(),
            "canonicalize 失败时应静默留空,不 panic/Err"
        );
    }
}
