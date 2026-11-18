#!/usr/bin/env python3
"""Compare all numeric benchmark measurements, with stable Markdown output."""
import argparse
import json
import math
from pathlib import Path
import sys

# Metadata/config are context, not performance measurements. PID and CLI return
# codes are identities/categories; statuses are emitted as explanatory rows.
EXCLUDE = {"pid", "returncode", "schema_version"}
ROOTS = ("startup_to_health_ms", "snapshot_copy_s", "elapsed_s", "endpoints", "post_start", "idle", "load",
         "load_average_before", "load_average_after", "cpu_top")


def metrics(value, prefix=""):
    result = {}
    if isinstance(value, dict):
        for key, child in sorted(value.items()):
            if key not in EXCLUDE:
                result.update(metrics(child, f"{prefix}.{key}" if prefix else key))
    elif isinstance(value, list):
        # Sort process rows by name/occurrence rather than unstable PIDs. Duplicate
        # names get a stable suffix within the recorded order; totals are canonical.
        counts = {}
        for index, child in enumerate(value):
            label = str(index)
            if isinstance(child, dict) and "name" in child:
                name = child["name"]
                counts[name] = counts.get(name, 0) + 1
                label = f"{name}#{counts[name]}"
            result.update(metrics(child, f"{prefix}[{label}]"))
    elif value is None or (isinstance(value, (int, float)) and not isinstance(value, bool)):
        result[prefix] = value
    return result


def number(value):
    if value is None:
        return "—"
    if isinstance(value, int):
        return str(value)
    if not math.isfinite(value):
        return "n/a"
    return f"{value:.6f}".rstrip("0").rstrip(".")


def escape(value):
    return str(value).replace("|", "\\|").replace("\n", " ").replace("\r", " ")


def percent(a, b):
    if a is None or b is None:
        return "n/a"
    if a == 0:
        return "0.00%" if b == 0 else "n/a (A=0)"
    return f"{(b - a) / abs(a) * 100:+.2f}%"


def load_context(label, data):
    def averages(key):
        values = data.get(key)
        return ", ".join(number(v) for v in values) if values else "unavailable"
    top = sorted(data.get("cpu_top", []), key=lambda row: -row["cpu_percent"])[:3]
    processes = ", ".join(f"{escape(row['name'])} {number(row['cpu_percent'])}%" for row in top)
    return (f"{label} load_average_before: [{averages('load_average_before')}]; "
            f"load_average_after: [{averages('load_average_after')}]; "
            f"cpu_top (3): {processes or 'unavailable'}")


def compare(a, b):
    left = metrics({key: a[key] for key in ROOTS if key in a})
    right = metrics({key: b[key] for key in ROOTS if key in b})
    lines = [f"A: {escape(a.get('label', ''))} ({escape(a.get('status', 'unknown'))}); "
             f"B: {escape(b.get('label', ''))} ({escape(b.get('status', 'unknown'))})", "",
             load_context("A", a), "", load_context("B", b), "",
             "注意：负载不同时，性能数字不可直接比较（different system loads are not directly comparable）。", "",
             "| Metric | A | B | Change (B−A)/A |", "| --- | ---: | ---: | ---: |"]
    for key in sorted(left.keys() | right.keys()):
        av, bv = left.get(key), right.get(key)
        lines.append(f"| {escape(key)} | {number(av)} | {number(bv)} | {percent(av, bv)} |")
    return "\n".join(lines) + "\n"


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("a", type=Path)
    parser.add_argument("b", type=Path)
    parser.add_argument("--md", action="store_true", help="Markdown (also the default)")
    args = parser.parse_args(argv)
    print(compare(json.loads(args.a.read_text()), json.loads(args.b.read_text())), end="")
    return 0


if __name__ == "__main__":
    sys.exit(main())
