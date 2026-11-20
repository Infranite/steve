// Domain-owned reason texts, grouped by the stable reason prefix.
export default {
  "node.no_such_operation": "Operation {operation} was not found.",
  "node.snapshot_gone": "The snapshot is no longer available. Read a new snapshot.",
  "node.downgrade_refused": "This release cannot read the Node journal. Choose the same or a newer release.",
  "node.error_detail_discarded": "Error details from before the Node upgrade were discarded. This failure will not retry automatically.",
} as const
