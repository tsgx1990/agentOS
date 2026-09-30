// Task4/Task5（P4）集成测试：`handle_maker_request` 的
// `__host_maker_stage_write__`/`__host_maker_install__` 真正实现。
//
// T3 只打通了分发管道（stub 恒定 `{ok:true}`）；T4 把 `stage_write` 换成了真正的
// 落盘实现：先校验 `draft_id` 本身是单一安全路径段，再用 `resolve_staging_path`
// （T2）算出安全路径 → mkdir -p 父目录 → 写文件 → 回 `{ok:true, path}`。逃逸的
// `rel_path` 必须不落盘、回 `{ok:false, error}`；恶意 `draft_id`（例如含 `..`）
// 同样必须在构造 `maker_staging_dir` 之前就被拒绝，否则它能绕开
// `resolve_staging_path` 的防线，直接从 `maker_staging_dir` 的拼接阶段逃逸到
// `maker-staging/` 根目录之外。
//
// T5 把 `install` 换成真正的实现：走 P1 标准安装流程（`pkg::load_and_validate` +
// `install::install_or_upgrade`），但接入执行期决策的专用 install-confirm seam
// （见 `docs/superpowers/plans/2026-07-18-p4-maker-flagship-onboarding.md`"执行
// 期决策：T5 安装权限确认 seam"）——`__host_maker_install__` 分支自身只校验 +
// 登记 pending（不安装任何东西），真正的安装动作由 `maker::resolve_install`
// （`allow=true`）触发，或 `allow=false` 时丢弃、不安装。`handle_maker_request`
// 签名因此新增了末位 `manager: &McpManager` 参数——本文件所有既有调用点都要
// 跟着补上这个参数（纯接线改动，不改变 stage_write 已验证过的行为）。

use super_agent_os::maker::{self, handle_maker_request};
use super_agent_os::mcp::McpManager;
use super_agent_os::paths::DataLayout;
use super_agent_os::pkg;
use super_agent_os::registry::RegistryStore;

fn temp_layout() -> (tempfile::TempDir, DataLayout, McpManager) {
    let tmp = tempfile::tempdir().unwrap();
    let layout = DataLayout::new(tmp.path().to_path_buf());
    (tmp, layout, McpManager::new())
}

/// 在 `layout.maker_staging_dir(draft_id)` 下写一份最小合法包（同
/// `install.rs`/`pkg.rs` 测试用的合法包形状：`package.json` 含
/// `pi-package`+`superagent-app` 关键字、`schemaVersion:1`、UI/permissions 文件
/// 真实存在），供 `__host_maker_install__` 的 happy-path 测试使用。
fn write_valid_maker_draft(layout: &DataLayout, draft_id: &str) {
    let dir = layout.maker_staging_dir(draft_id);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("package.json"),
        r#"{
          "name": "@superagent/maker-demo", "version": "1.0.0",
          "keywords": ["pi-package", "superagent-app"],
          "engines": { "superagent-host": ">=1.0.0, <2.0.0" },
          "superagent": { "schemaVersion": 1, "displayName": "Maker示例",
            "category": "life", "ui": "ui/index.html", "permissions": "permissions.json" }
        }"#,
    )
    .unwrap();
    std::fs::write(dir.join("permissions.json"), "{}").unwrap();
    std::fs::create_dir_all(dir.join("ui")).unwrap();
    std::fs::write(
        dir.join("ui/index.html"),
        "<html><body>maker demo</body></html>",
    )
    .unwrap();
}

/// 缺 `superagent` 块的非法草稿：`Manifest::superagent` 是必填字段（非
/// `Option`），缺失时 `serde_json::from_str` 解析本身就会失败——
/// `pkg::load_and_validate` 因此在最早一步就拒绝，符合 Task5 brief"缺
/// `superagent` 块的非法草稿 → install 拒、不入列表"这条要求。
fn write_invalid_maker_draft_missing_superagent_block(layout: &DataLayout, draft_id: &str) {
    let dir = layout.maker_staging_dir(draft_id);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("package.json"),
        r#"{
          "name": "@superagent/maker-bad", "version": "1.0.0",
          "keywords": ["pi-package", "superagent-app"],
          "engines": { "superagent-host": ">=1.0.0, <2.0.0" }
        }"#,
    )
    .unwrap();
}

/// 核心场景：写 `package.json` 到某 draft 的暂存目录，文件确实落在
/// `resolve_staging_path` 解析出的路径上，内容一致，返回 `{ok:true, path}`。
#[tokio::test]
async fn stage_write_writes_file_into_draft_staging_dir() {
    let (_tmp, layout, manager) = temp_layout();

    let resp = handle_maker_request(
        "app-1",
        "__host_maker_stage_write__",
        serde_json::json!({
            "draft_id": "draft-1",
            "rel_path": "package.json",
            "content": "{\"name\":\"demo\"}",
        }),
        &layout,
        &manager,
    )
    .await;

    assert_eq!(
        resp["ok"],
        serde_json::json!(true),
        "应成功，实际：{resp:?}"
    );

    // 此时 stage_write 已经 mkdir -p 过暂存目录，canonicalize 应该成功。
    let canonical_staging_dir =
        std::fs::canonicalize(layout.maker_staging_dir("draft-1")).expect("暂存目录应已被创建");
    let expected_file = canonical_staging_dir.join("package.json");

    assert!(
        expected_file.exists(),
        "文件应写入暂存目录：{expected_file:?}"
    );
    let written = std::fs::read_to_string(&expected_file).unwrap();
    assert_eq!(written, "{\"name\":\"demo\"}");

    let path_field = resp["path"].as_str().expect("应返回 path 字段");
    assert_eq!(
        std::path::PathBuf::from(path_field),
        expected_file,
        "返回的 path 应正是解析出的暂存文件路径"
    );
}

/// 逃逸场景：`rel_path` 含 `..`，必须 `{ok:false, error}` 且不写出任何文件——
/// 宿主端核验：暂存根目录（`maker-staging/`）本身、它的父目录（host root）里，
/// 都不应该出现名为 "evil" 的文件。
#[tokio::test]
async fn stage_write_rejects_escaping_rel_path_and_writes_nothing() {
    let (tmp, layout, manager) = temp_layout();

    let resp = handle_maker_request(
        "app-1",
        "__host_maker_stage_write__",
        serde_json::json!({
            "draft_id": "draft-1",
            "rel_path": "../evil",
            "content": "pwned",
        }),
        &layout,
        &manager,
    )
    .await;

    assert_eq!(
        resp["ok"],
        serde_json::json!(false),
        "逃逸应失败，实际：{resp:?}"
    );
    assert!(
        resp.get("error").is_some(),
        "失败应带 error 字段，实际：{resp:?}"
    );

    let staging_root = layout.maker_staging_root();
    assert!(!staging_root.join("evil").exists(), "不应写出到暂存根");
    assert!(!tmp.path().join("evil").exists(), "不应写出到 host root");
}

/// 缺参 / 参数类型错误 → `{ok:false}`，不 panic。
#[tokio::test]
async fn stage_write_missing_or_wrong_type_params_returns_ok_false_no_panic() {
    let (_tmp, layout, manager) = temp_layout();

    let missing_content = handle_maker_request(
        "app-1",
        "__host_maker_stage_write__",
        serde_json::json!({ "draft_id": "draft-1", "rel_path": "a.txt" }),
        &layout,
        &manager,
    )
    .await;
    assert_eq!(
        missing_content["ok"],
        serde_json::json!(false),
        "缺 content 应失败"
    );

    let missing_rel_path = handle_maker_request(
        "app-1",
        "__host_maker_stage_write__",
        serde_json::json!({ "draft_id": "draft-1", "content": "hi" }),
        &layout,
        &manager,
    )
    .await;
    assert_eq!(
        missing_rel_path["ok"],
        serde_json::json!(false),
        "缺 rel_path 应失败"
    );

    let missing_draft_id = handle_maker_request(
        "app-1",
        "__host_maker_stage_write__",
        serde_json::json!({ "rel_path": "a.txt", "content": "hi" }),
        &layout,
        &manager,
    )
    .await;
    assert_eq!(
        missing_draft_id["ok"],
        serde_json::json!(false),
        "缺 draft_id 应失败"
    );

    // 类型错误（数字而非字符串）也应被当作"缺失"处理，而不是 panic。
    let wrong_type = handle_maker_request(
        "app-1",
        "__host_maker_stage_write__",
        serde_json::json!({ "draft_id": 123, "rel_path": "a.txt", "content": "hi" }),
        &layout,
        &manager,
    )
    .await;
    assert_eq!(
        wrong_type["ok"],
        serde_json::json!(false),
        "draft_id 类型错误应失败"
    );
}

/// 恶意 `draft_id`（含 `..`）必须在拼 `maker_staging_dir` 之前被拒绝——否则
/// `mkdir -p root/maker-staging/../x` 会真的在 `maker-staging/` 之外创建目录，
/// 完全绕开 `resolve_staging_path` 的escape防线。
#[tokio::test]
async fn stage_write_rejects_malicious_draft_id_no_dir_created_outside_root() {
    let (tmp, layout, manager) = temp_layout();

    let resp = handle_maker_request(
        "app-1",
        "__host_maker_stage_write__",
        serde_json::json!({
            "draft_id": "../x",
            "rel_path": "a.txt",
            "content": "pwned",
        }),
        &layout,
        &manager,
    )
    .await;

    assert_eq!(
        resp["ok"],
        serde_json::json!(false),
        "恶意 draft_id 应失败，实际：{resp:?}"
    );
    assert!(resp.get("error").is_some());

    assert!(
        !tmp.path().join("x").exists(),
        "不应在 host root 下创建 x 目录/文件"
    );
}

/// 恶意 `draft_id` 含路径分隔符 `/` 同样应被拒绝（draft_id 必须是单一路径段）。
#[tokio::test]
async fn stage_write_rejects_draft_id_containing_slash() {
    let (tmp, layout, manager) = temp_layout();

    let resp = handle_maker_request(
        "app-1",
        "__host_maker_stage_write__",
        serde_json::json!({ "draft_id": "a/b", "rel_path": "x.txt", "content": "hi" }),
        &layout,
        &manager,
    )
    .await;

    assert_eq!(
        resp["ok"],
        serde_json::json!(false),
        "draft_id 含 `/` 应失败，实际：{resp:?}"
    );
    assert!(
        !tmp.path().join("maker-staging").join("a").exists(),
        "不应创建中间目录 a/"
    );
}

/// 恶意 `draft_id` 含反斜线 `\` 同样应被拒绝——即便运行平台是 Unix（此时 `\`
/// 本身不是路径分隔符），规则也必须统一生效，因为同一份 `draft_id` 规则要在
/// Windows 上也安全。
#[tokio::test]
async fn stage_write_rejects_draft_id_containing_backslash() {
    let (_tmp, layout, manager) = temp_layout();

    let resp = handle_maker_request(
        "app-1",
        "__host_maker_stage_write__",
        serde_json::json!({ "draft_id": "a\\b", "rel_path": "x.txt", "content": "hi" }),
        &layout,
        &manager,
    )
    .await;

    assert_eq!(
        resp["ok"],
        serde_json::json!(false),
        "draft_id 含 `\\` 应失败，实际：{resp:?}"
    );
}

/// 恶意 `draft_id` 含 NUL 字节同样应被拒绝（防截断类注入）。
#[tokio::test]
async fn stage_write_rejects_draft_id_containing_nul() {
    let (_tmp, layout, manager) = temp_layout();

    let resp = handle_maker_request(
        "app-1",
        "__host_maker_stage_write__",
        serde_json::json!({ "draft_id": "\0", "rel_path": "x.txt", "content": "hi" }),
        &layout,
        &manager,
    )
    .await;

    assert_eq!(
        resp["ok"],
        serde_json::json!(false),
        "draft_id 含 NUL 应失败，实际：{resp:?}"
    );
}

/// `draft_id == "."`：会让 `maker_staging_root().join(".")` 坍缩回暂存根本身，
/// 丧失"按 draft 隔离"的不变式（不同 draft 共享同一个目录），必须被拒绝。
#[tokio::test]
async fn stage_write_rejects_draft_id_dot() {
    let (_tmp, layout, manager) = temp_layout();

    let resp = handle_maker_request(
        "app-1",
        "__host_maker_stage_write__",
        serde_json::json!({ "draft_id": ".", "rel_path": "x.txt", "content": "hi" }),
        &layout,
        &manager,
    )
    .await;

    assert_eq!(
        resp["ok"],
        serde_json::json!(false),
        "draft_id 为 `.` 应失败，实际：{resp:?}"
    );
    // 若这里没被拒绝，`.` 会坍缩回暂存根，文件本会直接写到暂存根下的 x.txt。
    assert!(
        !layout.maker_staging_root().join("x.txt").exists(),
        "不应把文件写到暂存根（`.` 坍缩场景）"
    );
}

/// CRITICAL：`draft_id` 含 `:`（如 `"C:evil"`）必须被拒绝。在 Windows 上，
/// `maker_staging_root().join("C:evil")` 会整体替换掉 root（`PathBuf::push`
/// 对"有前缀无根"路径的文档语义——参见 `resolve_staging_path` 对同一逃逸面的
/// 详细说明），逃出 `maker-staging/` 落到暂存树之外，`create_dir_all` 会在
/// 宿主数据目录之外真的建目录。本测试在所有平台上都跑（拒绝是纯词法的，不
/// 依赖平台），宿主侧核验：既没有在暂存根下、也没有在暂存根的宿主 root 下，
/// 出现任何名为 `C:evil` 的目录/文件。
#[tokio::test]
async fn stage_write_rejects_draft_id_containing_colon_windows_drive_escape() {
    let (tmp, layout, manager) = temp_layout();

    let resp = handle_maker_request(
        "app-1",
        "__host_maker_stage_write__",
        serde_json::json!({ "draft_id": "C:evil", "rel_path": "x.txt", "content": "hi" }),
        &layout,
        &manager,
    )
    .await;

    assert_eq!(
        resp["ok"],
        serde_json::json!(false),
        "draft_id 含 `:` 应失败，实际：{resp:?}"
    );
    assert!(
        !tmp.path().join("C:evil").exists(),
        "不应在 host root 下创建 C:evil"
    );
    assert!(
        !layout.maker_staging_root().join("C:evil").exists(),
        "不应在暂存根下创建 C:evil"
    );
}

// ---------------------------------------------------------------------------
// T5：`__host_maker_install__` + `resolve_install`
// ---------------------------------------------------------------------------

/// Test A（happy path）：合法草稿 → `__host_maker_install__` 只登记 pending、
/// 自身不安装（registry 仍为空）→ `resolve_install(..., allow=true)` 才真正
/// 安装，装完后 registry 里能查到、包目录能通过 `pkg::load_and_validate`、且
/// `trusted` 字段确实是 `false`（Maker 输出不免检 P2 受限模式/沙盒）。
#[tokio::test]
async fn install_happy_path_registers_pending_then_installs_only_after_confirm() {
    let (_tmp, layout, manager) = temp_layout();
    let registry = RegistryStore::new(layout.registry_path());
    write_valid_maker_draft(&layout, "draft-x");

    let resp = handle_maker_request(
        "app-1",
        "__host_maker_install__",
        serde_json::json!({ "draft_id": "draft-x" }),
        &layout,
        &manager,
    )
    .await;

    assert_eq!(
        resp["pending_confirm"],
        serde_json::json!(true),
        "合法草稿应返回 pending_confirm，实际：{resp:?}"
    );
    let confirm_id = resp["confirm_id"]
        .as_str()
        .filter(|s| !s.is_empty())
        .expect("应返回非空 confirm_id")
        .to_string();

    // 分支自身不安装：确认前 registry 应仍为空。
    assert!(registry.load().is_empty(), "确认前不应安装任何东西");

    let installed = maker::resolve_install(&manager, &layout, &registry, &confirm_id, true)
        .expect("allow=true 且 confirm_id 合法应成功")
        .expect("allow=true 应返回 Some(app)");

    assert_eq!(installed.app_id, "superagent__maker-demo");
    assert!(!installed.trusted, "Maker 输出必须 trusted=false，不免检");

    let from_registry = registry
        .get(&installed.app_id)
        .expect("确认后应已在 registry 中");
    assert!(!from_registry.trusted);

    let installed_dir = layout.packages_dir(&installed.app_id);
    pkg::load_and_validate(&installed_dir).expect("已装包目录应能通过 load_and_validate");
}

/// Test B（reject）：注册一条合法 pending 后 `resolve_install(..., allow=false)`
/// → `Ok(None)`，且该 app 没有被安装（registry 仍为空）。
#[tokio::test]
async fn install_reject_confirms_nothing_installed() {
    let (_tmp, layout, manager) = temp_layout();
    let registry = RegistryStore::new(layout.registry_path());
    write_valid_maker_draft(&layout, "draft-y");

    let resp = handle_maker_request(
        "app-1",
        "__host_maker_install__",
        serde_json::json!({ "draft_id": "draft-y" }),
        &layout,
        &manager,
    )
    .await;
    let confirm_id = resp["confirm_id"]
        .as_str()
        .expect("应有 confirm_id")
        .to_string();

    let result = maker::resolve_install(&manager, &layout, &registry, &confirm_id, false)
        .expect("allow=false 且 confirm_id 合法不应返回 Err");
    assert!(result.is_none(), "拒绝应返回 Ok(None)，实际：{result:?}");
    assert!(registry.load().is_empty(), "拒绝后不应安装任何东西");
}

/// Test C（invalid draft）：缺 `superagent` 块的草稿 → `handle_maker_request`
/// 返回 `{ok:false}`（无 `pending_confirm`），且没有登记任何 pending——任意
/// confirm_id 去 `resolve_install` 都应该 `Err`（"未知"），registry 仍为空。
#[tokio::test]
async fn install_invalid_draft_rejected_and_registers_nothing() {
    let (_tmp, layout, manager) = temp_layout();
    let registry = RegistryStore::new(layout.registry_path());
    write_invalid_maker_draft_missing_superagent_block(&layout, "draft-bad");

    let resp = handle_maker_request(
        "app-1",
        "__host_maker_install__",
        serde_json::json!({ "draft_id": "draft-bad" }),
        &layout,
        &manager,
    )
    .await;

    assert_eq!(
        resp["ok"],
        serde_json::json!(false),
        "非法草稿应失败，实际：{resp:?}"
    );
    assert!(
        resp.get("pending_confirm").is_none(),
        "非法草稿不应带 pending_confirm"
    );
    assert!(resp.get("error").is_some(), "非法草稿应带 error 字段");

    // 没有登记任何 pending：任意 confirm_id 去 resolve 都应该是 "未知"。
    assert!(
        maker::resolve_install(&manager, &layout, &registry, "whatever", true).is_err(),
        "非法草稿不应留下任何可被消费的 pending install"
    );
    assert!(registry.load().is_empty());
}

/// Test D（unknown id）：从未注册过的 confirm_id → `resolve_install` 必须
/// `Err`，不能 panic、也不能静默当成功处理。
#[tokio::test]
async fn resolve_install_unknown_confirm_id_is_err() {
    let (_tmp, layout, manager) = temp_layout();
    let registry = RegistryStore::new(layout.registry_path());

    let result =
        maker::resolve_install(&manager, &layout, &registry, "bogus-never-registered", true);
    assert!(
        result.is_err(),
        "未知 confirm_id 必须是 Err，实际：{result:?}"
    );
}

// ---------------------------------------------------------------------------
// T5b：`list_pending_installs` —— 前端确认面查询（只读快照，不消费）
// ---------------------------------------------------------------------------

/// 注册一条合法 pending install 后，`maker::list_pending_installs` 应该能看到它：
/// confirm_id 对得上、`display_name` 取自草稿清单、`permissions` 非空（哪怕
/// 草稿声明的是空权限，`render_human` 也会落回"仅在自己的数据区内活动"这条
/// 兜底文案，见 `permissions::render_human` 文档）；`resolve_install` 消费之后
/// 同一个 confirm_id 不应该再出现在列表里——只读查询不能绕过
/// `take_pending_install` 的移除语义。
#[tokio::test]
async fn list_pending_installs_sees_registered_pending_and_drops_after_resolve() {
    let (_tmp, layout, manager) = temp_layout();
    let registry = RegistryStore::new(layout.registry_path());
    write_valid_maker_draft(&layout, "draft-z");

    let resp = handle_maker_request(
        "app-1",
        "__host_maker_install__",
        serde_json::json!({ "draft_id": "draft-z" }),
        &layout,
        &manager,
    )
    .await;
    let confirm_id = resp["confirm_id"]
        .as_str()
        .expect("应有 confirm_id")
        .to_string();

    let listed = maker::list_pending_installs(&manager);
    assert_eq!(
        listed.len(),
        1,
        "应能看到刚登记的 pending install，实际：{listed:?}"
    );
    assert_eq!(listed[0].confirm_id, confirm_id);
    assert_eq!(listed[0].display_name, "Maker示例");
    assert!(!listed[0].permissions.is_empty(), "权限预览不应为空");

    let installed = maker::resolve_install(&manager, &layout, &registry, &confirm_id, true)
        .expect("allow=true 应成功")
        .expect("应返回 Some(app)");
    assert_eq!(installed.app_id, "superagent__maker-demo");

    let listed_after = maker::list_pending_installs(&manager);
    assert!(
        listed_after.is_empty(),
        "已被 resolve 消费的 pending 不应再出现在列表里，实际：{listed_after:?}"
    );
}

/// 没有任何 pending install 时，`list_pending_installs` 应返回空列表（不是
/// panic/错误）——测试初始态与"全部已解决"态同一断言，防止实现悄悄退化成
/// 遇空 map 就 panic。
#[tokio::test]
async fn list_pending_installs_empty_when_none_registered() {
    let (_tmp, _layout, manager) = temp_layout();
    assert!(maker::list_pending_installs(&manager).is_empty());
}
