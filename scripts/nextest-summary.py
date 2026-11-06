#!/usr/bin/env python3
"""Summarize a cargo-nextest JUnit report: every test that passed only on a retry, and every test
that failed all its tries.

    python3 scripts/nextest-summary.py hub-rs/target/nextest/ci/junit.xml

Writes Markdown to stdout, and appends it to `$GITHUB_STEP_SUMMARY` when that is set. On GitHub
Actions each flaky test is also a warning annotation, so a retried pass stays visible on the run
page. Exits 0 whatever the tests did (the test step itself carries the verdict); exits 2 when the
report is missing or unreadable.
"""
import os
import sys
import xml.etree.ElementTree as ET
from dataclasses import dataclass, field


@dataclass
class Case:
    name: str
    failures: list = field(default_factory=list)  # first lines of each failed try, in order
    passed: bool = True


def first_line(el):
    msg = el.get("message") or (el.text or "").strip()
    return msg.splitlines()[0] if msg else el.get("type", "failed")


def read(path):
    """The flaky (passed after failing) and failed (never passed) tests in a JUnit report."""
    root = ET.parse(path).getroot()
    flaky, failed = [], []
    for tc in root.iter("testcase"):
        name = f"{tc.get('classname', '')} {tc.get('name', '')}".strip()
        case = Case(name)
        final =tc.find("failure") if tc.find("failure") is not None else tc.find("error")
        if final is not None:
            case.passed = False
            case.failures.append(first_line(final))
            case.failures += [first_line(e) for e in tc if e.tag in ("rerunFailure", "rerunError")]
            failed.append(case)
            continue
        tries = [first_line(e) for e in tc if e.tag in ("flakyFailure", "flakyError")]
        if tries:
            case.failures = tries
            flaky.append(case)
    return flaky, failed


def markdown(flaky, failed, title):
    out = [f"### {title}", ""]
    if not flaky and not failed:
        out.append("Every test passed on its first try.")
    if flaky:
        out += [f"**{len(flaky)} passed only on retry** (flaky: fix them, a retry is not a pass):", ""]
        out += ["| Test | Failed tries | First failure |", "|---|---|---|"]
        for c in flaky:
            out.append(f"| `{c.name}` | {len(c.failures)} | {esc(c.failures[0])} |")
        out.append("")
    if failed:
        out += [f"**{len(failed)} failed every try:**", ""]
        out += ["| Test | Tries | Failure |", "|---|---|---|"]
        for c in failed:
            out.append(f"| `{c.name}` | {len(c.failures)} | {esc(c.failures[0])} |")
        out.append("")
    return "\n".join(out) + "\n"


def esc(s):
    return s.replace("|", "\\|").replace("\n", " ")


def annotation(s):
    return s.replace("%", "%25").replace("\r", "%0D").replace("\n", "%0A")


def main(argv):
    if len(argv) != 2:
        print(__doc__.strip(), file=sys.stderr)
        return 2
    title = f"Rust tests ({os.environ['RUNNER_OS']})" if os.environ.get("RUNNER_OS") else "Rust tests"
    try:
        flaky, failed = read(argv[1])
    except (OSError, ET.ParseError) as e:
        text = f"### {title}\n\nNo nextest report ({e}): the tests did not run to the end.\n"
        code = 2
    else:
        text = markdown(flaky, failed, title)
        code = 0
        if os.environ.get("GITHUB_ACTIONS") == "true":
            for c in flaky:
                print(annotation(f"::warning title=Flaky test::{c.name} passed only after "
                                 f"{len(c.failures)} failed tr{'y' if len(c.failures) == 1 else 'ies'}: "
                                 f"{c.failures[0]}"))
    sys.stdout.write(text)
    summary = os.environ.get("GITHUB_STEP_SUMMARY")
    if summary:
        with open(summary, "a", encoding="utf-8") as f:
            f.write(text)
    return code


if __name__ == "__main__":
    sys.exit(main(sys.argv))
