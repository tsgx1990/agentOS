import { useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import "./OnboardingWizard.css";
import { setApiKey } from "../lib/keys";

type Step = "welcome" | "key" | "app";

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
 * 首次启动 key-first 引导向导：欢迎 → 配置 BYOK key（存系统钥匙串，P0
 * `set_api_key`，复用 `KeySetup` 同款 try/catch 错误展示）→ 可选装一个起步
 * 应用（`install_builtin_sample`，白名单 + resource_dir 解析——不是
 * `InstallDialog` 手动装第三方包走的 `install_app(sourcePath, trusted)` 管道）
 * → 完成回调（交回 `Shell` 渲主工作台）。OAuth 未做，本向导只有 BYOK 一条
 * 路径。全程只用 C4a token。
 */
export function OnboardingWizard({ onComplete }: { onComplete: () => void }) {
  const [step, setStep] = useState<Step>("welcome");

  const [key, setKey] = useState("");
  const [keyErr, setKeyErr] = useState("");
  const [saving, setSaving] = useState(false);

  const [installingId, setInstallingId] = useState<string | null>(null);
  const [installedId, setInstalledId] = useState<string | null>(null);
  const [installErr, setInstallErr] = useState<string | null>(null);

  async function saveKey() {
    setKeyErr("");
    setSaving(true);
    try {
      await setApiKey("anthropic", key);
      setStep("app");
    } catch (e) {
      // BYOK 错误分治：keychain 打开/写入失败等具体原因原样透出（P0 secrets.rs
      // 已把各失败场景拼成人话字符串），不吞、不改写成通用提示。
      setKeyErr(String(e));
    } finally {
      setSaving(false);
    }
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
      <div className="onboarding-card">
        {step === "welcome" && (
          <>
            <div className="onboarding-orb" />
            <h1>欢迎使用 Super Agent OS</h1>
            <p>一个能自己创建应用的智能助手系统。只需两步即可开始：配置模型 Key，再选一个起步应用（可选）。</p>
            <div className="onboarding-actions">
              <button className="primary" onClick={() => setStep("key")}>开始设置</button>
            </div>
          </>
        )}

        {step === "key" && (
          <>
            <h2>配置模型 API Key</h2>
            <p>你的 Key 只保存在系统钥匙串，永不写入磁盘文件。当前主助手运行于 Claude，需要 Anthropic API Key。</p>
            <input
              type="password"
              placeholder="粘贴 Anthropic API Key"
              value={key}
              onChange={(e) => setKey(e.target.value)}
            />
            {keyErr && <p className="onboarding-error">{keyErr}</p>}
            <div className="onboarding-actions">
              <button className="primary" disabled={!key || saving} onClick={saveKey}>
                {saving ? "保存中…" : "保存并继续"}
              </button>
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
