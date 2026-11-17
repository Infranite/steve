Needs a minor release: dynamic workflows now default off; operators wishing to retain multi-Bot orchestration must explicitly set `STEVE_DYNAMIC_WORKFLOWS=1` and restart the Hub.

Ordinary tasks execute in the current responsible Bot session without extra settings. Disabled workflows cannot be planned, retried, resumed or start new child turns, including through cached tools or queued prompts. Existing in-flight work can settle, and stop reconciliation, results, history and acceptance safeguards remain available.

Existing unfinished graphs are not automatically accepted or converted. Pause them, wait for confirmed settlement, then explicitly confirm single-session takeover on the item page if desired. The confirmation records that required graph review/integration acceptance is waived; earlier review defects still need fixing and ordinary PRs still require human review. The existing panel explains the disabled state and keeps safe recovery actions available.
