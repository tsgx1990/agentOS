import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import "./Shell.css";
import { NavRail } from "./NavRail";
import { SessionPanel } from "./SessionPanel";
import { Chat } from "./Chat";
import { AppGrid } from "./AppGrid";
import { AppFrame } from "./AppFrame";
import { InstallDialog } from "./InstallDialog";
import { UninstallDialog } from "./UninstallDialog";
import { AuditView } from "./AuditView";
import { ConnectorSettings } from "./ConnectorSettings";
import { ModelSettings } from "./ModelSettings";
import { MarketView } from "./MarketView";
import { NotificationCenter } from "./NotificationCenter";
import { ApprovalCenter } from "./ApprovalCenter";
import { MakerInstallConfirm } from "./MakerInstallConfirm";
import { SkillPendingConfirm } from "./SkillPendingConfirm";
import { SkillsView } from "./SkillsView";
import { OnboardingWizard } from "./OnboardingWizard";
import { listApps, type InstalledApp } from "../lib/registry";
import { installBridgeRelay } from "../lib/bridge";
import { listProviders } from "../lib/providers";

/**
 * C4a 三栏工作台外壳：左类目导航 168px · 中内容区 1fr · 右主助手 288px。
 *
 * 中区状态机：`uninstalling`（卸载对话框打开）→ `UninstallDialog`；`open`（某应用
 * 已打开）→ `AppFrame`；`installing`（安装对话框打开）→ `InstallDialog`；
 * `apps.length > 0`（有已装应用但都未打开）→ `AppGrid`；否则回落主助手 `Chat`。
 * 右栏在 `open` 时翻转为精简模式（`compact`），并始终挂着安装入口（`onInstall`）。
 *
 * `AppFrame` 头部的「← 返回」/「卸载」分别接线到 `closeApp`（`close_app` 释放该应用的
 * pi 会话 + 界面槽位 + 并发闸门名额，否则应用打开后永远无法离开）与打开
 * `UninstallDialog`；卸载成功（`onDone`）后同样要 `close_app` 收尾（应用已被卸载，
 * 其会话不能再留着），再清空 `open`、刷新应用列表。
 *
 * `auditing`（Task 9 新增）：`SessionPanel` 的「查看审计日志」入口置位，中区
 * 切到 `AuditView`（`list_audit` 只读查询，无副作用）；项目暂无独立设置区，
 * 借用常驻的 `SessionPanel` 承载入口，优先级仅次于卸载对话框——高于已打开的
 * 应用/安装对话框/应用网格/主助手回落，因为它是用户主动导航过去的只读视图。
 *
 * `connectorSettings`/`notifications`（Task17 新增）：同 `auditing` 一样借用
 * `SessionPanel` 常驻入口（「连接器设置」/「通知中心」）+ 同一优先级挂进中区，
 * 分别切到 `ConnectorSettings`（MCP server 增删查）与 `NotificationCenter`
 * （通知列表 + 确认请求的允许/总是允许/拒绝）。三个只读/轻写导航视图互斥关系
 * 未强制（各自独立 state），但正常操作路径下用户一次只会点开一个入口。
 *
 * `notifications` 分支里额外挂了 `MakerInstallConfirm`（P4 T5b，`NotificationCenter`
 * 之上）：Maker 生成草稿请求安装（`__host_maker_install__` 登记的 pending
 * install，见 `maker.rs`"执行期决策：T5 安装权限确认 seam"）在这里展示 app 名 +
 * 权限预览供用户批准/拒绝——与 `NotificationCenter` 的 `confirm_request`
 * （MCP 写确认）是两条独立通道，只是共享同一个导航入口，不合并渲染逻辑；
 * 该组件无 pending 项时不渲染任何内容，不影响 `NotificationCenter` 一贯的
 * 空态展示。
 *
 * `approvals`（P6-C Task7 新增）：同一批常驻入口再加「审批中心」，切到
 * `ApprovalCenter`（待批写调用分组批量验收 + 放行规则撤销）。
 * `NotificationCenter` 的 `confirm_request` 条目不再直接摆三个按钮，改为
 * 「去审批中心」——其 `onOpenApprovals` 接到这里，同时置位 `approvals` 并把
 * `notifications` 收掉，实现"从通知中心跳过去"的观感。
 *
 * `skills`（P6-B Task 8 新增）：同一批常驻入口再加「技能」，切到 `SkillsView`
 * （已装技能启停/授予/卸载 + 市场 `kind==="skill"` 条目 + 本地目录导入）。
 * `notifications` 分支里 `MakerInstallConfirm` 旁再挂 `SkillPendingConfirm`
 * （Maker 生成技能的安装确认，命令签名假定——见 `lib/skills.ts` 顶部说明）——
 * 与 `MakerInstallConfirm` 同一套"共享导航入口、不合并渲染逻辑、无 pending 项
 * 不渲染任何内容"的处理方式。
 *
 * 首次启动引导（Task12 新增；P6-D 改为不限 provider）：挂载时用 `list_providers`
 * 查是否有任一 provider 已在 keychain 配置 BYOK key——`hasKey===null` 是加载中
 * 过渡态；`hasKey===false`（首次启动/所有 key 被清空）整个替换渲染
 * `OnboardingWizard`（欢迎→选服务→配 key 并测试→可选装起步应用），不渲染三栏
 * 外壳；向导 `onComplete` 时把 `hasKey` 置 true，退回渲染下面正常的三栏工作台。
 * 已配置任一 key 的老用户 `hasKey` 直接为 true。
 *
 * `modelSettings`（P6-D 新增）：「模型与密钥」入口，同 `connectorSettings` 的挂载
 * 方式，切到 `ModelSettings`（服务与密钥、默认模型、按应用覆盖、用量）。
 * 详见 .superpowers/sdd/task-12-brief.md。
 *
 * 详见 docs/superpowers/specs/2026-07-17-ui-direction-c4a.md §3/§4，
 * 及 .superpowers/sdd/task-17-brief.md、.superpowers/sdd/task-16-brief.md、
 * .superpowers/sdd/task-9-brief.md。
 */
export function Shell() {
  const [apps, setApps] = useState<InstalledApp[]>([]);
  const [open, setOpen] = useState<{ appId: string; slot: number } | null>(null);
  const [installing, setInstalling] = useState(false);
  const [uninstalling, setUninstalling] = useState<string | null>(null);
  const [auditing, setAuditing] = useState(false);
  const [connectorSettings, setConnectorSettings] = useState(false);
  const [modelSettings, setModelSettings] = useState(false);
  const [market, setMarket] = useState(false);
  const [notifications, setNotifications] = useState(false);
  const [approvals, setApprovals] = useState(false);
  const [skills, setSkills] = useState(false);
  const [hasKey, setHasKey] = useState<boolean | null>(null);

  const refresh = () => listApps().then(setApps).catch(() => setApps([]));
  useEffect(() => { refresh(); }, []);
  useEffect(() => {
    listProviders()
      .then((ps) => setHasKey(ps.some((p) => p.configured)))
      .catch(() => setHasKey(false));
  }, []);

  useEffect(() => {
    // 自定义 scheme 的 origin 形如 `sagent3://localhost`（tauri
    // register_asynchronous_uri_scheme_protocol 文档所述格式）。这里用精确相等
    // 而非前缀匹配——slot 是数字，前缀匹配会让 slot=1 误配上 slot=10/11 的消息
    // （都以 "sagent1" 开头）。
    // P1 单开场景下一个 origin 只对应当前这一个 open.appId，够用；多开需要完整
    // 的 slot→appId 映射（宿主可加命令 `list_open` 暴露，或前端自维护已开表），
    // 留作后续增强。
    return installBridgeRelay((origin) =>
      open && origin === `sagent${open.slot}://localhost` ? open.appId : null
    );
  }, [open]);

  async function openApp(appId: string) {
    const slot = await invoke<number>("open_app", { appId });
    setOpen({ appId, slot });
  }

  async function closeApp() {
    if (!open) return;
    await invoke("close_app", { appId: open.appId });
    setOpen(null);
    refresh();
  }

  async function finishUninstall() {
    const appId = uninstalling;
    setUninstalling(null);
    if (appId) await invoke("close_app", { appId });
    setOpen(null);
    refresh();
  }

  if (hasKey === null) return <p className="shell-loading">加载中……</p>;
  if (!hasKey) return <OnboardingWizard onComplete={() => setHasKey(true)} />;

  return (
    <div className="shell">
      <NavRail apps={apps} />
      <main className="shell-center">
        {uninstalling ? (
          <UninstallDialog
            appId={uninstalling}
            onDone={finishUninstall}
            onCancel={() => setUninstalling(null)}
          />
        ) : auditing ? (
          <AuditView onClose={() => setAuditing(false)} />
        ) : connectorSettings ? (
          <ConnectorSettings onClose={() => setConnectorSettings(false)} />
        ) : modelSettings ? (
          <ModelSettings onClose={() => setModelSettings(false)} />
        ) : market ? (
          <MarketView onClose={() => setMarket(false)} onInstalled={refresh} />
        ) : notifications ? (
          <>
            <MakerInstallConfirm />
            <SkillPendingConfirm />
            <NotificationCenter
              onClose={() => setNotifications(false)}
              onOpenApprovals={() => { setNotifications(false); setApprovals(true); }}
            />
          </>
        ) : approvals ? (
          <ApprovalCenter onClose={() => setApprovals(false)} />
        ) : skills ? (
          <SkillsView onClose={() => setSkills(false)} />
        ) : open ? (
          <AppFrame
            slot={open.slot}
            appId={open.appId}
            onClose={closeApp}
            onUninstall={() => setUninstalling(open.appId)}
          />
        ) : installing ? (
          <InstallDialog
            onDone={() => { setInstalling(false); refresh(); }}
            onCancel={() => setInstalling(false)}
          />
        ) : apps.length > 0 ? (
          <AppGrid apps={apps} onOpen={openApp} />
        ) : (
          <Chat />
        )}
      </main>
      <SessionPanel
        compact={!!open}
        onInstall={() => setInstalling(true)}
        onAudit={() => setAuditing(true)}
        onConnectorSettings={() => setConnectorSettings(true)}
        onModelSettings={() => setModelSettings(true)}
        onMarket={() => setMarket(true)}
        onNotifications={() => setNotifications(true)}
        onApprovals={() => setApprovals(true)}
        onSkills={() => setSkills(true)}
      />
    </div>
  );
}
