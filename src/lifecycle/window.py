"""Daemon-owned window update coordinator. Terminal helpers contain no app adapters."""
import json
import hashlib
import fcntl
import copy
import os
from pathlib import Path
import signal
import shutil
import stat
import time

import lifecycle_worker as worker


def private_request(path):
    descriptor = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    with os.fdopen(descriptor, "rb") as stream:
        metadata = os.fstat(stream.fileno())
        if (not stat.S_ISREG(metadata.st_mode) or metadata.st_uid != os.getuid()
                or metadata.st_mode & 0o077 or metadata.st_nlink != 1):
            raise worker.Refused("Lifecycle request must be a private same-user regular file")
        data = stream.read(worker.LIMIT + 1)
    if len(data) > worker.LIMIT:
        raise worker.Refused("Lifecycle request exceeds its size limit")
    return json.loads(data)


def atomic_record(path, value):
    path = Path(path)
    data = json.dumps(value).encode()
    if len(data) > worker.LIMIT:
        raise worker.Refused("Lifecycle recovery metadata exceeds its bounded size limit")
    temporary = path.with_name(path.name + ".tmp-" + str(os.getpid()))
    descriptor = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
    try:
        with os.fdopen(descriptor, "wb") as stream:
            stream.write(data)
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(temporary, path)
        descriptor = os.open(path.parent, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
        try:
            os.fsync(descriptor)
        finally:
            os.close(descriptor)
    finally:
        temporary.unlink(missing_ok=True)


def scope_fingerprint(tabs):
    values = []
    for tab in tabs:
        program = copy.deepcopy(tab["program"])
        program["argv"][0] = "captured-running-image"
        values.append(dict(program=program, record=worker.binding_identity(tab["record"]),
                           active=tab["active"], cwd=tab["cwd"], environment=tab.get("environment", {}),
                           return_shell={key: tab["return_shell"][key] for key in ("source", "image", "argv0")}
                               if "return_shell" in tab else None))
    return worker.signature(values)


class WindowUpdate:
    def __init__(self, request, system, directory, backend=None):
        self.request, self.system, self.directory = request, system, Path(directory)
        self.backend = backend
        self.descriptors = []
        self.interrupted = False
        self.released = False
        self.original_closed = False
        self.snapshot = None
        self.tabs = []

    def remote(self, action, value=None, timeout=45000):
        if self.backend is not None:
            return self.backend.operate(action, value or {})
        request = self.request
        reply = self.system.call(request["helper_owner"], "/org/xfce/Terminal/Update",
            "org.xfce.Terminal.Update1", "Operate", "(sss)",
            [request["token"], action, json.dumps(value or {})], timeout=timeout)
        result = json.loads(reply[0])
        if "error" in result:
            raise worker.Refused(result["error"])
        return result["ok"]

    def publish(self, phase, message, **values):
        status = dict(protocol=1, phase=phase, message=message, original_closed=self.original_closed,
                      backup=str(self.directory / "restart-plan.json"))
        for key in ("job_id", "window_uuid", "owner_pid"):
            if key in self.request: status["operation_id" if key == "job_id" else key] = self.request[key]
        if "external" in self.request:
            status["window_id"] = self.request["external"]["window"]["id"]
            status["owner_pid"] = self.request["external"]["window"]["pid"]
        status.update(values)
        atomic_record(self.directory / "status.json", status)
        self.remote("Publish", status)

    def event(self, phase, **details):
        worker.restart_event(self.directory, phase, **details)

    def pin(self, saved):
        return self.pin_image(f"/proc/{saved['pid']}/exe", saved["image"])

    def pin_image(self, source, image):
        descriptor = os.open(source, os.O_RDONLY | os.O_CLOEXEC)
        self.descriptors.append(descriptor)
        path = f"/proc/{os.getpid()}/fd/{descriptor}"
        if worker.file_identity(path) != image:
            raise worker.Refused("An executable changed while preparing its restart")
        return path

    def return_shell(self, ancestors):
        saved = next((process for process in ancestors if worker.kind(process) == "fish"), None)
        source = f"/proc/{saved['pid']}/exe" if saved else worker.latest_binary("fish")
        image = saved["image"] if saved else worker.file_identity(source)
        return dict(source=source, image=image, argv0=saved["arguments"][0] if saved else source,
                    executable=self.pin_image(source, image))

    def capture_tabs(self, snapshot):
        records = snapshot["records"]
        if not 0 < len(records) <= 32 or sum(bool(row["active"]) for row in records) != 1:
            raise worker.Refused("The terminal tab topology is ambiguous")
        tabs = []
        for record in records:
            root, active = worker.foreground(record["child_pid"], record["foreground_pgid"])
            pinned = self.pin(active)
            if worker.kind(active) == "tmux":
                server, session, name = worker.tmux_client(active)
                created = worker.run_tmux(server, "display-message", "-p", "-t", session, "#{session_created}").strip()
                program = dict(kind="tmux", process=active, installed=active["image"], cwd=active["cwd"],
                    argv=[pinned, "-N", "-S", server["socket"], "attach-session", "-t", session],
                    server=server, session=session, session_name=name, created=created)
            else:
                if worker.kind(active) == "pi":
                    raise worker.Refused("A non-tmux Pi restart cannot yet preserve the complete loaded runtime/extensions build; no application was stopped")
                program = worker.program_plan(active, executable=pinned)
                if program["kind"] == "codex" and worker.children(active["pid"]):
                    raise worker.Refused("Codex has running child processes; finish or cancel its tools before updating this terminal")
            worker.require_dedicated_tree(root, active, program)
            ancestors, cursor = [], active
            for _ in range(32):
                ancestors.append(cursor)
                if cursor["pid"] == root["pid"]:
                    break
                cursor = worker.process(cursor["parent"])
            else:
                raise worker.Refused("The application ancestry is not bounded")
            # execve's image path and argv[0] are distinct. In particular, shells
            # and multi-call binaries must not see a numeric /proc/fd name.
            arguments = [active["arguments"][0], *program["argv"][1:]]
            tab = dict(argv=arguments, executable=pinned, cwd=program["cwd"], active=record["active"],
                       program=program, record=record, ancestors=ancestors,
                       environment=worker.terminal_environment(active["pid"]))
            if program["kind"] == "codex":
                shell = tab["return_shell"] = self.return_shell(ancestors)
                tab["argv"] = worker.fish_command(pinned, arguments, shell["argv0"])
                tab["executable"] = shell["executable"]
            if isinstance(record.get("terminal_title"), str):
                tab["title"] = record["terminal_title"]
            elif program["kind"] in {"htop", "nvtop"}:
                try:
                    relative = Path(tab["cwd"]).relative_to(Path.home())
                    directory = "~" if relative == Path(".") else f"~/{relative}"
                except ValueError:
                    directory = tab["cwd"]
                tab["title"] = f"{program['kind']} {directory}"
            tabs.append(tab)
        return tabs

    def preserve_builds(self):
        cache = Path(os.environ.get("XDG_STATE_HOME", str(Path.home() / ".local/state"))) / "applicationlauncher/lifecycle/builds"
        cache.mkdir(mode=0o700, parents=True, exist_ok=True)
        metadata = cache.lstat()
        if not stat.S_ISDIR(metadata.st_mode) or metadata.st_uid != os.getuid() or metadata.st_mode & 0o077:
            raise worker.Refused("The lifecycle recovery build cache is not private")
        for tab in self.tabs:
            program = tab["program"]
            name = Path(program["process"]["executable"].removesuffix(" (deleted)")).name
            target, fingerprint = self.preserve_image(cache, program["argv"][0], name)
            arguments = [program["process"]["arguments"][0], *program["argv"][1:]]
            if "return_shell" in tab:
                shell = tab["return_shell"]
                shell_target, shell_hash = self.preserve_image(cache, shell["executable"], "fish")
                shell.update(recovery_executable=str(shell_target), recovery_build_sha256=shell_hash)
                tab["recovery_argv"] = worker.fish_command(str(target), arguments, str(shell_target))
            else:
                tab["recovery_argv"] = [str(target), *tab["argv"][1:]]
            tab["recovery_build_sha256"] = fingerprint
        for parent in (cache, cache.parent):
            descriptor = os.open(parent, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
            try: os.fsync(descriptor)
            finally: os.close(descriptor)

    def preserve_image(self, cache, source, name):
        if os.stat(source).st_size > 1024 * 1024 * 1024:
            raise worker.Refused("The executable exceeds the 1 GiB native recovery-build limit")
        with open(source, "rb") as stream:
            fingerprint = hashlib.file_digest(stream, "sha256").hexdigest()
        directory = cache / fingerprint
        directory.mkdir(mode=0o700, exist_ok=True)
        metadata = directory.lstat()
        if not stat.S_ISDIR(metadata.st_mode) or metadata.st_uid != os.getuid() or metadata.st_mode & 0o077:
            raise worker.Refused("A recovery build directory is not private")
        target = directory / name
        if not target.exists():
            temporary = directory / (".capture-" + self.request["job_id"])
            descriptor = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o700)
            try:
                with open(source, "rb") as source_file, os.fdopen(descriptor, "wb") as destination:
                    try:
                        # Linux FICLONE preserves bytes without coupling future writes.
                        fcntl.ioctl(destination.fileno(), 0x40049409, source_file.fileno())
                    except OSError:
                        shutil.copyfileobj(source_file, destination, 1024 * 1024)
                    destination.flush()
                    os.fsync(destination.fileno())
                temporary.chmod(0o500)
                os.link(temporary, target, follow_symlinks=False)
            finally:
                temporary.unlink(missing_ok=True)
        descriptor = os.open(target, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
        with os.fdopen(descriptor, "rb") as stream:
            metadata = os.fstat(stream.fileno())
            if (not stat.S_ISREG(metadata.st_mode) or metadata.st_uid != os.getuid()
                    or metadata.st_mode & 0o022 or metadata.st_size > 1024 * 1024 * 1024
                    or hashlib.file_digest(stream, "sha256").hexdigest() != fingerprint):
                raise worker.Refused("The recovery executable does not match the captured running build")
        descriptor = os.open(directory, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
        try: os.fsync(descriptor)
        finally: os.close(descriptor)
        return target, fingerprint

    def revalidate(self):
        self.remote("Preflight")
        for tab in self.tabs:
            saved = tab["program"]["process"]
            if not worker.same_process(saved):
                raise worker.Refused("An application changed after preview; nothing further was stopped")
            root, active = worker.foreground(tab["record"]["child_pid"], tab["record"]["foreground_pgid"])
            if (active["pid"], active["start"], active["image"], active["cwd"]) != (
                    saved["pid"], saved["start"], saved["image"], saved["cwd"]):
                raise worker.Refused("The foreground application changed after preview")
            if tab["program"]["kind"] == "tmux":
                self.verify_tmux(tab["program"], active)
            else:
                fresh = worker.program_plan(active, executable=tab["program"]["argv"][0])
                if worker.signature(fresh) != worker.signature(tab["program"]):
                    raise worker.Refused("The exact application session or restart options changed")
                if fresh["kind"] == "codex" and worker.children(active["pid"]):
                    raise worker.Refused("Codex started a tool after preview; nothing was stopped")
            worker.require_dedicated_tree(root, active, tab["program"])
            if "return_shell" in tab:
                shell = tab["return_shell"]
                if worker.file_identity(shell["source"]) != shell["image"]:
                    raise worker.Refused("The return-to-Fish executable changed after preview")

    def verify_tmux(self, program, active):
        server, session, _ = worker.tmux_client(active)
        original = program["server"]
        if (server["socket"], server["process"], session) != (original["socket"], original["process"], program["session"]):
            raise worker.Refused("The tmux client did not attach to the exact existing server/session")
        if worker.run_tmux(server, "display-message", "-p", "-t", session, "#{session_created}").strip() != program["created"]:
            raise worker.Refused("The tmux session identity changed")

    def stop_tab(self, tab, retire_tmux=False):
        program = tab["program"]
        if program["kind"] == "tmux":
            if not retire_tmux: return
            saved = program["process"]
            if not worker.same_process(saved): raise worker.Refused("The original tmux client changed before detachment")
            root, active = worker.foreground(tab["record"]["child_pid"], tab["record"]["foreground_pgid"])
            if (active["pid"], active["start"], active["image"], active["cwd"]) != (
                    saved["pid"], saved["start"], saved["image"], saved["cwd"]):
                raise worker.Refused("The original tab changed before tmux detachment")
            worker.require_dedicated_tree(root, active, program)
            self.verify_tmux(program, active)
        self.event("stopping_application", pid=program["process"]["pid"], application=program["kind"])
        self.interrupted = True
        worker.stop_program(program)
        # Stop only the captured, dedicated shell ancestry. An exec into an idle
        # shell after Codex exits is expected; a new job is not safe to terminate.
        for saved in tab["ancestors"]:
            try:
                current = worker.process(saved["pid"])
            except (OSError, ProcessLookupError):
                continue
            if current["start"] != saved["start"] or current["tty"] != saved["tty"]:
                raise worker.Refused("A captured process identity changed during graceful shutdown")
            allowed = worker.SHELLS | {"htop", "nvtop"} | ({"tmux"} if retire_tmux else set())
            if (worker.kind(current) not in allowed
                    or worker.children(current["pid"])):
                raise worker.Refused("A shell started another job; the original window was retained")
            descriptor = os.pidfd_open(current["pid"])
            try:
                if worker.process(current["pid"])["start"] != saved["start"]:
                    raise worker.Refused("Process identity changed before graceful shutdown")
                signal.pidfd_send_signal(descriptor, signal.SIGHUP)
            finally:
                os.close(descriptor)
            deadline = time.monotonic() + 5
            while time.monotonic() < deadline:
                try:
                    if worker.process(saved["pid"])["start"] != saved["start"]:
                        break
                except (OSError, ProcessLookupError):
                    break
                time.sleep(0.05)
            else:
                raise worker.Refused("The application/shell did not exit gracefully; no force-kill was attempted")

    def verify_replacement(self):
        result = self.remote("Records")
        records = result["records"]
        if len(records) != len(self.tabs):
            raise worker.Refused("The replacement tab topology changed")
        for tab, record in zip(self.tabs, records, strict=True):
            program = tab["program"]
            if program["kind"] == "tmux":
                root = worker.process(record["child_pid"])
                _, active = worker.foreground(root["pid"], root["foreground"])
                self.verify_tmux(program, active)
            else:
                worker.verify_program(program, record["child_pid"])
        self.event("applications_verified")

    def run(self):
        try:
            self.publish("preparing", "The lifecycle backend is preparing this exact terminal update")
            self.snapshot = self.remote("Capture")
            self.tabs = self.capture_tabs(self.snapshot)
            if (self.request.get("scope_fingerprint") is not None
                    and scope_fingerprint(self.tabs) != self.request["scope_fingerprint"]):
                raise worker.Refused("The exact application/session scope changed after preview; nothing was stopped")
            if (any(tab["program"]["kind"] != "tmux" for tab in self.tabs)
                    and not self.snapshot.get("capabilities", {}).get("hold") and self.backend is None):
                raise worker.Refused("This running terminal lacks the safe window-hold API; restart it manually once. No application was stopped")
            descriptions = [f"{tab['program']['kind']}: {tab['cwd']}" + (
                f"; conversation {tab['argv'][2]}" if tab["program"]["kind"] == "codex" else "") for tab in self.tabs]
            self.publish("ready", "\n".join(descriptions) + "\nOnly the terminal build is updated; application builds are preserved.",
                         preserves_processes=all(tab["program"]["kind"] == "tmux" for tab in self.tabs))
            self.remote("AwaitConfirmation", timeout=310000)
            with worker.operation_lock():
                self.revalidate()
                self.preserve_builds()
                atomic_record(self.directory / "restart-plan.json", dict(protocol=1, request=self.request,
                    snapshot=self.snapshot, tabs=self.tabs, scope="terminal-only; preserve running application builds"))
                self.event("confirmed")
                self.publish("running", "Staging the replacement terminal; original window remains open")
                self.remote("Stage", dict(tabs=[{key: tab[key] for key in ("argv", "executable", "cwd", "active", "title", "environment") if key in tab}
                                              for tab in self.tabs]))
                self.revalidate()
                if (any(tab["program"]["kind"] != "tmux" for tab in self.tabs)
                        and self.snapshot.get("capabilities", {}).get("hold")):
                    self.remote("Hold")
                for tab in self.tabs:
                    self.stop_tab(tab)
                self.event("applications_stopped")
                self.remote("Release")
                self.released = True
                self.verify_replacement()
                if self.backend is not None:
                    # Retire old clients only after their replacement attachment
                    # is verified. The server and its pane applications stay live.
                    for tab in self.tabs:
                        if tab["program"]["kind"] == "tmux": self.stop_tab(tab, retire_tmux=True)
                self.publish("closing", "Applications verified; handing over to the replacement window")
                committed = self.remote("Commit")
                self.original_closed = bool(committed.get("original_closed"))
                self.publish("complete", "Terminal updated in place; exact application sessions and running builds preserved", **committed)
                self.event("complete")
                return dict(ok=True, message="Terminal update completed and verified")
        except Exception as error:
            message = str(error)
            failure = worker.failure_details(error)
            recovery_failure = None
            self.event("failed", interrupted=self.interrupted, released=self.released, error=message, failure=failure)
            try:
                recovery = self.remote("Recover" if self.interrupted or self.released else "Discard")
                self.original_closed = bool(recovery.get("original_closed", self.original_closed))
            except Exception as recovery_error:
                recovery_failure = worker.failure_details(recovery_error)
            try:
                self.publish("failed", message + "; original window retained when possible; inspect recovery records before retrying")
            except Exception:
                pass
            return dict(ok=False, message=message, failure=failure, recovery_failure=recovery_failure)
        finally:
            for descriptor in self.descriptors:
                os.close(descriptor)
