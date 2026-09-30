#!/usr/bin/env bash
# 下载 pi 官方独立二进制（Bun 编译，不依赖 Node）到 src-tauri/binaries/pi/，供随包资源分发。
#
# 用法：scripts/fetch-pi.sh [version]
#   version 省略时读 src-tauri/pi-version.txt。
# 行为：
#   - 已存在且 `pi --version` 与目标版本一致 → 直接退出（幂等，可放进 beforeBuildCommand）。
#   - 下载 pi-<os>-<arch>.tar.gz，用仓库里固定的 SHA-256（src-tauri/pi-sha256.txt）校验，
#     校验不过即失败，绝不落盘半成品。没有登记过校验和的版本一律拒绝下载。
#   - 解包结果是整个目录（pi 可执行文件 + native/ + node_modules/ + wasm 等运行时资产），
#     独立二进制必须与这些兄弟目录同处一处，所以不能只拷 `pi` 一个文件。
# 环境变量：
#   PI_RELEASE_BASE  覆盖下载基址（默认 GitHub Releases），便于内网镜像。
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
VERSION="${1:-$(tr -d '[:space:]' < "${ROOT}/src-tauri/pi-version.txt")}"
DEST="${ROOT}/src-tauri/binaries/pi"
BASE="${PI_RELEASE_BASE:-https://github.com/earendil-works/pi/releases/download}/v${VERSION}"

os="$(uname -s)"
arch="$(uname -m)"
case "${os}" in
  Darwin) os=darwin ;;
  Linux) os=linux ;;
  *) echo "fetch-pi: 不支持的平台 ${os}（官方仅提供 darwin/linux 的 tar.gz 与 windows 的 zip）" >&2; exit 1 ;;
esac
case "${arch}" in
  arm64|aarch64) arch=arm64 ;;
  x86_64|amd64) arch=x64 ;;
  *) echo "fetch-pi: 不支持的架构 ${arch}" >&2; exit 1 ;;
esac
ASSET="pi-${os}-${arch}.tar.gz"

if [ -x "${DEST}/pi" ]; then
  have="$("${DEST}/pi" --version 2>/dev/null || true)"
  if [ "${have}" = "${VERSION}" ]; then
    echo "fetch-pi: ${DEST}/pi 已是 ${VERSION}，跳过"
    exit 0
  fi
  echo "fetch-pi: 现有版本 '${have:-未知}' ≠ 目标 ${VERSION}，重新下载"
fi

# 校验和取自仓库里的固定清单，而不是和压缩包同一个发布页上的 SHA256SUMS：
# 后者与压缩包同源，发布页被替换时两者会一起变，校验形同虚设。
PINS="${ROOT}/src-tauri/pi-sha256.txt"
expected="$(awk -v v="${VERSION}" -v a="${ASSET}" '$2 == v && $3 == a {print $1}' "${PINS}")"
if [ -z "${expected}" ]; then
  echo "fetch-pi: ${PINS} 里没有 ${VERSION} ${ASSET} 的校验和；升级 pi 版本时先把官方 SHA256SUMS 里对应的行登记进去" >&2
  exit 1
fi

tmp="$(mktemp -d)"
trap 'rm -rf "${tmp}"' EXIT

echo "fetch-pi: 下载 ${BASE}/${ASSET}"
curl -fsSL --retry 3 --retry-delay 2 -o "${tmp}/${ASSET}" "${BASE}/${ASSET}"

if command -v shasum >/dev/null 2>&1; then
  actual="$(shasum -a 256 "${tmp}/${ASSET}" | awk '{print $1}')"
else
  actual="$(sha256sum "${tmp}/${ASSET}" | awk '{print $1}')"
fi
if [ "${expected}" != "${actual}" ]; then
  echo "fetch-pi: 校验失败 expected=${expected} actual=${actual}" >&2
  exit 1
fi

tar -xzf "${tmp}/${ASSET}" -C "${tmp}"
if [ ! -x "${tmp}/pi/pi" ]; then
  echo "fetch-pi: 解包后找不到 pi/pi 可执行文件，发布包布局可能已变" >&2
  exit 1
fi

mkdir -p "$(dirname "${DEST}")"
rm -rf "${DEST}"
mv "${tmp}/pi" "${DEST}"
# .gitkeep 是入库的占位文件（让没拉二进制的检出也能通过 tauri-build 的资源校验）。
# 上面是整目录替换，会把它一起带走；放回去，否则跑完脚本后 git 会显示它被删除。
: > "${DEST}/.gitkeep"

got="$("${DEST}/pi" --version)"
if [ "${got}" != "${VERSION}" ]; then
  echo "fetch-pi: 解包后的 pi --version 输出 '${got}' ≠ ${VERSION}" >&2
  exit 1
fi
echo "fetch-pi: 就位 ${DEST}/pi（${got}，$(du -sh "${DEST}" | cut -f1)）"
