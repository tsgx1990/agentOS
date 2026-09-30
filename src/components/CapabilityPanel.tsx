import { useEffect, useState } from "react";
import "./CapabilityPanel.css";
import { ENFORCEMENT_LABELS, type CapabilityReport } from "../lib/registry";
import { skillGrants } from "../lib/skills";

const FALLBACK = "仅在自己的数据区内活动，无额外权限";

/**
 * 「这个应用到底能做什么、由谁强制」诊断面板：只呈现已声明
 * （`declared && human.length > 0`）的能力——每条人话描述 + 强制点标签
 * （`ENFORCEMENT_LABELS`）+ 涉及的工具名（可折叠）；`InstallDialog`（安装前）
 * 与 `AppFrame` 的「权限」弹层（运行时）共用同一份渲染逻辑，保证两处呈现的是
 * 同一张真相表，不会出现"装的时候没说、跑起来才发现"的脱节。
 *
 * P6-B Task 8：可选 `appId`——传入时额外拉一次 `skill_grants(appId)`，在能力列表
 * 末尾加一条独立的「skills」行，显示「已授予 N 个（M 个启用）」（简报逐字稿，不带「技能」
 * 二字后缀——上一轮审查 Minor 1 已指出与简报不一致，这里改回逐字对齐）。`InstallDialog`
 * 装前预览阶段没有 `appId`（应用还没装，不存在任何技能授予关系），不传即不渲染
 * 这一行；`AppFrame` 运行时视角有 `appId`，会传入。
 */
export function CapabilityPanel({
  reports,
  sandboxed,
  appId,
}: {
  reports: CapabilityReport[];
  sandboxed: boolean;
  appId?: string;
}) {
  const declared = reports.filter((r) => r.declared && r.human.length > 0);
  const [skillCounts, setSkillCounts] = useState<{ total: number; enabled: number } | null>(null);

  useEffect(() => {
    if (!appId) {
      setSkillCounts(null);
      return;
    }
    skillGrants(appId)
      .then((entries) => {
        setSkillCounts({ total: entries.length, enabled: entries.filter(([, enabled]) => enabled).length });
      })
      .catch(() => setSkillCounts({ total: 0, enabled: 0 }));
  }, [appId]);

  return (
    <div className="cap-panel">
      {!sandboxed && (
        <p className="cap-panel-hint">本平台无硬沙盒：以下能力仅作声明，第三方应用按受限模式运行。</p>
      )}
      {declared.length === 0 ? (
        <p className="cap-panel-empty">{FALLBACK}</p>
      ) : (
        <ul className="cap-panel-list">
          {declared.map((r) => (
            <li key={r.key} className="cap-panel-item">
              <div className="cap-panel-human">
                {r.human.map((h, i) => (
                  <div key={i}>{h}</div>
                ))}
              </div>
              <div className="cap-panel-chips">
                {r.enforcement.map((e) => (
                  <span key={e} className={`cap-chip cap-chip-${e}`}>
                    {ENFORCEMENT_LABELS[e]}
                  </span>
                ))}
              </div>
              {r.error && <div className="cap-panel-error">{r.error}</div>}
              {r.tools.length > 0 && (
                <details className="cap-panel-tools">
                  <summary>工具 {r.tools.length} 个</summary>
                  <code>{r.tools.join(", ")}</code>
                </details>
              )}
            </li>
          ))}
        </ul>
      )}
      {appId && skillCounts && (
        <div className="cap-panel-item cap-panel-skills-row">
          <div className="cap-panel-human">已授予 {skillCounts.total} 个（{skillCounts.enabled} 个启用）</div>
        </div>
      )}
    </div>
  );
}
