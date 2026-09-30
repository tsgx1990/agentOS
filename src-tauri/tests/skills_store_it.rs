// P6-B Task 2：`SkillStore`——技能库、清单/授予表持久化、安装门。
//
// 覆盖 spec §8 不变量：安装门任一条失败 -> `skills/` 下无残留；`allowed-tools`
// 含未知工具名拒装；`trusted=false` 命中 High 直接拒装（fail-closed，不给"仍然
// 安装"按钮，裁决 3）；`trusted=true` 允许但仍记录 findings；授予要求
// `allowed-tools ⊆` 目标应用工具集；禁用/撤销后不在 `enabled_skill_dirs` 里；
// 卸载清空所有应用的授予且不留目录残留；重名（同 id）拒装。

use std::path::{Path, PathBuf};
use super_agent_os::paths::DataLayout;
use super_agent_os::skills::{
    Severity, SkillInstallError, SkillSource, SkillSourceKind, SkillStore,
};

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/skills")
        .join(name)
}

fn local_source() -> SkillSource {
    SkillSource {
        kind: SkillSourceKind::Local,
        url: None,
        sha256: None,
    }
}

fn layout(tmp: &Path) -> DataLayout {
    DataLayout::new(tmp.to_path_buf())
}

#[test]
fn install_then_list_persists() {
    let tmp = tempfile::tempdir().unwrap();
    let store = SkillStore::new(layout(tmp.path()));

    let installed = store
        .install_from_dir(
            &fixture("good-skill"),
            local_source(),
            true,
            &["bash".to_string(), "python".to_string()],
            1000,
        )
        .expect("good-skill 应装成功");
    assert_eq!(installed.meta.id, "good-skill");
    assert!(layout(tmp.path()).skill_dir("good-skill").is_dir());
    assert!(layout(tmp.path())
        .skill_dir("good-skill")
        .join("SKILL.md")
        .is_file());

    // 现造第二个 SkillStore 实例（同一个 layout 根）——证明数据真的落了盘，不是
    // 只存在内存里（同 approvals.rs 的 staged_persists_across_fresh_store 惯例）。
    let store2 = SkillStore::new(layout(tmp.path()));
    let listed = store2.list().expect("list 应成功");
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].meta.id, "good-skill");
    assert_eq!(listed[0].source.kind, SkillSourceKind::Local);
    assert!(listed[0].trusted);
}

#[test]
fn install_failure_leaves_no_residue() {
    let tmp = tempfile::tempdir().unwrap();
    let store = SkillStore::new(layout(tmp.path()));

    let err = store
        .install_from_dir(&fixture("bad-name"), local_source(), true, &[], 1000)
        .expect_err("Bad-Name 应被 validate_name 拒绝");
    assert!(matches!(err, SkillInstallError::Invalid(_)));

    assert!(
        !tmp.path().join("skills").exists(),
        "安装门失败不应创建 skills/ 目录"
    );
    assert!(store.list().unwrap().is_empty());
}

#[test]
fn untrusted_high_risk_rejected_but_trusted_allowed_with_findings_recorded() {
    let tmp = tempfile::tempdir().unwrap();
    let store = SkillStore::new(layout(tmp.path()));

    let err = store
        .install_from_dir(&fixture("evil-skill"), local_source(), false, &[], 1000)
        .expect_err("不受信来源命中 High 应拒装");
    assert!(matches!(err, SkillInstallError::HighRiskUntrusted(_)));
    assert!(
        !tmp.path().join("skills").join("evil-skill").exists(),
        "拒装不应留下目录"
    );

    let installed = store
        .install_from_dir(&fixture("evil-skill"), local_source(), true, &[], 1000)
        .expect("受信来源即便命中 High 也应放行");
    assert!(
        installed.trusted,
        "trusted 参数应原样记录（这次传的是 true）"
    );
    let high = installed
        .scan
        .findings
        .iter()
        .filter(|f| f.severity == Severity::High)
        .count();
    assert_eq!(high, 1, "命中记录应保留，供前端安装确认弹窗高亮");
}

#[test]
fn unknown_allowed_tool_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let store = SkillStore::new(layout(tmp.path()));

    // good-skill 声明 allowed-tools: bash python；known_tools 里只给 bash。
    let err = store
        .install_from_dir(
            &fixture("good-skill"),
            local_source(),
            true,
            &["bash".to_string()],
            1000,
        )
        .expect_err("声明了宿主不认识的工具应拒装");
    match err {
        SkillInstallError::UnknownTools(tools) => {
            assert_eq!(tools, vec!["python".to_string()]);
        }
        other => panic!("应为 UnknownTools，实际是 {other:?}"),
    }
    assert!(!tmp.path().join("skills").exists());
}

#[test]
fn grant_requires_tools_subset() {
    let tmp = tempfile::tempdir().unwrap();
    let store = SkillStore::new(layout(tmp.path()));
    store
        .install_from_dir(
            &fixture("good-skill"),
            local_source(),
            true,
            &["bash".to_string(), "python".to_string()],
            1000,
        )
        .unwrap();

    let err = store
        .grant("app1", "good-skill", &["bash".to_string()])
        .expect_err("应用没有 python 工具，不该能被授予");
    match err {
        SkillInstallError::ToolsNotSubset(tools) => assert_eq!(tools, vec!["python".to_string()]),
        other => panic!("应为 ToolsNotSubset，实际是 {other:?}"),
    }

    store
        .grant(
            "app1",
            "good-skill",
            &["bash".to_string(), "python".to_string()],
        )
        .expect("工具集足够时应授予成功");
    let grants = store.grants_for("app1").unwrap();
    assert_eq!(grants.len(), 1);
    assert!(grants[0].1, "grant 之后默认应是启用状态");
}

#[test]
fn enabled_dirs_reflect_grant_and_toggle() {
    let tmp = tempfile::tempdir().unwrap();
    let store = SkillStore::new(layout(tmp.path()));
    store
        .install_from_dir(
            &fixture("good-skill"),
            local_source(),
            true,
            &["bash".to_string(), "python".to_string()],
            1000,
        )
        .unwrap();
    store
        .install_from_dir(&fixture("script-skill"), local_source(), true, &[], 1000)
        .unwrap();

    // 未授予任何技能 -> 空集合。
    assert!(store.enabled_skill_dirs("app1").unwrap().is_empty());

    store
        .grant(
            "app1",
            "good-skill",
            &["bash".to_string(), "python".to_string()],
        )
        .unwrap();
    store.grant("app1", "script-skill", &[]).unwrap();

    let mut dirs = store.enabled_skill_dirs("app1").unwrap();
    dirs.sort();
    assert_eq!(dirs.len(), 2, "两个都已授予且默认启用");

    store.set_enabled("app1", "script-skill", false).unwrap();
    let dirs = store.enabled_skill_dirs("app1").unwrap();
    assert_eq!(dirs.len(), 1);
    assert!(dirs[0].ends_with("good-skill"));

    store.revoke("app1", "good-skill").unwrap();
    assert!(store.enabled_skill_dirs("app1").unwrap().is_empty());
}

#[test]
fn uninstall_removes_grants_everywhere() {
    let tmp = tempfile::tempdir().unwrap();
    let store = SkillStore::new(layout(tmp.path()));
    store
        .install_from_dir(
            &fixture("good-skill"),
            local_source(),
            true,
            &["bash".to_string(), "python".to_string()],
            1000,
        )
        .unwrap();
    store
        .grant(
            "app1",
            "good-skill",
            &["bash".to_string(), "python".to_string()],
        )
        .unwrap();
    store
        .grant(
            "app2",
            "good-skill",
            &["bash".to_string(), "python".to_string()],
        )
        .unwrap();

    let removed = store.uninstall("good-skill").expect("卸载应成功");
    assert!(removed);
    assert!(store.list().unwrap().is_empty());
    assert!(store.enabled_skill_dirs("app1").unwrap().is_empty());
    assert!(store.enabled_skill_dirs("app2").unwrap().is_empty());
    assert!(!tmp.path().join("skills").join("good-skill").exists());

    // 幂等：再卸载一次返回 false，不报错。
    assert!(!store.uninstall("good-skill").unwrap());
}

#[test]
fn duplicate_name_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let store = SkillStore::new(layout(tmp.path()));
    store
        .install_from_dir(
            &fixture("good-skill"),
            local_source(),
            true,
            &["bash".to_string(), "python".to_string()],
            1000,
        )
        .unwrap();

    let err = store
        .install_from_dir(
            &fixture("good-skill"),
            local_source(),
            true,
            &["bash".to_string(), "python".to_string()],
            2000,
        )
        .expect_err("同 id 第二次安装应被拒绝");
    assert!(matches!(err, SkillInstallError::Duplicate(id) if id == "good-skill"));
    assert_eq!(store.list().unwrap().len(), 1, "不应变成两条记录");
}

/// 审查修复轮 1 Minor：`skills-index.json` 被写成非法 JSON（损坏/被篡改）之后，
/// `list()`/`grant()` 必须 fail-closed——`list()` 返回 `Err`，不能悄悄当成
/// "空清单"糊弄过去（那会让宿主以为一个装满技能的用户其实什么都没装，静默
/// 丢失授权状态）；`grant()` 同样 `Err`，且不能用一份"重新算出来的新索引"
/// 覆盖掉损坏文件——覆盖会把还原损坏文件、人工排查的机会一起抹掉。
#[test]
fn corrupted_index_fails_closed_without_overwriting() {
    let tmp = tempfile::tempdir().unwrap();
    let store = SkillStore::new(layout(tmp.path()));

    // 先装一个真技能，确认损坏之前索引文件确实存在且非空。
    store
        .install_from_dir(
            &fixture("good-skill"),
            local_source(),
            true,
            &["bash".to_string(), "python".to_string()],
            1000,
        )
        .unwrap();

    let index_path = layout(tmp.path()).skills_index_path();
    let corrupt = "{ this is not valid json";
    std::fs::write(&index_path, corrupt).unwrap();

    let list_err = store.list().expect_err("损坏索引应 Err，不应假装是空清单");
    assert!(
        list_err.contains("解析失败"),
        "错误信息应点名解析失败：{list_err}"
    );

    let grant_err = store
        .grant(
            "app1",
            "good-skill",
            &["bash".to_string(), "python".to_string()],
        )
        .expect_err("损坏索引下 grant 应 Err");
    assert!(matches!(grant_err, SkillInstallError::Io(_)));

    let after = std::fs::read_to_string(&index_path).unwrap();
    assert_eq!(
        after, corrupt,
        "grant 失败路径不应覆盖损坏文件——内容必须原样保留"
    );
}
