import { useEffect, useState } from "react";
import "./ConnectorSettings.css";
import { listServers, putServer, deleteServer, type ServerConfig } from "../lib/mcp";

/** 把「空格分隔」的参数文本框内容解析成 `ServerConfig.args`（去掉空片段）。 */
function parseArgs(text: string): string[] {
  return text.split(/\s+/).map((s) => s.trim()).filter(Boolean);
}

/** 把「每行 KEY=VALUE」的环境变量文本框内容解析成 `ServerConfig.env`。 */
function parseEnv(text: string): Record<string, string> {
  const env: Record<string, string> = {};
  for (const line of text.split("\n")) {
    const trimmed = line.trim();
    if (!trimmed) continue;
    const idx = trimmed.indexOf("=");
    if (idx === -1) continue;
    env[trimmed.slice(0, idx).trim()] = trimmed.slice(idx + 1).trim();
  }
  return env;
}

const emptyForm = { id: "", category: "", command: "", args: "", env: "" };

/**
 * 连接器设置：列出已配置的 MCP server（`list_servers`）+ 新增表单
 * （id/category/command/args/env → `put_server`）+ 逐条删除（`delete_server`）。
 * 凭据（env 里的密钥）只落 keychain（`vault.rs`），本视图只是薄前端。
 * `transport` 当前后端固定 `"stdio"`（见 `vault.rs` 文档），表单不暴露该字段。
 * 挂载点同 P2 `AuditView`：`SessionPanel` 常驻入口 → `Shell` 中区状态分支，
 * 见 task-17-brief 与 Shell.tsx。
 */
export function ConnectorSettings({ onClose }: { onClose?: () => void } = {}) {
  const [servers, setServers] = useState<ServerConfig[] | null>(null);
  const [err, setErr] = useState<string | null>(null);
  const [form, setForm] = useState(emptyForm);
  const [formErr, setFormErr] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const refresh = () => listServers().then(setServers).catch((e) => setErr(String(e)));
  useEffect(() => { refresh(); }, []);

  async function submit() {
    setFormErr(null);
    if (!form.id.trim() || !form.command.trim()) {
      setFormErr("Server ID 和命令为必填项");
      return;
    }
    setBusy(true);
    try {
      await putServer({
        id: form.id.trim(),
        category: form.category.trim(),
        command: form.command.trim(),
        args: parseArgs(form.args),
        env: parseEnv(form.env),
        transport: "stdio",
      });
      setForm(emptyForm);
      refresh();
    } catch (e) {
      setFormErr(String(e));
    } finally {
      setBusy(false);
    }
  }

  async function remove(id: string) {
    try {
      await deleteServer(id);
      refresh();
    } catch (e) {
      setErr(String(e));
    }
  }

  return (
    <div className="connector-settings">
      <div className="connector-settings-header">
        <h2>连接器设置</h2>
        {onClose && <button onClick={onClose}>返回</button>}
      </div>

      {err && <p className="connector-err">{err}</p>}

      {servers === null && !err && <p className="connector-hint">加载中…</p>}

      {servers !== null && servers.length === 0 && (
        <p className="connector-hint">还没有配置任何连接器。添加一个 MCP server 后，符合权限的应用即可使用。</p>
      )}

      {servers !== null && servers.length > 0 && (
        <table className="connector-table">
          <thead>
            <tr>
              <th>ID</th>
              <th>类别</th>
              <th>命令</th>
              <th>参数</th>
              <th></th>
            </tr>
          </thead>
          <tbody>
            {servers.map((s) => (
              <tr key={s.id}>
                <td className="connector-cell-id">{s.id}</td>
                <td>{s.category}</td>
                <td>{s.command}</td>
                <td>{s.args.join(" ")}</td>
                <td><button onClick={() => remove(s.id)}>删除</button></td>
              </tr>
            ))}
          </tbody>
        </table>
      )}

      <div className="connector-form">
        <p className="connector-form-title">新增连接器</p>
        <div className="connector-form-row">
          <input
            placeholder="Server ID"
            value={form.id}
            onChange={(e) => setForm({ ...form, id: e.target.value })}
          />
          <input
            placeholder="类别（如 filesystem）"
            value={form.category}
            onChange={(e) => setForm({ ...form, category: e.target.value })}
          />
        </div>
        <div className="connector-form-row">
          <input
            placeholder="命令（如 npx）"
            value={form.command}
            onChange={(e) => setForm({ ...form, command: e.target.value })}
          />
        </div>
        <textarea
          placeholder="参数（空格分隔，如 -y @modelcontextprotocol/server-filesystem）"
          value={form.args}
          onChange={(e) => setForm({ ...form, args: e.target.value })}
        />
        <textarea
          placeholder="环境变量（每行 KEY=VALUE）"
          value={form.env}
          onChange={(e) => setForm({ ...form, env: e.target.value })}
        />
        {formErr && <p className="connector-err">{formErr}</p>}
        <div className="connector-form-actions">
          <button onClick={submit} disabled={busy}>添加连接器</button>
        </div>
      </div>
    </div>
  );
}
