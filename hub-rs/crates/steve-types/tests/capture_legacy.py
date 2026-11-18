#!/usr/bin/env python3
"""Capture goldens with the actual legacy enum definitions from git, in a disposable Rust program.
Default is read/compare; --bless writes fixtures. No Hub, database or production source edits.
"""
import argparse
import json
import os
from pathlib import Path
import re
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[4]
FIXTURES = Path(__file__).resolve().parent / "fixtures/wire"


def enum_source(text, name):
    start = text.index("pub enum " + name)
    start = text.rfind("#[derive(", 0, start)
    # The legacy enum's outer braces have balanced nested braces only inside comments; line-start
    # closing brace is unambiguous for both current legacy definitions.
    end = re.search(r"^}", text[start:], re.M).end() + start
    return text[start:end]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--baseline", default="3b69ca887")
    parser.add_argument("--bless", action="store_true")
    args = parser.parse_args()
    def show(path):
        return subprocess.check_output(["git", "show", f"{args.baseline}:{path}"], cwd=ROOT, text=True)
    store = show("hub-rs/crates/steve-store/src/bus.rs")
    hub = show("hub-rs/crates/steve-hub/src/bus.rs")
    store_enum = enum_source(store, "ChangeEvent")
    hub_enum = enum_source(hub, "HubEvent")
    helper_start = hub.index("mod double_option")
    helper_end = re.search(r"^}", hub[helper_start:], re.M).end() + helper_start
    variants = list(re.finditer(r'#\[serde\(rename = "([^"]+)"\)\]\s*(\w+)\s*{([^}]*?)}', store_enum, re.S))
    if len(variants) != 36:
        raise RuntimeError("legacy variant inventory changed; review capture coverage")
    values = {
        "id": '"entity-1".into()', "ws": '"workspace-a".into()', "user": '"user-a".into()',
        "session_id": '"session-1".into()', "item_id": "None", "seq": "7", "kind": '"append".into()',
        "data": 'json!({"text":"hello"})', "on": "true", "phase": "None", "mid": '"message-1".into()',
        "thought": '"thinking".into()', "text": '"answer".into()', "run": '"run-1".into()',
        "name": '"search".into()', "brief": '"lookup".into()', "ok": "true", "running": "false",
        "call": "None", "started_at": "None", "at": "None", "hold": "None", "flow_id": '"flow-1".into()',
        "space_id": '"space-1".into()', "decision_id": "None", "day": "None",
    }
    cases = []
    for match in variants:
        wire, variant, raw = match.groups()
        fields = re.sub(r'///[^\n]*|#\[[\s\S]*?\]', '', raw)
        fields = {name: values[name] for name in re.findall(r'\b(\w+)\s*:', fields)}
        if variant == "SteveThinking":
            fields["ws"] = "None"
        if variant == "SteveTool":
            fields["kind"] = "String::new()"
        cases.append((wire, variant, fields))
        if variant in ("Decision", "Session"):
            cases.extend([(wire + "-null", variant, dict(fields, item_id="Some(None)")),
                          (wire + "-value", variant, dict(fields, item_id='Some(Some("item-1".into()))'))])
        full = {
            "SteveThinking": dict(fields, ws='Some("workspace-a".into())', phase='Some("answer".into())'),
            "SteveTool": dict(fields, kind='"search".into()', running="true", call="Some(2)", started_at="Some(10)", at="Some(20)"),
            "ReleaseHold": dict(fields, hold='Some(json!({"item":"item-1","running":2,"since":10}))'),
            "Stats": dict(fields, day='Some("2026-11-19".into())'),
            "YoloAction": dict(fields, decision_id='Some("decision-1".into())'),
        }.get(variant)
        if full is not None:
            cases.append((wire + "-full", variant, full))
    scratch = ROOT / "scratch"
    scratch.mkdir(exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="legacy-wire-", dir=scratch) as tmp:
        tmp = Path(tmp)
        (tmp / "src").mkdir()
        (tmp / "fixtures").mkdir()
        (tmp / "Cargo.toml").write_text('''[package]
name="legacy-wire-capture"
version="0.0.0"
edition="2024"
[workspace]
[dependencies]
serde={version="=1.0.229",features=["derive"]}
serde_json="=1.0.154"
''')
        program = 'use serde::{Serialize,Deserialize};\nuse serde_json::{Value,json};\n'
        program += store_enum + "\n" + hub_enum + "\n" + hub[helper_start:helper_end]
        program += '\nfn main(){let dir=std::path::Path::new("fixtures");\n'
        shared = set(re.findall(r'#\[serde\(rename = "[^"]+"\)\]\s*(\w+)', hub_enum))
        for wire, variant, fields in cases:
            constructor = " { " + ", ".join(k + ": " + v for k, v in fields.items()) + " }"
            program += f'let bytes=serde_json::to_string(&ChangeEvent::{variant}{constructor}).unwrap();\n'
            other = "Thinking" if variant == "SteveThinking" else variant
            if other in shared:
                program += f'assert_eq!(bytes,serde_json::to_string(&HubEvent::{other}{constructor}).unwrap());\n'
            program += f'std::fs::write(dir.join("{wire}.json"),bytes).unwrap();\n'
        # Actual publish_value payload expressions from wire/modules.rs and wire/signals.rs.
        for wire in ("kb", "gh", "mail"):
            program += f'std::fs::write(dir.join("{wire}.json"),serde_json::to_string(&json!({{"type":"{wire}","id":"entity-1","ws":"workspace-a"}})).unwrap()).unwrap();\n'
        program += "}\n"
        (tmp / "src/main.rs").write_text(program)
        env = dict(os.environ)
        env.setdefault("CARGO_TARGET_DIR", str(ROOT / "hub-rs/target"))
        result = subprocess.run(["cargo", "run", "--quiet", "--offline"], cwd=tmp, env=env)
        if result.returncode:
            raise SystemExit(result.returncode)
        actual = {p.name: p.read_bytes() for p in (tmp / "fixtures").glob("*.json")}
        if args.bless:
            FIXTURES.mkdir(parents=True, exist_ok=True)
            for name, data in actual.items():
                (FIXTURES / name).write_bytes(data)
        expected = {p.name: p.read_bytes() for p in FIXTURES.glob("*.json")}
        differences = sorted(name for name in actual.keys() | expected.keys() if actual.get(name) != expected.get(name))
        if differences:
            raise SystemExit(f"legacy golden differences: {differences}")
        print(f"{args.baseline}: {len(variants)} ChangeEvent + {len(shared)} shared HubEvent variants; {len(actual)} goldens identical")


if __name__ == "__main__":
    main()
