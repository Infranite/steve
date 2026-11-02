# 技术验证一：ACP 适配器 + Steve 工具（MCP）

2026-10-31，Mac mini（macOS 27.0.1）。脚本：`spikes/acp-mcp/spike.mjs`。

## Claude Code（`@agentclientprotocol/claude-agent-acp` 0.99.1 + Claude Code 2.1.308）

| 能力 | 结果 | 说明 |
| --- | --- | --- |
| 初始化 | 通 | 协议 1；声明 `loadSession`、MCP HTTP |
| 注入 Steve 工具 | 通 | `session/new` 的 `mcpServers` 传 HTTP 服务 + Bearer 令牌；3 个工具都被调用 |
| 权限请求 | 通 | 写文件和调用 MCP 工具都会请求权限；Steve 对自己的工具自动放行，其余转成决定 |
| 文件读写 | 通 | |
| 用量 | 通 | `usage_update` 给出已用上下文、窗口大小、费用和模型；`session/prompt` 返回 token 用量 |
| 进程被杀后恢复 | 通 | 新进程 `session/load` 同一个会话，重放 12 条更新，记得之前的回答 |
| 一轮耗时 | 11 秒 | 含 4 次工具调用 |
| 需要的配置 | `CLAUDE_CODE_EXECUTABLE=/opt/homebrew/bin/claude` | 不设置时适配器找自带的二进制，启动失败（spawn -88） |

## Codex（`@zed-industries/codex-acp` 0.16.0 + Codex CLI 0.166.0）

| 能力 | 结果 | 说明 |
| --- | --- | --- |
| 初始化 | 通 | 协议 1；声明 `loadSession`、MCP HTTP（不支持 SSE） |
| 注入 Steve 工具 | 通 | 和 Claude Code 一样传 HTTP + Bearer；3 个工具都被调用 |
| 模型 | 通，但接口不同 | `session/new` 不返回 `models`，模型只在 `configOptions` 的 `model` 里（来自本机模型目录，默认 `gpt-6.1-sol`）；`session/set_model` 不存在（Method not found），要用 `session/set_config_option`（`configId: "model"`） |
| 模式 | 通 | 只有 `read-only` / `auto` / `full-access`，默认跟随 config.toml（本机是 `full-access`）；`session/set_mode` 可用 |
| 权限请求 | 通 | `full-access` 下一次都不问。`read-only` 下写文件问一次；调用 MCP 工具也问，标题是 `Approve MCP tool call`，工具名在 `rawInput.server_name` + `rawInput.request.message` 里 |
| 用量 | 部分 | `usage_update` 只有 `used` / `size`，没有费用；`session/prompt` 不返回 token 用量 |
| 进程被杀后恢复 | 通 | `session/load` 重放 10 条更新，记得之前的回答 |
| 一轮耗时 | 22 秒 | 含 4 次工具调用 |

本机需要的处理：

- `~/.codex/config.toml` 的 `model_catalog_json` 是给新版 Codex 写的，适配器内嵌的 Codex 核心解析不了（缺 `base_instructions`、不认 `max` 推理档），启动即退出。
- 目录里的模型是 `tool_mode: code_mode_only`，适配器一执行命令就启动内嵌 V8，在本机崩溃（`Failed to reserve virtual memory for CodeRange`，SIGTRAP）。改成 `direct` 后正常。
- 所以 Steve 生成一份修正过的目录副本（`$STEVE_HOME/codex/models.json`），用 `-c model_catalog_json=…` 传给适配器；用户自己的文件不动。
- Steve 的权限档映射：`plan`、`default` → `read-only`（改文件、跑命令都问），`acceptEdits`、`auto` → `auto`（工作区内随意，出网或写工作区外才问）；从不用 `full-access`。每次启动进程都显式设置，不跟随 config.toml。
- npm 包的 `bin/codex-acp.js` 用 spawnSync 启动原生二进制，杀掉外壳会留下孤儿进程；Steve 直接运行平台二进制。

在 Steve 里端到端跑通（临时 Hub，`STEVE_HOME` 为临时目录，端口 8461）：codex Bot 接事项后调用 `steve_update` / `steve_deliver`，写文件变成一条“允许执行：Edit …/hello.txt”的决定，批准后交付、验收，事项完成；Hub 重启后同一会话 `session/load` 继续对话。

## 模型网关：harness 无界面调用

`claude -p … --output-format json --model haiku`：结构化分类正确，5.3 秒，0.039 美元一次。分流异步进行，先回“收到”。
