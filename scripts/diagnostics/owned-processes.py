"""Track only observed descendants of one newly spawned diagnostic adapter.

No command lines, names or process groups grant ownership. A PID alone is not
an identity. Discovery is sampled: descendants never observed before their
parent exits cannot be recovered safely by guessing their later ownership.
"""
import asyncio
import ctypes
import errno
import os
from pathlib import Path
import signal
import subprocess
import sys
import time


class BsdInfo(ctypes.Structure):
    # Darwin's public proc_bsdinfo ABI (libproc.h / sys/proc_info.h).
    _fields_ = [(name, ctypes.c_uint32) for name in (
        "flags", "status", "xstatus", "pid", "ppid", "uid", "gid", "ruid",
        "rgid", "svuid", "svgid", "reserved")]
    _fields_ += [("comm", ctypes.c_char * 16), ("name", ctypes.c_char * 32)]
    _fields_ += [(name, ctypes.c_uint32) for name in (
        "nfiles", "pgid", "pjobc", "tdev", "tpgid", "nice")]
    _fields_ += [("start_sec", ctypes.c_uint64), ("start_usec", ctypes.c_uint64)]


def process_info(pid):
    """Return birth identity/parent/state, None only for a confirmed absent PID."""
    if sys.platform.startswith("linux"):
        try:
            fields = Path(f"/proc/{pid}/stat").read_text().rsplit(") ", 1)[1].split()
        except FileNotFoundError:
            return None
        return {"pid": pid, "ppid": int(fields[1]), "birth": "linux:" + fields[19],
                "state": "zombie" if fields[0] in ("Z", "X") else "alive"}
    if sys.platform == "darwin":
        lib = ctypes.CDLL("/usr/lib/libproc.dylib", use_errno=True)
        lib.proc_pidinfo.argtypes = [ctypes.c_int, ctypes.c_int, ctypes.c_uint64,
                                     ctypes.c_void_p, ctypes.c_int]
        lib.proc_pidinfo.restype = ctypes.c_int
        info = BsdInfo()
        ctypes.set_errno(0)
        size = lib.proc_pidinfo(pid, 3, 0, ctypes.byref(info), ctypes.sizeof(info))
        if size == 0 and ctypes.get_errno() == errno.ESRCH:
            return None
        if size != ctypes.sizeof(info) or info.pid != pid:
            raise OSError(ctypes.get_errno(), "process identity unavailable")
        return {"pid": pid, "ppid": info.ppid, "birth": f"darwin:{info.start_sec}:{info.start_usec}",
                "state": "zombie" if info.status == 5 else "alive"}
    raise OSError("process birth identity is supported only on Linux and macOS")


def process_parents():
    # Never collect command lines or environments into shareable receipts.
    ps = subprocess.run(["ps", "-axo", "pid=,ppid="], capture_output=True,
                        text=True, timeout=2, check=True)
    return dict(tuple(map(int, line.split())) for line in ps.stdout.splitlines())


class OwnedProcesses:
    def __init__(self, process, grace=2, kill_timeout=2):
        self.process = process
        self.grace, self.kill_timeout = grace, kill_timeout
        self.records = {}
        self.errors = []
        self.lock = asyncio.Lock()
        try:
            info = process_info(process.pid)
            if info is None:
                raise ProcessLookupError("adapter exited before identity capture")
            self.add(info, None)
        except Exception as error:
            self.errors.append({"stage": "adapter_identity", "error_type": type(error).__name__})

    def add(self, info, owner):
        self.records[info["pid"]] = {"pid": info["pid"], "birth": info["birth"],
            "discovered_ppid": info["ppid"], "owner_pid": owner,
            "events": [{"action": "tracked", "at": time.time(), "readback": info}]}

    def inspect(self, record):
        try:
            info = process_info(record["pid"])
            if info is None:
                return {"state": "exited"}
            if info["birth"] != record["birth"]:
                info = {**info, "state": "identity_unknown", "reason": "birth_changed"}
        except Exception as error:
            info = {"state": "identity_unknown", "error_type": type(error).__name__}
        if info["state"] == "identity_unknown":
            record.setdefault("identity_failure", info)
        return info

    async def observe(self):
        async with self.lock:
            try:
                parents = await asyncio.to_thread(process_parents)
                # Only a currently matching identity can establish new ownership.
                anchors = {pid for pid, r in self.records.items() if self.inspect(r)["state"] == "alive"}
                while anchors:
                    next_anchors = set()
                    for pid, parent in parents.items():
                        if pid in self.records or parent not in anchors:
                            continue
                        try:
                            info = process_info(pid)
                            if info and info["ppid"] == parent and self.inspect(self.records[parent])["state"] == "alive":
                                self.add(info, parent)
                                next_anchors.add(pid)
                        except Exception as error:
                            self.errors.append({"stage": "descendant_identity", "pid": pid,
                                                "error_type": type(error).__name__})
                    anchors = next_anchors
            except Exception as error:
                self.errors.append({"stage": "discovery", "error_type": type(error).__name__})

    async def watch(self):
        while True:
            await self.observe()
            await asyncio.sleep(.05)

    def send_signal(self, record, sig):
        event = {"action": "signal", "signal": sig.name, "at": time.time(),
                 "before": self.inspect(record), "sent": False}
        record["events"].append(event)
        if event["before"]["state"] != "alive":
            return
        fd = None
        try:
            # A pidfd binds Linux signalling to the verified process even if
            # its PID is recycled between the last read and the signal syscall.
            if hasattr(os, "pidfd_open") and hasattr(signal, "pidfd_send_signal"):
                fd = os.pidfd_open(record["pid"])
            event["verified"] = self.inspect(record)
            if event["verified"]["state"] == "alive":
                if fd is not None:
                    signal.pidfd_send_signal(fd, sig)
                else:
                    os.kill(record["pid"], sig)
                event["sent"] = True
        except ProcessLookupError:
            event["signal_race"] = "process_exited"
        except Exception as error:
            event["error_type"] = type(error).__name__
        finally:
            if fd is not None:
                os.close(fd)
        event["after"] = self.inspect(record)

    async def wait(self, timeout, stage):
        started = time.monotonic()
        polls = 0
        while True:
            polls += 1
            states = {pid: self.inspect(r) for pid, r in self.records.items()}
            if not any(s["state"] == "alive" for s in states.values()) or time.monotonic() - started >= timeout:
                break
            await asyncio.sleep(min(.05, max(0, timeout - (time.monotonic() - started))))
        for pid, state in states.items():
            self.records[pid]["events"].append({"action": "wait", "stage": stage,
                "timeout_seconds": timeout, "elapsed_seconds": round(time.monotonic() - started, 3),
                "polls": polls, "readback": state})

    async def cleanup(self):
        await self.observe()
        # Children first, but every tracked PID is waited for independently of
        # the adapter's returncode. Reparenting does not erase birth identity.
        for record in reversed(list(self.records.values())):
            self.send_signal(record, signal.SIGTERM)
        await self.wait(self.grace, "after_sigterm")
        await self.observe()
        for record in reversed(list(self.records.values())):
            self.send_signal(record, signal.SIGKILL)
        await self.wait(self.kill_timeout, "after_sigkill")
        try:
            await asyncio.wait_for(asyncio.shield(self.process.wait()), self.kill_timeout)
        except Exception as error:
            self.errors.append({"stage": "adapter_wait", "error_type": type(error).__name__})
        await self.observe()
        for record in self.records.values():
            record["final"] = self.inspect(record)
            record["passed"] = record["final"]["state"] in ("exited", "zombie") and "identity_failure" not in record
        return {"passed": not self.errors and bool(self.records) and all(r["passed"] for r in self.records.values()),
                "processes": list(self.records.values()), "errors": self.errors,
                "scope": "observed descendants only; exited or zombie means no running process"}
