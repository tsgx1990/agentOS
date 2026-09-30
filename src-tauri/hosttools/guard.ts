// 宿主权限网关的纯函数校验器：写/读/命令三类越权判断。
// 纯函数、无副作用，可被 vitest 独立测试。P1 只做路径规范化包含判断，不做 realpath/symlink 解析（L2 留给 P2）。
import * as path from "node:path";

function contains(base: string, target: string): boolean {
  const b = path.resolve(base);
  const t = path.resolve(target);
  return t === b || t.startsWith(b + path.sep);
}

// 写操作只允许落在应用自己的数据目录（appDataDir）内。
export function writeAllowed(target: string, appDataDir: string): boolean {
  return contains(appDataDir, target);
}

// 读操作允许应用数据目录，或用户显式声明的只读区（readPaths）。
export function readAllowed(target: string, appDataDir: string, readPaths: string[]): boolean {
  return contains(appDataDir, target) || readPaths.some((r) => contains(r, target));
}

// P1 最小危险命令名单：明显破坏性/自我复制型命令直接拒绝。
const DANGER = [/\brm\s+-rf\b/, /\bmkfs\b/, /\bdd\s+if=/, />\s*\/dev\/sd/, /\bshutdown\b/, /:\(\)\s*\{/];
export function commandAllowed(cmd: string): boolean {
  return !DANGER.some((re) => re.test(cmd));
}
