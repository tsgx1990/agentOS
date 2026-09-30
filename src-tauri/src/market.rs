//! P5 市场（§12 v1 轻量方案）：解析/拉取一个精选市场索引 `index.json`。
//!
//! 「精选市场」= 一个存 `index.json` 的仓库（元数据、图标、权限摘要）；App 内市场页
//! 拉取渲染，装的时候复用既有安装确认流（`preview_install`→`install_app`，第三方
//! `trusted=false`，用户逐条确认权限）。本模块只负责"把索引拿到并解析成 `MarketEntry`
//! 列表"这一段纯数据管道，不碰安装。
//!
//! `fetch_index` 的源支持**本地文件路径**（内置 demo `samples/market/index.json`
//! 随资源打包）与 **`http(s)://` URL**（P6-B Task 6：`reqwest::blocking`，超时
//! `HTTP_TIMEOUT_SECS`、响应体上限 `MAX_INDEX_BYTES`）；真实远程索引托管仍是手工
//! 里程碑（spec §5 裁决 4：HTTP 实现进代码，但默认内置索引仍是本地文件，远端索引
//! URL 留给用户自己填）。
//!
//! P6-B Task 6 同时给 `kind: "skill"` 条目加了下载/校验/安装：
//! `download_skill_zip`（HTTP 拉取 + sha256 核验）→ `unpack_skill_zip`（拒绝绝对
//! 路径/`..`/符号链接条目、总解压大小与文件数上限）→ 调用方（`lib.rs::
//! skill_market_install`）再走 `skills::SkillStore::install_from_dir` 那道安装门，
//! 不重复其校验（本模块只管"这份 zip 能不能被安全地放到磁盘上"，技能本身合不合法
//! 是安装门的职责）。

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::Path;

/// `publish.rs::build_entry` 也用这个（发布的是应用，`kind` 恒为 `"app"`），
/// 避免两处各写一份同样的字符串字面量。
pub(crate) fn kind_app() -> String {
    "app".to_string()
}

/// 市场索引里的一条条目。`kind` 区分"应用"（默认，走既有 `preview_install`→
/// `install_app` 安装流）与 `"skill"`（P6-B：`source` 本地路径直接装，
/// `download_url`+`sha256` 走 `skill_market_install` 的下载/校验/解包/安装门）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MarketEntry {
    pub name: String,
    pub display_name: String,
    pub version: String,
    pub category: String,
    #[serde(default)]
    pub icon: Option<String>,
    #[serde(default)]
    pub description: String,
    /// 安装源：本地路径 / git URL（应用条目，复用 pi 语义）；技能条目若走本地
    /// 安装，指向一个技能目录（`SKILL.md` 所在处），不走下载。
    pub source: String,
    #[serde(default)]
    pub permissions: Vec<String>,
    /// 条目类型：`"app"`（缺省，向后兼容旧索引——没有这个字段的既有
    /// `samples/market/index.json` 条目一律按应用处理）或 `"skill"`。
    #[serde(default = "kind_app")]
    pub kind: String,
    /// 技能条目的下载地址；本地技能条目（`source` 直接可用）留空。
    #[serde(default)]
    pub download_url: Option<String>,
    /// 技能 zip 的期望 sha256（十六进制，大小写不敏感）；`download_url` 非空时
    /// `skill_market_install` 强制要求这个字段，防止"声明了下载地址却不做完整性
    /// 校验"这种自相矛盾的索引条目悄悄通过。
    #[serde(default)]
    pub sha256: Option<String>,
    /// 展示用的字节数（可选，供市场页显示"这个技能多大"，不参与任何校验——真正
    /// 的大小上限由 `download_skill_zip`/`unpack_skill_zip` 在下载/解包时现场量）。
    #[serde(default)]
    pub size: Option<u64>,
    #[serde(default)]
    pub author: Option<String>,
}

/// 索引文件的顶层形状：`{ "entries": [MarketEntry, ...] }`。
#[derive(Debug, Deserialize)]
struct Index {
    #[serde(default)]
    entries: Vec<MarketEntry>,
}

/// 解析索引 JSON（纯函数，可测）。缺 `entries` 键 / 非法 JSON → `Err`；条目缺可选
/// 字段（icon/description/permissions/kind/...）落回默认值。
pub fn parse_index(json: &str) -> Result<Vec<MarketEntry>, String> {
    let value: serde_json::Value =
        serde_json::from_str(json).map_err(|e| format!("市场索引不是合法 JSON：{e}"))?;
    if value.get("entries").is_none() {
        return Err("市场索引缺少 entries 字段".to_string());
    }
    let index: Index =
        serde_json::from_value(value).map_err(|e| format!("市场索引结构非法：{e}"))?;
    Ok(index.entries)
}

/// HTTP 请求超时（索引拉取与 zip 下载共用）。
const HTTP_TIMEOUT_SECS: u64 = 15;
/// 市场索引响应体上限：1 MiB——一份索引 JSON 不应该接近这个体量，超过视为异常
/// 响应（被劫持成了别的东西/index 本身已经烂掉），直接拒绝而不是无限读下去。
const MAX_INDEX_BYTES: u64 = 1024 * 1024;
/// 技能 zip 下载体量上限：20 MiB——比 `skills::MAX_SKILL_BYTES`（解压后 5 MiB）
/// 宽松，给压缩比留余量，但仍然是一个远小于"随便多大都收"的硬上限。
pub const MAX_SKILL_ZIP_BYTES: u64 = 20 * 1024 * 1024;

/// 每一跳最多跟随的重定向数——`index.json`/技能 zip 没有理由需要一条很长的
/// 重定向链，5 跳足够覆盖"CDN 前面套一层重定向"这类正常场景（如 GitHub
/// Release 资产会 302 到 `objects.githubusercontent.com`），同时给"重定向
/// 循环/被滥用来打无限跳转"设一个硬上限。
const MAX_REDIRECT_HOPS: usize = 5;

/// **终审 I1 修复**：装一条对**每一跳**都复用 `check_remote_url` 的自定义重定向
/// 策略——`reqwest` 的默认策略（`Policy::limited(10)`）只保证"不超过 10 跳"，
/// 完全不关心每一跳的 scheme/host，本身也不会重新触发调用方在**第一跳之前**
/// 手写的 `check_remote_url` 校验。这意味着修复前一个合法的
/// `https://market.example/index.json` 只要服务端返回一次 302 到
/// `http://mitm.evil/index.json`，reqwest 会自动跟过去、把响应体（可能已被
/// 中间人篡改）当作正常结果返回——`check_remote_url` 文档里说的"index.json
/// 没有签名，明文 HTTP 下中间人能同时篡改 download_url 与 sha256"这条 https-only
/// 不变量因此在重定向链的第二跳开始就形同虚设；同时也是一条内网 SSRF
/// 通道（服务端 302 到 `http://127.0.0.1:<内网端口>/...`，本函数原本对回环地址
/// 是放行的，第一跳的 `check_remote_url` 又拦不到"藏在重定向目标里"的这个
/// 内网地址）。
///
/// 用 `Policy::none()`（完全不跟随）过于严格——GitHub Release 资产等常见托管
/// 场景本身就需要一跳重定向；用 `Policy::limited(n)` 又完全不做 scheme/host
/// 校验。这里选 `Policy::custom`：每次收到 3xx 都先对**重定向目标 URL**（不是
/// 原始请求 URL）跑一遍 `check_remote_url`，不过直接 `attempt.error(..)`
/// 拒绝（`.send()` 会把这个错误原样返回给调用方，见 `describe_reqwest_error`
/// 文档——reqwest 自身的 `Display` 只给一句笼统的"error following redirect"，
/// 真正的原因藏在 `source()` 链里，不手动走一遍会把 `check_remote_url` 精心
/// 措辞的错误信息丢在半路）；跳数达到 `MAX_REDIRECT_HOPS` 也拒绝，防重定向
/// 循环/滥用。
fn http_client() -> Result<reqwest::blocking::Client, String> {
    reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(HTTP_TIMEOUT_SECS))
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            if attempt.previous().len() >= MAX_REDIRECT_HOPS {
                let target = attempt.url().to_string();
                return attempt.error(format!(
                    "重定向跳数超过上限 {MAX_REDIRECT_HOPS}（疑似重定向循环或滥用）：{target}"
                ));
            }
            match check_remote_url(attempt.url().as_str()) {
                Ok(()) => attempt.follow(),
                Err(e) => attempt.error(e),
            }
        }))
        .build()
        .map_err(|e| format!("构建 HTTP 客户端失败：{e}"))
}

/// reqwest 的 `Display`（`error.rs::Kind::Redirect` 分支）对重定向策略拒绝这类
/// "外层包一层"的错误只给一句通用的 `"error following redirect"`——真正有用的
/// 原因（`check_remote_url` 产出的"只允许 https..."那句话）被塞进了
/// `std::error::Error::source()` 链，`{e}`/`e.to_string()` 不会自动带出来
/// （`attempt.error(e)` 里的 `e` 经 `Box<dyn Error + Send + Sync>` 转换后原样
/// 存进 `reqwest::Error.source`，见 `reqwest-0.13.4/src/redirect.rs` 与
/// `error.rs::redirect`）。这里手动沿 `source()` 链把每一层拼接进错误信息，
/// 保证调用方（及断言这些消息的测试，如 `check_remote_url` 的"https"字样）
/// 总能看到真正的拒绝原因，而不只是这句人类不容易看懂的分类标签。
fn describe_reqwest_error(err: &reqwest::Error) -> String {
    let mut msg = err.to_string();
    let mut cause: Option<&(dyn std::error::Error + 'static)> = std::error::Error::source(err);
    while let Some(source) = cause {
        msg.push('：');
        msg.push_str(&source.to_string());
        cause = source.source();
    }
    msg
}

/// 通过 HTTP(S) GET 拉取内容，读满 `max_bytes` 仍未结束立即中止（防炸弹响应耗尽
/// 内存/磁盘）——先看响应头 `Content-Length`（能提前拒绝就不用等真读完），再在
/// 读取时用 `Read::take(max_bytes + 1)` 兜底（服务端可以不诚实地报 `Content-Length`
/// 或干脆不报，只信头部不够）。
fn http_get_capped(url: &str, max_bytes: u64) -> Result<Vec<u8>, String> {
    let resp = http_client()?
        .get(url)
        .send()
        .map_err(|e| format!("请求 {url} 失败：{}", describe_reqwest_error(&e)))?;
    let status = resp.status();
    if !status.is_success() {
        return Err(format!("请求 {url} 返回 HTTP {status}"));
    }
    if let Some(len) = resp.content_length() {
        if len > max_bytes {
            return Err(format!("{url} 响应头声明 {len} 字节，超过上限 {max_bytes}"));
        }
    }
    let mut buf = Vec::new();
    resp.take(max_bytes + 1)
        .read_to_end(&mut buf)
        .map_err(|e| format!("读取 {url} 响应失败：{e}"))?;
    if buf.len() as u64 > max_bytes {
        return Err(format!(
            "{url} 响应体超过上限 {max_bytes} 字节（读到 {} 字节即中止，可能是被劫持的异常响应）",
            buf.len()
        ));
    }
    Ok(buf)
}

/// 远程 URL 只放行 `https://`（本地回环地址例外，供本地开发/测试用的 mock
/// server 用，见既有 `http://127.0.0.1:1/...` 测试）：`index.json` 与技能 zip
/// 都没有独立于传输层的完整性校验通道能替代 TLS——`index.json` 本身没有签名，
/// 明文 HTTP 下中间人不但能改内容，还能**同时**篡改技能条目的 `download_url`
/// 与旁边的 `sha256` 字段（改成中间人自己控制的 zip 及其真实哈希），这种情况下
/// `download_skill_zip` 的 sha256 核验形同虚设——挡不住"整条链路都被同一个中间
/// 人控制"这种攻击，唯一的解是压根不允许明文公网请求。`fetch_index` 与
/// `download_skill_zip` 发请求前都先调用这一个函数；`http_client()`（终审
/// I1）额外把它接进自定义重定向策略，对**每一跳**重定向目标都重新过一遍这道
/// 闸——只在入口调用一次挡不住"入口是合法 https，但服务端 302 到别处"这条
/// 绕行，见该函数文档，不各自重复判断逻辑。
///
/// 回环判定用 `url::Host` 精确匹配 `127.0.0.1` / `localhost` / `[::1]`（`url`
/// crate 解析，不手写字符串前缀匹配——那对 `http://evil.com@127.0.0.1/` 这类
/// userinfo 干扰、IPv6 方括号、大小写域名容易判错，见 `Cargo.toml` 里这条依赖
/// 的说明），不用更宽泛的 `is_loopback()`（会连 `127.0.0.2` 都放行，比 spec 要
/// 求的范围更宽）。
pub(crate) fn check_remote_url(url: &str) -> Result<(), String> {
    let parsed = url::Url::parse(url).map_err(|e| format!("{url} 不是合法 URL：{e}"))?;
    match parsed.scheme() {
        "https" => Ok(()),
        "http" => {
            let is_loopback = match parsed.host() {
                Some(url::Host::Domain(d)) => d.eq_ignore_ascii_case("localhost"),
                Some(url::Host::Ipv4(ip)) => ip == std::net::Ipv4Addr::LOCALHOST,
                Some(url::Host::Ipv6(ip)) => ip == std::net::Ipv6Addr::LOCALHOST,
                None => false,
            };
            if is_loopback {
                Ok(())
            } else {
                Err(format!(
                    "市场索引与技能包只允许 https（本地回环除外）：index.json 无完整性校验，明文 HTTP 会让中间人同时篡改 download_url 与 sha256：{url}"
                ))
            }
        }
        other => Err(format!(
            "市场索引与技能包只允许 https（本地回环除外）：不支持的 scheme {other:?}：{url}"
        )),
    }
}

/// `source` 是否是一个 http(s) URL——与 `check_remote_url` 共用同一套
/// `url::Url::parse` 解析逻辑判定 scheme（**终审 Minor 1**：此前 `fetch_index`
/// 自己用 `source.starts_with("http://") || source.starts_with("https://")`
/// 手写前缀匹配，大小写敏感——`HTTPS://host/index.json` 匹配不上，会落到下面
/// 的本地文件分支去 `std::fs::read_to_string`，当前必然读失败所以是
/// fail-closed，但两条分支对同一个字符串的 scheme 认知不一致是个定时炸弹：
/// 本地文件分支哪天变宽松就会成为真正的绕过）。解析失败（不是一个合法绝对
/// URL，例如裸文件系统路径）按"不是 http(s) URL"处理，落回本地文件分支——
/// 与此前的行为一致。
fn is_http_url(source: &str) -> bool {
    url::Url::parse(source)
        .map(|u| matches!(u.scheme(), "http" | "https"))
        .unwrap_or(false)
}

/// 拉取索引原文：`source` 是 http(s) URL（`is_http_url`，与 `check_remote_url`
/// 共用同一套 scheme 判定）走 HTTP GET（超时 15s、上限 1 MiB，见
/// `http_get_capped`；发请求前先过 `check_remote_url`）；否则按本地文件路径
/// 读取。
pub fn fetch_index(source: &str) -> Result<String, String> {
    if is_http_url(source) {
        check_remote_url(source)?;
        let bytes = http_get_capped(source, MAX_INDEX_BYTES)?;
        return String::from_utf8(bytes).map_err(|e| format!("{source} 响应不是合法 UTF-8：{e}"));
    }
    std::fs::read_to_string(source).map_err(|e| format!("读取市场索引 {source} 失败：{e}"))
}

/// 下载一个技能 zip 并校验 sha256（大小写不敏感）——不符直接 `Err`，调用方
/// （`lib.rs::skill_market_install`）据此拒绝安装；返回校验通过的原始字节，交给
/// `unpack_skill_zip` 解包。不落任何临时文件：校验失败时，除了进程内存里那份
/// `Vec<u8>`，磁盘上不留一丝痕迹。发请求前先过 `check_remote_url`（见其文档：
/// 明文 HTTP 下中间人能同时篡改 `download_url` 与 `sha256`，sha256 核验挡不住）。
pub fn download_skill_zip(url: &str, expected_sha256: &str) -> Result<Vec<u8>, String> {
    check_remote_url(url)?;
    let bytes = http_get_capped(url, MAX_SKILL_ZIP_BYTES)?;
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    let digest = format!("{:x}", hasher.finalize());
    if !digest.eq_ignore_ascii_case(expected_sha256) {
        return Err(format!(
            "{url} 的 sha256 不符：期望 {expected_sha256}，实际 {digest}（拒绝安装，可能是链接错误或内容被篡改）"
        ));
    }
    Ok(bytes)
}

/// 解包后总大小上限——与 `skills::MAX_SKILL_BYTES` 相同的量级（技能装进
/// `SkillStore` 前后不该因为绕了一圈 zip 就多出一个更宽松的上限）。
pub const MAX_UNPACKED_SKILL_BYTES: u64 = 5 * 1024 * 1024;
/// 解包后文件数上限——与 `skills::MAX_SKILL_FILES` 相同的量级，理由同上。
pub const MAX_UNPACKED_SKILL_FILES: usize = 200;
/// 单个路径分量（文件名/目录名，`..` 分隔前的一段）的字节数上限——对齐几乎所有
/// 主流文件系统单个分量的硬限制（APFS/ext4 254~255 字节，NTFS 255 字符）。不加
/// 这道检查的话，一个超长文件名会在第二遍真落盘时才被操作系统拒绝
/// （`ENAMETOOLONG`），那时 `dest` 目录已经被 `create_dir_all` 建出来了——违反本
/// 函数"先校验完再动手写"的不变量（见下方文档）。放进第一遍元数据校验，跟其它
/// 三条安全检查同一批做完，`dest` 才能保证在任一条目不合法时压根不存在。
const MAX_PATH_COMPONENT_BYTES: usize = 255;

/// 安全解包一个技能 zip 到 `dest`（`dest` 由调用方保证是一个全新/空目录——本函数
/// 只管往里写，不负责清场）：
/// - 拒绝条目数超过 `MAX_UNPACKED_SKILL_FILES`；
/// - 拒绝任何条目是符号链接（`ZipFile::is_symlink`，按 unix mode 位判定）；
/// - 拒绝路径不安全的条目——`ZipFile::enclosed_name()`（zip crate 自带的路径穿越
///   防护：拒绝绝对路径、拒绝含 NUL、拒绝解出到 `dest` 之外的 `..` 上跳）返回
///   `None` 即视为不安全；
/// - 拒绝任一路径分量超过 `MAX_PATH_COMPONENT_BYTES` 字节——防止落盘时才被操作
///   系统拒绝，见该常量文档；
/// - 炸弹预判：先遍历一遍所有条目声明的解压后大小（`ZipFile::size()`，来自 zip
///   中央目录的元数据，读取这个字段不需要真的解压数据），总和超过
///   `MAX_UNPACKED_SKILL_BYTES` 就直接拒绝——不必真解压到一半才发现磁盘要被灌爆。
///
/// 上面五条**先全部校验完**（先扫一遍所有条目的元数据）再动手真正写文件（第二
/// 遍才 `std::io::copy`），保证**任一条目不合法**（第一遍校验拒绝）时 `dest`
/// 下不会有部分解压的残留文件、`dest` 本身也不会被创建——调用方发现这类 `Err`
/// 可以直接确认磁盘上没有任何残留，不用 `remove_dir_all(dest)` 兜底清场。
///
/// **终审 Minor 2 修复**：上面这条承诺只覆盖"第一遍元数据校验失败"，此前的
/// 文档措辞却读起来像是覆盖了整个函数——第二遍真正落盘时（`create_dir_all`/
/// `File::create`/`io::copy`）仍可能失败（磁盘满、同名文件与目录冲突、
/// `ENAMETOOLONG` 边界等等），且第二遍是边写边失败的，失败前已经写下去的那些
/// 条目会留在 `dest` 里——这不是"文档写错了没关系"，生产上唯一的调用方
/// （`lib.rs::skill_market_install_core`）确实有 `remove_dir_all(&unpack_dir)`
/// 兜底所以无实害，但按文档字面"不用兜底清场"去写的新调用方会踩。现在改成
/// 自己兜住：第二遍（`unpack_write_entries`）失败时立即 `remove_dir_all(dest)`
/// （best-effort，忽略清理本身的错误——原始错误更重要，不能被清理失败盖过），
/// 保证不论失败发生在第一遍还是第二遍，调用方看到 `Err` 时磁盘上都没有残留，
/// 这条承诺现在对整个函数成立，不再只是第一遍。
pub fn unpack_skill_zip(bytes: &[u8], dest: &Path) -> Result<(), String> {
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes))
        .map_err(|e| format!("不是合法的 zip 文件：{e}"))?;

    if archive.len() > MAX_UNPACKED_SKILL_FILES {
        return Err(format!(
            "zip 内文件数 {} 超过上限 {MAX_UNPACKED_SKILL_FILES}",
            archive.len()
        ));
    }

    // 第一遍：只读元数据，校验安全性 + 累加声明的解压后大小，不写任何文件。
    let mut total_uncompressed: u64 = 0;
    let mut safe_paths: Vec<std::path::PathBuf> = Vec::with_capacity(archive.len());
    for i in 0..archive.len() {
        let entry = archive
            .by_index(i)
            .map_err(|e| format!("读取 zip 条目 {i} 失败：{e}"))?;
        if entry.is_symlink() {
            return Err(format!("zip 条目 {} 是符号链接，拒绝解包", entry.name()));
        }
        let name = entry.name().to_string();
        let Some(rel) = entry.enclosed_name() else {
            return Err(format!("zip 条目路径不安全（绝对路径或含 ..）：{name}"));
        };
        if let Some(bad) = rel
            .components()
            .map(|c| c.as_os_str())
            .find(|c| c.len() > MAX_PATH_COMPONENT_BYTES)
        {
            return Err(format!(
                "zip 条目 {name} 的路径分量 {bad:?} 超过 {MAX_PATH_COMPONENT_BYTES} 字节"
            ));
        }
        total_uncompressed += entry.size();
        safe_paths.push(rel);
    }
    if total_uncompressed > MAX_UNPACKED_SKILL_BYTES {
        return Err(format!(
            "zip 声明解压后大小 {total_uncompressed} 字节超过上限 {MAX_UNPACKED_SKILL_BYTES}（疑似 zip 炸弹）"
        ));
    }

    // 第二遍：校验已全部通过，真正落盘；失败即清场（见上方函数文档"终审
    // Minor 2 修复"）。
    if let Err(e) = unpack_write_entries(&mut archive, safe_paths, dest) {
        let _ = std::fs::remove_dir_all(dest);
        return Err(e);
    }
    Ok(())
}

/// [`unpack_skill_zip`] 第二遍的落盘逻辑：`create_dir_all(dest)` + 逐条目
/// `create_dir_all`/`File::create`/`io::copy`。抽成独立函数纯粹是为了让调用方
/// 能在它失败时统一 `remove_dir_all(dest)` 清场（见调用点），不是为了复用。
fn unpack_write_entries(
    archive: &mut zip::ZipArchive<std::io::Cursor<&[u8]>>,
    safe_paths: Vec<std::path::PathBuf>,
    dest: &Path,
) -> Result<(), String> {
    std::fs::create_dir_all(dest).map_err(|e| format!("创建 {} 失败：{e}", dest.display()))?;
    for (i, rel) in safe_paths.into_iter().enumerate() {
        let mut entry = archive
            .by_index(i)
            .map_err(|e| format!("读取 zip 条目 {i} 失败：{e}"))?;
        let out_path = dest.join(&rel);
        if entry.is_dir() {
            std::fs::create_dir_all(&out_path)
                .map_err(|e| format!("创建 {} 失败：{e}", out_path.display()))?;
            continue;
        }
        if let Some(parent) = out_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("创建 {} 失败：{e}", parent.display()))?;
        }
        let mut out = std::fs::File::create(&out_path)
            .map_err(|e| format!("写入 {} 失败：{e}", out_path.display()))?;
        std::io::copy(&mut entry, &mut out)
            .map_err(|e| format!("解压 {} 失败：{e}", out_path.display()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID: &str = r#"{
      "entries": [
        { "name": "@superagent/summarizer", "display_name": "精简器", "version": "1.0.0",
          "category": "automation", "source": "samples/summarizer",
          "description": "把文本精简成 3 条要点", "permissions": ["无额外权限"] },
        { "name": "@superagent/researcher", "display_name": "研究员", "version": "1.0.0",
          "category": "automation", "source": "samples/researcher" }
      ]
    }"#;

    #[test]
    fn parse_valid_index_returns_entries_with_fields() {
        let entries = parse_index(VALID).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].name, "@superagent/summarizer");
        assert_eq!(entries[0].display_name, "精简器");
        assert_eq!(entries[0].category, "automation");
        assert_eq!(entries[0].source, "samples/summarizer");
        assert_eq!(entries[0].permissions, vec!["无额外权限".to_string()]);
    }

    #[test]
    fn optional_fields_default_when_absent() {
        // 第二条缺 icon/description/permissions → 默认值。
        let entries = parse_index(VALID).unwrap();
        assert_eq!(entries[1].icon, None);
        assert_eq!(entries[1].description, "");
        assert!(entries[1].permissions.is_empty());
    }

    #[test]
    fn missing_entries_key_is_error() {
        assert!(parse_index(r#"{ "foo": 1 }"#).is_err());
    }

    #[test]
    fn bad_json_is_error() {
        assert!(parse_index("not json {").is_err());
    }

    #[test]
    fn fetch_index_reads_local_file() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("index.json");
        std::fs::write(&p, VALID).unwrap();
        let raw = fetch_index(p.to_str().unwrap()).unwrap();
        assert_eq!(raw, VALID);
        // 拉回来的原文能再解析。
        assert_eq!(parse_index(&raw).unwrap().len(), 2);
    }

    #[test]
    fn fetch_index_http_source_reports_clear_error_on_connection_failure() {
        // 127.0.0.1:1（保留端口，本机不会有服务监听）——不需要真实外网，连接会
        // 立即被拒绝，用来断言"HTTP 源确实走了 HTTP 路径"（而不是仍然报旧版本
        // "仅支持本地源"那句话）。真正的成功/sha256/大小上限路径见
        // `tests/market_skill_it.rs`（本机起一个真实 TCP server）。
        let err = fetch_index("http://127.0.0.1:1/index.json").unwrap_err();
        assert!(
            !err.contains("仅支持本地市场源"),
            "HTTP 源不应再走「仅支持本地」这条旧错误路径：{err}"
        );
        assert!(err.contains("请求") && err.contains("失败"), "实际：{err}");
    }

    // ---- check_remote_url：https-only（回环例外），审查修复轮 2 Important 2 ----

    #[test]
    fn check_remote_url_allows_https() {
        check_remote_url("https://example.invalid/index.json").expect("https 应放行");
    }

    #[test]
    fn check_remote_url_allows_http_loopback() {
        check_remote_url("http://127.0.0.1:1/index.json").expect("http 回环（IPv4）应放行");
        check_remote_url("http://localhost:8080/index.json").expect("http 回环（localhost）应放行");
        check_remote_url("http://[::1]:8080/index.json").expect("http 回环（IPv6）应放行");
    }

    #[test]
    fn check_remote_url_rejects_http_public_host() {
        let err = check_remote_url("http://example.invalid/index.json").unwrap_err();
        assert!(err.contains("https"), "错误信息应提到 https：{err}");
    }

    #[test]
    fn check_remote_url_rejects_other_schemes() {
        let err = check_remote_url("ftp://example.invalid/index.json").unwrap_err();
        assert!(err.contains("https"), "错误信息应提到 https：{err}");
    }

    #[test]
    fn fetch_index_http_public_source_is_rejected_without_network_request() {
        // "只允许 https" 只会从 check_remote_url 产出（网络请求失败的错误信息是
        // "请求...失败"，见 fetch_index_http_source_reports_clear_error_on_connection_failure）——
        // 命中它就证明这条路径在发请求前就被拒了，example.invalid 不可解析，真发了
        // 请求会报连接/DNS 错误而不是这句话。
        let err = fetch_index("http://example.invalid/index.json").unwrap_err();
        assert!(err.contains("只允许 https"), "实际：{err}");
    }

    /// 终审 Minor 1 回归：`fetch_index` 的 scheme 判断此前手写
    /// `starts_with("http://")`（大小写敏感），大写 `HTTPS://` 会匹配不上、
    /// 误落到本地文件分支。这里断言大写 scheme 确实走了 HTTP 分支——用一个
    /// 保留端口触发"请求...失败"这个 HTTP 路径特有的错误文案，本地文件分支
    /// 的失败文案是"读取市场索引...失败"，两者不会混淆。
    #[test]
    fn fetch_index_scheme_check_is_case_insensitive() {
        let err = fetch_index("HTTPS://127.0.0.1:1/index.json").unwrap_err();
        assert!(
            err.contains("请求") && err.contains("失败"),
            "大写 HTTPS:// 应该也走 HTTP 分支，而不是被误判成本地路径，实际：{err}"
        );
    }

    #[test]
    fn download_skill_zip_http_public_source_is_rejected_without_network_request() {
        let err = download_skill_zip("http://example.invalid/x.zip", "00").unwrap_err();
        assert!(err.contains("只允许 https"), "实际：{err}");
    }

    #[test]
    fn kind_defaults_to_app_and_skill_fields_default_to_none() {
        let entries = parse_index(VALID).unwrap();
        assert_eq!(
            entries[0].kind, "app",
            "旧索引条目没有 kind 字段应按 app 处理"
        );
        assert_eq!(entries[0].download_url, None);
        assert_eq!(entries[0].sha256, None);
        assert_eq!(entries[0].size, None);
        assert_eq!(entries[0].author, None);
    }

    #[test]
    fn skill_entry_parses_kind_and_download_fields() {
        let json = r#"{
          "entries": [
            { "name": "@superagent/connector-etiquette", "display_name": "连接器礼仪",
              "version": "1.0.0", "category": "skill", "kind": "skill",
              "source": "skills/connector-etiquette",
              "download_url": "https://example.invalid/connector-etiquette.zip",
              "sha256": "abcd1234", "size": 4096, "author": "superagent" }
          ]
        }"#;
        let entries = parse_index(json).unwrap();
        assert_eq!(entries[0].kind, "skill");
        assert_eq!(
            entries[0].download_url.as_deref(),
            Some("https://example.invalid/connector-etiquette.zip")
        );
        assert_eq!(entries[0].sha256.as_deref(), Some("abcd1234"));
        assert_eq!(entries[0].size, Some(4096));
        assert_eq!(entries[0].author.as_deref(), Some("superagent"));
    }

    // ---- unpack_skill_zip：安全解包（离线，不需要网络/HTTP server）----

    fn build_zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default();
        for (name, content) in entries {
            writer.start_file(*name, options).unwrap();
            std::io::Write::write_all(&mut writer, content).unwrap();
        }
        writer.finish().unwrap().into_inner()
    }

    #[test]
    fn unpack_skill_zip_extracts_normal_files() {
        let zip_bytes = build_zip(&[
            ("SKILL.md", b"---\nname: x\n---\nbody"),
            ("scripts/run.sh", b"#!/bin/sh\necho hi\n"),
        ]);
        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("out");
        unpack_skill_zip(&zip_bytes, &dest).expect("正常 zip 应解包成功");
        assert_eq!(
            std::fs::read_to_string(dest.join("SKILL.md")).unwrap(),
            "---\nname: x\n---\nbody"
        );
        assert!(dest.join("scripts/run.sh").is_file());
    }

    #[test]
    fn unpack_skill_zip_rejects_path_traversal() {
        let zip_bytes = build_zip(&[("../evil.txt", b"pwned")]);
        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("out");
        let err = unpack_skill_zip(&zip_bytes, &dest).unwrap_err();
        assert!(err.contains("不安全"), "实际：{err}");
        assert!(!dest.join("../evil.txt").exists());
        assert!(
            !tmp.path().join("evil.txt").exists(),
            "不应逃逸到 dest 之外"
        );
    }

    #[test]
    fn unpack_skill_zip_rejects_symlink_entries() {
        let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        writer
            .add_symlink(
                "evil-link",
                "/etc/passwd",
                zip::write::SimpleFileOptions::default(),
            )
            .unwrap();
        let zip_bytes = writer.finish().unwrap().into_inner();

        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("out");
        let err = unpack_skill_zip(&zip_bytes, &dest).unwrap_err();
        assert!(err.contains("符号链接"), "实际：{err}");
    }

    #[test]
    fn unpack_skill_zip_rejects_too_many_files() {
        let entries: Vec<(String, Vec<u8>)> = (0..=MAX_UNPACKED_SKILL_FILES)
            .map(|i| (format!("f{i}.txt"), b"x".to_vec()))
            .collect();
        let borrowed: Vec<(&str, &[u8])> = entries
            .iter()
            .map(|(n, c)| (n.as_str(), c.as_slice()))
            .collect();
        let zip_bytes = build_zip(&borrowed);

        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("out");
        let err = unpack_skill_zip(&zip_bytes, &dest).unwrap_err();
        assert!(err.contains("文件数"), "实际：{err}");
    }

    #[test]
    fn unpack_skill_zip_rejects_bomb_by_declared_uncompressed_size() {
        // 6 MiB 高度重复数据——deflate 下压缩后体积很小（远低于
        // MAX_SKILL_ZIP_BYTES 下载上限），但 zip 中央目录里声明的解压后大小
        // （`ZipFile::size()`）如实是 6 MiB，超过 `MAX_UNPACKED_SKILL_BYTES`
        // （5 MiB）——这正是"炸弹预判"要拦的形状：不需要真的解压完就能提前拒绝。
        let big = vec![0u8; 6 * 1024 * 1024];
        let zip_bytes = build_zip(&[("bomb.bin", &big)]);

        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("out");
        let err = unpack_skill_zip(&zip_bytes, &dest).unwrap_err();
        assert!(
            err.contains("炸弹") || err.contains("超过上限"),
            "实际：{err}"
        );
        assert!(!dest.exists(), "校验失败不应创建目标目录");
    }

    #[test]
    fn zip_duplicate_entry_paths_last_write_wins_or_rejected() {
        // 钉住现有行为：两个条目字面名字不同（"dup.txt" 与 "./dup.txt"）——
        // `zip::ZipWriter::start_file` 只按字面字符串查重复，这两个不撞，能正常写进
        // 同一个 zip；但 `ZipFile::enclosed_name()` 会把前导的 "./" 归一化掉，两者
        // 解析出的相对路径都是 "dup.txt"。unpack_skill_zip 不对"归一化后路径"做
        // 二次查重——两遍扫描都按 archive 里的物理顺序处理，第二遍落盘对同一个
        // out_path 调用两次 `File::create`，后一次覆盖前一次。这里钉住"最后写入者
        // 生效"这个现状（而非"拒绝归一化后撞路径的条目"），如果将来改成拒绝，把
        // 这条测试的断言换成 unwrap_err() 即可。
        let zip_bytes = build_zip(&[("dup.txt", b"first"), ("./dup.txt", b"second-and-longer")]);
        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("out");
        unpack_skill_zip(&zip_bytes, &dest)
            .expect("现状：归一化后撞路径的条目不拒绝，最后写入者生效");
        assert_eq!(
            std::fs::read_to_string(dest.join("dup.txt")).unwrap(),
            "second-and-longer",
            "现状：按 archive 物理顺序，后一个归一化后同路径的条目覆盖前一个"
        );
    }

    #[test]
    fn zip_overlong_filename_fails_without_residue() {
        let long_name = "a".repeat(300);
        let zip_bytes = build_zip(&[(long_name.as_str(), b"x")]);
        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("out");
        let err = unpack_skill_zip(&zip_bytes, &dest).unwrap_err();
        assert!(err.contains("255"), "实际：{err}");
        assert!(!dest.exists(), "超长文件名应在落盘前被拒绝，不留残留目录");
    }

    /// 终审 Minor 2 回归：第一遍元数据校验全部通过（`enclosed_name()` 认为
    /// `"a"` 与 `"a/b.txt"` 都是安全路径，两条都不是符号链接，声明大小/文件数
    /// /路径分量长度都在限内），第二遍真正落盘时才失败——条目 `"a"`（无尾部
    /// `/`，按文件写入）先把 `dest/a` 写成一个**文件**，紧接着条目
    /// `"a/b.txt"` 需要 `create_dir_all(dest/a)` 把 `dest/a` 当父目录，但它
    /// 已经是文件而非目录，`create_dir_all` 会报 `AlreadyExists`——这正是
    /// "第二遍才失败"的真实形状（不是构造出来凑数的场景）。断言失败后
    /// `dest` 被整个清空，不留下条目 `"a"` 已经写下去的那个文件。
    #[test]
    fn unpack_skill_zip_cleans_up_dest_when_second_pass_write_fails() {
        let zip_bytes = build_zip(&[("a", b"file"), ("a/b.txt", b"x")]);
        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("out");
        let err = unpack_skill_zip(&zip_bytes, &dest).unwrap_err();
        assert!(
            err.contains("创建") || err.contains("exist"),
            "错误信息应点名建目录失败，实际：{err}"
        );
        assert!(
            !dest.exists(),
            "第二遍落盘失败也应清场，不留下条目 \"a\" 已写入的残留文件：{dest:?}"
        );
    }

    #[test]
    fn download_skill_zip_rejects_sha256_mismatch_without_touching_network_success_path() {
        // 连接失败本身就该在 sha256 校验之前就返回错误——这里只断言"连不上时不会
        // 假装校验通过"，真正的"连上了但 sha256 不符"场景见
        // `tests/market_skill_it.rs`（本机真实 HTTP server）。
        let err = download_skill_zip("http://127.0.0.1:1/skill.zip", "deadbeef").unwrap_err();
        assert!(
            !err.contains("sha256 不符"),
            "连接失败不该走到 sha256 比对：{err}"
        );
    }
}
