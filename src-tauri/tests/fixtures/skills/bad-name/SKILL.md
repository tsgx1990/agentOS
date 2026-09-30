---
name: Bad-Name
description: 用来测试非法 name（含大写字母）在安装门里被拒绝，且不留半成品。
---

# Bad Name

frontmatter 里的 name 字段故意写成大写开头，validate_name 应当拒绝它。
