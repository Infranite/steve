#!/usr/bin/env python3
"""Recover old prose-only artifact locations into persistent per-item output files.

Dry run by default. --apply takes a verified SQLite backup before changing any row. It only
repairs references that explicitly name a delivery summary and have a matching saved full text.
Missing screenshots or arbitrary paths are never reconstructed from prose.
"""

import argparse
import hashlib
import os
from pathlib import Path
import sqlite3
import sys
from datetime import datetime, timezone


def summary_reference(location):
    if not location:
        return False
    value = location.strip()
    return (value.startswith(("本次交付摘要", "交付摘要", "本次交付正文"))
            or "steve_deliver summary" in value
            or ("steve_workflow" in value and "summary" in value)
            or (value.startswith("ST-") and "交付摘要" in value))


def recoverable(kind, title, location):
    if summary_reference(location):
        return True
    return (not location and any(word in kind for word in ("方案", "设计", "报告", "报表", "分析", "评审", "结论"))
            and any(word in title for word in ("设计", "方案", "评审", "结论", "报告", "报表", "分析")))


def source(c, item, created):
    row = c.execute("""SELECT summary FROM workflow_units
        WHERE item_id=? AND length(summary)>80 ORDER BY length(summary) DESC LIMIT 1""", (item,)).fetchone()
    if row:
        return row[0]
    row = c.execute("""SELECT text FROM timeline WHERE item_id=? AND kind='sys'
        AND text LIKE '%已交付，等待执行结束后交给下游%' AND ts<=?
        ORDER BY ts DESC,id DESC LIMIT 1""", (item, created)).fetchone()
    if row and "\n" in row[0]:
        body = row[0].split("\n", 1)[1]
        if len(body) > 80:
            return body
    return None


def real_dir(path):
    if path.is_symlink():
        raise RuntimeError(f"refusing symlink directory: {path}")
    path.mkdir(mode=0o700, exist_ok=True)
    if not path.is_dir() or path.is_symlink():
        raise RuntimeError(f"not a real directory: {path}")
    path.chmod(0o700)


def save(home, item, body):
    if not item or not all(c.isascii() and (c.isalnum() or c in "-_") for c in item):
        raise RuntimeError(f"invalid item id: {item}")
    directory = home / "workspaces" / item / "outputs"
    for part in (directory.parent.parent, directory.parent, directory):
        real_dir(part)
    data = body.encode("utf-8")
    digest = hashlib.sha256(data).hexdigest()
    path = directory / f"report-{digest}.md"
    if path.exists() or path.is_symlink():
        if path.is_symlink() or path.read_bytes() != data:
            raise RuntimeError(f"report hash collision or changed file: {path}")
        return path
    tmp = directory / f".report-{os.getpid()}-{digest}.tmp"
    try:
        with tmp.open("xb", buffering=0) as file:
            os.chmod(tmp, 0o600)
            file.write(data)
            os.fsync(file.fileno())
        try:
            os.link(tmp, path)
        except FileExistsError:
            if path.is_symlink() or path.read_bytes() != data:
                raise RuntimeError(f"report changed during save: {path}")
        fd = os.open(directory, os.O_RDONLY)
        try:
            os.fsync(fd)
        finally:
            os.close(fd)
    finally:
        tmp.unlink(missing_ok=True)
    return path


def run(home, apply):
    db = home / "data" / "steve.sqlite3"
    if not db.is_file():
        raise RuntimeError(f"database missing: {db}")
    uri = f"file:{db}?mode={'rw' if apply else 'ro'}"
    c = sqlite3.connect(uri, uri=True)
    try:
        rows = c.execute("SELECT id,item_id,kind,title,location,created FROM artifacts ORDER BY created,id").fetchall()
        pending = []
        missing = []
        for aid, item, kind, title, location, created in rows:
            if not recoverable(kind, title, location):
                continue
            body = source(c, item, created)
            if body:
                pending.append((aid, item, location, body))
            else:
                missing.append((aid, item))
        print(f"recoverable={len(pending)} missing_source={len(missing)}")
        for aid, item, _, body in pending:
            print(f"  {item} {aid}: {len(body)} chars")
        for aid, item in missing:
            print(f"  {item} {aid}: no saved full text", file=sys.stderr)
        if not apply or not pending:
            return
        backup_dir = home / "backups"
        real_dir(backup_dir)
        stamp = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ")
        backup = backup_dir / f"pre-delivery-recovery-{stamp}-{os.getpid()}.sqlite3"
        dst = sqlite3.connect(backup)
        try:
            c.backup(dst)
            if dst.execute("PRAGMA quick_check").fetchone()[0] != "ok":
                raise RuntimeError("backup quick_check failed")
        finally:
            dst.close()
        backup.chmod(0o600)
        print(f"backup={backup}")
        paths = [(aid, location, str(save(home, item, body))) for aid, item, location, body in pending]
        c.execute("BEGIN IMMEDIATE")
        try:
            for aid, old, path in paths:
                changed = c.execute("UPDATE artifacts SET location=? WHERE id=? AND location IS ?",
                                    (path, aid, old)).rowcount
                if changed != 1:
                    raise RuntimeError(f"artifact changed concurrently: {aid}")
            c.commit()
        except BaseException:
            c.rollback()
            raise
        print(f"repaired={len(paths)}")
    finally:
        c.close()


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--home", type=Path, default=Path.home() / ".steve")
    parser.add_argument("--apply", action="store_true")
    args = parser.parse_args()
    run(args.home, args.apply)
