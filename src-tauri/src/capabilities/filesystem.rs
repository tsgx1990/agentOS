//! filesystem 能力：清单里的 `$VAR[/子路径]` 声明 → 安装期校验（白名单变量、无 ..、非绝对路径）→
//! 启动期展开为真实目录并交给 sandbox 放行（read → file-read*，write → file-write*+file-read*）。
//! `$APP_DATA` 是隐含的唯一默认可写区：合法、不呈现、不额外放行。
use crate::capability::*;
use crate::permissions::Permissions;
use std::path::{Path, PathBuf};

pub struct FilesystemCapability;
pub const ALLOWED_VARS: [&str; 7] = [
    "$APP_DATA",
    "$DOWNLOADS",
    "$DOCUMENTS",
    "$DESKTOP",
    "$PICTURES",
    "$MUSIC",
    "$MOVIES",
];

fn split(spec: &str) -> (&str, Option<&str>) {
    match spec.find('/') {
        Some(i) => (&spec[..i], Some(&spec[i + 1..])),
        None => (spec, None),
    }
}

pub fn validate_spec(spec: &str) -> Result<(), String> {
    if spec.contains('\0') {
        return Err(format!("路径声明 {spec:?} 含 NUL"));
    }
    if spec.contains('"') || spec.contains('\\') {
        return Err(format!(
            "路径声明 {spec:?} 含引号或反斜线（防 SBPL/-D 注入）"
        ));
    }
    if spec.starts_with('/') {
        return Err(format!(
            "路径声明 {spec} 是绝对路径，请用变量（{}）",
            ALLOWED_VARS.join(" / ")
        ));
    }
    if spec.starts_with('~') {
        return Err(format!("路径声明 {spec} 不支持 ~，请用变量"));
    }
    let (var, rest) = split(spec);
    if var == "$HOME" {
        return Err("路径声明 $HOME 过宽（等于全盘），请用具体目录变量".to_string());
    }
    if !ALLOWED_VARS.contains(&var) {
        return Err(format!(
            "路径声明 {spec} 的变量 {var} 不在白名单：{}",
            ALLOWED_VARS.join(" / ")
        ));
    }
    if let Some(r) = rest {
        if r.is_empty()
            || r.split('/')
                .any(|seg| seg == ".." || seg == "." || seg.is_empty())
        {
            return Err(format!("路径声明 {spec} 的子路径含 .. / . / 空段"));
        }
    }
    Ok(())
}

fn base_dir(var: &str) -> Option<PathBuf> {
    match var {
        "$DOWNLOADS" => dirs::download_dir(),
        "$DOCUMENTS" => dirs::document_dir(),
        "$DESKTOP" => dirs::desktop_dir(),
        "$PICTURES" => dirs::picture_dir(),
        "$MUSIC" => dirs::audio_dir(),
        "$MOVIES" => dirs::video_dir(),
        _ => None,
    }
}

pub fn human_name(spec: &str) -> String {
    let (var, rest) = split(spec);
    let zh = match var {
        "$DOWNLOADS" => "下载文件夹",
        "$DOCUMENTS" => "文稿文件夹",
        "$DESKTOP" => "桌面",
        "$PICTURES" => "图片",
        "$MUSIC" => "音乐",
        "$MOVIES" => "影片",
        "$APP_DATA" => "应用数据区",
        other => other,
    };
    match rest {
        Some(r) => format!("{zh}/{r}"),
        None => zh.to_string(),
    }
}

fn non_app_data(specs: &[String]) -> impl Iterator<Item = &String> {
    specs.iter().filter(|s| split(s).0 != "$APP_DATA")
}

/// 共用前置：校验 spec + 展开成候选路径 + 复核 `base` 落在 `home` 之下（P6-A 加固：
/// 防"某标准目录被替换成指向用户主目录之外的东西"——例如测试/精简系统上
/// `dirs::download_dir()` 被环境变量污染指向别处，或未来平台适配层算错了标准目录）。
/// `$APP_DATA` 直接返回 `Ok(None)`（隐含、不展开）。`base` 本身尚不存在（canonicalize
/// 失败）视为"还没发生"，返回 `Ok(None)` 跳过——与既有对未落地路径的容忍度一致。
/// `base` 落在 `home` 之外则 **fail-closed**：`Err`，不静默放行。
///
/// M-4（review）：本函数是本文件唯一的 spec 展开逻辑——此前还有一个平行的 `expand_path`
/// 独立重新实现同一段"校验 + split + 按 `$APP_DATA` 短路 + 拼子路径"逻辑（区别只是内部
/// 调用真实 `base_dir(var)` 而非吃调用方传入的 `base`），两处一旦其中一处改了拼接规则
/// （例如子路径分隔符处理）另一处不会自动跟着变。`expand_path` 已删除，`base_dir` 仍
/// 保留（`confined_expand`/`confined_expand_write` 两个生产入口用它取真实标准目录，
/// 见文件下方）。
fn expand_and_check_base(
    spec: &str,
    base: &Path,
    home: &Path,
) -> Result<Option<(PathBuf, PathBuf)>, String> {
    validate_spec(spec)?;
    let (var, rest) = split(spec);
    if var == "$APP_DATA" {
        return Ok(None);
    }
    let expanded = match rest {
        Some(r) => base.join(r),
        None => base.to_path_buf(),
    };
    let canon_base = match std::fs::canonicalize(base) {
        Ok(b) => b,
        Err(_) => return Ok(None),
    };
    let canon_home = std::fs::canonicalize(home)
        .map_err(|e| format!("用户主目录 {} 不可用：{e}", home.display()))?;
    if !canon_base.starts_with(&canon_home) {
        return Err(format!(
            "{var} 目录（{}）不在用户主目录（{}）之下，拒绝放行",
            canon_base.display(),
            canon_home.display()
        ));
    }
    Ok(Some((expanded, canon_base)))
}

/// 授权目录的「真实性」检查（I-c）：用户授权的目录由应用在沙盒里读写，应用可以把授权的
/// 子目录改名后换成链接，指向**同一标准目录里的其它位置**——规范化后仍在 `base` 之内，
/// 单靠「规范化后仍在 base 内」拦不住，但已超出用户同意的范围。所以对每个贡献给沙盒的路径要求：
/// 存在时不是符号链接，且规范化后**等于**按字面从已规范化的 `base` 推出的预期路径
/// （这同时挡住中间某一级被换成链接）。返回 `Ok(None)` 表示路径尚不存在，`Ok(Some(is_dir))`
/// 表示存在且合格，`Err` 表示被换链（可读错误，调用方 fail-closed、不贡献该路径）。
fn check_real_literal(spec: &str, var: &str, literal: &Path) -> Result<Option<bool>, String> {
    let not_real = |why: &str| {
        format!(
            "路径声明 {spec} 的目录（{}）{why}，已不是授权时的真实位置（可能被替换成了符号链接），拒绝启动",
            literal.display()
        )
    };
    let meta = match std::fs::symlink_metadata(literal) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => {
            return Err(format!(
                "检查 {var} 下的路径 {} 失败：{e}",
                literal.display()
            ))
        }
    };
    if meta.file_type().is_symlink() {
        return Err(not_real("是符号链接"));
    }
    match std::fs::canonicalize(literal) {
        Ok(c) if c == literal => Ok(Some(meta.is_dir())),
        Ok(_) => Err(not_real(
            "规范化后与预期路径不一致（路径中某一级是符号链接）",
        )),
        Err(e) => Err(format!("规范化 {} 失败：{e}", literal.display())),
    }
}

/// 把 spec 的子路径按字面接到已规范化的 `canon_base` 上（只取 Normal 分量）。
fn literal_components(spec: &str) -> Vec<String> {
    split(spec)
        .1
        .map(|r| {
            Path::new(r)
                .components()
                .filter_map(|c| match c {
                    std::path::Component::Normal(n) => Some(n.to_string_lossy().into_owned()),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default()
}

/// READ 展开：授权路径存在时必须是真目录/真文件、不是链接、规范化等于由已规范化 `base`
/// 按字面推出的预期路径（`check_real_literal`），否则 **fail-closed**：整次启动直接失败并给出
/// 可读错误，不贡献该路径——在自己被授权的目录里换链越权是恶意行为，不是「配置写错了」。
/// 候选路径尚不存在视为「还没发生」，跳过而非报错——与 spec §9 对未落地路径的既有容忍度
/// 一致（WRITE 侧不同，见 `confined_expand_write_with`）。
///
/// `base`/`home` 作为参数注入（而非内部直接调 `base_dir`/`dirs::home_dir`）：便于用
/// 假 tempdir 单测，不需要真的污染/依赖本机的标准目录布局。生产入口见 `confined_expand`。
pub(crate) fn confined_expand_with(
    spec: &str,
    base: &Path,
    home: &Path,
) -> Result<Option<PathBuf>, String> {
    let Some((_expanded, canon_base)) = expand_and_check_base(spec, base, home)? else {
        return Ok(None);
    };
    let (var, _) = split(spec);
    let mut literal = canon_base;
    for c in literal_components(spec) {
        literal.push(c);
    }
    Ok(check_real_literal(spec, var, &literal)?.map(|_| literal))
}

/// WRITE 展开：候选目录允许尚不存在（清单声明的写目录，安装/首次启动时通常还没创建
/// 过）。从已规范化的 `base` 起逐级按字面向下走：已存在的每一级都必须是真目录、不是链接、
/// 规范化等于该级预期字面路径（`check_real_literal`），任何一级被换链 → `Err`（fail-closed，
/// 不创建任何东西）；遇到第一个不存在的一级：`materialize` 为 `true` 则只用单层 `create_dir`
/// 逐级建出剩余部分，为 `false` 则只返回 `Ok(None)`、不落地任何目录。
/// `materialize`（F1，review）：供 `describe()`（`preview_install`/`app_capabilities` 只读
/// 诊断）传 `false` 复用同一套校验而不产生磁盘副作用；生产真实启动路径
/// （`open_app_after_acquire`/`headless_contribution`）传 `true`。
pub(crate) fn confined_expand_write_with(
    spec: &str,
    base: &Path,
    home: &Path,
    materialize: bool,
) -> Result<Option<PathBuf>, String> {
    let Some((_expanded, canon_base)) = expand_and_check_base(spec, base, home)? else {
        return Ok(None);
    };
    let (var, _) = split(spec);
    let mut cur = canon_base;
    for c in literal_components(spec) {
        cur.push(c);
        match check_real_literal(spec, var, &cur)? {
            Some(true) => {}
            Some(false) => {
                return Err(format!(
                    "路径声明 {spec} 的写目录（{}）已存在但不是目录，拒绝启动",
                    cur.display()
                ))
            }
            None => {
                if !materialize {
                    // 只校验、不落地：已存在的祖先都合格，剩余部分尚未创建。
                    return Ok(None);
                }
                std::fs::create_dir(&cur)
                    .map_err(|e| format!("创建写目录 {} 失败：{e}", cur.display()))?;
                if check_real_literal(spec, var, &cur)? != Some(true) {
                    return Err(format!("创建后的写目录 {} 不合格，拒绝启动", cur.display()));
                }
            }
        }
    }
    Ok(Some(cur))
}

fn resolved_home() -> Result<PathBuf, String> {
    dirs::home_dir().ok_or_else(|| "本机无法确定用户主目录（安全检查前置条件缺失）".to_string())
}

/// 生产入口（READ）：`base`/`home` 均取本机真实标准目录，见 `confined_expand_with`。
fn confined_expand(spec: &str) -> Result<Option<PathBuf>, String> {
    let (var, _) = split(spec);
    let Some(base) = base_dir(var) else {
        return Ok(None);
    };
    confined_expand_with(spec, &base, &resolved_home()?)
}

/// 生产入口（WRITE）：`base`/`home` 均取本机真实标准目录，见 `confined_expand_write_with`。
/// `materialize`：见该函数文档——由调用方（`FilesystemCapability::launch`）原样
/// 转发 `ctx.materialize`。
fn confined_expand_write(spec: &str, materialize: bool) -> Result<Option<PathBuf>, String> {
    let (var, _) = split(spec);
    let Some(base) = base_dir(var) else {
        return Ok(None);
    };
    confined_expand_write_with(spec, &base, &resolved_home()?, materialize)
}

#[async_trait::async_trait]
impl Capability for FilesystemCapability {
    fn key(&self) -> &'static str {
        "filesystem"
    }
    fn declared(&self, p: &Permissions, _i: &CallerIdentity) -> bool {
        non_app_data(&p.filesystem.read).next().is_some()
            || non_app_data(&p.filesystem.write).next().is_some()
    }
    fn render_human(&self, p: &Permissions) -> Vec<String> {
        let mut out = vec![];
        let r: Vec<String> = non_app_data(&p.filesystem.read)
            .map(|s| human_name(s))
            .collect();
        if !r.is_empty() {
            out.push(format!("读取：{}", r.join("、")));
        }
        let w: Vec<String> = non_app_data(&p.filesystem.write)
            .map(|s| human_name(s))
            .collect();
        if !w.is_empty() {
            out.push(format!("修改：{}", w.join("、")));
        }
        out
    }
    fn launch(&self, p: &Permissions, ctx: &LaunchCtx<'_>) -> Result<LaunchContribution, String> {
        if !ctx.sandboxed {
            return Ok(LaunchContribution::default());
        } // 非 L2 平台：只声明不放宽
        let mut c = LaunchContribution::default();
        for s in &p.filesystem.read {
            if let Some(path) = confined_expand(s)? {
                c.sandbox_read.push(path);
            }
        }
        for s in &p.filesystem.write {
            if let Some(path) = confined_expand_write(s, ctx.materialize)? {
                c.sandbox_write.push(path);
            }
        }
        Ok(c)
    }
    fn enforcement(&self) -> &'static [Enforcement] {
        &[Enforcement::Sandbox]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn app_data_is_implicit_and_expands_to_none() {
        // M-4（review）：`expand_path` 已删除（与 `expand_and_check_base` 是同一段逻辑
        // 的两份实现），这条属性改测唯一存活的展开函数——`$APP_DATA` 短路发生在触碰
        // `base`/`home` 之前，传两个不存在的占位路径也应该照样返回 `Ok(None)`。
        assert!(validate_spec("$APP_DATA").is_ok());
        let irrelevant = Path::new("/does/not/exist");
        assert_eq!(
            expand_and_check_base("$APP_DATA", irrelevant, irrelevant).unwrap(),
            None
        );
        assert_eq!(
            expand_and_check_base("$APP_DATA/notes", irrelevant, irrelevant).unwrap(),
            None
        );
    }
    #[test]
    fn downloads_base_dir_is_absolute_and_has_chinese_name() {
        // M-4（review）：同上，改测 `base_dir`（生产入口 `confined_expand`/
        // `confined_expand_write` 用它取真实标准目录）——只断言"是绝对路径"这条不依赖
        // 该目录在磁盘上是否真的存在的属性，不通过 `expand_and_check_base`
        // 走完整的 canonicalize+home 复核（那条路径需要 `~/Downloads` 真实存在，不该让
        // 一条纯断言"返回值形状"的单测依赖测试机器的磁盘状态）。
        let p = base_dir("$DOWNLOADS").expect("本机应有下载目录标准路径");
        assert!(p.is_absolute());
        assert_eq!(human_name("$DOWNLOADS"), "下载文件夹");
        assert_eq!(human_name("$DOCUMENTS/工作"), "文稿文件夹/工作");
    }
    #[test]
    fn rejects_home_absolute_dotdot_tilde_and_unknown_var() {
        for bad in [
            "$HOME",
            "/etc",
            "~/x",
            "$DOWNLOADS/../..",
            "$DOWNLOADS/a/../b",
            "$FOO",
            "relative",
            "$DOWNLOADS/\0",
        ] {
            assert!(validate_spec(bad).is_err(), "{bad} 应被拒");
        }
    }
    #[test]
    fn rejects_quote_and_backslash_at_install_time() {
        for bad in ["$DOWNLOADS/a\"b", "$DOWNLOADS/a\\b"] {
            assert!(
                validate_spec(bad).is_err(),
                "{bad:?} 应被拒（防 SBPL/-D 注入，早于 build_profile 的同类检查）"
            );
        }
    }
    #[test]
    fn render_lists_read_and_write_separately_skipping_app_data() {
        let mut p = Permissions::default();
        p.filesystem.read = vec!["$DOWNLOADS".into(), "$APP_DATA".into()];
        p.filesystem.write = vec!["$APP_DATA".into(), "$DESKTOP/导出".into()];
        let lines = FilesystemCapability.render_human(&p);
        assert_eq!(
            lines,
            vec![
                "读取：下载文件夹".to_string(),
                "修改：桌面/导出".to_string()
            ]
        );
        let mut only_app = Permissions::default();
        only_app.filesystem.write = vec!["$APP_DATA".into()];
        assert!(!FilesystemCapability.declared(&only_app, &CallerIdentity::installing("a", false)));
    }

    // ---- P6-A ruling 7(a)：base 目录替换防护——base 必须落在 home 之下 ----

    #[test]
    fn confined_expand_with_allows_base_under_home_and_rejects_base_outside_home() {
        let home = tempfile::tempdir().unwrap();
        let home_path = std::fs::canonicalize(home.path()).unwrap();

        // base 在 home 之下 -> Ok（候选路径就是 base 自身，因为 spec 没有子路径）。
        let base_under = home_path.join("Downloads");
        std::fs::create_dir_all(&base_under).unwrap();
        let ok = confined_expand_with("$DOWNLOADS", &base_under, &home_path)
            .expect("base 落在 home 之下应放行");
        assert_eq!(ok, Some(base_under.clone()));

        // base 不在 home 之下（另一个不相关的 tempdir）-> Err，且错误信息指出原因。
        let outside = tempfile::tempdir().unwrap();
        let outside_path = std::fs::canonicalize(outside.path()).unwrap();
        let err = confined_expand_with("$DOWNLOADS", &outside_path, &home_path).unwrap_err();
        assert!(err.contains("不在用户主目录"), "{err}");
    }

    // ---- P6-A ruling 7(b)：WRITE 声明目录尚不存在时应被创建；READ 侧维持既有跳过 ----

    #[test]
    fn write_spec_creates_absent_dir_while_read_spec_still_skips_absent() {
        let home = tempfile::tempdir().unwrap();
        let home_path = std::fs::canonicalize(home.path()).unwrap();
        let base = home_path.join("Downloads");
        std::fs::create_dir_all(&base).unwrap();

        // 写：base/new/sub 尚不存在 -> 应被创建并原样返回展开后的路径。
        let created = confined_expand_write_with("$DOWNLOADS/new/sub", &base, &home_path, true)
            .expect("写目录尚不存在应能创建")
            .expect("应返回展开后的路径");
        assert!(created.is_dir(), "应真的建出目录：{}", created.display());
        assert_eq!(
            created,
            std::fs::canonicalize(base.join("new/sub")).unwrap()
        );

        // 读：同样尚不存在的路径 -> 维持既有"跳过"容忍度，Ok(None)，不创建任何东西。
        let read = confined_expand_with("$DOWNLOADS/absent/sub", &base, &home_path).unwrap();
        assert_eq!(read, None);
        assert!(!base.join("absent").exists(), "READ 分支绝不应创建目录");
    }

    // ---- F1（review）：materialize=false 时绝不 create_dir_all（describe()/
    // preview_install/app_capabilities 只读诊断路径必须零磁盘副作用）----

    #[test]
    fn write_spec_with_materialize_false_validates_without_creating_dir() {
        let home = tempfile::tempdir().unwrap();
        let home_path = std::fs::canonicalize(home.path()).unwrap();
        let base = home_path.join("Downloads");
        std::fs::create_dir_all(&base).unwrap();

        // materialize=false：base/new/sub 尚不存在 -> Ok(None)，且绝不创建。
        let result = confined_expand_write_with("$DOWNLOADS/new/sub", &base, &home_path, false)
            .expect("只校验不落地不应报错");
        assert_eq!(result, None, "materialize=false 时不应返回展开后的路径");
        assert!(
            !base.join("new").exists(),
            "materialize=false 绝不应创建任何目录：{}",
            base.join("new").display()
        );

        // materialize=true：同一个 spec -> 真的建出目录（与既有行为一致）。
        let created = confined_expand_write_with("$DOWNLOADS/new/sub", &base, &home_path, true)
            .expect("materialize=true 应能创建")
            .expect("应返回展开后的路径");
        assert!(
            created.is_dir(),
            "materialize=true 应真的建出目录：{}",
            created.display()
        );
    }

    #[test]
    fn write_spec_rejects_when_nearest_existing_ancestor_escapes_base() {
        let home = tempfile::tempdir().unwrap();
        let home_path = std::fs::canonicalize(home.path()).unwrap();
        let base = home_path.join("Downloads");
        std::fs::create_dir_all(&base).unwrap();

        // base 内一个符号链接指向 base 外部；写声明的深层子路径挂在这个逃逸点之下——
        // "尚不存在"这个借口不能绕过符号链接放大检查：最近的已存在祖先就是这个逃逸
        // 符号链接本身，必须被拒绝，且绝不能因此创建任何目录。
        let outside = home_path.join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        let escape = base.join("escape");
        std::os::unix::fs::symlink(&outside, &escape).unwrap();

        let err = confined_expand_write_with("$DOWNLOADS/escape/new/sub", &base, &home_path, true)
            .expect_err("最近的已存在祖先逃出 base 应被拒");
        assert!(err.contains("符号链接"), "{err}");
        assert!(
            !outside.join("new").exists(),
            "拒绝时绝不应在逃逸目标下创建任何目录"
        );
    }

    // ---- I-c：授权目录被换成指向「同一标准目录内其它位置」的链接——不放行 ----

    fn std_dir_with_swapped_link() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let home = tempfile::tempdir().unwrap();
        let home_path = std::fs::canonicalize(home.path()).unwrap();
        let base = home_path.join("Downloads");
        std::fs::create_dir_all(base.join("granted")).unwrap();
        std::fs::create_dir_all(base.join("private-other")).unwrap();
        (home, home_path, base)
    }

    #[test]
    fn write_dir_swapped_to_in_base_symlink_is_rejected_ic() {
        let (_h, home_path, base) = std_dir_with_swapped_link();
        // 正常：真目录照常贡献。
        let ok = confined_expand_write_with("$DOWNLOADS/granted", &base, &home_path, true)
            .unwrap()
            .unwrap();
        assert_eq!(ok, base.join("granted"));
        // 应用把 granted 换成指向同一标准目录里另一处的链接：规范化后仍在 base 内，旧逻辑放行。
        std::fs::remove_dir(base.join("granted")).unwrap();
        std::os::unix::fs::symlink(base.join("private-other"), base.join("granted")).unwrap();
        for materialize in [true, false] {
            let err =
                confined_expand_write_with("$DOWNLOADS/granted", &base, &home_path, materialize)
                    .expect_err("换链的写目录必须被拒");
            assert!(err.contains("符号链接"), "{err}");
        }
        // 中间一级被换成链接
        std::fs::create_dir_all(base.join("a")).unwrap();
        std::os::unix::fs::symlink(base.join("private-other"), base.join("a/b")).unwrap();
        assert!(confined_expand_write_with("$DOWNLOADS/a/b/c", &base, &home_path, true).is_err());
        assert!(!base.join("private-other/c").exists());
    }

    #[test]
    fn read_dir_swapped_to_in_base_symlink_is_rejected_ic() {
        let (_h, home_path, base) = std_dir_with_swapped_link();
        let ok = confined_expand_with("$DOWNLOADS/granted", &base, &home_path)
            .unwrap()
            .unwrap();
        assert_eq!(ok, base.join("granted"));
        // 单个文件也可以授权读取（真文件，非链接）
        std::fs::write(base.join("report.txt"), "x").unwrap();
        assert_eq!(
            confined_expand_with("$DOWNLOADS/report.txt", &base, &home_path).unwrap(),
            Some(base.join("report.txt"))
        );
        std::fs::remove_dir(base.join("granted")).unwrap();
        std::os::unix::fs::symlink(base.join("private-other"), base.join("granted")).unwrap();
        let err = confined_expand_with("$DOWNLOADS/granted", &base, &home_path)
            .expect_err("换链的读目录必须被拒");
        assert!(err.contains("符号链接"), "{err}");
    }
}
