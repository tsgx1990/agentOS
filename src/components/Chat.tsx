import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { EVENTS, type AgentError, type RetryStatus } from "../lib/events";
import "./Chat.css";

type Msg = { role: "user" | "assistant"; text: string };

/**
 * 中区主助手聊天：流式增量拼接（assistant-delta）、完成（assistant-done）、
 * 鉴权/其它错误（agent-error）与重试提示（retry-status）。
 * 逻辑与 .superpowers/sdd/task-9-brief.md 步骤 5 一致；样式改走 tokens.css 变量
 * （不再用内联硬编码颜色），以贴合 C4a 设计规范。
 */
export function Chat() {
  const [msgs, setMsgs] = useState<Msg[]>([]);
  const [input, setInput] = useState("");
  const [status, setStatus] = useState("");
  const [errorKind, setErrorKind] = useState("");
  const streaming = useRef(false);

  useEffect(() => {
    const unsubs = [
      listen<string>(EVENTS.delta, (e) => {
        setMsgs((prev) => {
          const next = [...prev];
          if (!streaming.current) {
            next.push({ role: "assistant", text: "" });
            streaming.current = true;
          }
          next[next.length - 1] = {
            role: "assistant",
            text: next[next.length - 1].text + e.payload,
          };
          return next;
        });
      }),
      listen(EVENTS.done, () => { streaming.current = false; setStatus(""); }),
      listen<AgentError>(EVENTS.error, (e) => {
        streaming.current = false;
        setErrorKind(e.payload.kind);
        setStatus(e.payload.message);
      }),
      listen<RetryStatus>(EVENTS.retry, (e) => {
        setStatus(`重试中（第 ${e.payload.attempt}/${e.payload.max} 次）……`);
      }),
    ];
    return () => { unsubs.forEach((u) => u.then((f) => f())); };
  }, []);

  async function send() {
    const text = input.trim();
    if (!text) return;
    setMsgs((prev) => [...prev, { role: "user", text }]);
    setInput("");
    setErrorKind("");
    setStatus("");
    try {
      await invoke("send_prompt", { text });
    } catch {
      setErrorKind("send-error");
      setStatus("发送失败，请稍后重试");
    }
  }

  return (
    <div className="chat">
      <div className="chat-log">
        {msgs.map((m, i) => (
          <div key={i} className={`chat-row ${m.role}`}>
            <span className={`chat-bubble ${m.role}`}>{m.text}</span>
          </div>
        ))}
      </div>
      {status && (
        <div className={`chat-status ${errorKind === "auth" ? "auth" : ""}`}>
          {status}
        </div>
      )}
      <div className="chat-composer">
        <input
          className="chat-input"
          placeholder="和主助手说点什么……"
          value={input}
          onChange={(e) => setInput(e.target.value)}
          onKeyDown={(e) => e.key === "Enter" && send()}
        />
        <button className="chat-send" onClick={send}>发送</button>
      </div>
    </div>
  );
}
