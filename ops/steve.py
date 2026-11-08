#!/usr/bin/env python3
"""Steve's installed CLI (Python 3.9+). Package files are read-only inputs.

setup owns only a fresh home or its own setup.json receipt. It never adopts a previous
installation. Lifecycle and upgrades remain delegated to the existing service/coordinator.
"""
import argparse
import contextlib
import fcntl
import getpass
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import shlex
import shutil
import stat
import subprocess
import sys
import urllib.parse
import uuid


class Refused(Exception):
    pass


def read_json(path):
    if path.is_symlink() or not path.is_file() or path.stat().st_size > 1024 * 1024:
        raise Refused(f"不是有效的元数据文件：{path}")
    value = json.loads(path.read_text())
    if not isinstance(value, dict):
        raise Refused(f"元数据必须是 JSON 对象：{path}")
    return value


def digest(path):
    h = hashlib.sha256()
    with path.open("rb") as source:
        for block in iter(lambda: source.read(1024 * 1024), b""):
            h.update(block)
    return h.hexdigest()


def private_file(path, data):
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    with os.fdopen(fd, "wb") as output:
        output.write(data)
        output.flush()
        os.fsync(output.fileno())


def canonical(path):
    path = Path(path).expanduser().absolute()
    if path.is_symlink():
        raise Refused(f"路径不能是符号链接：{path}")
    return path.parent.resolve() / path.name


def private_owned(path, directory=False):
    s = path.lstat()
    kind = stat.S_ISDIR(s.st_mode) if directory else stat.S_ISREG(s.st_mode)
    if not kind or s.st_uid != os.getuid() or stat.S_IMODE(s.st_mode) != (0o700 if directory else 0o600):
        raise Refused(f"路径必须由当前用户拥有，权限为 {'0700' if directory else '0600'}：{path}")


def within(child, parent):
    return child == parent or parent in child.parents


def url(value, label):
    value = value.strip().rstrip("/")
    u = urllib.parse.urlsplit(value)
    try:
        u.port
    except ValueError as e:
        raise Refused(f"{label}端口无效") from e
    if (not u.hostname or u.username or u.password or u.query or u.fragment
            or u.scheme not in ("http", "https")
            or any(c.isspace() or ord(c) < 32 for c in value)):
        raise Refused(f"{label}需要完整的 HTTP(S) URL，不能包含账号、查询参数或片段")
    if u.scheme != "https" and u.hostname not in ("localhost", "127.0.0.1", "::1"):
        raise Refused(f"{label}跨机器访问必须使用 HTTPS")
    return value


class CLI:
    def __init__(self, bundle=None, runner=None, ask=None, secret=None, system=None, machine=None, user_home=None):
        self.bundle = Path(bundle or Path(__file__).resolve().parent.parent).resolve()
        self.run = runner or subprocess.run
        self.ask = ask or input
        self.secret = secret or getpass.getpass
        self.secret_terminal = secret is not None or sys.stdin.isatty()
        self.system = system or platform.system()
        self.machine = machine or platform.machine()
        self.user_home = Path(user_home or Path.home())
        self.home = None
        self.receipt = None

    def home_for(self, role, value=None):
        # STEVE_HOME inherited from a shell must not collapse Hub and Node into one home.
        return canonical(value or self.user_home / (".steve" if role == "hub" else ".steve-node"))

    def environment(self, values=None):
        env = {k: v for k, v in os.environ.items() if not k.startswith("STEVE_")}
        if self.home is not None:
            env["STEVE_HOME"] = str(self.home)
        env.update(values or {})
        return env

    def command(self, args, capture=False, values=None):
        # stdin is inherited, including for pairing. No pairing code is read from env/argv.
        result = self.run([str(x) for x in args], env=self.environment(values),
                          capture_output=capture, text=True, check=False)
        if result.returncode:
            if capture:
                try:
                    report = json.loads(result.stdout or "{}")
                    errors = report.get("errors", [report.get("error")])
                    if any(errors):
                        raise Refused("；".join(str(e) for e in errors if e))
                except (ValueError, AttributeError):
                    pass
            raise Refused(f"命令失败（退出码 {result.returncode}）；已完成的步骤保留，可修复后重试。")
        return result.stdout if capture else ""

    def prompt(self, text, default=None):
        answer = self.ask(text + (f" [{default}]" if default is not None else "") + ": ").strip()
        answer = answer or default
        if not answer or any(c in answer for c in "\r\n\0"):
            raise Refused("需要填写有效值；尚未完成的设置可以稍后继续。")
        return answer

    def manifest(self):
        m = read_json(self.bundle / "release.json")
        if (m.get("format_version") != 1 or not re.fullmatch(r"(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)", str(m.get("version", "")))
                or not re.fullmatch(r"[0-9a-f]{40}", str(m.get("commit", ""))) or not isinstance(m.get("schema"), dict)):
            raise Refused("安装包身份无效；请使用可信的正式 Release 包。")
        return m

    def verify_bundle(self, root, m):
        target = {("Darwin", "arm64"): "aarch64-apple-darwin",
                  ("Darwin", "aarch64"): "aarch64-apple-darwin",
                  ("Linux", "x86_64"): "x86_64-unknown-linux-gnu",
                  ("Linux", "amd64"): "x86_64-unknown-linux-gnu"}.get((self.system, self.machine))
        if not target:
            raise Refused("此平台尚无受支持的 Steve Release 包。")
        for p in [root, *root.rglob("*")]:
            mode = p.lstat().st_mode
            if not (stat.S_ISREG(mode) or stat.S_ISDIR(mode)):
                raise Refused(f"安装包包含链接或特殊文件：{p}")
        disk = read_json(root / "release.json")
        web = read_json(root / "web/dist/release.json")
        for item in (disk, web):
            if any(item.get(k) != m.get(k) for k in ("version", "commit")):
                raise Refused("安装包与 Web 身份不一致。")
        if any(m.get(k) != m["version"] for k in ("hub_version", "web_version")):
            raise Refused("安装包版本不一致。")
        for role in ("hub", "node"):
            binary = root / "bin" / ("steve-" + role)
            if not binary.is_file() or not os.access(binary, os.X_OK):
                raise Refused(f"安装包缺少可执行文件：{binary}")
            info = json.loads(self.command([binary, "--build-info"], capture=True))
            if (any(info.get(k) != m.get(k) for k in ("version", "commit")) or info.get("target") != target
                    or (role == "hub" and info.get("schema") != (m.get("schema") or {}).get("target"))):
                raise Refused(f"{role} 的版本、提交、平台或 schema 与安装包不一致。")
        html = (root / "web/dist/index.html").read_text()
        if not re.search(r'<meta\s+name=["\']steve-web-version["\']\s+content=["\']' + re.escape(m["version"]) + r'["\']', html):
            raise Refused("Web 页面版本不一致。")
        required = ["steve-service.py", "local-release.py", "run.sh", "run-managed.sh", "upgrade.py"]
        if self.system == "Darwin":
            required.extend(("sign-setup.sh", "release-lib.sh"))
            if not (root / "bin/steve-keyhelper").is_file():
                raise Refused("macOS 安装包缺少 keyhelper。")
        if any(not (root / "ops" / name).is_file() for name in required):
            raise Refused("安装包缺少运行脚本。")

    def save(self):
        path = self.home / "setup.json"
        tmp = self.home / (".setup-" + uuid.uuid4().hex)
        try:
            private_file(tmp, (json.dumps(self.receipt, indent=2) + "\n").encode())
            os.replace(tmp, path)
        finally:
            tmp.unlink(missing_ok=True)

    @contextlib.contextmanager
    def claim(self, role, home, m):
        self.home = home
        if within(home, self.bundle) or within(self.bundle, home):
            raise Refused("数据目录与只读安装包必须分开。")
        if os.path.lexists(home):
            private_owned(home, directory=True)
            if not os.path.lexists(home / "setup.json"):
                raise Refused("目录已经存在但没有本次初始化收据；不会覆盖或接管旧数据。")
            private_owned(home / "setup.json")
            record = read_json(home / "setup.json")
            if (record.get("format") != 1 or record.get("component") != role
                    or record.get("home") != str(home)
                    or record.get("version") != m["version"] or record.get("commit") != m["commit"]):
                raise Refused("此目录不是本次初始化拥有的目录；不会覆盖或接管旧安装。")
        else:
            home.mkdir(mode=0o700, parents=True, exist_ok=False)
            record = dict(format=1, component=role, home=str(home), version=m["version"], commit=m["commit"], files={}, steps=[])
            private_file(home / "setup.json", (json.dumps(record, indent=2) + "\n").encode())
        fd = os.open(home / ".setup.lock", os.O_CREAT | os.O_RDWR | getattr(os, "O_NOFOLLOW", 0), 0o600)
        try:
            try:
                fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
            except BlockingIOError as e:
                raise Refused("此目录已有初始化正在进行。") from e
            self.receipt = read_json(home / "setup.json")
            yield
        finally:
            os.close(fd)

    def owned_file(self, relative, data=None):
        path = self.home / relative
        expected = self.receipt["files"].get(relative)
        if expected is None:
            if os.path.lexists(path):
                raise Refused(f"已有文件不属于本次初始化，不会覆盖：{path}")
            if data is None:
                raise Refused(f"初始化文件缺失：{path}")
            expected = hashlib.sha256(data).hexdigest()
            self.receipt["files"][relative] = expected
            self.save()
        if os.path.lexists(path):
            private_owned(path)
            if digest(path) != expected:
                raise Refused(f"文件已被修改，初始化不会覆盖：{path}")
        else:
            if data is None or hashlib.sha256(data).hexdigest() != expected:
                raise Refused(f"初始化中断在写入文件之前；需要原输入才能继续：{path}")
            path.parent.mkdir(parents=True, mode=0o700, exist_ok=True)
            if path.parent.is_symlink():
                raise Refused(f"初始化目录不能是符号链接：{path.parent}")
            private_file(path, data)
        return path

    def copy_bundle(self, m):
        releases = self.home / "releases"
        if releases.is_symlink():
            raise Refused("releases 不能是符号链接。")
        releases.mkdir(mode=0o700, exist_ok=True)
        candidate = releases / m["version"]
        if os.path.lexists(candidate):
            if not self.receipt.get("copy_planned") or candidate.is_symlink():
                raise Refused("已有版本目录不属于本次初始化，不会覆盖。")
            for p in [candidate, *candidate.rglob("*")]:
                if not (stat.S_ISREG(p.lstat().st_mode) or stat.S_ISDIR(p.lstat().st_mode)):
                    raise Refused("初始化版本目录出现链接或特殊文件，不会继续。")
            expected = self.receipt.get("bundle_files", {})
            found = {str(p.relative_to(candidate)) for p in candidate.rglob("*") if p.is_file()}
            if set(expected) != found:
                raise Refused("初始化版本目录的文件清单已改变。")
            for name, sha in expected.items():
                if name == "bin/steve-hub" and self.receipt.get("keychain_started"):
                    signed = self.receipt.get("signed_hub_sha256")
                    actual = digest(candidate / name)
                    if signed:
                        if actual != signed:
                            raise Refused("已签名的 Hub 文件发生变化，不会继续初始化。")
                    elif actual != sha:
                        # The signer may have finished before its receipt was saved. Never execute
                        # changed bytes whose signed digest was not recorded; require inspection.
                        raise Refused("本地签名中断且没有完整收据；请检查原签名身份，不会自动执行或重新签名。")
                    continue
                if digest(candidate / name) != sha:
                    raise Refused(f"初始化版本文件已改变，不会继续：{name}")
            self.verify_bundle(candidate, m)
            return candidate
        self.receipt["copy_planned"] = True
        self.receipt["bundle_files"] = {str(p.relative_to(self.bundle)): digest(p)
                                         for p in self.bundle.rglob("*") if p.is_file()}
        self.save()
        staging = releases / (".setup-" + uuid.uuid4().hex)
        try:
            shutil.copytree(self.bundle, staging, symlinks=True)
            for p in [staging, *staging.rglob("*")]:
                if p.is_symlink():
                    raise Refused("复制过程中安装包出现符号链接。")
                if p.is_dir():
                    p.chmod(0o700)
                elif p.is_file():
                    if digest(p) != self.receipt["bundle_files"].get(str(p.relative_to(staging))):
                        raise Refused("安装包复制校验失败。")
                    p.chmod(0o755 if os.access(p, os.X_OK) else 0o644)
                else:
                    raise Refused("复制过程中出现特殊文件。")
            self.verify_bundle(staging, m)
            if os.path.lexists(candidate):
                raise Refused("版本目录在复制过程中出现；不会覆盖。")
            staging.rename(candidate)
        finally:
            if staging.exists():
                shutil.rmtree(staging)
        return candidate

    def hub_config(self):
        if "environment" in self.receipt:
            env = self.receipt["environment"]
            self.owned_file("steve.env", self.env_bytes(env))
            self.owned_file("secrets/oidc-client-secret")
            return env
        public = url(self.prompt("浏览器访问地址", "http://127.0.0.1:8440"), "访问地址")
        issuer = url(self.prompt("OIDC issuer"), "OIDC issuer")
        client = self.prompt("OIDC client ID")
        print(f"请在 OIDC 提供方登记回调：{public}/auth/callback")
        secret_path = self.home / "secrets/oidc-client-secret"
        if "secrets/oidc-client-secret" not in self.receipt["files"] or not secret_path.exists():
            if not self.secret_terminal:
                raise Refused("需要交互终端输入 OIDC secret；不会从环境或命令行参数读取。")
            secret = self.secret("OIDC client secret（不回显）: ")
            if not secret or any(c in secret for c in "\r\n\0"):
                raise Refused("OIDC secret 不能为空或包含换行。")
            self.owned_file("secrets/oidc-client-secret", secret.encode())
        else:
            self.owned_file("secrets/oidc-client-secret")
        env = dict(STEVE_HOST="127.0.0.1", STEVE_PORT="8440", STEVE_PUBLIC_URL=public,
                   STEVE_OIDC_ISSUER=issuer, STEVE_OIDC_CLIENT_ID=client,
                   STEVE_OIDC_CLIENT_SECRET_FILE=str(secret_path), STEVE_WEB_DIR=str(self.home / "web/current"))
        if self.system == "Linux":
            value = self.receipt.get("key_directory") or self.prompt("Vault 密钥目录（数据目录之外，权限 0700）")
            keys = canonical(value)
            if within(keys, self.home) or within(self.home, keys):
                raise Refused("Vault 密钥目录必须与数据目录分开，不能互相包含。")
            if keys.exists():
                private_owned(keys, directory=True)
                if not self.receipt.get("key_directory") and any(keys.iterdir()):
                    raise Refused("密钥目录不是空目录；不会接管旧密钥。")
            else:
                keys.mkdir(mode=0o700, parents=True, exist_ok=False)
            self.receipt["key_directory"] = str(keys)
            env.update(STEVE_VAULT_KEYSTORE="dir", STEVE_VAULT_KEY_DIR=str(keys))
        else:
            env["STEVE_VAULT_KEYSTORE"] = "keychain"
        self.receipt["environment"] = env
        self.save()
        self.owned_file("steve.env", self.env_bytes(env))
        return env

    @staticmethod
    def env_bytes(values):
        return ("# Managed by steve setup; secrets are referenced by file.\n" +
                "".join(k + "=" + shlex.quote(v) + "\n" for k, v in sorted(values.items()))).encode()

    def setup(self, role, home=None, hub=None, pin=None):
        if not role:
            role = self.prompt("安装角色（hub / node）", "hub").lower()
        if role not in ("hub", "node") or (role == "hub" and (hub or pin)):
            raise Refused("请选择 hub 或 node；--hub/--pin 仅用于 Node 配对。")
        if pin is not None:
            if not re.fullmatch(r"sha256:[0-9a-fA-F]{64}", pin):
                raise Refused("--pin 必须是由 Hub 提供的 sha256: 加 64 位十六进制证书指纹。")
            pin = pin.lower()
        m = self.manifest()
        self.verify_bundle(self.bundle, m)
        with self.claim(role, self.home_for(role, home), m):
            candidate = self.copy_bundle(m)
            if role == "hub":
                env = self.hub_config()
                if self.system == "Darwin":
                    if "keychain" not in self.receipt["steps"]:
                        print("接下来创建此安装的本地签名身份和 Keychain helper；不会替换旧身份。")
                        self.receipt["keychain_started"] = True
                        self.save()
                        self.command([sys.executable, candidate / "ops/local-release.py", "setup-keychain", candidate])
                        self.receipt["signed_hub_sha256"] = digest(candidate / "bin/steve-hub")
                        self.receipt["helper_sha256"] = digest(self.home / "bin/steve-keyhelper")
                        self.receipt["identity_sha256"] = digest(self.home / "signing/identity.json")
                        self.receipt["steps"].append("keychain")
                        self.save()
                    for relative, key in (("bin/steve-keyhelper", "helper_sha256"), ("signing/identity.json", "identity_sha256")):
                        path = self.home / relative
                        if path.is_symlink() or not path.is_file() or digest(path) != self.receipt.get(key):
                            raise Refused("原签名身份或 keyhelper 已改变；不会重新创建或替换。")
                report = json.loads(self.command([candidate / "bin/steve-hub", "--check-config"], capture=True, values=env))
                if report.get("ok") is not True:
                    raise Refused("Hub 配置检查失败；请运行 steve hub doctor 后修复。")
                for warning in report.get("warnings", []):
                    print(warning)
            else:
                self.owned_file("roots.json", b"[]\n")
                self.owned_file("policy.json", b'{"max_permissions":"auto","full_network":true,"exec":false,"jobs":false}\n')
                self.owned_file("harness.json", b'{"adapters":{}}\n')
                pairing = self.home / "node/node.json"
                if pairing.exists():
                    if not self.receipt.get("pair_started"):
                        raise Refused("已有配对不属于本次初始化，不会覆盖。")
                    paired = read_json(pairing)
                    if (paired.get("hub_url") != self.receipt.get("hub_url")
                            or (hub and url(hub, "Hub") != paired.get("hub_url"))
                            or (pin is not None and pin != paired.get("pin"))):
                        raise Refused("已有配对指向另一个 Hub，不会重新配对。")
                    self.command([candidate / "bin/steve-node", "status"])
                else:
                    chosen = hub or self.receipt.get("hub_url")
                    if not chosen and "installed" not in self.receipt["steps"]:
                        chosen = self.ask("Hub URL（留空稍后配对）: ").strip()
                    if chosen:
                        chosen = url(chosen, "Hub")
                        if chosen.startswith("http://"):
                            raise Refused("Node 初始化配对需要 HTTPS Hub；本机明文测试请使用已有高级 Node CLI。")
                        if not self.receipt.get("pair_started") and os.path.lexists(self.home / "node/identity.key"):
                            raise Refused("已有 Node 身份不属于本次初始化，不会复用。")
                        if self.receipt.get("pair_started") and self.receipt.get("hub_url") != chosen:
                            raise Refused("配对曾向另一个 Hub 发出；请先检查原配对，不会自动更换身份。")
                        selected_pin = pin or self.receipt.get("pin")
                        self.receipt.update(pair_started=True, hub_url=chosen, pin=selected_pin)
                        self.save()
                        print("输入 Hub Devices 页生成的一次性配对码；随后仍需在 Hub 确认指纹。")
                        command = [candidate / "bin/steve-node", "pair", chosen]
                        if selected_pin:
                            command.extend(("--pin", selected_pin))
                        self.command(command)
                        paired = read_json(pairing)
                        if paired.get("hub_url") != chosen:
                            raise Refused("Node 配对回读与所选 Hub 不一致。")
            self.command([sys.executable, candidate / "ops/steve-service.py", role, "install",
                          "--home", self.home, "--release", candidate], capture=True)
            if "installed" not in self.receipt["steps"]:
                self.receipt["steps"].append("installed")
            self.save()
        print(f"{role} 已配置并注册服务；本次 setup 未启动或重启服务。数据目录：{self.home}")
        if role == "node":
            print("配对后请在 Hub Devices 确认同一指纹，再明确启动；未授权任何工作目录。")
        else:
            print("OIDC 登录仍需在浏览器验证；HTTPS 地址需先配置反向代理。")
            if self.system == "Darwin" and self.home != self.home_for("hub"):
                print("自定义 macOS Hub 目录需使用原签名身份手动升级；doctor 会说明受支持入口。")
        print("principal 的模型登录需要单独完成，不会导入既有登录凭据。")
        print("启动：" + shlex.join(["steve", role, "start", "--home", str(self.home)]))
        return 0

    def lifecycle(self, args):
        self.home = self.home_for(args.role, args.home)
        command = [sys.executable, self.bundle / "ops/steve-service.py", args.role, args.action, "--home", self.home]
        for key in ("release", "label", "wait"):
            value = getattr(args, key, None)
            if value is not None:
                command.extend(("--" + key, str(value)))
        return self.run([str(x) for x in command], env=self.environment(),
                        capture_output=False, text=True, check=False).returncode


def main(argv=None, cli=None):
    parser = argparse.ArgumentParser(description="安装、初始化和管理 Steve；setup 不启动服务。")
    parser.add_argument("--version", action="store_true")
    sub = parser.add_subparsers(dest="command")
    setup = sub.add_parser("setup", help="交互初始化一个新的 Hub 或 Node")
    setup.add_argument("role", nargs="?", choices=("hub", "node"))
    setup.add_argument("--home")
    setup.add_argument("--hub", help="Node 要配对的 HTTPS Hub URL；配对码从 stdin 输入")
    setup.add_argument("--pin", help="用户核对的 Hub TLS 证书 sha256: 指纹（Node，可选）")
    for role in ("hub", "node"):
        group = sub.add_parser(role)
        group.set_defaults(role=role)
        actions = group.add_subparsers(dest="action", required=True)
        for action in ("start", "stop", "restart", "status", "doctor", "update", "uninstall"):
            p = actions.add_parser(action)
            p.add_argument("--home")
            p.add_argument("--label")
            if action in ("doctor", "status", "update"):
                p.add_argument("--release")
            if action == "update":
                p.add_argument("--wait", type=float)
    upgrade = sub.add_parser("upgrade", help="委派现有 signed-Hub 升级协调器，不绕过 drain")
    upgrade.add_argument("arguments", nargs=argparse.REMAINDER)
    args = parser.parse_args(argv)
    app = cli or CLI()
    try:
        if args.version:
            print("steve CLI/package " + app.manifest()["version"])
            return 0
        if args.command == "setup":
            return app.setup(args.role, args.home, args.hub, args.pin)
        if args.command in ("hub", "node"):
            return app.lifecycle(args)
        if args.command == "upgrade":
            return app.run([sys.executable, str(app.bundle / "ops/upgrade.py"), *(args.arguments or ["--help"])],
                           env=app.environment(), capture_output=False, text=True, check=False).returncode
        parser.print_help()
        return 0
    except (Refused, OSError, ValueError, EOFError, KeyboardInterrupt) as e:
        print(f"steve: {e or '初始化已中断；已完成步骤保留。'}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
