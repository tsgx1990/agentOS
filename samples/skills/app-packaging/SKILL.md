---
name: app-packaging
description: 生成一个可安装的应用包（package.json 清单 + permissions.json 权限声明 + ui/agent 目录结构）时应遵循的规范——字段怎么填、权限怎么声明得刚好够用、目录怎么摆放，帮应用工坊（Maker）产出的应用能被宿主顺利装上、且不多要一分权限。
license: MIT
compatibility: 给 Maker（应用工坊）生成新应用时使用；不是给普通用户直接阅读的技能
---

# 应用包规范

生成一个新应用前，先确认要产出下面这套结构，字段按这份规范填，不要凭空
发明字段名。

## 目录结构

一个应用包至少包含：

- `package.json`：清单文件，见下面的字段说明。
- `permissions.json`：权限声明文件，清单里 `superagent.permissions` 指向它。
- `ui/`：界面目录，清单里 `superagent.ui` 指向其中的入口文件。
- `agent/`：人格与提示词目录，通常含 `persona.md`。

## package.json 关键字段

- `name`：应用标识，会被归一化成 app id，避免使用容易和已有应用撞名的
  通用词。
- `version`：语义化版本号，形如 `1.0.0`。
- `keywords`：必须含 `superagent-app`，否则宿主不认这是一个合法应用。
- `engines.superagent-host`：声明兼容的宿主版本区间。
- `superagent.schemaVersion`：当前固定为 `1`。
- `superagent.displayName`：给用户看的应用名称，中文、简短、说清楚这个应
  用是干什么的。
- `superagent.category`：从宿主已有的分类里选一个贴切的，不要自造新分类。
- `superagent.ui`/`superagent.permissions`：分别指向 `ui/` 与
  `permissions.json` 里的相对路径，不能是绝对路径，也不能包含上跳到包目
  录之外的路径。
- `superagent.model`/`superagent.tools`：按需声明，不需要就省略，不要为
  了"以防万一"多填。

## permissions.json 的最小权限原则

只声明这个应用真正需要的权限，不要预留"以后可能用得上"的权限：

- 不需要读写文件就不声明 `filesystem`。
- 不需要调用其它应用就不声明 `agents.call`。
- 不需要外部服务就不声明 `connectors`；需要时，`access` 能选只读就不选
  读写。
- 不需要定时任务/系统通知就不声明 `scheduledTasks`/`system`。

多要一分权限，用户安装时就要多看一行确认，也会让这个应用看起来比实际需
要的更"重"。

## 生成后自查

产出后对照一遍：清单里引用的每个相对路径文件是否真的存在、权限声明是否
和应用实际会用到的能力一一对应、`displayName`/`description` 是否讲清楚了
这个应用做什么。不确定某个字段该怎么填时，参照宿主已有的内置示例应用，
不要臆造新的字段名或取值。
