import { useState } from "react";
import "./PublishButton.css";
import { publishApp } from "../lib/market";

/**
 * 发布按钮（P5 §11.6 机械化内核的最小前端 affordance）：把当前应用导出成可分发形态
 * + 生成市场条目（`publish_app`），成功后提示产出路径 + "把 published/ 推到你的市场
 * 仓库即可发布"。真实推送到 GitHub 是手工里程碑，不在此自动执行。挂在 `AppFrame` 头部
 * （返回/卸载旁边）。
 */
export function PublishButton({ appId }: { appId: string }) {
  const [status, setStatus] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  async function publish() {
    setBusy(true);
    setStatus(null);
    try {
      const entry = await publishApp(appId);
      setStatus(
        `已导出「${entry.display_name}」到 published/${appId}/ 并写入 index.json——把 published/ 推到你的市场仓库即可发布。`,
      );
    } catch (e) {
      setStatus(`发布失败：${String(e)}`);
    } finally {
      setBusy(false);
    }
  }

  return (
    <span className="publish-button">
      <button onClick={publish} disabled={busy}>
        {busy ? "发布中…" : "发布"}
      </button>
      {status && <span className="publish-status">{status}</span>}
    </span>
  );
}
