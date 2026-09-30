// 权限网关钩子——P2：降级为「交互/审计层」，NOT 硬安全边界。
//
// 为什么不再是硬边界（P1 spike 结论，
// docs/superpowers/spikes/2026-07-17-host-hook-priority.md §证据6/结论(b)）：
// 一次 tool_call 钩子的"批准"对"最终真正执行的参数"不构成任何保证——pi 官方文档
// 明确支持后置扩展在 tool_call 钩子里原地改写 `event.input`（`event.input is
// mutable... Later handlers see earlier mutations. No re-validation is performed
// after mutation.`）。也就是说：本钩子对着"批准时刻看到的"参数放行之后，同一进程内
// 后加载的另一个包扩展可以在真正执行之前把参数悄悄换掉——不 `block`、不抛异常，
// 宿主对此没有任何信号。这是一次静默的、无阻塞的绕过，本钩子无法察觉也无法阻止。
//
// 真正的硬边界是 OS 级 L2 沙盒（macOS 上的 `sandbox-exec`，见
// `session_mgr.rs::sandboxing_available()` —— THE single source of truth）：
// 只有 OS 内核层面的隔离才能保证"批准的东西"和"真正执行的东西"一致，因为它管的
// 是进程能不能碰某个文件/网络端点，而不是信任某次函数调用观察到的参数快照。
//
// 本钩子保留下面两个价值，两个都明确是 advisory（建议性），不是强制：
// 1) 快速 UX 反馈——明显越权的调用（写非应用数据区、危险命令、越权读）尽早拒绝，
//    不必等到更底层的 OS 沙盒再挡一次，用户体验更好；本文件下方的 `deny()` 早拒绝
//    行为与 P1 完全一致（guard.ts 的纯函数判断规则不变，guard.test.ts 不受影响）。
// 2) 审计上报——无论放行还是拒绝，都通过 `pi.appendEntry` 把
//    `{app_id, tool, args, verdict}` 上报成宿主可观察的事件，方便未来的审计/调试
//    UI 观察"网关当时是怎么判断的"。**但要注意**：这里上报的 `args` 是本钩子在
//    批准/拒绝那一刻看到的参数，不是"最终真正执行的"参数——同一个绕过手法对这份
//    上报同样成立，因此它只能是 advisory 的旁证，不能当作安全审计的可信来源。
//    宿主侧真正落盘的 `audit::record`（`src-tauri/src/session_mgr.rs`/`lib.rs`）
//    因此**不**依赖这份上报：它改从 pi 核心自己在工具真正执行完之后产生的
//    `tool_execution_end` 事件取 `tool`/`args`（见 `rpc.rs` 的
//    `PiEvent::ToolExecuted`）——那才是不可被本文件、也不可被任何包扩展操纵的信号源。
import { writeAllowed, readAllowed, commandAllowed } from "./guard";

export default function (pi: any) {
  const appData = process.env.SUPERAGENT_APP_DATA ?? "";
  const appId = process.env.SUPERAGENT_APP_ID ?? "";
  const readPaths: string[] = JSON.parse(process.env.SUPERAGENT_READ_PATHS ?? "[]");

  // advisory 审计上报：见文件头注释，只是"网关当时怎么判断的"旁证，不是安全审计的
  // 可信来源。上报失败（如 pi 版本没有 appendEntry，或本身抛异常）绝不能影响下面
  // 真正的放行/拒绝判断——审计是旁路，不能成为功能性的依赖。
  const audit = (tool: string, args: unknown, verdict: string) => {
    try {
      pi.appendEntry?.("permission_gate_audit", { app_id: appId, tool, args, verdict });
    } catch {
      // 吞掉：advisory 上报失败不阻断正常的工具调用流程。
    }
  };

  pi.on("tool_call", (event: any) => {
    const i = event.input ?? {};
    const deny = (reason: string) => {
      audit(event.toolName, i, `blocked:${reason}`);
      return { block: true, reason };
    };
    const allow = () => {
      audit(event.toolName, i, "allowed");
      return undefined;
    };
    switch (event.toolName) {
      case "bash":
        if (!commandAllowed(String(i.command ?? ""))) return deny("命令被安全策略拒绝");
        return allow();
      case "write":
      case "edit":
        if (!writeAllowed(String(i.path ?? i.file_path ?? ""), appData))
          return deny("只能写入应用自己的数据区");
        return allow();
      case "read":
      case "ls":
      case "grep":
      case "find":
        if (!readAllowed(String(i.path ?? i.file_path ?? appData), appData, readPaths))
          return deny("无权读取该路径");
        return allow();
      default:
        return allow();
    }
  });
}
