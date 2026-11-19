Daily file backups now omit regenerable directories only within item workspaces, using the same toolchain names, cache markers and build output namespace as workspace disk reclamation. Cache directories at any depth in a workspace are skipped, including those inside outputs/. Attachments, signing data, secrets and stored deliveries remain included even when their directories have cache names or markers. The backup log records the skipped directories and bytes.

Daily backup status now includes the file archive size alongside the database size.
