Needs a minor release: Hub 与 Node 必须一起升级（Node 协议去掉了对旧版本的兼容），请同时换用随 Hub 提供的 Web 客户端。

不再兼容旧版本与旧数据：

- Hub 与 Node 只按当前业务协议 v12 交互：不再为旧 Node 退回旧的快照分页、会话容量与 harness 能力判断，`artifact-read-v1`、`harness-updates-v1` 两个能力声明删除；harness 组件只报告 `status`（`installed` 字段删除）。请先升级 Hub，再升级各 Node。
- 本机 Node 未开启时，流程里的 `cmd` 步骤不再改在 Hub 上直接执行，而是失败并说明"本机 Node 未开启"。
- 本地升级与发布脚本不再兼容旧 Hub 的升级 runner（`local-release.py build` 的下载桥、`--operation`、`--keychain-pass-stdin`）；`build` 只做源码构建，正式发布包用 `prepare-bundle` 或 `steve upgrade`。没有 `STEVE_HOME` 的签名服务定义、没有 `schema_compat` 标记的数据库会被拒绝。
- 升级迁移一次性清理旧数据：没有出站记录却标为"已发送"的回复改为"未确认"；把失败构建写成发布版本的事项卡片改为构建行；没有记录时点的旧"停止"请求和已结束会话里残留的排队输入删除；未写 harness 的模型选择补为默认适配器；模型种子标记合并为一个；仍未处理的接手卡片、新群卡片换成当前选项。没有周期或修订号的旧完成事件清除，事项本身保留。
- 调试面板显示当前推算状态及拟执行的动作。历史费用只在能通过 turn 或 call 确定归属时计入同一用量。
- macOS 首次安装直接把签名口令写入安装的 key store。启动时不再迁移平面口令文件，备份只复制签名身份和钥匙串。旧版 Python 签名初始化入口由 `steve setup` 取代。
- 网页与 macOS 客户端去掉了对旧 Hub、旧路由（`#/settings/hub`、`#/personas`、`#/plugins` 等）和旧卡片选项的兼容。请使用会话范围的排队输入接口 `/api/sessions/{id}/queued/{qid}`。
