-- 59: Steve's own model calls are booked under the harness adapter they ran on (harness parity S5),
-- not under the print CLIs they no longer use: `claude-cli` is `claude-code`, `codex-cli` is `codex`.
UPDATE usage_records SET harness = 'claude-code' WHERE harness = 'claude-cli';
UPDATE usage_records SET harness = 'codex' WHERE harness = 'codex-cli';
UPDATE OR IGNORE stat_daily SET harness = 'claude-code' WHERE harness = 'claude-cli';
UPDATE OR IGNORE stat_daily SET harness = 'codex' WHERE harness = 'codex-cli';
