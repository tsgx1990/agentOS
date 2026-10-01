import { test, expect } from "vitest";
import { formatBytes, formatDuration } from "./resources";

test("formatBytes 边界", () => {
  expect(formatBytes(0)).toBe("0 B");
  expect(formatBytes(1023)).toBe("1023 B");
  expect(formatBytes(1024)).toBe("1.0 KB");
  expect(formatBytes(1.5 * 1024 ** 3)).toBe("1.5 GB");
});

test("formatDuration 边界", () => {
  expect(formatDuration(0)).toBe("0 秒");
  expect(formatDuration(59)).toBe("59 秒");
  expect(formatDuration(60)).toBe("1 分钟");
  expect(formatDuration(3700)).toBe("1 小时 1 分钟");
  expect(formatDuration(3600)).toBe("1 小时");
});
