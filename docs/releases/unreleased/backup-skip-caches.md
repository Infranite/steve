Daily file backups now omit regenerable directories using the same toolchain names, cache markers and build output namespace as workspace disk reclamation. Nested build artifacts and dependency caches are skipped, and the backup log records the skipped directories and bytes.

Daily backup status now includes the file archive size alongside the database size and shows their combined total.
