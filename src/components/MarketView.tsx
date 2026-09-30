import { useEffect, useState } from "react";
import "./MarketView.css";
import { fetchMarketIndex, installMarketEntry, type MarketEntry } from "../lib/market";
import { skillMarketInstall } from "../lib/skills";
import { CATEGORIES } from "../lib/registry";

function categoryLabel(key: string): string {
  return CATEGORIES.find((c) => c.key === key)?.label ?? key;
}

/**
 * 应用市场（P5 §12 v1 轻量方案）：拉取精选市场索引（`market_fetch_index`，缺省
 * 读内置 demo），渲染条目卡（名称/分类/描述 + 人话权限摘要），每条一个「安装」
 * 按钮。v1 的 demo 条目都是内置白名单样例，装走 `install_builtin_sample`
 * （见 `lib/market.ts installMarketEntry` 文档）；真实第三方条目的确认安装流
 * 留待手工里程碑。装完刷新已装列表（`onInstalled`）。
 *
 * 挂载点同 `AuditView`/`ConnectorSettings`：`SessionPanel` 常驻入口 → `Shell`
 * 中区状态分支。
 */
export function MarketView({
  onClose,
  onInstalled,
}: { onClose?: () => void; onInstalled?: () => void } = {}) {
  const [entries, setEntries] = useState<MarketEntry[] | null>(null);
  const [err, setErr] = useState<string | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  // done：装成功后固定存 "已安装"（驱动按钮态）；error：装失败时的原样错误文案；
  // scriptHint：技能条目装完且 scan.has_scripts 为真时的一句额外提示（三者独立，
  // 不再像早前版本那样把三种语义塞进同一个字符串里靠 startsWith/includes 猜）。
  const [done, setDone] = useState<Record<string, boolean>>({});
  const [error, setError] = useState<Record<string, string>>({});
  const [scriptHint, setScriptHint] = useState<Record<string, boolean>>({});

  useEffect(() => {
    fetchMarketIndex()
      .then(setEntries)
      .catch((e) => setErr(String(e)));
  }, []);

  async function install(entry: MarketEntry) {
    setBusy(entry.name);
    setError((d) => ({ ...d, [entry.name]: "" }));
    try {
      if (entry.kind === "skill") {
        // 技能条目走独立的安装门（skill_market_install），不复用应用的
        // install_builtin_sample；成功后若装回的技能含脚本，提示用户去技能页复核
        // （市场索引条目本身不带 has_scripts，只有装完拿到 InstalledSkill.scan 才知道）。
        const installed = await skillMarketInstall(entry);
        if (installed.scan.has_scripts) setScriptHint((d) => ({ ...d, [entry.name]: true }));
      } else {
        await installMarketEntry(entry);
      }
      setDone((d) => ({ ...d, [entry.name]: true }));
      onInstalled?.();
    } catch (e) {
      setError((d) => ({ ...d, [entry.name]: `安装失败：${String(e)}` }));
    } finally {
      setBusy(null);
    }
  }

  return (
    <div className="market-view">
      <div className="market-header">
        <h2>应用市场</h2>
        {onClose && <button onClick={onClose}>返回</button>}
      </div>

      {err && <p className="market-err">加载市场失败：{err}</p>}
      {entries === null && !err && <p className="market-hint">加载中…</p>}
      {entries !== null && entries.length === 0 && (
        <p className="market-hint">精选市场暂无应用。</p>
      )}

      {entries !== null && entries.length > 0 && (
        <ul className="market-list">
          {entries.map((e) => (
            <li key={e.name} className="market-card">
              <div className="market-card-head">
                <span className="market-card-name">{e.display_name}</span>
                {e.kind === "skill" && <span className="market-card-skill-tag">技能</span>}
                <span className="market-card-cat">{categoryLabel(e.category)}</span>
              </div>
              <p className="market-card-desc">{e.description}</p>
              {e.permissions.length > 0 && (
                <ul className="market-perms">
                  {e.permissions.map((p, i) => (
                    <li key={i}>{p}</li>
                  ))}
                </ul>
              )}
              <div className="market-card-actions">
                <button onClick={() => install(e)} disabled={busy === e.name || done[e.name]}>
                  {done[e.name] ? "已安装" : busy === e.name ? "安装中…" : "安装"}
                </button>
                {error[e.name] && <span className="market-card-status">{error[e.name]}</span>}
                {scriptHint[e.name] && (
                  <span className="market-card-status market-card-status-hint">
                    含脚本，建议到技能页查看扫描发现
                  </span>
                )}
              </div>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}
