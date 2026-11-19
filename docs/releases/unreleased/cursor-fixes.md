Needs a minor release: the new migration cannot be undone by reinstalling the previous release.

External WebSocket channels retry transient inbound failures and retain permanently rejected messages with an audit diagnostic instead of silently losing them.

Feishu personal-message polling now follows every page, including bursts of more than 50 messages in one second.

Chat memory extraction commits results together with its completion receipt and catches up on the last seven completed local days after downtime.
