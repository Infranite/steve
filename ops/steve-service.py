#!/usr/bin/env python3
"""User services for a verified Steve release (Python 3.9+).

python3 ops/steve-service.py {hub|node} {install|start|stop|restart|status|doctor|update}
    [--home PATH] [--release EXTRACTED_BUNDLE] [--label LABEL] [--wait SECONDS]

install registers a service without starting it or pairing a Node. update switches only a stopped Node;
it cannot establish that a running Node has drained, and never kills one to perform an update. Hub updates
use Settings > About, or ops/upgrade.py for a signed macOS Hub at the OS account's default ~/.steve,
so migrations, backups and task drain remain owned by the upgrade coordinator.
"""
import argparse
import json
import os
from pathlib import Path
import platform
import plistlib
import re
import shlex
import subprocess
import sys
import time
import uuid


class Refused(Exception):
    pass


def read_json(path, default=None):
    if not path.exists():
        return default
    return json.loads(path.read_text())


def atomic(path, content):
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_name("." + path.name + "." + uuid.uuid4().hex)
    try:
        fd = os.open(tmp, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
        with os.fdopen(fd, "wb") as f:
            f.write(content)
            f.flush()
            os.fsync(f.fileno())
        os.replace(tmp, path)
    finally:
        tmp.unlink(missing_ok=True)


def unit_quote(value, command=False):
    value = str(value)
    if any(c in value for c in "\n\r\0"):
        raise Refused("service paths and environment cannot contain newlines or NUL")
    value = value.replace("\\", "\\\\").replace('"', '\\"').replace("%", "%%")
    if command:
        value = value.replace("$", "$$")
    return '"' + value + '"'


class Service:
    def __init__(self, component, home, label=None, system=None, user_home=None, runner=None):
        self.component = component
        self.home = Path(home).expanduser().resolve()
        self.system = system or platform.system()
        if self.system not in ("Darwin", "Linux"):
            raise Refused("supported service managers: macOS launchd and Linux systemd --user")
        self.user_home = Path(user_home or Path.home())
        self.record_path = self.home / "services" / (component + ".json")
        self.record = read_json(self.record_path, {})
        identity = read_json(self.home / "signing/identity.json", {}) if component == "hub" else {}
        self.signed = bool(identity)
        self.label = label or self.record.get("label") or identity.get("label") or "io.github.infranite.steve." + component
        if self.signed and self.label != identity.get("label"):
            raise Refused("signed Hub service label must match signing/identity.json; changing it would break upgrades")
        if not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9._-]{0,200}", self.label):
            raise Refused("invalid service label")
        if self.record.get("label") and self.record["label"] != self.label:
            raise Refused("this home already uses another service label")
        if self.system == "Darwin":
            self.config = self.user_home / "Library/LaunchAgents" / (self.label + ".plist")
        else:
            self.config = self.user_home / ".config/systemd/user" / (self.label + ".service")
        self.run = runner or subprocess.run

    def command(self, args, check=True):
        try:
            result = self.run(args, capture_output=True, text=True, timeout=20)
        except (OSError, subprocess.TimeoutExpired) as e:
            raise Refused(f"service manager unavailable: {e}") from e
        if check and result.returncode:
            raise Refused(f"service manager command failed ({args[0]}): {result.stderr.strip()}")
        return result

    def check_owner(self):
        if not self.config.exists():
            return
        try:
            if self.system == "Darwin":
                definition = plistlib.loads(self.config.read_bytes())
                environment = definition.get("EnvironmentVariables", {})
                owner = environment.get("STEVE_HOME")
                # The original signed Hub plist predates an explicit STEVE_HOME. Its exact entry
                # point uses $HOME/.steve by default; no other home, command or environment is
                # evidence of that ownership. In particular Program can override argv[0].
                legacy_script = self.home / "current/ops/run.sh"
                if (self.component == "hub" and self.signed and not environment
                        and self.home == (self.user_home / ".steve").resolve()
                        and definition.get("Label") == self.label
                        and definition.get("ProgramArguments") == ["/bin/zsh", str(legacy_script)]
                        and not any(key in definition for key in ("Program", "RootDirectory", "UserName", "GroupName"))
                        and legacy_script.is_file()):
                    return
            else:
                owner = None
                for line in self.config.read_text().splitlines():
                    if line.startswith("Environment="):
                        for assignment in shlex.split(line[len("Environment="):]):
                            if assignment.startswith("STEVE_HOME="):
                                owner = assignment[len("STEVE_HOME="):].replace("%%", "%")
            if not owner or Path(owner).expanduser().resolve() != self.home:
                raise Refused("service label belongs to another or unknown home; use that home's original --label")
        except (ValueError, plistlib.InvalidFileException) as e:
            raise Refused("existing service definition cannot establish its home; inspect it before adoption") from e

    def state(self):
        self.check_owner()
        if self.system == "Darwin":
            r = self.command(["launchctl", "print", f"gui/{os.getuid()}/{self.label}"], check=False)
            pid = re.search(r"^\s*pid = (\d+)\s*$", r.stdout, re.M)
            state = re.search(r"^\s*state = (.+)$", r.stdout, re.M)
            # A failed print is expected for an installed but unloaded job. Other failures are unknown,
            # never proof that replacing its executable would be safe.
            absent = r.returncode != 0 and any(s in (r.stderr + r.stdout).lower()
                                                for s in ("could not find service", "no such process"))
            return dict(installed=self.config.is_file(), loaded=r.returncode == 0,
                        running=bool(pid) if r.returncode == 0 else (False if absent else None),
                        pid=int(pid.group(1)) if pid else None,
                        state=state.group(1).strip() if state else ("stopped" if absent else "unknown"))
        r = self.command(["systemctl", "--user", "show", self.label + ".service", "--no-pager",
                          "--property=LoadState,ActiveState,MainPID,ExecMainStatus"], check=False)
        values = dict(line.split("=", 1) for line in r.stdout.splitlines() if "=" in line)
        pid = int(values.get("MainPID", "0") or "0")
        active = values.get("ActiveState", "unknown")
        running = bool(pid) or active in ("active", "activating", "deactivating", "reloading")
        if not values or active == "unknown":
            running = None
        return dict(installed=self.config.is_file(), loaded=values.get("LoadState") == "loaded",
                    running=running, pid=pid or None, state=active)

    def bundle(self, release):
        release = Path(release).expanduser().resolve()
        manifest = read_json(release / "release.json")
        if not isinstance(manifest, dict) or manifest.get("format_version") != 1:
            raise Refused("release.json is missing or unsupported; use a verified release bundle")
        binary = release / "bin" / ("steve-" + self.component)
        if not binary.is_file() or binary.is_symlink() or not os.access(binary, os.X_OK):
            raise Refused(f"missing executable {binary}")
        try:
            info = json.loads(subprocess.check_output([str(binary), "--build-info"], timeout=15))
        except (OSError, ValueError, subprocess.SubprocessError) as e:
            raise Refused(f"cannot read {self.component} build identity: {e}") from e
        if any(info.get(k) != manifest.get(k) for k in ("version", "commit")):
            raise Refused("binary version/commit differs from release manifest")
        if self.component == "hub":
            web = read_json(release / "web/dist/release.json", {})
            if any(web.get(k) != manifest.get(k) for k in ("version", "commit")):
                raise Refused("Web version/commit differs from release manifest")
        return release, info

    def definition(self, release):
        env = dict(STEVE_HOME=str(self.home), PATH=os.environ.get("PATH", "/usr/local/bin:/usr/bin:/bin"))
        if self.component == "hub":
            script = "run.sh" if self.signed else "run-managed.sh"
            # Hub's coordinator switches current; Node service definitions pin a concrete release instead.
            command = ["/bin/zsh" if self.signed else "/bin/sh", str(self.home / "current/ops" / script)]
            if not (release / "ops" / script).is_file():
                raise Refused(f"release has no runtime ops/{script}")
        else:
            command = [str(release / "bin/steve-node"), "run"]
            pairing = read_json(self.home / "node/node.json", {})
            if pairing.get("loopback") is True:
                env["STEVE_NODE_LOOPBACK_PLAINTEXT"] = "1"
        if self.system == "Darwin":
            return plistlib.dumps(dict(Label=self.label, ProgramArguments=command, EnvironmentVariables=env,
                                       RunAtLoad=True, KeepAlive=True, ExitTimeOut=600,
                                       StandardOutPath=str(self.home / "logs" / (self.component + ".log")),
                                       StandardErrorPath=str(self.home / "logs" / (self.component + ".log"))))
        lines = ["[Unit]", "Description=Steve " + self.component, "[Service]", "Type=simple",
                 "ExecStart=" + " ".join(unit_quote(a, command=True) for a in command),
                 *["Environment=" + unit_quote(k + "=" + v) for k, v in env.items()],
                 "Restart=on-failure", "RestartSec=3", "KillMode=control-group", "TimeoutStopSec=600",
                 "[Install]", "WantedBy=default.target", ""]
        return "\n".join(lines).encode()

    def install(self, release, update=False, wait=0):
        release, info = self.bundle(release)
        if update and self.component == "hub":
            raise Refused("Hub updates use Settings > About, or ops/upgrade.py for signed macOS default-home installs, for task drain, backup and schema checks")
        if self.component == "hub" and (self.home / "current").exists() and (self.home / "current").resolve() != release:
            raise Refused("current Hub already exists; use Settings > About or the supported ops/upgrade.py entry to update it")
        if self.component == "hub" and self.system == "Darwin" and not self.signed:
            raise Refused("new macOS Hub needs explicit Keychain setup: run local-release.py setup-keychain <bundle> first")
        data = self.definition(release)
        same = self.config.is_file() and self.config.read_bytes() == data
        state = self.state()
        if not same:
            deadline = time.monotonic() + wait
            while (state["running"] is True or (self.system == "Darwin" and state["loaded"])) and time.monotonic() < deadline:
                time.sleep(min(1, max(0, deadline - time.monotonic())))
                state = self.state()
            if state["running"] is not False or (self.system == "Darwin" and state["loaded"]):
                raise Refused("service is running or its state is unknown; no files changed. Finish Node sessions, "
                              "verify Devices, then stop the service before update; --wait only waits for a stopped service")
            # A loaded launchd job can acquire a PID after observation (KeepAlive). Only an explicit stop
            # may unload it; install/update never bootout a job on the assumption that no PID means idle.
        self.home.mkdir(parents=True, exist_ok=True)
        (self.home / "logs").mkdir(mode=0o700, exist_ok=True)
        if self.component == "hub" and not (self.home / "current").exists():
            current = self.home / "current"
            if current.is_symlink():
                raise Refused("current is a broken symlink; inspect it before installation")
            current.symlink_to(release)
        if self.component == "hub":
            web_link = self.home / "web/current"
            web_link.parent.mkdir(parents=True, exist_ok=True)
            if not web_link.exists() and not web_link.is_symlink():
                web_link.symlink_to(release / "web/dist")
        if not same:
            if self.config.exists():
                atomic(self.config.with_suffix(self.config.suffix + ".previous"), self.config.read_bytes())
            atomic(self.config, data)
        self.record = dict(component=self.component, label=self.label, release=str(release), build=info,
                           previous=self.record.get("release") if update and not same else self.record.get("previous"))
        atomic(self.record_path, (json.dumps(self.record, indent=2) + "\n").encode())
        if self.system == "Linux":
            self.command(["systemctl", "--user", "daemon-reload"])
            self.command(["systemctl", "--user", "enable", self.label + ".service"])
        return dict(installed=True, changed=not same, started=False, release=str(release), build=info)

    def action(self, action):
        state = self.state()
        if action == "stop" and state["running"] is False and not state["loaded"]:
            return state
        if not self.config.is_file():
            raise Refused("service is not installed; run install --release <verified bundle> first")
        if self.system == "Darwin":
            domain = f"gui/{os.getuid()}"
            target = domain + "/" + self.label
            if action in ("stop", "restart") and state["loaded"]:
                self.command(["launchctl", "bootout", target])
            if action in ("start", "restart"):
                if action == "start" and state["loaded"]:
                    if not state["running"]:
                        self.command(["launchctl", "kickstart", target])
                else:
                    self.command(["launchctl", "bootstrap", domain, str(self.config)])
        else:
            self.command(["systemctl", "--user", action, self.label + ".service"])
        return self.status()

    def status(self, candidate=None, doctor=False):
        state = self.state()
        release = self.home / "current" if self.component == "hub" else Path(self.record.get("release", self.home / "current"))
        report = dict(component=self.component, home=str(self.home), manager="launchd" if self.system == "Darwin" else "systemd-user",
                      label=self.label, service=state, configured_release=str(release.resolve()),
                      configured_build=self.record.get("build"), connection="unknown")
        try:
            _, report["configured_build"] = self.bundle(release)
        except (Refused, OSError, ValueError) as e:
            report["build_error"] = str(e)
        # A PID is not evidence of a binary version (current may have switched during an upgrade).
        # Connection and session counts likewise belong to the Hub; do not infer them from local files.
        report["running_build"] = None
        report["runtime_identity"] = "unknown; verify Hub /healthz or Node handshake in Devices"
        if self.component == "node":
            pairing = read_json(self.home / "node/node.json", {})
            report["pairing"] = dict(configured=bool(pairing), identity_present=(self.home / "node/identity.key").is_file(),
                                     node_id=pairing.get("node_id"), hub_url=pairing.get("hub_url"))
            report["sessions"] = "unknown; inspect Hub Devices before stopping"
        if candidate:
            path, info = self.bundle(candidate)
            report["candidate"] = dict(release=str(path), build=info)
            report["update_command"] = shlex.join([sys.executable, str(Path(__file__).resolve()), self.component,
                "update", "--home", str(self.home), "--label", self.label, "--release", str(path)]) if self.component == "node" else "Settings > About"
        if self.component == "hub" and self.system == "Darwin" and self.signed:
            default_home = (self.user_home / ".steve").resolve()
            supported = self.home == default_home
            report["automatic_upgrade"] = dict(supported=supported,
                entry="Settings > About or ops/upgrade.py" if supported else "local-release.py prepare-bundle, then deploy",
                reason=None if supported else "signed runner grants Keychain access only to the default ~/.steve home; "
                "custom homes require explicit terminal preparation/deployment with their original signing identity")
            if supported:
                report["upgrade_check_command"] = shlex.join([sys.executable, str(Path(__file__).with_name("upgrade.py")), "check"])
                if candidate:
                    report["update_command"] = "Settings > About or ops/upgrade.py"
        if doctor:
            issues = []
            if not state["installed"]:
                issues.append("service is not installed")
            if not state["running"]:
                issues.append("service is stopped or state is unknown")
            if report.get("build_error"):
                issues.append(report["build_error"])
            if self.component == "node":
                for name in ("roots.json", "policy.json", "harness.json"):
                    try:
                        read_json(self.home / name)
                    except ValueError:
                        issues.append(name + " is not valid JSON")
                if not report["pairing"]["configured"]:
                    issues.append("Node is not paired; install never creates or approves a pairing")
                if not report["pairing"]["identity_present"]:
                    issues.append("Node identity is missing")
                issues.append("connection and active sessions require Hub Devices verification")
            if report.get("automatic_upgrade", {}).get("supported") is False:
                issues.append(report["automatic_upgrade"]["reason"])
            report["diagnostics"] = issues
        return report


def main():
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("component", choices=("hub", "node"))
    p.add_argument("action", choices=("install", "start", "stop", "restart", "status", "doctor", "update"))
    p.add_argument("--home")
    p.add_argument("--release")
    p.add_argument("--label")
    p.add_argument("--wait", type=float, default=0)
    a = p.parse_args()
    home = a.home or os.environ.get("STEVE_HOME") or str(Path.home() / (".steve" if a.component == "hub" else ".steve-node"))
    try:
        service = Service(a.component, home, a.label)
        if a.action in ("install", "update"):
            release = a.release or service.record.get("release") or str(Path(home) / "current")
            result = service.install(release, update=a.action == "update", wait=max(0, a.wait))
        elif a.action in ("status", "doctor"):
            result = service.status(a.release, doctor=a.action == "doctor")
        else:
            result = service.action(a.action)
        print(json.dumps(result, indent=2))
        return 0
    except (Refused, OSError, ValueError) as e:
        print(json.dumps(dict(error=str(e))), file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
