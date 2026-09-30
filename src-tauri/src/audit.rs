//! 宿主级审计日志：记录每个 app 触发的工具调用（what each app tried to do）。
//!
//! 落盘位置是 **host-global** 的 `<data_root>/audit/<YYYY-MM-DD>.jsonl`（不是
//! per-app 目录）——每条记录用 `app_id` 字段标出是谁触发的，见
//! `paths::DataLayout::audit_dir`。按日滚动 + 单文件大小上限（超限滚号）+
//! 保留期（按文件名日期，非 mtime，可确定性测试）。`redact` 是安全关键路径：
//! 落盘前必须先脱敏，绝不能让审计日志本身泄漏密钥。

use crate::paths::DataLayout;
use std::io::Write as _;
use std::path::{Path, PathBuf};

/// 审计日志保留天数：早于「今天 - N 天」的按文件名日期一律删除。
pub const AUDIT_RETENTION_DAYS: u64 = 30;
/// 单个日志文件大小上限（字节）：超过则滚动到编号的同日兄弟文件。
pub const AUDIT_MAX_FILE_BYTES: u64 = 5 * 1024 * 1024;

/// 一条审计记录。`args` 是**已脱敏、JSON 字符串化**的工具参数——绝不直接存原始参数。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Entry {
    pub ts: String,
    pub app_id: String,
    pub tool: String,
    pub args: String,
    pub verdict: String,
}

/// `query` 的过滤条件；三个字段全为 `None` 时等价于「不过滤，只按 limit 截断」。
#[derive(Debug, Default, Clone, serde::Serialize, serde::Deserialize)]
pub struct AuditFilter {
    #[serde(default)]
    pub app_id: Option<String>,
    #[serde(default)]
    pub tool: Option<String>,
    #[serde(default)]
    pub limit: Option<usize>,
}

// ---------------------------------------------------------------------------
// redact：安全关键的纯函数，不做任何 IO。
// ---------------------------------------------------------------------------

/// 对将要落盘的字符串做脱敏。V2 设计——修复 V1 的两个真实缺陷：
/// (a) 漏掉 `postgres://user:pass@host` 这类 URL 明文密码、`Authorization: Basic
///     <base64>` 这类真实凭据形状；
/// (b) 把普通路径 / URL 整体吞成 `***`，毁掉审计日志的可读性。
///
/// 根因是 V1 单纯依赖「长度 + 数字」熵下限去扫描一个包含 `/`、`.` 的宽字符类：
/// 这个类既会把 `user:pass@host` 里被 `:`/`@` 分隔出来的短密码漏判（够不到熵阈值），
/// 又会把整条路径/URL（大量 `/`、`.`）拼成一个超长 run 一起误伤成 `***`。
///
/// V2 按「先精确上下文、后保守熵兜底」重新排序，依次跑六趟；任何一趟的残留碎片
/// 都指望后面的层兜底——底线是「整体不泄漏」而非「每个 pass 各自独立完美」：
///
/// 1. home 目录前缀 -> `~`（`$HOME` 为空/为 `/` 时跳过，避免误伤）。
/// 2. **URL userinfo 密码**：`<scheme>://<user>:<password>@<host>` 中，密码段
///    （`://` 后第一个 `:` 到 `@` 之间）整体替换为 `***`，user/host/path 原样保留。
///    强上下文信号（`://` + `@`）——不论密码长度/形状一律脱敏。
/// 3. **认证方案凭据**：`Bearer`/`Basic`/`Token`/`Digest`/`APIKey`（大小写不敏感，
///    要求词边界）+ 空白之后，紧跟的凭据 run（`[A-Za-z0-9+/=_.-]{6,}`）整体替换为
///    `***`，方案词本身保留（`Bearer <token>` -> `Bearer ***`）。必须排在第 5 步
///    （键名触发）之前——否则键名触发对裸形式的扫描会在第一个空白处截断，只吃掉
///    "Bearer"/"Basic" 这个词本身，残留真正的凭据明文落盘。
/// 4. **显式 token 锚点**（精确前缀 + 形状，低误伤）：`sk-`、`ghp_`/`gho_`/`ghs_`/
///    `ghu_`/`github_pat_`、`xox[baprs]-`、`AIza`、`AKIA`/`ASIA`、JWT `eyJ`。
/// 5. **键名触发**：`"<key>":"<value>"`（JSON）或 `<key>=<value>` / `<key>: <value>`
///    （裸形式），当 key（大小写不敏感）包含 token/secret/password/apikey/... 等
///    词时，不论 value 长什么样一律整段替换——专治「值本身没有高熵特征」的命名
///    密钥（如 `password=hunter2short`）。
/// 6. **保守熵兜底**：扫描字符类 `[A-Za-z0-9+=_-]`——刻意排除 `/`、`.`、`:`、`@`，
///    使路径/URL 按分隔符天然断成短段而不会被拼接成一个超长 run 误伤；只有长度
///    ≥24、同时含字母、且（含大写字母，或含 `+`/`=`，或数字数 ≥4）才整体替换。
///    用于兜底「既没有已知前缀也没有键名信号」的裸高熵密钥（40 位 hex、独立出现
///    的 mixed-case base64 token），同时放过纯小写连字符路径段（`some-tool`）、
///    短版本号（`v1.2.3`）等良性文本。
pub fn redact(s: &str) -> String {
    let mut out = s.to_string();
    if let Ok(home) = std::env::var("HOME") {
        if !home.is_empty() && home != "/" {
            out = out.replace(home.as_str(), "~");
        }
    }
    out = redact_url_userinfo_password(&out);
    out = redact_auth_scheme_credential(&out);
    out = redact_explicit_anchors(&out);
    out = redact_key_triggered_values(&out);
    out = redact_entropy_runs(&out);
    out
}

/// pass 2：URL userinfo 密码。定位 `://`，authority 边界为下一个 `/`、`?`、`#`、
/// 空白或字符串结尾；authority 内若存在 `@`，且 `@` 之前存在 `:`，则该 `:` 到 `@`
/// 之间整体替换为 `***`（不论内容/长度）。没有 `@`（无 userinfo）或没有 `:`
/// （只有 `user@host`，无密码）时原样保留——避免误伤纯粹的 `scheme://host` URL。
fn redact_url_userinfo_password(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let n = chars.len();
    let mut out = String::new();
    let mut i = 0;
    while i < n {
        if i + 3 <= n && chars[i] == ':' && chars[i + 1] == '/' && chars[i + 2] == '/' {
            let auth_start = i + 3;
            let mut auth_end = auth_start;
            while auth_end < n
                && !matches!(chars[auth_end], '/' | '?' | '#')
                && !chars[auth_end].is_whitespace()
            {
                auth_end += 1;
            }
            let at_pos = chars[auth_start..auth_end]
                .iter()
                .position(|&c| c == '@')
                .map(|p| p + auth_start);
            if let Some(at_pos) = at_pos {
                let colon_pos = chars[auth_start..at_pos]
                    .iter()
                    .position(|&c| c == ':')
                    .map(|p| p + auth_start);
                if let Some(colon_pos) = colon_pos {
                    out.push_str("://");
                    out.extend(&chars[auth_start..=colon_pos]); // "<user>:"
                    out.push_str("***");
                    out.extend(&chars[at_pos..auth_end]); // "@<host...>"
                    i = auth_end;
                    continue;
                }
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

const AUTH_SCHEME_WORDS: &[&str] = &["Bearer", "Basic", "Token", "Digest", "APIKey"];
const AUTH_SCHEME_CRED_MIN_RUN: usize = 6;

fn is_word_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// pass 3 凭据字符类：base64url + 传统 base64 的 `+`/`/`/`=` + JWT 分段用的 `.`。
/// 这里刻意保留宽字符类——`Bearer`/`Basic` 后面的凭据本身就常见 `.`/`-`/`_`
/// 打断（如 JWT），需要整体吃掉。
fn is_scheme_cred_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '=' | '_' | '.' | '-')
}

/// pass 3：认证方案凭据。`Bearer`/`Basic`/`Token`/`Digest`/`APIKey`（大小写不敏感，
/// 要求词边界——前后都不能紧邻 word 字符，避免命中复合标识符里的子串）+ 空白后，
/// 紧跟的凭据 run 整体替换为 `***`；方案词本身保留在输出里。
fn redact_auth_scheme_credential(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let n = chars.len();
    let mut out = String::new();
    let mut i = 0;
    'outer: while i < n {
        let boundary_before = i == 0 || !is_word_char(chars[i - 1]);
        if boundary_before {
            for &word in AUTH_SCHEME_WORDS {
                let wlen = word.chars().count();
                if i + wlen > n {
                    continue;
                }
                let matches_word = chars[i..i + wlen]
                    .iter()
                    .zip(word.chars())
                    .all(|(&c, w)| c.eq_ignore_ascii_case(&w));
                if !matches_word {
                    continue;
                }
                let boundary_after = i + wlen == n || !is_word_char(chars[i + wlen]);
                if !boundary_after {
                    continue;
                }
                let ws_start = i + wlen;
                let mut p = ws_start;
                while p < n && chars[p].is_whitespace() {
                    p += 1;
                }
                if p == ws_start {
                    continue; // 方案词后没有空白，不是 "<scheme> <credential>" 形状
                }
                let cred_start = p;
                let mut e = cred_start;
                while e < n && is_scheme_cred_char(chars[e]) {
                    e += 1;
                }
                if e - cred_start >= AUTH_SCHEME_CRED_MIN_RUN {
                    out.extend(&chars[i..cred_start]); // 方案词 + 空白，原样保留
                    out.push_str("***");
                    i = e;
                    continue 'outer;
                }
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

fn is_aws_key_char(c: char) -> bool {
    c.is_ascii_digit() || c.is_ascii_uppercase()
}

fn is_us_hyphen_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '_' | '-')
}

fn is_us_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

fn is_hyphen_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '-'
}

/// `(前缀, 前缀后 run 的最小长度, run 字符谓词)`。
type AnchorRule = (&'static str, usize, fn(char) -> bool);

/// pass 4 锚点表。前缀区分大小写——真实凭据的前缀大小写是固定的（GitHub/Slack
/// token 前缀恒为小写，AWS/Google/JWT 前缀恒定大小写混合），不需要也不应该做
/// 大小写不敏感匹配。命中后整体（前缀 + run）替换为 `***`。
const ANCHOR_RULES: &[AnchorRule] = &[
    ("sk-", 16, is_us_hyphen_char as fn(char) -> bool),
    ("ghp_", 16, is_us_char as fn(char) -> bool),
    ("gho_", 16, is_us_char as fn(char) -> bool),
    ("ghs_", 16, is_us_char as fn(char) -> bool),
    ("ghu_", 16, is_us_char as fn(char) -> bool),
    ("github_pat_", 16, is_us_char as fn(char) -> bool),
    ("xoxb-", 8, is_hyphen_char as fn(char) -> bool),
    ("xoxa-", 8, is_hyphen_char as fn(char) -> bool),
    ("xoxp-", 8, is_hyphen_char as fn(char) -> bool),
    ("xoxr-", 8, is_hyphen_char as fn(char) -> bool),
    ("xoxs-", 8, is_hyphen_char as fn(char) -> bool),
    ("AIza", 20, is_us_hyphen_char as fn(char) -> bool),
    ("AKIA", 12, is_aws_key_char as fn(char) -> bool),
    ("ASIA", 12, is_aws_key_char as fn(char) -> bool),
    ("eyJ", 8, is_us_hyphen_char as fn(char) -> bool),
];

/// pass 4：显式 token 锚点（`sk-` / GitHub `gh[posu]_`|`github_pat_` / Slack
/// `xox[baprs]-` / Google `AIza` / AWS `AKIA`/`ASIA` / JWT `eyJ`）。见 `ANCHOR_RULES`。
/// 每个位置按表顺序尝试匹配前缀 + 达标长度的 run，命中则整体（含前缀）替换为 `***`。
fn redact_explicit_anchors(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let n = chars.len();
    let mut out = String::new();
    let mut i = 0;
    'outer: while i < n {
        for &(prefix, min_run, pred) in ANCHOR_RULES {
            let plen = prefix.chars().count();
            if i + plen <= n && chars[i..i + plen].iter().copied().eq(prefix.chars()) {
                let mut j = i + plen;
                while j < n && pred(chars[j]) {
                    j += 1;
                }
                if j - (i + plen) >= min_run {
                    out.push_str("***");
                    i = j;
                    continue 'outer;
                }
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

/// 键名触发列表（大小写不敏感、子串匹配）：key 名只要包含其中任意一个词，
/// 不论 value 形状如何都整段替换——用于捕获没有高熵/前缀特征的命名密钥。
/// 不包含 `authorization`/`bearer` 等词——`Authorization: Bearer <token>` 这类
/// 形状已由 pass 3（认证方案凭据）精确处理并保留方案词，此处再触发会把
/// "Bearer"/"Basic" 这个词本身也吞掉，产出双重 `***` 的冗余噪音。
const KEY_TRIGGERS: &[&str] = &[
    "token",
    "secret",
    "password",
    "passwd",
    "apikey",
    "api_key",
    "access_key",
    "secret_key",
    "credential",
    "private_key",
    "session_id",
];

fn is_key_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '-'
}

fn key_matches_trigger(key: &str) -> bool {
    let lower = key.to_ascii_lowercase();
    KEY_TRIGGERS.iter().any(|t| lower.contains(t))
}

fn find_char(chars: &[char], from: usize, target: char) -> Option<usize> {
    chars[from..]
        .iter()
        .position(|&c| c == target)
        .map(|p| p + from)
}

fn skip_ws(chars: &[char], mut p: usize) -> usize {
    while p < chars.len() && chars[p].is_whitespace() {
        p += 1;
    }
    p
}

/// 尝试从位置 `i` 解析 `"<key>":"<value>"`（JSON）或 `<key>=<value>` / `<key>: <value>`
/// （裸形式）。命中触发词时返回 `(value_open, value_close, next_i)`：
/// - `[i, value_open)` 原样保留（key + 分隔符 + 可能的开引号）；
/// - `[value_open, value_close)` 整体替换为 `***`（无论其形状）；
/// - `[value_close, next_i)` 原样保留（可能的闭引号）。
///
/// 裸形式的 value 在遇到空白 / `&` / `,` / `"` 处终止。
fn match_key_triggered_value(chars: &[char], i: usize) -> Option<(usize, usize, usize)> {
    let n = chars.len();
    let (key, after_key) = if chars[i] == '"' {
        let close = find_char(chars, i + 1, '"')?;
        (chars[i + 1..close].iter().collect::<String>(), close + 1)
    } else if is_key_char(chars[i]) {
        let mut j = i;
        while j < n && is_key_char(chars[j]) {
            j += 1;
        }
        (chars[i..j].iter().collect::<String>(), j)
    } else {
        return None;
    };

    let mut p = skip_ws(chars, after_key);
    if p >= n || (chars[p] != ':' && chars[p] != '=') {
        return None;
    }
    p = skip_ws(chars, p + 1);

    if !key_matches_trigger(&key) {
        return None;
    }

    if p < n && chars[p] == '"' {
        let close = find_char(chars, p + 1, '"')?;
        Some((p + 1, close, close + 1))
    } else {
        let mut e = p;
        while e < n
            && !chars[e].is_whitespace()
            && chars[e] != '&'
            && chars[e] != ','
            && chars[e] != '"'
        {
            e += 1;
        }
        Some((p, e, e))
    }
}

fn redact_key_triggered_values(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let n = chars.len();
    let mut out = String::new();
    let mut i = 0;
    while i < n {
        if let Some((val_open, val_close, next_i)) = match_key_triggered_value(&chars, i) {
            out.extend(&chars[i..val_open]);
            out.push_str("***");
            out.extend(&chars[val_close..next_i]);
            i = next_i;
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    out
}

const ENTROPY_RUN_MIN_LEN: usize = 24;

/// pass 6 字符类：`[A-Za-z0-9+=_-]`——刻意排除 `/`、`.`、`:`、`@`，使路径分量
/// （`/a/b/c`）、URL host（`api.example.com`）、URL path（`/v1/users/12345`）按
/// 分隔符天然断成短段，不会被拼接成一个跨越整条路径/URL 的超长 run 一起误伤。
fn is_entropy_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '+' | '=' | '_' | '-')
}

/// 保守高熵 run backstop：`is_entropy_char` 组成的 run 长度 ≥24，且同时满足
/// 「含字母」与「含大写字母 或 含 `+`/`=` 或 数字数 ≥4」时整体替换为 `***`。
/// 用于兜底没有已知前缀/键名信号的裸密钥（40 位 hex、独立出现的 mixed-case
/// base64 token）；纯小写连字符路径段（`some-tool`）、短版本号等良性文本因为
/// 长度不够或没有大写/多位数字而不会被误伤。
fn redact_entropy_runs(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let n = chars.len();
    let mut out = String::new();
    let mut i = 0;
    while i < n {
        if is_entropy_char(chars[i]) {
            let mut j = i;
            let mut has_alpha = false;
            let mut has_upper = false;
            let mut has_plus_eq = false;
            let mut digit_count: usize = 0;
            while j < n && is_entropy_char(chars[j]) {
                let c = chars[j];
                if c.is_ascii_alphabetic() {
                    has_alpha = true;
                }
                if c.is_ascii_uppercase() {
                    has_upper = true;
                }
                if c == '+' || c == '=' {
                    has_plus_eq = true;
                }
                if c.is_ascii_digit() {
                    digit_count += 1;
                }
                j += 1;
            }
            let len = j - i;
            if len >= ENTROPY_RUN_MIN_LEN
                && has_alpha
                && (has_upper || has_plus_eq || digit_count >= 4)
            {
                out.push_str("***");
            } else {
                out.extend(&chars[i..j]);
            }
            i = j;
            continue;
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

// ---------------------------------------------------------------------------
// 日期/时间：不引入 chrono，纯整数算法（Howard Hinnant civil_from_days）。
// ---------------------------------------------------------------------------

/// days-since-epoch(UTC) -> (year, month, day)，proleptic Gregorian，对正负 z 都成立。
/// 参考 http://howardhinnant.github.io/date_algorithms.html（公开算法，无需外部 crate）。
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    let y = if m <= 2 { y + 1 } else { y };
    (y, m as u32, d as u32)
}

fn now_duration() -> std::time::Duration {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
}

fn today_days_since_epoch() -> i64 {
    (now_duration().as_secs() / 86400) as i64
}

/// 返回 (今天的日期字符串 "YYYY-MM-DD", 当前时刻的 RFC3339 字符串, UTC)。
fn now_parts() -> (String, String) {
    let dur = now_duration();
    let total_secs = dur.as_secs() as i64;
    let days = total_secs.div_euclid(86400);
    let secs_of_day = total_secs.rem_euclid(86400);
    let (y, m, d) = civil_from_days(days);
    let date = format!("{y:04}-{m:02}-{d:02}");
    let hh = secs_of_day / 3600;
    let mm = (secs_of_day % 3600) / 60;
    let ss = secs_of_day % 60;
    let ms = dur.subsec_millis();
    let ts = format!("{date}T{hh:02}:{mm:02}:{ss:02}.{ms:03}Z");
    (date, ts)
}

fn is_valid_date_str(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 10
        && b[4] == b'-'
        && b[7] == b'-'
        && b[0..4].iter().all(u8::is_ascii_digit)
        && b[5..7].iter().all(u8::is_ascii_digit)
        && b[8..10].iter().all(u8::is_ascii_digit)
}

// ---------------------------------------------------------------------------
// 滚动：按日文件 + size cap 溢出滚号。
// ---------------------------------------------------------------------------

fn file_size(p: &Path) -> u64 {
    std::fs::metadata(p).map(|m| m.len()).unwrap_or(0)
}

/// 选出今天该写入的文件：base `<date>.jsonl` 未超限就用它；超限则往
/// `<date>.1.jsonl`、`<date>.2.jsonl`... 找第一个「不存在，或存在但未超限」的槽位。
///
/// TOCTOU 说明：这里的「查大小 -> 决定目标文件 -> append」不是原子操作，v1 故意不加
/// 文件锁（YAGNI）。并发写入者之间可能出现竞态，导致 size cap 被轻微越过（比如两个
/// 进程同时判断 base 文件「未超限」、都往同一个文件写入，写完后总大小超过 cap 一点
/// 点）——但由于每次 `append_line` 走的是 `O_APPEND` 打开模式，单次 write 本身在
/// POSIX 上是原子的，不会出现「写一半」的截断/交错/数据损坏或丢失，只是 cap 变成
/// best-effort/soft 上限而非硬上限。对审计日志这种「宁可稍微超限也不要丢数据」的
/// 场景，这个权衡是可以接受的。
fn target_file_for_day_with_cap(dir: &Path, date: &str, cap: u64) -> PathBuf {
    let base = dir.join(format!("{date}.jsonl"));
    if file_size(&base) < cap {
        return base;
    }
    let mut idx: u32 = 1;
    loop {
        let candidate = dir.join(format!("{date}.{idx}.jsonl"));
        if !candidate.exists() || file_size(&candidate) < cap {
            return candidate;
        }
        idx += 1;
    }
}

fn target_file_for_day(dir: &Path, date: &str) -> PathBuf {
    target_file_for_day_with_cap(dir, date, AUDIT_MAX_FILE_BYTES)
}

fn append_line(path: &Path, line: &str) -> Result<(), String> {
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| e.to_string())?;
    writeln!(f, "{line}").map_err(|e| e.to_string())
}

/// 解析文件名 `<date>.jsonl` 或 `<date>.<idx>.jsonl` -> (date, idx)（base 文件 idx=0）。
/// 非法/不认识的文件名一律返回 `None`（调用方跳过，不 panic）。
fn parse_filename(name: &str) -> Option<(String, u32)> {
    let stem = name.strip_suffix(".jsonl")?;
    let mut parts = stem.split('.');
    let date = parts.next()?;
    if !is_valid_date_str(date) {
        return None;
    }
    let idx = match parts.next() {
        Some(n) => n.parse::<u32>().ok()?,
        None => 0,
    };
    if parts.next().is_some() {
        return None;
    }
    Some((date.to_string(), idx))
}

// ---------------------------------------------------------------------------
// 保留期
// ---------------------------------------------------------------------------

/// 删除 `audit/*.jsonl` 中「文件名日期」早于 `AUDIT_RETENTION_DAYS` 天前的文件。
/// 刻意按文件名日期而非 mtime 判断——确定性、可测试，不依赖真实文件系统时间戳。
/// best-effort：目录不存在 / 单个文件删除失败都忽略，不向上传播错误。
pub fn prune(layout: &DataLayout) -> Result<(), String> {
    let dir = layout.audit_dir();
    let entries = match std::fs::read_dir(&dir) {
        Ok(e) => e,
        Err(_) => return Ok(()), // 目录还不存在：无事可做
    };
    let cutoff_days = today_days_since_epoch() - AUDIT_RETENTION_DAYS as i64;
    let (cy, cm, cd) = civil_from_days(cutoff_days);
    let cutoff = format!("{cy:04}-{cm:02}-{cd:02}");

    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let Some((date, _idx)) = parse_filename(name) else {
            continue; // 不认识的文件名：不动它
        };
        if date.as_str() < cutoff.as_str() {
            let _ = std::fs::remove_file(&path);
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// record / query
// ---------------------------------------------------------------------------

/// 追加一条审计记录：确保目录存在 → 脱敏 args → 选目标文件（含 size cap 滚号）
/// → append 一行 JSON → 顺手跑一次保留期清理（best-effort，失败不影响主流程）。
/// 审计写入本身也是 best-effort——调用方不应因为审计写失败而崩溃/中断业务逻辑，
/// 由调用方决定拿到 `Err` 后是否 log。
pub fn record(
    layout: &DataLayout,
    app_id: &str,
    tool: &str,
    args: &str,
    verdict: &str,
) -> Result<(), String> {
    let dir = layout.audit_dir();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;

    let (date, ts) = now_parts();
    let entry = Entry {
        ts,
        app_id: app_id.to_string(),
        tool: tool.to_string(),
        args: redact(args),
        verdict: verdict.to_string(),
    };
    let line = serde_json::to_string(&entry).map_err(|e| e.to_string())?;
    let path = target_file_for_day(&dir, &date);
    append_line(&path, &line)?;

    let _ = prune(layout);
    Ok(())
}

/// 按 filter 查询审计记录：日文件从新到旧扫描（同日内按滚号从高到低，文件内
/// 按行倒序），返回新到旧、最多 `limit` 条。找不到 `audit_dir` / 文件损坏的行
/// 一律跳过，不报错。
pub fn query(layout: &DataLayout, filter: &AuditFilter) -> Vec<Entry> {
    let dir = layout.audit_dir();
    let mut files: Vec<(String, u32, PathBuf)> = match std::fs::read_dir(&dir) {
        Ok(rd) => rd
            .flatten()
            .filter_map(|e| {
                let path = e.path();
                let name = path.file_name()?.to_str()?.to_string();
                let (date, idx) = parse_filename(&name)?;
                Some((date, idx, path))
            })
            .collect(),
        Err(_) => Vec::new(),
    };
    files.sort_by(|a, b| (b.0.as_str(), b.1).cmp(&(a.0.as_str(), a.1)));

    let mut out = Vec::new();
    for (_, _, path) in files {
        let Ok(content) = std::fs::read_to_string(&path) else {
            continue;
        };
        for line in content.lines().rev() {
            if line.trim().is_empty() {
                continue;
            }
            let Ok(entry) = serde_json::from_str::<Entry>(line) else {
                continue;
            };
            if let Some(app_id) = &filter.app_id {
                if &entry.app_id != app_id {
                    continue;
                }
            }
            if let Some(tool) = &filter.tool {
                if &entry.tool != tool {
                    continue;
                }
            }
            out.push(entry);
            if let Some(limit) = filter.limit {
                if out.len() >= limit {
                    return out;
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    // ---- redact: pure cases ----

    #[test]
    fn redact_replaces_home_dir_prefix() {
        std::env::set_var("HOME", "/Users/testuser_audit_redact");
        let input = "/Users/testuser_audit_redact/secret/file.txt opened";
        assert_eq!(redact(input), "~/secret/file.txt opened");
        std::env::remove_var("HOME");
    }

    #[test]
    fn redact_masks_sk_style_api_key() {
        let input = "api_key=sk-abcdEFGH12345678wxyz please";
        assert_eq!(redact(input), "api_key=*** please");
    }

    #[test]
    fn redact_masks_bearer_token() {
        // V2：认证方案词本身保留，只脱敏方案词后面的凭据（见 pass 3 doc）。
        let input = "Authorization: Bearer abc123XYZtoken";
        assert_eq!(redact(input), "Authorization: Bearer ***");
    }

    #[test]
    fn redact_masks_long_hex_blob() {
        let input = "sha1:a1b2c3d4e5f6a7b8c9d0e1f2a3b4c5d6e7f8a9b0 done";
        assert_eq!(input.len() - "sha1: done".len(), 40);
        assert_eq!(redact(input), "sha1:*** done");
    }

    #[test]
    fn redact_leaves_benign_string_unchanged() {
        let input = "hello world, this task finished with status ok (12 items)";
        assert_eq!(redact(input), input);
    }

    #[test]
    fn redact_does_not_leak_sk_key_short_of_threshold() {
        // 15 word chars（< 16）不算命中——边界值本身不是本测试重点，但验证不会
        // 误伤明显太短、不像真密钥的串。
        let short = "sk-abcdEFGH12345"; // 15 chars after "sk-"
        assert_eq!(redact(short), short);
    }

    // ---- redact: 分片泄露修复 — 必须完全脱敏的向量 1-8（安全评审要求） ----
    // 根因：旧实现每个 pass 只匹配"连续的 word/b64 字符 run"，密钥内部一旦出现
    // `-`/`.`/`_` 分隔符就被切成两段，各段都达不到长度阈值，从而以明文落盘。
    // 下面每个用例都断言"原始密钥主体的任何一段都不再出现在输出里"，而不只是
    // 检查整体字符串相等——这才是这次修复真正要堵住的漏洞。

    #[test]
    fn redact_vector1_sk_key_split_by_interior_hyphens() {
        let input = "token=sk-AAAAAAAAAA-BBBBBBBBBB";
        let out = redact(input);
        assert!(!out.contains("AAAAAAAAAA"), "leaked fragment: {out}");
        assert!(!out.contains("BBBBBBBBBB"), "leaked fragment: {out}");
        assert_eq!(out, "token=***");
    }

    #[test]
    fn redact_vector2_aws_access_key_id() {
        let input = "aws_access_key_id=AKIAIOSFODNN7EXAMPLE";
        let out = redact(input);
        assert!(!out.contains("AKIAIOSFODNN7EXAMPLE"));
        assert!(!out.contains("IOSFODNN7EXAMPLE"));
        assert_eq!(out, "aws_access_key_id=***");
    }

    #[test]
    fn redact_vector3_jwt_dot_separated_segments() {
        // V2：JWT 锚点字符类不含 `.`（避免吞掉相邻路径的点号），所以三个以 `.`
        // 分隔的 segment 分别被 pass 4（前两段，各自都以 eyJ 开头）和 pass 6 熵
        // 兜底（第三段，纯 base64url 且含大写字母）脱敏，`.` 分隔符本身保留
        // （`***.***.***`）——三段主体都不再出现在输出里，这才是安全底线。
        let input = "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.SflKxwRJSMeKKF2QT4fwpMeJf36POk6yJV_adQssw5c";
        let out = redact(input);
        assert!(!out.contains("eyJhbGciOiJIUzI1NiJ9"));
        assert!(!out.contains("eyJzdWIiOiIxMjM0NTY3ODkwIn0"));
        assert!(!out.contains("SflKxwRJSMeKKF2QT4fwpMeJf36POk6yJV_adQssw5c"));
    }

    #[test]
    fn redact_vector4_sk_ant_key_split_by_interior_hyphens() {
        let input = "sk-ant-api03-abc123DEF456-ghi789JKL012mno";
        let out = redact(input);
        assert!(!out.contains("abc123DEF456"));
        assert!(!out.contains("ghi789JKL012mno"));
        assert_eq!(out, "***");
    }

    #[test]
    fn redact_vector5_bearer_token_with_interior_dots() {
        let input = "Authorization: Bearer abc.def.ghi123456789";
        let out = redact(input);
        assert!(!out.contains("ghi123456789"));
        assert!(!out.contains("abc.def"));
        assert_eq!(out, "Authorization: Bearer ***");
    }

    #[test]
    fn redact_vector6_json_secret_key_no_shape_signal() {
        let input = r#"{"secret_key":"AKIAsomethingLong123"}"#;
        let out = redact(input);
        assert!(!out.contains("AKIAsomethingLong123"));
        assert!(!out.contains("somethingLong123"));
        assert_eq!(out, r#"{"secret_key":"***"}"#);
    }

    #[test]
    fn redact_vector7_standalone_40char_hex() {
        let input = "da39a3ee5e6b4b0d3255bfef95601890afd80709";
        let out = redact(input);
        assert!(!out.contains("da39a3ee5e6b4b0d3255bfef95601890afd80709"));
        assert_eq!(out, "***");
    }

    #[test]
    fn redact_vector8_password_key_triggered_short_value() {
        // 短值 + 含数字，靠 entropy backstop（长度 >=24）抓不到——必须靠键名触发。
        let input = "password=hunter2short";
        let out = redact(input);
        assert!(!out.contains("hunter2short"));
        assert_eq!(out, "password=***");
    }

    // ---- redact: V2 新增覆盖 — URL 明文密码 / 认证方案凭据(Basic) / 更多显式
    // token 锚点(GitHub/Slack/Google)。这些是本次修复要补的两个真实缺陷之一
    // （之前会漏判）。----

    #[test]
    fn redact_vector9_url_userinfo_password() {
        let input = "postgres://user:s3cretPass@host:5432/db";
        let out = redact(input);
        assert!(!out.contains("s3cretPass"), "leaked password: {out}");
        assert_eq!(out, "postgres://user:***@host:5432/db");
    }

    #[test]
    fn redact_vector10_authorization_basic_base64() {
        let input = "Authorization: Basic dXNlcjpwYXNz";
        let out = redact(input);
        assert!(!out.contains("dXNlcjpwYXNz"), "leaked credential: {out}");
        assert_eq!(out, "Authorization: Basic ***");
    }

    #[test]
    fn redact_vector11_github_pat_token() {
        let input = "ghp_16charsOrMoreAAAAAAAAAAAAAAAAAA";
        let out = redact(input);
        assert!(!out.contains("16charsOrMoreAAAAAAAAAAAAAAAAAA"));
        assert_eq!(out, "***");
    }

    #[test]
    fn redact_vector12_slack_bot_token() {
        let input = "xoxb-1234-5678-abcdEFGHijkl";
        let out = redact(input);
        assert!(!out.contains("1234-5678-abcdEFGHijkl"));
        assert_eq!(out, "***");
    }

    #[test]
    fn redact_vector13_google_api_key() {
        // 假密钥拆成两段拼接：整段字面量的形态与真实 Google API key 完全一致，会被
        // 代码托管平台的密钥扫描拦下；拼接后的运行时取值不变。
        let input = concat!("AIza", "SyA1234567890abcdefGHIJKLMNOPqrstuv");
        let out = redact(input);
        assert!(!out.contains("SyA1234567890abcdefGHIJKLMNOPqrstuv"));
        assert_eq!(out, "***");
    }

    // ---- redact: best-effort 保留（Minor，不得为了它们牺牲上面的召回率） ----
    // 这是本次修复要补的另一个真实缺陷：V1 的宽熵字符类含 `/`、`.`，会把整条
    // 路径/URL 拼成一个超长 run 一起误伤成 `***`。下面几个用例专门覆盖「路径/URL
    // 里混有数字」这个曾经触发误伤的形状（纯字母路径段本来就不会误伤）。

    #[test]
    fn redact_vector9_plain_path_survives() {
        let input = "/usr/local/bin/some-tool";
        assert_eq!(redact(input), input);
    }

    #[test]
    fn redact_vector10_plain_filename_survives() {
        let input = "opened report file notes.txt";
        assert_eq!(redact(input), input);
    }

    #[test]
    fn redact_vector11_version_string_survives() {
        let input = "version v1.2.3";
        assert_eq!(redact(input), input);
    }

    #[test]
    fn redact_preserves_path_with_digits_row_a() {
        let input = "/Users/alice/projects/app-v2/report-2024.txt";
        assert_eq!(redact(input), input);
    }

    #[test]
    fn redact_preserves_url_path_with_digits_row_b() {
        // 曾经的回归：V1 会把 "/v1/users/12345/profile" 整条拼成一个 run 误伤成
        // `***`，因为它的宽字符类包含 `/`。
        let input = "GET https://api.example.com/v1/users/12345/profile";
        assert_eq!(redact(input), input);
    }

    #[test]
    fn redact_preserves_json_path_with_digits_row_c() {
        let input = r#"{"path":"/Users/alice/workspace/build-2024/output.log"}"#;
        assert_eq!(redact(input), input);
    }

    #[test]
    fn redact_preserves_prose_with_numbers_row_f() {
        let input = "processed 42 items in 3 batches";
        assert_eq!(redact(input), input);
    }

    // ---- append -> query roundtrip ----

    #[test]
    fn record_then_query_roundtrip_is_newest_first_and_redacted() {
        let tmp = tempdir().unwrap();
        let layout = DataLayout::new(tmp.path().to_path_buf());

        record(&layout, "app-a", "fs.read", "{\"path\":\"a\"}", "allow").unwrap();
        record(
            &layout,
            "app-b",
            "net.fetch",
            "token=sk-abcdEFGH12345678wxyz",
            "deny",
        )
        .unwrap();
        record(&layout, "app-a", "fs.write", "{\"path\":\"b\"}", "allow").unwrap();

        let all = query(&layout, &AuditFilter::default());
        assert_eq!(all.len(), 3);
        // newest-first: 最后写入的（fs.write）应排最前
        assert_eq!(all[0].tool, "fs.write");
        assert_eq!(all[1].tool, "net.fetch");
        assert_eq!(all[2].tool, "fs.read");
        // 脱敏结果体现在 query 出来的 Entry 里
        assert_eq!(all[1].args, "token=***");

        // 原始落盘文件也不能包含明文密钥（redact 必须在写之前生效）
        let today = now_parts().0;
        let raw =
            std::fs::read_to_string(layout.audit_dir().join(format!("{today}.jsonl"))).unwrap();
        assert!(!raw.contains("sk-abcdEFGH12345678wxyz"));

        // 按 app_id 过滤
        let filtered = query(
            &layout,
            &AuditFilter {
                app_id: Some("app-a".into()),
                tool: None,
                limit: None,
            },
        );
        assert_eq!(filtered.len(), 2);
        assert!(filtered.iter().all(|e| e.app_id == "app-a"));

        // limit 截断（仍然 newest-first）
        let limited = query(
            &layout,
            &AuditFilter {
                app_id: None,
                tool: None,
                limit: Some(1),
            },
        );
        assert_eq!(limited.len(), 1);
        assert_eq!(limited[0].tool, "fs.write");
    }

    // ---- cross-file ordering（IMPORTANT：之前未覆盖）----

    #[test]
    fn query_orders_newest_first_across_day_files() {
        let tmp = tempdir().unwrap();
        let layout = DataLayout::new(tmp.path().to_path_buf());
        std::fs::create_dir_all(layout.audit_dir()).unwrap();

        let today = now_parts().0;
        let (yy, ym, yd) = civil_from_days(today_days_since_epoch() - 1);
        let yesterday = format!("{yy:04}-{ym:02}-{yd:02}");

        let mk = |tool: &str| Entry {
            ts: "irrelevant".into(),
            app_id: "app-x".into(),
            tool: tool.into(),
            args: "{}".into(),
            verdict: "allow".into(),
        };

        // 昨天的文件：按写入顺序 older-1 -> older-2（older-2 是当天内更晚写入的）。
        let older_content = format!(
            "{}\n{}\n",
            serde_json::to_string(&mk("older-1")).unwrap(),
            serde_json::to_string(&mk("older-2")).unwrap(),
        );
        std::fs::write(
            layout.audit_dir().join(format!("{yesterday}.jsonl")),
            older_content,
        )
        .unwrap();

        // 今天的文件：同样两行。
        let newer_content = format!(
            "{}\n{}\n",
            serde_json::to_string(&mk("newer-1")).unwrap(),
            serde_json::to_string(&mk("newer-2")).unwrap(),
        );
        std::fs::write(
            layout.audit_dir().join(format!("{today}.jsonl")),
            newer_content,
        )
        .unwrap();

        let all = query(&layout, &AuditFilter::default());
        let tools: Vec<&str> = all.iter().map(|e| e.tool.as_str()).collect();
        assert_eq!(
            tools,
            vec!["newer-2", "newer-1", "older-2", "older-1"],
            "跨文件必须严格按新到旧排序（先按文件名日期，同文件内再按行倒序）"
        );

        // limit 必须跨文件生效：3 应拿到「今天两条 + 昨天最新一条」，昨天更早一条不应出现。
        let limited = query(
            &layout,
            &AuditFilter {
                app_id: None,
                tool: None,
                limit: Some(3),
            },
        );
        let limited_tools: Vec<&str> = limited.iter().map(|e| e.tool.as_str()).collect();
        assert_eq!(limited_tools, vec!["newer-2", "newer-1", "older-2"]);
    }

    #[test]
    fn query_orders_newest_first_across_idx_rollover_same_day() {
        let tmp = tempdir().unwrap();
        let layout = DataLayout::new(tmp.path().to_path_buf());
        std::fs::create_dir_all(layout.audit_dir()).unwrap();

        let date = "2026-02-02";
        let mk = |tool: &str| Entry {
            ts: "irrelevant".into(),
            app_id: "app-x".into(),
            tool: tool.into(),
            args: "{}".into(),
            verdict: "allow".into(),
        };

        // base 文件（idx=0）先写满、滚号到 `.1.jsonl`（idx=1）——`.1` 里的内容按时间顺序更晚写入。
        std::fs::write(
            layout.audit_dir().join(format!("{date}.jsonl")),
            format!("{}\n", serde_json::to_string(&mk("base-1")).unwrap()),
        )
        .unwrap();
        std::fs::write(
            layout.audit_dir().join(format!("{date}.1.jsonl")),
            format!("{}\n", serde_json::to_string(&mk("rollover-1")).unwrap()),
        )
        .unwrap();

        let all = query(&layout, &AuditFilter::default());
        let tools: Vec<&str> = all.iter().map(|e| e.tool.as_str()).collect();
        assert_eq!(
            tools,
            vec!["rollover-1", "base-1"],
            "同一天内滚号文件（idx 更大）应排在 base 文件之前"
        );
    }

    // ---- retention ----

    #[test]
    fn prune_deletes_stale_files_keeps_fresh_by_filename_date() {
        let tmp = tempdir().unwrap();
        let layout = DataLayout::new(tmp.path().to_path_buf());
        std::fs::create_dir_all(layout.audit_dir()).unwrap();

        let stale_days = today_days_since_epoch() - AUDIT_RETENTION_DAYS as i64 - 5;
        let (sy, sm, sd) = civil_from_days(stale_days);
        let stale_name = format!("{sy:04}-{sm:02}-{sd:02}.jsonl");
        let fresh_name = format!("{}.jsonl", now_parts().0);

        std::fs::write(layout.audit_dir().join(&stale_name), "{}\n").unwrap();
        std::fs::write(layout.audit_dir().join(&fresh_name), "{}\n").unwrap();

        prune(&layout).unwrap();

        assert!(
            !layout.audit_dir().join(&stale_name).exists(),
            "stale file should be pruned"
        );
        assert!(
            layout.audit_dir().join(&fresh_name).exists(),
            "fresh file should survive prune"
        );
    }

    #[test]
    fn prune_keeps_file_exactly_at_retention_boundary() {
        let tmp = tempdir().unwrap();
        let layout = DataLayout::new(tmp.path().to_path_buf());
        std::fs::create_dir_all(layout.audit_dir()).unwrap();

        // 恰好 today - AUDIT_RETENTION_DAYS：cutoff 判断是严格 `<`，等于 cutoff 时应保留
        // （complementing 既有的 35 天过期用例，这里补的是精确边界）。
        let boundary_days = today_days_since_epoch() - AUDIT_RETENTION_DAYS as i64;
        let (by, bm, bd) = civil_from_days(boundary_days);
        let boundary_name = format!("{by:04}-{bm:02}-{bd:02}.jsonl");
        std::fs::write(layout.audit_dir().join(&boundary_name), "{}\n").unwrap();

        prune(&layout).unwrap();

        assert!(
            layout.audit_dir().join(&boundary_name).exists(),
            "file exactly at retention boundary (today - AUDIT_RETENTION_DAYS) must be kept (strict < cutoff)"
        );
    }

    // ---- size cap rollover ----

    #[test]
    fn target_file_rolls_to_numbered_sibling_when_base_exceeds_cap() {
        let tmp = tempdir().unwrap();
        let dir = tmp.path();
        let date = "2026-01-01";
        std::fs::write(dir.join(format!("{date}.jsonl")), vec![0u8; 20]).unwrap();

        let target = target_file_for_day_with_cap(dir, date, 10);
        assert_eq!(target, dir.join(format!("{date}.1.jsonl")));
    }

    #[test]
    fn target_file_rolls_past_full_numbered_sibling_too() {
        let tmp = tempdir().unwrap();
        let dir = tmp.path();
        let date = "2026-01-01";
        std::fs::write(dir.join(format!("{date}.jsonl")), vec![0u8; 20]).unwrap();
        std::fs::write(dir.join(format!("{date}.1.jsonl")), vec![0u8; 20]).unwrap();

        let target = target_file_for_day_with_cap(dir, date, 10);
        assert_eq!(target, dir.join(format!("{date}.2.jsonl")));
    }

    #[test]
    fn target_file_stays_on_base_when_under_cap() {
        let tmp = tempdir().unwrap();
        let dir = tmp.path();
        let date = "2026-01-01";
        std::fs::write(dir.join(format!("{date}.jsonl")), vec![0u8; 5]).unwrap();

        let target = target_file_for_day_with_cap(dir, date, 10);
        assert_eq!(target, dir.join(format!("{date}.jsonl")));
    }

    // ---- sanity: date algorithm + named consts ----

    #[test]
    fn civil_from_days_epoch_is_1970_01_01() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
    }

    #[test]
    fn named_consts_match_spec() {
        assert_eq!(AUDIT_RETENTION_DAYS, 30);
        assert_eq!(AUDIT_MAX_FILE_BYTES, 5 * 1024 * 1024);
    }
}
