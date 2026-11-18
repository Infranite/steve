"""Prometheus text snapshots and bounded numeric differences (stdlib only)."""
import errno
import http.client
import json
import re
import urllib.error
import urllib.request


def scrape(client, url, destination, authenticated=True):
    headers = client.headers() if authenticated else {}
    headers["Accept-Encoding"] = "identity"
    request = urllib.request.Request(url, headers=headers)
    try:
        with client.opener.open(request, timeout=client.timeout) as response:
            body = response.read().decode("utf-8")
            # Credentials must never be persisted, even if a broken endpoint echoes them.
            if client.credential and client.credential in body:
                raise RuntimeError("scrape echoed a credential")
            if destination is not None:
                with destination.open("x") as stream:
                    stream.write(body)
            return {"status": response.status, "file": destination.name if destination else None,
                    "content_type": response.headers.get("Content-Type", "")}
    except urllib.error.HTTPError as exc:
        code = exc.code
        exc.close()
        return {"status": code, "file": None}
    except (OSError, urllib.error.URLError, http.client.HTTPException) as exc:
        reason = getattr(exc, "reason", exc)
        return {"status": "refused" if getattr(reason, "errno", None) == errno.ECONNREFUSED
                else type(reason).__name__, "file": None}


def parse(text):
    series = {}
    for line in text.splitlines():
        if not line or line.startswith("#"):
            continue
        match = re.fullmatch(r'([a-zA-Z_:][a-zA-Z0-9_:]*)(?:\{(.*)\})?\s+(\S+)(?:\s+\d+)?', line)
        if not match:
            raise ValueError("malformed Prometheus sample")
        name, labels, value = match.groups()
        pairs = re.findall(r'([a-zA-Z_][a-zA-Z0-9_]*)="((?:\\.|[^"\\])*)"', labels or "")
        tags = tuple(sorted((k, json.loads('"' + v + '"')) for k, v in pairs))
        series[name, tags] = float(value)
    return series


def difference(before, after, route, method):
    left, right = parse(before), parse(after)
    delta = {key: value - left.get(key, 0) for key, value in right.items()}
    def selected(name, **labels):
        return [(dict(tags), value) for (metric, tags), value in delta.items()
                if metric == name and all(dict(tags).get(k) == v for k, v in labels.items())]
    def total(name, **labels):
        rows = selected(name, **labels)
        return sum(v for _, v in rows) if rows else None
    http = {name: total(name, route=route, method=method, status="2xx") for name in (
        "steve_http_requests_total", "steve_http_request_duration_seconds_sum",
        "steve_http_request_duration_seconds_count")}
    store = {suffix: total("steve_http_store_calls_per_request_" + suffix, route=route)
             for suffix in ("sum", "count")}
    locks = {}
    for kind in ("wait", "hold"):
        name = f"steve_lock_{kind}_seconds"
        locks[kind] = {}
        for lock in ("store", "core"):
            rows = []
            for tags, value in selected(name + "_sum", lock=lock):
                count = total(name + "_count", **tags)
                rows.append({"lock": lock, "caller": tags.get("caller"), "sum": value,
                             "count": count, "mean_ms": 1000 * value / count if count else None})
            locks[kind][lock] = sorted(rows, key=lambda r: r["sum"], reverse=True)[:10]
    disk = ("steve_process_disk_written_bytes", ())
    return {"http": http, "store_calls": store, "lock_top10": locks,
            "disk_written_bytes": {"before": left.get(disk), "after": right.get(disk), "delta": delta.get(disk)},
            "collection_errors": [{"source": tags.get("source"), "delta": value}
                                  for tags, value in selected("steve_metrics_collection_errors_total")],
            "missing": [name for name, value in http.items() if value is None] +
                       (["steve_process_disk_written_bytes"] if disk not in left or disk not in right else []) +
                       (["steve_http_store_calls_per_request"] if any(v is None for v in store.values()) else [])}
