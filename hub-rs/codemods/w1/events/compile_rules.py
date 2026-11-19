#!/usr/bin/env python3
"""Type-check rewritten constructors for every rule; verify all workspace warnings are emitted."""
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import tempfile

HERE = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location("event_driver", HERE / "run.py")
driver = importlib.util.module_from_spec(spec)
spec.loader.exec_module(driver)
ROOT = driver.ROOT


def expr(value, old, present):
    if old == "String":
        return json.dumps(value if value is not None else "") + ".into()"
    if old == "bool":
        return str(value or False).lower()
    if old in ("i64", "u32"):
        return str(value)
    if old.startswith("Option<"):
        if not present:
            return "None"
        inner = old[len("Option<"):-1]
        if value is None and not inner.startswith("Option"):
            return "None"
        return "Some(" + ("None" if value is None else expr(value, inner, True)) + ")"
    if old == "Value":
        raw = json.dumps(value, separators=(",", ":"))
        return "serde_json::from_str::<serde_json::Value>(" + json.dumps(raw) + ").unwrap()"
    raise ValueError(old)


def main():
    rules = {r["id"]: r for r in json.loads((HERE / "manifest.json").read_text())}
    fixture_dir = ROOT / "hub-rs/crates/steve-types/tests/fixtures/wire"
    sources = []
    expected_types = []
    for rid, rule in rules.items():
        is_hub = rid.startswith("hubevent-")
        source = "s::HubEvent" if is_hub else "s::ChangeEvent"
        variant = "Thinking" if rid == "hubevent-thinking" else rule["variant"]
        # Wire tags are independent of capitalization (Mm, ChanMessage, SteveThinking, etc.).
        tag = {"ThreadRun": "thread.run", "ReleaseHold": "release.hold", "SteveThinking": "steve.thinking",
               "SteveDraft": "steve.draft", "SteveTool": "steve.tool", "YoloAction": "yolo_action",
               "ChanMessage": "chan_message"}.get(rule["variant"], rule["variant"].lower())
        for fixture in sorted(fixture_dir.glob(tag + "*.json")):
            # Avoid prefix collisions, e.g. channel and chan_message.
            if fixture.stem != tag and not fixture.stem.startswith(tag + "-"):
                continue
            data = json.loads(fixture.read_text())
            fields = ", ".join(f"{name}: {expr(data.get(name), old, name in data)}" for name, old in rule["fields"])
            sources.append(f"fn f{len(sources)}() {{ let _ = {source}::{variant} {{ {fields} }}; }}")
            expected_types.append("LiveEvent" if rule["type"] == "WorkLiveEvent" else "DomainEvent")
    scratch = ROOT / "scratch"
    scratch.mkdir(exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="events-compile-", dir=scratch) as tmp:
        tmp = Path(tmp)
        src = tmp / "src/lib.rs"
        src.parent.mkdir()
        src.write_text("\n".join(sources))
        hits = driver.run_json([os.environ.get("AST_GREP", "/opt/homebrew/bin/ast-grep"), "scan", "--config", str(HERE / "sgconfig.yml"), "--json=compact", str(src)], ROOT)
        assert len(hits) == len(sources), (len(hits), len(sources))
        result = "#![allow(dead_code)]\n"
        expected_warnings = 0
        for i, hit in enumerate(sorted(hits, key=lambda h: h["range"]["byteOffset"]["start"])):
            rewritten = driver.replacement(hit, rules[hit["ruleId"]])
            expected_warnings += 2 * rewritten.count("steve_types::ws_todo!()")
            result += f"fn f{i}(store: &steve_store::Store, tx: &steve_store::Tx) {{ store.publish({rewritten}); tx.publish({rewritten}); }}\n"
        src.write_text(result)
        (tmp / "Cargo.toml").write_text('[package]\nname="event-rule-compile"\nversion="0.0.0"\nedition="2024"\n[workspace]\n[dependencies]\nsteve-types={path=' + json.dumps(str(ROOT / "hub-rs/crates/steve-types")) + '}\nsteve-store={path=' + json.dumps(str(ROOT / "hub-rs/crates/steve-store")) + '}\nserde_json="=1.0.154"\n')
        env = dict(os.environ)
        env.setdefault("CARGO_TARGET_DIR", str(ROOT / "hub-rs/target"))
        check = subprocess.run(["cargo", "check", "--offline", "--message-format=json"], cwd=tmp, env=env, capture_output=True, text=True)
        messages = [json.loads(line) for line in check.stdout.splitlines() if line.startswith("{")]
        if check.returncode:
            errors = [m["message"].get("rendered", "") for m in messages if m.get("reason") == "compiler-message" and m["message"]["level"] == "error"]
            raise SystemExit("\n".join(errors) + check.stderr)
        warnings = [m for m in messages if m.get("reason") == "compiler-message" and
                    (m["message"].get("code") or {}).get("code") == "deprecated" and
                    "ws_todo" in m["message"]["message"]]
        assert len(warnings) == expected_warnings, (len(warnings), expected_warnings)
        print(f"{len(rules)} rules; {len(sources)} rewritten constructors type-check; all {len(warnings)} workspace sites warn")


if __name__ == "__main__":
    main()
