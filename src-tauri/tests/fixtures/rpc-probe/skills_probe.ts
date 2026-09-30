// 测试专用（P6-B Task 4）：在 session_start 上报 pi 当前会话实际可见的技能名字
// 集合——`pi.getCommands()` 里 `source === "skill"` 的条目。真实 pi 0.84.4
// （`core/agent-session.ts::_bindExtensionCore`）把技能命令名拼成 `skill:<name>`
// 前缀，这里剥掉前缀只报告技能本身的名字，与 `SkillMeta.id`/`.name`（frontmatter
// `name`）保持同一命名空间，方便测试断言。
//
// 用 `session_start` 而非 `before_agent_start`：技能发现（package-manager 扫描
// `~/.agents/skills`/`$PI_CODING_AGENT_DIR/skills`/CLI `--skill` + resource-loader
// 按 `--no-skills` 过滤）在会话启动期就已经完成，不需要等 `before_agent_start`
// 那个要等一次 prompt 才触发的事件——与 `active_tools_probe.ts`
// （`real_pi_tools_allowlist_it.rs` 用它探测 `--tools` 白名单）同款手法：极简
// 探针 + `ctx.ui.notify()` 上报，测试进程从 stdout 的 `extension_ui_request`
// 事件里解析。
export default function (pi: any) {
  pi.on("session_start", async (_e: any, ctx: any) => {
    const names = pi
      .getCommands()
      .filter((c: any) => c.source === "skill")
      .map((c: any) => c.name.replace(/^skill:/, ""));
    ctx.ui.notify("SKILLS_PROBE:" + JSON.stringify({ skills: names }), "info");
  });
}
