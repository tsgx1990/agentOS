import { useEffect, useState } from "react";
import "./SkillPendingConfirm.css";
import { SkillMetaSummary } from "./SkillMetaSummary";
import {
  listPendingSkillInstalls,
  skillRespondInstallConfirm,
  type PendingSkillInstall,
} from "../lib/skills";

/**
 * Maker 生成技能的安装确认面（spec §6 "Maker 生成"来源）：镜像
 * `MakerInstallConfirm.tsx`（应用侧同类流程）的刷新/操作模式——挂载时拉一次，
 * 批准/拒绝后重新拉取，已解决的条目自然从列表消失，不额外维护本地摘除状态。
 *
 * **命令签名为假定**（`list_pending_skill_installs`/`skill_respond_install_confirm`，
 * 见 `lib/skills.ts` 顶部的详细说明）——批次 D（Task 7，Maker 技能确认的后端 seam）
 * 截至本组件编写时尚未落地：`src-tauri/src/lib.rs` 的 `generate_handler!` 里没有
 * 这两个命令名，`maker.rs` 也没有任何 `skill` 分支。这里按简报给出的命令名 +
 * `SkillPreview`（`meta`+`scan`）的形状写出前端封装与本组件，一旦后端落地且签名
 * 与假定一致即可直接跑通；若签名不同，只需改 `lib/skills.ts` 里这两个函数与
 * `PendingSkillInstall` 类型，本组件复用 `SkillMetaSummary` 的渲染逻辑不受影响。
 *
 * 与 `SkillInstallDialog` 同样的安全闸门：不受信（Maker 输出恒 `trusted=false`，
 * 同应用侧 `maker::resolve_install` 的 `pending.trusted` 写死逻辑）+ 存在 High
 * 级发现时，「批准」按钮换成禁用态 + 说明，只留「拒绝」。
 *
 * 无 pending 项时不渲染任何内容（含首次加载中的过渡态）——偶发确认面，不是
 * 常驻列表。
 */
export function SkillPendingConfirm() {
  const [items, setItems] = useState<PendingSkillInstall[] | null>(null);
  const [err, setErr] = useState<string | null>(null);
  const [busyId, setBusyId] = useState<string | null>(null);

  const refresh = () =>
    listPendingSkillInstalls().then(setItems).catch((e) => setErr(String(e)));
  useEffect(() => { refresh(); }, []);

  async function respond(confirmId: string, allow: boolean) {
    setBusyId(confirmId);
    setErr(null);
    try {
      await skillRespondInstallConfirm(confirmId, allow);
      refresh();
    } catch (e) {
      setErr(String(e));
    } finally {
      setBusyId(null);
    }
  }

  if (!items || items.length === 0) return null;

  return (
    <div className="skill-pending-confirm">
      <h3>技能安装确认</h3>
      {err && <p className="skill-pending-err">{err}</p>}
      <div className="skill-pending-list">
        {items.map((item) => {
          const hasHigh = item.scan.findings.some((f) => f.severity === "high");
          // Maker 输出恒不受信（同 maker.rs::handle_install 对应用草稿的处理），
          // 有 High 发现即拦「批准」，与 SkillInstallDialog 的拦截规则保持一致。
          const blocked = hasHigh;
          return (
            <div className="skill-pending-row" key={item.confirm_id}>
              <SkillMetaSummary meta={item.meta} scan={item.scan} trusted={false} sourceLabel="Maker 生成" />
              {blocked && (
                <p className="skill-pending-blocked-reason">
                  存在高危内容，Maker 生成的技能未经信任来源审核，不能直接批准——请检查扫描发现后再决定。
                </p>
              )}
              <div className="skill-pending-actions">
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
                  disabled={busyId === item.confirm_id || blocked}
                >
                  批准
                </button>
              </div>
            </div>
          );
        })}
      </div>
    </div>
  );
}
