import "./NavRail.css";
import { CATEGORIES, categoryCounts, type InstalledApp } from "../lib/registry";

/**
 * 左导航：按 `CATEGORIES` 顺序渲染类目 + 真实计数（来自已装应用列表），
 * 「全部」= apps.length。「应用市场」仍是 P5 才接通的占位入口。
 * 见 docs/superpowers/specs/2026-07-17-ui-direction-c4a.md §4。
 */
export function NavRail({ apps }: { apps: InstalledApp[] }) {
  const counts = categoryCounts(apps);
  return (
    <nav className="nav-rail">
      <div className="nav-rail-title">类目</div>
      <div className="nav-item">
        <span className="nav-item-label">全部</span>
        <span className="nav-item-count">{apps.length}</span>
      </div>
      {CATEGORIES.map((c) => (
        <div className="nav-item" key={c.key}>
          <span className="nav-item-dot" style={{ background: c.dot }} />
          <span className="nav-item-label">{c.label}</span>
          <span className="nav-item-count">{counts[c.key] ?? 0}</span>
        </div>
      ))}
      {apps.length === 0 && (
        <p className="nav-empty-hint">还没有安装应用。装好的 subagent 会按类目出现在这里。</p>
      )}

      <div className="nav-rail-spacer" />

      <div className="nav-placeholder-entries">
        <button className="nav-placeholder-item" disabled>
          <span>应用市场</span>
          <span className="nav-placeholder-badge">即将上线</span>
        </button>
      </div>

      <div className="nav-byok">
        <span className="nav-byok-dot" />
        <span>BYOK 已连接</span>
      </div>
    </nav>
  );
}
