// 测试专用探针扩展（不是任何第三方包的一部分）——供
// `tests/real_pi_bash_escape_it.rs` 的 bonus 用例
// （`real_pi_loads_mock_malicious_extension_and_registers_escape_attempt_under_sandbox`）
// 使用。
//
// 目的：在**真实 pi**（`--mode rpc`）启动阶段，不发送任何 `prompt`/`steer`/
// `follow_up`（即完全不触发模型调用、不需要任何 API key）的前提下，证明真实 pi
// 确实加载了同一 `-e` 参数列表里的 `mock-malicious/escape.ts`，且其
// `escape_attempt` 工具确实被注册——用 `pi.getAllTools()`（extensions.md
// §"pi.getActiveTools() / pi.getAllTools()"）而非猜测；`sourceInfo.path` 直接就
// 是 escape.ts 的绝对路径，不是同名巧合。
//
// 结果通过 `ctx.ui.notify()`（fire-and-forget，RPC 模式下原样序列化成 stdout
// 上的 `extension_ui_request` 事件，见 docs/rpc.md "Extension UI Protocol"）
// 上报——`session_start` 在 pi 完成所有 `-e` 扩展的工厂函数之后、且早于任何
// prompt 处理之前触发（extensions.md: "If the factory returns a Promise, pi
// awaits it before continuing startup. That means async initialization
// completes before session_start"），所以此时 escape.ts 的 `pi.registerTool()`
// 调用必然已经跑完。
//
// 刻意保持最小：三类越权操作本身（写 $APP_DATA 外/读沙盒外内容/联网）已经由
// 同一测试文件里的 `real_pi_bash_escape_attempts_blocked_under_production_
// untrusted_sandbox` 用例经真实 pi 的 `type:"bash"` RPC 命令独立证明过，这里不
// 重复验证，只做"扩展确实被加载 + 工具确实被注册"这一件事。
export default function (pi: any) {
  pi.on("session_start", async (_event: any, ctx: any) => {
    const all = pi.getAllTools();
    const found = all.find((t: any) => t.name === "escape_attempt");

    ctx.ui.notify(
      "TOOL_REGISTRATION_PROBE_RESULT:" +
        JSON.stringify({
          escapeAttemptRegistered: !!found,
          escapeAttemptSourcePath: found ? found.sourceInfo?.path ?? null : null,
          allToolNames: all.map((t: any) => t.name),
        }),
      "info"
    );
  });
}
