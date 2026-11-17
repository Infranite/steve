Needs a minor release: STEVE_WORKFLOW_MAX_PARALLEL changes from a Hub-wide collaboration limit to a per-root-item-tree limit.

协作并发不再让无关事项争抢全局默认四个名额。每个根事项及其所有层级的派生事项共享自己的协作额度，默认仍为 4；工作图的 `max_parallel` 只能进一步收紧当前图，不能给子事项重新分配额度。

升级后多个根事项可以同时执行超过四个协作单元，所有实际执行仍受所在 Node 的会话容量限制（默认 32）。运维应按机器资源设置 Node 容量；原 `STEVE_WORKFLOW_MAX_PARALLEL` 配置会保留数值，但不再限制整台 Hub 的总并发。暂停或结束中的执行在确认停止前仍保留占位，损坏的事项父链不会获得新的额度。本次不开放工作单元再建嵌套工作图。
