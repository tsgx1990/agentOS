import { useCallback, useEffect, useState } from "react";
import { ModelSettingsView, type OtherUsageRow } from "./ModelSettingsView";
import { clearApiKey, setApiKey } from "../lib/keys";
import {
  customProviderPresets,
  getModelSettings,
  listProviders,
  removeCustomProvider,
  saveCustomProvider,
  setAppModel,
  setGlobalModel,
  testProvider,
  type CustomPreset,
  type CustomProvider,
  type ModelChoice,
  type ModelSettingsView as SettingsData,
  type ProbeReport,
  type ProviderInfo,
} from "../lib/providers";
import { appUsage, usageByModel, type ModelUsageRow } from "../lib/usage";

const EMPTY_SETTINGS: SettingsData = { global: null, apps: [] };

/**
 * 「模型与密钥」设置页容器：拉数据、调命令，渲染 `ModelSettingsView`。
 * 密钥只经 `set_api_key` 进系统钥匙串，这里不保存也不回显；每次写入后重新拉取，
 * 页面始终显示后端的真值。用量是「本次运行」的累计（内存里，重启清零）。
 * 挂载点同 `ConnectorSettings`：`SessionPanel` 常驻入口 → `Shell` 中区状态分支。
 */
export function ModelSettings({ onClose }: { onClose?: () => void } = {}) {
  const [providers, setProviders] = useState<ProviderInfo[]>([]);
  const [presets, setPresets] = useState<CustomPreset[]>([]);
  const [settings, setSettings] = useState<SettingsData>(EMPTY_SETTINGS);
  const [usageRows, setUsageRows] = useState<ModelUsageRow[]>([]);
  const [otherUsage, setOtherUsage] = useState<OtherUsageRow[]>([]);
  const [probes, setProbes] = useState<Record<string, ProbeReport>>({});
  const [testingId, setTestingId] = useState<string | null>(null);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  const refreshProviders = useCallback(async () => setProviders(await listProviders()), []);
  const refreshSettings = useCallback(async () => setSettings(await getModelSettings()), []);
  const refreshUsage = useCallback(async (appIds: string[]) => {
    const rows = await usageByModel();
    setUsageRows(rows);
    // 「其他（工具 / 压缩）」= 应用总量 − 各模型之和（总量来自 app_usage）。
    const ids = [...new Set([...rows.map((r) => r.app_id), ...appIds, "main"])];
    const totals = await Promise.all(ids.map((id) => appUsage(id)));
    const others: OtherUsageRow[] = [];
    ids.forEach((id, i) => {
      const total = totals[i];
      const mine = rows.filter((r) => r.app_id === id);
      const input = Math.max(0, total.input - mine.reduce((s, r) => s + r.input, 0));
      const output = Math.max(0, total.output - mine.reduce((s, r) => s + r.output, 0));
      const cost = Math.max(0, total.cost - mine.reduce((s, r) => s + r.cost, 0));
      if (input > 0 || output > 0 || cost > 1e-9) others.push({ app_id: id, input, output, cost });
    });
    setOtherUsage(others);
  }, []);

  useEffect(() => {
    (async () => {
      try {
        const [, s, pr] = await Promise.all([refreshProviders(), getModelSettings(), customProviderPresets()]);
        setSettings(s);
        setPresets(pr);
        await refreshUsage(s.apps.map((a) => a.app_id));
      } catch (e) {
        setError(String(e));
      }
    })();
  }, [refreshProviders, refreshUsage]);

  /** 跑一个写操作；失败把后端文案原样展示，返回是否成功。 */
  async function run(op: () => Promise<void>): Promise<boolean> {
    setError(null);
    try {
      await op();
      return true;
    } catch (e) {
      setError(String(e));
      return false;
    }
  }

  const onSaveKey = (id: string, key: string) =>
    run(async () => {
      await setApiKey(id, key);
      setProbes(({ [id]: _drop, ...rest }) => rest); // 换了密钥，上次的测试结果作废
      await Promise.all([refreshProviders(), refreshSettings()]);
    });

  const onClearKey = (id: string) =>
    void run(async () => {
      await clearApiKey(id);
      setProbes(({ [id]: _drop, ...rest }) => rest);
      await Promise.all([refreshProviders(), refreshSettings()]);
    });

  async function onTest(id: string, model: string) {
    setTestingId(id);
    try {
      const report = await testProvider(id, model);
      setProbes((m) => ({ ...m, [id]: report }));
    } catch (e) {
      setProbes((m) => ({
        ...m,
        [id]: { ok: false, kind: "other", latency_ms: 0, provider: id, model, message: String(e), detail: "" },
      }));
    } finally {
      setTestingId(null);
    }
  }

  const onSetGlobal = (choice: ModelChoice | null) =>
    void run(async () => {
      await setGlobalModel(choice);
      await refreshSettings();
    });

  const onSetAppOverride = (appId: string, choice: ModelChoice | null) =>
    void run(async () => {
      await setAppModel(appId, choice);
      await refreshSettings();
    });

  const onSaveCustom = (provider: CustomProvider) =>
    run(async () => {
      await saveCustomProvider(provider);
      await refreshProviders();
      setSelectedId(provider.id);
    });

  const onRemoveCustom = (id: string) =>
    void run(async () => {
      await removeCustomProvider(id);
      setSelectedId(null);
      setProbes(({ [id]: _drop, ...rest }) => rest);
      await Promise.all([refreshProviders(), refreshSettings()]);
    });

  return (
    <ModelSettingsView
      providers={providers}
      customPresets={presets}
      settings={settings}
      usageRows={usageRows}
      otherUsage={otherUsage}
      probeResults={probes}
      testingId={testingId}
      selectedId={selectedId}
      error={error}
      onClose={onClose}
      onSelect={setSelectedId}
      onSaveKey={onSaveKey}
      onClearKey={onClearKey}
      onTest={onTest}
      onSetGlobal={onSetGlobal}
      onSetAppOverride={onSetAppOverride}
      onSaveCustom={onSaveCustom}
      onRemoveCustom={onRemoveCustom}
    />
  );
}
