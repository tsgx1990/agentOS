import { useEffect, useState } from "react";
import "./SkillsView.css";
import { SkillInstallDialog } from "./SkillInstallDialog";
import {
  listSkills,
  previewSkill,
  installSkillFromPath,
  uninstallSkill,
  skillGrants,
  grantSkill,
  revokeSkill,
  setSkillEnabled,
  skillMarketInstall,
  type InstalledSkill,
  type SkillPreview,
  type SkillSourceKind,
} from "../lib/skills";
import { fetchMarketIndex, type MarketEntry } from "../lib/market";
import { listApps, type InstalledApp } from "../lib/registry";

const SOURCE_LABELS: Record<SkillSourceKind, string> = {
  local: "本地导入",
  builtin: "内置",
  market: "市场",
  maker: "Maker 生成",
};

/** 「扫描发现 N 项（M 项高危）」——0 条时不带括号后缀。 */
function findingsSummary(findings: InstalledSkill["scan"]["findings"]): string {
  if (findings.length === 0) return "扫描发现 0 项";
  const high = findings.filter((f) => f.severity === "high").length;
  return high > 0 ? `扫描发现 ${findings.length} 项（${high} 项高危）` : `扫描发现 ${findings.length} 项`;
}

/**
 * 技能视图（spec §6）：「已安装」（启停开关、授予给哪些应用的多选、卸载）、
 * 「市场」（复用市场索引里 `kind === "skill"` 的条目）、「本地导入」（目录路径
 * → `preview_skill` → `SkillInstallDialog` → `install_skill_from_path`）三区。
 *
 * 「授予给…」与「启用开关」的关系（spec §6 只说了这两样，未细化交互）：
 * `grant_skill`/`revoke_skill` 只认 `(app_id, skill_id)` 是否存在授予记录，
 * `set_skill_enabled` 要求授予记录已存在才能启停（见 `skills.rs::set_enabled`
 * 文档"启停和授予是两个动作"）——两者天然是"每个已装应用一行，行内一个授予
 * 复选框 + 授予后才出现的启用开关"，而不是技能卡片级的单一全局开关：一个技能
 * 可以对 A 应用启用、对 B 应用禁用，这个状态本就是按 (app, skill) 记的，不存在
 * 脱离应用语境的"这个技能全局启不启用"。
 *
 * 已装技能列表本身不带"这个技能授予了哪些应用"的反向索引（`skill_grants` 只能
 * 按 app_id 正向查），这里在挂载时对 `listApps()` 里的每个应用各拉一次
 * `skill_grants(appId)`，客户端拼出 skillId → appId → enabled 的反向表。
 */
export function SkillsView({
  onClose,
  variant = "list",
}: { onClose?: () => void; variant?: "list" | "cards" } = {}) {
  const [skills, setSkills] = useState<InstalledSkill[] | null>(null);
  const [err, setErr] = useState<string | null>(null);
  const [uninstallBusy, setUninstallBusy] = useState<string | null>(null);

  const [apps, setApps] = useState<InstalledApp[]>([]);
  // skillId -> appId -> enabled（只含真正存在授予记录的 (skill, app) 对）
  const [grants, setGrants] = useState<Record<string, Record<string, boolean>>>({});
  const [grantErr, setGrantErr] = useState<string | null>(null);
  // 按行键（`${appId}::${skillId}`，启用开关额外加 `::enabled` 后缀区分）的忙态集合
  // ——两个不同 (skill, app) 行的授予/启停操作并发进行时，各自独立 add/delete 自己的
  // key，不会像单个全局 busy 那样被先完成的一次把还在途的另一行 busy 态一并清掉
  // （审查 Important 2）。
  const [grantBusy, setGrantBusy] = useState<Set<string>>(new Set());

  function addGrantBusy(key: string) {
    setGrantBusy((prev) => new Set(prev).add(key));
  }
  function removeGrantBusy(key: string) {
    setGrantBusy((prev) => {
      const next = new Set(prev);
      next.delete(key);
      return next;
    });
  }

  const [marketEntries, setMarketEntries] = useState<MarketEntry[] | null>(null);
  const [marketErr, setMarketErr] = useState<string | null>(null);
  const [marketBusy, setMarketBusy] = useState<string | null>(null);
  const [marketDone, setMarketDone] = useState<Record<string, boolean>>({});
  const [marketEntryErr, setMarketEntryErr] = useState<Record<string, string>>({});

  const [importPath, setImportPath] = useState("");
  const [importPreview, setImportPreview] = useState<SkillPreview | null>(null);
  const [importErr, setImportErr] = useState<string | null>(null);
  const [importBusy, setImportBusy] = useState(false);
  const [previewBusy, setPreviewBusy] = useState(false);

  const refreshSkills = () => listSkills().then(setSkills).catch((e) => setErr(String(e)));

  async function refreshGrants(appList: InstalledApp[]) {
    const pairs = await Promise.all(
      appList.map((a) =>
        skillGrants(a.app_id)
          .then((entries) => [a.app_id, entries] as const)
          .catch(() => [a.app_id, []] as const),
      ),
    );
    const map: Record<string, Record<string, boolean>> = {};
    for (const [appId, entries] of pairs) {
      for (const [skill, enabled] of entries) {
        (map[skill.meta.id] ??= {})[appId] = enabled;
      }
    }
    setGrants(map);
  }

  useEffect(() => {
    refreshSkills();
    listApps()
      .then((a) => {
        setApps(a);
        refreshGrants(a);
      })
      .catch(() => setApps([]));
    fetchMarketIndex()
      .then(setMarketEntries)
      .catch((e) => setMarketErr(String(e)));
  }, []);

  async function toggleGrant(skillId: string, appId: string, checked: boolean) {
    const key = `${appId}::${skillId}`;
    addGrantBusy(key);
    setGrantErr(null);
    try {
      if (checked) await grantSkill(appId, skillId);
      else await revokeSkill(appId, skillId);
      await refreshGrants(apps);
    } catch (e) {
      setGrantErr(String(e));
    } finally {
      removeGrantBusy(key);
    }
  }

  async function toggleEnabled(skillId: string, appId: string, enabled: boolean) {
    const key = `${appId}::${skillId}::enabled`;
    addGrantBusy(key);
    setGrantErr(null);
    try {
      await setSkillEnabled(appId, skillId, enabled);
      await refreshGrants(apps);
    } catch (e) {
      setGrantErr(String(e));
    } finally {
      removeGrantBusy(key);
    }
  }

  async function doUninstall(skillId: string) {
    setUninstallBusy(skillId);
    setErr(null);
    try {
      await uninstallSkill(skillId);
      refreshSkills();
      refreshGrants(apps);
    } catch (e) {
      setErr(String(e));
    } finally {
      setUninstallBusy(null);
    }
  }

  async function doPreviewImport() {
    setPreviewBusy(true);
    setImportErr(null);
    try {
      setImportPreview(await previewSkill(importPath));
    } catch (e) {
      setImportErr(String(e));
    } finally {
      setPreviewBusy(false);
    }
  }

  async function confirmImport() {
    setImportBusy(true);
    setImportErr(null);
    try {
      await installSkillFromPath(importPath);
      setImportPreview(null);
      setImportPath("");
      refreshSkills();
    } catch (e) {
      setImportErr(String(e));
    } finally {
      setImportBusy(false);
    }
  }

  async function installFromMarket(entry: MarketEntry) {
    setMarketBusy(entry.name);
    setMarketEntryErr((d) => ({ ...d, [entry.name]: "" }));
    try {
      await skillMarketInstall(entry);
      setMarketDone((d) => ({ ...d, [entry.name]: true }));
      refreshSkills();
    } catch (e) {
      setMarketEntryErr((d) => ({ ...d, [entry.name]: String(e) }));
    } finally {
      setMarketBusy(null);
    }
  }

  const skillMarketEntries = (marketEntries ?? []).filter((e) => e.kind === "skill");

  return (
    <div className="skills-view">
      <div className="skills-header">
        <h2>技能</h2>
        {onClose && <button onClick={onClose}>返回</button>}
      </div>

      <section className="skills-section">
        <h3>已安装</h3>
        {err && <p className="skills-err">{err}</p>}
        {grantErr && <p className="skills-err">{grantErr}</p>}
        {skills === null && !err && <p className="skills-hint">加载中…</p>}
        {skills !== null && skills.length === 0 && <p className="skills-hint">还没有安装技能</p>}
        {skills !== null && skills.length > 0 && (
          <div className={`skills-installed skills-installed-${variant}`}>
            {skills.map((s) => (
              <div className="skill-card" key={s.meta.id}>
                <div className="skill-card-head">
                  <span className="skill-card-name">{s.meta.name}</span>
                  <span className={`skill-trust-badge ${s.trusted ? "trusted" : "untrusted"}`}>
                    {s.trusted ? "受信" : "不受信"}
                  </span>
                  <span className="skill-source-label">{SOURCE_LABELS[s.source.kind]}</span>
                  {s.scan.has_scripts && <span className="skill-scripts-tag">含脚本</span>}
                </div>
                <p className="skill-card-desc">{s.meta.description}</p>
                <p className="skill-card-findings">{findingsSummary(s.scan.findings)}</p>

                {apps.length > 0 && (
                  <div className="skill-grant-list">
                    <div className="skill-grant-title">授予给…</div>
                    {apps.map((a) => {
                      const enabled = grants[s.meta.id]?.[a.app_id];
                      const granted = enabled !== undefined;
                      const busy =
                        grantBusy.has(`${a.app_id}::${s.meta.id}`) ||
                        grantBusy.has(`${a.app_id}::${s.meta.id}::enabled`);
                      return (
                        <div className="skill-grant-row" key={a.app_id}>
                          <label className="skill-grant-app">
                            <input
                              type="checkbox"
                              aria-label={`授予给${a.display_name}`}
                              checked={granted}
                              disabled={busy}
                              onChange={(e) => toggleGrant(s.meta.id, a.app_id, e.target.checked)}
                            />
                            <span>{a.display_name}</span>
                          </label>
                          {granted && (
                            <label className="skill-grant-enabled-wrap">
                              <input
                                type="checkbox"
                                className="skill-grant-enabled"
                                aria-label={`${a.display_name}已启用`}
                                checked={!!enabled}
                                disabled={busy}
                                onChange={(e) => toggleEnabled(s.meta.id, a.app_id, e.target.checked)}
                              />
                              <span className="skill-grant-enabled-label">启用</span>
                            </label>
                          )}
                        </div>
                      );
                    })}
                  </div>
                )}

                <div className="skill-card-actions">
                  <button
                    className="danger"
                    disabled={uninstallBusy === s.meta.id}
                    onClick={() => doUninstall(s.meta.id)}
                  >
                    卸载
                  </button>
                </div>
              </div>
            ))}
          </div>
        )}
      </section>

      <section className="skills-section" data-testid="skills-market-section">
        <h3>市场</h3>
        {marketErr && <p className="skills-err">加载市场失败：{marketErr}</p>}
        {marketEntries === null && !marketErr && <p className="skills-hint">加载中…</p>}
        {marketEntries !== null && skillMarketEntries.length === 0 && (
          <p className="skills-hint">市场里暂无技能</p>
        )}
        {skillMarketEntries.length > 0 && (
          <div className="skills-market-list">
            {skillMarketEntries.map((e) => (
              <div className="skill-card" key={e.name}>
                <div className="skill-card-head">
                  <span className="skill-card-name">{e.display_name}</span>
                  {e.author && <span className="skill-source-label">{e.author}</span>}
                </div>
                <p className="skill-card-desc">{e.description}</p>
                <div className="skill-card-actions">
                  <button
                    onClick={() => installFromMarket(e)}
                    disabled={marketBusy === e.name || marketDone[e.name]}
                  >
                    {marketDone[e.name] ? "已安装" : marketBusy === e.name ? "安装中…" : "安装"}
                  </button>
                  {marketEntryErr[e.name] && (
                    <span className="skills-err">{marketEntryErr[e.name]}</span>
                  )}
                </div>
              </div>
            ))}
          </div>
        )}
      </section>

      <section className="skills-section">
        <h3>本地导入</h3>
        <div className="skills-import-row">
          <input
            placeholder="技能目录路径"
            value={importPath}
            onChange={(e) => setImportPath(e.target.value)}
          />
          <button onClick={doPreviewImport} disabled={!importPath || previewBusy}>
            {previewBusy ? "预览中…" : "预览"}
          </button>
        </div>
        {importErr && <p className="skills-err">{importErr}</p>}
        {importPreview && (
          <SkillInstallDialog
            meta={importPreview.meta}
            scan={importPreview.scan}
            trusted={importPreview.trusted}
            sourceLabel="本地导入"
            busy={importBusy}
            onConfirm={confirmImport}
            onCancel={() => setImportPreview(null)}
          />
        )}
      </section>
    </div>
  );
}
