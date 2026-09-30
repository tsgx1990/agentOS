// P6-B Task 5：三个内置技能（`samples/skills/*`）必须真的过安装门，不能只是
// "文件存在"——`find_skill_md` → `parse_skill_md` → `validate_name` →
// `scan_skill_dir` 全部走一遍，且不含脚本/不命中危险模式（纯指令技能），
// `id` 与目录名一致（spec 裁决 1：id 用 frontmatter `name`，本仓库内置技能
// 刻意让两者相同，方便 `seed_builtin_skills`/`BUILTIN_SKILLS` 白名单直接按
// 目录名拼路径）。
//
// 同时覆盖 `seed_builtin_skills`（不依赖 `tauri::AppHandle`，同
// `builtin_seed_it.rs::seed_builtin_maker` 测试的手法）：空技能库上播种一次
// 应装满三个、trusted=true；再播种一次应保持幂等（不报错、不产生重复记录）。

use super_agent_os::mcp::McpManager;
use super_agent_os::paths::DataLayout;
use super_agent_os::skills::{find_skill_md, parse_skill_md, scan_skill_dir, validate_name};
use super_agent_os::{seed_builtin_skills, BUILTIN_SKILLS};

/// 仓库真实的 `samples/` 目录：`CARGO_MANIFEST_DIR` 是 `src-tauri/`，上一级
/// 才是仓库根（同 `builtin_seed_it.rs::real_samples_dir` 的手法）。
fn real_samples_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../samples")
}

#[test]
fn builtin_skills_whitelist_has_exactly_three_entries() {
    assert_eq!(
        BUILTIN_SKILLS,
        &[
            "daily-brief-writing",
            "connector-etiquette",
            "app-packaging"
        ]
    );
}

#[test]
fn every_builtin_skill_passes_the_install_gate() {
    let samples = real_samples_dir();
    for name in BUILTIN_SKILLS {
        let dir = samples.join("skills").join(name);
        let md = find_skill_md(&dir)
            .unwrap_or_else(|| panic!("{name}：应能在 {} 下找到 SKILL.md", dir.display()));
        let (meta, body) =
            parse_skill_md(&md).unwrap_or_else(|e| panic!("{name}：frontmatter 解析应成功：{e}"));

        let normalized =
            validate_name(&meta.name).unwrap_or_else(|e| panic!("{name}：name 应合法：{e}"));
        assert_eq!(
            normalized, *name,
            "{name}：id 应与目录名一致（内置技能刻意让两者相同）"
        );
        assert!(!body.trim().is_empty(), "{name}：正文不应为空");
        assert!(
            meta.license.is_some(),
            "{name}：内置技能应声明 license（第一方产物）"
        );

        let report = scan_skill_dir(&dir).unwrap_or_else(|e| panic!("{name}：目录扫描应成功：{e}"));
        assert!(!report.has_scripts, "{name}：内置技能应是纯指令、无脚本");
        assert!(
            report.findings.is_empty(),
            "{name}：不应命中任何危险模式：{:?}",
            report.findings
        );
    }
}

#[test]
fn seed_builtin_skills_installs_all_three_trusted_and_is_idempotent() {
    let tmp = tempfile::tempdir().unwrap();
    let layout = DataLayout::new(tmp.path().to_path_buf());
    let mcp = McpManager::new();
    let samples = real_samples_dir();

    seed_builtin_skills(&samples, &layout, &mcp).expect("首次播种应成功");

    let store = super_agent_os::skills::SkillStore::new(layout.clone());
    let installed = store.list().unwrap();
    assert_eq!(installed.len(), 3, "应恰好装入三个内置技能");
    for s in &installed {
        assert!(s.trusted, "内置技能应 trusted=true：{}", s.meta.id);
        assert!(
            BUILTIN_SKILLS.contains(&s.meta.id.as_str()),
            "装入的 id 应在白名单内：{}",
            s.meta.id
        );
    }

    // 幂等：再播种一次不应报错、不应产生重复记录。
    seed_builtin_skills(&samples, &layout, &mcp).expect("重复播种不应报错");
    assert_eq!(store.list().unwrap().len(), 3, "重复播种不应产生重复记录");
}
