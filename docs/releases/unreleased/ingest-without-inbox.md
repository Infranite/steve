Hub 收到 Node 事件即应用，不再保存原始事件副本：每批事件在一个事务里按流顺序应用，提交后才向 Node 确认；重发的事件只确认不重复应用，未确认的事件由 Node 重发。一个事件应用失败只停它所在的流（会话会收到说明），其他流照常。升级迁移会删除 Node 事件收件箱和隔离区，并从中保留会话的恢复句柄与修订号；请按常规流程先排空再升级，已确认但尚未应用的事件无法再取回，对应会话会在对账时标记为缺口。

新增指标 `steve_node_ingest_commits_total`、`steve_node_ingest_events`、`steve_node_ingest_seconds`，与已有的 `steve_process_disk_written_bytes` 一起观察写入量。
