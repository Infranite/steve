Needs a minor release: refresh open web tabs after upgrading the Hub so they receive item and session patches.

事项和会话用快照加载，变更按行更新。长列表随滚动渲染，归档事项和已结束会话可以继续翻页。

会话显示实际记录的 Bot、harness、模型和 Node，缺失的身份显示为未知。事项详情逐个列出参与的会话及其状态，方便核对不同 Bot 的分工。

在 `/api/stream` 订阅 `patch` 帧获取事项和会话变更。初始快照和历史分页分别可通过 `/api/snapshot` 与 `/api/views/page` 读取。
