#!/usr/bin/env python3
r"""Repeatable, isolated Hub benchmark. Python 3.11+, standard library only.

Hub benchmark

Python 3.11+，仅标准库。将同一静态 SQLite 快照与不同的 Hub 构建配对运行；每次在 `--work` 下新建私有的 `run-*` 目录，保留副本和日志以便排查。源快照只通过文件复制读取，不能传入仍有活动 WAL 的数据库文件。

```sh
python3 -I scripts/reform/bench/run.py \
  --hub-bin /path/to/steve-hub --snapshot /path/to/snapshot.sqlite3 \
  --work /path/to/isolated-bench --port 18770 \
  --out /path/to/results/before.json --label before
python3 -I scripts/reform/bench/compare.py before.json after.json --md
python3 -I -m unittest discover -s scripts/reform/bench -v
```

`--config custom.json` 覆盖内置默认值的顶层键，`endpoints` 整体替换；默认配置也在 `default.json`。默认每个接口首次请求加 20 次串行请求，启动后稳定采样 30 秒（settle_s）、空闲 30 秒、4 客户端负载 60 秒、每秒 2 次通知写入、负载中运行 CLI 3 次。540 秒总预算包含复制、启动和测量，清理在预算外；正常运行应低于十分钟。慢请求或错误会进入结果，不自动重试。退出码 0 表示采集完成且请求、CLI、探针没有错误；2 表示采集完成但存在测量错误；1 表示运行失败或清理失败。失败时尽量保存部分 JSON。

T0.3 独立负载

`--workload read|write` 或配置 `workload` 选择纯 GET/POST，并强制 `invalidate_rps=0`；未指定时保留旧混合模式。写默认 `/api/notifications/read`、`load_body={"at":1}`，连续两次 200/`{"seen":1}` 冒烟后预热；`warmup_s=5` 使用相同负载且不纳入计时。`auth="bearer"` 在启动前用隔离 CLI 创建短期 owner token，并经 `/auth/token` 换凭据、`/api/me` 取 workspace；凭据只在内存。默认仍为 cookie。`cli_runs=0` 可关闭竞争 CLI；`bench/metrics.json` 是正式指标测量配置。A/B 与抓取入口见 `ab.py --help`。

隔离与生命周期

Hub 和 CLI 都通过 `/usr/bin/env -i` 和相同的干净环境启动，显式设置隔离的 `STEVE_HOME`、Hub 端口、本地 Node 端口（Hub + 3），禁用恢复、备份、适配器更新，所有 17 个适配器指向 `/usr/bin/false`。另设私有 `HOME`，不读取当前用户的登录配置。启动点对隔离环境做断言。不要用被测二进制的 `--help` 或 `--version` 探测能力：该构建遇到未知参数会启动 Hub。版本从 `/healthz` 获取。

工具拒绝当前用户的 `~/.steve*` 目录及其子目录（解析符号链接后判断），拒绝 Hub/Node 端口 8440，并检查两个端口没有被占用。只修改快照副本中最早 owner 的 subject 为 `dev:local`；通过 `/auth/dev` 获取内存 cookie。HTTP 不使用环境代理，不跟随重定向，不保存 cookie 或响应内容。CLI 使用被测二进制的 `--access token create --name … --json`，输出丢弃，token 只留在隔离数据库内。

Hub 会给本地 Node 调用 `setsid()`，因此清理不能只杀 Hub 进程组。退出、异常、SIGINT、SIGTERM、总超时都会冻结父进程、发现子树并结束它们的所有进程组；Hub 已退出时，从本次新 home 的 `data/node/service.pid` 找到 Node，并核对它的隔离环境后清理。SIGKILL 无法捕获。工具不删除旧目录，每次重跑都建立新副本，不复用被测数据库。

测量方法

| 项目 | 方法与含义 |
| --- | --- |
| 启动 | 从启动子进程到首次成功读取 `/healthz` 的时间；不含快照复制和 owner 修改。 |
| 启动后稳定期 | 登录后先运行 settle_s 秒，磁盘与锁探针单独记入 post_start，然后测接口、idle、load；不把这段启动后写入算为空闲。固定等待时间不能保证所有后台任务已结束。 |
| 接口 | 每个接口第一次调用作为 cold，随后串行 N 次作为 warm；cold 指本次脚本首次访问，不保证操作系统缓存冷、也不保证其他接口没预热共享数据。 |
| 响应字节 | 完整读取响应后的字节数，发送 `Accept-Encoding: gzip`，不自动解压，响应是否压缩以记录的 Content-Encoding 为准，不含 HTTP 头。请求记录 accept_encoding，响应记录 content_encoding；缺少响应头时记 identity，未收到响应时为 null。串行/负载汇总的 accept_encodings/content_encodings 按编码计数，未收到响应计为 unavailable。 |
| 延迟 | `perf_counter` 测量请求到读完响应；分位数将排序数据的 `(n-1)*p` 位置线性插值。空集为 null；超时和 HTTP 错误也纳入分布，并单独计错。状态 0 表示未得到 HTTP 响应。 |
| 并发 | K 个独立客户端持续 GET；单独写线程按目标频率 POST，不补发错过的时间槽。吞吐用实际阶段时间（含在途请求排空）作分母；同时给出成功读取吞吐和实际写入频率。 |
| 失效 | 默认 POST `/api/notifications/read`，请求体 `{}`。已核实基线源码 `api/mod.rs` 路由、`api/misc.rs::notifications_read` → `notify.rs::mark_seen` 发布 `HubEvent::Settings`；`bus.rs::publish_value` 将非 STATELESS 的全局事件计入 state generation。未来构建改变该契约时通过配置显式换写接口，并记录基线。 |
| 写锁 | 同一副本上独立 SQLite 连接，每 5 ms 执行零等待 `BEGIN IMMEDIATE` 并立即 `ROLLBACK`。只将 SQLITE_BUSY/LOCKED 算忙；其他错误单独报告。忙比例是忙样本/有效样本；连续忙样本形成忙段，其时长是采样估算，受调度、探针自身拿锁影响，首尾段可能截断，不代表精确事务持锁时间。 |
| CLI | 在负载阶段均匀安排真实 CLI；每次记录启动到退出的耗时、返回码和超时标记。它包括建库检查和 CLI 工作的总时间，是拿锁体验的代理指标，不是纯锁等待时间。单次不超出 CLI 超时或剩余负载时间。 |
| 磁盘 | Darwin `proc_pid_rusage` v4 的设备写入和逻辑写入计数。每秒重新发现 Hub 全部后代，含独立 session 的 Node，各阶段基线作差并给每进程与合计值；新子进程计入首次观察到的生命周期计数。退出子进程最后一次采样之后的写入不可见，短命子进程可能漏采；`present_at_end` 和 `sampling_errors` 辅助识别缺口。非 Darwin 明确标为 unavailable/null。CLI 为单独测量进程，不纳入 Hub 后代磁盘合计。 |
| 主机负载 | 前后调用 `uptime`，只保存三个 load average；采样 `ps` 的 CPU%，仅保存进程名和峰值 CPU%，不保存命令参数或主机名。`ps` CPU% 是操作系统提供的平均口径，可能超过 100%。 |

基线构建在 `STEVE_NO_RESUME=1` 下禁用本地 Node，因此该模式的实测磁盘明细可能只有 Hub；`health.local_node_enabled` 保留这个事实，不能把它解释成 Node 零写入。磁盘实现会采集实际存在的 Hub 后代。

WAL 提交速率为可选项，本工具未采集。总吞吐与磁盘字节主要看合计，进程明细可能因进程重启而不可逐行比较。探针、CLI、CPU 观察本身会干扰负载，比较时应保留相同配置，并检查前后系统负载。

JSON 字段

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
| `endpoints[path].cold` | `ms/status/bytes/error/accept_encoding/content_encoding`。 |
| `endpoints[path].warm` | `latency_ms/response_bytes` 分布、`total_response_bytes/status_codes/errors/accept_encodings/content_encodings`。 |
| `post_start`, `idle`, `load` | `target_s/elapsed_s`、`lock/disk/worker_errors`。 |
| `lock` | 探针数、忙样本数、`busy_ratio`（0–1）、`busy_segments_ms` 分布、`segments_over_1s`、错误分类计数。 |
| `disk` | `available/method`、每进程明细、采样错误；`device_bytes/logical_bytes` 总量和 `device_Bps/logical_Bps`。 |
| `load.reads`, `load.writes` | 与 warm 同形的请求统计；同级另给读取/写入/成功读取每秒吞吐和总错误数。 |
| `load.cli` | 每次 `ms/returncode/timed_out`、耗时分布及错误数；负载太短未完成全部 CLI 次数也视为测量错误。 |
| `cleanup`, `failure_type` | 残留进程组清单，以及失败时的异常类型。 |

所有分布均含 `count/p50/p90/p99/max`，单位由字段名决定。`compare.py` 默认输出 Markdown，`--md` 是显式等价选项；遍历测量字段并列出 A/B 的并集，变化为 `(B-A)/abs(A)*100%`。缺失值与零基线增长显示 n/a，零到零为 0%。PID、配置、时间戳、哈希不做数值比较；进程按名称匹配。表头列出两边的 load_average_before/after 和 cpu_top 前三名，并提示负载不同时性能数字不可直接比较。负值只表示下降，是否改善取决于指标（如吞吐下降通常较差）。
"""
import sys

if sys.version_info < (3, 11):
    raise SystemExit("Python 3.11+ is required")

import argparse
import collections
from contextlib import closing
import ctypes
import hashlib
import http.cookiejar
import http.client
import importlib.util
import json
import math
import os
from pathlib import Path
import platform
import shutil
import signal
import socket
import sqlite3
import subprocess
import tempfile
import threading
import time
import urllib.error
import urllib.request

_spec = importlib.util.spec_from_file_location("bench_isolation", Path(__file__).with_name("isolation.py"))
isolation = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(isolation)

DEFAULT = {
    "endpoints": [{"path": p, "requests": 20} for p in (
        "/api/state", "/api/stats/summary", "/api/hub-metadata", "/api/personas",
        "/api/spaces", "/api/channels", "/api/me/attention", "/api/devices")],
    "workload": None,  # None preserves the original mixed GET + invalidation workload.
    "auth": "cookie",
    "warmup_s": 5,
    "load_body": {"at": 1},
    "request_timeout_s": 10,
    "startup_timeout_s": 90,
    "total_timeout_s": 540,
    "settle_s": 30,
    "idle_s": 30,
    "load_s": 60,
    "clients": 4,
    "load_path": "/api/state",
    "invalidate_rps": 2,
    "invalidate_path": "/api/notifications/read",
    "invalidate_body": {},
    "lock_interval_ms": 5,
    "process_interval_s": 1,
    "cpu_top_n": 8,
    "cli_runs": 3,
    "cli_timeout_s": 10,
}


def distribution(values):
    """Linear interpolation at (n-1)*p, like statistics.quantiles inclusive."""
    ordered = sorted(values)
    def q(p):
        if not ordered:
            return None
        index = (len(ordered) - 1) * p
        lo, hi = math.floor(index), math.ceil(index)
        return ordered[lo] + (ordered[hi] - ordered[lo]) * (index - lo)
    return {"count": len(ordered), "p50": q(.5), "p90": q(.9),
            "p99": q(.99), "max": ordered[-1] if ordered else None}


def configuration(path, workload=None):
    config = json.loads(json.dumps(DEFAULT))
    if path:
        override = json.loads(path.read_text())
        if not isinstance(override, dict) or override.keys() - config.keys():
            raise ValueError("config must be an object containing known keys")
        config.update(override)
    for key in ("request_timeout_s", "startup_timeout_s", "total_timeout_s", "settle_s", "idle_s",
                "load_s", "warmup_s", "lock_interval_ms", "process_interval_s", "cli_timeout_s"):
        value = config[key]
        if isinstance(value, bool) or not isinstance(value, (int, float)) or not math.isfinite(value) or value <= 0:
            raise ValueError(f"{key} must be finite and positive")
    for key in ("clients", "cpu_top_n", "cli_runs"):
        if type(config[key]) is not int or config[key] < (0 if key == "cli_runs" else 1):
            raise ValueError(f"{key} must be a positive integer")
    configure_workload(config, workload)
    rate = config["invalidate_rps"]
    if isinstance(rate, bool) or not isinstance(rate, (int, float)) or not math.isfinite(rate) or rate < 0:
        raise ValueError("invalidate_rps must be finite and nonnegative")
    def api_path(value):
        return isinstance(value, str) and value.startswith("/api/") and not any(c in value for c in "\r\n#")
    if not api_path(config["load_path"]) or not api_path(config["invalidate_path"]):
        raise ValueError("load and invalidation paths must start with /api/")
    if not isinstance(config["endpoints"], list) or not config["endpoints"]:
        raise ValueError("endpoints must be a nonempty list")
    paths = []
    for item in config["endpoints"]:
        if not isinstance(item, dict) or set(item) != {"path", "requests"} or not api_path(item["path"]):
            raise ValueError("endpoint needs path and requests")
        if type(item["requests"]) is not int or item["requests"] < 1:
            raise ValueError("endpoint requests must be a positive integer")
        paths.append(item["path"])
    if len(set(paths)) != len(paths):
        raise ValueError("duplicate endpoint paths")
    return config


def configure_workload(config, workload=None):
    if workload is not None:
        config["workload"] = workload
    if config["workload"] not in (None, "read", "write") or config["auth"] not in ("cookie", "bearer"):
        raise ValueError("workload must be read/write; auth must be cookie/bearer")
    if not isinstance(config["load_body"], dict):
        raise ValueError("load_body must be a JSON object")
    if config["workload"] is not None:
        config["invalidate_rps"] = 0
    if config["workload"] == "write" and config["load_path"] == DEFAULT["load_path"]:
        config["load_path"] = "/api/notifications/read"
    return config


def access_token(processes, binary, env, timeout):
    # CLI is just as dangerous as a server launch: Processes.start asserts isolation.
    child = processes.start([str(binary), "--access", "token", "create", "--name",
                             "metrics-acceptance", "--days", "1", "--json"], env,
                            stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)
    try:
        output, _ = child.communicate(timeout=timeout)
        if child.returncode:
            raise RuntimeError("isolated access token creation failed")
        value = json.loads(output)
        return {key: value[key] for key in ("user", "token")}
    except subprocess.TimeoutExpired:
        processes.kill_group(child, signal.SIGKILL)
        child.communicate(timeout=5)
        raise TimeoutError("isolated token CLI timed out") from None


def warmup(hub, db, client, config, env, processes, binary):
    if config["workload"] == "write":
        for _ in range(2):
            if client.json_request(config["load_path"], config["load_body"]) != {"seen": 1}:
                raise RuntimeError("write smoke must return 200 and seen=1 twice")
    result = phase("warmup", config["warmup_s"], hub, db, client, config, env, processes, binary)
    if not result["requests"] or result["errors"] or result["worker_errors"] or result["lock"]["probe_errors"]:
        raise RuntimeError("warmup failed")
    return {"elapsed_s": result["elapsed_s"], "requests": result["requests"]}


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None


class Client:
    def __init__(self, base, timeout, cookie=""):
        self.base, self.timeout, self.cookie = base, timeout, cookie
        self.credential, self.workspace = "", ""
        # Do not inherit proxy settings, or follow redirects into another service.
        self.opener = urllib.request.build_opener(urllib.request.ProxyHandler({}), NoRedirect())

    def headers(self):
        headers = {"Cookie": self.cookie, "Accept-Encoding": "gzip", "Origin": self.base}
        if self.credential:
            headers["Authorization"] = "Bearer " + self.credential
        if self.workspace:
            headers["X-Steve-Ws"] = self.workspace
        return headers

    def fork(self):
        local = Client(self.base, self.timeout, self.cookie)
        local.credential, local.workspace = self.credential, self.workspace
        return local

    def json_request(self, path, payload=None):
        headers = self.headers() | {"Accept-Encoding": "identity"}
        if payload is not None:
            headers["Content-Type"] = "application/json"
        req = urllib.request.Request(self.base + path,
            data=None if payload is None else json.dumps(payload).encode(), headers=headers)
        with self.opener.open(req, timeout=self.timeout) as response:
            if response.status != 200:
                raise RuntimeError("JSON request must return 200")
            return json.loads(response.read())

    def request(self, path, data=None):
        headers = self.headers()
        if data is not None:
            headers["Content-Type"] = "application/json"
        req = urllib.request.Request(self.base + path, data=data, headers=headers)
        start = time.perf_counter()
        code, size, error, content_encoding = 0, 0, None, None
        try:
            with self.opener.open(req, timeout=self.timeout) as response:
                content_encoding = response.headers.get("Content-Encoding", "identity")
                code, body = response.status, response.read()
                size = len(body)
        except urllib.error.HTTPError as exc:
            content_encoding = exc.headers.get("Content-Encoding", "identity")
            code, size, error = exc.code, len(exc.read()), "http"
        except (OSError, urllib.error.URLError, TimeoutError, http.client.HTTPException) as exc:
            error = type(exc).__name__
        return {"ms": (time.perf_counter() - start) * 1000,
                "status": code, "bytes": size, "error": error,
                "accept_encoding": headers["Accept-Encoding"], "content_encoding": content_encoding}

    def login(self, token=None):
        if token is not None:
            payload = token | {"client": {"kind": "cli", "name": "metrics-acceptance",
                                          "platform": "macOS", "version": "1"}}
            self.credential = self.json_request("/auth/token", payload)["credential"]
            self.workspace = self.json_request("/api/me")["workspace"]
            if not self.credential or not isinstance(self.workspace, str) or not self.workspace:
                raise RuntimeError("owner login missing credential/workspace")
            return
        jar = http.cookiejar.CookieJar()
        opener = urllib.request.build_opener(urllib.request.ProxyHandler({}),
                                             urllib.request.HTTPCookieProcessor(jar), NoRedirect())
        try:
            opener.open(self.base + "/auth/dev", timeout=self.timeout).close()
        except urllib.error.HTTPError as exc:
            if exc.code not in (302, 303, 307):
                raise RuntimeError("dev login failed") from None
            exc.close()
        self.cookie = "; ".join(f"{c.name}={c.value}" for c in jar)
        if not self.cookie:
            raise RuntimeError("dev login returned no cookie")


def requests_summary(rows):
    return {"latency_ms": distribution([r["ms"] for r in rows]),
            "response_bytes": distribution([r["bytes"] for r in rows]),
            "total_response_bytes": sum(r["bytes"] for r in rows),
            "status_codes": dict(collections.Counter(str(r["status"]) for r in rows)),
            "accept_encodings": dict(collections.Counter(r["accept_encoding"] for r in rows)),
            "content_encodings": dict(collections.Counter(r["content_encoding"] if r["content_encoding"] is not None
                                                         else "unavailable" for r in rows)),
            "errors": sum(r["error"] is not None for r in rows)}


class LockProbe:
    def __init__(self, db, interval):
        self.db, self.interval = db, interval
        self.busy = self.free = 0
        self.streaks, self.errors = [], collections.Counter()
        self.started = None

    def sample(self, stop, deadline):
        with closing(sqlite3.connect(self.db, timeout=0, isolation_level=None)) as conn:
            conn.execute("PRAGMA busy_timeout=0")
            while not stop.is_set() and time.perf_counter() < deadline:
                now = time.perf_counter()
                try:
                    conn.execute("BEGIN IMMEDIATE")
                    conn.execute("ROLLBACK")
                    self.free += 1
                    if self.started is not None:
                        self.streaks.append((now - self.started) * 1000)
                        self.started = None
                except sqlite3.OperationalError as exc:
                    # BUSY/LOCKED only: corruption and I/O errors are not lock occupancy.
                    if (getattr(exc, "sqlite_errorcode", 0) & 255) in (5, 6):
                        self.busy += 1
                        if self.started is None:
                            self.started = now
                    else:
                        self.errors[type(exc).__name__] += 1
                stop.wait(self.interval)
        if self.started is not None:
            self.streaks.append((time.perf_counter() - self.started) * 1000)

    def result(self):
        total = self.busy + self.free
        return {"probes": total, "busy_probes": self.busy,
                "busy_ratio": self.busy / total if total else None,
                "busy_segments_ms": distribution(self.streaks),
                "segments_over_1s": sum(v > 1000 for v in self.streaks),
                "probe_errors": dict(self.errors)}


U64 = ctypes.c_uint64


class RUsage(ctypes.Structure):
    # Darwin rusage_info_v4; the layout matches libproc's public ABI.
    _fields_ = [("uuid", ctypes.c_uint8 * 16)] + [(name, U64) for name in (
        "user", "system", "pkg_idle", "intr", "pageins", "wired", "resident", "phys",
        "start", "exit", "c_user", "c_system", "c_pkg_idle", "c_intr", "c_pageins", "c_elapsed",
        "dread", "dwritten", "q1", "q2", "q3", "q4", "q5", "q6", "q7", "billed_sys",
        "serviced_sys", "logical_writes", "lifetime_max_phys", "instr", "cycles", "billed_e",
        "serviced_e", "interval_max_phys", "runnable")]


def process_table():
    raw = subprocess.check_output(["ps", "-axo", "pid=,ppid=,pcpu=,comm="], text=True, timeout=5)
    rows = []
    for line in raw.splitlines():
        bits = line.strip().split(None, 3)
        if len(bits) == 4:
            try:
                rows.append((int(bits[0]), int(bits[1]), float(bits[2]), Path(bits[3]).name))
            except ValueError:
                continue
    return rows


def live_groups(groups):
    # Darwin may return EPERM to killpg(..., 0) while a detached child is
    # being reaped. Inspect non-zombie members rather than treating that race
    # as a surviving process or trying to signal an already dead process.
    raw = subprocess.check_output(["ps", "-axo", "pgid=,stat="], text=True, timeout=5)
    return {int(bits[0]) for line in raw.splitlines()
            if len(bits := line.split()) == 2 and int(bits[0]) in groups and not bits[1].startswith("Z")}


class DiskMeter:
    def __init__(self, pid):
        self.pid, self.records, self.errors = pid, {}, collections.Counter()
        self.lib = None
        if sys.platform == "darwin":
            self.lib = ctypes.CDLL("/usr/lib/libproc.dylib", use_errno=True)
            self.lib.proc_pid_rusage.argtypes = [ctypes.c_int, ctypes.c_int, ctypes.c_void_p]
            self.lib.proc_pid_rusage.restype = ctypes.c_int

    def sample(self, table, baseline=False):
        if self.lib is None:
            return
        descendants = {self.pid}
        while True:
            found = {p for p, parent, _, _ in table if parent in descendants}
            if found <= descendants:
                break
            descendants.update(found)
        names = {p: name for p, _, _, name in table}
        for pid in descendants:
            r = RUsage()
            if self.lib.proc_pid_rusage(pid, 4, ctypes.byref(r)) != 0:
                self.errors["rusage_unavailable"] += 1
                continue
            key = (pid, r.start)
            current = (r.dwritten, r.logical_writes)
            if key not in self.records:
                # A child born after phase start contributes its lifetime counters.
                self.records[key] = {"pid": pid, "name": names.get(pid, "unknown"),
                                     "first": current if baseline else (0, 0),
                                     "last": current, "present_at_end": False}
            self.records[key]["last"] = current
            self.records[key]["present_at_end"] = True
        for key, record in self.records.items():
            if key[0] not in descendants:
                record["present_at_end"] = False

    def result(self, seconds):
        rows = []
        for record in self.records.values():
            device, logical = [max(0, b - a) for a, b in zip(record["first"], record["last"])]
            rows.append({k: record[k] for k in ("pid", "name", "present_at_end")} |
                        {"device_bytes": device, "logical_bytes": logical,
                         "device_Bps": device / seconds, "logical_Bps": logical / seconds})
        available = self.lib is not None and bool(rows)
        return {"available": available, "method": "proc_pid_rusage_v4" if self.lib else "unsupported",
                "processes": rows, "sampling_errors": dict(self.errors),
                "device_bytes": sum(r["device_bytes"] for r in rows) if available else None,
                "logical_bytes": sum(r["logical_bytes"] for r in rows) if available else None,
                "device_Bps": sum(r["device_Bps"] for r in rows) if available else None,
                "logical_Bps": sum(r["logical_Bps"] for r in rows) if available else None}


class Processes:
    def __init__(self):
        self.children = []
        self.lock = threading.Lock()
        self.home = None

    def start(self, args, env, **kwargs):
        # No inherited environment is ever passed to the binary, including CLI.
        assert "STEVE_HOME" in env and "STEVE_PORT" in env
        assert safe_work(Path(env["STEVE_HOME"])) == Path(env["STEVE_HOME"])
        assert 8440 not in (int(env["STEVE_PORT"]), int(env["STEVE_LOCAL_NODE_PORT"]))
        assert env["STEVE_NO_RESUME"] == "1" and env["STEVE_BACKUP"] == "off"
        # a snapshot copy has production's workspace ids: a keychain keystore would share their entries
        assert env.get("STEVE_VAULT_KEYSTORE") == "file"
        assert "STEVE_CONFIG" not in env and "INVOCATION_ID" not in env
        assert all(env.get(k) == v for k, v in isolation.DISABLED_ENV.items())
        self.home = Path(env["STEVE_HOME"])
        with self.lock:
            child = subprocess.Popen(["/usr/bin/env", "-i", *[f"{k}={v}" for k, v in env.items()], *args],
                                     env={}, start_new_session=True, **kwargs)
            self.children.append(child)
        return child

    @staticmethod
    def kill_group(child, sig):
        try:
            os.killpg(child.pid, sig)
        except ProcessLookupError:
            pass

    def cleanup(self):
        with self.lock:
            roots = {p.pid for p in self.children if p.poll() is None}
            # Local Node deliberately calls setsid(), so the Hub group alone is
            # insufficient. The fresh home's PID file survives Hub failure. Verify
            # its inherited isolation variables before accepting an orphan's PID.
            if self.home:
                try:
                    pid = int((self.home / "data/node/service.pid").read_text())
                    command = subprocess.check_output(
                        ["ps", "eww", "-p", str(pid), "-o", "command="], text=True, timeout=5)
                    if "--local-node" in command and f"STEVE_HOME={self.home} " in command:
                        roots.add(pid)
                except (OSError, ValueError, subprocess.SubprocessError):
                    pass
            groups = {p.pid for p in self.children}
            frozen = set()
            # Freeze parents before enumerating descendants, preventing respawn
            # between tree discovery and termination (including detached sessions).
            while roots - frozen:
                for pid in roots - frozen:
                    try:
                        os.kill(pid, signal.SIGSTOP)
                        group = os.getpgid(pid)
                        if group != os.getpgrp():
                            groups.add(group)
                    except (ProcessLookupError, PermissionError):
                        pass
                    frozen.add(pid)
                table = process_table()
                roots.update(pid for pid, parent, _, _ in table if parent in roots)
            def send(group, sig):
                try:
                    os.killpg(group, sig)
                except (ProcessLookupError, PermissionError):
                    pass
            for group in groups:
                send(group, signal.SIGTERM)
                send(group, signal.SIGCONT)
            limit = time.perf_counter() + 3
            remaining = set(groups)
            while remaining and time.perf_counter() < limit:
                for child in self.children:
                    child.poll()
                remaining = live_groups(remaining)
                if remaining:
                    time.sleep(.05)
            for group in remaining:
                send(group, signal.SIGKILL)
            for child in self.children:
                child.wait(timeout=5)
            # Detached Nodes are reaped by the OS, not Popen.wait().
            for _ in range(50):
                remaining = live_groups(remaining)
                if not remaining:
                    break
                time.sleep(.05)
            return {"process_groups_remaining": sorted(remaining)}


def phase(name, seconds, hub, db, client, config, env, processes, binary,
          samples=None, cpu_meter=None):
    start = time.perf_counter()
    deadline = start + seconds
    stop = threading.Event()
    probe = LockProbe(db, config["lock_interval_ms"] / 1000)
    disk = DiskMeter(hub.pid)
    table = process_table()
    disk.sample(table, baseline=True)
    if cpu_meter is not None:
        cpu_meter.sample(table, baseline=True)
    reads, writes, cli, thread_errors = [], [], [], []
    def get_loop():
        local = client.fork()
        while not stop.is_set() and time.perf_counter() < deadline:
            body = json.dumps(config["load_body"]).encode() if config["workload"] == "write" else None
            (writes if body is not None else reads).append(local.request(config["load_path"], body))
    def invalidate_loop():
        local = client.fork()
        rate, tick = config["invalidate_rps"], start
        body = json.dumps(config["invalidate_body"]).encode()
        while rate > 0 and not stop.is_set() and tick < deadline:
            if stop.wait(max(0, tick - time.perf_counter())) or time.perf_counter() >= deadline:
                break
            writes.append(local.request(config["invalidate_path"], body))
            # Skip missed slots: slow writes must not cause a catch-up burst.
            tick = max(tick + 1 / rate, time.perf_counter())
    def cli_loop():
        for index in range(config["cli_runs"]):
            target = start + seconds * (index + 1) / (config["cli_runs"] + 1)
            if stop.wait(max(0, target - time.perf_counter())) or time.perf_counter() >= deadline:
                break
            before = time.perf_counter()
            child = processes.start([str(binary), "--access", "token", "create", "--name",
                                     f"bench-{index}", "--json"], env,
                                    stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            timed_out = False
            try:
                child.wait(timeout=min(config["cli_timeout_s"], max(.001, deadline - before)))
            except subprocess.TimeoutExpired:
                timed_out = True
                processes.kill_group(child, signal.SIGKILL)
                child.wait(timeout=5)
            cli.append({"ms": (time.perf_counter() - before) * 1000,
                        "returncode": child.returncode, "timed_out": timed_out})
    def guarded(fn):
        try:
            fn()
        except Exception as exc:
            thread_errors.append(type(exc).__name__)
            stop.set()
    funcs = [lambda: probe.sample(stop, deadline)]
    if name in ("load", "warmup"):
        funcs += [get_loop] * config["clients"] + [invalidate_loop]
        if name == "load":
            funcs += [cli_loop]
    threads = [threading.Thread(target=guarded, args=(fn,), daemon=True) for fn in funcs]
    try:
        for thread in threads:
            thread.start()
        while time.perf_counter() < deadline and not stop.is_set():
            if hub.poll() is not None:
                raise RuntimeError("Hub exited during measurement")
            table = process_table()
            disk.sample(table)
            stop.wait(min(config["process_interval_s"], max(0, deadline - time.perf_counter())))
    finally:
        stop.set()
        for thread in threads:
            thread.join(timeout=config["request_timeout_s"] + 6)
    duration = time.perf_counter() - start
    table = process_table()
    disk.sample(table)
    result = {"target_s": seconds, "elapsed_s": duration, "lock": probe.result(),
              "disk": disk.result(duration), "worker_errors": thread_errors}
    if name in ("load", "warmup"):
        workload_rows = writes if config["workload"] == "write" else reads
        result["requests"] = len(workload_rows)
        result["requests_per_s"] = len(workload_rows) / duration
        if samples is not None:
            samples["reads_ms"] = [r["ms"] for r in reads]
            samples["workload_ms"] = [r["ms"] for r in workload_rows]
            samples["workload"] = config["workload"] or "mixed"
        if cpu_meter is not None:
            cpu_meter.sample(table)
            result["cpu"] = cpu_meter.result(len(reads) + len(writes))
        result.update({"reads": requests_summary(reads), "writes": requests_summary(writes),
                       "read_requests_per_s": len(reads) / duration,
                       "write_requests_per_s": len(writes) / duration,
                       "successful_reads_per_s": sum(r["error"] is None for r in reads) / duration,
                       "errors": sum(r["error"] is not None for r in reads + writes),
                       "cli": {"runs": cli, "latency_ms": distribution([r["ms"] for r in cli]),
                               "errors": sum(r["returncode"] != 0 for r in cli)}})
    return result


def load_average():
    # uptime is read for the requested source, but never save its user/host details.
    raw = subprocess.check_output(["uptime"], text=True, timeout=5)
    import re
    match = re.search(r"load averages?:\s*(.*)", raw)
    if not match:
        raise RuntimeError("uptime has no load averages")
    return [float(x) for x in re.findall(r"\d+(?:\.\d+)?", match[1])[:3]]


def host_info():
    info = {"system": platform.system(), "release": platform.release(),
            "architecture": platform.machine(), "logical_cpus": os.cpu_count(),
            "model": None, "memory_bytes": None}
    if sys.platform == "darwin":
        for key, sysctl in (("model", "hw.model"), ("memory_bytes", "hw.memsize")):
            value = subprocess.check_output(["sysctl", "-n", sysctl], text=True, timeout=5).strip()
            info[key] = int(value) if key == "memory_bytes" else value
    else:
        info["model"] = platform.machine()
        try:
            info["memory_bytes"] = os.sysconf("SC_PAGE_SIZE") * os.sysconf("SC_PHYS_PAGES")
        except (ValueError, OSError):
            pass
    return info


def safe_work(path):
    path = path.expanduser().resolve()
    user_home = Path.home().resolve()
    # Reject production homes (including symlink aliases) and their descendants.
    if path == user_home or path == Path("/") or any(
            p.parent == user_home and p.name.startswith(".steve") for p in (path, *path.parents)):
        raise ValueError("work directory must be outside production homes")
    return path


def isolated_environment(home, port):
    # A private HOME also isolates any future subprocess's default config discovery.
    private_home = home / "user-home"
    private_home.mkdir()
    (home / "web").mkdir()
    return isolation.DISABLED_ENV | {"STEVE_WEB_DIR": str(home / "web"), "HOME": str(private_home), "USER": "benchmark", "PATH": "/usr/bin:/bin:/usr/sbin:/sbin",
            "TMPDIR": str(home / "tmp"), "STEVE_HOME": str(home), "STEVE_HOST": "127.0.0.1",
            "STEVE_PORT": str(port), "STEVE_LOCAL_NODE_PORT": str(port + 3),
            "STEVE_PUBLIC_URL": f"http://127.0.0.1:{port}", "STEVE_DEV_LOGIN": "1",
            "STEVE_NO_RESUME": "1", "STEVE_VAULT_KEYSTORE": "file", "STEVE_BACKUP": "off",
            "STEVE_ADAPTER_UPDATES": "off",
            "NO_COLOR": "1", "RUST_LOG": "warn"}


def health(client, hub, timeout):
    start = time.perf_counter()
    while time.perf_counter() - start < timeout:
        if hub.poll() is not None:
            raise RuntimeError("Hub exited before health check")
        try:
            with client.opener.open(client.base + "/healthz", timeout=min(2, timeout)) as response:
                if response.status == 200:
                    value = json.loads(response.read())
                    # Whitelist fields: health payloads may grow private diagnostics later.
                    return {key: value.get(key) for key in ("version", "build_commit")} | {
                        "local_node_enabled": (value.get("node") or {}).get("enabled")}
        except (OSError, urllib.error.URLError, ValueError):
            pass
        time.sleep(.1)
    raise TimeoutError("Hub startup timeout")


def write_result(path, result):
    path.parent.mkdir(parents=True, exist_ok=True)
    temp = path.with_name(path.name + ".tmp")
    temp.write_text(json.dumps(result, indent=2, ensure_ascii=False, allow_nan=False) + "\n")
    temp.replace(path)


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    for flag in ("hub-bin", "snapshot", "work", "out"):
        parser.add_argument("--" + flag, required=True, type=Path)
    parser.add_argument("--port", required=True, type=int)
    parser.add_argument("--config", type=Path)
    parser.add_argument("--label", default="")
    parser.add_argument("--workload", choices=("read", "write"))
    args = parser.parse_args(argv)
    os.umask(0o077)
    config = configuration(args.config, args.workload)
    work = safe_work(args.work)
    if not 1024 <= args.port <= 65532 or 8440 in (args.port, args.port + 3):
        parser.error("unsafe Hub/Node port")
    # Both ports must be free; never authenticate against an existing process.
    for port in (args.port, args.port + 3):
        with socket.socket() as sock:
            sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
            sock.bind(("127.0.0.1", port))
    binary, snapshot, out = [p.expanduser().resolve() for p in (args.hub_bin, args.snapshot, args.out)]
    if not binary.is_file() or not os.access(binary, os.X_OK) or not snapshot.is_file():
        parser.error("binary must be executable and snapshot must exist")
    safe_work(out)
    if out in (binary, snapshot) or out.is_relative_to(Path(__file__).resolve().parent):
        parser.error("output must not overwrite inputs or be in tool directory")
    with binary.open("rb") as stream:
        binary_sha256 = hashlib.file_digest(stream, "sha256").hexdigest()
    processes, cpu_peaks = Processes(), {}
    result = {"schema_version": 1, "label": args.label, "config": config,
              "started_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
              "host": host_info(), "load_average_before": load_average(),
              "binary": {"name": binary.name, "sha256": binary_sha256},
              "snapshot": {"name": snapshot.name, "size_bytes": snapshot.stat().st_size},
              "status": "running"}
    started = time.perf_counter()
    cpu_stop = threading.Event()
    def observe_cpu():
        while not cpu_stop.is_set():
            try:
                for _, _, cpu, name in process_table():
                    cpu_peaks[name] = max(cpu, cpu_peaks.get(name, 0))
            except (OSError, subprocess.SubprocessError):
                pass
            cpu_stop.wait(config["process_interval_s"])
    cpu_thread = threading.Thread(target=observe_cpu, daemon=True)
    cpu_thread.start()
    def interrupted(signum, frame):
        raise TimeoutError("total benchmark timeout") if signum == signal.SIGALRM else KeyboardInterrupt()
    old_handlers = {sig: signal.signal(sig, interrupted) for sig in (signal.SIGTERM, signal.SIGINT, signal.SIGALRM)}
    signal.setitimer(signal.ITIMER_REAL, config["total_timeout_s"])
    rc = 1
    try:
        work.mkdir(parents=True, exist_ok=True)
        home = Path(tempfile.mkdtemp(prefix="run-", dir=work))
        os.chmod(home, 0o700)
        result["run_directory"] = home.name
        for directory in ("data", "logs", "tmp"):
            (home / directory).mkdir()
        db = home / "data" / "steve.sqlite3"
        before = time.perf_counter()
        shutil.copyfile(snapshot, db)  # Never opens the source through writable SQLite.
        result["snapshot_copy_s"] = time.perf_counter() - before
        with sqlite3.connect(db) as conn:
            changed = conn.execute("UPDATE users SET subject='dev:local' WHERE id=("
                                   "SELECT id FROM users WHERE role='owner' ORDER BY created, id LIMIT 1)").rowcount
            if changed != 1:
                raise RuntimeError("snapshot must contain an owner")
        env = isolated_environment(home, args.port)
        client = Client(f"http://127.0.0.1:{args.port}", config["request_timeout_s"])
        token = access_token(processes, binary, env, config["cli_timeout_s"]) if config["auth"] == "bearer" else None
        with (home / "logs" / "hub.log").open("wb") as log:
            before = time.perf_counter()
            hub = processes.start([str(binary)], env, stdout=log, stderr=subprocess.STDOUT)
            result["health"] = health(client, hub, config["startup_timeout_s"])
            result["startup_to_health_ms"] = (time.perf_counter() - before) * 1000
            client.login(token)
            result["post_start"] = phase("post_start", config["settle_s"], hub, db, client, config,
                                         env, processes, binary)
            result["endpoints"] = {}
            for endpoint in config["endpoints"]:
                cold = client.request(endpoint["path"])
                rows = [client.request(endpoint["path"]) for _ in range(endpoint["requests"])]
                result["endpoints"][endpoint["path"]] = {"cold": cold, "warm": requests_summary(rows)}
            for name in ("idle", "load"):
                if name == "load":
                    result["warmup"] = warmup(hub, db, client, config, env, processes, binary)
                result[name] = phase(name, config[name + "_s"], hub, db, client, config,
                                     env, processes, binary)
            if hub.poll() is not None:
                raise RuntimeError("Hub exited during measurement")
        failed = any(e["cold"]["error"] or e["warm"]["errors"] for e in result["endpoints"].values())
        failed |= any(result[p]["worker_errors"] or result[p]["lock"]["probe_errors"] for p in ("post_start", "idle", "load"))
        failed |= bool(result["load"]["errors"] or result["load"]["cli"]["errors"])
        failed |= len(result["load"]["cli"]["runs"]) != config["cli_runs"]
        result["status"] = "measurement_errors" if failed else "ok"
        rc = 2 if failed else 0
    except (Exception, KeyboardInterrupt) as exc:
        # Preserve partial evidence without leaking paths, cookies, token output or DB content.
        result["status"], result["failure_type"] = "failed", type(exc).__name__
    finally:
        signal.setitimer(signal.ITIMER_REAL, 0)
        # Cleanup itself must not be interrupted by a second Ctrl-C.
        for sig in (signal.SIGTERM, signal.SIGINT):
            signal.signal(sig, signal.SIG_IGN)
        cpu_stop.set()
        cpu_thread.join(timeout=6)
        result["cleanup"] = processes.cleanup()
        result["elapsed_s"] = time.perf_counter() - started
        result["finished_at"] = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())
        result["load_average_after"] = load_average()
        result["cpu_top"] = [{"name": name, "cpu_percent": value} for name, value in
                             sorted(cpu_peaks.items(), key=lambda item: (-item[1], item[0]))[:config["cpu_top_n"]]]
        if result["cleanup"]["process_groups_remaining"]:
            result["status"], rc = "cleanup_failed", 1
        write_result(out, result)
        for sig, handler in old_handlers.items():
            signal.signal(sig, handler)
    print(json.dumps({"status": result["status"], "elapsed_s": result["elapsed_s"]}))
    return rc


if __name__ == "__main__":
    sys.exit(main())
