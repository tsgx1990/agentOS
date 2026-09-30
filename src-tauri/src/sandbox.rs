// 沙盒 SBPL（Sandbox Profile Language）生成与管理

/// 沙盒基础策略：Node/dyld 启动所需的最小 allow 集 + process/device 权限。
/// 源自 OpenAI Codex（<https://github.com/openai/codex>，Apache-2.0）的
/// `seatbelt_base_policy.sbpl`，在其基础上做了删减与改写（来源声明见仓库根 `NOTICE`），
/// 包含系统启动路径只读与 /dev/null 写入。
pub const BASE_PROFILE: &str = r#"(version 1)
(deny default)
; 子进程继承本策略
(allow process-exec)
(allow process-fork)
(allow signal (target same-sandbox))
(allow process-info* (target same-sandbox))
; /dev/null
(allow file-write-data (require-all (path "/dev/null") (vnode-type CHARACTER-DEVICE)))
; 运行时启动探测
(allow sysctl-read)
(allow mach-lookup (global-name "com.apple.system.opendirectoryd.libinfo"))
(allow ipc-posix-sem)
(allow mach-lookup
  (global-name "com.apple.cfprefsd.daemon")
  (global-name "com.apple.cfprefsd.agent")
  (local-name "com.apple.cfprefsd.agent"))
(allow user-preference-read)
; node/dyld 启动必须的系统只读(缺则 SIGABRT)
(allow file-read* (literal "/"))
(allow file-read*
  (subpath "/usr") (subpath "/System") (subpath "/Library")
  (subpath "/bin") (subpath "/sbin") (subpath "/opt")
  (subpath "/private/var") (subpath "/private/etc") (subpath "/private/tmp")
  (subpath "/dev"))
; 真实 node 启动时 CommonJS loader 对入口脚本路径调用 fs.realpathSync（本质是对
; 该绝对路径每一级祖先目录逐个 lstat，例如 pi 装在 /Users/x/.nvm/... 时会
; lstat("/Users")、lstat("/Users/x")……）——`subpath` 谓词只匹配目标路径本身与其
; 后代，不覆盖祖先目录，即使最终 runtime_paths 已放行 .nvm 内的安装前缀，祖先目录
; （如 /Users）仍会被 `deny default` 拒绝，导致 EPERM/lstat 崩在启动期。
;
; 初版修复（见 `.superpowers/sdd/task-5-report.md`）曾在此处加一条全局的
; file-read-metadata allow（不带任何路径限定）——放行*任意路径*的 stat/lstat
; （只是元数据，不含文件内容），确实修好了启动问题，但代价是把整个文件系统变成
; 一个 stat oracle：沙盒内的（可能不受信的第三方）进程能探测 /Users/<其他用户>、
; ~/.ssh、其他已装应用的数据目录是否存在、大小、mtime、mode——权限收窄 review
; 判定这比实际需要宽得多（威胁模型只要求"能启动"，不要求"能看到全盘元数据"）。
;
; 收窄后：BASE_PROFILE 本身不再含任何不限路径的 file-read-metadata 规则。改由
; `build_profile`（同文件下方）为每个实际会被 lstat 到的路径——app_data(WRITE)、
; 每个 read_path、每个 runtime_path——各自的**祖先目录**分别生成一条限定
; `(literal (param "ANC_i"))` 的 file-read-metadata allow（见 `strict_ancestors`）。
; 用 `literal` 而非 `subpath`：只放行该祖先目录自身的元数据，不放行其子目录/
; 兄弟目录——兄弟应用目录的存在性依旧对沙盒内进程不可见。"/" 本身已被上面
; `(allow file-read* (literal "/"))` 覆盖，不需要再放行，因此严格祖先链算到
; "/" 前一级为止。"#;

/// 沙盒配置描述：既含渲染完整的 SBPL 策略，也包含对应的 param 映射（参数名->实际路径）。
/// T2（build_profile）与 T3-5（沙盒启动）会分别用。
pub struct SandboxProfile {
    /// 渲染后的完整 SBPL 字符串，就绪交给 sandbox-exec
    pub profile: String,
    /// 参数映射：参数名 -> 实际路径（T2 拼 -D PARAM=path 传给 sandbox-exec）
    pub params: Vec<(String, String)>,
}

/// 从基础模板拼接完整沙盒策略。
///
/// 参数：
/// - `write_root_key`：写入路径参数名（将添加 `(allow file-write* (subpath (param "...")))`
///   规则，**同时**添加对应的 `(allow file-read* ...)`——应用需要能读回自己刚写的数据/配置，
///   只给写权限没有读权限会让沙盒内进程写得进去却读不出来，正常应用逻辑基本都会失败）
/// - `extra_write_keys`（P6-A）：额外的可写路径参数名集合——`filesystem` 能力清单里
///   `filesystem.write` 声明的（`$APP_DATA` 之外的）目录，每个都获得与 `write_root_key`
///   完全同规格的两条规则（`file-write*` + `file-read*`），紧跟在 `write_root_key`
///   两条规则之后、`read_root_keys` 之前——多写根之间彼此独立，互不影响。
/// - `read_root_keys`：只读路径参数名集合（每个添加 `(allow file-read* (subpath (param "...")))` 规则）
/// - `runtime_root_keys`：运行时安装目录参数名集合（每个**同时**添加
///   `(allow file-read* (subpath (param "...")))` 与 `(allow process-exec* (subpath (param "...")))`
///   两条规则）——区别于 `read_root_keys`：真实 pi/node 自身安装路径（如 nvm 的
///   `versions/node/vX/`）不仅要能被读（node 从这棵树下分页读自身依赖/ICU 数据），
///   还要能被 **exec**（`execvp` 真实 pi 二进制/node 解释器本身）。`read_root_keys`
///   只给读，不给 exec，语义上不该用来放行运行时目录。
/// - `deny_network`：若为 true，追加 `(deny network*)` 拒绝网络访问；若为 false（可信应用），
///   追加 `(allow network*)` 放行全部出站网络——P2 阶段没有域名级过滤能力，"可信"在此等价于
///   "放行全部网络"，域名粒度的 egress 过滤留待后续任务（design spec §14 非目标）
/// - `mcp_socket_key`（Task 9c）：若为 `Some(key)`，追加一条**窄**放行——只允许连接
///   `(param key)` 指向的**这一个** unix socket 路径，供 `deny_network=true` 的受限
///   第三方 app 的 pi 子进程连接 Task9/9b 起的宿主 MCP 桥监听器（否则 `(deny
///   network*)` 会把这条桥也一并挡死，MCP 对受限 app 完全不可达）。为 `None` 时
///   不追加任何东西——行为与本任务之前完全一致（P2 既有沙盒逃逸套件必须继续
///   字节级不变）。
///
/// 返回：拼接后的完整 SBPL 字符串（含 BASE_PROFILE + 参数化规则）。
///
/// **注意**：参数值不插入 SBPL（防注入攻击），改由调用方（T2 build_profile）
/// 用 `-D PARAM=path` 传给 sandbox-exec。
pub fn render_profile(
    write_root_key: &str,
    extra_write_keys: &[String],
    read_root_keys: &[String],
    runtime_root_keys: &[String],
    deny_network: bool,
    mcp_socket_key: Option<&str>,
) -> String {
    let mut profile = BASE_PROFILE.to_string();

    // 追加 write 规则 + 对应的 read 规则（同一 subpath：应用要能读自己写下的数据）
    profile.push('\n');
    profile.push_str(&format!(
        r#"(allow file-write* (subpath (param "{}")))"#,
        write_root_key
    ));
    profile.push('\n');
    profile.push_str(&format!(
        r#"(allow file-read* (subpath (param "{}")))"#,
        write_root_key
    ));

    // 追加每个额外可写根的规则（P6-A：filesystem 能力声明的 write 路径），
    // 与 write_root_key 同规格：写 + 读各一条。
    for k in extra_write_keys {
        profile.push('\n');
        profile.push_str(&format!(r#"(allow file-write* (subpath (param "{}")))"#, k));
        profile.push('\n');
        profile.push_str(&format!(r#"(allow file-read* (subpath (param "{}")))"#, k));
    }

    // 追加每个 read 规则
    for read_key in read_root_keys {
        profile.push('\n');
        profile.push_str(&format!(
            r#"(allow file-read* (subpath (param "{}")))"#,
            read_key
        ));
    }

    // 追加每个运行时安装目录规则：读 + exec 都要放行（见上文文档）。
    for rt_key in runtime_root_keys {
        profile.push('\n');
        profile.push_str(&format!(
            r#"(allow file-read* (subpath (param "{}")))"#,
            rt_key
        ));
        profile.push('\n');
        profile.push_str(&format!(
            r#"(allow process-exec* (subpath (param "{}")))"#,
            rt_key
        ));
    }

    // 网络：禁止则显式 deny（deny default 已隐式覆盖，这里显式更清晰）；
    // 放行（可信应用）则显式 allow——BASE_PROFILE 的 `(deny default)` 不会自动放行网络，
    // 不加这条 deny_network=false 时网络其实仍被拒绝（T4 手工核验发现的空操作 gap）。
    if deny_network {
        profile.push('\n');
        profile.push_str("(deny network*)");
    } else {
        profile.push('\n');
        profile.push_str("(allow network*)");
    }

    // MCP socket 窄放行（Task 9c）：刻意放在网络 deny/allow 块**之后**——手工用
    // 真实 sandbox-exec 核验过（见 task-9c-report.md），SBPL 对同一操作类别
    // （这里是 `network-outbound`）的多条 allow/deny 语句按"更靠后的规则生效"
    // 解析；放在 `(deny network*)` 之前会被它盖掉，导致这条窄放行形同虚设。
    // 只放行 `network-outbound`（不放宽到 `network*`，那会连 `network-inbound`/
    // `network-bind` 一起打开，超出"能连自己的 MCP socket"这一件事）；只按
    // `literal` 匹配单一路径（不用 `subpath`，避免放行到该 socket 所在目录下的
    // 其它任意路径/其它 app 的 socket）。
    if let Some(key) = mcp_socket_key {
        profile.push('\n');
        profile.push_str(&format!(
            r#"(allow network-outbound (literal (param "{}")))"#,
            key
        ));
    }

    profile
}

/// 计算 `path` 的"严格祖先链"：从最近的父目录开始逐级向上，直到但不包含 `/` 本身
/// （根目录的 vnode 已由 `BASE_PROFILE` 的 `(allow file-read* (literal "/"))` 覆盖，
/// 不需要再重复放行）。要求 `path` 已是绝对路径——调用方（`build_profile`）传入的
/// 都是 canonicalize（或危险字符校验过的绝对回退路径）后的路径。
///
/// 用途：node 的 `fs.realpathSync` 对一条路径做祖先解析时，会对**每一级祖先目录**
/// 逐个 `lstat`——这里算出的正是"实际会被 lstat 到"的目录集合，供 `build_profile`
/// 为它们各生成一条 `(allow file-read-metadata (literal ...))`，取代过去全局放行的
/// `(allow file-read-metadata)`（见 `BASE_PROFILE` 尾部注释里记录的收窄理由）。
///
/// 例：`/Users/alice/Library/x/data` → `[/Users/alice/Library/x, /Users/alice/Library,
/// /Users/alice, /Users]`；`/a` → `[]`（唯一祖先是 `/`，已被全局规则覆盖，不算严格祖先）。
fn strict_ancestors(path: &std::path::Path) -> Vec<std::path::PathBuf> {
    path.ancestors()
        .skip(1) // 跳过 path 自身——只要祖先，不要它本身
        .filter(|p| p.parent().is_some()) // 排除 "/"：它没有 parent
        .map(|p| p.to_path_buf())
        .collect()
}

/// 构建沙盒配置：规范化路径并检查危险字符。
///
/// 参数：
/// - `app_data`：应用数据目录，唯一**默认**可写的路径。必须成功 canonicalize。
/// - `read_paths`：只读路径集合。若 canonicalize 失败，回退到绝对路径。
/// - `write_paths`（P6-A）：额外可写路径集合——`filesystem` 能力清单里
///   `filesystem.write` 声明的（`$APP_DATA` 之外的）目录，处理规则与 `read_paths`
///   完全一致（canonicalize 失败回退绝对路径、危险字符检查、键前缀改为 `WRITE_`），
///   同时渲染到 `render_profile` 的 `extra_write_keys`（写 + 读两条规则，见该函数文档）。
/// - `runtime_paths`：运行时安装目录集合（pi/node 自身安装前缀，见
///   `pi_bin::runtime_install_dirs`）——同时放行读 + exec，处理规则与 `read_paths`
///   完全一致（canonicalize 失败回退绝对路径、危险字符检查），只是渲染到
///   `render_profile` 的 `runtime_root_keys`（多一条 `process-exec*`）而非
///   `read_root_keys`。
/// - `deny_network`：是否拒绝网络访问。
///
/// 返回：
/// - `Ok(SandboxProfile)`：包含参数映射和渲染后的 SBPL 配置。
/// - `Err(String)`：如果任何路径含有 `"`、`\`、NUL 字符。
///
/// **安全检查**：
/// - 拒绝包含可能注入 SBPL/`-D` 语法的字符（`"`、`\`、NUL）。
/// - `app_data` 必须成功 canonicalize（防符号链接逃逸）。
/// - `read_paths`/`runtime_paths` 若 canonicalize 失败，使用绝对路径。
///
/// **祖先目录元数据放行**（收窄取代 `BASE_PROFILE` 曾经的全局
/// `(allow file-read-metadata)`，见该常量尾部注释）：对 `app_data`、每个
/// `read_paths`、每个 `runtime_paths`（均取已规范化后的路径）分别算出
/// `strict_ancestors`，去重（多个路径常共享 `/Users`、`/Users/<user>` 等前缀
/// 祖先）后，每个唯一祖先目录各生成一个 `ANC_i` 参数与一条
/// `(allow file-read-metadata (literal (param "ANC_i")))`。
///
/// - `mcp_socket_path`（Task 9c）：`Some(path)` 时，该 app 的 pi 子进程需要能
///   连接**自己的** MCP unix socket（Task9/9b 起的宿主监听器）——调用方
///   （`session_mgr::sandboxed_argv`）只应在确认 `CapabilityRegistry::launch`
///   算出的贡献真的 `needs_socket`（且监听器 bind 成功，见
///   `session_mgr::run_headless_session`/`open_app_after_acquire`）时才传
///   `Some`，否则传 `None`（没有依赖 socket 的贡献时不应该平白多一条网络放行）。
///   `path` 必须真实存在并可 `canonicalize`——不像 `read_paths`/`runtime_paths`
///   那样在 canonicalize 失败时回退到绝对路径原样：unix socket 的
///   `network-outbound` 窄放行走的是内核对 `literal` 的路径级精确匹配（详见
///   `render_profile` 文档），`-D` 参数必须与该路径**规范化后的真实形式**一致
///   才能命中；若调用方传入一个还不存在/不可规范化的路径，多半意味着上游的
///   "先 bind 监听器、再构建沙盒 profile" 顺序假设被打破了，此时直接报错比
///   静默生成一条永远不会命中的放行规则更安全。
///   同样做危险字符检查（引号/反斜线/NUL），生成固定键名 `"MCP_SOCK"` 的
///   `-D` 参数。**不**纳入下面的 `strict_ancestors`/`ANC_i` 祖先元数据放行——
///   已用真实 `sandbox-exec` 手工核验过（`task-9c-report.md`），`connect()`
///   到一个 unix socket 路径不需要祖先目录的 `file-read-metadata` 放行（不同于
///   `fs.realpathSync` 对 write/read/runtime 路径的祖先 lstat 需求），额外放行
///   只会不必要地扩大可见面。
pub fn build_profile(
    app_data: &std::path::Path,
    read_paths: &[std::path::PathBuf],
    write_paths: &[std::path::PathBuf],
    runtime_paths: &[std::path::PathBuf],
    deny_network: bool,
    mcp_socket_path: Option<&std::path::Path>,
) -> Result<SandboxProfile, String> {
    // 规范化 app_data（唯一可写路径，必须成功）
    let write_path = std::fs::canonicalize(app_data)
        .map_err(|e| format!("Failed to canonicalize app_data: {}", e))?;
    let write_path_str = write_path.to_string_lossy().to_string();

    // 检查危险字符
    if write_path_str.contains('"')
        || write_path_str.contains('\\')
        || write_path_str.contains('\0')
    {
        return Err(
            "Write path contains dangerous characters (quote, backslash, or NUL)".to_string(),
        );
    }

    // 规范化或转换一条路径：canonicalize 成功用规范化结果，失败回退到绝对路径
    // （相对路径拼当前目录）；`read_paths`/`runtime_paths` 共用同一套规则。
    fn canonicalize_or_fallback(
        path: &std::path::Path,
        label: &str,
        i: usize,
    ) -> Result<String, String> {
        let path_str = match std::fs::canonicalize(path) {
            Ok(canonical) => canonical.to_string_lossy().to_string(),
            Err(_) => {
                if path.is_absolute() {
                    path.to_string_lossy().to_string()
                } else {
                    std::env::current_dir()
                        .map_err(|e| format!("Failed to get current dir: {}", e))?
                        .join(path)
                        .to_string_lossy()
                        .to_string()
                }
            }
        };
        if path_str.contains('"') || path_str.contains('\\') || path_str.contains('\0') {
            return Err(format!(
                "{} path {} contains dangerous characters",
                label, i
            ));
        }
        Ok(path_str)
    }

    // 构建参数映射
    let mut params = vec![("WRITE".to_string(), write_path_str)];
    let mut read_keys = Vec::new();
    let mut extra_write_keys = Vec::new();
    let mut runtime_keys = Vec::new();
    // 与 params 平行收集规范化后的路径（PathBuf 形式），供下面算祖先链——
    // 只是同一批已校验路径的另一种表示，不引入新的路径来源。
    let mut all_canonical_paths = vec![write_path.clone()];

    for (i, rpath) in read_paths.iter().enumerate() {
        let path_str = canonicalize_or_fallback(rpath, "Read", i)?;
        all_canonical_paths.push(std::path::PathBuf::from(&path_str));
        let key = format!("READ_{}", i);
        read_keys.push(key.clone());
        params.push((key, path_str));
    }

    for (i, wpath) in write_paths.iter().enumerate() {
        let path_str = canonicalize_or_fallback(wpath, "Write", i)?;
        all_canonical_paths.push(std::path::PathBuf::from(&path_str));
        let key = format!("WRITE_{}", i);
        extra_write_keys.push(key.clone());
        params.push((key, path_str));
    }

    for (i, rpath) in runtime_paths.iter().enumerate() {
        let path_str = canonicalize_or_fallback(rpath, "Runtime", i)?;
        all_canonical_paths.push(std::path::PathBuf::from(&path_str));
        let key = format!("RT_{}", i);
        runtime_keys.push(key.clone());
        params.push((key, path_str));
    }

    // MCP socket 窄放行（Task 9c）：见上方文档注释。必须在调用 render_profile 之前
    // 算出 key（render_profile 需要在网络 deny/allow 块之后追加对应的
    // network-outbound literal 行），并同步把参数塞进 params——与 write_path 的
    // 危险字符检查同规格。
    let mcp_socket_key: Option<String> = match mcp_socket_path {
        Some(p) => {
            let canonical = std::fs::canonicalize(p)
                .map_err(|e| format!("Failed to canonicalize mcp_socket_path: {}", e))?;
            let s = canonical.to_string_lossy().to_string();
            if s.contains('"') || s.contains('\\') || s.contains('\0') {
                return Err(
                    "MCP socket path contains dangerous characters (quote, backslash, or NUL)"
                        .to_string(),
                );
            }
            params.push(("MCP_SOCK".to_string(), s));
            Some("MCP_SOCK".to_string())
        }
        None => None,
    };

    // 生成配置
    let mut profile = render_profile(
        "WRITE",
        &extra_write_keys,
        &read_keys,
        &runtime_keys,
        deny_network,
        mcp_socket_key.as_deref(),
    );

    // 祖先目录元数据放行：去重（BTreeSet）后对每个唯一祖先目录生成一个 ANC_i
    // 参数 + 一条 literal file-read-metadata 规则（取代 BASE_PROFILE 曾经的全局
    // 放行，理由见该常量尾部注释）。
    let mut ancestor_set: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for p in &all_canonical_paths {
        for anc in strict_ancestors(p) {
            ancestor_set.insert(anc.to_string_lossy().to_string());
        }
    }

    for (i, anc_str) in ancestor_set.into_iter().enumerate() {
        // 与 write/read/runtime 路径同规则的危险字符检查：这些祖先字符串是已校验
        // 路径的前缀切片，理论上已经干净，这里仍显式复查一遍，防御未来
        // strict_ancestors/canonicalize_or_fallback 实现变化引入的回归。
        if anc_str.contains('"') || anc_str.contains('\\') || anc_str.contains('\0') {
            return Err(format!("Ancestor path {} contains dangerous characters", i));
        }
        let key = format!("ANC_{}", i);
        profile.push('\n');
        profile.push_str(&format!(
            r#"(allow file-read-metadata (literal (param "{}")))"#,
            key
        ));
        params.push((key, anc_str));
    }

    Ok(SandboxProfile { profile, params })
}

/// 生成 sandbox-exec 命令行参数。
///
/// 参数：
/// - `profile`：沙盒配置（包含 SBPL 和参数映射）。
/// - `inner`：要在沙盒内执行的命令及参数。
///
/// 返回：命令行参数列表，格式为：
/// `["-p", <profile>, "-DKEY=value", ..., "--", <inner[0]>, <inner[1]>, ...]`
pub fn sandbox_exec_argv(profile: &SandboxProfile, inner: &[String]) -> Vec<String> {
    let mut argv = vec!["-p".to_string(), profile.profile.clone()];

    // 追加每个参数作为 -Dkey=value
    for (key, value) in &profile.params {
        argv.push(format!("-D{}={}", key, value));
    }

    // 分隔符
    argv.push("--".to_string());

    // 追加内部命令
    argv.extend(inner.iter().cloned());

    argv
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 检验 render_profile 包含必要的 SBPL 语法、参数引用、network 拒绝
    #[test]
    fn render_has_deny_default_and_param_refs() {
        let p = render_profile(
            "WRITE",
            &[],
            &["READ_0".into(), "READ_1".into()],
            &[],
            true,
            None,
        );
        assert!(p.contains("(version 1)"));
        assert!(p.contains("(deny default)"));
        assert!(p.contains(r#"(allow file-write* (subpath (param "WRITE")))"#));
        assert!(p.contains(r#"(subpath (param "READ_0"))"#));
        assert!(p.contains(r#"(subpath (param "READ_1"))"#));
        assert!(p.contains("(deny network*)"));
        // base 里 node 启动必须的 root 只读
        assert!(p.contains("process-exec"));
        assert!(p.contains(r#"(subpath (param "WRITE"))"#));
    }

    /// 检验当 deny_network=false 时，不包含 "(deny network*)"，且显式包含
    /// "(allow network*)"（可信应用放行网络——之前是空操作，T4 手工核验发现的 gap，本任务修复）。
    #[test]
    fn render_no_deny_network_when_allowed() {
        let p = render_profile("WRITE", &[], &[], &[], false, None);
        assert!(!p.contains("(deny network*)"));
        assert!(p.contains("(allow network*)"), "可信应用应显式放行网络");
    }

    /// 检验 write_root 同时可读：app 需要能读回自己写入 $APP_DATA 的数据/配置
    /// （之前只加了 file-write*，没加 file-read*，T4 review 发现的 gap，本任务修复）。
    #[test]
    fn render_write_root_also_readable() {
        let p = render_profile("WRITE", &[], &[], &[], true, None);
        assert!(
            p.contains(r#"(allow file-read* (subpath (param "WRITE")))"#),
            "app 需能读自己的 $APP_DATA"
        );
    }

    /// 检验 runtime_root_keys 同时获得 file-read* 与 process-exec*（区别于 read_root_keys
    /// 只给 file-read*）——真实 pi/node 安装路径既要能读也要能被 execvp。
    #[test]
    fn render_runtime_root_gets_read_and_exec() {
        let p = render_profile("WRITE", &[], &[], &["RT_0".into()], true, None);
        assert!(
            p.contains(r#"(allow file-read* (subpath (param "RT_0")))"#),
            "运行时目录需可读"
        );
        assert!(
            p.contains(r#"(allow process-exec* (subpath (param "RT_0")))"#),
            "运行时目录需可 exec"
        );
    }

    /// 检验 build_profile 规范化路径并生成参数映射
    #[test]
    fn build_profile_canonicalizes_and_params() {
        let d = tempfile::tempdir().unwrap();
        let sp = build_profile(d.path(), &[], &[], &[], true, None).unwrap();
        assert!(sp
            .params
            .iter()
            .any(|(k, v)| k == "WRITE" && std::path::Path::new(v).exists()));
        assert!(sp.profile.contains("(deny network*)"));
    }

    /// 检验 build_profile 拒绝含有危险字符（引号、反斜线、NUL）的路径
    #[test]
    fn build_profile_rejects_dangerous_path() {
        let d = tempfile::tempdir().unwrap();
        let bad = std::path::PathBuf::from("/tmp/a\"b");
        assert!(build_profile(d.path(), &[bad], &[], &[], true, None).is_err());
    }

    /// 检验 build_profile 对 runtime_paths 做同样的 canonicalize + 参数映射（键前缀
    /// `RT_`），且渲染出的 SBPL 含对应的 read+exec 规则。
    #[test]
    fn build_profile_handles_runtime_paths() {
        let d = tempfile::tempdir().unwrap();
        let rt = tempfile::tempdir().unwrap();
        let sp = build_profile(d.path(), &[], &[], &[rt.path().to_path_buf()], true, None).unwrap();
        assert!(sp
            .params
            .iter()
            .any(|(k, v)| k == "RT_0" && std::path::Path::new(v).exists()));
        assert!(sp
            .profile
            .contains(r#"(allow process-exec* (subpath (param "RT_0")))"#));
    }

    /// 检验 build_profile 对 runtime_paths 同样做危险字符检查。
    #[test]
    fn build_profile_rejects_dangerous_runtime_path() {
        let d = tempfile::tempdir().unwrap();
        let bad = std::path::PathBuf::from("/tmp/rt\"evil");
        assert!(build_profile(d.path(), &[], &[], &[bad], true, None).is_err());
    }

    // --- strict_ancestors ----------------------------------------------------

    /// 多级路径：从最近父目录到 "/" 前一级（严格祖先，不含自身、不含 "/"）。
    #[test]
    fn strict_ancestors_multi_level() {
        let anc = strict_ancestors(std::path::Path::new("/Users/alice/Library/x/data"));
        assert_eq!(
            anc,
            vec![
                std::path::PathBuf::from("/Users/alice/Library/x"),
                std::path::PathBuf::from("/Users/alice/Library"),
                std::path::PathBuf::from("/Users/alice"),
                std::path::PathBuf::from("/Users"),
            ]
        );
    }

    /// 只比根深一级的路径：唯一祖先是 "/"，已被全局规则覆盖，严格祖先为空。
    #[test]
    fn strict_ancestors_single_level_is_empty() {
        assert_eq!(
            strict_ancestors(std::path::Path::new("/a")),
            Vec::<std::path::PathBuf>::new()
        );
    }

    // --- 祖先元数据放行收窄（取代全局 file-read-metadata） --------------------

    /// BASE_PROFILE 不应再含全局 `(allow file-read-metadata)`——本次收窄的核心断言。
    #[test]
    fn base_profile_has_no_global_metadata_allow() {
        assert!(!BASE_PROFILE.contains("(allow file-read-metadata)"));
    }

    /// build_profile 应按祖先目录字面量生成 file-read-metadata 放行，不再有全局放行。
    #[test]
    fn build_profile_emits_ancestor_metadata_literals_not_global_allow() {
        let d = tempfile::tempdir().unwrap();
        let sp = build_profile(d.path(), &[], &[], &[], true, None).unwrap();
        assert!(
            !sp.profile.contains("(allow file-read-metadata)"),
            "不应再有全局放行"
        );
        assert!(
            sp.params.iter().any(|(k, _)| k.starts_with("ANC_")),
            "应至少放行一条祖先目录字面量元数据参数"
        );
        assert!(
            sp.profile
                .contains(r#"(allow file-read-metadata (literal (param "ANC_0")))"#),
            "应含逐条 literal 形式的祖先元数据放行"
        );
    }

    /// 多个来源共享祖先目录时应去重：app_data 与 read_path 同在一个共同父目录下，
    /// 该共享祖先只应产生一个 ANC_ 参数，不重复。
    #[test]
    fn build_profile_dedupes_shared_ancestors() {
        let parent = tempfile::tempdir().unwrap();
        let app_data = parent.path().join("app");
        let read_dir = parent.path().join("read");
        std::fs::create_dir_all(&app_data).unwrap();
        std::fs::create_dir_all(&read_dir).unwrap();
        let sp = build_profile(
            &app_data,
            std::slice::from_ref(&read_dir),
            &[],
            &[],
            true,
            None,
        )
        .unwrap();
        let canon_parent = std::fs::canonicalize(parent.path())
            .unwrap()
            .to_string_lossy()
            .to_string();
        let count = sp
            .params
            .iter()
            .filter(|(k, v)| k.starts_with("ANC_") && *v == canon_parent)
            .count();
        assert_eq!(count, 1, "共享祖先目录应去重，实际 params: {:?}", sp.params);
    }

    /// 检验 sandbox_exec_argv 生成正确的命令行格式
    #[test]
    fn argv_shape() {
        let sp = SandboxProfile {
            profile: "P".into(),
            params: vec![("WRITE".into(), "/w".into())],
        };
        let a = sandbox_exec_argv(&sp, &["pi".into(), "--mode".into(), "rpc".into()]);
        assert_eq!(a[0], "-p");
        assert_eq!(a[1], "P");
        assert!(a.iter().any(|s| s == "-DWRITE=/w"));
        let sep = a.iter().position(|s| s == "--").unwrap();
        assert_eq!(&a[sep + 1..], &["pi", "--mode", "rpc"]);
    }

    // --- MCP socket 窄放行（Task 9c） ------------------------------------------

    /// render_profile 传 mcp_socket_key=Some(..) 时应追加按 literal 匹配该 key 的
    /// network-outbound 窄放行；且该行必须出现在 `(deny network*)` 之后（详见该
    /// 函数文档注释里记录的"更靠后的规则生效"手工核验结论）。
    #[test]
    fn render_mcp_socket_key_appends_narrow_network_outbound_allow_after_deny() {
        let p = render_profile("WRITE", &[], &[], &[], true, Some("MCP_SOCK"));
        assert!(p.contains(r#"(allow network-outbound (literal (param "MCP_SOCK")))"#));
        let deny_pos = p.find("(deny network*)").unwrap();
        let allow_pos = p
            .find(r#"(allow network-outbound (literal (param "MCP_SOCK")))"#)
            .unwrap();
        assert!(
            allow_pos > deny_pos,
            "MCP socket 窄放行必须在 deny network* 之后，否则会被盖掉"
        );
    }

    /// render_profile 传 mcp_socket_key=None 时不应含任何 network-outbound 规则——
    /// 无 MCP 场景下渲染结果必须与本任务之前完全一致。
    #[test]
    fn render_no_mcp_socket_key_omits_network_outbound() {
        let p = render_profile("WRITE", &[], &[], &[], true, None);
        assert!(!p.contains("network-outbound"));
    }

    /// build_profile 传 mcp_socket_path=Some(存在的路径) 时：应生成一个规范化后的
    /// "MCP_SOCK" 参数，且渲染出的 profile 含对应的 literal 窄放行行。
    #[test]
    fn build_profile_with_mcp_socket_path_adds_param_and_allow() {
        let d = tempfile::tempdir().unwrap();
        let sock_dir = tempfile::tempdir().unwrap();
        let sock_path = sock_dir.path().join("mcp.sock");
        // network-outbound 的 literal 匹配需要一个真实存在、可 canonicalize 的路径
        // （见 build_profile 文档）；用真实 unix socket bind 出来，而不是普通文件，
        // 更贴近真实调用场景（Task9b McpSocketListener 已经 bind 过）。
        let _listener = std::os::unix::net::UnixListener::bind(&sock_path).unwrap();
        let sp = build_profile(d.path(), &[], &[], &[], true, Some(&sock_path)).unwrap();
        let canon = std::fs::canonicalize(&sock_path)
            .unwrap()
            .to_string_lossy()
            .to_string();
        assert!(sp
            .params
            .iter()
            .any(|(k, v)| k == "MCP_SOCK" && *v == canon));
        assert!(sp
            .profile
            .contains(r#"(allow network-outbound (literal (param "MCP_SOCK")))"#));
    }

    /// build_profile 传 mcp_socket_path=None 时：不应产生任何 "MCP_SOCK" 参数或
    /// network-outbound 规则——这条钉住"无 MCP 时字节级不变"这一不变量（越权测试
    /// 套件的既有 7 条用例正是靠它才继续成立）。
    #[test]
    fn build_profile_no_mcp_socket_is_unchanged_from_pre_task9c_shape() {
        let d = tempfile::tempdir().unwrap();
        let sp = build_profile(d.path(), &[], &[], &[], true, None).unwrap();
        assert!(!sp.params.iter().any(|(k, _)| k == "MCP_SOCK"));
        assert!(!sp.profile.contains("network-outbound"));
    }

    /// build_profile 对 mcp_socket_path 做与 write/read/runtime 路径相同规格的危险
    /// 字符检查：路径必须真实存在（这里同样用一个真实 bind 出来的 unix socket），
    /// 但其规范化后的字符串形式含引号时仍应被拒。
    #[test]
    fn build_profile_rejects_dangerous_mcp_socket_path() {
        let d = tempfile::tempdir().unwrap();
        let sock_dir = tempfile::tempdir().unwrap();
        let bad_path = sock_dir.path().join("mcp\"evil.sock");
        let _listener = std::os::unix::net::UnixListener::bind(&bad_path).unwrap();
        assert!(build_profile(d.path(), &[], &[], &[], true, Some(&bad_path)).is_err());
    }

    /// build_profile 对一个不存在的 mcp_socket_path 应直接报错（Err），而不是静默
    /// 回退到绝对路径原样——见该参数文档注释："先 bind 监听器、再构建沙盒
    /// profile" 的顺序假设一旦被打破就应该立刻在这里暴露出来。
    #[test]
    fn build_profile_rejects_nonexistent_mcp_socket_path() {
        let d = tempfile::tempdir().unwrap();
        let missing = std::path::PathBuf::from("/tmp/does-not-exist-mcp-sock-9c/mcp.sock");
        assert!(build_profile(d.path(), &[], &[], &[], true, Some(&missing)).is_err());
    }
}
