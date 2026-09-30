import { render, screen } from "@testing-library/react";
import { test, expect, vi } from "vitest";
vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn().mockResolvedValue([]) }));
import { NavRail } from "./NavRail";
import type { InstalledApp } from "../lib/registry";

const app = (id: string, cat: string): InstalledApp => ({
  app_id: id, name: id, version: "1.0.0", display_name: id,
  category: cat, icon: null, trusted: true, domains: [],
});

test("类目计数反映已装应用", () => {
  render(<NavRail apps={[app("a", "life"), app("b", "life"), app("c", "info")]} />);
  expect(screen.getByText("生活").parentElement?.textContent).toContain("2");
  expect(screen.getByText("信息").parentElement?.textContent).toContain("1");
});
