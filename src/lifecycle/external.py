"""Daemon-owned whole-window replacement, without an updater API in the old terminal."""
import fcntl
import hashlib
import json
import os
from pathlib import Path
import re
import shlex
import socket
import struct
import subprocess
import tempfile
import termios
import threading
import time
import uuid

import lifecycle_worker as worker
import lifecycle_timing as update_timing
from lifecycle_window import WindowUpdate, atomic_record, scope_fingerprint

PLACEMENT_XML = """<node><interface name='com.terrydaktal.ApplicationLauncher.Placement1'>
<method name='Result'><arg type='s' direction='in'/><arg type='s' direction='in'/></method>
</interface></node>"""


def same_output(saved, current):
    if (not saved or any(saved.get(key, 0) <= 0 for key in ("width", "height", "scale")) or saved != current):
        raise worker.Refused("Monitor geometry or scale is unavailable or changed")


def same_placement(saved, actual, minimized):
    same_output(saved.get("output"), actual.get("output"))
    rectangle = "normal" if saved["maximized"] or saved["fullscreen"] else "geometry"
    if (not saved.get(rectangle) or not actual.get(rectangle)
            or any(abs(saved[rectangle][key] - actual[rectangle][key]) > 2 for key in ("x", "y", "width", "height"))):
        raise worker.Refused("The replacement geometry does not match the original")
    for key in ("maximized", "fullscreen", "desktops", "activities", "keepAbove", "keepBelow", "noBorder",
                "shaded", "skipTaskbar", "skipPager", "skipSwitcher"):
        if saved[key] != actual[key]:
            raise worker.Refused(f"Window state changed: {key}")
    if actual["minimized"] != minimized:
        raise worker.Refused("Window minimization state changed")


class Desktop:
    def __init__(self, system):
        self.system = system
        self.owner = system.dbus("GetNameOwner", "(s)", ["org.kde.KWin"])[0]
        self.pending = {}
        self.directory = tempfile.TemporaryDirectory(prefix="launcher-placement-", dir=os.environ.get("XDG_RUNTIME_DIR"))
        self.path = "/LifecyclePlacement/i" + uuid.uuid4().hex
        node = system.Gio.DBusNodeInfo.new_for_xml(PLACEMENT_XML)
        self.registration = system.bus.register_object(self.path, node.interfaces[0], self.result, None, None)
        self.loop = system.GLib.MainLoop()
        self.thread = threading.Thread(target=self.loop.run, daemon=True)
        self.thread.start()

    def result(self, connection, sender, path, interface, method, parameters, invocation):
        nonce, payload = parameters.unpack()
        if sender != self.owner or len(payload) > 65536 or nonce not in self.pending:
            invocation.return_dbus_error("com.terrydaktal.ApplicationLauncher.Placement1.Refused", "Untrusted placement reply")
            return
        event, result = self.pending[nonce]
        try: result.update(json.loads(payload))
        except (ValueError, TypeError): result["error"] = "Malformed compositor reply"
        event.set()
        invocation.return_value(None)

    def call(self, action, **values):
        nonce = uuid.uuid4().hex
        name = "applicationlauncher-placement-" + nonce
        script = Path(self.directory.name) / (nonce + ".js")
        script.write_text("var request = " + json.dumps(dict(action=action, nonce=nonce,
            service=self.system.bus.get_unique_name(), path=self.path, **values)) + ";\n" + PLACEMENT_JS)
        event, result = threading.Event(), {}
        self.pending[nonce] = (event, result)
        loaded = False
        try:
            identifier = self.system.call(self.owner, "/Scripting", "org.kde.kwin.Scripting", "loadScript", "(ss)", [str(script), name])[0]
            if identifier < 0: raise worker.Refused("KWin refused the placement helper")
            loaded = True
            self.system.call(self.owner, f"/Scripting/Script{identifier}", "org.kde.kwin.Script", "run")
            if not event.wait(3): raise worker.Refused("KWin did not answer the placement request")
            if "error" in result: raise worker.Refused(result["error"])
            if not isinstance(result.get("ok"), dict): raise worker.Refused("Invalid compositor response")
            return result["ok"]
        finally:
            if loaded:
                try: self.system.call(self.owner, "/Scripting", "org.kde.kwin.Scripting", "unloadScript", "(s)", [name])
                except Exception: pass
            self.pending.pop(nonce, None)
            script.unlink(missing_ok=True)

    def close(self):
        self.loop.quit()
        self.thread.join(timeout=3)
        self.system.bus.unregister_object(self.registration)
        self.directory.cleanup()


def native_records(system, pid):
    owners = set()
    for name in system.dbus("ListNames")[0]:
        if name != "org.xfce.Terminal5" and not name.startswith("org.xfce.Terminal5.Instance."):
            continue
        try:
            owner = system.dbus("GetNameOwner", "(s)", [name])[0]
            if system.dbus("GetConnectionUnixProcessID", "(s)", [owner])[0] == pid:
                owners.add(owner)
        except Exception: continue
    if len(owners) > 1: raise worker.Refused("Multiple terminal control owners claim the same process")
    if not owners: return None, None
    owner = owners.pop()
    try:
        values = system.call(owner, worker.TERMINAL_PATH, worker.TERMINAL_INTERFACE, "ListTerminals")[0]
    except Exception as error:
        if "UnknownMethod" in str(error) or "UnknownInterface" in str(error) or "UnknownObject" in str(error):
            return None, None
        raise worker.Refused("The terminal's tab metadata could not be read; it is not safe to guess") from error
    if not isinstance(values, list) or len(values) > 512:
        raise worker.Refused("Malformed or oversized terminal tab metadata")
    return owner, values


def proc_record(pid, window_id):
    roots = []
    for child in worker.children(pid):
        try:
            saved = worker.process(child)
            if saved["tty"]: roots.append(saved)
        except ProcessLookupError: continue
    if len(roots) != 1:
        raise worker.Refused("Without tab metadata, exactly one terminal session is required; multiple tabs cannot be ordered safely")
    root = roots[0]
    pty = None
    for descriptor in (0, 1, 2):
        path = f"/proc/{root['pid']}/fd/{descriptor}"
        try:
            candidate = os.readlink(path)
            if re.fullmatch(r"/dev/pts/[0-9]+", candidate) and os.stat(path).st_rdev == root["tty"]:
                pty = candidate
                break
        except OSError: pass
    if pty is None: raise worker.Refused("The terminal session has no verifiable PTY")
    return dict(window_uuid=window_id, tab_uuid=f"proc-{root['pid']}-{root['start']}", child_pid=root["pid"],
        foreground_pgid=root["foreground"], active=True, working_directory=root["cwd"], input_enabled=True,
        pty=pty)


@update_timing.observed("capture_terminal")
def capture(system, desktop, window):
    pid, identifier = window["pid"], window["id"]
    owner_process = worker.process(pid)
    if worker.kind(owner_process) != "xfce4-terminal":
        raise worker.Refused("The selected window is no longer owned by xfce4-terminal")
    if any(argument.startswith("--drop-down") for argument in owner_process["arguments"]):
        raise worker.Refused("Drop-down terminals are not ordinary replaceable windows")
    actual = desktop.call("capture", pid=pid, id=identifier)
    same_output(actual.get("output"), actual.get("output"))
    rectangle = actual["normal"] if actual["maximized"] or actual["fullscreen"] else actual["geometry"]
    if not rectangle or min(rectangle["width"], rectangle["height"]) <= 0:
        raise worker.Refused("The original normal window geometry is unavailable")
    windows = desktop.call("list", pid=pid)["windows"]
    owner, records = native_records(system, pid)
    if records is None:
        if len(windows) != 1:
            raise worker.Refused("Without tab metadata, a shared process with multiple windows cannot be mapped safely")
        records = [proc_record(pid, identifier)]
    else:
        def stable(title):
            return re.sub(r"[\u2800-\u28ff\u0336]", "", title).strip()
        groups = {}
        for record in records:
            groups.setdefault(record.get("window_uuid"), []).append(record)
        matches = [rows for key, rows in groups.items() if key and (len(windows) == len(groups) == 1
            or stable(rows[0].get("window_title", "")) == stable(actual["title"]))]
        if len(matches) != 1 or (len(windows) > 1 and sum(stable(row["title"]) == stable(actual["title"]) for row in windows) != 1):
            raise worker.Refused("The window-to-tab mapping is ambiguous; no window was changed")
        records = matches[0]
    binary = worker.latest_binary("xfce4-terminal")
    with open(binary, "rb") as stream: installed = hashlib.file_digest(stream, "sha256").hexdigest()
    with open(f"/proc/{pid}/exe", "rb") as stream: running = hashlib.file_digest(stream, "sha256").hexdigest()
    if installed == running: raise worker.Refused("This terminal already runs the current installed build")
    environment = {}
    for entry in worker.read_limited(f"/proc/{pid}/environ", 128 * 1024).split(b"\0"):
        key, separator, value = entry.partition(b"=")
        name = os.fsdecode(key)
        if separator and name in {"DISPLAY", "WAYLAND_DISPLAY", "GDK_BACKEND", "XDG_CURRENT_DESKTOP"}:
            environment[name] = os.fsdecode(value)
    return dict(owner=owner_process, owner_bus=owner, records=records, window=actual, binary=binary, display_environment=environment,
                installed=worker.file_identity(binary), capabilities=dict(hold=False), running_sha256=running, sha256=installed)


def inspect(windows, system):
    desktop = Desktop(system)
    result = []
    try:
        for window in windows[:512]:
            row = dict(window_id=window["id"], owner_pid=window["pid"], title=window["title"], terminal_window_uuid="",
                tabs=[], eligible=False, recommended=False, preserves_processes=False, executable="", build_id=None,
                sha256="", status="unsupported", message="", external=None)
            planner = WindowUpdate({}, system, "/not-used-for-preview")
            try:
                snapshot = capture(system, desktop, window)
                tabs = planner.capture_tabs(snapshot)
                row.update(eligible=True, recommended=True, tabs=[tab["cwd"] for tab in tabs],
                    preserves_processes=all(tab["program"]["kind"] == "tmux" for tab in tabs), executable=snapshot["binary"],
                    sha256=snapshot["sha256"], status="ready", message="Daemon-managed replacement; the old terminal needs no updater API")
                row["external"] = dict(window=dict(id=window["id"], pid=window["pid"], title=window["title"]),
                    owner=snapshot["owner"], records=snapshot["records"], binary=snapshot["binary"], installed=snapshot["installed"],
                    window_state=snapshot["window"], scope_fingerprint=scope_fingerprint(tabs), display_environment=snapshot["display_environment"])
            except Exception as error:
                row["message"] = str(error)
                if "already runs the current" in str(error): row["status"] = "up_to_date"
            finally:
                for descriptor in planner.descriptors: os.close(descriptor)
            result.append(row)
        return result
    finally:
        desktop.close()


def validate_gate(plan):
    arguments, environment, title = plan.get("argv"), plan.get("environment", {}), plan.get("title")
    if (plan.get("protocol") != 1 or not isinstance(plan.get("executable"), str)
            or not os.path.isabs(plan["executable"]) or "\0" in plan["executable"]
            or not isinstance(arguments, list) or not 0 < len(arguments) <= 128
            or any(not isinstance(argument, str) or "\0" in argument for argument in arguments)
            or not arguments[0] or sum(len(argument.encode()) for argument in arguments) > 65536
            or not isinstance(environment, dict) or len(environment) > len(worker.TERMINAL_ENVIRONMENT)
            or any(key not in worker.TERMINAL_ENVIRONMENT or not isinstance(value, (str, type(None)))
                   or (value is not None and ("\0" in value or len(value.encode()) > 16384)) for key, value in environment.items())):
        raise worker.Refused("Invalid prepared startup gate")
    if title is not None and (not isinstance(title, str) or len(title.encode()) > 16384
            or any(ord(char) < 32 or 127 <= ord(char) < 160 for char in title)):
        raise worker.Refused("Unsafe restored terminal title")


def gate(plan):
    validate_gate(plan)
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as connection:
        connection.settimeout(180)
        connection.connect(plan["socket"])
        if connection.recv(1) != b"S":
            raise worker.Refused("The startup gate was cancelled before release")
    title = plan.get("title")
    if title is not None:
        os.write(1, b"\033]2;" + title.encode() + b"\007")
    environment = os.environ.copy()
    environment.pop("XFCE4_TERMINAL_UPDATE_SERVICE", None)
    for key, value in plan.get("environment", {}).items():
        if value is None: environment.pop(key, None)
        else: environment[key] = value
    os.execve(plan["executable"], plan["argv"], environment)


class GateIdentityError(worker.Refused):
    pass


class Backend:
    def __init__(self, request, system, directory):
        self.request, self.system, self.directory = request, system, Path(directory)
        self.desktop = Desktop(system)
        self.snapshot = None
        self.replacement = None
        self.sockets, self.connections = [], []
        self.descriptor = None
        self.released = self.closed = False
        self.new_window = None
        self.new_records = []
        self.new_owner = None
        self.service = "org.xfce.Terminal5.Update.i" + request["job_id"].replace("-", "_")
        if system.dbus("RequestName", "(su)", [self.service, 4])[0] != 1:
            raise worker.Refused("The replacement service identity is already in use")

    def current(self, allow_missing=False):
        saved = self.request["external"]
        return self.desktop.call("capture", pid=saved["window"]["pid"], id=saved["window"]["id"], allowMissing=allow_missing)

    def preflight(self, stopped=False):
        saved = self.request["external"]
        actual = self.current(allow_missing=stopped)
        if actual.get("missing"):
            if not stopped: raise worker.Refused("The original window disappeared before interruption")
            self.closed = True
        else:
            if not worker.same_process(saved["owner"]): raise worker.Refused("The terminal process identity changed")
            same_placement(saved["window_state"], actual, saved["window_state"]["minimized"])
            if not stopped:
                fresh = capture(self.system, self.desktop, saved["window"])
                keys = ("window_uuid", "tab_uuid", "active", "child_pid", "foreground_pgid", "pty")
                if [[row.get(key) for key in keys] for row in fresh["records"]] != [[row.get(key) for key in keys] for row in saved["records"]]:
                    raise worker.Refused("The original tabs or foreground ownership changed after preview")
        if worker.latest_binary("xfce4-terminal") != saved["binary"] or worker.file_identity(saved["binary"]) != saved["installed"]:
            raise worker.Refused("The installed terminal build changed after preview")
        return {}

    def accept_gates(self, records):
        # VTE can publish a tab before its asynchronous child-spawn callback.
        # Keep connected gates waiting until every child PID is authoritative.
        if any(record.get("child_pid", 0) <= 1 for record in records): return False
        for index in range(len(self.connections), len(self.sockets)):
            connection, _ = self.sockets[index].accept()
            pid, uid, _ = struct.unpack("3i", connection.getsockopt(socket.SOL_SOCKET, socket.SO_PEERCRED, 12))
            if uid != os.getuid() or pid != records[index]["child_pid"]:
                connection.close()
                raise GateIdentityError("A startup gate belongs to a different process")
            self.connections.append(connection)
        return True

    def stage(self, tabs):
        if self.replacement is not None: raise worker.Refused("A replacement was already staged")
        self.preflight()
        if (len(tabs) != len(self.snapshot["records"])
                or any(tab.get("active") != record.get("active") or not os.path.isdir(tab.get("cwd", ""))
                       for tab, record in zip(tabs, self.snapshot["records"], strict=True))):
            raise worker.Refused("The prepared tab topology or working directory changed")
        for tab in tabs: validate_gate(dict(protocol=1, **tab))
        commands = []
        for index, tab in enumerate(tabs):
            listener = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
            listener.settimeout(0.05)
            # Unix socket paths must fit even when XDG_STATE_HOME is very long.
            if not hasattr(self, "gates_directory"):
                self.gates_directory = tempfile.TemporaryDirectory(prefix="launcher-gates-", dir=os.environ.get("XDG_RUNTIME_DIR"))
            listener.bind(str(Path(self.gates_directory.name) / str(index)))
            os.chmod(listener.getsockname(), 0o600)
            listener.listen(1)
            self.sockets.append(listener)
            plan = self.directory / f"gate-{index}.json"
            atomic_record(plan, dict(protocol=1, socket=listener.getsockname(), **tab))
            commands.append([self.request["daemon_binary"], "--lifecycle-gate", str(plan)])
        saved = self.request["external"]
        self.descriptor = os.open(saved["binary"], os.O_RDONLY | os.O_CLOEXEC | os.O_NOFOLLOW)
        if worker.file_identity(f"/proc/self/fd/{self.descriptor}") != saved["installed"]:
            raise worker.Refused("The terminal binary changed while pinning it")
        arguments = [saved["binary"], "--disable-server", "--minimize"]
        active = next(row for row in saved["records"] if row["active"])
        try:
            descriptor = os.open(active["pty"], os.O_RDONLY | os.O_NOCTTY | os.O_NONBLOCK)
            try: rows, columns, _, _ = struct.unpack("HHHH", fcntl.ioctl(descriptor, termios.TIOCGWINSZ, b"\0" * 8))
            finally: os.close(descriptor)
            if columns > 0 and rows > 0: arguments.append(f"--geometry={columns}x{rows}")
        except OSError: pass
        for index, (tab, command) in enumerate(zip(tabs, commands, strict=True)):
            if index: arguments.append("--tab")
            arguments.extend(["--working-directory", tab["cwd"]])
            if tab["active"]: arguments.append("--active-tab")
            arguments.extend(["--command", shlex.join(command)])
        environment = os.environ.copy()
        # GUI activation is compositor state, not inherited daemon launch state.
        for key in ("XDG_ACTIVATION_TOKEN", "DESKTOP_STARTUP_ID", "TMUX", "TMUX_PANE"):
            environment.pop(key, None)
        for key in ("DISPLAY", "WAYLAND_DISPLAY", "GDK_BACKEND", "XDG_CURRENT_DESKTOP"):
            value = self.request.get("display_environment", {}).get(key)
            if value is not None: environment[key] = value
        environment.update(XFCE4_TERMINAL_LAUNCH_PATH=saved["binary"], XFCE4_TERMINAL_LAUNCH_KIND="executable",
                           XFCE4_TERMINAL_UPDATE_SERVICE=self.service)
        atomic_record(self.directory / "replacement-launch.json", dict(arguments=arguments, binary=saved["binary"],
            installed=saved["installed"], display_environment={key: environment.get(key) for key in ("DISPLAY", "WAYLAND_DISPLAY", "GDK_BACKEND")}))
        with open(self.directory / "replacement.log", "wb") as log:
            self.replacement = subprocess.Popen(arguments, executable=f"/proc/self/fd/{self.descriptor}",
                pass_fds=(self.descriptor,), env=environment, stdin=subprocess.DEVNULL, stdout=log, stderr=log, start_new_session=True)
        worker.restart_event(self.directory, "replacement_started", pid=self.replacement.pid)
        deadline = time.monotonic() + 30
        last = "The replacement has not registered"
        while time.monotonic() < deadline:
            if self.replacement.poll() is not None:
                failures = [path.read_text(errors="replace") for path in self.directory.glob("gate-*.error.json")]
                raise worker.Refused(f"The replacement terminal exited before handover (exit {self.replacement.returncode}): " +
                    (self.directory / "replacement.log").read_text(errors="replace")[-4096:] + "\n".join(failures))
            try:
                owner, records = native_records(self.system, self.replacement.pid)
                if records is None:
                    records = [proc_record(self.replacement.pid, "replacement")]
                    if len(tabs) != 1: raise worker.Refused("Multiple replacement tabs require readable tab metadata")
                if (len(records) != len(tabs) or any(record.get("active") != tab["active"]
                        or os.path.realpath(record["working_directory"]) != tab["cwd"] for record, tab in zip(records, tabs, strict=True))):
                    raise worker.Refused("Replacement tab order, active tab or directory does not match: " + json.dumps(dict(
                        actual=[{key: row.get(key) for key in ("active", "working_directory", "child_pid")} for row in records],
                        expected=[{key: tab[key] for key in ("active", "cwd")} for tab in tabs])))
                if not self.accept_gates(records):
                    raise worker.Refused("The replacement tabs have not published their child PIDs yet")
                if worker.file_identity(f"/proc/{self.replacement.pid}/exe") != saved["installed"]:
                    raise worker.Refused("The replacement is not running the pinned terminal binary")
                self.new_owner = owner
                self.new_records = records
                self.new_window = self.desktop.call("place", pid=self.replacement.pid, saved=self.snapshot["window"])
                actual = self.desktop.call("capture", pid=self.replacement.pid, id=self.new_window["id"])
                same_placement(self.snapshot["window"], actual, True)
                return {}
            except GateIdentityError:
                raise
            except (worker.Refused, OSError) as error:
                last = str(error)
                time.sleep(0.05)
        failures = [path.read_text(errors="replace") for path in self.directory.glob("gate-*.error.json")]
        raise worker.Refused("Replacement preparation failed; original untouched: " + last + "\n".join(failures))

    def records(self):
        if self.replacement is None or self.replacement.poll() is not None:
            raise worker.Refused("The replacement terminal exited")
        if self.new_owner:
            records = self.system.call(self.new_owner, worker.TERMINAL_PATH, worker.TERMINAL_INTERFACE, "ListTerminals")[0]
        else: records = [proc_record(self.replacement.pid, "replacement")]
        if worker.file_identity(f"/proc/{self.replacement.pid}/exe") != self.request["external"]["installed"]:
            raise worker.Refused("The replacement terminal executable changed")
        return dict(records=records, pid=self.replacement.pid)

    def present(self):
        self.records()
        actual = self.desktop.call("present", pid=self.replacement.pid, id=self.new_window["id"], saved=self.snapshot["window"])
        same_placement(self.snapshot["window"], actual, self.snapshot["window"]["minimized"])
        return actual

    def finish(self):
        if self.new_owner:
            try: self.system.call(self.new_owner, worker.TERMINAL_PATH, worker.TERMINAL_INTERFACE, "FinishUpdate", "(s)", [self.service])
            except Exception as error:
                if not any(name in str(error) for name in ("UnknownMethod", "UnknownInterface")): raise

    def operate(self, action, value):
        if action == "Publish": return {}
        if action == "AwaitConfirmation":
            if self.request.get("confirmed") is not True: raise worker.Refused("Explicit restart consent is required")
            return {}
        if action == "Capture":
            self.preflight()
            self.snapshot = capture(self.system, self.desktop, self.request["external"]["window"])
            return self.snapshot
        if action == "Preflight": return self.preflight()
        if action == "Stage": return self.stage(value["tabs"])
        if action == "Release":
            if self.released: raise worker.Refused("Startup gates have already been released")
            self.released = True
            for connection in self.connections: connection.sendall(b"S")
            # An old terminal may auto-close as its child exits. Show the already
            # placed replacement immediately, while exact app verification runs.
            self.present()
            return {}
        if action == "Records": return self.records()
        if action == "Commit":
            self.preflight(stopped=True)
            self.records()
            self.finish()
            if not self.closed:
                self.desktop.call("close", pid=self.snapshot["owner"]["pid"], id=self.snapshot["window"]["id"])
                deadline = time.monotonic() + 5
                while not self.current(allow_missing=True).get("missing"):
                    if time.monotonic() >= deadline: raise worker.Refused("The original window did not close; no process was force-killed")
                    time.sleep(0.05)
                self.closed = True
            actual = self.present()
            return dict(original_closed=True, replacement_pid=self.replacement.pid, replacement_window_id=actual["id"])
        if action in ("Recover", "Discard"):
            if self.replacement is not None and self.replacement.poll() is None:
                if self.released:
                    self.finish()
                    self.present()
                else: self.replacement.terminate()
            self.closed = bool(self.current(allow_missing=True).get("missing"))
            return dict(original_closed=self.closed)
        raise worker.Refused("Unsupported daemon-owned terminal operation")

    def close(self):
        for connection in self.connections: connection.close()
        for listener in self.sockets: listener.close()
        if hasattr(self, "gates_directory"): self.gates_directory.cleanup()
        if self.descriptor is not None: os.close(self.descriptor)
        self.desktop.close()
