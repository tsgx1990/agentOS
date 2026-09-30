//! 真实 pi（本机 PATH 或 SUPERAGENT_PI_BIN）：应用会话里 pi 能看到的技能集合 =
//! 宿主授予且启用的集合（spec §4/§8 不变量）。需要本机 pi；
//! `cargo test --test real_pi_skills_visibility_it -- --ignored`。
//!
//! 三组用例，同一个探针（`tests/fixtures/rpc-probe/skills_probe.ts`，
//! `session_start` 上报 `pi.getCommands()` 里 `source === "skill"` 的名字集合）：
//! 1. 正例：`--no-skills --skill <fixtures/skills/good-skill>` -> 恰好看到
//!    `good-skill`（`SkillsCapability::launch` 真实产出的参数形状）。
//! 2. 对照组：临时 `HOME` 下放一个诱饵技能（`~/.agents/skills/decoy-skill`）、
//!    临时 `PI_CODING_AGENT_DIR` 下再放一个（`skills/decoy2`）——`--no-skills`
//!    且不带任何 `--skill` -> 可见集合为空，坐实"自动发现已关"这半条不变量
//!    （只给的那一半授予集合，不代表用户真实主目录下的技能会漏进来）。
//! 3. 反证组：同一份对照组诱饵，去掉 `--no-skills` -> `decoy2` 可见——证明
//!    对照组不是"这台机器本来就看不到任何技能"这种巧合通过，而是 `--no-skills`
//!    真的挡住了本来会被发现的东西（已用本机真实 pi 0.84.4 人工复现过这三组
//!    输出，形状与下方断言一致）。
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};

fn probe() -> PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/rpc-probe/skills_probe.ts")
}

fn fixture_skill(name: &str) -> PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/skills")
        .join(name)
}

/// 从 pi `--mode rpc` 的一行 stdout 里取出 `SKILLS_PROBE:` 探针 payload——同
/// `real_pi_tools_allowlist_it.rs::parse_probe_line` 的手法（外层 `message` 字段
/// 已被 serde_json 反转义成普通 Rust `String`，在其上找前缀、剥掉、再整体解析
/// 一次，不手工切片找 `}`，见该文件文档记录的踩坑）。
fn parse_probe_line(line: &str) -> Option<Vec<String>> {
    let v: serde_json::Value = serde_json::from_str(line).ok()?;
    if v["type"] != "extension_ui_request" || v["method"] != "notify" {
        return None;
    }
    let msg = v["message"].as_str()?;
    let rest = msg.strip_prefix("SKILLS_PROBE:")?;
    let payload: serde_json::Value = serde_json::from_str(rest).ok()?;
    Some(
        payload["skills"]
            .as_array()?
            .iter()
            .map(|v| v.as_str().unwrap_or_default().to_string())
            .collect(),
    )
}

/// 起一个 `--mode rpc --no-session --offline` 的 pi 子进程，写一条 `get_state`
/// 触发 `session_start`，从 stdout 里取出探针上报的技能名集合。`extra_args` 由
/// 各测试自己拼（`--no-skills`/`--skill <dir>` 的组合是三组用例的差异所在）；
/// `home`/`agent_dir` 分别喂给子进程的 `HOME`/`PI_CODING_AGENT_DIR`，对照组/
/// 反证组靠它们摆诱饵技能。
fn visible_skills(
    extra_args: &[&str],
    home: &std::path::Path,
    agent_dir: &std::path::Path,
) -> Vec<String> {
    let pi = super_agent_os::pi_bin::resolve_pi_bin();
    let mut args = vec!["--mode", "rpc", "--no-session", "--offline"];
    args.extend_from_slice(extra_args);
    let probe_path = probe();
    args.push("-e");
    args.push(probe_path.to_str().unwrap());

    let mut child = Command::new(&pi)
        .args(&args)
        .env("HOME", home)
        .env("PI_CODING_AGENT_DIR", agent_dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(b"{\"type\":\"get_state\"}\n")
        .unwrap();
    let reader = BufReader::new(child.stdout.take().unwrap());
    let mut found = None;
    for line in reader.lines().take(50) {
        let line = line.unwrap();
        if let Some(skills) = parse_probe_line(&line) {
            found = Some(skills);
            break;
        }
    }
    let _ = child.kill();
    let _ = child.wait();
    found.expect("探针未上报（SKILLS_PROBE 未出现在前 50 行 stdout 里）")
}

fn write_decoy_skill(dir: &std::path::Path, name: &str) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(
        dir.join("SKILL.md"),
        format!(
            "---\nname: {name}\ndescription: 对照组诱饵技能，验证 --no-skills 是否真的关闭了自动发现。\n---\n\n# {name}\n"
        ),
    )
    .unwrap();
}

// ---------------------------------------------------------------------------
// 1. 正例：授予（--skill）+ 启用（--no-skills 不挡显式路径）-> 恰好可见该技能
// ---------------------------------------------------------------------------

#[test]
#[ignore = "手工里程碑自动化：依赖真实 pi 装在本机(或 PATH)，用 `cargo test -- --ignored` 显式跑"]
fn granted_skill_is_the_only_one_visible() {
    let home = tempfile::tempdir().unwrap();
    let agent_dir = tempfile::tempdir().unwrap();
    let good = fixture_skill("good-skill");
    let skills = visible_skills(
        &["--no-skills", "--skill", good.to_str().unwrap()],
        home.path(),
        agent_dir.path(),
    );
    assert_eq!(skills, vec!["good-skill".to_string()], "{skills:?}");
}

// ---------------------------------------------------------------------------
// 2. 对照组：真实自动发现路径下放诱饵，--no-skills 且不带任何 --skill -> 空
// ---------------------------------------------------------------------------

#[test]
#[ignore = "手工里程碑自动化：依赖真实 pi 装在本机(或 PATH)，用 `cargo test -- --ignored` 显式跑"]
fn no_skills_flag_hides_both_default_discovery_locations() {
    let home = tempfile::tempdir().unwrap();
    let agent_dir = tempfile::tempdir().unwrap();
    write_decoy_skill(
        &home.path().join(".agents/skills/decoy-skill"),
        "decoy-skill",
    );
    write_decoy_skill(&agent_dir.path().join("skills/decoy2"), "decoy2");

    let skills = visible_skills(&["--no-skills"], home.path(), agent_dir.path());
    assert!(
        skills.is_empty(),
        "--no-skills 应关闭 ~/.agents/skills 与 $PI_CODING_AGENT_DIR/skills 的自动发现：{skills:?}"
    );
}

// ---------------------------------------------------------------------------
// 3. 反证组：同一份诱饵，去掉 --no-skills -> decoy2 可见（证明 2. 不是巧合通过）
// ---------------------------------------------------------------------------

#[test]
#[ignore = "手工里程碑自动化：依赖真实 pi 装在本机(或 PATH)，用 `cargo test -- --ignored` 显式跑"]
fn without_no_skills_default_discovery_is_visible() {
    let home = tempfile::tempdir().unwrap();
    let agent_dir = tempfile::tempdir().unwrap();
    write_decoy_skill(
        &home.path().join(".agents/skills/decoy-skill"),
        "decoy-skill",
    );
    write_decoy_skill(&agent_dir.path().join("skills/decoy2"), "decoy2");

    let skills = visible_skills(&[], home.path(), agent_dir.path());
    assert!(
        skills.iter().any(|s| s == "decoy2"),
        "不带 --no-skills 时应能自动发现 $PI_CODING_AGENT_DIR/skills 下的技能：{skills:?}"
    );
}
