#!/usr/bin/env python3
"""Maintainer tool: fetch affectUI items into web/src/components/affect/<slug>.tsx.

Pro items need the maintainer's own access key in AFFECTUI_TOKEN (or ~/.config/affectui/token); the key is sent
as a header and never written anywhere. Every Pro file gets the notice the affectUI Pro license requires at its
top: those files stay under that license, not under Steve's.

    python3 scripts/affectui-pull.py agent-lanes trace-waterfall

Items come in the "clean" style (one card, no gray tray around it), which matches Steve's surfaces; the file and
its class names carry the -clean suffix the registry gives it. --classic fetches the other style.
"""
import json
import os
import pathlib
import sys
import urllib.request

ROOT = pathlib.Path(__file__).resolve().parent.parent
OUT = ROOT / "web/src/components/affect"
FREE = "/* affectUI (MIT) – https://www.affectui.com, MIT License, Copyright (c) 2026 Florian Höller (adfects); full text: https://github.com/flohoeller/affectui/blob/main/LICENSE */\n"
NOTICE = ("/* affectUI Pro – licensed under https://www.affectui.com/license, not under this project's open-source license. "
          "Don't copy it out of this project. */\n")


def token() -> str | None:
    if os.environ.get("AFFECTUI_TOKEN"):
        return os.environ["AFFECTUI_TOKEN"].strip()
    p = pathlib.Path.home() / ".config/affectui/token"
    return p.read_text().strip() if p.exists() else None


def tiers() -> dict[str, str]:
    """slug -> "Free" or "Pro", from the catalogue the site publishes for tools"""
    import re
    with urllib.request.urlopen("https://www.affectui.com/llms.txt") as r:
        text = r.read().decode()
    return {m[0]: m[1] for m in re.findall(r"affectui\.com/[a-z-]+/([a-z0-9-]+)\):.*\((Free|Pro)\b", text)}


def pull(slug: str, key: str | None, tier: str, clean: bool) -> None:
    req = urllib.request.Request(f"https://www.affectui.com/r/{slug}{'-clean' if clean else ''}.json")
    if key:
        req.add_header("Authorization", f"Bearer {key}")
    with urllib.request.urlopen(req) as r:
        item = json.load(r)
    for f in item["files"]:
        body = f["content"]
        name = pathlib.Path(f["path"]).name
        # Pro items carry the license notice; free ones (MIT) their attribution
        head = NOTICE if tier != "Free" else FREE
        if not body.startswith(head):
            body = head + body
        (OUT / name).write_text(body)
        print(f"{slug}: {name}")


def main() -> None:
    if len(sys.argv) < 2:
        sys.exit(__doc__)
    OUT.mkdir(parents=True, exist_ok=True)
    args = [a for a in sys.argv[1:] if a != "--classic"]
    key, tier = token(), tiers()
    for slug in args:
        pull(slug, key, tier.get(slug, "Pro"), "--classic" not in sys.argv)


if __name__ == "__main__":
    main()
