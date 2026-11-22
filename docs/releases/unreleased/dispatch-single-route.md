Needs a minor release: `STEVE_NODE_BIND` and `STEVE_NODE_PLACEMENT` are removed and new sessions are placed differently; move any Node you named there into a Bot's execution Nodes after upgrading.

事项的新会话只按一种方式安排：交给执行它的 Bot 的执行节点（按顺序，首个合格的优先，其余备用），有 Team 时再按 Team 收窄。Steve 主 Bot 使用 Hub 本机节点。其他 Bot 的执行节点默认也是 Hub 本机，显式留空则不再接新任务。“原有派发”选项去掉了：设置页的执行节点直接显示当前列表，Team 选择里的“原有派发”改为“不使用 Team”。

`STEVE_NODE_BIND`（把新会话交给指定 Node）和 `STEVE_NODE_PLACEMENT=spread`（按负载把新会话分给更空闲的 Node）不再生效，请改在 Bot 的“执行节点”里绑定远程 Node。事项要求的 Node 标签只在 Bot 的执行节点里挑选；没有带该标签的节点时，会话会说明原因而不再按别的方式安排。升级迁移会删除升级前配对节点的自动分流名单，以及与实际绑定对不上的旧放置记录。
