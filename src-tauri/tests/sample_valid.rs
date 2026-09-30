use super_agent_os::permissions::{self, Access};

// 验证第一方样例应用 todo-notes 能经 pkg::load_and_validate 校验通过。
// 该样例是里程碑的参考实现，练全 window.superagent 桥（prompt / on / state.get/set）。
#[test]
fn sample_todo_notes_is_valid() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../samples/todo-notes");
    let m = super_agent_os::pkg::load_and_validate(&dir).expect("样例应校验通过");
    assert_eq!(m.app_id(), "superagent__todo-notes");
    assert_eq!(m.superagent.category, "life");
}

// 验证 Maker 内置包（应用工坊）能经 pkg::load_and_validate 校验通过（P4 T10）。
// 两条跨任务契约在这里被同时锁定：
// 1. 派生 app_id 必须与 `maker::MAKER_APP_ID` 完全一致——T9（session_mgr.rs）靠
//    `app_id == MAKER_APP_ID` 认出"这次打开的是 Maker"才会注入 maker_bridge，
//    若 T10 的 package.json 产出别的 app_id，注入静默失效（不报错，但拿不到三个
//    `__host_maker_*__` 工具），所以这里直接断言等于共享常量，而非字面量
//    `"superagent"`，防止两处字面量漂移。
// 2. category 必须是 "maker" 且是宿主承认的已知类目（前端 `src/lib/registry.ts`
//    的 `CATEGORIES` 列表已含 "maker" 项）——Rust 侧 `pkg::load_and_validate` 目前
//    对 `category` 字段本身不做枚举白名单校验（自由字符串，见 pkg.rs
//    `SuperagentField::category` 字段），所以这里的断言锁的是"值确实是
//    'maker'"，不是"通过了某个 Rust 枚举校验"。
#[test]
fn sample_maker_is_valid() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../samples/maker");
    let m = super_agent_os::pkg::load_and_validate(&dir).expect("Maker 样例应校验通过");
    assert_eq!(m.app_id(), super_agent_os::maker::MAKER_APP_ID);
    assert_eq!(m.superagent.category, "maker");
}

// P4 Task11：三类招牌极简范例包——信息获取 / 创作 / 自动化，各自展示一种
// P1 能力面（定时任务、纯对话、连接器读写）。三者 app_id 均不同于 Maker 的
// 保留 app_id（"superagent"，见上面 `sample_maker_is_valid`），category 取
// 前端 `src/lib/registry.ts` `CATEGORIES` 的真实枚举值（"info"/"create"/
// "automation"）。

/// daily-brief（信息获取）：既声明 `scheduledTasks`（每日定时简报）又声明
/// `connectors`（只读 filesystem，用作简报素材来源）——覆盖 brief 对这一类
/// 的双重要求。字段名/取值直接对照 `permissions.rs` 的
/// `connectors_and_schedule_parse_full`/`p3_milestone_it.rs` 的 notes-writer
/// fixture，不是凭空拼的形状。
#[test]
fn sample_daily_brief_is_valid() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../samples/daily-brief");
    let m = super_agent_os::pkg::load_and_validate(&dir).expect("daily-brief 样例应校验通过");
    assert_eq!(m.app_id(), "superagent__daily-brief");
    assert_eq!(m.superagent.category, "info");

    let perms = permissions::load(&dir, &m.superagent.permissions)
        .expect("daily-brief 的 permissions.json 应能解析");
    assert!(
        !perms.scheduled_tasks.is_empty(),
        "daily-brief 应声明至少一个定时任务"
    );
    assert_eq!(perms.scheduled_tasks[0].id, "morning-brief");
    assert_eq!(perms.scheduled_tasks[0].cron, "0 8 * * *");
    assert!(
        perms.scheduled_tasks[0].catch_up,
        "省略 catchUp 应落回默认 true"
    );
    assert!(
        !perms.connectors.is_empty(),
        "daily-brief 应声明至少一个连接器"
    );
    assert_eq!(perms.connectors[0].category, "filesystem");
    assert_eq!(perms.connectors[0].access, Access::Read);
    assert!(
        perms.system.schedule,
        "声明了 scheduledTasks 就必须打开 system.schedule 权限，否则调度器不会注册它"
    );
}

/// writing-helper（创作）：纯对话 + 最小工具，不声明 connectors/scheduledTasks
/// ——与 daily-brief/connector-demo 形成对照，证明"极简"范例里也有完全不需要
/// 这两类高级权限的一档。
#[test]
fn sample_writing_helper_is_valid() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../samples/writing-helper");
    let m = super_agent_os::pkg::load_and_validate(&dir).expect("writing-helper 样例应校验通过");
    assert_eq!(m.app_id(), "superagent__writing-helper");
    assert_eq!(m.superagent.category, "create");

    let perms = permissions::load(&dir, &m.superagent.permissions)
        .expect("writing-helper 的 permissions.json 应能解析");
    assert!(
        perms.connectors.is_empty(),
        "writing-helper 是纯对话应用，不应声明连接器"
    );
    assert!(
        perms.scheduled_tasks.is_empty(),
        "writing-helper 不应声明定时任务"
    );
}

/// connector-demo（自动化）：一个 filesystem 连接器，`access` 取
/// `readwrite`（H5 要能触发写操作，只读不够），对照 `permissions.rs`
/// `Access` 枚举（`Read`/`ReadWrite`，`#[serde(rename_all = "lowercase")]`
/// 故 JSON 取值是 `"read"`/`"readwrite"`）。
#[test]
fn sample_connector_demo_is_valid() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../samples/connector-demo");
    let m = super_agent_os::pkg::load_and_validate(&dir).expect("connector-demo 样例应校验通过");
    assert_eq!(m.app_id(), "superagent__connector-demo");
    assert_eq!(m.superagent.category, "automation");

    let perms = permissions::load(&dir, &m.superagent.permissions)
        .expect("connector-demo 的 permissions.json 应能解析");
    assert!(
        !perms.connectors.is_empty(),
        "connector-demo 应声明至少一个连接器"
    );
    assert_eq!(perms.connectors[0].category, "filesystem");
    assert_eq!(
        perms.connectors[0].access,
        Access::ReadWrite,
        "H5 要触发写操作，access 必须是 readwrite，只读权限做不到"
    );
}

// P5 T6：researcher / summarizer 协作样例——演示"两个应用协作完成一个任务"。
// researcher 声明 agents.call:["@superagent/summarizer"]（调用方），summarizer 无
// agents.call（被调方）。两者 app_id 对齐是互联白名单闸能匹配的关键：
// researcher 清单里的 "@superagent/summarizer" 经 normalize_app_id 归一后 ==
// summarizer 包 name 归一后的 app_id "superagent__summarizer"。

/// researcher（调用方）：声明调用 summarizer 的权限。
#[test]
fn sample_researcher_is_valid_and_declares_call_to_summarizer() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../samples/researcher");
    let m = super_agent_os::pkg::load_and_validate(&dir).expect("researcher 样例应校验通过");
    assert_eq!(m.app_id(), "superagent__researcher");
    assert_eq!(m.superagent.category, "automation");

    let perms = permissions::load(&dir, &m.superagent.permissions)
        .expect("researcher 的 permissions.json 应能解析");
    assert_eq!(
        perms.agents.call,
        vec!["@superagent/summarizer".to_string()]
    );
    // 白名单闸对齐：清单条目归一后 == summarizer 的 app_id。
    assert_eq!(
        super_agent_os::pkg::normalize_app_id(&perms.agents.call[0]),
        "superagent__summarizer"
    );
}

/// summarizer（被调方）：无 agents.call，纯做精简。
#[test]
fn sample_summarizer_is_valid_with_no_call_declaration() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../samples/summarizer");
    let m = super_agent_os::pkg::load_and_validate(&dir).expect("summarizer 样例应校验通过");
    assert_eq!(m.app_id(), "superagent__summarizer");
    assert_eq!(m.superagent.category, "automation");

    let perms = permissions::load(&dir, &m.superagent.permissions)
        .expect("summarizer 的 permissions.json 应能解析");
    assert!(perms.agents.call.is_empty(), "被调方不应声明 agents.call");
}
