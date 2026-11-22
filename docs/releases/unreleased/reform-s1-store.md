The installer uses the same database schema checks as the Hub when preparing a release upgrade or rollback.

Calendar agenda, search, and free/busy read from consistent database snapshots while sources sync.

GitHub delivery traces read pull requests, issues, and flow runs from a single storage snapshot. GitHub change notices follow the transaction that recorded them.

Mail synchronization, threads, attachments, drafts, and the send queue use the shared storage contract. Thread updates publish after their database changes commit.
