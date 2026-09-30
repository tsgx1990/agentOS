//! `skills` 能力（P6-B Task 3）：清单字段 `skills.allow`（默认 false）声明后，
//! `launch()` 把该应用**已授予且已启用**的技能目录逐个贡献为 `--skill <dir>`，
//! 并把这些目录加入 `sandbox_read`（沙盒要能读到技能内容才能加载）。
//!
//! 真正堵住"pi 自动发现用户 `~/.agents/skills`/`PI_CODING_AGENT_DIR` 下技能"这个
//! 泄漏口子的是 `session_mgr::build_launch` 基线追加的 `--no-skills`——**所有**
//! 应用（无论是否声明本能力）都会拿到它，与本能力是否被声明无关（spec §4/裁决
//! 2）。本能力只负责"声明了就能看见授予集合"这一半，不负责"没声明就必须看不
//! 到自动发现"的另一半。
//!
//! `launch()` 只读 `SkillStore`（`grants_for` → `load_index`），文件不存在时
//! `SkillStore` 内部已把它当"空清单"处理、不做任何写操作——因此本能力天然满足
//! `LaunchCtx.materialize == false`（`describe()`/`preview_install`/
//! `app_capabilities` 只读路径）时不得在磁盘上留下任何目录这条 P6-A 终审修的坑：
//! 这里根本不存在"要不要落地"的分支，读操作不受 `materialize` 影响。
//!
//! `on_uninstall`：应用被卸载时清空它名下的全部技能授予（技能库本身不受影响，
//! 技能还装着，只是这个已经不存在的应用不再持有任何授予记录）。
use crate::capability::*;
use crate::paths::DataLayout;
use crate::permissions::Permissions;
use crate::skills::SkillStore;

pub struct SkillsCapability;

#[async_trait::async_trait]
impl Capability for SkillsCapability {
    fn key(&self) -> &'static str {
        "skills"
    }
    fn declared(&self, p: &Permissions, _i: &CallerIdentity) -> bool {
        p.skills.allow
    }
    fn render_human(&self, _p: &Permissions) -> Vec<String> {
        vec!["可加载你在「技能」里授予它并启用的技能".to_string()]
    }
    fn launch(&self, _p: &Permissions, ctx: &LaunchCtx<'_>) -> Result<LaunchContribution, String> {
        let store = SkillStore::new(ctx.layout.clone());
        let dirs = store.enabled_skill_dirs(ctx.app_id)?;
        let mut c = LaunchContribution::default();
        for dir in dirs {
            c.extra_args.push("--skill".to_string());
            c.extra_args.push(dir.to_string_lossy().to_string());
            c.sandbox_read.push(dir);
        }
        Ok(c)
    }
    fn on_uninstall(&self, app_id: &str, layout: &DataLayout) -> Result<(), String> {
        SkillStore::new(layout.clone())
            .remove_grants_for_app(app_id)
            .map(|_| ())
    }
    fn enforcement(&self) -> &'static [Enforcement] {
        &[
            Enforcement::Launch,
            Enforcement::Sandbox,
            Enforcement::InstallHook,
        ]
    }
}
