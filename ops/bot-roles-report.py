#!/usr/bin/env python3
"""How the items after a change of the bots' division of work went, against the same number before it.

Reads the database read-only (it never writes, not even the WAL) and prints, per item and per group:
who did it, how often the user had to explain more, how often it went back (rework, reopened), and how
long from the first delivery to done. ST-56 uses it to judge the new roles after 5–10 items.

    python3 ops/bot-roles-report.py --since ST-56            # items made after ST-56, up to 10
    python3 ops/bot-roles-report.py --since ST-56 --count 5 --db path/to/steve.sqlite3
"""

import argparse
import os
import sqlite3
import sys

COLUMNS = ("事项", "Bot", "状态", "补充说明", "你的操作", "打回", "重开", "交付次数", "首次交付→完成(时)", "标题")


def connect(path):
    if not os.path.exists(path):
        sys.exit(f"没有数据库：{path}")
    # immutable: no lock, no WAL write; a running Hub's latest writes may be missing
    return sqlite3.connect(f"file:{path}?mode=ro&immutable=1", uri=True)


def item_no(db, ws, ident):
    row = db.execute("SELECT no FROM items WHERE workspace_id = ? AND id = ?", (ws, ident.upper())).fetchone()
    if not row:
        sys.exit(f"没有事项 {ident}")
    return row[0]


def rows(db, ws, where, args, order, count):
    q = f"""
        SELECT i.id, coalesce(b.name, i.bot_id), i.status, i.cycle, i.title,
               (SELECT count(*) FROM timeline t WHERE t.item_id = i.id AND t.kind = 'me' AND t.actor = 'me'),
               (SELECT count(*) FROM timeline t WHERE t.item_id = i.id AND t.kind = 'me' AND t.actor <> 'me'),
               coalesce(s.reworks, 0), coalesce(s.delivers, 0), s.first_deliver, s.done_at
        FROM items i
        LEFT JOIN bots b ON b.id = i.bot_id
        LEFT JOIN item_stats s ON s.item_id = i.id
        WHERE i.workspace_id = ? AND {where}
        ORDER BY i.no {order} LIMIT ?"""
    out = []
    for r in db.execute(q, (ws, *args, count)):
        ident, bot, status, cycle, title, said, acts, reworks, delivers, first, done = r
        hours = round((done - first) / 3_600_000, 1) if first and done and done >= first else None
        out.append(
            {
                "事项": ident,
                "Bot": bot or "—",
                "状态": status,
                "补充说明": said,
                "你的操作": acts,
                "打回": reworks,
                "重开": max((cycle or 1) - 1, 0),
                "交付次数": delivers,
                "首次交付→完成(时)": hours,
                "标题": title[:24],
            }
        )
    return sorted(out, key=lambda x: int(x["事项"].split("-")[-1]))


def table(items):
    lines = ["| " + " | ".join(COLUMNS) + " |", "|" + "---|" * len(COLUMNS)]
    for it in items:
        lines.append("| " + " | ".join("—" if it[c] is None else str(it[c]) for c in COLUMNS) + " |")
    return "\n".join(lines)


def summary(name, items):
    if not items:
        return f"- {name}：还没有事项"
    n = len(items)
    said = sum(x["补充说明"] for x in items)
    acts = sum(x["你的操作"] for x in items)
    back = sum(1 for x in items if x["打回"] or x["重开"])
    hours = sorted(x["首次交付→完成(时)"] for x in items if x["首次交付→完成(时)"] is not None)
    mid = hours[len(hours) // 2] if hours else None
    bots = {}
    for x in items:
        bots[x["Bot"]] = bots.get(x["Bot"], 0) + 1
    by = "、".join(f"{k} {v}" for k, v in sorted(bots.items(), key=lambda kv: -kv[1]))
    return (
        f"- {name}：{n} 件（{by}）；补充说明共 {said} 次，平均 {said / n:.1f}；你的操作平均 {acts / n:.1f} 次；"
        f"被打回或重开 {back} 件；首次交付到完成中位 {'—' if mid is None else mid} 小时"
    )


def main():
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--since", required=True, help="分工调整生效时的事项，如 ST-56；统计它之后的事项")
    ap.add_argument("--count", type=int, default=10, help="前后各取几件（默认 10）")
    ap.add_argument("--workspace", default="default")
    home = os.environ.get("STEVE_HOME", os.path.join(os.path.expanduser("~"), ".steve"))
    ap.add_argument("--db", default=os.path.join(home, "data", "steve.sqlite3"))
    a = ap.parse_args()
    db = connect(a.db)
    no = item_no(db, a.workspace, a.since)
    after = rows(db, a.workspace, "i.no > ?", (no,), "ASC", a.count)
    before = rows(db, a.workspace, "i.no < ?", (no,), "DESC", a.count)
    print(f"## {a.since.upper()} 之后\n\n{table(after) if after else '还没有事项。'}\n")
    print(f"## {a.since.upper()} 之前（对照）\n\n{table(before)}\n")
    print("## 对比\n")
    print(summary("之后", after))
    print(summary("之前", before))
    print(
        "\n“补充说明”是你在事项里另外打字说的话；“你的操作”是批准、验收、回答、暂停这类点按；"
        "“打回”是验收时退回重做；“重开”是完成后又打开。"
    )


if __name__ == "__main__":
    main()
