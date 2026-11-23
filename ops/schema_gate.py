#!/usr/bin/env python3
"""Can a release open the database? Every script that switches `current` asks this first.

The rule is the Hub's own (steve-store `db::migrate`): a database records in `schema_compat` the oldest
schema a build must know to open it, and a build opens it only if it knows that schema. A database with a schema
but no stamp is refused: nothing says which build may open it.

A release's schema is `schema.target` in its release.json (release.sh and package-release.py write it from the
binary at build time). A release that doesn't say is refused: nobody can show it reads the database. The Hub
applies the same rule (`Core::rollback_refusal`).

    schema_gate.py state <db>               the database's {"schema", "min_reader"} as JSON
    schema_gate.py check <db> <release dir> exit 0 if the release can open it, 1 (with the reason) if not
"""
import json
import pathlib
import sqlite3
import sys


def state(db):
    """{"schema", "min_reader"} of the database; a missing database needs nothing. ValueError: not stamped."""
    db = pathlib.Path(db)
    if not db.exists():
        return {"schema": 0, "min_reader": 0}
    with sqlite3.connect(f"file:{db}?mode=ro", uri=True) as c:
        tables = {r[0] for r in c.execute("SELECT name FROM sqlite_master WHERE type='table'")}
        row = c.execute("SELECT v FROM schema_version").fetchone() if "schema_version" in tables else None
        schema = row[0] if row else 0
        row = c.execute("SELECT min_reader FROM schema_compat").fetchone() if "schema_compat" in tables else None
    if schema and row is None:
        raise ValueError(f"schema v{schema} 的数据库没有 schema_compat 标记，不知道哪个版本能打开它")
    return {"schema": schema, "min_reader": row[0] if row else 0}


def release_schema(release):
    """The schema the release's Hub knows (`schema.target` in its release.json), or None when it doesn't say."""
    try:
        target = json.loads((pathlib.Path(release) / "release.json").read_text()).get("schema", {}).get("target")
    except (OSError, ValueError, AttributeError):
        return None
    return target if isinstance(target, int) else None


def check(db, release):
    """None if the release can open the database, else why not."""
    try:
        s = state(db)
    except ValueError as e:
        return str(e)
    knows = release_schema(release)
    name = pathlib.Path(release).name
    if s["min_reader"] == 0:
        return None
    if knows is None:
        return f"{name} 说不清自己认识哪个 schema（release.json 里没有），不能打开 schema v{s['schema']} 的数据库"
    if knows < s["min_reader"]:
        return f"{name} 只认识到 schema v{knows}，数据库（v{s['schema']}）需要至少 v{s['min_reader']}"
    return None


def main(argv):
    if len(argv) == 3 and argv[1] == "state":
        try:
            print(json.dumps(state(argv[2])))
        except ValueError as e:
            print(e, file=sys.stderr)
            return 1
        return 0
    if len(argv) == 4 and argv[1] == "check":
        why = check(argv[2], argv[3])
        if why:
            print(why)
            return 1
        return 0
    print(__doc__.strip().splitlines()[-2:][0].strip(), file=sys.stderr)
    print(__doc__.strip().splitlines()[-1].strip(), file=sys.stderr)
    return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv))
