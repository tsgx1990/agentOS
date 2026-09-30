# 给 AI 编程助手的项目约定

这是一个**公开仓库**，开发直接在这里进行。每一次 `git push` 都是一次不可撤回的发布：内容、提交说明、作者身份都会永久公开，事后删除也会留在克隆、缓存和搜索引擎里。下面的规则优先于任何通用的默认做法。

## 硬规则

1. **提交与推送必须经过隐私闸。** 维护者的检出里装有 `pre-commit`、`commit-msg`、`pre-push` 三个钩子。永远不要用 `--no-verify`，不要改动 `core.hooksPath`，不要用别的办法绕过钩子。钩子报错时修改被拦下的内容；确认是误报时停下来告诉维护者，不要自行放宽规则。
2. **这些东西不进仓库，也不进提交说明**：密钥与令牌；真实邮箱；带真实用户名的本机路径；机器名、内部服务名这类运维细节；任何个人信息；对其它产品的点名评价（公开文档只描述问题类别）。测试里需要密钥形态的字符串时用明显的假值，示例路径用 `/Users/alice` 这类占位名。
3. **提交说明写给公开读者看。** 格式见 [`CONTRIBUTING.md`](CONTRIBUTING.md)。可以带 `Co-Authored-By` 署名行，**不要带会话链接**或任何会话标识。提交身份用仓库里已经配置好的那一个，不要修改 `user.name` / `user.email`，也不要用环境变量覆盖。
4. **不要新增 remote，不要强制推送，不要把被忽略的目录强行加入版本控制**（`git add -f`）。
5. **过程文档不写进本仓库。** 不要在这里创建 `HANDOFF.md`、`PROGRESS.md`、`docs/superpowers/`、`docs/research/` 这类交接、计划、调研文件；它们的位置见下一节。

## 维护者的内部文档（目录 `.internal/` 存在时适用）

`.internal/` 是一个单独的私有仓库，被本仓库忽略。它存在时：

- **开工先读** `.internal/HANDOFF.md`（当前状态、下一步、已知的坑）。收尾时更新它，并在 `.internal/` 里单独提交、推送。
- 设计规格写到 `.internal/docs/superpowers/specs/`，实施计划写到 `.internal/docs/superpowers/plans/`，调研写到 `.internal/docs/research/`。代码注释里引用的 `docs/superpowers/…` 路径都相对于 `.internal/`。
- **每个会话第一次提交前**运行一次 `.internal/guard/install.sh --check`，确认隐私闸真的在生效；它报错就先修好再提交。隐私闸的说明在 `.internal/guard/README.md`。
- 在 worktree 里工作时，内部文档仍然只有主检出里的那一份，用主检出的路径访问。

`.internal/` 不存在时（外部贡献者的克隆），这一节不适用，按 `CONTRIBUTING.md` 参与即可。

## 开发流程

- 改代码在 git worktree 里做，建在 `.claude/worktree/<名字>`（已被忽略）；完成后合并回 `main`，再删除 worktree。
- 门禁是 `./scripts/ci-local.sh`（与 CI 同一份步骤）。本地反复跑时加 `--skip-install`，只跑一步用 `--only <step>`。声称完成之前必须跑过并给出结果。
- 默认不跑的两类测试（写真实钥匙串的、依赖真实 pi 的）以及改安全内核的额外要求，见 `CONTRIBUTING.md`。
- 已知边界如实记录在 [`docs/known-limitations.md`](docs/known-limitations.md)；改动让其中某条不再成立或新增了边界时，同步更新它。
- 依赖改动后运行 `cd src-tauri && cargo deny check`。升级 pi 版本时，同时更新 `src-tauri/pi-version.txt` 和 `src-tauri/pi-sha256.txt`。
