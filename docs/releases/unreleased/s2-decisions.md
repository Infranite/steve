待处理（决定）改为增量同步：页面打开时取一次决定快照（`GET /api/snapshot?kinds=decision`，内存直出、不查库，未变化时 304），之后只接收事件流里的 `patch` 帧（`{"type":"patch","ws","rev","ops":[{type,op,id,view?}]}`）；事件流不再发送 `decision` 事件，答复、撤回卡片也不再整份重拉 `/api/state`。`/api/state` 的输出保持不变。新增 `GET /api/views?kind=decision&ids=…`、`GET /api/views/page?kind=decision&status=settled&since=…`（已处理列表）和 `GET /api/chan-messages/{id}/open-url`（“在飞书中打开”改为卡片显示时再取）；事项详情里的 `decisions` 改为决定视图（`undo` 换成 `undo_until`，问题与草稿按原文返回）。写操作若改动了视图，响应带 `x-steve-rev`，页面等到不旧于它的补丁再结束。

待处理分三层（#213 的分层部分），由 Hub 按固定顺序判定并写在决定视图的 `tier` 上：批准后发送失败的、星标联系人的消息永远“要你决定”；你已在该对话里亲自回复过的“已收起”（`folded: replied`）；替你起草的回复是“草稿”，每个对话一行；其余（Steve 的提问、连接断开等）“要你决定”。角标、主对话上方的待处理条和提醒只算“要你决定”。

新增指标 `steve_projection_*`（冲刷耗时与条数、补丁操作数、补丁延迟、内存视图数与字节、对账差异、重建次数），`steve_sse_frames_total` 按 `type`（patch/live/event/control）区分，网页上报带 `page` 标签并新增 `steve_web_patch_apply_seconds`；运行状况页增加“页面数据更新延迟”和“视图对账差异”（应始终为 0）。升级后请刷新已打开的网页。
