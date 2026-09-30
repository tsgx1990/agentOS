import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import "./MakerInstallConfirm.css";

type PendingInstall = { confirm_id: string; display_name: string; permissions: string[] };

/**
 * Maker 安装确认面（P4 T5b）：`__host_maker_install__` 登记的 pending install
 * （见 `maker.rs`"执行期决策：T5 安装权限确认 seam · 方案 B"）在这里对用户
 * 可见——`list_pending_installs`（只读查询，不消费，见其 Rust 文档）拉取当前
 * 所有待确认安装的 `display_name` + 权限人话预览（`permissions::render_human`，
 * 与 `InstallDialog` 展示权限的方式一致）；用户点「批准」/「拒绝」→
 * `maker_respond_install_confirm(confirmId, allow)`（Task5 引入、本组件唯一
 * 调用的安装/丢弃入口）→ 成功后重新拉取列表——已解决的条目已从
 * `McpManager.pending_installs` 里移除，自然不再出现，不需要额外的本地状态
 * 摘除逻辑。
 *
 * 刷新机制镜像 `NotificationCenter`：挂载时 `useEffect` 拉一次，每次操作
 * （批准/拒绝）后调用同一个 `refresh()`，不引入轮询/事件订阅——`NotificationCenter`
 * 本身也是这个模式（见其模块文档），这里复用同一套，不新造一种数据刷新方式。
 *
 * 与 MCP 写确认是两条完全独立的通道：`NotificationCenter` 的 `confirm_request`
 * 分支调 `respond_confirm`（Task7/15 的 `PendingCall` resume 语义）；本组件只调
 * `list_pending_installs`/`maker_respond_install_confirm`，两套确认体系在前端
 * 也保持不交叉，与后端 `pending`/`pending_installs` 两张独立表的隔离一致。
 *
 * 无 pending install 时不渲染任何内容（含首次加载中的过渡态）——这是一个
 * 偶发出现的确认面，不是常驻列表，空着不该占用界面空间。
 */
export function MakerInstallConfirm() {
  const [items, setItems] = useState<PendingInstall[] | null>(null);
  const [err, setErr] = useState<string | null>(null);
  const [busyId, setBusyId] = useState<string | null>(null);

  const refresh = () =>
    invoke<PendingInstall[]>("list_pending_installs").then(setItems).catch((e) => setErr(String(e)));
  useEffect(() => { refresh(); }, []);

  async function respond(confirmId: string, allow: boolean) {
    setBusyId(confirmId);
    setErr(null);
    try {
      await invoke("maker_respond_install_confirm", { confirmId, allow });
      refresh();
    } catch (e) {
      setErr(String(e));
    } finally {
      setBusyId(null);
    }
  }

  if (!items || items.length === 0) return null;

  return (
    <div className="maker-install-confirm">
      <h3>应用安装确认</h3>
      {err && <p className="maker-install-err">{err}</p>}
      <div className="maker-install-list">
        {items.map((item) => (
          <div className="maker-install-row" key={item.confirm_id}>
            <p className="maker-install-name">{item.display_name}</p>
            <p className="maker-install-request-line">请求安装，此应用请求：</p>
            <ul>{item.permissions.map((p, i) => <li key={i}>{p}</li>)}</ul>
            <div className="maker-install-actions">
              <button
                className="deny"
                onClick={() => respond(item.confirm_id, false)}
                disabled={busyId === item.confirm_id}
              >
                拒绝
              </button>
              <button
                className="primary"
                onClick={() => respond(item.confirm_id, true)}
                disabled={busyId === item.confirm_id}
              >
                批准
              </button>
            </div>
          </div>
        ))}
      </div>
    </div>
  );
}
