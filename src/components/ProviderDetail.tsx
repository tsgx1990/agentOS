import { useState } from "react";
import { RATE_LIMIT_HINT, TEST_COST_HINT, type ProbeReport } from "../lib/providers";

export interface ProviderDetailProps {
  id: string;
  display: string;
  configured: boolean;
  /** 区域说明或自定义 provider 的 base_url，仅作副标题展示 */
  subtitle: string;
  models: string[];
  probe: ProbeReport | null;
  testing?: boolean;
  /** 返回 true 表示已保存；组件据此清空密钥输入框（失败时保留，方便改了重试） */
  onSaveKey: (id: string, key: string) => Promise<boolean>;
  onClearKey: (id: string) => void;
  /** 正在使用该服务的默认模型 / 应用覆盖（如「默认模型」「应用 x」）；非空时清除密钥前要二次确认 */
  usedBy?: string[];
  onTest: (id: string, model: string) => void;
}

/** 单个 provider 的详情：密钥、连通性测试、预设模型。密钥草稿只活在这里的本地 state，从不回显。 */
export function ProviderDetail(p: ProviderDetailProps) {
  const [draft, setDraft] = useState("");
  const [model, setModel] = useState("");
  const activeModel = p.models.includes(model) ? model : (p.models[0] ?? "");
  const inputId = `ms-key-${p.id}`;
  const [confirmClear, setConfirmClear] = useState(false);
  const usedBy = p.usedBy ?? [];

  async function save() {
    if (await p.onSaveKey(p.id, draft.trim())) setDraft("");
  }

  return (
    <div className="ms-detail">
      <div className="ms-detail-head">
        <div>
          <h3>{p.display}</h3>
          <p className="ms-sub">{p.subtitle}</p>
        </div>
        <span className={`chip ${p.configured ? "chip-ready" : "chip-idle"}`}>
          {p.configured ? "已配置" : "未配置"}
        </span>
      </div>

      <label className="ms-field-label" htmlFor={inputId}>
        API 密钥
      </label>
      <div className="ms-row">
        <input
          id={inputId}
          type="password"
          autoComplete="off"
          value={draft}
          placeholder={p.configured ? "已保存在系统钥匙串，输入新值可替换" : "粘贴 API 密钥"}
          onChange={(e) => setDraft(e.target.value)}
        />
        <button
          type="button"
          className="ms-btn ms-btn-primary"
          disabled={draft.trim().length === 0}
          onClick={save}
        >
          保存
        </button>
        {p.configured && (
          <button
            type="button"
            className="ms-btn"
            onClick={() => (usedBy.length > 0 ? setConfirmClear(true) : p.onClearKey(p.id))}
          >
            清除
          </button>
        )}
      </div>
      {confirmClear && usedBy.length > 0 && (
        <div className="ms-confirm" role="alertdialog" aria-label="确认清除密钥">
          <p>
            {usedBy.join("、")}正在用它，清除后这些会话将无法调用模型。确定清除密钥？
          </p>
          <div className="ms-row">
            <button
              type="button"
              className="ms-btn ms-btn-danger-solid"
              onClick={() => {
                setConfirmClear(false);
                p.onClearKey(p.id);
              }}
            >
              确认清除
            </button>
            <button type="button" className="ms-btn" onClick={() => setConfirmClear(false)}>
              取消
            </button>
          </div>
        </div>
      )}
      <p className="ms-hint">密钥只存系统钥匙串，不写入任何配置文件。</p>

      <label className="ms-field-label" htmlFor={`ms-model-${p.id}`}>
        连通性测试
      </label>
      <div className="ms-row">
        <select
          id={`ms-model-${p.id}`}
          value={activeModel}
          onChange={(e) => setModel(e.target.value)}
          disabled={p.models.length === 0}
        >
          {p.models.length === 0 && <option value="">（尚无模型）</option>}
          {p.models.map((m) => (
            <option key={m} value={m}>
              {m}
            </option>
          ))}
        </select>
        <button
          type="button"
          className="ms-btn"
          disabled={!p.configured || !activeModel || p.testing}
          onClick={() => p.onTest(p.id, activeModel)}
        >
          {p.testing ? "测试中…" : "测试连通性"}
        </button>
      </div>
      <p className="ms-hint">{TEST_COST_HINT}</p>
      {!p.configured && <p className="ms-hint">先保存密钥才能测试。</p>}
      {p.probe && <ProbeResult probe={p.probe} />}

      {p.models.length > 0 && (
        <>
          <div className="ms-field-label">预设模型</div>
          <ul className="ms-models">
            {p.models.map((m, i) => (
              <li key={m}>
                <code>{m}</code>
                {i === 0 && <span className="ms-tag">默认</span>}
              </li>
            ))}
          </ul>
        </>
      )}
    </div>
  );
}

/** 连通性测试结果：成功显示延迟；失败显示 message，`detail`（已脱敏）折叠在「详情」里。 */
export function ProbeResult({ probe }: { probe: ProbeReport }) {
  if (probe.ok) {
    return (
      <p className="ms-probe ms-probe-ok" role="status">
        <b>连通</b>
        <span>{probe.latency_ms} ms</span>
        <span className="ms-probe-model">{probe.model}</span>
      </p>
    );
  }
  return (
    <div className="ms-probe ms-probe-err" role="alert">
      <p>
        <b>失败</b>
        <span>{probe.message}</span>
      </p>
      {probe.kind === "rate_limited" && <p className="ms-probe-detail">{RATE_LIMIT_HINT}</p>}
      {probe.detail && (
        <details className="ms-probe-more">
          <summary>详情</summary>
          <p className="ms-probe-detail">{probe.detail}</p>
        </details>
      )}
    </div>
  );
}
