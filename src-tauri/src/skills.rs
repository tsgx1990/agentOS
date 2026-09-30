//! P6-B：Agent Skills 标准 `SKILL.md` 的解析、名称/描述校验、危险模式静态扫描
//! （Task 1，纯函数层），以及技能库持久化 `SkillStore`（Task 2：目录 + 清单/授予表
//! + 安装门）。
//!
//! 规则抄自 `docs/research/2026-09-02-agent-skills-rules.md`（移植自
//! `agentskills/agentskills` 的 Python 参考实现 `skills-ref`，Apache-2.0）——标准
//! 只有 `name` 校验里要求"与所在目录名一致"这一条我方**刻意不抄**：
//! spec 裁决 1（`docs/superpowers/specs/2026-09-02-p6b-skills-subsystem-design.md`
//! §9）明确"技能 id 用 frontmatter `name`，不强制目录同名"，与 pi 自己的行为一致，
//! `validate_name` 因此只校验 `name` 自身，不比对目录名。
//!
//! 与 Interfaces 文档（计划 Task 1）的一处必要偏差：`Finding.rule` 那里写的是
//! `&'static str`，但 `Finding`/`ScanReport` 要经 `InstalledSkill.scan` 整份序列化进
//! `skills-index.json` 再读回来——`#[derive(Deserialize)]` 没法把 JSON 里的字符串
//! 变成一个 `&'static str`（那需要要么泄漏内存要么绑定输入的生命周期，两者都跟
//! "读回持久化文件里独立存活的数据"这个要求冲突）。这里改成 `String`（拥有型），
//! 危险模式表本身仍然用 `&'static str` 命名规则（零额外分配），只在装进 `Finding`
//! 时 `.to_string()` 一次。

use crate::approvals::fresh_id;
use crate::paths::DataLayout;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

// ---------------------------------------------------------------------------
// Task 1：解析 / 校验 / 扫描（纯函数层）
// ---------------------------------------------------------------------------

pub const MAX_SKILL_NAME_LEN: usize = 64;
pub const MAX_DESCRIPTION_LEN: usize = 1024;
pub const MAX_COMPAT_LEN: usize = 500;

/// SKILL.md frontmatter 允许的字段——多余字段视为错误。最后一项
/// `disable-model-invocation` 是 pi 的扩展字段（标准之外，但 pi 认，我方也放行）。
pub const ALLOWED_FRONTMATTER_FIELDS: &[&str] = &[
    "name",
    "description",
    "license",
    "allowed-tools",
    "metadata",
    "compatibility",
    "disable-model-invocation",
];

/// 单个技能的元数据（frontmatter 解析结果 + 目录扫描得出的 `has_scripts`）。
/// `id` 与 `name` 目前总是同一个值（NFKC 归一后的 name，spec 裁决 1）——保留两个
/// 字段是因为将来"同名不同来源以后缀区分 name@source"时两者会分叉。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SkillMeta {
    pub id: String,
    pub name: String,
    pub description: String,
    pub license: Option<String>,
    pub compatibility: Option<String>,
    pub allowed_tools: Vec<String>,
    pub disable_model_invocation: bool,
    pub has_scripts: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    High,
    Medium,
}

/// 一条危险模式命中记录。`rule` 是规则的短名（见 `dangerous_patterns`），`file` 是
/// 相对技能目录根的路径（`/` 分隔），`line` 是 1-based 行号，`excerpt` 是命中的原文
/// 片段（用于 evil-skill 安装确认弹窗高亮，Task 8）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Finding {
    pub severity: Severity,
    pub rule: String,
    pub file: String,
    pub line: usize,
    pub excerpt: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct ScanReport {
    pub has_scripts: bool,
    pub script_files: Vec<String>,
    pub findings: Vec<Finding>,
    pub total_bytes: u64,
    pub file_count: usize,
}

pub const MAX_SKILL_BYTES: u64 = 5 * 1024 * 1024;
pub const MAX_SKILL_FILES: usize = 200;
/// 单文件大小上限（审查修复轮 1 Important）：技能是指令文本，不该有超过 1 MiB
/// 的单个文件。与 `MAX_SKILL_BYTES`（目录总大小）分开判——`scan_skill_dir` 据此
/// 在 `metadata().len()` 阶段就能拒绝一个畸大的单文件，不必等累计到目录总量。
pub const MAX_SKILL_FILE_BYTES: u64 = 1024 * 1024;

/// 在 `dir` 下找入口文件：`SKILL.md` 优先，其次 `skill.md`；都没有则 `None`。
pub fn find_skill_md(dir: &Path) -> Option<PathBuf> {
    let upper = dir.join("SKILL.md");
    if upper.is_file() {
        return Some(upper);
    }
    let lower = dir.join("skill.md");
    if lower.is_file() {
        return Some(lower);
    }
    None
}

/// 解析 `SKILL.md`：gray_matter 解 YAML frontmatter，校验允许字段/必填字段/长度，
/// 返回 `(SkillMeta, body)`。返回的 `SkillMeta.has_scripts` 恒为 `false`
/// 占位——**不在这里扫目录**（审查修复轮 1 Minor：此前的实现在这里为算
/// `has_scripts` 对 `path` 所在目录跑一次 `scan_skill_dir`，而唯一的生产调用方
/// `preflight` 紧接着自己又要跑一次 `scan_skill_dir` 做大小/符号链接/危险模式
/// 校验，两次扫描结果里只有后一次的 `has_scripts` 真正被用上——同一个目录被
/// 完整遍历两遍，前一遍白扫）。真正的值由调用方在自己那次 `scan_skill_dir`
/// 拿到 `ScanReport` 后据 `report.has_scripts` 回填（见 `preflight`）。
///
/// **终审 Minor 5 修复**：读文件前先用 `symlink_metadata`（不跟随符号链接，
/// 同 `collect_files` 判定手法）拒绝 `path` 本身是符号链接的情况。此前的调用
/// 顺序是 `find_skill_md`（`is_file()` **跟随**符号链接）→ 直接
/// `read_to_string`（同样跟随）→ …… → `scan_skill_dir`（唯一会拒绝符号链接
/// 的地方）——一个 `SKILL.md -> /etc/passwd` 之类的符号链接会先被整个读进
/// 内存，`gray_matter` 的 YAML 解析错误信息还可能把目标文件的内容片段带进
/// `Err` 字符串、经调用方回显到前端。安装最终仍会失败（`scan_skill_dir`
/// fail-closed），所以此前只是信息面问题，不是安装门被绕过；这里把校验挪到
/// 读文件之前，不多花一次目录遍历的成本（`scan_skill_dir` 本来就要跑）。
pub fn parse_skill_md(path: &Path) -> Result<(SkillMeta, String), String> {
    let file_type = std::fs::symlink_metadata(path)
        .map_err(|e| format!("读取 {} 元数据失败：{e}", path.display()))?
        .file_type();
    if file_type.is_symlink() {
        return Err(format!("拒绝符号链接：{}", path.display()));
    }

    let raw =
        std::fs::read_to_string(path).map_err(|e| format!("读取 {} 失败：{e}", path.display()))?;

    let matter = gray_matter::Matter::<gray_matter::engine::YAML>::new();
    let parsed = matter
        .parse::<gray_matter::Pod>(&raw)
        .map_err(|e| format!("SKILL.md frontmatter 不是合法 YAML：{e}"))?;

    let Some(data) = parsed.data else {
        return Err("SKILL.md 缺少 YAML frontmatter（--- 包裹的 YAML 块）".to_string());
    };
    let gray_matter::Pod::Hash(map) = data else {
        return Err("SKILL.md frontmatter 必须是 YAML 映射（key: value）".to_string());
    };

    for key in map.keys() {
        if !ALLOWED_FRONTMATTER_FIELDS.contains(&key.as_str()) {
            return Err(format!("SKILL.md frontmatter 含标准之外的字段：{key}"));
        }
    }

    let name = match map.get("name") {
        Some(gray_matter::Pod::String(s)) if !s.is_empty() => s.clone(),
        Some(_) => return Err("SKILL.md frontmatter 的 name 必须是非空字符串".to_string()),
        None => return Err("SKILL.md frontmatter 缺少必填字段 name".to_string()),
    };

    let description = match map.get("description") {
        Some(gray_matter::Pod::String(s)) if !s.is_empty() => s.clone(),
        Some(_) => return Err("SKILL.md frontmatter 的 description 必须是非空字符串".to_string()),
        None => return Err("SKILL.md frontmatter 缺少必填字段 description".to_string()),
    };
    if description.chars().count() > MAX_DESCRIPTION_LEN {
        return Err(format!(
            "SKILL.md frontmatter 的 description 长度超过 {MAX_DESCRIPTION_LEN}"
        ));
    }

    let license = match map.get("license") {
        Some(gray_matter::Pod::String(s)) => Some(s.clone()),
        Some(_) => return Err("SKILL.md frontmatter 的 license 必须是字符串".to_string()),
        None => None,
    };

    let compatibility = match map.get("compatibility") {
        Some(gray_matter::Pod::String(s)) => {
            if s.chars().count() > MAX_COMPAT_LEN {
                return Err(format!(
                    "SKILL.md frontmatter 的 compatibility 长度超过 {MAX_COMPAT_LEN}"
                ));
            }
            Some(s.clone())
        }
        Some(_) => return Err("SKILL.md frontmatter 的 compatibility 必须是字符串".to_string()),
        None => None,
    };

    // 终审 Minor 6：`allowed-tools` 同时接受空格分隔的字符串（标准原始写法）
    // 与 YAML 数组两种形态——此前只认前者，Agent Skills 生态里常见的数组写法
    // （如 `allowed-tools: [Bash, Read]`）会被拒（`Some(_)` 分支直接报"必须是
    // 空格分隔的字符串"），fail-closed、不构成绕过，但会拒掉一批本来合法的
    // 第三方技能。数组元素必须逐个是字符串，混了别的类型（数字/布尔/嵌套结构）
    // 仍然拒绝——不做"尽力转成字符串"这种静默纠错。
    let allowed_tools = match map.get("allowed-tools") {
        Some(gray_matter::Pod::String(s)) => s.split_whitespace().map(|t| t.to_string()).collect(),
        Some(gray_matter::Pod::Array(items)) => {
            let mut tools = Vec::with_capacity(items.len());
            for item in items {
                match item {
                    gray_matter::Pod::String(s) => tools.push(s.clone()),
                    _ => {
                        return Err(
                            "SKILL.md frontmatter 的 allowed-tools 数组元素必须都是字符串"
                                .to_string(),
                        )
                    }
                }
            }
            tools
        }
        Some(_) => {
            return Err(
                "SKILL.md frontmatter 的 allowed-tools 必须是空格分隔的字符串或字符串数组"
                    .to_string(),
            )
        }
        None => Vec::new(),
    };

    let disable_model_invocation = match map.get("disable-model-invocation") {
        Some(gray_matter::Pod::Boolean(b)) => *b,
        Some(_) => {
            return Err("SKILL.md frontmatter 的 disable-model-invocation 必须是布尔值".to_string())
        }
        None => false,
    };

    let meta = SkillMeta {
        id: name.clone(),
        name,
        description,
        license,
        compatibility,
        allowed_tools,
        disable_model_invocation,
        has_scripts: false, // 占位，调用方（preflight）据自己那次 scan_skill_dir 回填
    };

    Ok((meta, parsed.content))
}

/// 校验并 NFKC 归一化技能名：≤64 字符、全小写、不以 `-` 开头/结尾、无连续 `--`、
/// 只允许 **ASCII** 小写字母（a-z）、数字（0-9）与 `-`。返回归一化后的字符串。
///
/// **终审 Minor 4 修复**：字符集检查此前是 `c.is_alphanumeric()`（任意
/// Unicode 字母/数字都放行），但 Agent Skills 标准原文的字符集就是
/// ASCII `a-z`、`0-9`、`-`（见模块文档"规则抄自
/// `docs/research/2026-09-02-agent-skills-rules.md`"）——放宽到 Unicode 会
/// 让同形字符（confusable，如西里尔字母 о U+043E 与拉丁字母 o 肉眼无法区分）
/// 冒充一个已装/内置技能的 id：NFKC **不会**把西里尔字母折叠成拉丁字母
/// （两者是不同书写系统，不存在"兼容分解"关系），所以
/// `c\u{043E}nnector-etiquette`（中间那个 "o" 是西里尔字母）此前能通过
/// `validate_name`，得到一个与内置技能 `connector-etiquette` id 不相等
/// （因此不触发 `Duplicate` 拒装）、但在技能列表/授予多选框里肉眼完全无法
/// 区分的第二个技能——不是提权（安装门其它防线都还在），但是真实的社工面。
/// 不构成路径穿越：ASCII 字符集本身就不含 `/`、`\`、`.`、`:`，比放宽前更严格
/// 而不是更宽松。
pub fn validate_name(name: &str) -> Result<String, String> {
    if name.is_empty() {
        return Err("name 不能为空".to_string());
    }

    let normalized = icu_normalizer::ComposingNormalizer::new_nfkc()
        .normalize(name)
        .into_owned();

    if normalized.chars().count() > MAX_SKILL_NAME_LEN {
        return Err(format!(
            "name 长度超过 {MAX_SKILL_NAME_LEN}（NFKC 归一后）：{normalized}"
        ));
    }
    if normalized.to_lowercase() != normalized {
        return Err(format!("name 必须全小写：{normalized}"));
    }
    if normalized.starts_with('-') || normalized.ends_with('-') {
        return Err(format!("name 不能以 - 开头或结尾：{normalized}"));
    }
    if normalized.contains("--") {
        return Err(format!("name 不能包含连续的 --：{normalized}"));
    }
    if !normalized
        .chars()
        .all(|c| c == '-' || c.is_ascii_lowercase() || c.is_ascii_digit())
    {
        return Err(format!(
            "name 只允许 ASCII 小写字母（a-z）、数字（0-9）与 -（Agent Skills 标准字符集，\
             不含任何非 ASCII 字母——防同形字符冒充已装技能 id）：{normalized}"
        ));
    }

    Ok(normalized)
}

/// 判断相对路径 `rel`（`/` 分隔，相对技能目录根）是否算"脚本文件"：位于
/// `scripts/` 下，或者扩展名是 `.sh`/`.py`/`.js`/`.ts`/`.rb`。
fn is_script_path(rel: &str) -> bool {
    if rel.starts_with("scripts/") {
        return true;
    }
    let lower = rel.to_ascii_lowercase();
    [".sh", ".py", ".js", ".ts", ".rb"]
        .iter()
        .any(|ext| lower.ends_with(ext))
}

/// 危险模式表（≤30 条，逐条 `(规则名, 严重度, 正则)`）。规则表是本项目自建的，
/// 不是从第三方扫描器移植的
/// （`secret_env_read` 那条因为 `|` 优先级看起来"怪"的写法是有意保留的，不要"修正"）。
/// `OnceLock` 保证每条正则只编译一次，供 `scan_skill_dir` 反复调用。
fn dangerous_patterns() -> &'static [(&'static str, Severity, regex::Regex)] {
    static PATTERNS: std::sync::OnceLock<Vec<(&'static str, Severity, regex::Regex)>> =
        std::sync::OnceLock::new();
    PATTERNS.get_or_init(|| {
        vec![
            // High
            (
                "curl_pipe_shell",
                Severity::High,
                regex::Regex::new(r"curl[^|\n]*\|\s*(ba)?sh").unwrap(),
            ),
            (
                "wget_pipe_shell",
                Severity::High,
                regex::Regex::new(r"wget[^|\n]*\|\s*(ba)?sh").unwrap(),
            ),
            (
                "base64_decode",
                Severity::High,
                regex::Regex::new(r"base64\s+(-d|--decode)").unwrap(),
            ),
            (
                "rm_rf_sensitive",
                Severity::High,
                regex::Regex::new(r"rm\s+-rf\s+(/|~|\$HOME)").unwrap(),
            ),
            (
                "ssh_dir_access",
                Severity::High,
                regex::Regex::new(r"~/\.ssh|\$HOME/\.ssh").unwrap(),
            ),
            (
                "aws_creds_access",
                Severity::High,
                regex::Regex::new(r"~/\.aws|\.aws/credentials").unwrap(),
            ),
            (
                "keychain_dump",
                Severity::High,
                regex::Regex::new(r"security\s+find-generic-password").unwrap(),
            ),
            (
                "sandbox_exec_bin",
                Severity::High,
                regex::Regex::new(r"/usr/bin/sandbox-exec").unwrap(),
            ),
            (
                "launchctl",
                Severity::High,
                regex::Regex::new(r"launchctl").unwrap(),
            ),
            (
                "crontab",
                Severity::High,
                regex::Regex::new(r"crontab").unwrap(),
            ),
            (
                "reverse_shell",
                Severity::High,
                regex::Regex::new(r"nc\s+-e|/dev/tcp/").unwrap(),
            ),
            // Medium
            (
                "secret_env_read",
                Severity::Medium,
                regex::Regex::new(r"[A-Z_]*API_KEY|SECRET|TOKEN").unwrap(),
            ),
            (
                "external_url",
                Severity::Medium,
                regex::Regex::new(r"https?://[^\s)]+").unwrap(),
            ),
            (
                "sudo",
                Severity::Medium,
                regex::Regex::new(r"sudo\s").unwrap(),
            ),
            (
                "chmod_exec",
                Severity::Medium,
                regex::Regex::new(r"chmod\s+\+x").unwrap(),
            ),
            (
                "eval",
                Severity::Medium,
                regex::Regex::new(r"eval\s").unwrap(),
            ),
            (
                "python_dash_c",
                Severity::Medium,
                regex::Regex::new(r"python[23]?\s+-c").unwrap(),
            ),
            (
                "osascript",
                Severity::Medium,
                regex::Regex::new(r"osascript").unwrap(),
            ),
        ]
    })
}

/// 递归收集 `dir` 下所有普通文件的绝对路径；碰到符号链接立即 `Err`（安装门
/// fail-closed：宁可拒装，也不静默跳过一个可能用来逃逸的链接）。
fn collect_files(dir: &Path, root: &Path, out: &mut Vec<PathBuf>) -> Result<(), String> {
    let entries =
        std::fs::read_dir(dir).map_err(|e| format!("读取目录 {} 失败：{e}", dir.display()))?;
    for entry in entries {
        let entry = entry.map_err(|e| e.to_string())?;
        let ft = entry
            .file_type()
            .map_err(|e| format!("读取 {} 的文件类型失败：{e}", entry.path().display()))?;
        let path = entry.path();
        if ft.is_symlink() {
            let rel = path.strip_prefix(root).unwrap_or(&path);
            return Err(format!("拒绝符号链接：{}", rel.display()));
        }
        if ft.is_dir() {
            collect_files(&path, root, out)?;
        } else if ft.is_file() {
            out.push(path);
        }
    }
    Ok(())
}

/// 递归扫描技能目录：统计大小/文件数、识别脚本文件、跑危险模式表。任一文件是
/// 符号链接、总大小超过 `MAX_SKILL_BYTES`、单文件超过 `MAX_SKILL_FILE_BYTES`、
/// 文件数超过 `MAX_SKILL_FILES` 都返回 `Err`（安装门据此直接拒装，不落半成品）。
///
/// **先限流再整读（审查修复轮 1 Important）**：分两遍扫描，中间以文件数/大小
/// 检查为界——第一遍只 `metadata().len()`，边读边累加边判断，一超限立即
/// `Err`、**绝不 `read_to_string`** 任何文件；只有全部文件都在限内，才会进入
/// 第二遍去真正整读内容跑危险模式表。此前的实现是单遍循环：先把每个文件整个
/// 读进内存做内容扫描，扫完全部文件后才检查总大小/文件数是否超限——一个精心
/// 构造的超大恶意技能目录能在被拒装前先把宿主进程的内存/CPU 耗光，安装门的
/// "先判后动"防线被架空。
pub fn scan_skill_dir(dir: &Path) -> Result<ScanReport, String> {
    let mut files = Vec::new();
    collect_files(dir, dir, &mut files)?;

    if files.len() > MAX_SKILL_FILES {
        return Err(format!(
            "技能目录文件数 {} 超过上限 {MAX_SKILL_FILES}",
            files.len()
        ));
    }

    let mut report = ScanReport {
        file_count: files.len(),
        ..Default::default()
    };

    // 第一遍：只读元数据算大小、边读边判——超限（单文件或累计）立即 `Err`，
    // 这一遍任何文件的内容都不会被读进内存。
    for path in &files {
        let file_meta = std::fs::metadata(path)
            .map_err(|e| format!("读取 {} 元数据失败：{e}", path.display()))?;
        let len = file_meta.len();
        if len > MAX_SKILL_FILE_BYTES {
            let rel = path
                .strip_prefix(dir)
                .unwrap_or(path)
                .to_string_lossy()
                .replace('\\', "/");
            return Err(format!(
                "文件 {rel} 大小 {len} 字节超过单文件上限 {MAX_SKILL_FILE_BYTES}"
            ));
        }
        report.total_bytes += len;
        if report.total_bytes > MAX_SKILL_BYTES {
            return Err(format!(
                "技能目录大小 {} 字节超过上限 {MAX_SKILL_BYTES}",
                report.total_bytes
            ));
        }
    }

    // 第二遍：走到这里说明文件数/大小全部在限内，才真正整读内容做危险模式扫描。
    let patterns = dangerous_patterns();
    let mut seen_urls: std::collections::HashSet<String> = std::collections::HashSet::new();

    for path in &files {
        let rel = path
            .strip_prefix(dir)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/");

        if is_script_path(&rel) {
            report.has_scripts = true;
            report.script_files.push(rel.clone());
        }

        // 二进制/非 UTF-8 文件跳过内容扫描（仍计入大小/文件数），不是错误。
        if let Ok(content) = std::fs::read_to_string(path) {
            for (line_idx, line) in content.lines().enumerate() {
                for (rule, severity, re) in patterns {
                    for m in re.find_iter(line) {
                        let excerpt = m.as_str().to_string();
                        if *rule == "external_url" && !seen_urls.insert(excerpt.clone()) {
                            continue; // 同一个外联 URL 只记一次（"去重列出"）
                        }
                        report.findings.push(Finding {
                            severity: severity.clone(),
                            rule: (*rule).to_string(),
                            file: rel.clone(),
                            line: line_idx + 1,
                            excerpt,
                        });
                    }
                }
            }
        }
    }

    Ok(report)
}

// ---------------------------------------------------------------------------
// Task 2：SkillStore —— 技能库、清单/授予表持久化、安装门
// ---------------------------------------------------------------------------

/// 一个已装技能的来源。`Local`/`Builtin` 一般没有 `url`/`sha256`；`Market` 走 HTTP
/// 拉取（Task 6，本批次不实现下载逻辑，字段先留出）；`Maker` 是宿主自己生成的
/// 产物（Task 7）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum SkillSourceKind {
    Local,
    Builtin,
    Market,
    Maker,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SkillSource {
    pub kind: SkillSourceKind,
    pub url: Option<String>,
    pub sha256: Option<String>,
}

/// 一条已装技能记录——`.superagent-skill.json` 的内存表示（实际落盘位置是整份
/// `skills-index.json` 里的 `skills` 数组，见 `SkillsIndex`）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct InstalledSkill {
    pub meta: SkillMeta,
    pub source: SkillSource,
    pub trusted: bool,
    pub installed_at: i64,
    pub scan: ScanReport,
}

/// 一条"某应用被授予某技能"记录；`enabled` 是启停开关（撤销授予是删除整条记录，
/// 禁用只是把这个字段置 false，两者语义不同——见 `revoke` 与 `set_enabled`）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SkillGrant {
    pub skill_id: String,
    pub enabled: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum SkillInstallError {
    #[error("{0}")]
    Invalid(String),
    #[error("技能声明的工具 {0:?} 不在宿主已知工具集内")]
    UnknownTools(Vec<String>),
    #[error("技能需要工具 {0:?}，目标应用未获得")]
    ToolsNotSubset(Vec<String>),
    #[error("不受信来源出现高危内容：{0}")]
    HighRiskUntrusted(String),
    #[error("已存在同名技能 {0}，请改名后再装")]
    Duplicate(String),
    #[error("{0}")]
    Io(String),
}

/// 一条待确认的 Maker 生成技能安装（Task7，P6-B spec §5 末段）：
/// `__host_maker_install_skill__`（`maker.rs::handle_install_skill`）在
/// `preflight` 通过后登记在这里，持久化在同一份 `skills-index.json`
/// 里——批次 D 简报明确要求这条 pending 表必须**持久化**，不能只放内存：
/// 不同于 `mcp.rs::McpManager::pending_installs`（应用安装确认，进程内存表，
/// 重启即丢，P4 时代遗留、尚未随 P6-C 一起迁移，不在本任务范围内），技能安装
/// 确认没有理由比应用安装确认更脆弱，而 `SkillStore` 已经有"整份 JSON + 原子
/// 写"的落盘基础设施（`load_index`/`save_index`），复用它登记这第三张表成本
/// 几乎为零。`draft_dir` 是唯一不对前端暴露的字段（内部路径，见
/// `maker::PendingSkillInstallView` 只透出 `confirm_id`/`meta`/`scan`）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PendingSkillInstall {
    pub confirm_id: String,
    pub draft_dir: PathBuf,
    pub meta: SkillMeta,
    pub scan: ScanReport,
}

/// 待确认技能安装 id 的生成前缀：`"skill-confirm-<纳秒时间戳>-<计数器>"`
/// （`approvals::fresh_id`，见其文档）——与 `mcp.rs::McpManager::confirm_counter`
/// （应用安装确认，格式 `"confirm-{n}"`）故意用不同前缀区分开，两套确认体系
/// 的 id 空间不应被误认成同一种。
///
/// **终审 C1 修复**：此前这里是裸 `static AtomicU64`（`format!("skill-confirm-{n}")`，
/// 不含任何时间戳分量）——但 pending 表本身持久化在 `skills-index.json`
/// 里，生存期跨进程重启；进程每次重启这个计数器都归零，两次不同的进程运行
/// 各自 mint 出的第一个 `confirm_id` 会是同一个字符串
/// `"skill-confirm-0"`，若此时磁盘上还留着上一个进程未被处理的 pending
/// 记录，`skills-index.json` 里就会出现两条 `confirm_id` 相同的记录——
/// `take_pending_skill_install` 按 `position(...)` 取第一条匹配，用户在
/// 确认框里看到 B 的 `meta`/`scan` 点"允许"，实际却把 A 装了进去（"同意框
/// 双向脱节"的又一例）。改用 `fresh_id`（纳秒时间戳 + 计数器）后，id 本身
/// 的生存期不再依赖进程存活，与它标识的持久化数据的生存期匹配。
const PENDING_SKILL_INSTALL_ID_PREFIX: &str = "skill-confirm";

/// `skills-index.json` 的整份内容（spec §3）：已装技能清单 + 各应用的授予表 +
/// 待确认的 Maker 生成技能安装（Task7）。内部实现细节，不对外导出——外部只
/// 通过 `SkillStore` 的方法读写。
#[derive(Debug, Serialize, Deserialize, Default)]
struct SkillsIndex {
    #[serde(default)]
    skills: Vec<InstalledSkill>,
    #[serde(default)]
    grants: HashMap<String, Vec<SkillGrant>>,
    #[serde(default)]
    pending_skill_installs: Vec<PendingSkillInstall>,
}

/// 进程内单把互斥锁：串行化对 `skills-index.json` 与 `skills/` 目录树的所有读改
/// 写——同 `approvals.rs::MUTEX` 的哲学（本模块调用量级不需要更细的分片）。
static MUTEX: Mutex<()> = Mutex::new(());

/// 技能库：`<root>/skills/<id>/` 目录树 + `<root>/skills-index.json` 清单/授予表。
/// 本身不缓存任何数据（无内存态、现读现写），`layout` 是唯一字段。
pub struct SkillStore {
    layout: DataLayout,
}

impl SkillStore {
    pub fn new(layout: DataLayout) -> Self {
        Self { layout }
    }

    /// 读整份 `skills-index.json`：文件不存在视为空清单（首次运行，不是错误）；
    /// 存在但读取/解析失败才是真错误。不加锁——调用方在自己的公开方法里已经持有
    /// `MUTEX`（同 `approvals.rs::load_staged` 的分工）。
    fn load_index(&self) -> Result<SkillsIndex, String> {
        let path = self.layout.skills_index_path();
        match std::fs::read_to_string(&path) {
            Ok(s) => {
                serde_json::from_str(&s).map_err(|e| format!("{} 解析失败：{e}", path.display()))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(SkillsIndex::default()),
            Err(e) => Err(format!("{} 读取失败：{e}", path.display())),
        }
    }

    /// 原子写：先写 `.json.tmp` 再 `rename`，同 `approvals.rs::save_json_file`。
    fn save_index(&self, idx: &SkillsIndex) -> Result<(), String> {
        let path = self.layout.skills_index_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let tmp = path.with_extension("json.tmp");
        let body = serde_json::to_string_pretty(idx).map_err(|e| e.to_string())?;
        std::fs::write(&tmp, body).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, path).map_err(|e| e.to_string())
    }

    /// 安装门（不落盘）：`find_skill_md` → `parse_skill_md` → `validate_name` →
    /// `scan_skill_dir`（大小/数量/符号链接）→ `allowed_tools ⊆ known_tools` →
    /// `trusted=false` 且命中 High → 拒装 → 重名检查。任一步失败即 `Err`，前面
    /// 的步骤都只是读，没有任何写操作。
    pub fn preflight(
        &self,
        dir: &Path,
        trusted: bool,
        known_tools: &[String],
    ) -> Result<(SkillMeta, ScanReport), SkillInstallError> {
        let md_path = find_skill_md(dir).ok_or_else(|| {
            SkillInstallError::Invalid(format!("{} 下未找到 SKILL.md", dir.display()))
        })?;
        let (mut meta, _body) = parse_skill_md(&md_path).map_err(SkillInstallError::Invalid)?;

        let normalized = validate_name(&meta.name).map_err(SkillInstallError::Invalid)?;
        meta.id = normalized.clone();
        meta.name = normalized;

        let scan = scan_skill_dir(dir).map_err(SkillInstallError::Invalid)?;
        meta.has_scripts = scan.has_scripts;

        let unknown: Vec<String> = meta
            .allowed_tools
            .iter()
            .filter(|t| !known_tools.iter().any(|k| k == *t))
            .cloned()
            .collect();
        if !unknown.is_empty() {
            return Err(SkillInstallError::UnknownTools(unknown));
        }

        if !trusted {
            if let Some(f) = scan.findings.iter().find(|f| f.severity == Severity::High) {
                return Err(SkillInstallError::HighRiskUntrusted(format!(
                    "{}（{}:{}）：{}",
                    f.rule, f.file, f.line, f.excerpt
                )));
            }
        }

        let _guard = MUTEX.lock().expect("SkillStore mutex poisoned");
        let idx = self.load_index().map_err(SkillInstallError::Io)?;
        if idx.skills.iter().any(|s| s.meta.id == meta.id) {
            return Err(SkillInstallError::Duplicate(meta.id.clone()));
        }

        Ok((meta, scan))
    }

    /// `preflight` 之后落盘：把 `dir` 拷到一个 staging 目录、`rename` 到
    /// `skills/<id>/`、写入索引；任一步失败都清理掉已拷贝的 staging/目标目录，
    /// 保证失败路径下 `skills/` 里不留半成品（spec §8 不变量）。
    pub fn install_from_dir(
        &self,
        dir: &Path,
        source: SkillSource,
        trusted: bool,
        known_tools: &[String],
        now: i64,
    ) -> Result<InstalledSkill, SkillInstallError> {
        let (meta, scan) = self.preflight(dir, trusted, known_tools)?;

        let _guard = MUTEX.lock().expect("SkillStore mutex poisoned");
        let mut idx = self.load_index().map_err(SkillInstallError::Io)?;
        if idx.skills.iter().any(|s| s.meta.id == meta.id) {
            // preflight 里已经查过一次；这里在持锁状态下再查一遍，堵住"两次
            // preflight 并发通过、只有一次能真正写"的竞态窗口。
            return Err(SkillInstallError::Duplicate(meta.id.clone()));
        }

        let skills_root = self.layout.skills_root();
        std::fs::create_dir_all(&skills_root).map_err(|e| SkillInstallError::Io(e.to_string()))?;
        let dest = self.layout.skill_dir(&meta.id);
        let staging = skills_root.join(format!(".staging-{}-{now}", meta.id));
        let _ = std::fs::remove_dir_all(&staging); // 清掉可能残留的上次失败产物

        if let Err(e) = crate::install::copy_dir(dir, &staging) {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(SkillInstallError::Io(e.to_string()));
        }
        if let Err(e) = std::fs::rename(&staging, &dest) {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(SkillInstallError::Io(e.to_string()));
        }

        let installed = InstalledSkill {
            meta,
            source,
            trusted,
            installed_at: now,
            scan,
        };
        idx.skills.push(installed.clone());
        if let Err(e) = self.save_index(&idx) {
            let _ = std::fs::remove_dir_all(&dest);
            return Err(SkillInstallError::Io(e));
        }

        Ok(installed)
    }

    /// 卸载：从清单删除该技能、清空所有应用名下对它的授予、删掉磁盘目录。幂等
    /// （技能本就不存在时返回 `Ok(false)`，不报错）。
    pub fn uninstall(&self, id: &str) -> Result<bool, String> {
        let _guard = MUTEX.lock().expect("SkillStore mutex poisoned");
        let mut idx = self.load_index()?;
        let before = idx.skills.len();
        idx.skills.retain(|s| s.meta.id != id);
        let removed = idx.skills.len() != before;
        if removed {
            for grants in idx.grants.values_mut() {
                grants.retain(|g| g.skill_id != id);
            }
            self.save_index(&idx)?;
            let dir = self.layout.skill_dir(id);
            if dir.exists() {
                std::fs::remove_dir_all(&dir).map_err(|e| e.to_string())?;
            }
        }
        Ok(removed)
    }

    pub fn list(&self) -> Result<Vec<InstalledSkill>, String> {
        let _guard = MUTEX.lock().expect("SkillStore mutex poisoned");
        Ok(self.load_index()?.skills)
    }

    /// 授予：`skill.meta.allowed_tools ⊆ app_tools` 不满足则 `Err(ToolsNotSubset)`，
    /// 技能本身未安装则 `Err(Invalid)`。重复授予同一 `(app_id, skill_id)` 是幂等
    /// 的——直接把已有记录的 `enabled` 置 `true`，不会出现重复条目。
    pub fn grant(
        &self,
        app_id: &str,
        skill_id: &str,
        app_tools: &[String],
    ) -> Result<(), SkillInstallError> {
        let _guard = MUTEX.lock().expect("SkillStore mutex poisoned");
        let mut idx = self.load_index().map_err(SkillInstallError::Io)?;
        let skill = idx
            .skills
            .iter()
            .find(|s| s.meta.id == skill_id)
            .ok_or_else(|| SkillInstallError::Invalid(format!("技能 {skill_id} 未安装")))?;

        let missing: Vec<String> = skill
            .meta
            .allowed_tools
            .iter()
            .filter(|t| !app_tools.iter().any(|a| a == *t))
            .cloned()
            .collect();
        if !missing.is_empty() {
            return Err(SkillInstallError::ToolsNotSubset(missing));
        }

        let list = idx.grants.entry(app_id.to_string()).or_default();
        match list.iter_mut().find(|g| g.skill_id == skill_id) {
            Some(g) => g.enabled = true,
            None => list.push(SkillGrant {
                skill_id: skill_id.to_string(),
                enabled: true,
            }),
        }
        self.save_index(&idx).map_err(SkillInstallError::Io)
    }

    /// 撤销授予：删除整条 `SkillGrant` 记录（不同于 `set_enabled(false)` 只是
    /// 禁用）。返回是否真的删掉了一条；本就没有该记录 -> `Ok(false)`，幂等。
    pub fn revoke(&self, app_id: &str, skill_id: &str) -> Result<bool, String> {
        let _guard = MUTEX.lock().expect("SkillStore mutex poisoned");
        let mut idx = self.load_index()?;
        let Some(list) = idx.grants.get_mut(app_id) else {
            return Ok(false);
        };
        let before = list.len();
        list.retain(|g| g.skill_id != skill_id);
        let removed = list.len() != before;
        if removed {
            self.save_index(&idx)?;
        }
        Ok(removed)
    }

    /// 启停开关：要求该 `(app_id, skill_id)` 已有授予记录，否则 `Err`（不会隐式
    /// 创建授予——启停和授予是两个动作）。
    pub fn set_enabled(&self, app_id: &str, skill_id: &str, enabled: bool) -> Result<(), String> {
        let _guard = MUTEX.lock().expect("SkillStore mutex poisoned");
        let mut idx = self.load_index()?;
        let list = idx.grants.entry(app_id.to_string()).or_default();
        match list.iter_mut().find(|g| g.skill_id == skill_id) {
            Some(g) => g.enabled = enabled,
            None => return Err(format!("应用 {app_id} 未获得技能 {skill_id} 的授予")),
        }
        self.save_index(&idx)
    }

    /// 列出某应用的全部授予，配对上完整的 `InstalledSkill`（供前端「已授予」列表
    /// 展示）；授予记录指向的技能已被卸载时静默跳过（不应发生，`uninstall` 会
    /// 同步清理，这里只是防御）。
    pub fn grants_for(&self, app_id: &str) -> Result<Vec<(InstalledSkill, bool)>, String> {
        let _guard = MUTEX.lock().expect("SkillStore mutex poisoned");
        let idx = self.load_index()?;
        let empty = Vec::new();
        let grants = idx.grants.get(app_id).unwrap_or(&empty);
        Ok(grants
            .iter()
            .filter_map(|g| {
                idx.skills
                    .iter()
                    .find(|s| s.meta.id == g.skill_id)
                    .map(|s| (s.clone(), g.enabled))
            })
            .collect())
    }

    /// `launch()`（Task 3）要用的最终结果：该应用**已授予且已启用**的技能目录
    /// 列表——`--skill <path>` 参数只会来自这里。
    pub fn enabled_skill_dirs(&self, app_id: &str) -> Result<Vec<PathBuf>, String> {
        let pairs = self.grants_for(app_id)?;
        Ok(pairs
            .into_iter()
            .filter(|(_, enabled)| *enabled)
            .map(|(s, _)| self.layout.skill_dir(&s.meta.id))
            .collect())
    }

    /// 应用卸载钩子（Task 3 `on_uninstall`）：清空该应用名下全部授予，返回删掉的
    /// 条数。不影响技能库本身（技能还是装着的，只是这个应用不再被授予）。
    pub fn remove_grants_for_app(&self, app_id: &str) -> Result<usize, String> {
        let _guard = MUTEX.lock().expect("SkillStore mutex poisoned");
        let mut idx = self.load_index()?;
        let removed = idx.grants.remove(app_id).map(|v| v.len()).unwrap_or(0);
        if removed > 0 {
            self.save_index(&idx)?;
        }
        Ok(removed)
    }

    /// 登记一条待确认的 Maker 生成技能安装（Task7）：调用方
    /// （`maker::handle_install_skill`）必须已经先跑过一次成功的 `preflight`——
    /// 本方法自己不再校验，只负责 mint 一个新 `confirm_id`、把
    /// `(draft_dir, meta, scan)` 持久化进 `skills-index.json`、返回该 id。
    pub fn register_pending_skill_install(
        &self,
        draft_dir: PathBuf,
        meta: SkillMeta,
        scan: ScanReport,
    ) -> Result<String, String> {
        let _guard = MUTEX.lock().expect("SkillStore mutex poisoned");
        let mut idx = self.load_index()?;
        let confirm_id = fresh_id(PENDING_SKILL_INSTALL_ID_PREFIX);
        // 终审 C1 防御层：`fresh_id` 已经让撞车概率降到可忽略，但既然这张表
        // 持久化跨进程、id 空间的生存期与数据一样长，这里仍显式拒绝任何形式的
        // 撞车——宁可让这次登记失败，也不能静默覆盖/追加出一条同 id 记录（那
        // 正是 C1 的根因：`take` 只会取到"第一条匹配"，第二条同 id 记录永远
        // 拿不到）。
        if idx
            .pending_skill_installs
            .iter()
            .any(|p| p.confirm_id == confirm_id)
        {
            return Err(format!("confirm_id 冲突（罕见，请重试）：{confirm_id}"));
        }
        idx.pending_skill_installs.push(PendingSkillInstall {
            confirm_id: confirm_id.clone(),
            draft_dir,
            meta,
            scan,
        });
        self.save_index(&idx)?;
        Ok(confirm_id)
    }

    /// 原子取出（并从持久化登记表移除）一条待确认的技能安装——
    /// `maker::resolve_install_skill` 唯一的消费入口。未知/已被消费过的
    /// `confirm_id` 返回 `Ok(None)`，不区分"从未存在"与"已经消费过一次"
    /// （同 `mcp.rs::McpManager::take_pending_install` 的语义）。
    pub fn take_pending_skill_install(
        &self,
        confirm_id: &str,
    ) -> Result<Option<PendingSkillInstall>, String> {
        let _guard = MUTEX.lock().expect("SkillStore mutex poisoned");
        let mut idx = self.load_index()?;
        let Some(pos) = idx
            .pending_skill_installs
            .iter()
            .position(|p| p.confirm_id == confirm_id)
        else {
            return Ok(None);
        };
        let item = idx.pending_skill_installs.remove(pos);
        self.save_index(&idx)?;
        Ok(Some(item))
    }

    /// 只读列出当前所有待确认的技能安装，供前端「技能」页确认面渲染。
    /// **不消费、不移除**——真正的消费入口唯一是 [`Self::take_pending_skill_install`]。
    pub fn list_pending_skill_installs(&self) -> Result<Vec<PendingSkillInstall>, String> {
        let _guard = MUTEX.lock().expect("SkillStore mutex poisoned");
        Ok(self.load_index()?.pending_skill_installs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/skills")
            .join(name)
    }

    #[test]
    fn parses_valid_frontmatter_and_body() {
        let dir = fixture("good-skill");
        let md = find_skill_md(&dir).expect("应找到 SKILL.md");
        let (meta, body) = parse_skill_md(&md).expect("应解析成功");

        assert_eq!(meta.id, "good-skill");
        assert_eq!(meta.name, "good-skill");
        assert!(meta.description.contains("示例技能"));
        assert_eq!(meta.license.as_deref(), Some("MIT"));
        assert_eq!(meta.compatibility.as_deref(), Some("任何应用皆可使用"));
        assert_eq!(
            meta.allowed_tools,
            vec!["bash".to_string(), "python".to_string()]
        );
        assert!(!meta.disable_model_invocation);
        assert!(!meta.has_scripts);
        assert!(body.contains("Good Skill"));
    }

    /// 终审 Minor 5 回归：`SKILL.md` 本身是一个指向别处文件的符号链接——
    /// `parse_skill_md` 应在读取文件内容**之前**就拒绝，而不是先把目标文件
    /// 整个读进内存再走到别的校验失败。用 `symlink_metadata` 断言目标文件
    /// 确实一个字节都没被读取过：链接指向的目标文件内容本身合法（不会因为
    /// "目标内容也不合法"这个混淆因素让断言看起来像是别的原因失败）。
    #[cfg(unix)]
    #[test]
    fn parse_skill_md_rejects_symlink_before_reading() {
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("real.md");
        std::fs::write(&target, "---\nname: x\ndescription: d\n---\nbody\n").unwrap();
        let link = tmp.path().join("SKILL.md");
        std::os::unix::fs::symlink(&target, &link).unwrap();

        let err = parse_skill_md(&link).unwrap_err();
        assert!(err.contains("符号链接"), "错误信息应点名符号链接：{err}");
    }

    #[test]
    fn rejects_unknown_frontmatter_field() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("SKILL.md");
        std::fs::write(&path, "---\nname: x\ndescription: d\nfoo: bar\n---\nbody\n").unwrap();
        let err = parse_skill_md(&path).unwrap_err();
        assert!(err.contains("foo"), "错误信息应点名字段：{err}");
    }

    #[test]
    fn rejects_bad_names() {
        for bad in [
            "PDF-Processing",
            "-pdf",
            "pdf--processing",
            &"a".repeat(65),
            "pdf processing",
        ] {
            assert!(validate_name(bad).is_err(), "应拒绝：{bad:?}");
        }
    }

    /// 终审 Minor 4：字符集从"任意 Unicode 字母数字"收紧为 ASCII a-z0-9-
    /// 之后，非 ASCII 名字（不论中文还是别的书写系统）一律拒绝——这与此前
    /// 版本的行为相反（此前一条叫 `accepts_i18n_lowercase_name` 的测试断言
    /// 中文名会被接受），是本次修复刻意引入的行为变更，不是回归。
    #[test]
    fn rejects_non_ascii_name() {
        let err = validate_name("数据分析-skill").unwrap_err();
        assert!(err.contains("ASCII"), "错误信息应点名 ASCII 字符集：{err}");
    }

    /// 终审 Minor 4 核心回归：西里尔字母 о（U+043E，与拉丁字母 o 肉眼无法
    /// 区分）冒充的技能名必须被拒绝；纯 ASCII 的原名 `connector-etiquette`
    /// （内置技能之一，见 `lib.rs::BUILTIN_SKILLS`）必须放行——不能矫枉过正
    /// 到连合法 ASCII 名字也拒。
    #[test]
    fn rejects_cyrillic_confusable_but_allows_ascii_original() {
        let confusable = "c\u{043E}nnector-etiquette"; // 中间的 "o" 是西里尔字母 U+043E
        assert!(
            validate_name(confusable).is_err(),
            "西里尔同形字符冒充的技能名应被拒绝"
        );
        assert_eq!(
            validate_name("connector-etiquette").expect("纯 ASCII 原名应放行"),
            "connector-etiquette"
        );
    }

    #[test]
    fn validate_name_applies_real_nfkc_transformation() {
        // U+2170 (SMALL ROMAN NUMERAL ONE) 的 NFKC 兼容分解是 ASCII 'i'——用它证明
        // validate_name 真的跑了 NFKC，不是简单透传输入字符串。
        let out = validate_name("\u{2170}").expect("兼容分解后应是合法的小写 ascii");
        assert_eq!(out, "i");
    }

    #[test]
    fn description_over_1024_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("SKILL.md");
        let long_desc = "d".repeat(MAX_DESCRIPTION_LEN + 1);
        std::fs::write(
            &path,
            format!("---\nname: x\ndescription: {long_desc}\n---\nbody\n"),
        )
        .unwrap();
        let err = parse_skill_md(&path).unwrap_err();
        assert!(err.contains("description"), "错误信息应点名字段：{err}");
    }

    /// 终审 Minor 6：`allowed-tools` 写成 YAML 数组（`[Bash, Read]`）应该被
    /// 接受，逐项按原样收进 `Vec<String>`（不额外 trim/大小写归一，与空格分隔
    /// 字符串形态对每个 token 的处理一致——两者都不做归一化，`known_tools`
    /// 子集校验按原样字符串比对）。
    #[test]
    fn allowed_tools_accepts_yaml_array_form() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("SKILL.md");
        std::fs::write(
            &path,
            "---
name: x
description: d
allowed-tools:
  - Bash
  - Read
---
body
",
        )
        .unwrap();
        let (meta, _body) = parse_skill_md(&path).expect("YAML 数组形态应解析成功");
        assert_eq!(
            meta.allowed_tools,
            vec!["Bash".to_string(), "Read".to_string()]
        );
    }

    /// 与上一条互补：空格分隔字符串形态（标准原始写法）仍然继续被接受，两种
    /// 形态是"同时支持"而不是"数组取代字符串"。
    #[test]
    fn allowed_tools_still_accepts_space_separated_string_form() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("SKILL.md");
        std::fs::write(
            &path,
            "---
name: x
description: d
allowed-tools: Bash Read
---
body
",
        )
        .unwrap();
        let (meta, _body) = parse_skill_md(&path).expect("空格分隔字符串形态应解析成功");
        assert_eq!(
            meta.allowed_tools,
            vec!["Bash".to_string(), "Read".to_string()]
        );
    }

    /// 数组元素类型不对（混了数字）仍应拒绝——不做"尽力转字符串"的静默纠错。
    #[test]
    fn allowed_tools_rejects_yaml_array_with_non_string_element() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("SKILL.md");
        std::fs::write(
            &path,
            "---
name: x
description: d
allowed-tools:
  - Bash
  - 42
---
body
",
        )
        .unwrap();
        let err = parse_skill_md(&path).unwrap_err();
        assert!(err.contains("allowed-tools"), "错误信息应点名字段：{err}");
    }

    #[test]
    fn scan_flags_scripts_and_high_findings() {
        let report = scan_skill_dir(&fixture("evil-skill")).expect("应扫描成功");
        assert!(report.has_scripts);
        assert!(report.script_files.iter().any(|f| f == "scripts/run.sh"));
        let high = report
            .findings
            .iter()
            .filter(|f| f.severity == Severity::High)
            .count();
        assert_eq!(
            high, 1,
            "curl|sh 应恰好命中 1 条 High：{:?}",
            report.findings
        );
    }

    #[test]
    fn scan_script_skill_has_scripts_but_no_findings() {
        let report = scan_skill_dir(&fixture("script-skill")).expect("应扫描成功");
        assert!(report.has_scripts);
        assert!(report.findings.is_empty(), "无害脚本不应命中任何规则");
    }

    #[test]
    fn scan_rejects_symlink() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("SKILL.md"),
            "---\nname: x\ndescription: d\n---\n",
        )
        .unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(tmp.path().join("SKILL.md"), tmp.path().join("evil-link"))
            .unwrap();
        let err = scan_skill_dir(tmp.path()).unwrap_err();
        assert!(err.contains("符号链接"), "错误信息应点名符号链接：{err}");
    }

    #[test]
    fn scan_counts_bytes_and_files() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("a.txt"), "1234567890").unwrap(); // 10 bytes
        std::fs::create_dir(tmp.path().join("sub")).unwrap();
        std::fs::write(tmp.path().join("sub").join("b.txt"), "12345").unwrap(); // 5 bytes

        let report = scan_skill_dir(tmp.path()).expect("应扫描成功");
        assert_eq!(report.file_count, 2);
        assert_eq!(report.total_bytes, 15);
    }

    #[test]
    fn scan_rejects_dir_exceeding_max_files() {
        let tmp = tempfile::tempdir().unwrap();
        for i in 0..(MAX_SKILL_FILES + 1) {
            std::fs::write(tmp.path().join(format!("f{i}.txt")), "x").unwrap();
        }
        let err = scan_skill_dir(tmp.path()).unwrap_err();
        assert!(err.contains("文件数"), "错误信息应点名文件数超限：{err}");
    }

    /// 审查修复轮 1 Important 的回归测试：单文件超过 `MAX_SKILL_FILE_BYTES`
    /// 必须在 `metadata().len()` 阶段就被拒绝，绝不能先 `read_to_string` 整个
    /// 文件——用 `File::set_len` 造一个 6 MiB 的稀疏文件（磁盘上几乎不占空间，
    /// 但 `metadata().len()` 如实报告 6 MiB；若实现整读它进内存/逐行扫描，
    /// 稀疏区间会被物化成真实数据，耗时会显著跳出"只读一次元数据"的量级）来
    /// 证明：断言错误分类正确（点名"超过"）且扫描本身极快。
    #[test]
    fn scan_rejects_single_huge_file_via_metadata_without_reading_it() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("huge.bin");
        let file = std::fs::File::create(&path).unwrap();
        file.set_len(6 * 1024 * 1024).unwrap(); // 6 MiB 稀疏文件
        drop(file);

        let start = std::time::Instant::now();
        let err = scan_skill_dir(tmp.path()).unwrap_err();
        let elapsed = start.elapsed();

        assert!(err.contains("超过"), "错误信息应点名超限：{err}");
        assert!(
            elapsed < std::time::Duration::from_secs(1),
            "只读 metadata 判定超限应远快于 1s（真整读会因稀疏文件展开而慢得多），\
             实际耗时 {elapsed:?}"
        );
    }

    /// 与上一条互补：单个文件都不超 `MAX_SKILL_FILE_BYTES`，但累计超过
    /// `MAX_SKILL_BYTES`——同样必须在第一遍 metadata 累加阶段就 `Err`，不进入
    /// 第二遍内容扫描。
    #[test]
    fn scan_rejects_dir_exceeding_total_bytes_without_single_file_over_limit() {
        let tmp = tempfile::tempdir().unwrap();
        // 每个文件 512 KiB（远小于 MAX_SKILL_FILE_BYTES = 1 MiB），11 个文件
        // 累计 5.5 MiB，超过 MAX_SKILL_BYTES = 5 MiB。
        let chunk = 512 * 1024u64;
        let files_needed = (MAX_SKILL_BYTES / chunk) + 2;
        for i in 0..files_needed {
            let path = tmp.path().join(format!("f{i}.bin"));
            let file = std::fs::File::create(&path).unwrap();
            file.set_len(chunk).unwrap();
        }

        let err = scan_skill_dir(tmp.path()).unwrap_err();
        assert!(
            err.contains("目录大小"),
            "错误信息应点名目录总大小超限：{err}"
        );
    }

    // --- Task7: 待确认 Maker 生成技能安装（持久化） ---

    fn temp_store() -> (tempfile::TempDir, SkillStore) {
        let tmp = tempfile::tempdir().unwrap();
        let layout = DataLayout::new(tmp.path().to_path_buf());
        (tmp, SkillStore::new(layout))
    }

    fn sample_meta() -> SkillMeta {
        SkillMeta {
            id: "good-skill".to_string(),
            name: "good-skill".to_string(),
            description: "示例".to_string(),
            license: None,
            compatibility: None,
            allowed_tools: vec![],
            disable_model_invocation: false,
            has_scripts: false,
        }
    }

    #[test]
    fn register_then_take_pending_skill_install_is_at_most_once() {
        let (_tmp, store) = temp_store();
        let confirm_id = store
            .register_pending_skill_install(
                PathBuf::from("/tmp/draft-x"),
                sample_meta(),
                ScanReport::default(),
            )
            .expect("register 应成功");

        let first = store
            .take_pending_skill_install(&confirm_id)
            .expect("take 应成功");
        assert!(first.is_some(), "第一次 take 应拿到登记的 pending");
        assert_eq!(first.unwrap().draft_dir, PathBuf::from("/tmp/draft-x"));

        let second = store
            .take_pending_skill_install(&confirm_id)
            .expect("take 应成功");
        assert!(
            second.is_none(),
            "第二次 take 同一个 id 应为 None（at-most-once）"
        );
    }

    #[test]
    fn pending_skill_install_persists_across_fresh_store() {
        let tmp = tempfile::tempdir().unwrap();
        let layout = DataLayout::new(tmp.path().to_path_buf());
        let store1 = SkillStore::new(layout.clone());
        store1
            .register_pending_skill_install(
                PathBuf::from("/tmp/draft-y"),
                sample_meta(),
                ScanReport::default(),
            )
            .expect("register 应成功");

        let store2 = SkillStore::new(layout);
        let pending = store2.list_pending_skill_installs().expect("list 应成功");
        assert_eq!(
            pending.len(),
            1,
            "同一 tempdir 建的第二个 SkillStore 应看到已落盘的 pending"
        );
        assert_eq!(pending[0].meta.id, "good-skill");
    }

    #[test]
    fn list_pending_skill_installs_does_not_consume() {
        let (_tmp, store) = temp_store();
        let confirm_id = store
            .register_pending_skill_install(
                PathBuf::from("/tmp/draft-z"),
                sample_meta(),
                ScanReport::default(),
            )
            .expect("register 应成功");

        assert_eq!(store.list_pending_skill_installs().unwrap().len(), 1);
        assert_eq!(store.list_pending_skill_installs().unwrap().len(), 1);

        let taken = store.take_pending_skill_install(&confirm_id).unwrap();
        assert!(taken.is_some());
        assert!(store.list_pending_skill_installs().unwrap().is_empty());
    }

    /// 终审 C1 回归：`register_pending_skill_install` 生成的 `confirm_id` 不能
    /// 依赖任何"进程重启即归零"的内存态——用一个新建的 `SkillStore` 连续登记
    /// 两条，再"模拟重启"（另 `new` 一个指向**同一** `DataLayout` 根的
    /// `SkillStore` 实例，不复用前一个实例）后登记第三条：三个 `confirm_id`
    /// 必须两两不同，且 `take` 各自取到的必须是对应那条真实记录（`meta.name`
    /// 逐一核对，不是"随便哪条同 id 的记录"）——这正是 C1 击穿的语义契约：
    /// 用户批准的 `confirm_id` 与真正被安装的草稿必须是同一个。
    #[test]
    fn pending_skill_install_ids_survive_simulated_restart_without_collision() {
        let tmp = tempfile::tempdir().unwrap();
        let layout = DataLayout::new(tmp.path().to_path_buf());

        let store1 = SkillStore::new(layout.clone());
        let mut meta_a = sample_meta();
        meta_a.id = "skill-a".to_string();
        meta_a.name = "skill-a".to_string();
        let id_a = store1
            .register_pending_skill_install(
                PathBuf::from("/tmp/draft-a"),
                meta_a,
                ScanReport::default(),
            )
            .expect("register A 应成功");

        let mut meta_b = sample_meta();
        meta_b.id = "skill-b".to_string();
        meta_b.name = "skill-b".to_string();
        let id_b = store1
            .register_pending_skill_install(
                PathBuf::from("/tmp/draft-b"),
                meta_b,
                ScanReport::default(),
            )
            .expect("register B 应成功");

        // 模拟「重启」：不复用 store1，另 new 一个指向同一 root 的 SkillStore。
        let store2 = SkillStore::new(layout);
        let mut meta_c = sample_meta();
        meta_c.id = "skill-c".to_string();
        meta_c.name = "skill-c".to_string();
        let id_c = store2
            .register_pending_skill_install(
                PathBuf::from("/tmp/draft-c"),
                meta_c,
                ScanReport::default(),
            )
            .expect("register C（「重启」后）应成功");

        assert_ne!(
            id_a, id_b,
            "同一个 store 连续两次登记的 confirm_id 应互不相同"
        );
        assert_ne!(id_b, id_c, "「重启」前后登记的 confirm_id 应互不相同");
        assert_ne!(id_a, id_c, "「重启」前后登记的 confirm_id 应互不相同");

        let taken_a = store2
            .take_pending_skill_install(&id_a)
            .expect("take 应成功")
            .expect("id_a 应仍能取到");
        assert_eq!(
            taken_a.meta.name, "skill-a",
            "take(id_a) 应精确取到 A，不是 B/C"
        );

        let taken_b = store2
            .take_pending_skill_install(&id_b)
            .expect("take 应成功")
            .expect("id_b 应仍能取到");
        assert_eq!(
            taken_b.meta.name, "skill-b",
            "take(id_b) 应精确取到 B，不是 A/C"
        );

        let taken_c = store2
            .take_pending_skill_install(&id_c)
            .expect("take 应成功")
            .expect("id_c 应仍能取到");
        assert_eq!(
            taken_c.meta.name, "skill-c",
            "take(id_c) 应精确取到 C，不是 A/B"
        );
    }
}
