import { useEffect, useState } from "react";
import "./AppGrid.css";
import { CATEGORIES, type InstalledApp } from "../lib/registry";
import { getSandboxStatus, type SandboxStatus } from "../lib/sandbox";

/**
 * 单张应用卡片的沙盒状态徽标：挂载后异步问 `app_sandbox_status`，加载完成前
 * 不渲染任何占位（不阻塞卡片本体渲染，见 task-9-brief）；请求失败（如未知
 * app_id）同样静默不渲染，不让后端错误打断整个网格。
 * 三态映射（C4a §2.2 status chip token）：
 * - `sandboxed`→ 「沙盒中」，安全/正向色（`chip-ready`）；
 * - 未沙盒且 `restricted`→ 「未沙盒·受限」，警示色（`chip-warn`）；
 * - 未沙盒但不受限（本地信任的第一方/受信第三方）→ 「本地信任」，中性色（`chip-idle`）。
 */
function SandboxBadge({ appId }: { appId: string }) {
  const [status, setStatus] = useState<SandboxStatus | null>(null);

  useEffect(() => {
    let cancelled = false;
    setStatus(null);
    getSandboxStatus(appId)
      .then((s) => { if (!cancelled) setStatus(s); })
      .catch(() => { /* 静默：徽标本就是可选增强，拉取失败不影响卡片可用 */ });
    return () => { cancelled = true; };
  }, [appId]);

  if (!status) return null;

  if (status.sandboxed) {
    return (
      <span
        className="chip chip-sandbox-safe"
        title="该应用的运行进程已被操作系统级沙盒（L2）隔离"
      >
        沙盒中
      </span>
    );
  }
  if (status.restricted) {
    return (
      <span
        className="chip chip-sandbox-warn"
        title="未获得系统级沙盒隔离；宿主已对其工具能力做裁剪限制"
        aria-label="未沙盒，且处于受限模式"
      >
        未沙盒·受限
      </span>
    );
  }
  return (
    <span
      className="chip chip-sandbox-neutral"
      title="本地受信任来源，未启用系统级沙盒但不受额外限制"
    >
      本地信任
    </span>
  );
}

/**
 * 中区应用网格：按 `CATEGORIES` 分组渲染已装应用卡片（空类目不渲染）。
 * 每张卡片含图标占位、`display_name`、状态徽标（P1 恒为 idle）+ 沙盒状态徽标
 * （`SandboxBadge`，异步拉取 `app_sandbox_status`）；
 * `trusted === false` 时叠加「未验证·受限」标识——由宿主渲染，非应用内容，
 * 应用自身无法伪造这块标识（视觉全走 tokens.css，见 C4a §2.2/§2.5）。
 * 本组件本任务不接入 Shell 中区（T16 再接）。
 */
export function AppGrid({ apps, onOpen, dormant = [] }: { apps: InstalledApp[]; onOpen: (appId: string) => void; dormant?: string[] }) {
  return (
    <div className="app-grid-root">
      {CATEGORIES.map((cat) => {
        const inCat = apps.filter((a) => a.category === cat.key);
        if (inCat.length === 0) return null;
        return (
          <section className="app-grid-section" key={cat.key}>
            <h3 className="app-grid-heading">
              <span className="app-grid-dot" style={{ background: cat.dot }} />
              {cat.label}
            </h3>
            <div className="app-grid">
              {inCat.map((a) => (
                <button className="app-card" key={a.app_id} onClick={() => onOpen(a.app_id)}>
                  <span className="app-card-icon" aria-hidden />
                  <span className="app-card-name">{a.display_name}</span>
                  {dormant.includes(a.app_id) ? (
                    <span className="chip chip-dormant" title="空闲超时已自动关闭以释放内存，点击重新打开">休眠</span>
                  ) : (
                    <span className="chip chip-idle">空闲</span>
                  )}
                  <SandboxBadge appId={a.app_id} />
                  {!a.trusted && <span className="app-card-unverified">未验证·受限</span>}
                </button>
              ))}
            </div>
          </section>
        );
      })}
    </div>
  );
}
