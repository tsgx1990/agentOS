import { render, screen, fireEvent, waitFor } from "@testing-library/react";
import { test, expect, vi, beforeEach } from "vitest";
const invokeMock = vi.fn();
vi.mock("@tauri-apps/api/core", () => ({ invoke: (...a: unknown[]) => invokeMock(...a) }));
import { ConnectorSettings } from "./ConnectorSettings";

beforeEach(() => invokeMock.mockReset());

test("渲染已配置的 MCP server 列表", async () => {
  invokeMock.mockImplementation((cmd: string) => {
    if (cmd === "list_servers") return Promise.resolve([
      { id: "fs", category: "filesystem", command: "npx", args: ["-y", "@modelcontextprotocol/server-filesystem"], env: {}, transport: "stdio" },
    ]);
    return Promise.resolve();
  });
  render(<ConnectorSettings />);
  await waitFor(() => expect(screen.getByText("fs")).toBeTruthy());
  expect(screen.getByText("filesystem")).toBeTruthy();
  expect(screen.getByText("npx")).toBeTruthy();
});

test("空列表显示空态", async () => {
  invokeMock.mockResolvedValue([]);
  render(<ConnectorSettings />);
  await waitFor(() => expect(screen.getByText(/还没有配置/)).toBeTruthy());
});

test("提交新增表单调用 put_server（正确 payload）", async () => {
  invokeMock.mockImplementation((cmd: string) => {
    if (cmd === "list_servers") return Promise.resolve([]);
    return Promise.resolve();
  });
  render(<ConnectorSettings />);
  await waitFor(() => expect(screen.getByText(/还没有配置/)).toBeTruthy());

  fireEvent.change(screen.getByPlaceholderText("Server ID"), { target: { value: "fs" } });
  fireEvent.change(screen.getByPlaceholderText(/类别/), { target: { value: "filesystem" } });
  fireEvent.change(screen.getByPlaceholderText(/命令/), { target: { value: "npx" } });
  fireEvent.change(screen.getByPlaceholderText(/参数/), { target: { value: "-y @modelcontextprotocol/server-filesystem" } });
  fireEvent.change(screen.getByPlaceholderText(/环境变量/), { target: { value: "API_KEY=abc123" } });
  fireEvent.click(screen.getByText("添加连接器"));

  await waitFor(() => expect(invokeMock).toHaveBeenCalledWith("put_server", {
    config: {
      id: "fs",
      category: "filesystem",
      command: "npx",
      args: ["-y", "@modelcontextprotocol/server-filesystem"],
      env: { API_KEY: "abc123" },
      transport: "stdio",
    },
  }));
});

test("点击删除调用 delete_server", async () => {
  invokeMock.mockImplementation((cmd: string) => {
    if (cmd === "list_servers") return Promise.resolve([
      { id: "fs", category: "filesystem", command: "npx", args: [], env: {}, transport: "stdio" },
    ]);
    return Promise.resolve();
  });
  render(<ConnectorSettings />);
  await waitFor(() => expect(screen.getByText("fs")).toBeTruthy());
  fireEvent.click(screen.getByText("删除"));
  await waitFor(() => expect(invokeMock).toHaveBeenCalledWith("delete_server", { id: "fs" }));
});
