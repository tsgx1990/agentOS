import { useEffect, useMemo, useState } from "react";
import "./ApprovalCenter.css";
import {
  listStagedCalls,
  respondStaged,
  listApprovalRules,
  revokeApprovalRule,
  type StagedCall,
  type ApprovalRule,
  type StagedOutcome,
} from "../lib/approvals";

const ARGS_PREVIEW_LIMIT = 200;

/** `JSON.stringify(args)` 截断 200 字 + "…"（spec §5 参数预览规则）。 */
function argsPreview(args: unknown): string {
  const s = JSON.stringify(args) ?? "";
  return s.length > ARGS_PREVIEW_LIMIT ? `${s.slice(0, ARGS_PREVIEW_LIMIT)}…` : s;
}

/**
 * 终审 Important 4：`respond_staged` 返回后按 verdict/reason 汇总成一行人话。
 * 此前只有 `verdict === "error"` 会被展示——但"验收前重新鉴权失败"
 * （`reason === "unauthorized"`，见 `notifications.rs::respond_staged`
 * Important 3 段）走的也是 `verdict === "rejected"`，与用户自己点拒绝共用同一
 * 个 verdict：用户点「允许」→ 后端因权限变化拒绝执行 → 界面刷新后那一行消失
 * → 用户以为执行成功了，人这一侧零反馈。这里把四种结果都汇总进一行摘要，
 * `unauthorized` 单独点名（`hasWarning` 供调用方决定是否用警示样式），
 * `missing`（被另一次请求抢先消费）也给一句人话，不再被悄悄吞掉。
 */
function summarizeOutcomes(outcomes: StagedOutcome[]): { text: string; hasWarning: boolean } {
  const executed = outcomes.filter((o) => o.verdict === "executed").length;
  const unauthorized = outcomes.filter((o) => o.verdict === "rejected" && o.reason === "unauthorized").length;
  const userRejected = outcomes.filter((o) => o.verdict === "rejected" && o.reason !== "unauthorized").length;
  const missing = outcomes.filter((o) => o.verdict === "missing").length;
  const failed = outcomes.filter((o) => o.verdict === "error").length;

  const parts = [`已执行 ${executed} 条`];
  if (unauthorized > 0) {
    parts.push(`${unauthorized} 条因应用权限变化被拒（该应用可能已被卸载、降权，或不再声明这个连接器）`);
  }
  if (userRejected > 0) parts.push(`${userRejected} 条已拒绝`);
  if (missing > 0) parts.push(`${missing} 条已被其它操作处理，未再次执行`);
  if (failed > 0) parts.push(`${failed} 条失败`);
  return { text: parts.join("，"), hasWarning: unauthorized > 0 || failed > 0 };
}

/** 暂存时长的人话：不满 1 分钟说「刚刚暂存」，满 60 分钟换小时（避免出现「0 分钟前」）。 */
function formatStagedAge(createdAt: number): string {
  const secs = Math.max(0, Math.floor(Date.now() / 1000 - createdAt));
  if (secs < 60) return "刚刚暂存";
  const mins = Math.floor(secs / 60);
  if (mins < 60) return `暂存于 ${mins} 分钟前`;
  return `暂存于 ${Math.floor(mins / 60)} 小时前`;
}

/** 按 `app_id` 分组，保留遇到顺序。 */
function groupByApp(calls: StagedCall[]): { appId: string; calls: StagedCall[] }[] {
  const order: string[] = [];
  const byApp = new Map<string, StagedCall[]>();
  for (const c of calls) {
    if (!byApp.has(c.app_id)) {
      byApp.set(c.app_id, []);
      order.push(c.app_id);
    }
    byApp.get(c.app_id)!.push(c);
  }
  return order.map((appId) => ({ appId, calls: byApp.get(appId)! }));
}

/** 应用分组内再按 连接器(server) → 工具(tool) 细分，供展示用的子标题。 */
// 终审 Minor 5：`tool` 名来自不受信的 `tools/list`——此前用 `${server}::${tool}`
// 拼字符串再 `split("::")` 还原，若 `tool` 本身含 `::` 子标题会显示成错误的
// server/tool。改为直接把 `{server, tool}` 存进 Map 的 value，不做字符串往返；
// key 仍用 `::` 拼接，但只用于分组去重，不再需要拆回来。
function groupByServerTool(calls: StagedCall[]): { server: string; tool: string; calls: StagedCall[] }[] {
  const order: string[] = [];
  const byKey = new Map<string, { server: string; tool: string; calls: StagedCall[] }>();
  for (const c of calls) {
    const key = `${c.server}::${c.tool}`;
    if (!byKey.has(key)) {
      byKey.set(key, { server: c.server, tool: c.tool, calls: [] });
      order.push(key);
    }
    byKey.get(key)!.calls.push(c);
  }
  return order.map((key) => byKey.get(key)!);
}

/**
 * 审批中心（P6-C Task7）：待批写调用按 应用 → 连接器 → 工具 分组渲染
 * （`list_staged_calls`），应用级「全部允许 / 全部拒绝」批量验收
 * （`respond_staged`），条级「允许 / 拒绝」+ 复选框「今后对此应用的这个
 * 工具自动放行」（勾选后点允许即 `always: true`，对应落一条 `ApprovalRule`）。
 * 面板下半部分列「已放行规则」（`list_approval_rules`）带「撤销」
 * （`revoke_approval_rule`）。`variant="table"` 是同一份数据的表格布局，
 * 仅供视觉自检截图切换，默认走分组卡片（`variant="cards"`）。
 *
 * `NotificationCenter` 的 `confirm_request` 条目改为跳转到这里（见其
 * `onOpenApprovals` prop），故本组件独立挂载于 `Shell` 中区状态机、不依赖
 * 任何通知上下文，挂载时自己现读 `list_staged_calls()`/`list_approval_rules()`
 * （不带 `appId`，看全部应用）。
 */
export function ApprovalCenter({
  onClose,
  variant = "cards",
}: { onClose?: () => void; variant?: "cards" | "table" } = {}) {
  const [staged, setStaged] = useState<StagedCall[] | null>(null);
  const [rules, setRules] = useState<ApprovalRule[] | null>(null);
  const [err, setErr] = useState<string | null>(null);
  const [always, setAlways] = useState<Record<string, boolean>>({});
  // 终审 Important 4：respondStaged 每次结果的一行人话摘要，见 summarizeOutcomes。
  const [summary, setSummary] = useState<{ text: string; hasWarning: boolean } | null>(null);
  // 终审 Minor 4：批准/拒绝/撤销进行中禁用操作按钮，防二次点击重复提交
  // ——正确性本就由后端 at-most-once 兜底（第二次全 missing），这里只是省一次
  // 多余的 IPC 往返，尤其是组级「全部允许」在慢网/慢盘下容易被连点。
  const [busy, setBusy] = useState(false);

  const refresh = () => {
    listStagedCalls().then(setStaged).catch((e) => setErr(String(e)));
    listApprovalRules().then(setRules).catch((e) => setErr(String(e)));
  };
  useEffect(() => { refresh(); }, []);

  async function respond(ids: string[], allow: boolean, allowAlways: boolean) {
    setBusy(true);
    try {
      const outcomes = await respondStaged(ids, allow, allowAlways);
      const failed = outcomes.filter((o) => o.verdict === "error");
      setErr(failed.length > 0 ? failed.map((o) => o.error ?? "验收失败").join("；") : null);
      setSummary(summarizeOutcomes(outcomes));
      refresh();
    } catch (e) {
      setErr(String(e));
      setSummary(null);
    } finally {
      setBusy(false);
    }
  }

  async function revoke(rule: ApprovalRule) {
    setBusy(true);
    try {
      await revokeApprovalRule(rule.app_id, rule.server, rule.tool);
      refresh();
    } catch (e) {
      setErr(String(e));
    } finally {
      setBusy(false);
    }
  }

  const groups = useMemo(() => groupByApp(staged ?? []), [staged]);

  return (
    <div className={`approval-center approval-center-${variant}`}>
      <div className="approval-center-header">
        <h2>审批中心</h2>
        {onClose && <button onClick={onClose}>返回</button>}
      </div>

      {err && <p className="approval-center-err">{err}</p>}
      {summary && (
        <p className={`approval-center-summary${summary.hasWarning ? " approval-center-summary-warn" : ""}`}>
          {summary.text}
        </p>
      )}

      {staged === null && !err && <p className="approval-center-hint">加载中…</p>}

      {staged !== null && staged.length === 0 && (
        <p className="approval-center-hint">没有待批操作</p>
      )}

      {staged !== null && staged.length > 0 && (
        <div className="approval-groups">
          {groups.map((g) => (
            <div className="approval-group" key={g.appId}>
              <div className="approval-group-header">
                <span className="approval-group-app">{g.appId}</span>
                <div className="approval-group-actions">
                  <button
                    className="allow"
                    disabled={busy}
                    onClick={() => respond(g.calls.map((c) => c.id), true, false)}
                  >
                    全部允许
                  </button>
                  <button
                    className="deny"
                    disabled={busy}
                    onClick={() => respond(g.calls.map((c) => c.id), false, false)}
                  >
                    全部拒绝
                  </button>
                </div>
              </div>

              {groupByServerTool(g.calls).map(({ server, tool, calls }) => (
                <div className="approval-subgroup" key={`${server}::${tool}`}>
                  <div className="approval-subgroup-title">{server} · {tool}</div>
                  {variant === "table" ? (
                    <table className="approval-table">
                      <thead>
                        <tr>
                          <th>参数</th>
                          <th>暂存于</th>
                          <th>自动放行</th>
                          <th>操作</th>
                        </tr>
                      </thead>
                      <tbody>
                        {calls.map((c) => (
                          <tr key={c.id}>
                            <td><code className="approval-args">{argsPreview(c.args)}</code></td>
                            <td className="approval-row-ts">{formatStagedAge(c.created_at)}</td>
                            <td>
                              <input
                                type="checkbox"
                                aria-label="今后对此应用的这个工具自动放行"
                                checked={!!always[c.id]}
                                onChange={(e) => setAlways((m) => ({ ...m, [c.id]: e.target.checked }))}
                              />
                            </td>
                            <td className="approval-row-actions">
                              <button className="allow" disabled={busy} onClick={() => respond([c.id], true, !!always[c.id])}>允许</button>
                              <button className="deny" disabled={busy} onClick={() => respond([c.id], false, false)}>拒绝</button>
                            </td>
                          </tr>
                        ))}
                      </tbody>
                    </table>
                  ) : (
                    <div className="approval-list">
                      {calls.map((c) => (
                        <div className="approval-row" key={c.id}>
                          <div className="approval-row-main">
                            <code className="approval-args">{argsPreview(c.args)}</code>
                            <span className="approval-row-ts">{formatStagedAge(c.created_at)}</span>
                          </div>
                          <label className="approval-row-always">
                            <input
                              type="checkbox"
                              checked={!!always[c.id]}
                              onChange={(e) => setAlways((m) => ({ ...m, [c.id]: e.target.checked }))}
                            />
                            今后对此应用的这个工具自动放行
                          </label>
                          <div className="approval-row-actions">
                            <button className="allow" disabled={busy} onClick={() => respond([c.id], true, !!always[c.id])}>允许</button>
                            <button className="deny" disabled={busy} onClick={() => respond([c.id], false, false)}>拒绝</button>
                          </div>
                        </div>
                      ))}
                    </div>
                  )}
                </div>
              ))}
            </div>
          ))}
        </div>
      )}

      <div className="approval-rules">
        <h3>已放行规则</h3>
        {rules === null && <p className="approval-center-hint">加载中…</p>}
        {rules !== null && rules.length === 0 && (
          <p className="approval-center-hint">还没有放行规则</p>
        )}
        {rules !== null && rules.length > 0 && (
          <table className="approval-table approval-rules-table">
            <thead>
              <tr>
                <th>应用</th>
                <th>连接器</th>
                <th>工具</th>
                <th></th>
              </tr>
            </thead>
            <tbody>
              {rules.map((r) => (
                <tr key={`${r.app_id}::${r.server}::${r.tool}`}>
                  <td>{r.app_id}</td>
                  <td>{r.server}</td>
                  <td>{r.tool}</td>
                  <td><button className="revoke" disabled={busy} onClick={() => revoke(r)}>撤销</button></td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </div>
    </div>
  );
}
