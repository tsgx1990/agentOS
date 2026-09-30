import { useEffect, useState } from "react";
import "./AuditView.css";
import { listAudit, type AuditEntry } from "../lib/audit";

/**
 * 审计视图：调用 `list_audit` 展示宿主级审计日志（各应用触发过的工具调用），
 * 列时间(`ts`)/应用(`app_id`)/工具(`tool`)/裁决(`verdict`)；`args` 已脱敏，
 * 作为次要信息挂在行 `title`（悬浮可见）而非独立列，避免喧宾夺主。
 * 空态给出提示文案；不带任何过滤条件时展示全部（受 `list_audit` 后端 limit
 * 默认行为约束）。挂载点：Shell 中区新增 `auditing` 视图分支，入口按钮挂在
 * SessionPanel（settings/面板区），见 task-9-brief 与 Shell.tsx。
 */
export function AuditView({ onClose }: { onClose?: () => void } = {}) {
  const [entries, setEntries] = useState<AuditEntry[] | null>(null);
  const [err, setErr] = useState<string | null>(null);

  useEffect(() => {
    listAudit({})
      .then(setEntries)
      .catch((e) => setErr(String(e)));
  }, []);

  return (
    <div className="audit-view">
      <div className="audit-view-header">
        <h2>审计日志</h2>
        {onClose && <button onClick={onClose}>返回</button>}
      </div>

      {err && <p className="audit-view-err">{err}</p>}

      {entries === null && !err && <p className="audit-view-hint">加载中…</p>}

      {entries !== null && entries.length === 0 && (
        <p className="audit-view-hint">还没有审计记录。应用触发工具调用后会出现在这里。</p>
      )}

      {entries !== null && entries.length > 0 && (
        <table className="audit-table">
          <thead>
            <tr>
              <th>时间</th>
              <th>应用</th>
              <th>工具</th>
              <th>裁决</th>
            </tr>
          </thead>
          <tbody>
            {entries.map((e, i) => (
              <tr key={i} title={e.args}>
                <td className="audit-cell-ts">{e.ts}</td>
                <td>{e.app_id}</td>
                <td>{e.tool}</td>
                <td>{e.verdict}</td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
    </div>
  );
}
