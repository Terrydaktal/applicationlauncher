"""Shared application adapters and lifecycle worker, embedded in applicationlauncherd.

Inspection never sends input or signals. Execution requires a daemon-owned,
confirmed plan, revalidates it, and writes recovery metadata before disruption.
"""

import hashlib
from contextlib import contextmanager
import fcntl
import errno
import json
import os
from pathlib import Path
import shlex
import shutil
import signal
import socket
import stat
import subprocess
import sys
import tempfile
import time
import uuid

LIMIT = 2 * 1024 * 1024
TERMINAL_PATH = "/org/xfce/Terminal"
TERMINAL_INTERFACE = "org.xfce.Terminal5"
SHELLS = {"fish", "bash", "zsh"}
COLOUR_ENVIRONMENT = {"NO_COLOR", "FORCE_COLOR", "CLICOLOR", "CLICOLOR_FORCE", "COLORTERM"}
TERMINAL_ENVIRONMENT = COLOUR_ENVIRONMENT | {"TERM", "LANG", "LANGUAGE", "LC_ALL", "LC_CTYPE", "LC_MESSAGES", "TZ"}
RESTART_METHODS = ("PrepareTabRestart", "CommitTabRestart", "CancelTabRestart", "GetTabRestartStatus")
CODEX_SESSION_ERROR = "The exact Codex session is unavailable or ambiguous; resume --last is not used"
PI_LAUNCHER = "pi-remote-qwen-radiance"
PI_SESSION_ERROR = "The exact Pi session is unavailable or ambiguous; --session last is not used"


class Refused(Exception):
    pass


def failure_details(error):
    """Required recovery result: preserve the first typed cause, without payloads."""
    code = ("policy-refused" if isinstance(error, Refused) else
            "timeout" if isinstance(error, (TimeoutError, subprocess.TimeoutExpired)) else
            "io" if isinstance(error, OSError) else
            "invalid-input" if isinstance(error, (ValueError, KeyError, TypeError)) else
            "internal")
    causes, seen = [], set()
    current = error
    while current is not None and id(current) not in seen and len(causes) < 8:
        seen.add(id(current))
        causes.append(dict(kind=type(current).__name__[:64],
                           errno=current.errno if isinstance(current, OSError) else None))
        current = current.__cause__ or (None if current.__suppress_context__ else current.__context__)
    return dict(schema_version=1, code=code, causes=causes, truncated=current is not None)


@contextmanager
def operation_lock():
    base = Path(os.environ.get("XDG_STATE_HOME", str(Path.home() / ".local/state"))) / "applicationlauncher/lifecycle"
    base.mkdir(mode=0o700, parents=True, exist_ok=True)
    metadata = base.lstat()
    if not stat.S_ISDIR(metadata.st_mode) or metadata.st_uid != os.getuid() or metadata.st_mode & 0o077:
        raise Refused("Lifecycle directory is not a private same-user directory")
    descriptor = os.open(base / "operation.lock", os.O_RDWR | os.O_CREAT | os.O_NOFOLLOW | os.O_NONBLOCK, 0o600)
    try:
        metadata = os.fstat(descriptor)
        if not stat.S_ISREG(metadata.st_mode) or metadata.st_uid != os.getuid() or metadata.st_mode & 0o077 or metadata.st_nlink != 1:
            raise Refused("Lifecycle lock is not private")
        try:
            fcntl.flock(descriptor, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError as error:
            raise Refused("Another lifecycle operation is in progress; nothing was stopped") from error
        yield
    finally:
        os.close(descriptor)


def read_limited(path, limit=LIMIT):
    descriptor = os.open(path, os.O_RDONLY | os.O_NONBLOCK)
    with os.fdopen(descriptor, "rb") as stream:
        data = stream.read(limit + 1)
    if len(data) > limit:
        raise Refused("Process or application metadata exceeds its safety limit")
    return data


def file_identity(path):
    metadata = os.stat(path)
    return [metadata.st_dev, metadata.st_ino, metadata.st_size, metadata.st_mtime_ns]


def terminal_environment(pid):
    result = dict.fromkeys(TERMINAL_ENVIRONMENT)
    for value in read_limited(f"/proc/{pid}/environ", 128 * 1024).split(b"\0"):
        key, separator, value = value.partition(b"=")
        name = os.fsdecode(key)
        if separator and name in result:
            result[name] = os.fsdecode(value)
    return result


def process(pid):
    if not isinstance(pid, int) or pid <= 1 or os.stat(f"/proc/{pid}").st_uid != os.getuid():
        raise Refused("A live same-user process is required")
    fields = read_limited(f"/proc/{pid}/stat", 16384).decode().rsplit(")", 1)[1].split()
    if fields[0] in ("Z", "X", "x"):
        raise ProcessLookupError(pid)
    executable = os.readlink(f"/proc/{pid}/exe")
    return dict(pid=pid, start=int(fields[19]), parent=int(fields[1]), group=int(fields[2]),
                tty=int(fields[4]), foreground=int(fields[5]), executable=executable,
                image=file_identity(f"/proc/{pid}/exe"),
                arguments=[os.fsdecode(arg) for arg in read_limited(f"/proc/{pid}/cmdline", 65536).split(b"\0") if arg],
                cwd=os.path.realpath(f"/proc/{pid}/cwd"))


def same_process(saved):
    try:
        current = process(saved["pid"])
        return all(current[key] == saved[key] for key in ("pid", "start", "image", "tty", "group"))
    except (OSError, ValueError, Refused):
        return False


def children(pid):
    result = [int(value) for value in read_limited(f"/proc/{pid}/task/{pid}/children", 16384).split()]
    if len(result) > 512:
        raise Refused("Too many child processes to identify this application safely")
    return result


def kind(saved):
    name = Path(saved["executable"].removesuffix(" (deleted)")).name
    if name in {"node", "nodejs"} and saved["arguments"] == ["pi"]:
        return "pi"
    return "codex" if name in ("codex", "codex-code-mode") else name


def foreground(root_pid, pgid):
    root = process(root_pid)
    pending, seen, matches = [root_pid], set(), []
    while pending:
        pid = pending.pop()
        if pid in seen:
            continue
        seen.add(pid)
        if len(seen) > 512:
            raise Refused("Terminal process tree exceeds its safety limit")
        try:
            current = process(pid)
            if current["tty"] != root["tty"]:
                continue
            if current["group"] == pgid and kind(current) in SHELLS | {"codex", "pi", "tmux", "htop", "nvtop"}:
                matches.append(current)
            if kind(current) != "tmux":
                pending.extend(children(pid))
        except (OSError, ProcessLookupError):
            continue
    applications = [saved for saved in matches if kind(saved) not in SHELLS]
    if len(applications) == 1:
        return root, applications[0]
    if not applications and root["group"] == pgid and kind(root) in SHELLS and not children(root_pid):
        return root, root
    raise Refused("The active program is unsupported, ambiguous, or has background jobs")


def require_dedicated_tree(root, active, program=None):
    """Replacing a tab/pane must not terminate unrelated sibling shell jobs."""
    cursor = active
    for _ in range(32):
        if cursor["pid"] == root["pid"]:
            return
        parent = process(cursor["parent"])
        allowed = {cursor["pid"]}
        if program and program["kind"] == "pi" and parent["pid"] == program["launcher"]["pid"]:
            allowed.update(helper["pid"] for helper in program["helpers"])
        if set(children(parent["pid"])) != allowed:
            raise Refused("The terminal/pane shell has other jobs; restarting this program alone would interrupt them")
        cursor = parent
    raise Refused("The active program does not have a verified, dedicated terminal process chain")


def latest_binary(name):
    published = shutil.which(name)
    if published is None:
        raise Refused(f"No installed {name} executable is available")
    executable = str(Path(published).resolve(strict=True))
    with open(executable, "rb") as stream:
        if stream.read(4) != b"\x7fELF":
            raise Refused(f"The published {name} launcher is not a verified native binary")
    return executable


def session_uuid(value):
    if not isinstance(value, str):
        raise ValueError("A session ID must be a UUID string")
    return str(uuid.UUID(value))


def rollout_session_identity(row):
    if not isinstance(row, dict) or row.get("type") != "session_meta":
        raise ValueError("The rollout header is not session metadata")
    payload = row.get("payload")
    if not isinstance(payload, dict):
        raise ValueError("The rollout session metadata is invalid")
    identity = session_uuid(payload.get("id"))
    parent_values = [payload.get("parent_thread_id")]
    source = payload.get("source")
    subagent = isinstance(source, dict) and "subagent" in source
    if subagent and isinstance(source["subagent"], dict) and "thread_spawn" in source["subagent"]:
        spawn = source["subagent"]["thread_spawn"]
        if not isinstance(spawn, dict):
            raise ValueError("The child-agent origin is invalid")
        parent_values.append(spawn.get("parent_thread_id"))
    subagent = subagent or payload.get("thread_source") == "subagent"
    parents = {session_uuid(parent) for parent in parent_values if parent is not None}
    if len(parents) > 1 or (subagent and not parents):
        raise ValueError("The child-agent parent is conflicting or unavailable")
    return identity, next(iter(parents), None)


def session_id(saved):
    found = set()
    parents = {}
    arguments = saved["arguments"]
    for before, value in zip(arguments, arguments[1:]):
        if before == "resume":
            try:
                found.add(session_uuid(value))
            except ValueError:
                pass
    for index, entry in enumerate(Path(f"/proc/{saved['pid']}/fd").iterdir()):
        if index >= 512:
            raise Refused("Too many descriptors to identify the exact Codex session")
        try:
            target = Path(os.readlink(entry).removesuffix(" (deleted)"))
            if not target.name.startswith("rollout-") or target.suffix != ".jsonl":
                continue
            descriptor = os.open(entry, os.O_RDONLY | os.O_NONBLOCK)
            with os.fdopen(descriptor, "rb") as stream:
                if not stat.S_ISREG(os.fstat(stream.fileno()).st_mode):
                    raise Refused(CODEX_SESSION_ERROR + "; rollout metadata is not a regular file")
                line = stream.readline(1024 * 1024 + 1)
            if len(line) > 1024 * 1024:
                raise Refused(CODEX_SESSION_ERROR + "; rollout metadata exceeds its safety limit")
            identity, parent = rollout_session_identity(json.loads(line))
            if identity in parents and parents[identity] != parent:
                raise Refused(CODEX_SESSION_ERROR + "; conflicting rollout lineage")
            parents[identity] = parent
            if parent is None:
                found.add(identity)
        except FileNotFoundError:
            continue
        except (OSError, ValueError, KeyError, TypeError) as error:
            raise Refused(CODEX_SESSION_ERROR + "; rollout metadata could not be verified") from error
    if len(found) != 1:
        raise Refused(CODEX_SESSION_ERROR)
    root = found.pop()
    if parents.get(root) is not None:
        raise Refused(CODEX_SESSION_ERROR + "; the requested session is a child agent")
    # Ignore child logs only after their complete observed lineage reaches this main conversation.
    for identity in parents:
        seen = set()
        while identity != root:
            if identity in seen or parents.get(identity) is None:
                raise Refused(CODEX_SESSION_ERROR + "; child-agent lineage is incomplete or unrelated")
            seen.add(identity)
            identity = parents[identity]
    return root


def codex_options(arguments):
    result, index = [], 1
    values = {"-m", "--model", "-p", "--profile", "-c", "--config", "--sandbox", "-a", "--ask-for-approval"}
    switches = {"--dangerously-bypass-approvals-and-sandbox", "--dangerously-bypass-all-permissions",
                "--yolo", "--full-auto", "--search", "--no-alt-screen"}
    while index < len(arguments):
        value = arguments[index]
        index += 1
        if value in ("resume", "--last"):
            continue
        try:
            uuid.UUID(value)
            continue
        except ValueError:
            pass
        if value in switches:
            result.append(value)
        elif value in values and index < len(arguments):
            result.extend([value, arguments[index]])
            index += 1
        elif any(value.startswith(option + "=") for option in values if option.startswith("--")):
            result.append(value)
        else:
            raise Refused(f"Codex launch option cannot be replayed safely: {value[:80]}")
    return result


def owned_directory(path):
    metadata = os.lstat(path)
    if not stat.S_ISDIR(metadata.st_mode) or metadata.st_uid != os.getuid() or metadata.st_mode & 0o022:
        raise Refused(f"Unsafe Pi metadata directory: {path}")


def owned_json(path, header=False):
    descriptor = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    with os.fdopen(descriptor, "rb") as stream:
        metadata = os.fstat(stream.fileno())
        if (not stat.S_ISREG(metadata.st_mode) or metadata.st_uid != os.getuid()
                or metadata.st_nlink != 1 or metadata.st_mode & 0o022):
            raise Refused("Unsafe Pi session/activity metadata")
        limit = 16384 if header else 512 * 1024
        data = stream.readline(limit + 1) if header else stream.read(limit + 1)
        if len(data) > limit:
            raise Refused("Pi session/activity metadata exceeds its safety limit")
    value = json.loads(data)
    if not isinstance(value, dict):
        raise Refused("Invalid Pi session/activity metadata")
    return value, [metadata.st_dev, metadata.st_ino]


def pi_activity(saved):
    root = Path.home() / ".local/state/qwen-r9700/pi-remote/radiance-public-clean-candidate"
    owned_directory(root)
    matches = []
    for index, agent in enumerate(root.iterdir()):
        if index >= 256:
            raise Refused("Pi activity directory exceeds its safety limit")
        if not agent.name.startswith("agent-") or not agent.name[6:].isdigit():
            continue
        directory = agent / "radiance-active"
        path = directory / f"{saved['pid']}.json"
        try:
            owned_directory(agent)
            owned_directory(directory)
            record, _ = owned_json(path)
        except FileNotFoundError:
            continue
        if (record.get("schema") != "urn:qwen-r9700:radiance-active-pi:v1"
                or record.get("pid") != saved["pid"]
                or record.get("process_start_ticks") != str(saved["start"])
                or type(record.get("port")) is not int
                or not 1 <= record["port"] <= 65535
                or agent.name != f"agent-{record['port']}"
                or not isinstance(record.get("chat_id"), str)
                or len(record["chat_id"]) != 64):
            raise Refused(PI_SESSION_ERROR + "; activity identity does not match the live process")
        matches.append(record)
    if len(matches) != 1:
        raise Refused(PI_SESSION_ERROR + "; one process-bound activity record is required")
    return matches[0]


def pi_session(saved, activity):
    directory = Path(saved["cwd"]) / ".pi/sessions"
    owned_directory(directory.parent)
    owned_directory(directory)
    try:
        index, _ = owned_json(directory / ".identity.json")
        if (index.get("schema") != "urn:qwen-r9700:pi-project-history:v1"
                or not isinstance(index.get("sessions"), dict)):
            raise Refused("Invalid Pi project history identity index")
        identities = index["sessions"]
    except FileNotFoundError:
        identities = {}
    matches = []
    for ordinal, path in enumerate(directory.iterdir()):
        if ordinal >= 512:
            raise Refused("Pi project session directory exceeds its safety limit")
        if path.suffix != ".jsonl":
            continue
        header, inode = owned_json(path, header=True)
        if header.get("type") != "session":
            raise Refused("Invalid Pi session header")
        identity = session_uuid(header.get("id"))
        original = str(path)
        if path.name in identities:
            record = identities[path.name]
            if (not isinstance(record, dict) or [record.get("device"), record.get("inode")] != inode
                    or not isinstance(record.get("identity_path"), str)
                    or not Path(record["identity_path"]).is_absolute()):
                raise Refused("Pi project history identity no longer matches the session file")
            original = os.path.normpath(record["identity_path"])
        digest = hashlib.sha256(json.dumps([original, identity], ensure_ascii=False, separators=(",", ":")).encode()).hexdigest()
        if digest == activity["chat_id"]:
            matches.append(dict(session_file=str(path), session_id=identity, session_inode=inode))
    if len(matches) != 1:
        raise Refused(PI_SESSION_ERROR + "; the live chat fingerprint has no unique session-header match")
    return matches[0]


def pi_options(arguments):
    result, index, seen = ["--reuse-existing", "--no-install"], 0, set()
    while index < len(arguments):
        option = arguments[index]
        index += 1
        if option in seen:
            raise Refused("Duplicate Pi launcher option")
        seen.add(option)
        if option in {"--reuse-existing", "--no-install"}:
            continue
        if option.startswith("--session="):
            if index != len(arguments) or not option.partition("=")[2]:
                raise Refused("Unsupported Pi session launch arguments")
            break
        if option not in {"--thinking", "--remote", "--local-port", "--session"} or index >= len(arguments):
            raise Refused(f"Pi launch option cannot be replayed safely: {option[:80]}")
        value = arguments[index]
        index += 1
        if option == "--session":
            if index != len(arguments) or not value:
                raise Refused("Unsupported Pi session launch arguments")
            break
        if ((option == "--thinking" and value not in {"off", "low", "medium", "xhigh"})
                or (option == "--local-port" and (not value.isdigit() or not 1 <= int(value) <= 65535))
                or (option == "--remote" and (not value or value.startswith("-")
                    or any(character not in "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789._@:-" for character in value)))):
            raise Refused("Unsupported Pi launcher configuration")
        result.extend([option, value])
    return result


def pi_plan(saved):
    launcher = process(saved["parent"])
    arguments = launcher["arguments"]
    if (kind(launcher) != "bash" or len(arguments) < 2
            or not arguments[1].startswith("/proc/self/fd/")
            or not arguments[1].removeprefix("/proc/self/fd/").isdigit()
            or any(launcher[key] != saved[key] for key in ("group", "tty", "cwd"))):
        raise Refused("Pi requires its verified Radiance launcher; the Node process title alone is not a restart recipe")
    script = f"/proc/{launcher['pid']}/fd/{arguments[1].rsplit('/', 1)[1]}"
    if os.readlink(script) != f"/memfd:{PI_LAUNCHER} (deleted)":
        raise Refused("Pi's original launcher script is unavailable or unsupported")
    descriptor = os.open(script, os.O_RDONLY | os.O_NONBLOCK)
    try:
        seals = fcntl.F_SEAL_WRITE | fcntl.F_SEAL_SHRINK | fcntl.F_SEAL_GROW | fcntl.F_SEAL_SEAL
        if fcntl.fcntl(descriptor, fcntl.F_GET_SEALS) & seals != seals:
            raise Refused("Pi's original launcher source is not sealed")
    finally:
        os.close(descriptor)
    activity = pi_activity(saved)
    session = pi_session(saved, activity)
    published = shutil.which(PI_LAUNCHER)
    if not published:
        raise Refused("No installed Pi Radiance launcher is available")
    executable = str(Path(published).resolve(strict=True))
    metadata = os.stat(executable)
    if not stat.S_ISREG(metadata.st_mode) or metadata.st_uid != os.getuid() or metadata.st_mode & 0o022:
        raise Refused("The installed Pi launcher is unsafe")
    helpers = []
    for pid in children(launcher["pid"]):
        if pid == saved["pid"]:
            continue
        helper = process(pid)
        argv = helper["arguments"]
        if (kind(helper) != "ssh" or helper["parent"] != launcher["pid"]
                or any(helper[key] != launcher[key] for key in ("group", "tty"))
                or argv[1:7] != ["-N", "-T", "-o", "ExitOnForwardFailure=yes", "-L", f"127.0.0.1:{activity['port']}:127.0.0.1:8080"]
                or len(argv) != 8 or argv[7] != next((value for option, value in zip(arguments, arguments[1:]) if option == "--remote"), "lewis@ai")
                or children(pid)):
            raise Refused("The Pi launcher has unrelated background jobs; only its verified SSH tunnel may be restarted")
        helpers.append(helper)
    if len(helpers) > 1 or children(saved["pid"]):
        raise Refused("Pi has active tool/background jobs; wait for it to become idle before restarting")
    entry = str((Path.home() / ".local/bin/pi").resolve(strict=True))
    result = dict(kind="pi", process=saved, launcher=launcher, helpers=helpers, cwd=saved["cwd"],
                  argv=[executable, *pi_options(arguments[2:]), "--session", session["session_file"]],
                  installed=file_identity(latest_binary("node")), launcher_image=file_identity(script),
                  installed_launcher=file_identity(executable), entry_point=entry, installed_entry=file_identity(entry), **session)
    if not same_process(saved) or not same_process(launcher):
        raise Refused("The Pi process or launcher changed during inspection")
    return result


def program_plan(saved, executable=None):
    name = kind(saved)
    if not os.path.isdir(saved["cwd"]):
        raise Refused("The program working directory is unavailable")
    if name == "pi":
        try:
            return pi_plan(saved)
        except (OSError, ValueError, KeyError) as error:
            raise Refused(PI_SESSION_ERROR + f"; required launcher/activity metadata is invalid or inaccessible ({type(error).__name__})") from error
    executable = latest_binary(name) if executable is None else executable
    if name == "codex":
        arguments = [executable, "resume", session_id(saved), *codex_options(saved["arguments"])]
    elif name in SHELLS:
        if children(saved["pid"]):
            raise Refused("This shell still has child/background jobs; it cannot be restarted alone")
        arguments = [executable, "-l"]
    elif name in {"htop", "nvtop"}:
        arguments = [executable, *saved["arguments"][1:]]
    else:
        raise Refused(f"No verified restart recipe for {name}")
    return dict(kind=name, process=saved, argv=arguments, cwd=saved["cwd"],
                installed=file_identity(executable))


def unpack(value):
    if hasattr(value, "unpack"):
        return unpack(value.unpack())
    if isinstance(value, dict):
        return {key: unpack(item) for key, item in value.items()}
    if isinstance(value, (tuple, list)):
        return [unpack(item) for item in value]
    return value


class System:
    def __init__(self):
        import gi
        gi.require_version("Gio", "2.0")
        from gi.repository import Gio, GLib
        self.Gio, self.GLib = Gio, GLib
        self.bus = Gio.bus_get_sync(Gio.BusType.SESSION, None)

    def call(self, owner, path, interface, method, signature="()", arguments=(), timeout=2000):
        reply = self.bus.call_sync(owner, path, interface, method,
            self.GLib.Variant(signature, tuple(arguments)), None, self.Gio.DBusCallFlags.NO_AUTO_START, timeout, None)
        return unpack(reply)

    def dbus(self, method, signature="()", arguments=()):
        return self.call("org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus",
                         method, signature, arguments)

    def terminals(self):
        records, seen = [], set()
        for name in self.dbus("ListNames")[0]:
            if name != "org.xfce.Terminal5" and not name.startswith("org.xfce.Terminal5.Instance."):
                continue
            owner = self.dbus("GetNameOwner", "(s)", [name])[0]
            if owner in seen:
                continue
            seen.add(owner)
            if len(seen) > 256:
                raise Refused("Too many terminal owners to inspect safely")
            pid = self.dbus("GetConnectionUnixProcessID", "(s)", [owner])[0]
            if self.dbus("GetConnectionUnixUser", "(s)", [owner])[0] != os.getuid():
                continue
            values = self.call(owner, TERMINAL_PATH, TERMINAL_INTERFACE, "ListTerminals")[0]
            xml = self.call(owner, TERMINAL_PATH, "org.freedesktop.DBus.Introspectable", "Introspect")[0]
            supported = all(f'name="{method}"' in xml or f"name='{method}'" in xml for method in RESTART_METHODS)
            for record in values:
                records.append(dict(record, owner=owner, owner_pid=pid, restart_supported=supported))
            if len(records) > 1024:
                raise Refused("Too many terminal tabs to inspect safely")
        return records

    def prepare(self, binding, arguments, cwd):
        return self.call(binding["owner"], TERMINAL_PATH, TERMINAL_INTERFACE, "PrepareTabRestart", "(suass)",
                         [binding["tab_uuid"], binding["child_pid"], arguments, cwd])[0]

    def tab(self, binding, token, method):
        reply = self.call(binding["owner"], TERMINAL_PATH, TERMINAL_INTERFACE, method, "(ss)",
                          [binding["tab_uuid"], token])
        return json.loads(reply[0]) if method == "GetTabRestartStatus" else None


def binding_identity(binding):
    return {key: binding.get(key) for key in ("owner", "owner_pid", "window_uuid", "tab_uuid",
            "child_pid", "foreground_pgid", "pty", "active")}


def require_handoff(binding):
    if not binding.get("restart_supported"):
        raise Refused("Update this xfce4-terminal window first; it lacks the safe tab-replacement API")
    if not binding.get("input_enabled", True):
        raise Refused("The terminal tab input is disabled or another update is pending")


def run_tmux(server, *arguments, create=False, installed=False):
    environment = os.environ.copy()
    for name in ("TMUX", "TMUX_PANE", "XDG_ACTIVATION_TOKEN", "DESKTOP_STARTUP_ID"):
        environment.pop(name, None)
    if create and "colour_environment" in server:
        for name in COLOUR_ENVIRONMENT:
            environment.pop(name, None)
            value = server["colour_environment"].get(name)
            if value is not None:
                if not isinstance(value, str) or "\0" in value or len(value) > 8192:
                    raise Refused("Invalid captured tmux colour environment")
                environment[name] = value
    executable = server["binary"] if create or installed else f"/proc/{server['process']['pid']}/exe"
    command = [executable, *([] if create else ["-N"]), "-S", server["socket"], *arguments]
    try:
        output = subprocess.run(command, env=environment, stdin=subprocess.DEVNULL,
                                stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=5)
    except subprocess.TimeoutExpired as error:
        raise Refused(f"tmux {arguments[0]} timed out; server state must be checked before retrying") from error
    if output.returncode or len(output.stdout) + len(output.stderr) > LIMIT:
        raise Refused(f"tmux {arguments[0]} failed: " + output.stderr.decode(errors="replace")[:1000])
    return output.stdout.decode()


def restart_event(directory, phase, **details):
    path = Path(directory) / "restart-events.jsonl"
    descriptor = os.open(path, os.O_CREAT | os.O_APPEND | os.O_WRONLY | os.O_NOFOLLOW | os.O_NONBLOCK, 0o600)
    with os.fdopen(descriptor, "w") as stream:
        metadata = os.fstat(stream.fileno())
        if (not stat.S_ISREG(metadata.st_mode) or metadata.st_uid != os.getuid()
                or metadata.st_mode & 0o077 or metadata.st_nlink != 1 or metadata.st_size > LIMIT):
            raise Refused("The restart progress record must be a private regular file owned by this user")
        stream.write(json.dumps(dict(unix_ms=time.time_ns() // 1_000_000, phase=phase, **details)) + "\n")
        stream.flush()
        os.fsync(stream.fileno())
    descriptor = os.open(directory, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
    try:
        os.fsync(descriptor)
    finally:
        os.close(descriptor)


def server_listening(server):
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as connection:
        connection.settimeout(1)
        try:
            connection.connect(server["socket"])
        except OSError as error:
            if error.errno in (errno.ENOENT, errno.ECONNREFUSED):
                return False
            raise Refused("Cannot prove the tmux listener is unused; no replacement will be started") from error
    return True


def replacement_server(server):
    before = os.stat(server["socket"], follow_symlinks=False)
    if not stat.S_ISSOCK(before.st_mode) or before.st_uid != os.getuid():
        raise Refused("The replacement tmux socket is not owned by this user")
    pid = int(run_tmux(server, "display-message", "-p", "#{pid}", installed=True).strip())
    saved = process(pid)
    after = os.stat(server["socket"], follow_symlinks=False)
    if (saved["image"] != server["installed"] or (before.st_dev, before.st_ino) != (after.st_dev, after.st_ino)):
        raise Refused("The replacement tmux server or socket changed; no other server will be modified")
    return dict(server, process=saved, socket_identity=[after.st_dev, after.st_ino])


def tmux_client(saved):
    arguments = saved["arguments"][1:]
    label, explicit, index, explicit_label = "default", None, 0, False
    while index < len(arguments) and arguments[index].startswith("-"):
        option = arguments[index]
        index += 1
        if option in ("-S", "-L", "-f", "-T"):
            if index >= len(arguments):
                raise Refused("Incomplete tmux connection arguments")
            value = arguments[index]
            index += 1
            if option == "-S":
                explicit = value
            elif option == "-L":
                label = value
                explicit_label = True
        elif option.startswith("-S"):
            explicit = option[2:]
        elif option.startswith("-L"):
            label = option[2:]
            explicit_label = True
    environment = {}
    for value in read_limited(f"/proc/{saved['pid']}/environ", 128 * 1024).split(b"\0"):
        key, _, entry = value.partition(b"=")
        if key in (b"TMUX_TMPDIR", b"TMUX"):
            environment[key.decode()] = os.fsdecode(entry)
    if not explicit and not explicit_label and "TMUX" in environment:
        explicit = environment["TMUX"].rsplit(",", 2)[0]
    if not label or "/" in label:
        raise Refused("Invalid tmux socket label")
    socket = explicit or str(Path(environment.get("TMUX_TMPDIR", "/tmp")) / f"tmux-{os.getuid()}" / label)
    metadata = os.stat(socket)
    if not Path(socket).is_absolute() or metadata.st_uid != os.getuid() or not stat.S_ISSOCK(metadata.st_mode):
        raise Refused("A same-user tmux socket is required")
    server = dict(socket=socket, binary=latest_binary("tmux"), process=saved,
                  socket_identity=[metadata.st_dev, metadata.st_ino])
    rows = run_tmux(server, "list-clients", "-F", "#{client_pid}\t#{pid}\t#{session_id}\t#{session_name}")
    matches = [line.split("\t") for line in rows.splitlines() if line.split("\t")[0] == str(saved["pid"])]
    if len(matches) != 1 or len(matches[0]) != 4:
        raise Refused("The exact tmux client could not be identified")
    _, pid, session, name = matches[0]
    server["process"] = process(int(pid))
    server["installed"] = file_identity(server["binary"])
    return server, session, name


def rows(text, width):
    result = [line.split("\t") for line in text.splitlines()]
    if len(result) > 1024 or any(len(row) != width for row in result):
        raise Refused("Unsupported control characters or oversized tmux session metadata")
    return result


def require_requested_process(saved, requested):
    if requested is None:
        return
    if requested["application"] == "fish" and kind(saved) != "fish":
        raise Refused("Fish has a foreground application; update it from an idle Fish prompt. No other application will be restarted.")
    if (saved["pid"] != requested["pid"] or saved["start"] != requested["start_ticks"]
            or kind(saved) != requested["application"]):
        raise Refused("The selected process is no longer this tab or pane's active program; reopen Show Info. No other process will be restarted.")


def pane_plan(server, pane_id, requested=None):
    values = rows(run_tmux(server, "display-message", "-p", "-t", pane_id,
                          "#{pane_id}\t#{pane_pid}\t#{pane_dead}"), 3)
    if len(values) != 1 or values[0][0] != pane_id or values[0][2] != "0":
        raise Refused("The tmux pane is missing or no longer running")
    root = process(int(values[0][1]))
    _, active = foreground(root["pid"], root["foreground"])
    require_requested_process(active, requested)
    program = program_plan(active)
    require_dedicated_tree(root, active, program)
    remain = run_tmux(server, "show-options", "-p", "-v", "-t", pane_id, "remain-on-exit").strip()
    return dict(pane_id=pane_id, root=root, program=program, remain_on_exit=remain)


def tab_for_client(client, records):
    candidates = []
    for binding in records:
        try:
            root = process(int(binding["child_pid"]))
            cursor = client
            for _ in range(32):
                if cursor["pid"] == root["pid"]:
                    require_handoff(binding)
                    require_dedicated_tree(root, client)
                    candidates.append(binding)
                    break
                if cursor["parent"] <= 1 or cursor["tty"] != root["tty"]:
                    break
                cursor = process(cursor["parent"])
        except (OSError, ProcessLookupError, KeyError):
            continue
    if len(candidates) != 1:
        raise Refused("Every tmux client must map unambiguously to a restart-capable xfce4-terminal tab")
    return candidates[0]


def remote_mosh_client(client, records=()):
    """Only verified mosh descendants may be disconnected without a local tab handoff."""
    local_pids = {str(record.get(key)) for record in records for key in ("owner_pid", "child_pid")}
    cursor, ancestry = client, []
    try:
        if kind(client) != "tmux" or not client["tty"]:
            return None
        for _ in range(32):
            if str(cursor["pid"]) in local_pids or kind(cursor) == "xfce4-terminal":
                return None
            ancestry.append({key: cursor[key] for key in ("pid", "start", "parent", "group", "tty", "image", "executable")})
            if kind(cursor) == "mosh-server":
                return dict(transport="mosh", ancestry=ancestry)
            if cursor["parent"] <= 1 or cursor["tty"] != client["tty"]:
                return None
            cursor = process(cursor["parent"])
    except (OSError, ValueError, KeyError, Refused):
        return None
    return None


def server_plan(server, records):
    sessions = rows(run_tmux(server, "list-sessions", "-F",
        "#{session_id}\t#{session_name}\t#{session_created}\t#{session_group}\t#{window_id}"), 5)
    if not sessions or any(row[3] for row in sessions):
        raise Refused("Empty or grouped tmux sessions are not supported for server replacement")
    windows = rows(run_tmux(server, "list-windows", "-a", "-F",
        "#{session_id}\t#{window_id}\t#{window_index}\t#{window_name}\t#{window_layout}\t#{window_width}\t#{window_height}\t#{window_zoomed_flag}"), 8)
    if len({row[1] for row in windows}) != len(windows):
        raise Refused("Linked tmux windows are not supported for server replacement; no sessions were stopped")
    panes = rows(run_tmux(server, "list-panes", "-a", "-F",
        "#{window_id}\t#{pane_id}\t#{pane_index}\t#{pane_active}"), 4)
    if len(panes) > 128:
        raise Refused("More than 128 tmux panes exceed the bounded server-replacement limit")
    clients = rows(run_tmux(server, "list-clients", "-F", "#{client_pid}\t#{session_id}\t#{session_name}"), 3)
    unique_panes = {}
    for _, pane, _, _ in panes:
        if pane not in unique_panes:
            try:
                unique_panes[pane] = pane_plan(server, pane)
            except Refused as error:
                raise Refused(f"Pane {pane}: {error}; no sessions were stopped") from error
    bindings, remote_clients = [], []
    for pid, session, name in clients:
        client = process(int(pid))
        try:
            binding = tab_for_client(client, records)
        except Refused as error:
            remote = remote_mosh_client(client, records)
            if remote is not None:
                remote_clients.append(dict(remote, client=client, session=session, session_name=name))
                continue
            raise Refused(f"Session {name!r} (tmux client PID {pid}): {error}; no sessions were stopped") from error
        bindings.append(dict(binding=binding, client=client, session=session, session_name=name))
    if len({row["binding"]["tab_uuid"] for row in bindings}) != len(bindings):
        raise Refused("Multiple tmux clients share one terminal tab; server replacement refused")
    colours = {}
    for line in run_tmux(server, "show-environment", "-g").splitlines():
        name, separator, value = line.partition("=")
        if separator and name in COLOUR_ENVIRONMENT:
            colours[name] = value
    server = dict(server, colour_environment=colours)
    return dict(kind="tmux_server", server=server, sessions=sessions, windows=windows, panes=panes,
                programs=unique_panes, bindings=bindings, remote_clients=remote_clients)


def server_restart_scope(plan):
    server = plan["server"]
    sessions = {row[0]: row[1] for row in plan["sessions"]}
    windows = {row[1]: row for row in plan["windows"]}
    panes = []
    for window, pane, index, _ in plan["panes"]:
        program = plan["programs"][pane]["program"]
        panes.append(dict(session=sessions[windows[window][0]], window_index=windows[window][2],
                          pane_index=index, application=program["kind"], cwd=program["cwd"]))
    return dict(identity=dict(pid=server["process"]["pid"], start_ticks=server["process"]["start"],
                              socket_path=server["socket"]), executable=server["binary"],
                sessions=list(sessions.values()), windows=len(windows),
                terminal_tabs=len(plan["bindings"]), panes=panes,
                remote_clients=[dict(pid=row["client"]["pid"], session=row["session_name"], transport=row["transport"])
                                for row in plan.get("remote_clients", [])])


def require_server_clients(plan):
    clients = plan["bindings"] + plan.get("remote_clients", [])
    current = rows(run_tmux(plan["server"], "list-clients", "-F", "#{client_pid}\t#{session_id}\t#{session_name}"), 3)
    expected = [[str(row["client"]["pid"]), row["session"], row["session_name"]] for row in clients]
    if sorted(current) != sorted(expected) or any(not same_process(row["client"]) for row in clients):
        raise Refused("The tmux client bindings changed before shutdown")
    for row in plan.get("remote_clients", []):
        current = remote_mosh_client(process(row["client"]["pid"]))
        if current != {key: row[key] for key in ("transport", "ancestry")}:
            raise Refused("A remote mosh client identity or ancestry changed before shutdown")


def inspect(payload, system):
    requested = payload.get("process")
    if requested is not None and (not isinstance(requested, dict)
            or type(requested.get("pid")) is not int or requested["pid"] <= 1
            or type(requested.get("start_ticks")) is not int or requested["start_ticks"] <= 0
            or requested.get("application") not in {"fish", "codex"}):
        raise Refused("Select an exact running Fish or Codex process")
    selected = [window for window in payload["windows"] if window["id"] == payload["window_id"]]
    if len(selected) != 1 or selected[0]["class"].lower() not in {"xfce4-terminal", "org.xfce.terminal"}:
        raise Refused("Select an existing xfce4-terminal window")
    window = selected[0]
    records = system.terminals()
    same_owner_windows = [other for other in payload["windows"] if other["pid"] == window["pid"]]
    candidates = [record for record in records if record["owner_pid"] == window["pid"]
                  and record.get("active") and (len(same_owner_windows) == 1 or record["window_title"] == window["title"])]
    if len(candidates) != 1:
        raise Refused("The selected window cannot be mapped unambiguously to its active terminal tab")
    binding = candidates[0]
    root, active = foreground(int(binding["child_pid"]), int(binding["foreground_pgid"]))
    actions, plans = [], {}

    def add(identifier, label, factory):
        try:
            plan = factory()
            plan["input"] = payload
            plans[identifier] = plan
            if plan["kind"] == "tmux_server":
                detail = f"{len(plan['sessions'])} sessions, {len(plan['programs'])} panes, {len(plan['bindings'])} terminal tabs. All are interrupted and recreated; terminal windows stay in place. Scrollback, shell-local state and runtime-only tmux options are not preserved."
                if plan.get("remote_clients"):
                    detail += f" {len(plan['remote_clients'])} remote mosh tmux client(s) will be disconnected, not reattached; reconnect manually after the update."
            else:
                detail = f"Restart {plan['program']['kind']} at {plan['program']['cwd']}. Other terminal windows and tmux panes are left untouched. Shell-local state in this tab/pane is not preserved."
            action = dict(id=identifier, label=label, eligible=True, message=detail)
            if plan["kind"] == "tmux_server":
                action["tmux_server"] = server_restart_scope(plan)
            else:
                program = plan["program"]
                saved = program["process"]
                action["program"] = dict(identity=dict(pid=saved["pid"], start_ticks=saved["start"], application=program["kind"]),
                                         executable=program["argv"][0], cwd=program["cwd"])
                if plan["kind"] == "tmux_pane":
                    action["program"].update(tmux_pane=plan["pane_id"], tmux_socket=plan["server"]["socket"])
            actions.append(action)
        except Exception as error:
            actions.append(dict(id=identifier, label=label, eligible=False, message=str(error)))

    if kind(active) == "tmux":
        server, session, _ = tmux_client(active)
        pane = run_tmux(server, "display-message", "-p", "-t", session, "#{pane_id}").strip()
        def pane_restart():
            value = pane_plan(server, pane, requested) if requested is not None else pane_plan(server, pane)
            return dict(kind="tmux_pane", server=server, **value)
        if requested is not None:
            add("program", f"Update only {requested['application']} in this tmux pane", pane_restart)
            return dict(actions=actions, plans=plans, title=window["title"])
        try:
            current = pane_restart()
            name = current["program"]["kind"]
        except Exception:
            name = "active program"
        if name in SHELLS | {"codex", "pi"}:
            add("program", f"Update {name} in this tmux pane", pane_restart)
        if name != "fish" and kind(process(int(run_tmux(server, "display-message", "-p", "-t", pane, "#{pane_pid}").strip()))) == "fish":
            actions.append(dict(id="fish", label="Update fish in this tmux pane", eligible=False,
                                message="Fish has a foreground application; restart it from an idle fish prompt instead."))
        add("tmux_server", "Update this tmux server and recreate all its sessions", lambda: server_plan(server, records))
    elif requested is not None or kind(active) in SHELLS | {"codex", "pi"}:
        def native_restart():
            require_handoff(binding)
            require_requested_process(active, requested)
            program = program_plan(active)
            require_dedicated_tree(root, active, program)
            return dict(kind="terminal_program", binding=binding, root=root, program=program)
        add("program", f"Update {kind(active)} in this terminal tab", native_restart)
        if requested is None and kind(root) == "fish" and kind(active) != "fish":
            actions.append(dict(id="fish", label="Update fish in this terminal tab", eligible=False,
                                message="Fish has a foreground application; restart it from an idle fish prompt instead."))
    return dict(actions=actions, plans=plans, title=window["title"])


def signature(plan):
    def strip(value):
        if isinstance(value, dict):
            result = {}
            for key, item in value.items():
                if key in {"input", "window_title", "terminal_title", "working_directory", "arguments", "title"}:
                    continue
                if key == "windows" and value.get("kind") == "tmux_server":
                    # tmux automatically renames windows; names do not change restart scope.
                    item = [row[:3] + row[4:] for row in item]
                result[key] = strip(item)
            return result
        if isinstance(value, list):
            return [strip(item) for item in value]
        return value
    return hashlib.sha256(json.dumps(strip(plan), sort_keys=True).encode()).hexdigest()


def stop_codex(saved):
    if kind(saved) != "codex":
        return
    if not same_process(saved):
        raise Refused("Codex process identity changed before interruption")
    descriptor = os.pidfd_open(saved["pid"])
    try:
        if not same_process(saved):
            raise Refused("Codex process identity changed before interruption")
        for _ in range(2):
            signal.pidfd_send_signal(descriptor, signal.SIGINT)
            deadline = time.monotonic() + 3
            while time.monotonic() < deadline:
                if not same_process(saved):
                    return
                time.sleep(0.05)
        raise Refused("Codex did not exit after two graceful interrupts; no force-kill was attempted")
    finally:
        os.close(descriptor)


def wait_tabs(system, leases):
    deadline = time.monotonic() + 15
    pending = list(leases)
    completed = {}
    while pending and time.monotonic() < deadline:
        for binding, token in list(pending):
            status = system.tab(binding, token, "GetTabRestartStatus")
            if status["phase"] in ("failed", "cancelled"):
                raise Refused(status["message"])
            if status["phase"] == "complete":
                completed[binding["tab_uuid"]] = status["child_pid"]
                pending.remove((binding, token))
        if pending:
            time.sleep(0.05)
    if pending:
        raise Refused("Terminal tab replacement did not complete; inspect the retained backup before retrying")
    return completed


def fish_command(executable, arguments, shell):
    # A short exec shim preserves argv[0] independently of a pinned image path.
    # Fish remains the interactive parent, including after an interrupt/failed exit.
    command = ["/usr/bin/python3", "-I", "-c", "import os,sys;os.execv(sys.argv[1],sys.argv[2:])",
               executable, *arguments]
    return [shell, "--interactive", "--init-command", shlex.join(command)]


def pane_command(program):
    arguments = program["argv"]
    if program["kind"] == "codex":
        argv0 = program["process"]["arguments"][0]
        return fish_command(arguments[0], [argv0, *arguments[1:]], latest_binary("fish"))
    if program["kind"] == "pi":
        shell = latest_binary("fish")
        return [shell, "-lc", shlex.join(arguments) + "; exec " + shlex.quote(shell)]
    return arguments


def verify_program(program, root_pid):
    deadline = time.monotonic() + (120 if program["kind"] == "pi" else 12)
    while time.monotonic() < deadline:
        try:
            root = process(root_pid)
            _, active = foreground(root_pid, root["foreground"])
            if kind(active) == program["kind"] and active["image"] == program["installed"] and active["cwd"] == program["cwd"]:
                if program["kind"] == "pi":
                    restored = pi_plan(active)
                    if all(restored[key] == program[key] for key in ("argv", "session_id", "session_inode", "entry_point", "installed_entry", "installed_launcher")):
                        return
                elif program["kind"] != "codex" or active["arguments"][1:] == program["argv"][1:]:
                    return
        except (OSError, ProcessLookupError, ValueError, Refused):
            pass
        time.sleep(0.05)
    raise Refused("The restarted program did not reach its verified binary, directory and exact session; inspect the backup")


def stop_program(program):
    if program["kind"] != "pi":
        return stop_codex(program["process"])
    saved, launcher = program["process"], program["launcher"]
    if not same_process(saved):
        if any(same_process(value) for value in [launcher, *program["helpers"]]):
            raise Refused("Pi exited but its launcher/tunnel is still running; refresh the preview")
        return  # An earlier interrupted server update already stopped the whole chain.
    if (not same_process(launcher) or children(saved["pid"])
            or set(children(launcher["pid"])) != {saved["pid"], *(helper["pid"] for helper in program["helpers"])}
            or any(not same_process(helper) for helper in program["helpers"])):
        raise Refused("Pi's launcher changed or a tool is running; no interruption was attempted")
    if signature(pi_plan(process(saved["pid"]))) != signature(program):
        raise Refused("Pi's session, installed application or launcher changed; no interruption was attempted")
    descriptor = os.pidfd_open(saved["pid"])
    try:
        if not same_process(saved):
            raise Refused("Pi process identity changed before interruption")
        signal.pidfd_send_signal(descriptor, signal.SIGTERM)
    finally:
        os.close(descriptor)
    deadline = time.monotonic() + 5
    processes = [saved, launcher, *program["helpers"]]
    while time.monotonic() < deadline:
        if not any(same_process(value) for value in processes):
            return
        time.sleep(0.05)
    raise Refused("Pi or its launcher/tunnel did not exit gracefully; no force-kill was attempted")


def restart_pane(plan):
    server, pane, program = plan["server"], plan["pane_id"], plan["program"]
    previous_remain = run_tmux(server, "show-options", "-p", "-v", "-t", pane, "remain-on-exit").strip()
    try:
        run_tmux(server, "set-option", "-p", "-t", pane, "remain-on-exit", "on")
        stop_program(program)
        # The stopped process may have left a shell; only this pane is replaced.
        run_tmux(server, "respawn-pane", "-k", "-t", pane, "-c", program["cwd"], *pane_command(program))
        pid = int(run_tmux(server, "display-message", "-p", "-t", pane, "#{pane_pid}").strip())
        verify_program(program, pid)
    finally:
        if previous_remain:
            run_tmux(server, "set-option", "-p", "-t", pane, "remain-on-exit", previous_remain)
        else:
            run_tmux(server, "set-option", "-p", "-t", pane, "-u", "remain-on-exit")


def recreate_server(plan, directory):
    server = plan["server"]
    replacement = None
    session_map, window_map, pane_map, gates = {}, {}, {}, []
    def creation(*arguments):
        nonlocal replacement
        if replacement is not None:
            metadata = os.stat(server["socket"], follow_symlinks=False)
            if (not same_process(replacement["process"])
                    or [metadata.st_dev, metadata.st_ino] != replacement["socket_identity"]):
                raise Refused("The gated replacement server disappeared or changed; no new server will be started")
            return run_tmux(replacement, *arguments).strip()
        if arguments[0] != "new-session":
            raise Refused("A replacement must start with the first captured session")
        for attempt in range(1, 4):
            if file_identity(server["binary"]) != server["installed"] or server_listening(server):
                raise Refused("The installed tmux binary changed or a server already owns this socket")
            restart_event(directory, "starting_server", attempt=attempt, session=arguments[arguments.index("-s") + 1])
            try:
                result = run_tmux(server, *arguments, create=True).strip()
            except Refused as error:
                restart_event(directory, "startup_failed", attempt=attempt, command=arguments[0], error=str(error))
                # Only a confirmed startup exit with no listener is safe to retry. Gates
                # are still closed, so no application/conversation has been started.
                if "server exited unexpectedly" not in str(error) or server_listening(server) or attempt == 3:
                    raise
                continue
            replacement = replacement_server(server)
            restart_event(directory, "server_started", pid=replacement["process"]["pid"],
                          start=replacement["process"]["start"], socket_identity=replacement["socket_identity"])
            return result
    def gated(pane):
        program = plan["programs"][pane]["program"]
        gate = Path(directory) / ("gate-" + pane.removeprefix("%"))
        if gate.exists() or gate.is_symlink():
            raise Refused("A program startup gate already exists; the old operation cannot be replayed blindly")
        gates.append(gate)
        # A closed gate ensures no duplicate Codex session starts before every binding is rebuilt.
        command = 'while [ ! -f "$1" ]; do sleep 0.05; done; shift; exec "$@"'
        return ["/bin/sh", "-c", command, "application-update-gate", str(gate), *pane_command(program)]
    for old_session, name, _, _, active_window in plan["sessions"]:
        session_windows = [row for row in plan["windows"] if row[0] == old_session]
        if not session_windows:
            raise Refused("A captured tmux session has no windows")
        for ordinal, row in enumerate(session_windows):
            _, old_window, index, title, layout, width, height, zoomed = row
            members = sorted([pane for pane in plan["panes"] if pane[0] == old_window], key=lambda value: int(value[2]))
            if not members:
                raise Refused("A captured tmux window has no panes")
            first = members[0][1]
            cwd = plan["programs"][first]["program"]["cwd"]
            if ordinal == 0:
                returned = creation("new-session", "-d", "-P", "-F", "#{session_id}\t#{window_id}\t#{pane_id}",
                                    "-s", name, "-n", title, "-x", width, "-y", height, "-c", cwd, *gated(first))
                new_session, new_window, new_pane = returned.split("\t")
                session_map[old_session] = new_session
                actual_index = creation("display-message", "-p", "-t", new_window, "#{window_index}")
                if actual_index != index:
                    creation("move-window", "-s", new_window, "-t", f"{new_session}:{index}")
            elif old_window in window_map:
                creation("link-window", "-s", window_map[old_window], "-t", f"{session_map[old_session]}:{index}")
                continue
            else:
                returned = creation("new-window", "-d", "-P", "-F", "#{window_id}\t#{pane_id}",
                                    "-t", f"{session_map[old_session]}:{index}", "-n", title, "-c", cwd, *gated(first))
                new_window, new_pane = returned.split("\t")
            window_map[old_window], pane_map[first] = new_window, new_pane
            for _, old_pane, _, _ in members[1:]:
                program = plan["programs"][old_pane]["program"]
                new_pane = creation("split-window", "-d", "-P", "-F", "#{pane_id}", "-t", new_window,
                                    "-c", program["cwd"], *gated(old_pane))
                pane_map[old_pane] = new_pane
            creation("select-layout", "-t", new_window, layout)
            for _, old_pane, _, _ in members:
                previous_remain = plan["programs"][old_pane].get("remain_on_exit", "")
                if previous_remain:
                    creation("set-option", "-p", "-t", pane_map[old_pane], "remain-on-exit", previous_remain)
            active = next((pane[1] for pane in members if pane[3] == "1"), first)
            creation("select-pane", "-t", pane_map[active])
            if zoomed == "1":
                creation("resize-pane", "-Z", "-t", pane_map[active])
        creation("select-window", "-t", window_map[active_window])
        restart_event(directory, "session_gated", session=name, pane_map=pane_map, gates=[str(path) for path in gates])
    return gates, pane_map


def preflight_server(plan, directory):
    restart_event(directory, "startup_check")
    # A short private path also stays within the AF_UNIX socket-path limit.
    with tempfile.TemporaryDirectory(prefix="launcher-tmux-startup-") as temporary:
        probe = dict(plan, server=dict(plan["server"], socket=str(Path(temporary) / "server.sock")))
        try:
            gates, pane_map = recreate_server(probe, temporary)
            checked = replacement_server(probe["server"])
            sessions = run_tmux(checked, "list-sessions", "-F", "#{session_name}").splitlines()
            if (sorted(sessions) != sorted(row[1] for row in plan["sessions"])
                    or len(pane_map) != len(plan["programs"]) or any(gate.exists() for gate in gates)):
                raise Refused("The replacement tmux startup check did not preserve all gated sessions and panes")
        except Exception as error:
            restart_event(directory, "startup_check_failed", error=str(error))
            raise Refused(f"Replacement tmux startup check failed; original sessions were not stopped: {error}") from error
        finally:
            if server_listening(probe["server"]):
                checked = replacement_server(probe["server"])
                run_tmux(checked, "kill-server")
                deadline = time.monotonic() + 5
                while same_process(checked["process"]) and time.monotonic() < deadline:
                    time.sleep(0.05)
                if same_process(checked["process"]):
                    raise Refused("The private tmux startup check did not stop; original sessions were not stopped")
    restart_event(directory, "startup_check_complete")


def execute(plan, system, directory):
    with operation_lock():
        return execute_locked(plan, system, directory)


def execute_locked(plan, system, directory):
    fresh = inspect(plan["input"], system)
    matches = [candidate for candidate in fresh["plans"].values() if signature(candidate) == signature(plan)]
    if len(matches) != 1:
        raise Refused("The application, installed binary, active tab, or tmux sessions changed; refresh the preview")
    plan = matches[0]
    # The daemon creates a private, fsynced plan file before starting this worker.
    leases, committed, interrupted = [], False, []
    try:
        if plan["kind"] == "terminal_program":
            binding, program = plan["binding"], plan["program"]
            token = system.prepare(binding, pane_command(program), program["cwd"])
            leases.append((binding, token))
            stop_program(program)
            system.tab(binding, token, "CommitTabRestart")
            committed = True
            clients = wait_tabs(system, leases)
            verify_program(program, clients[binding["tab_uuid"]])
        elif plan["kind"] == "tmux_pane":
            restart_pane(plan)
        elif plan["kind"] == "tmux_server":
            server = plan["server"]
            preflight_server(plan, directory)
            current = inspect(plan["input"], system)
            if not any(signature(candidate) == signature(plan) for candidate in current["plans"].values()):
                raise Refused("The application or terminal bindings changed during the startup check; refresh the preview")
            require_server_clients(plan)
            for row in plan["bindings"]:
                binding = row["binding"]
                argv = [server["binary"], "-N", "-S", server["socket"], "attach-session", "-t", "=" + row["session_name"]]
                token = system.prepare(binding, argv, process(binding["child_pid"])["cwd"])
                leases.append((binding, token))
            restart_event(directory, "tabs_prepared", tabs=[binding_identity(binding) for binding, _ in leases])
            for value in plan["programs"].values():
                run_tmux(server, "set-option", "-p", "-t", value["pane_id"], "remain-on-exit", "on")
            for value in plan["programs"].values():
                if value["program"]["kind"] in {"codex", "pi"}:
                    interrupted.append(value)
                stop_program(value["program"])
            if not same_process(server["process"]) or file_identity(server["binary"]) != server["installed"]:
                raise Refused("The tmux server or installed replacement changed before shutdown")
            require_server_clients(plan)
            current_panes = rows(run_tmux(server, "list-panes", "-a", "-F", "#{window_id}\t#{pane_id}\t#{pane_index}\t#{pane_active}"), 4)
            if sorted(current_panes) != sorted(plan["panes"]):
                raise Refused("The tmux pane topology changed before shutdown")
            for binding, token in leases:
                if system.tab(binding, token, "GetTabRestartStatus")["phase"] != "prepared":
                    raise Refused("A terminal tab replacement expired before server shutdown")
            restart_event(directory, "stopping_server", pid=server["process"]["pid"])
            committed = True
            shutdown_error = None
            try:
                run_tmux(server, "kill-server")
            except Refused as error:
                shutdown_error = error
            deadline = time.monotonic() + 5
            while same_process(server["process"]) and time.monotonic() < deadline:
                time.sleep(0.05)
            if same_process(server["process"]):
                raise shutdown_error or Refused("The tmux server did not exit; no force-kill was attempted")
            if shutdown_error is not None:
                # A lost command reply may precede actual process exit. Wait for
                # the captured identity to exit before deciding whether it failed.
                restart_event(directory, "shutdown_reply_lost", error=str(shutdown_error))
            restart_event(directory, "server_stopped")
            gates, pane_map = recreate_server(plan, directory)
            for binding, token in leases:
                system.tab(binding, token, "CommitTabRestart")
            clients = wait_tabs(system, leases)
            replacement = replacement_server(server)
            attached = {int(row[0]):row[1] for row in rows(run_tmux(replacement, "list-clients", "-F", "#{client_pid}\t#{session_name}"), 2)}
            if any(attached.get(clients[row["binding"]["tab_uuid"]]) != row["session_name"] for row in plan["bindings"]):
                raise Refused("A replacement tmux client attached to the wrong session or failed to attach")
            restart_event(directory, "clients_reattached", clients=clients, pane_map=pane_map)
            for gate in gates:
                descriptor = os.open(gate, os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o600)
                os.close(descriptor)
            restart_event(directory, "programs_released")
            for old_pane, new_pane in pane_map.items():
                pid = int(run_tmux(replacement, "display-message", "-p", "-t", new_pane, "#{pane_pid}").strip())
                verify_program(plan["programs"][old_pane]["program"], pid)
            restart_event(directory, "complete")
        else:
            raise Refused("Unknown application update scope")
        message = "Update completed in the existing terminal window(s). Backup retained."
        if plan["kind"] == "tmux_server" and plan.get("remote_clients"):
            message += f" {len(plan['remote_clients'])} remote mosh tmux client(s) were disconnected; reconnect manually."
        return dict(ok=True, message=message)
    except Exception as error:
        recovered = True
        if plan["kind"] == "tmux_server" and same_process(plan["server"]["process"]):
            for pane in interrupted:
                if not same_process(pane["program"]["process"]):
                    try:
                        restart_pane(dict(server=plan["server"], **pane))
                    except Exception:
                        recovered = False
        detail = "; shutdown began: retain the backup and inspect affected tabs before retrying" if committed else "; work may have been interrupted; no force-kill was attempted"
        if plan["kind"] == "tmux_server" and not committed and not interrupted:
            detail = "; no application was stopped"
        if not recovered:
            detail += "; some interrupted programs could not be resumed; use the retained recovery plan"
        if plan["kind"] == "tmux_server":
            try:
                restart_event(directory, "failed", shutdown_began=committed, error=str(error))
            except (OSError, Refused):
                pass
        return dict(ok=False, message=str(error) + detail)
    finally:
        if plan["kind"] == "tmux_server" and same_process(plan["server"]["process"]):
            for pane in plan["programs"].values():
                try:
                    previous = pane.get("remain_on_exit", "")
                    arguments = ["set-option", "-p", "-t", pane["pane_id"]]
                    run_tmux(plan["server"], *arguments, *(["remain-on-exit", previous] if previous else ["-u", "remain-on-exit"]))
                except Exception:
                    pass
        for binding, token in leases:
            try:
                if system.tab(binding, token, "GetTabRestartStatus")["phase"] == "prepared":
                    system.tab(binding, token, "CancelTabRestart")
            except Exception:
                pass


def main(arguments):
    if len(arguments) != 2 or arguments[0] not in ("preview", "run"):
        raise Refused("Expected preview/run and a private daemon request path")
    path = Path(arguments[1])
    descriptor = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    with os.fdopen(descriptor, "rb") as stream:
        metadata = os.fstat(stream.fileno())
        if not stat.S_ISREG(metadata.st_mode) or metadata.st_uid != os.getuid() or metadata.st_mode & 0o077 or metadata.st_nlink != 1:
            raise Refused("Application update request must be a private, same-user regular file")
        data = stream.read(LIMIT + 1)
    if len(data) > LIMIT:
        raise Refused("Application update request exceeds its safety limit")
    value = json.loads(data)
    system = System()
    result = inspect(value, system) if arguments[0] == "preview" else execute(value, system, path.parent)
    print(json.dumps(result))


if __name__ == "__main__":
    try:
        main(sys.argv[1:])
    except Exception as error:
        print(json.dumps(dict(error=str(error))))
        sys.exit(1)
