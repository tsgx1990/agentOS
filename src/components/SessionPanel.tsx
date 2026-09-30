import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import "./SessionPanel.css";
import { appUsage, type UsageResponse } from "../lib/usage";

/**
 * 右栏（P0 复用为「会话 / 用量面板」）：显示当前会话名 + 主助手 token 用量
 * （`app_usage("main")`，Task17），并常驻「从文件安装应用」入口
 * （`onInstall`）、「查看审计日志」（`onAudit`）、「连接器设置」
 * （`onConnectorSettings`，Task17 新增，`ConnectorSettings` 挂载入口）、
 * 「通知中心」（`onNotifications`，Task17 新增，`NotificationCenter` 挂载
 * 入口）、「审批中心」（`onApprovals`，P6-C Task7 新增，`ApprovalCenter` 挂载
 * 入口——待批写调用的分组批量验收 + 放行规则撤销）、「技能」（`onSkills`，P6-B
 * Task 8 新增，`SkillsView` 挂载入口——已装技能的启停/授予/卸载 + 市场技能条目
 * + 本地导入）与「重启会话」入口——本项目
 * 暂无独立「设置」区，都借用这个常驻面板承载。中区打开某个应用时（`compact`），
 * 翻转为精简模式：只留头部 + 安装入口，让出空间给正在使用的应用，不再展示主
 * 助手自己的会话/用量卡片，也不拉取用量（无展示位置，省一次 invoke）。
 * 见 docs/superpowers/specs/2026-07-17-ui-direction-c4a.md §4/§13。
 *
 * 「重启会话/重开配置」：调 `restart_session`（kill 主会话子进程 + 重新
 * spawn）。主助手多次异常退出触发 faulted 后，`main_session` 已被后端清空，
 * 这个按钮是用户手动恢复的唯一入口；平时也可用于「改了 BYOK key/配置后
 * 想重开一次」。
 */
export function SessionPanel({
  compact = false,
  onInstall,
  onAudit,
  onConnectorSettings,
  onMarket,
  onNotifications,
  onApprovals,
  onSkills,
}: {
  compact?: boolean;
  onInstall?: () => void;
  onAudit?: () => void;
  onConnectorSettings?: () => void;
  onMarket?: () => void;
  onNotifications?: () => void;
  onApprovals?: () => void;
  onSkills?: () => void;
} = {}) {
  const [restarting, setRestarting] = useState(false);
  const [restartErr, setRestartErr] = useState<string | null>(null);
  const [usage, setUsage] = useState<UsageResponse | null>(null);

  useEffect(() => {
    if (compact) return;
    appUsage("main").then(setUsage).catch(() => { /* 用量是次要信息，拉取失败不影响面板其余部分可用 */ });
  }, [compact]);

  async function restartSession() {
    setRestarting(true);
    setRestartErr(null);
    try {
      await invoke("restart_session");
    } catch (e) {
      setRestartErr(String(e));
    } finally {
      setRestarting(false);
    }
  }

  return (
    <aside className={`session-panel${compact ? " compact" : ""}`}>
      <div className="session-panel-header">
        <span className="session-orb" />
        <div>
          <div className="session-panel-title">主助手</div>
          <div className="session-panel-subtitle">{compact ? "精简模式" : "会话 / 用量"}</div>
        </div>
      </div>

      {!compact && (
        <div className="session-card">
          <div className="session-card-title">当前会话</div>
          <div className="session-row">
            <span className="session-row-label">名称</span>
            <span className="session-row-value">默认会话</span>
          </div>
        </div>
      )}

      {!compact && (
        <div className="session-card">
          <div className="session-card-title">用量</div>
          <div className="session-row">
            <span className="session-row-label">已用 tokens</span>
            <span className="session-row-value">{usage ? usage.input + usage.output : "—"}</span>
          </div>
          <div className="session-row">
            <span className="session-row-label">估算花费</span>
            <span className="session-row-value">{usage ? `$${usage.cost.toFixed(4)}` : "—"}</span>
          </div>
        </div>
      )}

      {onInstall && (
        <button className="session-install-btn" onClick={onInstall}>从文件安装应用</button>
      )}

      {onAudit && (
        <button className="session-install-btn" onClick={onAudit}>查看审计日志</button>
      )}

      {onConnectorSettings && (
        <button className="session-install-btn" onClick={onConnectorSettings}>连接器设置</button>
      )}

      {onMarket && (
        <button className="session-install-btn" onClick={onMarket}>应用市场</button>
      )}

      {onNotifications && (
        <button className="session-install-btn" onClick={onNotifications}>通知中心</button>
      )}

      {onApprovals && (
        <button className="session-install-btn" onClick={onApprovals}>审批中心</button>
      )}

      {onSkills && (
        <button className="session-install-btn" onClick={onSkills}>技能</button>
      )}

      <button className="session-install-btn" onClick={restartSession} disabled={restarting}>
        {restarting ? "重启中…" : "重启会话/重开配置"}
      </button>
      {restartErr && <p className="session-panel-err">{restartErr}</p>}
    </aside>
  );
}
