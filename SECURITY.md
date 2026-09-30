# 安全

## 威胁模型（三句话）

1. **第三方应用、技能与应用工坊产物都是不受信的代码**：它们在各自的 `sandbox-exec` 沙盒里运行，只能读写自己的数据区和清单里声明并经用户同意的目录，默认无网络。
2. **宿主是唯一执行点**：连接器调用、通知、应用间调用都经每应用独占的 unix socket 回到宿主，宿主按监听器绑定的身份（不是请求里自称的）和该应用的清单做二次授权，拒绝也写审计。
3. **权限不随调用链放大，控制值不取自 wire**：应用 A 调应用 B 时 B 用自己的清单启动，调用深度上限 3；身份、深度、`trusted` 都来自宿主侧。

## 不在防护范围内

- 用户自己标为受信（`trusted`）的应用：受信即放行全部网络，宿主不再替用户把关它的出网目的地。
- 模型服务商与用户自己接入的 MCP 连接器本身的安全性。
- 已经拿到本机用户权限的攻击者：宿主与它保护的数据同属一个用户。
- macOS 之外的平台：没有操作系统级沙盒。

## 已知边界

明确接受的取舍和尚未解决的缺口集中列在 [`docs/known-limitations.md`](docs/known-limitations.md)。如果你发现的问题已经在那份清单里，它不算新漏洞，但欢迎提出更好的修法。

## 自证材料

- `src-tauri/tests/sandbox_escape_it.rs`：越权套件（读写逃逸、symlink、`$HOME` 守卫）。
- `src-tauri/tests/real_pi_bash_escape_it.rs`：真实 pi 在生产沙盒下执行逃逸尝试被拒。
- `src-tauri/tests/capability_invariants_it.rs`、`p6a_consent_alignment_it.rs`：同意框与执行层对齐的不变式。
- `src-tauri/tests/call_agent_socket_it.rs`、`maker_socket_it.rs`：socket 身份门控与越权回归。

仓库里有几份**故意写成恶意的测试样本**，用来证明沙盒与安装门确实会拦住它们：`src-tauri/tests/fixtures/mock-malicious/`、`src-tauri/tests/fixtures/mock-thirdparty/` 与 `src-tauri/tests/fixtures/skills/evil-skill/`（含越权读写、`curl … | sh` 之类的写法）。它们只在测试里被加载，不会随应用分发；安全扫描工具对这些路径的告警属于预期。

## 报告漏洞

请使用 GitHub 的私密漏洞报告：在本仓库的 **Security** 页签里点 **Report a vulnerability**。不要在公开 issue、讨论区或 PR 里披露细节。

报告里请尽量包含：受影响的文件或功能、复现步骤、你认为能越过的是哪一条边界。这是个人维护的项目，没有响应时限承诺，但安全报告会优先处理；确认后会在修复提交里致谢（如果你愿意）。

## 支持的版本

项目尚未发布正式版本，只有 `main` 分支的最新快照会收到安全修复。
