# pi 外部二进制（P6-E）

`binaries/pi/` 放官方 [pi](https://github.com/earendil-works/pi) 独立二进制的**解包目录**（Bun 编译，不依赖 Node）：`pi` 可执行文件本身 + `native/` + `node_modules/` + `photon_rs_bg.wasm` + `package.json` 等运行时资产同处一处，不能只拷 `pi` 单个文件。整个 `binaries/pi/` 目录已 `.gitignore`，不入库。

## 怎么拿到它

```bash
./scripts/fetch-pi.sh          # 版本取 ../pi-version.txt（当前 0.84.4）
./scripts/fetch-pi.sh 0.84.4   # 或显式指定版本
```

脚本按本机 `uname -s`/`uname -m` 选官方 Release 里的 `pi-<os>-<arch>.tar.gz`，用仓库里固定的 SHA-256（`../pi-sha256.txt`，没登记过的版本会被拒绝）校验后解包到这里，已是目标版本则跳过（幂等，可放进构建流水线）。`npm run tauri build` 的 `beforeBuildCommand`（见 `../tauri.conf.json`）已经接了这一步，正常打包不用手动跑。

## 怎么随包分发

`../tauri.conf.json` 的 `bundle.resources` 把 `binaries/pi/` 映射到 `$RESOURCE/pi`：

```json
"resources": { "binaries/pi/": "pi" }
```

生产打包后，独立二进制的真实落地处是 `$RESOURCE/pi/pi`（`$RESOURCE` 即 `tauri::path::PathResolver::resource_dir()`）。应用启动时 `lib.rs::setup()` 会把这个路径登记进 `pi_bin::register_bundled_pi`。

## 解析顺序（`pi_bin::resolve_pi_bin()`，四级）

1. `SUPERAGENT_PI_BIN` 环境变量（测试/手动指定覆盖一切）。
2. 随包资源里登记的路径（`$RESOURCE/pi/pi`，见上一节；生产打包内命中，`cargo build`/`cargo test` 未真正打包时 `resource_dir()` 会失败，跳过这一级）。
3. 开发检出里的 `src-tauri/binaries/pi/pi`（相对仓库根，跑过 `fetch-pi.sh` 之后存在；开发者在源码树里 `npm run tauri dev` 时命中）。
4. 裸名 `pi`，交给 `PATH` 解析（本机全局装过 `npm i -g @earendil-works/pi-coding-agent`，或完全没跑过 `fetch-pi.sh` 时的最终兜底）。

开发时不跑 `fetch-pi.sh` 也能用：只要 `PATH` 上有 `pi`（或设了 `SUPERAGENT_PI_BIN`），第 4/1 级照常兜底，不强制要求这个目录存在。
