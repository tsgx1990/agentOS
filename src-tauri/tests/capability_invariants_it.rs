//! spec §7：任一非特权能力被声明 ⇒ 必有人话呈现 且 必有执行层；清单字段全覆盖；无重名工具/方法。
use std::path::Path;
use std::sync::Mutex;
use super_agent_os::capabilities;
use super_agent_os::capability::{CallerIdentity, LaunchCtx, NO_EXTRA_PERMISSIONS};
use super_agent_os::mcp::McpManager;
use super_agent_os::paths::DataLayout;
use super_agent_os::permissions::{parse, Permissions};

/// `declared_capability_always_has_an_enforcement_point` 下面临时改写 `HOME`
/// 环境变量（见该测试文档）——`std::env::set_var`/`remove_var` 改的是进程全局
/// 状态，`cargo test` 默认多线程并行跑测试，不序列化就会和本文件里任何其它
/// 恰好也读/写 `HOME` 的测试竞争。本文件目前只有这一处碰 `HOME`，这把锁仍是
/// 防御性的——同一模式见 `pi_bin.rs::tests::ENV_LOCK`。
static HOME_LOCK: Mutex<()> = Mutex::new(());

/// RAII 守卫：构造时把 `HOME` 改成 `new_home`，析构（含 panic 展开期间——默认
/// `panic = unwind`，`Drop` 仍会跑）时无条件还原成构造前的值（`None` 则
/// `remove_var`）。断言失败在循环中途 panic 是这条测试的常规使用方式（`assert!`
/// 就是为了在第一个不满足的能力上立刻炸），若只用"函数体末尾裸代码"复原
/// `HOME`，panic 会跳过它，把改写后的 `HOME` 泄漏给同一测试二进制里后续运行
/// 的其它测试——必须用 `Drop` 兜底。
struct HomeVarGuard {
    old: Option<std::ffi::OsString>,
}
impl HomeVarGuard {
    fn set(new_home: &Path) -> Self {
        let old = std::env::var_os("HOME");
        std::env::set_var("HOME", new_home);
        Self { old }
    }
}
impl Drop for HomeVarGuard {
    fn drop(&mut self) {
        match self.old.take() {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
    }
}

/// 每个非特权能力一份「只声明它」的最小清单（新增能力必须在此登记，否则测试 `every_capability_has_a_minimal_manifest` 失败）。
/// 顺序须与 `capabilities::builtin()` 里非特权能力的注册顺序一致（该顺序是
/// connectors, agents.call, system.schedule, system.notifications, filesystem, ui.connectSrc, skills——
/// 不是 P6-A 设计稿草案里 filesystem 打头的顺序，`capabilities/mod.rs::builtin()` 才是真源）。
fn minimal_manifests() -> Vec<(&'static str, &'static str)> {
    vec![
        (
            "connectors",
            r#"{ "connectors": [{ "category": "filesystem", "access": "read" }] }"#,
        ),
        (
            "agents.call",
            r#"{ "agents": { "call": ["@superagent/summarizer"] } }"#,
        ),
        (
            "system.schedule",
            r#"{ "system": { "schedule": true }, "scheduledTasks": [{ "id": "t", "cron": "0 8 * * *", "prompt": "p" }] }"#,
        ),
        (
            "system.notifications",
            r#"{ "system": { "notifications": true } }"#,
        ),
        (
            "filesystem",
            r#"{ "filesystem": { "read": ["$DOWNLOADS"] } }"#,
        ),
        (
            "ui.connectSrc",
            r#"{ "ui": { "connectSrc": ["api.example.com"] } }"#,
        ),
        ("skills", r#"{ "skills": { "allow": true } }"#),
    ]
}

#[test]
fn every_capability_has_a_minimal_manifest() {
    let keys: Vec<&str> = capabilities::builtin()
        .iter()
        .filter(|c| !c.privileged())
        .map(|c| c.key())
        .collect();
    let listed: Vec<&str> = minimal_manifests().into_iter().map(|(k, _)| k).collect();
    assert_eq!(keys, listed, "非特权能力集合与最小清单登记不一致");
}

#[test]
fn declared_capability_always_renders_and_never_falls_back() {
    let reg = capabilities::builtin();
    let id = CallerIdentity::installing("x", false);
    for (key, json) in minimal_manifests() {
        let p = parse(json).unwrap();
        let cap = reg.iter().find(|c| c.key() == key).unwrap();
        assert!(cap.declared(&p, &id), "{key} 应被判定为已声明");
        let lines = reg.render_human(&p, &id);
        assert!(
            !lines.is_empty() && !lines.iter().any(|l| l == NO_EXTRA_PERMISSIONS),
            "{key} 渲染 {lines:?}"
        );
    }
}

/// P6-A 终审残留收口：`filesystem` 这一条最小清单声明的是 `$DOWNLOADS`
/// （`capabilities/filesystem.rs::confined_expand` 生产入口取本机真实
/// `dirs::download_dir()`/`dirs::home_dir()`，两者都读真实 `HOME` 环境变量）。
/// 此前这个断言隐式依赖"运行测试的机器上 `~/Downloads` 真实存在"——`materialize:
/// true` 下 `FilesystemCapability::launch` 会尝试 canonicalize 这个目录，不存在
/// 就静默返回 `Ok(None)`（见该模块 `expand_and_check_base` 文档，未落地路径的
/// 既有容忍度），`sandbox_read` 因此留空，若 `filesystem` 自己又没有
/// `methods()`/`InstallHook`/`UiCsp` 兜底，`has_launch || has_method || has_hook`
/// 就会判假——在一个 `HOME` 指向空目录（没有 `Downloads`）的环境下必然复现
/// （已用编译好的测试二进制配 `HOME=<empty tempdir>` 直接验证过）。
///
/// 修法：不依赖真实用户主目录，测试自己搭一个"确定存在 `Downloads`"的临时
/// `HOME` 并在断言期间把真实 `HOME` 环境变量临时指向它（`dirs` crate 在 macOS
/// 上就是读这个变量，见 `dirs-sys` 源码），断言完立刻还原——不是注入参数（生产
/// `confined_expand`/`confined_expand_write` 没有暴露可注入 base 的入口，那两个
/// 函数只在内部调用不吃外部 base 的 `confined_expand_with`），而是让"真实标准
/// 目录确实存在"这个前提条件在测试自己搭的沙盒 `HOME` 下必然成立，不再赌宿主
/// 机器的真实 `~/Downloads`。
#[test]
fn declared_capability_always_has_an_enforcement_point() {
    let _lock = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let fake_home = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(fake_home.path().join("Downloads")).unwrap();
    let _home_guard = HomeVarGuard::set(fake_home.path());

    let tmp = tempfile::tempdir().unwrap();
    let layout = DataLayout::new(tmp.path().to_path_buf());
    let mcp = McpManager::new();
    let sock = layout.mcp_socket_path("x");
    let ctx = LaunchCtx {
        app_id: "x",
        trusted: false,
        sandboxed: true,
        materialize: true,
        layout: &layout,
        hosttools_dir: Path::new("/ht"),
        socket_path: &sock,
        mcp: &mcp,
    };
    let reg = capabilities::builtin();
    for (key, json) in minimal_manifests() {
        let p = parse(json).unwrap();
        let cap = reg.iter().find(|c| c.key() == key).unwrap();
        assert!(!cap.enforcement().is_empty(), "{key} 未声明执行点");
        let c = cap.launch(&p, &ctx).unwrap();
        let has_launch = !c.bridges.is_empty()
            || !c.tools.is_empty()
            || !c.env.is_empty()
            || !c.sandbox_read.is_empty()
            || !c.sandbox_write.is_empty();
        let has_method = !cap.methods().is_empty();
        let has_hook = cap
            .enforcement()
            .contains(&super_agent_os::capability::Enforcement::InstallHook)
            || cap
                .enforcement()
                .contains(&super_agent_os::capability::Enforcement::UiCsp);
        assert!(
            has_launch || has_method || has_hook,
            "{key} 声明了却没有任何执行层"
        );
    }
}

#[test]
fn no_duplicate_tool_or_method_names_across_capabilities() {
    let tmp = tempfile::tempdir().unwrap();
    let layout = DataLayout::new(tmp.path().to_path_buf());
    let mcp = McpManager::new();
    let sock = layout.mcp_socket_path("superagent");
    let ctx = LaunchCtx {
        app_id: "superagent",
        trusted: true,
        sandboxed: true,
        materialize: true,
        layout: &layout,
        hosttools_dir: Path::new("/ht"),
        socket_path: &sock,
        mcp: &mcp,
    };
    let mut all = Permissions::default();
    all.system.notifications = true;
    all.agents.call.push("@a/b".into());
    let mut methods = std::collections::HashSet::new();
    let mut tools = std::collections::HashSet::new();
    for cap in capabilities::builtin().iter() {
        for m in cap.methods() {
            assert!(methods.insert(*m), "方法名重复：{m}");
        }
        for t in cap.launch(&all, &ctx).unwrap().tools {
            assert!(tools.insert(t.clone()), "工具名重复：{t}");
        }
    }
}

/// F4（review）：此前这条不变式比对的是两份**手写**列表（能力集合 vs. 上面那个
/// 字面量数组）——`Permissions` 新增一个顶层字段完全不影响这两份列表中的任何一份，
/// 测试照样全绿，2026-08-10 的缺陷正是这个形状（新字段没有对应能力，同意框却
/// 不知道该字段的存在）。现在改为真正读 `Permissions`（经 `serde_json::to_value`
/// 序列化后的顶层键集合）与 `capabilities::builtin()` 对账：
/// - 每个序列化出来的顶层键都必须出现在下面的别名表 `aliases` 里——新增字段却
///   忘了在这里登记，本测试直接失败并指名是哪个字段。
/// - 别名表里指向的每一个能力 key 都必须真的存在于 `builtin()` 的非特权能力集合
///   里（防止别名表自己写错/引用了一个已改名/已下线的能力）。
/// - 反向检查（原测试的另一半）：每个非特权能力 key 都必须被至少一个别名指向
///   （防止一个能力声明了自己却不对应任何清单字段——那它声明的到底是什么）。
#[test]
fn every_permissions_field_is_owned_by_some_capability() {
    let v = serde_json::to_value(Permissions::default()).unwrap();
    let keys: Vec<String> = v
        .as_object()
        .expect("Permissions 应序列化成 JSON 对象")
        .keys()
        .cloned()
        .collect();

    // 顶层字段（序列化后的 key，即清单 JSON 里实际出现的名字）-> 归属它的一个或
    // 多个能力 key。`system` 一个字段拆给两个能力（schedule/notifications 各拥有
    // 里面一个布尔子字段）；`scheduledTasks` 归 `system.schedule` 所有（只声明
    // tasks 不声明 schedule 时不得注册，Task5 已测，这里只对账字段表本身）。
    let aliases: &[(&str, &[&str])] = &[
        ("filesystem", &["filesystem"]),
        ("ui", &["ui.connectSrc"]),
        ("agents", &["agents.call"]),
        ("system", &["system.schedule", "system.notifications"]),
        ("connectors", &["connectors"]),
        ("scheduledTasks", &["system.schedule"]),
        ("skills", &["skills"]),
    ];

    for key in &keys {
        assert!(
            aliases.iter().any(|(field, _)| field == key),
            "Permissions 新增了字段 {key:?}，但它没有在本测试的别名表里登记——\
             请新增/复用一个 capability 来实现并 owning 它，然后把 {key:?} 加进 `aliases`（这正是 2026-08-10 缺陷的形状：新字段没有能力归属，同意框不会呈现它）"
        );
    }

    let owned: Vec<&str> = capabilities::builtin()
        .iter()
        .filter(|c| !c.privileged())
        .map(|c| c.key())
        .collect();
    for (field, targets) in aliases {
        for t in *targets {
            assert!(owned.contains(t), "别名表 {field} -> {t} 指向一个不存在/已下线的非特权能力 key，实际非特权能力集合：{owned:?}");
        }
    }
    for cap_key in &owned {
        assert!(
            aliases.iter().any(|(_, targets)| targets.contains(cap_key)),
            "非特权能力 {cap_key} 没有被任何 Permissions 字段的别名指向——它声明的字段来自哪里？"
        );
    }
}
