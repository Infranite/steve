# Hub benchmark

Python 3.11+，仅标准库。将同一静态 SQLite 快照与不同的 Hub 构建配对运行；每次在 `--work` 下新建私有的 `run-*` 目录，保留副本和日志以便排查。源快照只通过文件复制读取，不能传入仍有活动 WAL 的数据库文件。

```sh
python3 -I scripts/reform/bench/run.py \
  --hub-bin /path/to/steve-hub --snapshot /path/to/snapshot.sqlite3 \
  --work /path/to/isolated-bench --port 18770 \
  --out /path/to/results/before.json --label before
python3 -I scripts/reform/bench/compare.py before.json after.json --md
python3 -I -m unittest discover -s scripts/reform/bench -v
```

`--config custom.json` 覆盖内置默认值的顶层键，`endpoints` 整体替换；默认配置也在 `default.json`。默认每个接口首次请求加 20 次串行请求，空闲 20 秒、4 客户端负载 60 秒、每秒 2 次通知写入、负载中运行 CLI 3 次。540 秒总预算包含复制、启动和测量，清理在预算外；正常运行应低于十分钟。慢请求或错误会进入结果，不自动重试。退出码 0 表示采集完成且请求、CLI、探针没有错误；2 表示采集完成但存在测量错误；1 表示运行失败或清理失败。失败时尽量保存部分 JSON。

## 隔离与生命周期

Hub 和 CLI 都通过 `/usr/bin/env -i` 和相同的干净环境启动，显式设置隔离的 `STEVE_HOME`、Hub 端口、本地 Node 端口（Hub + 3），禁用恢复、备份、适配器更新，三个适配器指向 `/usr/bin/false`。另设私有 `HOME`，不读取当前用户的登录配置。启动点对隔离环境做断言。不要用被测二进制的 `--help` 或 `--version` 探测能力：该构建遇到未知参数会启动 Hub。版本从 `/healthz` 获取。

工具拒绝当前用户的 `~/.steve*` 目录及其子目录（解析符号链接后判断），拒绝 Hub/Node 端口 8440，并检查两个端口没有被占用。只修改快照副本中最早 owner 的 subject 为 `dev:local`；通过 `/auth/dev` 获取内存 cookie。HTTP 不使用环境代理，不跟随重定向，不保存 cookie 或响应内容。CLI 使用被测二进制的 `--access token create --name … --json`，输出丢弃，token 只留在隔离数据库内。

Hub 会给本地 Node 调用 `setsid()`，因此清理不能只杀 Hub 进程组。退出、异常、SIGINT、SIGTERM、总超时都会冻结父进程、发现子树并结束它们的所有进程组；Hub 已退出时，从本次新 home 的 `data/node/service.pid` 找到 Node，并核对它的隔离环境后清理。SIGKILL 无法捕获。工具不删除旧目录，每次重跑都建立新副本，不复用被测数据库。

## 测量方法

| 项目 | 方法与含义 |
| --- | --- |
| 启动 | 从启动子进程到首次成功读取 `/healthz` 的时间；不含快照复制和 owner 修改。 |
| 接口 | 每个接口第一次调用作为 cold，随后串行 N 次作为 warm；cold 指本次脚本首次访问，不保证操作系统缓存冷、也不保证其他接口没预热共享数据。 |
| 响应字节 | 完整读取响应后的字节数，发送 `Accept-Encoding: gzip`，不自动解压，因此是压缩响应体大小，不含 HTTP 头。 |
| 延迟 | `perf_counter` 测量请求到读完响应；分位数将排序数据的 `(n-1)*p` 位置线性插值。空集为 null；超时和 HTTP 错误也纳入分布，并单独计错。状态 0 表示未得到 HTTP 响应。 |
| 并发 | K 个独立客户端持续 GET；单独写线程按目标频率 POST，不补发错过的时间槽。吞吐用实际阶段时间（含在途请求排空）作分母；同时给出成功读取吞吐和实际写入频率。 |
| 失效 | 默认 POST `/api/notifications/read`，请求体 `{}`。已核实基线源码 `api/mod.rs` 路由、`api/misc.rs::notifications_read` → `notify.rs::mark_seen` 发布 `HubEvent::Settings`；`bus.rs::publish_value` 将非 STATELESS 的全局事件计入 state generation。未来构建改变该契约时通过配置显式换写接口，并记录基线。 |
| 写锁 | 同一副本上独立 SQLite 连接，每 5 ms 执行零等待 `BEGIN IMMEDIATE` 并立即 `ROLLBACK`。只将 SQLITE_BUSY/LOCKED 算忙；其他错误单独报告。忙比例是忙样本/有效样本；连续忙样本形成忙段，其时长是采样估算，受调度、探针自身拿锁影响，首尾段可能截断，不代表精确事务持锁时间。 |
| CLI | 在负载阶段均匀安排真实 CLI；每次记录启动到退出的耗时、返回码和超时标记。它包括建库检查和 CLI 工作的总时间，是拿锁体验的代理指标，不是纯锁等待时间。单次不超出 CLI 超时或剩余负载时间。 |
| 磁盘 | Darwin `proc_pid_rusage` v4 的设备写入和逻辑写入计数。每秒重新发现 Hub 全部后代，含独立 session 的 Node，各阶段基线作差并给每进程与合计值；新子进程计入首次观察到的生命周期计数。退出子进程最后一次采样之后的写入不可见，短命子进程可能漏采；`present_at_end` 和 `sampling_errors` 辅助识别缺口。非 Darwin 明确标为 unavailable/null。CLI 为单独测量进程，不纳入 Hub 后代磁盘合计。 |
| 主机负载 | 前后调用 `uptime`，只保存三个 load average；采样 `ps` 的 CPU%，仅保存进程名和峰值 CPU%，不保存命令参数或主机名。`ps` CPU% 是操作系统提供的平均口径，可能超过 100%。 |

基线构建在 `STEVE_NO_RESUME=1` 下禁用本地 Node，因此该模式的实测磁盘明细可能只有 Hub；`health.local_node_enabled` 保留这个事实，不能把它解释成 Node 零写入。磁盘实现会采集实际存在的 Hub 后代。

WAL 提交速率为可选项，本工具未采集。总吞吐与磁盘字节主要看合计，进程明细可能因进程重启而不可逐行比较。探针、CLI、CPU 观察本身会干扰负载，比较时应保留相同配置，并检查前后系统负载。

## JSON 字段

| 字段 | 内容 |
| --- | --- |
| `schema_version`, `label`, `status` | 格式版本、调用者标签、完成状态。 |
| `started_at`, `finished_at`, `elapsed_s` | UTC 时间与总耗时（含清理，不含元数据预检）。 |
| `host` | 系统、内核、架构、型号、逻辑核数、物理内存字节数。 |
| `binary`, `snapshot` | 文件名、二进制 SHA256、快照字节数；无输入绝对路径。 |
| `health`, `startup_to_health_ms`, `snapshot_copy_s` | Hub 版本、build_commit、local_node_enabled、启动时间与复制时间。 |
| `config` | 合并默认值后的完整配置，可用于复跑。 |
| `load_average_before`, `load_average_after`, `cpu_top` | 前后系统负载，以及采样期间的进程名/CPU% 峰值。 |
| `run_directory` | `--work` 下本次副本目录名；日志入口是该目录的 `logs/hub.log`。 |
| `endpoints[path].cold` | `ms/status/bytes/error`。 |
| `endpoints[path].warm` | `latency_ms/response_bytes` 分布、`total_response_bytes/status_codes/errors`。 |
| `idle`, `load` | `target_s/elapsed_s`、`lock/disk/worker_errors`。 |
| `lock` | 探针数、忙样本数、`busy_ratio`（0–1）、`busy_segments_ms` 分布、`segments_over_1s`、错误分类计数。 |
| `disk` | `available/method`、每进程明细、采样错误；`device_bytes/logical_bytes` 总量和 `device_Bps/logical_Bps`。 |
| `load.reads`, `load.writes` | 与 warm 同形的请求统计；同级另给读取/写入/成功读取每秒吞吐和总错误数。 |
| `load.cli` | 每次 `ms/returncode/timed_out`、耗时分布及错误数；负载太短未完成全部 CLI 次数也视为测量错误。 |
| `cleanup`, `failure_type` | 残留进程组清单，以及失败时的异常类型。 |

所有分布均含 `count/p50/p90/p99/max`，单位由字段名决定。`compare.py` 默认输出 Markdown，`--md` 是显式等价选项；遍历测量字段并列出 A/B 的并集，变化为 `(B-A)/abs(A)*100%`。缺失值与零基线增长显示 n/a，零到零为 0%。PID、配置、时间戳、哈希不做数值比较；进程按名称匹配。负值只表示下降，是否改善取决于指标（如吞吐下降通常较差）。
