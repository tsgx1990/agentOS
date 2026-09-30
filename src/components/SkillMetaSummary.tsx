import "./SkillMetaSummary.css";
import type { SkillMeta, ScanReport, Severity } from "../lib/skills";

const SEVERITY_LABELS: Record<Severity, string> = { high: "高危", medium: "中危" };

/**
 * 技能 frontmatter 摘要 + 扫描发现的只读展示，被 `SkillInstallDialog`（安装
 * 前确认）与 `SkillPendingConfirm`（Maker 生成技能待确认）共用——两处呈现的是
 * 同一张"这个技能到底是什么"的真相表，同 `CapabilityPanel` 被 `InstallDialog`
 * 与 `AppFrame` 共用的道理一致，不重复写两份渲染逻辑。
 *
 * 展示内容严格对齐 spec §6："frontmatter 摘要、`allowed-tools`、脚本清单、
 * 扫描发现（高亮）、来源信任标签"：
 * - frontmatter 摘要：name/description/license/compatibility/是否禁止模型
 *   自主调用（`disable_model_invocation`）
 * - `allowed-tools`：技能声明需要的工具全名列表
 * - 脚本清单：`scan.script_files`（技能目录下的可执行脚本）
 * - 扫描发现：`scan.findings`，High 用 `--color-danger` 红、Medium 用
 *   `--chip-warn-*` 黄（危险模式命中的严重度分级，见 `skills.rs::Severity`）
 * - 来源信任标签：`trusted` 布尔渲染成「受信」/「不受信」中文标签
 */
export function SkillMetaSummary({
  meta,
  scan,
  trusted,
  sourceLabel,
}: {
  meta: SkillMeta;
  scan: ScanReport;
  trusted: boolean;
  sourceLabel?: string;
}) {
  return (
    <div className="skill-meta-summary">
      <div className="skill-meta-head">
        <span className="skill-meta-name">{meta.name}</span>
        <span className={`skill-trust-badge ${trusted ? "trusted" : "untrusted"}`}>
          {trusted ? "受信" : "不受信"}
        </span>
        {sourceLabel && <span className="skill-source-label">{sourceLabel}</span>}
      </div>
      <p className="skill-meta-desc">{meta.description}</p>
      <dl className="skill-meta-fields">
        {meta.license && (
          <>
            <dt>许可证</dt>
            <dd>{meta.license}</dd>
          </>
        )}
        {meta.compatibility && (
          <>
            <dt>兼容性</dt>
            <dd>{meta.compatibility}</dd>
          </>
        )}
        <dt>模型自主调用</dt>
        <dd>{meta.disable_model_invocation ? "已禁止" : "允许"}</dd>
      </dl>

      <div className="skill-meta-section">
        <h4>allowed-tools</h4>
        {meta.allowed_tools.length === 0 ? (
          <p className="skill-meta-empty">未声明工具</p>
        ) : (
          <ul className="skill-tool-list">
            {meta.allowed_tools.map((t) => (
              <li key={t}><code>{t}</code></li>
            ))}
          </ul>
        )}
      </div>

      <div className="skill-meta-section">
        <h4>脚本清单</h4>
        {scan.script_files.length === 0 ? (
          <p className="skill-meta-empty">不含脚本</p>
        ) : (
          <ul className="skill-tool-list">
            {scan.script_files.map((f) => (
              <li key={f}><code>{f}</code></li>
            ))}
          </ul>
        )}
      </div>

      <div className="skill-meta-section">
        <h4>扫描发现（{scan.findings.length}）</h4>
        {scan.findings.length === 0 ? (
          <p className="skill-meta-empty">未发现危险模式</p>
        ) : (
          <ul className="skill-finding-list">
            {scan.findings.map((f, i) => (
              <li key={i} className={`skill-finding skill-finding-${f.severity}`}>
                <span className="skill-finding-severity">{SEVERITY_LABELS[f.severity]}</span>
                <span className="skill-finding-rule">{f.rule}</span>
                <span className="skill-finding-loc">{f.file}:{f.line}</span>
                <code className="skill-finding-excerpt">{f.excerpt}</code>
              </li>
            ))}
          </ul>
        )}
      </div>
    </div>
  );
}
