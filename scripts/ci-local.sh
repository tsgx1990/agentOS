#!/usr/bin/env bash
# 本地与 GitHub Actions 共用的门禁脚本：CI 工作流只做「装工具链 + 调本脚本」，
# 所以任何一台开发机上跑通本脚本 ≈ CI 会绿（差别只剩 runner 上的系统状态）。
#
# 用法：scripts/ci-local.sh [--skip-install] [--only <step>]
#   --skip-install  跳过两次 npm ci（本地反复跑时省时间；CI 不要传）。
#   --only <step>   只跑一个步骤：install|fmt|clippy|cargo-test|tsc|vitest|build
# 任一步失败即非零退出，不继续后面的步骤（fail-fast，和 CI 一致）。
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SKIP_INSTALL=0
ONLY=""
while [ $# -gt 0 ]; do
  case "$1" in
    --skip-install) SKIP_INSTALL=1 ;;
    --only) shift; ONLY="${1:-}" ;;
    *) echo "ci-local: 未知参数 $1" >&2; exit 2 ;;
  esac
  shift
done

step() {
  local name="$1"; shift
  if [ -n "${ONLY}" ] && [ "${ONLY}" != "${name}" ]; then
    return 0
  fi
  echo
  echo "==> [${name}] $*"
  local t0
  t0="$(date +%s)"
  "$@"
  echo "==> [${name}] ok（$(( $(date +%s) - t0 ))s）"
}

if [ "${SKIP_INSTALL}" -eq 0 ]; then
  step install bash -c "cd '${ROOT}' && npm ci --no-audit --no-fund && cd '${ROOT}/src-tauri/hosttools' && npm ci --no-audit --no-fund"
fi
text_check() {
  # 受控文本文件里不得出现 NUL 字节：曾有实现者把 NUL 当复合键分隔符写进 .tsx，
  # 文件被 grep/file 判为二进制，diff 与搜索全部失效。按扩展名扫描，零依赖。
  local bad=0 f n
  while IFS= read -r f; do
    [ -f "${f}" ] || continue
    n="$(LC_ALL=C tr -cd '\000' < "${f}" | wc -c | tr -d ' ')"
    if [ "${n}" != "0" ]; then
      echo "text-check: ${f} 含 ${n} 个 NUL 字节" >&2
      bad=1
    fi
  done < <(cd "${ROOT}" && git ls-files | grep -E '\.(rs|ts|tsx|css|json|md|sh|yml|yaml|toml|html)$')
  return "${bad}"
}
step text-check text_check
step fmt        bash -c "cd '${ROOT}/src-tauri' && cargo fmt --check"
step clippy     bash -c "cd '${ROOT}/src-tauri' && cargo clippy --all-targets -- -D warnings"
step cargo-test bash -c "cd '${ROOT}/src-tauri' && cargo test"
step tsc        bash -c "cd '${ROOT}' && npx tsc -b"
step vitest     bash -c "cd '${ROOT}' && npx vitest run"
step build      bash -c "cd '${ROOT}' && npm run build"

echo
echo "ci-local: 全部门禁通过"
