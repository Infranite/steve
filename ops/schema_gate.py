#!/usr/bin/env python3
"""Can a release open the database? Every script that switches `current` asks this first.

The rule is the Hub's own (steve-store `db::migrate`): a database records in `schema_compat` the oldest
schema a build must know to open it (a database from before 0.2.14 has no stamp: then its own schema), and a
build opens it only if it knows that schema. Builds from 0.2.14 on refuse to start otherwise; older builds
(and the TS Hub) don't check, so a switch to one must be refused here.

A release's schema is `schema.target` in its release.json, else what `bin/steve-hub --build-info` says. A
release that tells neither (a TS-only release) is refused: nobody can show it reads the database.

    schema_gate.py state <db>               the database's {"schema", "min_reader"} as JSON
    schema_gate.py check <db> <release dir> exit 0 if the release can open it, 1 (with the reason) if not
"""
import json
import pathlib
import sqlite3
import subprocess
import sys


def state(db):
    """{"schema", "min_reader"} of the database; a missing database needs nothing."""
    db = pathlib.Path(db)
    if not db.exists():
        return {"schema": 0, "min_reader": 0}
    with sqlite3.connect(f"file:{db}?mode=ro", uri=True) as c:
        tables = {r[0] for r in c.execute("SELECT name FROM sqlite_master WHERE type='table'")}
        row = c.execute("SELECT v FROM schema_version").fetchone() if "schema_version" in tables else None
        schema = row[0] if row else 0
        row = c.execute("SELECT min_reader FROM schema_compat").fetchone() if "schema_compat" in tables else None
    return {"schema": schema, "min_reader": row[0] if row else schema}


def release_schema(release):
    """The schema the release's Hub knows, or None when it doesn't say."""
    release = pathlib.Path(release)
    try:
        target = json.loads((release / "release.json").read_text()).get("schema", {}).get("target")
        if isinstance(target, int):
            return target
    except (OSError, ValueError, AttributeError):
        pass
    binary = release / "bin/steve-hub"
    if binary.is_file():
        try:
            out = subprocess.run([str(binary), "--build-info"], capture_output=True, text=True, timeout=10)
            schema = json.loads(out.stdout).get("schema")
            if isinstance(schema, int):
                return schema
        except (OSError, ValueError, subprocess.TimeoutExpired):
            pass
    return None


def check(db, release):
    """None if the release can open the database, else why not."""
    s, knows = state(db), release_schema(release)
    name = pathlib.Path(release).name
    if s["min_reader"] == 0:
        return None
    if knows is None:
        return f"{name} 说不清自己认识哪个 schema（没有 release.json 的 schema，也没有 bin/steve-hub），不能打开 schema v{s['schema']} 的数据库"
    if knows < s["min_reader"]:
        return f"{name} 只认识到 schema v{knows}，数据库（v{s['schema']}）需要至少 v{s['min_reader']}"
    return None


def main(argv):
    if len(argv) == 3 and argv[1] == "state":
        print(json.dumps(state(argv[2])))
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
