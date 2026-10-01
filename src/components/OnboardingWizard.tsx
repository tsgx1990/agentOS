import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import "./OnboardingWizard.css";
import { setApiKey } from "../lib/keys";
import {
  TEST_COST_HINT,
  getModelSettings,
  listProviders,
  setGlobalModel,
  testProvider,
  type ProbeReport,
  type ProviderInfo,
} from "../lib/providers";
import { OnboardingProviderStep } from "./OnboardingProviderStep";
import { ProbeResult } from "./ProviderDetail";

type Step = "welcome" | "provider" | "key" | "app";

/**
 * 起步应用候选：`name` 对应后端 `install_builtin_sample` 的白名单样例目录名
 * （whole-branch review I1 修复——不再是裸相对 `sourcePath` 交给 `install_app`
 * 那条在生产环境没有 base 解析、必然失败的旧管道，见该命令文档）。
 *
 * 「应用工坊」（Maker）不再在这里列出：它已经在应用启动时由
 * `seed_builtin_maker` 自动播种为 **trusted** 内置应用（见 I2——Maker 必须
 * trusted 才能连上模型 API），无需（也不应该）再经向导重复安装一遍。这里
 * 只保留一个真正装得上的招牌起步样例——「待办便签」，最小的生活工具，感受
 * 应用是怎么跑起来的；OAuth 与更多起步应用一样后置，不在本向导。
 */
const STARTER_APPS: { id: string; label: string; desc: string; name: string }[] = [
  { id: "todo-notes", label: "待办便签", desc: "最小的生活工具样例：记点滴待办，感受应用是怎么跑起来的。", name: "todo-notes" },
];

/**
 * 首次启动 key-first 引导向导：欢迎 → 选模型服务（国内 / 国际卡片，P6-D）→
 * 配置 BYOK key（存系统钥匙串，`set_api_key`，复用 `KeySetup` 同款 try/catch
 * 错误展示）并自动做一次连通性测试：成功则（尚无全局默认时）把该服务的首个预设
 * 设为全局默认；失败显示原因，可「重新填写」或「仍然继续」（不让网络 / 代理问题
 * 把人锁在门外）→ 可选装一个起步
 * 应用（`install_builtin_sample`，白名单 + resource_dir 解析——不是
 * `InstallDialog` 手动装第三方包走的 `install_app(sourcePath, trusted)` 管道）
 * → 完成回调（交回 `Shell` 渲主工作台）。OAuth 未做，本向导只有 BYOK 一条
 * 路径。全程只用 C4a token。
 */
export function OnboardingWizard({ onComplete }: { onComplete: () => void }) {
  const [step, setStep] = useState<Step>("welcome");

  const [providers, setProviders] = useState<ProviderInfo[] | null>(null);
  const [providerErr, setProviderErr] = useState("");
  const [selectedId, setSelectedId] = useState<string | null>(null);

  const [key, setKey] = useState("");
  const [keyErr, setKeyErr] = useState("");
  const [saving, setSaving] = useState(false);
  const [probe, setProbe] = useState<ProbeReport | null>(null);
  // 测试通过、密钥已存，但设全局默认失败：给提示并允许继续（同 continueAnyway）。
  const [defaultErr, setDefaultErr] = useState("");

  const [installingId, setInstallingId] = useState<string | null>(null);
  const [installedId, setInstalledId] = useState<string | null>(null);
  const [installErr, setInstallErr] = useState<string | null>(null);

  useEffect(() => {
    listProviders()
      .then((ps) => setProviders(Array.isArray(ps) ? ps : []))
      .catch((e) => {
        setProviders([]);
        setProviderErr(String(e));
      });
  }, []);

  const selected = providers?.find((x) => x.id === selectedId) ?? null;
  const testModel = selected?.presets[0] ?? "";

  /** 尚无全局默认时，把所选服务的首个预设设为默认；已有就不覆盖。 */
  async function ensureDefault(p: ProviderInfo) {
    const s = await getModelSettings();
    if (!s.global && p.presets[0]) await setGlobalModel({ provider: p.id, model: p.presets[0] });
  }

  async function saveKey() {
    if (!selected) return;
    setKeyErr("");
    setDefaultErr("");
    setProbe(null);
    setSaving(true);
    try {
      await setApiKey(selected.id, key.trim());
      setKey(""); // 保存即清空，从不回显
    } catch (e) {
      // BYOK 错误分治：keychain 打开/写入失败等具体原因原样透出（P0 secrets.rs
      // 已把各失败场景拼成人话字符串），不吞、不改写成通用提示。
      setKeyErr(String(e));
      setSaving(false);
      return;
    }
    let report: ProbeReport;
    try {
      report = await testProvider(selected.id, testModel);
    } catch (e) {
      report = { ok: false, kind: "other", latency_ms: 0, provider: selected.id, model: testModel, message: String(e), detail: "" };
    }
    if (report.ok) {
      try {
        await ensureDefault(selected);
        setKey("");
        setStep("app");
      } catch (e) {
        setKey("");
        setDefaultErr(String(e));
      }
    } else {
      setProbe(report);
    }
    setSaving(false);
  }

  /** 测试没过但用户确认继续：密钥已存，照样设默认（失败不拦人，之后可在「模型与密钥」里改）。 */
  async function continueAnyway() {
    if (selected) await ensureDefault(selected).catch(() => {});
    setKey("");
    setProbe(null);
    setDefaultErr("");
    setStep("app");
  }

  async function installStarter(app: (typeof STARTER_APPS)[number]) {
    setInstallingId(app.id);
    setInstallErr(null);
    try {
      await invoke("install_builtin_sample", { name: app.name });
      setInstalledId(app.id);
    } catch (e) {
      setInstallErr(String(e));
    } finally {
      setInstallingId(null);
    }
  }

  return (
    <div className="onboarding-wizard">
      <div className={`onboarding-card${step === "provider" ? " onboarding-card-wide" : ""}`}>
        {step === "welcome" && (
          <>
            <div className="onboarding-orb" />
            <h1>欢迎使用 Super Agent OS</h1>
            <p>一个能自己创建应用的智能助手系统。只需两步即可开始：选一个模型服务并配置 Key，再选一个起步应用（可选）。</p>
            <div className="onboarding-actions">
              <button className="primary" onClick={() => setStep("provider")}>开始设置</button>
            </div>
          </>
        )}

        {step === "provider" && (
          <>
            <h2>选择模型服务</h2>
            <p>应用需要一个大模型来工作。选一个你已有密钥的服务，稍后随时可以改。</p>
            {providers === null ? (
              <p>加载中……</p>
            ) : (
              <OnboardingProviderStep providers={providers} selectedId={selectedId} onSelect={setSelectedId} />
            )}
            {providerErr && <p className="onboarding-error" role="alert">{providerErr}</p>}
            <div className="onboarding-actions">
              <button onClick={() => setStep("welcome")}>上一步</button>
              <button className="primary" disabled={!selected} onClick={() => setStep("key")}>下一步</button>
            </div>
          </>
        )}

        {step === "key" && selected && (
          <>
            <h2>配置 {selected.display} 的 API Key</h2>
            <p>你的 Key 只保存在系统钥匙串，永不写入磁盘文件。保存后会自动测试连通性（{TEST_COST_HINT}）。</p>
            <input
              type="password"
              autoComplete="off"
              aria-label="API 密钥"
              placeholder={`粘贴 ${selected.display} 的 API Key`}
              value={key}
              onChange={(e) => setKey(e.target.value)}
            />
            {keyErr && <p className="onboarding-error" role="alert">{keyErr}</p>}
            {defaultErr && (
              <>
                <p className="onboarding-error" role="alert">{defaultErr}</p>
                <p className="onboarding-note">密钥已保存、连通性测试通过，但没能设置默认模型。可以先继续，稍后在「模型与密钥」里设置。</p>
              </>
            )}
            {probe && (
              <>
                <ProbeResult probe={probe} />
                <p className="onboarding-note">密钥已保存。可能是网络或代理问题，你可以重新填写，或先继续、稍后在「模型与密钥」里再测。</p>
              </>
            )}
            <div className="onboarding-actions">
              {probe || defaultErr ? (
                <>
                  {probe && <button onClick={() => { setProbe(null); setKey(""); }}>重新填写</button>}
                  <button className="primary" onClick={continueAnyway}>仍然继续</button>
                </>
              ) : (
                <>
                  <button disabled={saving} onClick={() => { setKeyErr(""); setKey(""); setStep("provider"); }}>上一步</button>
                  <button className="primary" disabled={!key.trim() || saving} onClick={saveKey}>
                    {saving ? "保存并测试中…" : "保存并继续"}
                  </button>
                </>
              )}
            </div>
          </>
        )}

        {step === "app" && (
          <>
            <h2>装一个起步应用（可选）</h2>
            <p>「应用工坊」（Maker）已自动装好，随时可在主界面直接对话造应用。再选一个体验起来；也可以直接跳过，之后随时能在右栏「从文件安装应用」里再装。</p>
            <div className="onboarding-starter-list">
              {STARTER_APPS.map((app) => (
                <div className="onboarding-starter-card" key={app.id}>
                  <div className="onboarding-starter-title">{app.label}</div>
                  <div className="onboarding-starter-desc">{app.desc}</div>
                  <button
                    className="primary"
                    disabled={installingId === app.id || installedId === app.id}
                    onClick={() => installStarter(app)}
                  >
                    {installedId === app.id ? "已安装" : installingId === app.id ? "安装中…" : "安装"}
                  </button>
                </div>
              ))}
            </div>
            {installErr && <p className="onboarding-error">{installErr}</p>}
            <div className="onboarding-actions">
              <button onClick={onComplete}>{installedId ? "进入主界面" : "跳过，直接进入"}</button>
            </div>
          </>
        )}
      </div>
    </div>
  );
}
