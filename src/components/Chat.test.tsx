import { render, screen, fireEvent, waitFor } from "@testing-library/react";
import { vi, test, expect, beforeEach } from "vitest";

const listeners: Record<string, (e: { payload: unknown }) => void> = {};
vi.mock("@tauri-apps/api/event", () => ({
  listen: (name: string, cb: (e: { payload: unknown }) => void) => {
    listeners[name] = cb;
    return Promise.resolve(() => {});
  },
}));
const invokeMock = vi.fn().mockResolvedValue(undefined);
vi.mock("@tauri-apps/api/core", () => ({ invoke: (...a: unknown[]) => invokeMock(...a) }));

import { Chat } from "./Chat";
import { EVENTS } from "../lib/events";

beforeEach(() => { invokeMock.mockClear(); });

test("发送后流式拼接助手回复", async () => {
  render(<Chat />);
  fireEvent.change(screen.getByPlaceholderText("和主助手说点什么……"), { target: { value: "你好" } });
  fireEvent.click(screen.getByText("发送"));
  await waitFor(() => expect(invokeMock).toHaveBeenCalledWith("send_prompt", { text: "你好" }));

  listeners[EVENTS.delta]({ payload: "你" });
  listeners[EVENTS.delta]({ payload: "好呀" });
  listeners[EVENTS.done]({ payload: null });
  await waitFor(() => expect(screen.getByText("你好呀")).toBeTruthy());
});

test("鉴权错误提示重新配置", async () => {
  render(<Chat />);
  listeners[EVENTS.error]({ payload: { kind: "auth", message: "API Key 无效或已过期，请重新配置" } });
  await waitFor(() => expect(screen.getByText(/重新配置/)).toBeTruthy());
});
