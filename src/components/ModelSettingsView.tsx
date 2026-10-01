import { useState } from "react";
import { ProviderDetail } from "./ProviderDetail";
import {
  SOURCE_LABEL,
  type CustomPreset,
  type CustomProvider,
  type ModelChoice,
  type ModelSettingsView as SettingsData,
  type ProbeReport,
  type ProviderInfo,
} from "../lib/providers";
import type { ModelUsageRow } from "../lib/usage";
import "./ModelSettingsView.css";

/** 某应用的「其他（工具 / 压缩）」用量 = 应用总量 − 各模型之和。 */
export interface OtherUsageRow {
  app_id: string;
  input: number;
  output: number;
  cost: number;
}

export interface ModelSettingsViewProps {
  providers: ProviderInfo[];
  customPresets: CustomPreset[];
  settings: SettingsData;
  usageRows: ModelUsageRow[];
  otherUsage: OtherUsageRow[];
  /** key = provider id */
  probeResults: Record<string, ProbeReport>;
  /** 正在测试的 provider id 集合（各自独立，互不解锁） */
  testingIds?: string[];
  selectedId: string | null;
  error?: string | null;
  onClose?: () => void;
  onSelect: (id: string | null) => void;
  onSaveKey: (id: string, key: string) => Promise<boolean>;
  onClearKey: (id: string) => void;
  onTest: (id: string, model: string) => void;
  onSetGlobal: (choice: ModelChoice | null) => void;
  onSetAppOverride: (appId: string, choice: ModelChoice | null) => void;
  /** 新建；id 已存在时后端报错。返回 true 表示已保存（表单随即收起） */
  onCreateCustom: (provider: CustomProvider) => Promise<boolean>;
  /** 修改已有服务；改了 base_url 时表单已先让用户确认 */
  onSaveCustom: (provider: CustomProvider) => Promise<boolean>;
  onRemoveCustom: (id: string) => void;
}

interface Entry {
  id: string;
  display: string;
  configured: boolean;
  subtitle: string;
  models: string[];
  group: "cn" | "intl" | "custom";
}

const GROUPS: { key: Entry["group"]; label: string }[] = [
  { key: "cn", label: "国内" },
  { key: "intl", label: "国际" },
  { key: "custom", label: "自定义" },
];

function buildEntries(providers: ProviderInfo[]): Entry[] {
  const rank = (e: Entry) => GROUPS.findIndex((g) => g.key === e.group);
  return providers
    .map<Entry>((p) => ({
      id: p.id,
      display: p.display,
      configured: p.configured,
      subtitle: p.native
        ? `${p.region === "cn" ? "国内服务" : "国际服务"} · 内置 · ${p.id}`
        : `OpenAI 兼容 · ${p.base_url ?? ""}`,
      models: p.presets,
      group: p.native ? (p.region === "cn" ? "cn" : "intl") : "custom",
    }))
    .sort((a, b) => rank(a) - rank(b));
}

// provider id 里不会出现 "|"，按第一个 "|" 拆分即可（模型 id 可含任意字符）。
const encodeChoice = (c: ModelChoice | null) => (c ? `${c.provider}|${c.model}` : "");
function decodeChoice(v: string): ModelChoice | null {
  const i = v.indexOf("|");
  return v && i > 0 ? { provider: v.slice(0, i), model: v.slice(i + 1) } : null;
}

/** 按应用覆盖：选项显示成「provider / model」并按 provider 分组，不会只剩一个看不出出处的模型名。 */
function OverrideSelect(props: {
  entries: Entry[];
  value: ModelChoice | null;
  label: string;
  onChange: (c: ModelChoice | null) => void;
}) {
  const usable = props.entries.filter((e) => e.configured && e.models.length > 0);
  const current = encodeChoice(props.value);
  const listed = usable.some((e) => e.models.some((m) => encodeChoice({ provider: e.id, model: m }) === current));
  const nameOf = (id: string) => props.entries.find((e) => e.id === id)?.display ?? id;
  return (
    <select
      aria-label={props.label}
      value={current}
      onChange={(e) => props.onChange(decodeChoice(e.target.value))}
    >
      <option value="">跟随默认</option>
      {props.value && !listed && (
        <option value={current}>
          {nameOf(props.value.provider)} / {props.value.model}
        </option>
      )}
      {usable.map((e) => (
        <optgroup key={e.id} label={e.display}>
          {e.models.map((m) => (
            <option key={m} value={encodeChoice({ provider: e.id, model: m })}>
              {e.display} / {m}
            </option>
          ))}
        </optgroup>
      ))}
    </select>
  );
}

const MANUAL = "__manual__";

/** 全局默认模型：provider 下拉（只列已配置的）+ 该 provider 预设的模型下拉 + 手填。 */
function GlobalModelPicker(props: {
  entries: Entry[];
  value: ModelChoice | null;
  onChange: (c: ModelChoice | null) => void;
}) {
  const [manualOn, setManualOn] = useState(false);
  const [text, setText] = useState("");
  // 已选的 provider 不一定已有对应的全局默认（没有预设可选时要等用户手填），所以单独记一份。
  const [picked, setPicked] = useState<string | null>(null);
  const usable = props.entries.filter((e) => e.configured);
  const providerId = picked ?? props.value?.provider ?? "";
  const entry = props.entries.find((e) => e.id === providerId) ?? null;
  const presets = entry?.models ?? [];
  const current = props.value?.provider === providerId ? props.value.model : "";
  const manual = manualOn || (current !== "" && !presets.includes(current));
  const typed = manualOn ? text : current;

  const globalEntry = props.value ? props.entries.find((e) => e.id === props.value?.provider) : null;
  const unusable = props.value !== null && !(globalEntry?.configured ?? false);

  return (
    <div className="ms-row ms-wrap ms-picker">
      {unusable && props.value && (
        <p className="ms-warn" role="alert">
          当前默认模型所用的服务「{globalEntry?.display ?? props.value.provider}」未配置密钥，会话将无法调用模型。
        </p>
      )}
      <select
        aria-label="默认模型服务"
        value={providerId}
        onChange={(e) => {
          const next = props.entries.find((x) => x.id === e.target.value) ?? null;
          setText("");
          setPicked(e.target.value);
          if (!next) {
            setManualOn(false);
            props.onChange(null);
          } else if (next.models.length === 0) {
            setManualOn(true); // 没有预设可选，只能手填
          } else {
            setManualOn(false);
            props.onChange({ provider: next.id, model: next.models[0] });
          }
        }}
      >
        <option value="">不设置（沿用应用清单）</option>
        {entry && !usable.includes(entry) && <option value={entry.id}>{entry.display}</option>}
        {usable.map((e) => (
          <option key={e.id} value={e.id}>
            {e.display}
          </option>
        ))}
      </select>
      <select
        aria-label="默认模型名称"
        disabled={!entry}
        value={manual ? MANUAL : current}
        onChange={(e) => {
          if (e.target.value === MANUAL) {
            setText(current);
            setManualOn(true);
          } else {
            setManualOn(false);
            props.onChange({ provider: providerId, model: e.target.value });
          }
        }}
      >
        {presets.map((m) => (
          <option key={m} value={m}>
            {m}
          </option>
        ))}
        <option value={MANUAL}>手填…</option>
      </select>
      {entry && manual && (
        <>
          <input
            aria-label="手填模型 id"
            value={typed}
            placeholder="模型 id"
            onChange={(e) => {
              setManualOn(true);
              setText(e.target.value);
            }}
          />
          <button
            type="button"
            className="ms-btn"
            disabled={!typed.trim()}
            onClick={() => props.onChange({ provider: providerId, model: typed.trim() })}
          >
            应用
          </button>
        </>
      )}
    </div>
  );
}

interface CustomForm {
  id: string;
  display: string;
  baseUrl: string;
  models: string;
}

function CustomProviderForm(props: {
  initial: CustomForm;
  /** 修改已有服务：id 锁定；改了接口地址要先确认 */
  editing?: boolean;
  onSubmit: (p: CustomProvider) => Promise<boolean>;
  onCancel: () => void;
}) {
  const [f, setF] = useState(props.initial);
  const [confirming, setConfirming] = useState(false);
  const set = (k: keyof CustomForm) => (e: { target: { value: string } }) => {
    setF({ ...f, [k]: e.target.value });
    // 确认针对的是「当时那个地址」：地址再变就作废，必须对新地址重新确认。
    if (k === "baseUrl") setConfirming(false);
  };
  const slug = f.id.trim().toLowerCase().replace(/^custom-/, "");
  const ok = slug !== "" && f.display.trim() !== "" && f.baseUrl.trim() !== "";
  const urlChanged = !!props.editing && f.baseUrl.trim() !== props.initial.baseUrl.trim();
  const submit = () =>
    props.onSubmit({
      id: `custom-${slug}`,
      display: f.display.trim(),
      base_url: f.baseUrl.trim(),
      api: "openai-completions",
      models: f.models.split(/[,，\s]+/).filter(Boolean),
    });
  return (
    <form
      className="ms-detail ms-custom-form"
      onSubmit={(e) => {
        e.preventDefault();
        if (!ok) return;
        if (urlChanged && !confirming) {
          setConfirming(true);
          return;
        }
        setConfirming(false);
        void submit();
      }}
    >
      <h3>{props.editing ? "修改自定义服务" : "添加自定义服务"}</h3>
      <p className="ms-sub">任何 OpenAI 兼容接口。密钥添加后在右侧填写，同样只存系统钥匙串。</p>
      <label className="ms-field-label" htmlFor="ms-c-id">
        服务 ID
      </label>
      <div className="ms-row ms-idrow">
        <span className="ms-idprefix">custom-</span>
        <input
          id="ms-c-id"
          value={f.id.replace(/^custom-/, "")}
          placeholder="例如 my-llm"
          disabled={props.editing}
          onChange={set("id")}
        />
      </div>
      <label className="ms-field-label" htmlFor="ms-c-name">
        显示名
      </label>
      <div className="ms-row">
        <input id="ms-c-name" value={f.display} onChange={set("display")} />
      </div>
      <label className="ms-field-label" htmlFor="ms-c-url">
        接口地址（base URL）
      </label>
      <div className="ms-row">
        <input id="ms-c-url" value={f.baseUrl} placeholder="https://…/v1" onChange={set("baseUrl")} />
      </div>
      <label className="ms-field-label" htmlFor="ms-c-models">
        模型 id（逗号分隔）
      </label>
      <div className="ms-row">
        <input id="ms-c-models" value={f.models} placeholder="例如 qwen-plus, qwen-max" onChange={set("models")} />
      </div>
      {confirming && (
        <div className="ms-confirm" role="alertdialog" aria-label="确认修改接口地址">
          <p>改地址后密钥会发往新地址，确定？</p>
        </div>
      )}
      <div className="ms-row ms-form-actions">
        <button type="submit" className="ms-btn ms-btn-primary" disabled={!ok}>
          {confirming ? "确定修改" : props.editing ? "保存修改" : "添加服务"}
        </button>
        <button type="button" className="ms-btn" onClick={props.onCancel}>
          取消
        </button>
      </div>
    </form>
  );
}

const nf = new Intl.NumberFormat("zh-CN");
const money = (n: number) => `$${n.toFixed(4)}`;
const BLANK_FORM: CustomForm = { id: "", display: "", baseUrl: "", models: "" };

export function ModelSettingsView(p: ModelSettingsViewProps) {
  const entries = buildEntries(p.providers);
  const [form, setForm] = useState<(CustomForm & { editing?: boolean }) | null>(null);
  const [confirmDelete, setConfirmDelete] = useState<string | null>(null);
  const selected = form ? null : (entries.find((e) => e.id === p.selectedId) ?? null);
  const nameOf = (id: string | null) => entries.find((e) => e.id === id)?.display ?? id ?? "";

  // 用量按 provider 分组（保持首次出现的顺序）
  const usageGroups: { provider: string; rows: ModelUsageRow[] }[] = [];
  for (const r of p.usageRows) {
    let g = usageGroups.find((x) => x.provider === r.provider);
    if (!g) usageGroups.push((g = { provider: r.provider, rows: [] }));
    g.rows.push(r);
  }
  /** 正在使用某 provider 的全局默认 / 应用覆盖，用于清除密钥前的提示。 */
  const usedBy = (id: string): string[] => [
    ...(p.settings.global?.provider === id ? ["默认模型"] : []),
    ...p.settings.apps.filter((a) => a.app_override?.provider === id).map((a) => `应用 ${a.app_id}`),
  ];
  const hasUsage = usageGroups.length > 0 || p.otherUsage.length > 0;

  return (
    <div className="ms">
      <header className="ms-header">
        <div>
          <h2>模型与密钥</h2>
          <p>选择模型服务、保存密钥，并决定每个应用用哪个模型。</p>
        </div>
        {p.onClose && (
          <button type="button" className="ms-btn" onClick={p.onClose}>
            返回
          </button>
        )}
      </header>
      {p.error && (
        <p className="ms-error" role="alert">
          {p.error}
        </p>
      )}

      <section className="ms-section" aria-label="模型服务">
        <h3 className="ms-section-title">模型服务</h3>
        <div className="ms-master">
          <nav className="ms-list" aria-label="服务列表">
            {GROUPS.map((g) => {
              const items = entries.filter((e) => e.group === g.key);
              return (
                <div key={g.key} className="ms-group">
                  <div className="ms-group-title">
                    {g.label}
                    <span>{items.length}</span>
                  </div>
                  {items.length === 0 && <p className="ms-empty">还没有自定义服务</p>}
                  {items.map((e) => (
                    <button
                      key={e.id}
                      type="button"
                      className={`ms-item${e.id === selected?.id ? " is-active" : ""}`}
                      aria-pressed={e.id === selected?.id}
                      onClick={() => {
                        setForm(null);
                        p.onSelect(e.id);
                      }}
                    >
                      <span className="ms-item-name">{e.display}</span>
                      <span className={`chip ${e.configured ? "chip-ready" : "chip-idle"}`}>
                        {e.configured ? "已配置" : "未配置"}
                      </span>
                    </button>
                  ))}
                </div>
              );
            })}
            <div className="ms-addcustom">
              <span className="ms-field-label">添加自定义服务</span>
              <div className="ms-row ms-wrap">
                {p.customPresets.map((c) => (
                  <button
                    key={c.suggested_id}
                    type="button"
                    className="ms-btn"
                    onClick={() =>
                      setForm({
                        id: c.suggested_id.replace(/^custom-/, ""),
                        display: c.display,
                        baseUrl: c.base_url,
                        models: c.models.join(", "),
                      })
                    }
                  >
                    + {c.display}
                  </button>
                ))}
                <button type="button" className="ms-btn" onClick={() => setForm(BLANK_FORM)}>
                  + 其它（OpenAI 兼容）
                </button>
              </div>
            </div>
          </nav>
          <div className="ms-pane">
            {form ? (
              <CustomProviderForm
                key={form.id + form.baseUrl + (form.editing ? "e" : "")}
                initial={form}
                editing={form.editing}
                onSubmit={async (c) => {
                  const ok = await (form.editing ? p.onSaveCustom(c) : p.onCreateCustom(c));
                  if (ok) setForm(null);
                  return ok;
                }}
                onCancel={() => setForm(null)}
              />
            ) : selected ? (
              <>
                <ProviderDetail
                  key={selected.id}
                  id={selected.id}
                  display={selected.display}
                  configured={selected.configured}
                  subtitle={selected.subtitle}
                  models={selected.models}
                  probe={p.probeResults[selected.id] ?? null}
                  testing={(p.testingIds ?? []).includes(selected.id)}
                  usedBy={usedBy(selected.id)}
                  onSaveKey={p.onSaveKey}
                  onClearKey={p.onClearKey}
                  onTest={p.onTest}
                />
                {selected.group === "custom" && confirmDelete !== selected.id && (
                  <div className="ms-row ms-custom-actions">
                    <button
                      type="button"
                      className="ms-btn"
                      onClick={() => {
                        const info = p.providers.find((x) => x.id === selected.id);
                        setForm({
                          id: selected.id.replace(/^custom-/, ""),
                          display: selected.display,
                          baseUrl: info?.base_url ?? "",
                          models: selected.models.join(", "),
                          editing: true,
                        });
                      }}
                    >
                      修改此服务
                    </button>
                    <button type="button" className="ms-btn ms-btn-danger" onClick={() => setConfirmDelete(selected.id)}>
                      删除此自定义服务
                    </button>
                  </div>
                )}
                {selected.group === "custom" && confirmDelete === selected.id && (
                  <div className="ms-confirm" role="alertdialog" aria-label="确认删除服务">
                    <p>删除后，保存在系统钥匙串里的密钥也会一并删除，无法恢复。确定删除「{selected.display}」？</p>
                    <div className="ms-row">
                      <button
                        type="button"
                        className="ms-btn ms-btn-danger-solid"
                        onClick={() => {
                          setConfirmDelete(null);
                          p.onRemoveCustom(selected.id);
                        }}
                      >
                        确认删除
                      </button>
                      <button type="button" className="ms-btn" onClick={() => setConfirmDelete(null)}>
                        取消
                      </button>
                    </div>
                  </div>
                )}
              </>
            ) : (
              <div className="ms-placeholder">
                <b>从左侧选一个服务</b>
                <span>已配置的服务才能被选为默认模型。</span>
              </div>
            )}
          </div>
        </div>
      </section>

      <section className="ms-section" aria-label="默认模型">
        <h3 className="ms-section-title">默认模型</h3>
        <div className="ms-panel ms-row">
          <span className="ms-panel-text">未单独指定模型的应用都用它</span>
          <GlobalModelPicker entries={entries} value={p.settings.global} onChange={p.onSetGlobal} />
        </div>
      </section>

      <section className="ms-section" aria-label="按应用">
        <h3 className="ms-section-title">按应用</h3>
        <p className="ms-hint ms-hint-top">已打开的应用下次打开时生效。</p>
        <div className="ms-panel ms-table-wrap">
          {p.settings.apps.length === 0 ? (
            <p className="ms-empty">还没有安装应用。</p>
          ) : (
            <table className="ms-table">
              <thead>
                <tr>
                  <th>应用</th>
                  <th>清单默认</th>
                  <th>覆盖为</th>
                  <th>实际使用</th>
                </tr>
              </thead>
              <tbody>
                {p.settings.apps.map((a) => (
                  <tr key={a.app_id}>
                    <td className="ms-strong">{a.app_id}</td>
                    <td>{a.manifest_model ? <code>{a.manifest_model}</code> : <span className="ms-muted">无</span>}</td>
                    <td>
                      <OverrideSelect
                        entries={entries}
                        value={a.app_override}
                        label={`${a.app_id} 的模型覆盖`}
                        onChange={(c) => p.onSetAppOverride(a.app_id, c)}
                      />
                    </td>
                    <td>
                      {a.effective.source === "none" ? (
                        <span className="ms-muted">未设置</span>
                      ) : (
                        <>
                          <span className="ms-eff">
                            <span className="ms-eff-provider">{nameOf(a.effective.provider)}</span>
                            <code>{a.effective.model}</code>
                          </span>{" "}
                          <span className={`ms-src ms-src-${a.effective.source}`}>{SOURCE_LABEL[a.effective.source]}</span>
                        </>
                      )}
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          )}
        </div>
      </section>

      <section className="ms-section" aria-label="用量">
        <h3 className="ms-section-title">用量（本次运行）</h3>
        <div className="ms-panel ms-table-wrap">
          {!hasUsage ? (
            <p className="ms-empty">还没有用量记录。应用运行后会在这里按模型汇总。</p>
          ) : (
            <table className="ms-table">
              <thead>
                <tr>
                  <th>应用</th>
                  <th>模型</th>
                  <th className="ms-num">输入 token</th>
                  <th className="ms-num">输出 token</th>
                  <th className="ms-num">费用</th>
                </tr>
              </thead>
              {usageGroups.map((g) => (
                <tbody key={g.provider}>
                  <tr className="ms-grouprow">
                    <th colSpan={5}>{nameOf(g.provider)}</th>
                  </tr>
                  {g.rows.map((u) => (
                    <tr key={`${u.app_id}/${u.model}`}>
                      <td className="ms-strong">{u.app_id}</td>
                      <td>
                        <code>{u.model}</code>
                      </td>
                      <td className="ms-num">{nf.format(u.input)}</td>
                      <td className="ms-num">{nf.format(u.output)}</td>
                      <td className="ms-num">{money(u.cost)}</td>
                    </tr>
                  ))}
                </tbody>
              ))}
              {p.otherUsage.length > 0 && (
                <tbody>
                  <tr className="ms-grouprow">
                    <th colSpan={5}>其他（工具 / 压缩）</th>
                  </tr>
                  {p.otherUsage.map((u) => (
                    <tr key={u.app_id}>
                      <td className="ms-strong">{u.app_id}</td>
                      <td>
                        <span className="ms-muted">非对话请求</span>
                      </td>
                      <td className="ms-num">{nf.format(u.input)}</td>
                      <td className="ms-num">{nf.format(u.output)}</td>
                      <td className="ms-num">{money(u.cost)}</td>
                    </tr>
                  ))}
                </tbody>
              )}
            </table>
          )}
        </div>
      </section>
    </div>
  );
}
