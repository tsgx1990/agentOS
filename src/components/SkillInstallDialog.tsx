import "./SkillInstallDialog.css";
import { SkillMetaSummary } from "./SkillMetaSummary";
import type { SkillMeta, ScanReport } from "../lib/skills";

/**
 * 技能安装确认对话框（spec §6）：展示 `SkillMetaSummary`（frontmatter 摘要、
 * `allowed-tools`、脚本清单、扫描发现、来源信任标签），供用户在真正落盘前确认。
 *
 * 安全闸门（spec §9 裁决 3"高危拒装"在前端的镜像）：**不受信来源 + 存在至少一条
 * High 级发现** 时只渲染「取消」按钮 + 一句说明——不给「安装」按钮，即使用户
 * 手贱也点不到；受信来源（`trusted=true`，如内置技能或用户已标记信任的来源）
 * 即便命中 High 也放行「安装」，因为信任判断本身就是"我确认这个来源，接受它
 * 声明的一切"，与后端 `SkillStore::install_from_dir`（`skills.rs`）里
 * `HighRiskUntrusted` 校验只在 `!trusted` 时触发的逻辑一致——真正的强制仍在
 * 后端，这里只是不让用户走一条注定会被拒的路径。
 *
 * `onConfirm` 由调用方（`SkillsView` 本地导入流）接到 `installSkillFromPath`；
 * `busy`/`error` 由调用方控制，`error` 原样透传展示（如授予/安装失败时后端
 * `String(e)` 的错误文案，不做二次加工）。
 */
export function SkillInstallDialog({
  meta,
  scan,
  trusted,
  sourceLabel,
  busy = false,
  error,
  onConfirm,
  onCancel,
}: {
  meta: SkillMeta;
  scan: ScanReport;
  trusted: boolean;
  sourceLabel?: string;
  busy?: boolean;
  error?: string | null;
  onConfirm: () => void | Promise<void>;
  onCancel: () => void;
}) {
  const hasHigh = scan.findings.some((f) => f.severity === "high");
  const blocked = !trusted && hasHigh;

  return (
    <div className="skill-install-dialog">
      <h2>安装技能确认</h2>
      <SkillMetaSummary meta={meta} scan={scan} trusted={trusted} sourceLabel={sourceLabel} />
      {blocked && (
        <p className="skill-install-blocked-reason">
          不受信来源出现高危内容，为安全起见不能安装——如确认该来源可信，请先在来源管理中把它标记为受信后再试。
        </p>
      )}
      {error && <p className="skill-install-err">{error}</p>}
      <div className="skill-install-actions">
        <button onClick={onCancel} disabled={busy}>取消</button>
        {!blocked && (
          <button className="primary" onClick={onConfirm} disabled={busy}>
            {busy ? "安装中…" : "安装"}
          </button>
        )}
      </div>
    </div>
  );
}
