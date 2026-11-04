# ST-46 对话里的表格与 A2UI 交互界面

结论：Markdown 表格不再被框进 300px 的小窗口，随内容展开、只在太宽时横向滚动。Steve 的回答可以带一段 A2UI v0.9.1 界面描述，网页把它画成卡片、图表、选项和表单；按钮点击作为一条受控消息回到同一个对话，每个按钮只提交一次，刷新后状态还在，描述有误时退回文字说明，不会让整条消息空白。

## 表格为什么被框住

Streamdown 2.7.1 给每张表格套了带复制、下载、全屏按钮的外框，并默认 `tableMaxHeight = 300`，超出就在框内纵向滚动。对话里因此出现“页面滚动里再套一个滚动”。

改法（`web/src/components/markdown-impl.tsx`、`web/src/index.css`）：

| 项 | 做法 |
| --- | --- |
| 高度 | `tableMaxHeight="none"`，表格多高就多高，跟页面一起滚 |
| 外框和工具栏 | `controls.table = false`，自己的 `table` 组件只留一层横向滚动容器；行间细线、表头次要色，和正文同一层级 |
| 宽表格 | 单元格最小 6em，窄屏上列多时整表横向滚动，不撑宽页面 |
| 超长表格 | 超过 30 行时先显示前 20 行，下面一个“展开其余 N 行” |

## A2UI 核实结果（2026-11-05）

- 版本：v0.9.1 是当前生产版本（规范已关闭），v1.0 仍是候选（计划 2026 Q4 定稿，改动包括 actionResponse、action id、theme 改名）。Steve 按 v0.9.1 实现，`version` 接受 `v0.9` 和 `v0.9.1`。
- 服务端到客户端四种消息：`createSurface {surfaceId, catalogId, theme?, sendDataModel?}`、`updateComponents {surfaceId, components[]}`、`updateDataModel {surfaceId, path?, value?}`、`deleteSurface {surfaceId}`。组件是扁平列表（邻接表），必须有一个 `id: "root"`；属性可以是字面量、`{"path": "/json/pointer"}` 绑定或 `{"call": …}` 函数。
- 客户端到服务端：`action {name, surfaceId, sourceComponentId, timestamp, context}`（`context` 里的绑定先在客户端求值）和 `error {code, surfaceId, path, message}`。
- 基础目录 18 个组件：Text、Image、Icon、Video、AudioPlayer、Row、Column、List、Card、Tabs、Modal、Divider、Button、TextField、CheckBox、ChoicePicker、Slider、DateTimeInput；14 个函数：required、regex、length、numeric、email、formatString、formatNumber、formatCurrency、formatDate、pluralize、openUrl、and、or、not。
- 基础目录**没有图表**。规范的做法是自定义目录：同一 `catalogId` 在两端都认得，渲染端为每个自定义组件给出实现。
- 官方 React 渲染器 `@a2ui/react` 0.15（Apache-2.0）依赖 `@a2ui/web_core`（解包 8.5 MB，含 lit、zod、markdown-it），版本仍在快速变化，样式也是它自己的。Steve 只需要基础目录加一个图表，自己按规范实现更小、能沿用现有设计和图表组件，所以不引入。

来源：a2ui.org 的 v0.9 / v1.0 规范页与路线图，GitHub `a2ui-project/a2ui` 仓库 `specification/v0_9_1`（`catalogs/basic/catalog.json`、`json/server_to_client.json`、`json/client_to_server.json`、`json/common_types.json`），npm 上 `@a2ui/react`、`@a2ui/web_core` 的元数据。

## 接入方案

### 消息里怎么放

A2UI 放在回答 Markdown 里的一个 ` ```a2ui ` 代码块中，内容是 A2UI 消息，每行一个 JSON（JSON Lines），也接受一个 JSON 数组。这样：

- 文字、表格、Mermaid 和界面可以在同一条回复里按顺序出现；
- 旧消息、其他渠道和历史记录不需要改格式，没有 `a2ui` 块的消息照旧显示；
- 模型的上下文里看到的就是它自己写的描述，追问时能接着改。

每个代码块是一个独立作用域，`surfaceId` 只需在块内唯一。块用内容哈希标识，提交记录挂在“消息 + 块 + surface”上：一个界面提交一次（无论点的是哪个按钮），之后整块变成只读记录。需要再选时，Steve 给一个新界面。

### 目录

| catalogId | 内容 |
| --- | --- |
| `https://a2ui.org/specification/v0_9/catalogs/basic/catalog.json` | 基础目录 |
| `steve:catalog/v1` | 基础目录 + `Chart` |

`Chart`（Steve 目录）：

```json
{"id":"trend","component":"Chart","kind":"line","title":"每日完成","x":"day",
 "series":[{"key":"done","label":"完成"}],"data":{"path":"/rows"},"last":{"path":"/range"},"unit":"件"}
```

- `kind`：`line` 折线、`bar` 柱、`stack` 堆叠柱、`hbar` 横向条形（分布）。
- `data`：行数组（字面量或绑定），`x` 是横轴字段，`series` 是数值字段。
- `last`：只显示最后 N 行，可绑定到一个 ChoicePicker，在本地完成“近 7 / 30 天”这类筛选，不需要来回请求。
- 复用统计页的图表（同样的配色、十字线提示、键盘左右键），每张图可切换成表格。

### 渲染与安全

- 只渲染目录里的组件，不执行脚本、不插入 HTML；Text 按受限 Markdown 显示。
- Image / Video / AudioPlayer 只接受 `https:` 和 `data:` 图片地址；`openUrl` 只打开 `http(s)` 链接，新标签页、`noopener`。
- 上限：每块 64 KB、每个 surface 300 个组件、嵌套 40 层、模板列表 500 项，超出部分不画并提示。

### 受控交互

1. Button 的 `action.event` 在客户端求值 `context` 和 `checks`，校验不过就在按钮下显示原因，不发送。
2. 发送 `POST /api/thread/{消息}/a2ui {block, surfaceId, sourceComponentId, name, label, context, data, timestamp}`。
3. Hub 在对话锁内检查：消息属于本工作区、是 Steve 在主线对话里的回复、正文里确有 `a2ui` 块和这个 surface；同一个“块 + surface”已经提交过，就不再发送，返回原记录和 `already: true`。
4. 通过后把提交记录写进原消息的 `meta.a2ui`（含当时的数据快照），并以用户身份在主线对话里发一条消息（`meta.a2ui_action`，引用原消息）。正文是给模型读的标准 A2UI `action` JSON；网页把这条消息显示成“提交了「…」”的简短记录。之后按普通消息走路由和回答，忙时同样进入排队。
5. 改设置这类操作仍然走可应用、可撤销的改动卡片：界面只把用户的选择交回 Steve，不直接执行任何操作。

| 情况 | 表现 |
| --- | --- |
| 连点、多标签页同时点 | 前端在途时禁用按钮；Hub 按“块 + surface”去重，第二次得到原记录，界面提示“已经提交过了”并显示为已提交 |
| 刷新 | 已提交的按钮显示“已提交”，表单回填提交时的值；没提交的输入存在本机，刷新后还在 |
| 提交失败 | 提示原因，按钮恢复可点，输入保留 |
| 其他位置（事项详情、IM 对话、知识预览、草稿） | 只读显示，按钮不可点，提示到主线对话里操作 |

### 降级

| 情况 | 表现 |
| --- | --- |
| 正在流式输出、代码块未闭合 | “正在生成界面…”占位 |
| JSON 无法解析、缺 `root`、版本不认识 | 一行说明 + 可展开的错误位置和原始描述，消息其余部分正常显示 |
| 不认识的组件 | 该处显示“不支持的组件：X”，其他组件照常 |
| 引用了还没定义的子组件 | 跳过 |

### 模型怎么知道

主线对话的直接回答和就地研究的提示词里各加一段说明（何时用、代码块格式、组件和 Chart 的字段、一个示例），IM 群聊里的回答不加，避免把 JSON 发到群里。

## 验证

- 单元测试 `web/test/a2ui.test.mjs`：解析 JSON Lines / 数组、邻接表、绑定和模板路径、函数、checks、增量更新、删除、未知组件、上限和错误路径。
- 浏览器检查 `web/test/a2ui.browser.mjs`：1440 / 390 宽、浅深色；长表格完整展开且无内部纵向滚动；趋势图 + 时间筛选；表单 + 提交（校验拦截、连点只发一次、另一标签页已提交、失败重试、刷新恢复）；旧消息、未闭合代码块、无效描述和未知组件。
- Hub（`steve-core` 的 `an_a2ui_action_is_said_once_per_surface`）：提示词里有界面说明、去重、非 Steve 消息和其他工作区拒绝、找不到 surface 拒绝、写回 meta 并作为用户消息发出、Steve 接着回答。
