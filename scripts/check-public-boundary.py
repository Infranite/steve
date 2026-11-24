#!/usr/bin/env python3
"""Audit public-tree dependencies, process documents, and private identifiers without printing contents.

Built-in rules cover generic boundary leaks (home paths, mounted volumes, numbered wiki links,
hardware model IDs, proposal front matter, external/git dependencies, and unallowlisted Markdown).
Maintainer-specific personal identifiers must live outside the public repository and be supplied via
`--rules <path>`, `STEVE_PUBLIC_BOUNDARY_RULES`, `STEVE_PUBLIC_BOUNDARY_PATTERNS`, or the optional
user config `~/.config/steve/public-boundary-rules.json`.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import sys

try:
    import tomllib
except ModuleNotFoundError:
    raise SystemExit("This check requires Python 3.11+ (tomllib); run python3.13 on the development Mac.")


BUILTIN_TOKENS = {
    "personal-home": re.compile(
        r"/(?:Users|home)/(?!runner(?:/|$)|user(?:/|$)|example(?:/|$))[A-Za-z0-9._-]+/"
    ),
    "private-volume": re.compile(
        r"/Volumes/(?!Macintosh[ _]HD(?:/|$))[A-Za-z0-9._-]+"
    ),
    "private-wiki-link": re.compile(
        r"\[\[\d+_[^\]\n]+/[^\]\n]+\]\]"
    ),
    "hardware-model-id": re.compile(
        r"\b(?:Mac|MacBookPro|MacBookAir|Macmini|MacStudio|iMac)\d+,\d+\b"
    ),
}

PROCESS_DOC_FRONTMATTER_RE = re.compile(
    r"^type:\s*(?:proposal|rfc|adr|design-doc|runbook|review)\b",
    re.I,
)

# Design notes, plans, specs and runbooks stay out of the public tree. Only user-facing docs,
# release notes, and explicitly allowlisted test fixture directories may hold Markdown files.
USER_DOCS = {
    "README.md",
    "README.zh-CN.md",
    "CONTRIBUTING.md",
    "AGENTS.md",
    "CLAUDE.md",
    "docs/deployment.md",
}
USER_DOC_DIRS = ("docs/assets/",)
RELEASE_NOTE_RE = re.compile(r"^docs/releases/\d+\.\d+\.\d+\.md$")
TEST_DOC_PREFIXES = (
    "hub-rs/crates/steve-channels/tests/fixtures/",
    "hub-rs/crates/steve-github/tests/fixtures/",
    "hub-rs/crates/steve-knowledge/tests/eval/corpus/",
)

DEFAULT_USER_RULES_PATH = Path.home() / ".config/steve/public-boundary-rules.json"
ENV_RULES_FILES = "STEVE_PUBLIC_BOUNDARY_RULES"
ENV_RULES_PATTERNS = "STEVE_PUBLIC_BOUNDARY_PATTERNS"
ENV_NO_DEFAULT_RULES = "STEVE_PUBLIC_BOUNDARY_NO_DEFAULT_RULES"

JS_DEP_SECTIONS = (
    "dependencies",
    "devDependencies",
    "optionalDependencies",
    "peerDependencies",
    "overrides",
    "resolutions",
)
JS_LOCAL_PREFIXES = ("file:", "link:", "portal:")
JS_GIT_OR_URL_PREFIXES = (
    "git+",
    "git://",
    "ssh://",
    "github:",
    "gitlab:",
    "bitbucket:",
    "http://",
    "https://",
)
NPM_PUBLIC_REGISTRY = "https://registry.npmjs.org/"

SWIFT_COMMENT_OR_STRING_RE = re.compile(r'"(?:\\.|[^"\\])*"|//[^\n]*|/\*.*?\*/', re.S)
SWIFT_PATH_RE = re.compile(r"\.(?:package|binaryTarget)\s*\([^)]*\bpath\s*:\s*\"([^\"]+)\"", re.S)
SWIFT_URL_RE = re.compile(r"\.(?:package|binaryTarget)\s*\([^)]*\burl\s*:\s*\"([^\"]+)\"", re.S)


def process_document(rel):
    if (
        rel in USER_DOCS
        or rel.startswith(USER_DOC_DIRS)
        or RELEASE_NOTE_RE.match(rel)
        or rel.startswith(TEST_DOC_PREFIXES)
    ):
        return False
    return rel.startswith("docs/") or rel.endswith((".md", ".markdown"))


def finding(path, rule, line, text):
    # Stable under line movement; never write matching contents or credentials into reports.
    return {
        "path": path,
        "rule": rule,
        "line": line,
        "fingerprint": hashlib.sha256((path + "\0" + rule + "\0" + text.strip()).encode()).hexdigest(),
    }


def _compile_rule(name, spec):
    if not isinstance(name, str) or not name.strip():
        raise ValueError("private rule name must be a non-empty string")
    if isinstance(spec, str):
        pattern = spec
        flags = re.I
    elif isinstance(spec, dict):
        has_pattern = "pattern" in spec
        has_literal = "literal" in spec
        if has_pattern == has_literal:
            raise ValueError(f"private rule '{name}' must specify exactly one of 'pattern' or 'literal'")
        if has_literal:
            raw = spec["literal"]
            if not isinstance(raw, str) or not raw:
                raise ValueError(f"private rule '{name}' literal must be a non-empty string")
            pattern = re.escape(raw)
        else:
            raw = spec["pattern"]
            if not isinstance(raw, str) or not raw:
                raise ValueError(f"private rule '{name}' pattern must be a non-empty string")
            pattern = raw
        flags = re.I if spec.get("ignore_case", True) else 0
    else:
        raise ValueError(f"private rule '{name}' must be a regex string or mapping")
    try:
        return re.compile(pattern, flags)
    except re.error as exc:
        # Never echo the raw pattern in diagnostics.
        raise ValueError(f"invalid regex in private rule '{name}': {type(exc).__name__}") from None


def parse_private_rules(data):
    """Parse private rules from a JSON/TOML structure without exposing pattern text on error."""
    rules = {}
    if isinstance(data, dict) and "rules" in data and isinstance(data["rules"], list):
        for entry in data["rules"]:
            if not isinstance(entry, dict) or "name" not in entry:
                raise ValueError("each entry in 'rules' must be an object with a 'name'")
            name = entry["name"]
            rules[name] = _compile_rule(name, entry)
        return rules
    if isinstance(data, dict):
        for name, spec in data.items():
            if name == "version":
                continue
            rules[name] = _compile_rule(name, spec)
        return rules
    raise ValueError("private rules document must be a JSON/TOML object")


def load_private_rules_file(path, root=None):
    resolved = Path(path).expanduser().resolve()
    if root is not None and resolved.is_relative_to(Path(root).resolve()):
        raise ValueError("private rules file must live outside the public repository")
    if not resolved.is_file():
        raise FileNotFoundError(f"private rules file not found: {resolved}")
    raw = resolved.read_text(encoding="utf-8")
    if resolved.suffix.lower() == ".toml":
        data = tomllib.loads(raw)
    else:
        data = json.loads(raw)
    return parse_private_rules(data)


def load_private_rules(root, cli_paths=None, env=None, use_default_user_config=True):
    env = os.environ if env is None else env
    rules = {}
    if use_default_user_config and env.get(ENV_NO_DEFAULT_RULES) != "1":
        if DEFAULT_USER_RULES_PATH.is_file():
            rules.update(load_private_rules_file(DEFAULT_USER_RULES_PATH, root=root))
    env_files = env.get(ENV_RULES_FILES, "").strip()
    if env_files:
        for item in env_files.split(os.pathsep):
            item = item.strip()
            if item:
                rules.update(load_private_rules_file(item, root=root))
    for path in cli_paths or ():
        rules.update(load_private_rules_file(path, root=root))
    env_patterns = env.get(ENV_RULES_PATTERNS, "").strip()
    if env_patterns:
        try:
            data = json.loads(env_patterns)
        except ValueError:
            raise ValueError(f"{ENV_RULES_PATTERNS} must contain valid JSON") from None
        rules.update(parse_private_rules(data))
    return rules


def cargo_dependencies(value):
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
                yield from cargo_dependencies(child)


def _path_outside_root(raw, base_dir, root_resolved, must_be_dir=False):
    if not raw or raw.startswith("~"):
        return True
    p = Path(raw)
    if p.is_absolute():
        return True
    resolved = (base_dir / p).resolve()
    if not resolved.is_relative_to(root_resolved):
        return True
    return not (resolved.is_dir() if must_be_dir else resolved.exists())


def _walk_js_dep_values(obj):
    if isinstance(obj, str):
        yield obj
    elif isinstance(obj, dict):
        for v in obj.values():
            yield from _walk_js_dep_values(v)


def _scan_package_json(rel, p, root_resolved):
    found = []
    try:
        data = json.loads(p.read_text(encoding="utf-8"))
    except (ValueError, UnicodeError) as exc:
        return [finding(rel, "dependency-manifest-invalid", 0, type(exc).__name__)]
    if not isinstance(data, dict):
        return [finding(rel, "dependency-manifest-invalid", 0, "not-object")]
    workspaces = data.get("workspaces")
    ws_list = workspaces.get("packages", []) if isinstance(workspaces, dict) else workspaces
    if isinstance(ws_list, list):
        for entry in ws_list:
            if not isinstance(entry, str):
                continue
            cleaned = entry.lstrip("!")
            if cleaned.startswith(("~", "/")) or ".." in Path(cleaned).parts:
                found.append(finding(rel, "dependency-outside-public-tree", 0, cleaned))
    for section in JS_DEP_SECTIONS:
        block = data.get(section)
        if not isinstance(block, dict):
            continue
        for spec in _walk_js_dep_values(block):
            val = spec.strip()
            local_raw = None
            for prefix in JS_LOCAL_PREFIXES:
                if val.startswith(prefix):
                    local_raw = val[len(prefix):]
                    break
            if local_raw is None and val.startswith(("./", "../", "/", "~/")):
                local_raw = val
            if local_raw is not None:
                if _path_outside_root(local_raw, p.parent, root_resolved):
                    found.append(finding(rel, "dependency-outside-public-tree", 0, val))
            elif val.startswith(JS_GIT_OR_URL_PREFIXES):
                found.append(finding(rel, "dependency-git-needs-review", 0, val))
    return found


def _scan_package_lock(rel, p, root_resolved):
    found = []
    try:
        data = json.loads(p.read_text(encoding="utf-8"))
    except (ValueError, UnicodeError) as exc:
        return [finding(rel, "dependency-manifest-invalid", 0, type(exc).__name__)]
    packages = data.get("packages") if isinstance(data, dict) else None
    if not isinstance(packages, dict):
        return found
    for pkg_key, entry in packages.items():
        if not isinstance(entry, dict):
            continue
        resolved = entry.get("resolved")
        if not isinstance(resolved, str) or not resolved:
            continue
        if entry.get("link") is True:
            if _path_outside_root(resolved, p.parent, root_resolved):
                found.append(finding(rel, "dependency-outside-public-tree", 0, f"{pkg_key}:{resolved}"))
        elif resolved.startswith("file:"):
            if _path_outside_root(resolved[len("file:"):], p.parent, root_resolved):
                found.append(finding(rel, "dependency-outside-public-tree", 0, f"{pkg_key}:{resolved}"))
        elif resolved.startswith(("git+", "git://", "ssh://")):
            found.append(finding(rel, "dependency-git-needs-review", 0, f"{pkg_key}:{resolved}"))
        elif resolved.startswith(("http://", "https://")) and not resolved.startswith(NPM_PUBLIC_REGISTRY):
            found.append(finding(rel, "dependency-custom-registry", 0, f"{pkg_key}:{resolved}"))
    return found


def _strip_swift_comments(text):
    def repl(match):
        token = match.group(0)
        if token.startswith('"'):
            return token
        return "\n" * token.count("\n")

    return SWIFT_COMMENT_OR_STRING_RE.sub(repl, text)


def _scan_package_swift(rel, p, root_resolved):
    found = []
    try:
        raw_text = p.read_text(encoding="utf-8")
    except (UnicodeError, OSError):
        return found
    code = _strip_swift_comments(raw_text)
    for match in SWIFT_PATH_RE.finditer(code):
        raw = match.group(1)
        if _path_outside_root(raw, p.parent, root_resolved):
            line = code.count("\n", 0, match.start()) + 1
            found.append(finding(rel, "dependency-outside-public-tree", line, raw))
    for match in SWIFT_URL_RE.finditer(code):
        url = match.group(1)
        line = code.count("\n", 0, match.start()) + 1
        if url.startswith("file://"):
            local_path = url[len("file://"):]
            if _path_outside_root(local_path, p.parent, root_resolved):
                found.append(finding(rel, "dependency-outside-public-tree", line, url))
        else:
            found.append(finding(rel, "dependency-git-needs-review", line, url))
    return found


def scan(root, paths, extra_tokens=None):
    root_resolved = Path(root).resolve()
    tokens = dict(BUILTIN_TOKENS)
    if extra_tokens:
        tokens.update(extra_tokens)
    found = []
    for rel in sorted(paths):
        p = root_resolved / rel
        if rel.startswith(".steve/"):
            found.append(finding(rel, "private-runtime-artifact", 0, rel))
        if process_document(rel):
            found.append(finding(rel, "process-document", 0, rel))
        if not p.is_file():
            continue
        if p.name == "Cargo.toml":
            try:
                data = tomllib.loads(p.read_text(encoding="utf-8"))
            except (ValueError, UnicodeError) as exc:
                found.append(finding(rel, "dependency-manifest-invalid", 0, type(exc).__name__))
                continue
            for dep in cargo_dependencies(data):
                raw = dep.get("path")
                if raw is not None and _path_outside_root(str(raw), p.parent, root_resolved, must_be_dir=True):
                    found.append(finding(rel, "dependency-outside-public-tree", 0, str(raw)))
                if "git" in dep:
                    found.append(finding(rel, "dependency-git-needs-review", 0, str(dep["git"])))
                if "registry" in dep:
                    found.append(finding(rel, "dependency-custom-registry", 0, str(dep["registry"])))
        elif p.name == "Cargo.lock":
            for n, line in enumerate(p.read_text(encoding="utf-8").splitlines(), 1):
                if line.startswith('source = "git+'):
                    found.append(finding(rel, "dependency-git-needs-review", n, line))
                elif line.startswith('source = "registry+') and "github.com/rust-lang/crates.io-index" not in line:
                    found.append(finding(rel, "dependency-custom-registry", n, line))
        elif p.name == "package.json":
            found.extend(_scan_package_json(rel, p, root_resolved))
        elif p.name == "package-lock.json":
            found.extend(_scan_package_lock(rel, p, root_resolved))
        elif p.name == "Package.swift":
            found.extend(_scan_package_swift(rel, p, root_resolved))

        try:
            content = p.read_text(encoding="utf-8")
        except (UnicodeError, OSError):
            continue
        lines = content.splitlines()
        if rel.endswith((".md", ".markdown")) and lines and lines[0].strip() == "---":
            for n, line in enumerate(lines[1:], 2):
                if line.strip() == "---":
                    break
                if PROCESS_DOC_FRONTMATTER_RE.search(line):
                    found.append(finding(rel, "process-document-frontmatter", n, line))
        for n, line in enumerate(lines, 1):
            for rule, pattern in tokens.items():
                if pattern.search(line):
                    found.append(finding(rel, rule, n, line))
    return found


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parents[1])
    parser.add_argument(
        "--strict",
        action="store_true",
        help="strict mode is always enforced; flag kept for CLI compatibility",
    )
    parser.add_argument(
        "--rules",
        action="append",
        default=[],
        help="external JSON/TOML file with private personal-identifier rules (must be outside --root)",
    )
    parser.add_argument(
        "--no-default-rules",
        action="store_true",
        help="ignore ~/.config/steve/public-boundary-rules.json if present",
    )
    args = parser.parse_args(argv)
    root = args.root.resolve()
    try:
        private_rules = load_private_rules(
            root,
            cli_paths=args.rules,
            use_default_user_config=not args.no_default_rules,
        )
    except (OSError, ValueError) as exc:
        print(json.dumps({"mode": "strict", "error": str(exc)}, indent=2), file=sys.stderr)
        return 2
    paths = subprocess.check_output(["git", "ls-files", "-z"], cwd=root).decode().split("\0")
    violations = scan(root, [p for p in paths if p], extra_tokens=private_rules)
    print(
        json.dumps(
            {
                "mode": "strict",
                "private_rules_loaded": len(private_rules),
                "violations": violations,
            },
            indent=2,
        )
    )
    return 1 if violations else 0


if __name__ == "__main__":
    sys.exit(main())
