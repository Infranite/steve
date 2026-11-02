#!/usr/bin/env python3
"""Audit public-tree dependencies and personal identifiers without printing source contents.

Normal mode permits only the exact personal-identifier debt recorded in the baseline.
--strict rejects all findings. Dependency violations can never be baselined.
"""
import argparse
from collections import Counter
import hashlib
import json
from pathlib import Path
import re
import subprocess
import sys
try:
    import tomllib
except ModuleNotFoundError:
    raise SystemExit("This check requires Python 3.11+ (tomllib); run python3.13 on the development Mac.")

TOKENS = {
    "personal-domain": re.compile(r"pengxiang(?:\.dev|\.[A-Za-z])", re.I),
    "personal-namespace": re.compile(r"dev\.pengxiang\.", re.I),
    "private-gateway": re.compile(r"astergate", re.I),
    "private-network": re.compile(r"(?<![\d.])100\.77\.\d+\.\d+(?![\d.])"),
    "personal-home": re.compile(r"/(?:Users|home)/(?!runner(?:/|$)|user(?:/|$)|example(?:/|$))[A-Za-z0-9._-]+/"),
}


def finding(path, rule, line, text):
    # Stable under line movement; never write the matching content or credentials into reports.
    return {"path": path, "rule": rule, "line": line,
            "fingerprint": hashlib.sha256((path + "\0" + rule + "\0" + text.strip()).encode()).hexdigest()}


def dependencies(value):
    if isinstance(value, dict):
        for key, child in value.items():
            if key in ("dependencies", "dev-dependencies", "build-dependencies") and isinstance(child, dict):
                yield from (v for v in child.values() if isinstance(v, dict))
            elif key == "patch" and isinstance(child, dict):
                for registry in child.values():
                    if isinstance(registry, dict):
                        yield from (v for v in registry.values() if isinstance(v, dict))
            elif key == "replace" and isinstance(child, dict):
                yield from (v for v in child.values() if isinstance(v, dict))
            else:
                yield from dependencies(child)


def scan(root, paths):
    found = []
    for rel in sorted(paths):
        p = root / rel
        if rel.startswith(".steve/"):
            found.append(finding(rel, "private-runtime-artifact", 0, rel))
        if not p.is_file():
            continue
        if p.suffix == ".toml" and p.name == "Cargo.toml":
            try:
                data = tomllib.loads(p.read_text())
            except (ValueError, UnicodeError) as e:
                found.append(finding(rel, "dependency-manifest-invalid", 0, type(e).__name__))
                continue
            for dep in dependencies(data):
                raw = dep.get("path")
                if raw is not None:
                    resolved = (p.parent / raw).resolve()
                    if Path(raw).is_absolute() or not resolved.is_relative_to(root.resolve()) or not resolved.is_dir():
                        found.append(finding(rel, "dependency-outside-public-tree", 0, str(raw)))
                if "git" in dep:
                    found.append(finding(rel, "dependency-git-needs-review", 0, str(dep["git"])))
                if "registry" in dep:
                    found.append(finding(rel, "dependency-custom-registry", 0, str(dep["registry"])))
        if p.name == "Cargo.lock":
            for n, line in enumerate(p.read_text().splitlines(), 1):
                if line.startswith('source = "git+'):
                    found.append(finding(rel, "dependency-git-needs-review", n, line))
                elif line.startswith('source = "registry+') and 'github.com/rust-lang/crates.io-index' not in line:
                    found.append(finding(rel, "dependency-custom-registry", n, line))
        # Audit every tracked text file, including documentation and fixtures.
        # Only the scanner and its synthetic adversarial tests contain detection literals.
        if rel in ("scripts/check-public-boundary.py", "scripts/test-public-boundary.py"):
            continue
        try:
            content = p.read_text()
        except (UnicodeError, OSError):
            continue
        for n, line in enumerate(content.splitlines(), 1):
            for rule, pattern in TOKENS.items():
                if pattern.search(line):
                    found.append(finding(rel, rule, n, line))
    return found


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parents[1])
    parser.add_argument("--strict", action="store_true")
    parser.add_argument("--write-baseline", action="store_true", help="explicitly record personal debt for review")
    args = parser.parse_args()
    root = args.root.resolve()
    paths = subprocess.check_output(["git", "ls-files", "-z"], cwd=root).decode().split("\0")
    found = scan(root, [p for p in paths if p])
    baseline_path = root / "scripts/public-boundary-baseline.json"
    if args.write_baseline:
        debt = [x for x in found if not x["rule"].startswith("dependency-")]
        baseline_path.write_text(json.dumps({"version": 1, "status": "known-debt-not-release-approval", "findings": debt}, indent=2) + "\n")
    baseline = Counter()
    if baseline_path.exists() and not args.strict:
        baseline = Counter(x["fingerprint"] for x in json.loads(baseline_path.read_text())["findings"])
    violations = []
    for x in found:
        if not x["rule"].startswith("dependency-") and baseline[x["fingerprint"]] > 0:
            baseline[x["fingerprint"]] -= 1
        else:
            violations.append(x)
    print(json.dumps({"mode": "strict" if args.strict else "baseline", "known_personal_findings": len(found) - len(violations), "violations": violations}, indent=2))
    return 1 if violations else 0


if __name__ == "__main__":
    sys.exit(main())
