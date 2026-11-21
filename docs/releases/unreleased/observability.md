Needs a minor release: the retention slice histogram is renamed `steve_retention_slice_seconds` (was `steve_retention_slice_ms`), and Node ingest metrics now carry a `node` label

The Hub now collects metrics from its start, whether or not they are exported; `STEVE_METRICS=prometheus` still serves them on their own listener, and the management socket serves the same text at `/metrics`. New metrics cover live update streams, the event bus, commands and links to each Node, what each Node reports about its journal, backlog, harness processes and own process, plugin calls and restarts, agent turns and model calls (tokens and cost), the Hub process's CPU, memory, descriptors and threads, and the web page's long tasks and interaction latency.

Settings · About has a new Health section for the instance owner: the key figures of the last fifteen minutes beside their limits, with what is past a limit said first (from `GET /api/system/health`).
