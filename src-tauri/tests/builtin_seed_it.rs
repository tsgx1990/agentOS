// P4 whole-branch review 修复（I1/I2）：内置 Maker 启动播种 + 起步向导安装
// 内置样例的白名单/trusted 覆盖。
//
// I1（生产装机缺口）：`OnboardingWizard.tsx` 此前直接把仓库相对路径
// `"samples/maker"` 传给 `install_app`（`Path::new(&source_path)`，见
// `lib.rs::install_app`，无 base 解析——生产环境 CWD 不可预测，运行时会失败）；
// 而且启动期没有任何代码播种内置应用，`samples/` 也从未作为 Tauri 资源打包
// 进 `resource_dir()`。净效果：唯一列出的旗舰功能 Maker 在生产环境里没有任何
// 真正跑得通的安装路径。
//
// I2（trust 策略缺口）：即使装上了，`handle_install`/`install_app` 此前对
// Maker 走的都是 `trusted=false`——但 Maker 是 spec §3.1 定义的第一方内置
// 应用，macOS 上 untrusted 应用的 pi 子进程运行在 `sandbox.rs` 的
// `(deny network*)` 规则下，连不上模型 API，生成式对话根本无法运行。
//
// 本文件覆盖两条新增的自由函数（均不依赖 `tauri::AppHandle`，靠显式传入
// `samples_dir` 注入依赖，供集成测试直接调用，不需要真正的 Tauri runtime）：
// - `super_agent_os::seed_builtin_maker`：启动期播种，`trusted==true`，幂等
//   （已装则跳过，不报错）。
// - `super_agent_os::install_builtin_sample_core`：起步向导安装内置样例的
//   可测试核心——白名单校验 + `trusted=true` 安装；拒绝任何不在白名单里的
//   `name`（含路径穿越形态），不产生任何安装副作用。
//
// 与 Maker **生成**出来的应用（`maker::resolve_install`，`trusted=false`，
// 未受信第三方输出，必须走沙盒）是两条完全不同的安装路径，本文件不触碰
// 后者，`maker_it.rs`/`p4_maker_e2e_it.rs` 对它的覆盖保持不变。

use super_agent_os::maker::MAKER_APP_ID;
use super_agent_os::paths::DataLayout;
use super_agent_os::registry::RegistryStore;
use super_agent_os::{install_builtin_sample_core, seed_builtin_maker};

/// 仓库真实的 `samples/` 目录：`CARGO_MANIFEST_DIR` 是 `src-tauri/`，上一级
/// 才是仓库根——这是"repo-root 相对的 samples/"这条 dev/test 回退路径背后的
/// 真实目录，这里显式算出绝对路径，不依赖 `cargo test` 的当前工作目录（避免
/// 从 workspace 根还是 `src-tauri/` 跑测试导致相对路径不一致）。
fn real_samples_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../samples")
}

fn temp_layout() -> (tempfile::TempDir, DataLayout, RegistryStore) {
    let tmp = tempfile::tempdir().unwrap();
    let layout = DataLayout::new(tmp.path().to_path_buf());
    let reg = RegistryStore::new(layout.registry_path());
    (tmp, layout, reg)
}

#[test]
fn seed_builtin_maker_installs_trusted_on_empty_registry() {
    let (_tmp, layout, reg) = temp_layout();
    assert!(reg.get(MAKER_APP_ID).is_none());

    seed_builtin_maker(&real_samples_dir(), &layout, &reg).unwrap();

    let rec = reg.get(MAKER_APP_ID).expect("Maker 应已被播种进 registry");
    assert_eq!(rec.app_id, MAKER_APP_ID);
    assert!(
        rec.trusted,
        "Maker 是第一方内置应用，必须 trusted=true（I2）"
    );
}

#[test]
fn seed_builtin_maker_is_idempotent() {
    let (_tmp, layout, reg) = temp_layout();
    seed_builtin_maker(&real_samples_dir(), &layout, &reg).unwrap();
    // 再调一次（模拟第二次应用启动）：不报错，registry 里仍然只有一条 Maker。
    seed_builtin_maker(&real_samples_dir(), &layout, &reg).unwrap();

    let all = reg.load();
    let makers: Vec<_> = all.iter().filter(|a| a.app_id == MAKER_APP_ID).collect();
    assert_eq!(makers.len(), 1, "重复播种不应产生第二条 Maker 记录");
    assert!(makers[0].trusted);
}

#[test]
fn install_builtin_sample_core_installs_whitelisted_sample_trusted() {
    let (_tmp, layout, reg) = temp_layout();
    let rec =
        install_builtin_sample_core("todo-notes", &real_samples_dir(), &layout, &reg).unwrap();
    assert_eq!(rec.app_id, "superagent__todo-notes");
    assert!(rec.trusted, "第一方内置样例安装必须 trusted=true");
    assert!(reg.get("superagent__todo-notes").is_some());
}

#[test]
fn publish_app_core_exports_installed_app_and_writes_index_entry() {
    // P5 T12：装一个样例 → 发布 → 导出目录 + published/index.json 落位，条目带权限摘要。
    let (_tmp, layout, reg) = temp_layout();
    install_builtin_sample_core("researcher", &real_samples_dir(), &layout, &reg).unwrap();

    let entry = super_agent_os::publish_app_core("superagent__researcher", &layout).unwrap();
    assert_eq!(entry.name, "@superagent/researcher");
    assert_eq!(entry.source, "superagent__researcher");
    assert!(
        entry.permissions.iter().any(|p| p.contains("调用其他应用")),
        "researcher 的权限摘要应含调用声明，实际：{:?}",
        entry.permissions
    );

    // 导出包目录 + 索引文件都落位。
    assert!(
        layout
            .published_app_dir("superagent__researcher")
            .join("package.json")
            .is_file(),
        "发布应把包导到 published/<app_id>/"
    );
    let idx = layout.published_dir().join("index.json");
    assert!(idx.is_file(), "发布应生成 published/index.json");
    let entries =
        super_agent_os::market::parse_index(&std::fs::read_to_string(&idx).unwrap()).unwrap();
    assert!(entries.iter().any(|e| e.name == "@superagent/researcher"));
}

#[test]
fn publish_app_core_errors_for_uninstalled_app() {
    let (_tmp, layout, _reg) = temp_layout();
    assert!(super_agent_os::publish_app_core("superagent__nope", &layout).is_err());
}

#[test]
fn market_fetch_index_core_reads_bundled_demo_index() {
    // P5 T9：内置 demo 精选市场索引能被读+解析，含协作样例条目 + 非空权限摘要。
    let entries = super_agent_os::market_fetch_index_core(&real_samples_dir(), None).unwrap();
    assert!(!entries.is_empty(), "内置 demo 市场应有条目");
    let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
    assert!(
        names.contains(&"@superagent/researcher") && names.contains(&"@superagent/summarizer"),
        "demo 市场应含协作样例，实际：{names:?}"
    );
    let researcher = entries
        .iter()
        .find(|e| e.name == "@superagent/researcher")
        .unwrap();
    assert!(
        !researcher.permissions.is_empty(),
        "条目应带人话权限摘要供装前预览"
    );
    assert_eq!(researcher.source, "researcher");
}

#[test]
fn install_builtin_sample_core_installs_researcher_and_summarizer_collab_pair() {
    // P5 T6：协作样例对儿都在白名单里、都能装、registry 出现对齐的 app_id——
    // 互联 e2e（T7）与手工里程碑装的就是这一对。
    let (_tmp, layout, reg) = temp_layout();
    let r = install_builtin_sample_core("researcher", &real_samples_dir(), &layout, &reg).unwrap();
    let s = install_builtin_sample_core("summarizer", &real_samples_dir(), &layout, &reg).unwrap();
    assert_eq!(r.app_id, "superagent__researcher");
    assert_eq!(s.app_id, "superagent__summarizer");
    assert!(
        r.trusted && s.trusted,
        "第一方内置样例安装必须 trusted=true"
    );
    assert!(reg.get("superagent__researcher").is_some());
    assert!(reg.get("superagent__summarizer").is_some());
}

#[test]
fn install_builtin_sample_core_rejects_unknown_name_and_installs_nothing() {
    let (_tmp, layout, reg) = temp_layout();
    let err = install_builtin_sample_core("not-a-real-sample", &real_samples_dir(), &layout, &reg);
    assert!(err.is_err());
    assert!(reg.load().is_empty(), "拒绝的名字不应产生任何安装副作用");
}

#[test]
fn install_builtin_sample_core_rejects_path_traversal_name() {
    let (_tmp, layout, reg) = temp_layout();
    let err = install_builtin_sample_core("../../etc", &real_samples_dir(), &layout, &reg);
    assert!(err.is_err());
    assert!(
        reg.load().is_empty(),
        "路径穿越形态的名字不应产生任何安装副作用"
    );
}
