import { useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import "./InstallDialog.css";
import { CapabilityPanel } from "./CapabilityPanel";
import type { CapabilityReport } from "../lib/registry";

type Preview = {
  display_name: string;
  category: string;
  permissions: string[];
  existing_version: string | null;
  capabilities: CapabilityReport[];
  sandboxed: boolean;
};

/**
 * 从文件安装应用：路径文本输入（P1，原生文件夹选择器列为后续打磨项）→
 * `preview_install`（只校验 + 返回能力诊断报告，不产生安装副作用）→ 用
 * `CapabilityPanel` 展示每条能力的人话 + 强制点标签（Task10：安装前必须先
 * 看到"由谁强制"，不再只是一份权限人话列表）供用户确认 → `install_app(path,
 * trusted=false)` 落位安装。
 */
export function InstallDialog({ onDone, onCancel }: { onDone: () => void; onCancel: () => void }) {
  const [path, setPath] = useState("");
  const [preview, setPreview] = useState<Preview | null>(null);
  const [err, setErr] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  async function doPreview() {
    setErr(null);
    try { setPreview(await invoke<Preview>("preview_install", { sourcePath: path })); }
    catch (e) { setErr(String(e)); }
  }
  async function doInstall() {
    setBusy(true); setErr(null);
    try { await invoke("install_app", { sourcePath: path, trusted: false }); onDone(); }
    catch (e) { setErr(String(e)); setBusy(false); }
  }

  return (
    <div className="install-dialog">
      <h2>从文件安装应用</h2>
      <input placeholder="应用文件夹路径" value={path} onChange={(e) => setPath(e.target.value)} />
      <button onClick={doPreview} disabled={!path}>预览</button>
      {err && <p className="install-err">{err}</p>}
      {preview && (
        <div className="install-preview">
          <p className="install-name">{preview.display_name}
            {preview.existing_version && <span>（升级自 {preview.existing_version}）</span>}</p>
          <p className="install-perm-title">此应用请求：</p>
          <CapabilityPanel reports={preview.capabilities} sandboxed={preview.sandboxed} />
          <div className="install-actions">
            <button onClick={onCancel}>取消</button>
            <button className="primary" onClick={doInstall} disabled={busy}>安装</button>
          </div>
        </div>
      )}
    </div>
  );
}
