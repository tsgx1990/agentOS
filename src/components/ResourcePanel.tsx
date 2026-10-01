import { useCallback, useEffect, useRef, useState } from "react";
import {
  clearCaches,
  diskReport,
  formatBytes,
  formatDuration,
  getIdlePolicy,
  resourceReport,
  setIdlePolicy,
  type AppUsage,
  type ClearReport,
  type DiskReport,
  type GroupUsage,
  type IdlePolicy,
  type ResourceReport,
} from "../lib/resources";
import "./ResourcePanel.css";

/** 内置 Maker 的应用 id（与 Rust `maker::MAKER_APP_ID` 一致），它从不被回收，不提供「不休眠」开关。 */
const MAKER_APP_ID = "superagent";
const POLL_MS = 5000;
const MIN_MINUTES = 5;
const MAX_MINUTES = 1440;

type AppInfo = { app_id: string; display_name: string };

type Props = {
  /** 已装应用，用来把 app_id 显示成名字。 */
  apps: AppInfo[];
  onCloseApp?: (appId: string) => void | Promise<void>;
  onClose?: () => void;
};

function cpuText(ready: boolean, v: number): string {
  return ready ? `${v.toFixed(1)}%` : "—";
}

function mbThreshold(bytes: number): string {
  return `${Math.round(bytes / (1024 * 1024))} MB`;
}

/** 应用行的状态：回复中 / 不会休眠的原因 / 约 N 分钟后休眠。 */
function appStatus(a: AppUsage, policy: IdlePolicy | null): { text: string; tone: "run" | "idle" | "warn" } {
  if (a.in_turn) return { text: "回复中", tone: "run" };
  if (a.exempt) return { text: a.exempt, tone: "idle" };
  if (policy && a.idle_secs != null) {
    const left = policy.timeout_secs - a.idle_secs;
    if (left <= 60) return { text: "即将休眠", tone: "warn" };
    return { text: `约 ${Math.ceil(left / 60)} 分钟后休眠`, tone: "warn" };
  }
  return { text: "—", tone: "idle" };
}

/** 「资源」面板：进程内存 / CPU、空闲回收策略、磁盘占用与缓存清理。 */
export function ResourcePanel({ apps, onCloseApp, onClose }: Props) {
  const [report, setReport] = useState<ResourceReport | null>(null);
  const [policy, setPolicy] = useState<IdlePolicy | null>(null);
  const [disk, setDisk] = useState<DiskReport | null>(null);
  const [err, setErr] = useState<string | null>(null);
  const [minutes, setMinutes] = useState("15");
  const [enabled, setEnabled] = useState(true);
  const [saving, setSaving] = useState(false);
  const [confirming, setConfirming] = useState(false);
  const [cleared, setCleared] = useState<ClearReport | null>(null);
  const alive = useRef(true);

  const pull = useCallback(async () => {
    try {
      const r = await resourceReport();
      if (alive.current) {
        setReport(r);
        setErr(null);
      }
    } catch (e) {
      if (alive.current) setErr(String(e));
    }
  }, []);

  const pullDisk = useCallback(async () => {
    try {
      const d = await diskReport();
      if (alive.current) setDisk(d);
    } catch (e) {
      if (alive.current) setErr(String(e));
    }
  }, []);

  useEffect(() => {
    alive.current = true;
    void pull();
    void pullDisk();
    getIdlePolicy()
      .then((p) => {
        if (!alive.current) return;
        setPolicy(p);
        setEnabled(p.enabled);
        setMinutes(String(Math.round(p.timeout_secs / 60)));
      })
      .catch((e) => alive.current && setErr(String(e)));
    const t = setInterval(() => void pull(), POLL_MS);
    return () => {
      alive.current = false;
      clearInterval(t);
    };
  }, [pull, pullDisk]);

  const nameOf = (id: string) => apps.find((a) => a.app_id === id)?.display_name ?? id;

  const minutesNum = Number(minutes);
  const minutesOk =
    minutes.trim() !== "" && Number.isInteger(minutesNum) && minutesNum >= MIN_MINUTES && minutesNum <= MAX_MINUTES;

  async function savePolicy(next: IdlePolicy) {
    setSaving(true);
    try {
      await setIdlePolicy(next);
      if (alive.current) setPolicy(next);
      void pull();
    } catch (e) {
      if (alive.current) setErr(String(e));
    } finally {
      if (alive.current) setSaving(false);
    }
  }

  function onSave() {
    if (!policy || !minutesOk) return;
    void savePolicy({ ...policy, enabled, timeout_secs: minutesNum * 60 });
  }

  function toggleExempt(appId: string, on: boolean) {
    if (!policy) return;
    const set = new Set(policy.exempt_apps);
    if (on) set.add(appId);
    else set.delete(appId);
    void savePolicy({ ...policy, exempt_apps: [...set].sort() });
  }

  async function doClear() {
    setConfirming(false);
    try {
      const c = await clearCaches();
      if (alive.current) setCleared(c);
      void pullDisk();
    } catch (e) {
      if (alive.current) setErr(String(e));
    }
  }

  async function closeApp(id: string) {
    try {
      await onCloseApp?.(id);
    } catch (e) {
      if (alive.current) setErr(String(e));
    }
    void pull();
  }

  const nowSecs = Math.floor(Date.now() / 1000);
  const sampledAgo = report ? Math.max(0, nowSecs - report.sampled_at) : 0;
  const total = report?.total_rss_bytes ?? 0;
  const pct = (b: number) => (total > 0 ? Math.min(100, (b / total) * 100) : 0);

  const systemGroups: { key: string; name: string; hint: string; g: GroupUsage }[] = [];
  if (report) {
    systemGroups.push({ key: "host", name: "宿主", hint: "Super Agent OS 本体", g: report.host });
    if (report.main) systemGroups.push({ key: "main", name: "主助手", hint: "常驻的主会话", g: report.main });
    report.mcp_servers.forEach((g) =>
      systemGroups.push({ key: `mcp-${g.label}`, name: g.label, hint: "MCP 服务", g }),
    );
    systemGroups.push({ key: "other", name: "其它", hint: "Maker 预览会话等", g: report.other });
  }

  const statusChip = (a: AppUsage) => {
    const s = appStatus(a, policy);
    const cls = s.tone === "run" ? "rp-chip-run" : s.tone === "warn" ? "rp-chip-warn" : "rp-chip-idle";
    return <span className={`rp-chip ${cls}`}>{s.text}</span>;
  };

  const appActions = (a: AppUsage) => (
    <div className="rp-actions">
      <label className={`rp-switch${a.app_id === MAKER_APP_ID ? " is-disabled" : ""}`}>
        <input
          type="checkbox"
          aria-label={`${nameOf(a.app_id)} 不休眠`}
          checked={a.app_id === MAKER_APP_ID || !!policy?.exempt_apps.includes(a.app_id)}
          disabled={!policy || saving || a.app_id === MAKER_APP_ID}
          onChange={(e) => toggleExempt(a.app_id, e.target.checked)}
        />
        <span>不休眠</span>
      </label>
      {/* 只有后台会话（没有打开的界面会话）时关闭是 no-op，不显示。 */}
      {a.opened_at != null && (
        <button type="button" className="rp-btn" onClick={() => void closeApp(a.app_id)}>
          关闭
        </button>
      )}
    </div>
  );

  const timesText = (a: AppUsage) => {
    const opened = a.opened_at != null ? formatDuration(nowSecs - a.opened_at) : "—";
    const idle = a.idle_secs != null ? formatDuration(a.idle_secs) : "—";
    return { opened, idle };
  };

  return (
    <section className="rp" aria-label="资源">
      <header className="rp-header">
        <div className="rp-header-row">
          <h2>资源</h2>
          {onClose && (
            <button type="button" className="rp-btn" onClick={onClose}>
              关闭
            </button>
          )}
        </div>
        <p>各应用占用的内存与 CPU；闲置的应用会自动休眠，随时可再打开。</p>
      </header>

      {err && (
        <p className="rp-error" role="alert">
          {err}
        </p>
      )}

      {report && (
        <div className="rp-summary">
          <div className="rp-stat">
            <span className="rp-stat-label">总内存</span>
            <b>{formatBytes(report.total_rss_bytes)}</b>
          </div>
          <div className="rp-stat">
            <span className="rp-stat-label">总 CPU</span>
            <b>{cpuText(report.cpu_ready, report.total_cpu_percent)}</b>
          </div>
          <div className="rp-stat">
            <span className="rp-stat-label">进程数</span>
            <b>{report.total_proc_count}</b>
          </div>
          <div className="rp-stat">
            <span className="rp-stat-label">采样</span>
            <b className="rp-stat-soft">{sampledAgo} 秒前</b>
          </div>
        </div>
      )}

      {report && (
        <>
          <div className="rp-section">
            <h3 className="rp-section-title">
              应用<span className="rp-section-count">{report.apps.length}</span>
            </h3>
            {report.apps.length === 0 ? (
              <p className="rp-empty">当前没有打开的应用</p>
            ) : (
              <ul className="rp-cards">
                {report.apps.map((a) => {
                  const t = timesText(a);
                  return (
                    <li key={a.app_id} className="rp-card">
                      <div className="rp-card-head">
                        <div className="rp-card-title">
                          <span className="rp-name">{nameOf(a.app_id)}</span>
                          {statusChip(a)}
                        </div>
                        {appActions(a)}
                      </div>
                      <div className="rp-bar-row">
                        <b className="rp-big">{formatBytes(a.usage.rss_bytes)}</b>
                        <div
                          className="rp-bar"
                          role="meter"
                          aria-label={`${nameOf(a.app_id)} 内存占比`}
                          aria-valuemin={0}
                          aria-valuemax={100}
                          aria-valuenow={Math.round(pct(a.usage.rss_bytes))}
                        >
                          <i style={{ width: `${Math.max(2, pct(a.usage.rss_bytes))}%` }} />
                        </div>
                        <span className="rp-pct" title="占总内存的比例">
                          占总内存 {Math.round(pct(a.usage.rss_bytes))}%
                        </span>
                      </div>
                      <dl className="rp-facts">
                        <div>
                          <dt>CPU</dt>
                          <dd>{cpuText(report.cpu_ready, a.usage.cpu_percent)}</dd>
                        </div>
                        <div>
                          <dt>进程</dt>
                          <dd>{a.usage.proc_count}</dd>
                        </div>
                        <div>
                          <dt>已打开</dt>
                          <dd>{t.opened}</dd>
                        </div>
                        <div>
                          <dt>已空闲</dt>
                          <dd>{t.idle}</dd>
                        </div>
                        {a.background_sessions > 0 && (
                          <div>
                            <dt>后台会话</dt>
                            <dd>{a.background_sessions}</dd>
                          </div>
                        )}
                      </dl>
                    </li>
                  );
                })}
              </ul>
            )}
          </div>

          <div className="rp-section">
            <h3 className="rp-section-title">宿主与服务</h3>
            <ul className="rp-minis">
              {systemGroups.map(({ key, name, hint, g }) => (
                <li key={key} className="rp-mini">
                  <span className="rp-name">{name}</span>
                  <span className="rp-sub">{hint}</span>
                  <b className="rp-big">{formatBytes(g.rss_bytes)}</b>
                  <div className="rp-bar rp-bar-thin">
                    <i style={{ width: `${Math.max(2, pct(g.rss_bytes))}%` }} />
                  </div>
                  <span className="rp-sub">
                    {g.proc_count} 个进程 · CPU {cpuText(report.cpu_ready, g.cpu_percent)}
                  </span>
                </li>
              ))}
            </ul>
          </div>
        </>
      )}

      <div className="rp-section">
        <h3 className="rp-section-title">空闲策略</h3>
        <div className="rp-panel">
          <label className="rp-check">
            <input
              type="checkbox"
              checked={enabled}
              disabled={!policy}
              onChange={(e) => setEnabled(e.target.checked)}
            />
            <span>空闲超时后自动休眠应用</span>
          </label>
          <div className="rp-row">
            <label className="rp-field">
              <span>空闲超过</span>
              <input
                type="number"
                inputMode="numeric"
                min={MIN_MINUTES}
                max={MAX_MINUTES}
                value={minutes}
                aria-label="空闲阈值（分钟）"
                aria-invalid={!minutesOk}
                disabled={!policy}
                onChange={(e) => setMinutes(e.target.value)}
              />
              <span>分钟</span>
            </label>
            <button
              type="button"
              className="rp-btn rp-btn-primary"
              disabled={!policy || !minutesOk || saving}
              onClick={onSave}
            >
              保存
            </button>
          </div>
          {!minutesOk ? (
            <p className="rp-hint rp-hint-warn">阈值须是 {MIN_MINUTES}–{MAX_MINUTES} 分钟的整数。</p>
          ) : (
            <p className="rp-hint">
              {policy && policy.exempt_apps.length > 0
                ? `${policy.exempt_apps.length} 个应用被设为不休眠。`
                : "没有被设为不休眠的应用。"}
              正在回复、有待批调用、后台任务运行中的应用不会被休眠。
            </p>
          )}
        </div>
      </div>

      <div className="rp-section">
        <h3 className="rp-section-title">磁盘</h3>
        {disk && (
          <>
            <ul className="rp-disk">
              <li>
                <span>数据目录合计</span>
                <b>{formatBytes(disk.root_bytes)}</b>
              </li>
              <li>
                <span>审计日志</span>
                <b>{formatBytes(disk.audit_bytes)}</b>
              </li>
              <li>
                <span>通知</span>
                <b>{formatBytes(disk.notifications_bytes)}</b>
              </li>
              <li>
                <span>Maker 草稿</span>
                <b>{formatBytes(disk.maker_staging_bytes)}</b>
              </li>
              <li>
                <span>主助手会话</span>
                <b>{formatBytes(disk.main_sessions_bytes)}</b>
              </li>
            </ul>
            {disk.incomplete && (
              <p className="rp-hint rp-hint-notice rp-gap" role="status">
                部分目录层级过深，统计不完整，实际占用可能比这里显示的更大。
              </p>
            )}
            {disk.apps.length > 0 && (
              <div className="rp-tablewrap rp-gap">
                <table className="rp-disk-table">
                  <thead>
                    <tr>
                      <th className="rp-disk-name">应用</th>
                      <th className="num">会话文件</th>
                      <th className="num">应用数据</th>
                      <th className="num">工作目录</th>
                    </tr>
                  </thead>
                  <tbody>
                    {disk.apps.map((d) => (
                      <tr key={d.app_id}>
                        <td className="rp-disk-name">
                          <span className="rp-name">{nameOf(d.app_id)}</span>
                          {d.sessions_over_threshold && (
                            <span className="rp-chip rp-chip-warn">
                              会话文件超过 {mbThreshold(disk.threshold_bytes)}
                            </span>
                          )}
                          {d.incomplete && (
                            <span className="rp-chip rp-chip-idle" title="实际占用可能比显示的更大">
                              目录层级过深，统计不完整
                            </span>
                          )}
                        </td>
                        <td className="num">{formatBytes(d.sessions_bytes)}</td>
                        <td className="num">{formatBytes(d.data_bytes)}</td>
                        <td className="num">{formatBytes(d.agent_home_bytes)}</td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            )}
          </>
        )}
        <div className="rp-clear">
          {confirming ? (
            <div className="rp-confirm" role="alertdialog" aria-label="确认清理缓存">
              <p>只删旧的会话记录与无主草稿，不动应用数据、审计和已装应用。</p>
              <div className="rp-row">
                <button type="button" className="rp-btn rp-btn-primary" onClick={() => void doClear()}>
                  确认清理
                </button>
                <button type="button" className="rp-btn" onClick={() => setConfirming(false)}>
                  取消
                </button>
              </div>
            </div>
          ) : (
            <div className="rp-row">
              <button type="button" className="rp-btn" onClick={() => setConfirming(true)}>
                清理缓存
              </button>
              {cleared && (
                <span className="rp-done" role="status">
                  已释放 {formatBytes(cleared.freed_bytes)}
                  {cleared.refused.length > 0 && `，另有 ${cleared.refused.length} 项未处理（目录是链接、被替换或层级过深）`}
                </span>
              )}
            </div>
          )}
        </div>
      </div>
    </section>
  );
}
