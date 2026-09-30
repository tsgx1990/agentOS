# 参与开发

## 先知道这件事：公开仓库是导出的快照

公开仓库的内容是从维护者的开发仓库定期导出的，所以提交历史是一串快照，不是逐次开发提交。这意味着：

- issue 和 PR 都欢迎，但 PR 不会被直接合并：通过评审后，改动会被移植进开发仓库，随下一次快照出现在这里，提交信息里会注明来源与作者。
- 安全问题请不要开公开 issue，按 [`SECURITY.md`](SECURITY.md) 私下报告。

## 环境

- macOS（唯一受支持平台：沙盒依赖 `sandbox-exec`，密钥依赖 Keychain）
- Rust 稳定版（含 `clippy`、`rustfmt` 组件）
- Node 20+（前端与 hosttools 的类型检查、测试）
- pi：`npm i -g @earendil-works/pi-coding-agent`，或 `./scripts/fetch-pi.sh` 下载官方独立二进制

```bash
npm ci && (cd src-tauri/hosttools && npm ci)
./scripts/ci-local.sh --skip-install     # 确认基线全绿
```

## 门禁

提交前必须通过 `./scripts/ci-local.sh`，它和 `.github/workflows/ci.yml` 是同一份步骤：

1. 文本检查（受控文本文件里不得有 NUL 字节）
2. `cargo fmt --check`
3. `cargo clippy --all-targets -- -D warnings`
4. `cargo test`（`src-tauri/`）
5. `npx tsc -b`
6. `npx vitest run`
7. `npm run build`

有两类测试标了 `#[ignore]`，默认不跑，需要显式执行：

- 依赖真实 pi 或 node 的套件：`cd src-tauri && cargo test -- --ignored`（先跑 `./scripts/fetch-pi.sh` 并设 `SUPERAGENT_PI_BIN`，或保证 PATH 上有 pi）。CI 里对应 `real-pi.yml`，手动触发或每周跑一次。
- `vault::` 里三条会写真实钥匙串的测试：在没有可交互安全会话的环境里会失败，所以请在本机终端里跑 `cd src-tauri && cargo test --lib vault:: -- --ignored`。改动密钥存取相关代码时必须跑。

## 工作方式

- **TDD**：先写会失败的测试，再实现，再让它变绿。修 bug 先复现、找根因，不允许只把坏结果盖住。
- **声称完成必须附证据**：跑了什么命令、看到什么输出，写进 PR 描述。
- **提交信息**：`<type>(<scope>): <中文摘要>`，`type` 取 `feat` / `fix` / `docs` / `test` / `refactor` / `style` / `ci` / `chore`，`scope` 通常是模块名。每个可独立验证的任务单元一次提交。
- **不要提交任何密钥、本机路径或个人信息**，测试里需要密钥形态的字符串时用明显的假值。

## 改安全内核的额外要求

`sandbox.rs`（SBPL 生成）、`mcp_socket.rs`（身份绑定）、`call_bus.rs`（三闸）、`session_mgr.rs::spawn_call_session` 签名，这四处是经过对抗审查的边界：

- 改动必须让 `cargo test --test sandbox_escape_it` 保持全绿（当前 12/12），并跑一遍 `--ignored` 的真实 pi 套件。
- 新增的放行规则要有对应的「被拒」测试；只加正例不算覆盖。
- 身份、深度、权限这类控制值只能来自宿主侧绑定，不能从子进程发来的数据里读。
- 不允许为了让测试通过而放宽 `BASE_PROFILE`。

## 许可证

本项目以 [Apache License 2.0](LICENSE) 发布。你提交的贡献默认以同一许可证授权（Apache-2.0 第 5 条），不需要另签协议。引入新依赖前请确认其许可证与 Apache-2.0 兼容；GPL 家族的依赖不接受。Rust 依赖的许可证白名单、已知漏洞与来源检查写在 `src-tauri/deny.toml`，改动依赖后请跑 `cd src-tauri && cargo deny check`。
