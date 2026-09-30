import { useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import "./InstallDialog.css";

/**
 * 卸载应用：保留/删除应用数据单选 → `uninstall_app`。
 * 杀运行中会话属于 Task 16/20 接线前置（见 lib.rs `uninstall_app` 注释），
 * 本对话框只负责数据取舍这一步。
 */
export function UninstallDialog({ appId, onDone, onCancel }: { appId: string; onDone: () => void; onCancel: () => void }) {
  const [keep, setKeep] = useState(true);
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  async function run() {
    setBusy(true);
    setErr(null);
    try {
      await invoke("uninstall_app", { appId, keepAppData: keep });
      onDone();
    } catch (e) {
      setErr(String(e));
      setBusy(false);
    }
  }
  return (
    <div className="uninstall-dialog">
      <h2>卸载应用</h2>
      <label><input type="radio" checked={keep} onChange={() => setKeep(true)} />保留应用数据</label>
      <label><input type="radio" checked={!keep} onChange={() => setKeep(false)} />一并删除应用数据</label>
      {err && <p className="install-err">{err}</p>}
      <div>
        <button onClick={onCancel}>取消</button>
        <button onClick={run} disabled={busy}>卸载</button>
      </div>
    </div>
  );
}
