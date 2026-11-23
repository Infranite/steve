Needs a minor release: refresh open web tabs after upgrading the Hub so they receive decision patches.

待处理页从快照加载决定，后续按卡片更新。星标联系人的决定、批准后发送失败和其他需要你处理的决定计入角标。草稿按对话归为一行，可以一次忽略全部草稿。已收起的卡片显示原因，已处理的记录可以继续翻页。

`/api/snapshot?kinds=decision` 返回初始快照，`/api/stream` 用 `patch` 帧推送决定变更。原先订阅 `decision` 帧的客户端需要订阅补丁。决定的代码值和原始文案也可通过 `/api/views` 与 `/api/views/page` 读取。
