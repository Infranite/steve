// Domain-owned reason texts, grouped by the stable reason prefix.
export default {
  "node.no_such_operation": "找不到操作 {operation}。",
  "node.snapshot_gone": "快照已失效，请重新读取。",
  "node.downgrade_refused": "此版本无法读取 Node 日志，请选择当前或更新的版本。",
  "node.error_detail_discarded": "Node 升级前的错误详情已丢弃；这次失败不会自动重试。",
} as const
