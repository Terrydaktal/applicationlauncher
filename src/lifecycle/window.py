"""Daemon-owned window update coordinator. Terminal helpers contain no app adapters."""
import json
import copy
import os
from pathlib import Path
import signal
import stat
import time

import lifecycle_worker as worker
import lifecycle_timing as update_timing


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
    worker.atomic_record(path, value)


def scope_fingerprint(tabs):
    values = []
    for tab in tabs:
        program = copy.deepcopy(tab["program"])
        program["argv"][0] = "captured-running-image"
        values.append(dict(program=program, record=worker.binding_identity(tab["record"]),
                           active=tab["active"], cwd=tab["cwd"], environment=tab.get("environment", {}),
                           application_target=tab.get("application_target"),
                           return_shell={key: tab["return_shell"][key] for key in ("source", "image", "argv0")}
                               if "return_shell" in tab else None))
    return worker.plan_identity(values)


def codex_rollout_idle(stream, size, budget):
    header = stream.readline(min(budget, 1024 * 1024) + 1)
    if len(header) > min(budget, 1024 * 1024) or not header.endswith(b"\n"):
        raise ValueError("Incomplete rollout header")
    identity, parent = worker.rollout_session_identity(json.loads(header))
    start = max(len(header), size - min(budget - len(header), 1024 * 1024))
    if start > len(header):
        stream.seek(start - 1)
        partial = stream.read(1) != b"\n"
    else:
        stream.seek(start)
        partial = False
    data = stream.read(size - start)
    if data and not data.endswith(b"\n"):
        raise ValueError("Codex is still publishing its activity")
    lines = data.splitlines()
    if partial:
        lines = lines[1:]
    idle = True if start == len(header) else None
    for line in reversed(lines):
        row = json.loads(line)
        if not isinstance(row, dict):
            raise ValueError("Invalid activity record")
        payload = row.get("payload")
        if row.get("type") != "event_msg" or not isinstance(payload, dict):
            continue
        event = payload.get("type")
        if event in {"task_started", "turn_started", "user_message"}:
            idle = False
            break
        if event in {"task_complete", "turn_complete", "turn_aborted"}:
            idle = True
            break
    return identity, parent, idle, len(header) + len(data)


def require_idle_codex(program, proc_root=Path("/proc")):
    if program["kind"] != "codex":
        return
    saved = program["process"]
    unknown = "Codex activity could not be verified; retry when it is idle"
    try:
        if not worker.same_process(saved) or worker.session_id(saved) != program["argv"][2]:
            raise ValueError("Codex identity changed")
        deadline, budget, rollouts, stamps = time.monotonic() + 1, 8 * 1024 * 1024, {}, []
        def stamp(value):
            return value.st_dev, value.st_ino, value.st_size, value.st_mtime_ns
        for index, entry in enumerate((proc_root / str(saved["pid"]) / "fd").iterdir()):
            if index >= 512 or time.monotonic() > deadline:
                raise ValueError("Activity inspection exceeded its bounds")
            try:
                target = Path(os.readlink(entry).removesuffix(" (deleted)"))
                if not target.name.startswith("rollout-") or target.suffix != ".jsonl":
                    continue
                descriptor = os.open(entry, os.O_RDONLY | os.O_NONBLOCK)
            except FileNotFoundError:
                continue
            with os.fdopen(descriptor, "rb") as stream:
                before = os.fstat(stream.fileno())
                if (not stat.S_ISREG(before.st_mode) or before.st_uid != os.getuid()
                        or before.st_mode & 0o022 or budget <= 0 or len(rollouts) >= 64):
                    raise ValueError("Unsafe or excessive rollout metadata")
                identity, parent, idle, used = codex_rollout_idle(stream, before.st_size, budget)
                after = os.fstat(stream.fileno())
            if stamp(before) != stamp(after):
                raise ValueError("Activity changed during inspection")
            observed = (parent, idle, stamp(after))
            if identity in rollouts and rollouts[identity] != observed:
                raise ValueError("Conflicting rollout activity")
            rollouts[identity] = observed
            stamps.append((entry, stamp(after)))
            budget -= used
        root = program["argv"][2]
        if root not in rollouts or rollouts[root][0] is not None:
            raise ValueError("No process-bound main conversation")
        for identity, (parent, idle, _) in rollouts.items():
            seen, cursor = set(), identity
            while cursor != root:
                if cursor in seen or cursor not in rollouts:
                    raise ValueError("Incomplete child-agent lineage")
                seen.add(cursor)
                cursor = rollouts[cursor][0]
            if idle is None:
                raise ValueError("No bounded turn-state evidence")
        if (not worker.same_process(saved) or time.monotonic() > deadline
                or any(stamp(os.stat(entry)) != expected for entry, expected in stamps)):
            raise ValueError("Codex activity changed during inspection")
    except (OSError, ValueError, KeyError, TypeError) as error:
        raise worker.Refused(unknown) from error
    if any(not value[1] for value in rollouts.values()):
        raise worker.Refused("Codex is working on a turn or tool; finish or cancel it before updating this terminal")


class WindowUpdate:
    def __init__(self, request, system, directory, backend=None):
        self.request, self.system, self.directory = request, system, Path(directory)
        self.backend = backend
        self.owns_backend = False
        self.descriptors = []
        self.interrupted = False
        self.released = False
        self.original_closed = False
        self.snapshot = None
        self.tabs = []
        self.timings = update_timing.from_directory(self.directory)

    def remote(self, action, value=None, timeout=45000):
        if self.backend is not None:
            return self.backend.operate(action, value or {})
        return self.native_remote(action, value, timeout)

    def native_remote(self, action, value=None, timeout=45000):
        request = self.request
        reply = self.system.call(request["helper_owner"], "/org/xfce/Terminal/Update",
            "org.xfce.Terminal.Update1", "Operate", "(sss)",
            [request["token"], action, json.dumps(value or {})], timeout=timeout)
        result = json.loads(reply[0])
        if "error" in result:
            raise worker.Refused(result["error"])
        return result["ok"]

    def compatibility_backend(self):
        from lifecycle_external import MenuBackend
        return MenuBackend(self.request, self.system, self.directory, self.snapshot, self.native_remote)

    def publish(self, phase, message, **values):
        status = dict(protocol=1, phase=phase, message=message, original_closed=self.original_closed,
                      backup=str(self.directory / "restart-plan.json"))
        for key in ("job_id", "window_uuid", "owner_pid"):
            if key in self.request: status["operation_id" if key == "job_id" else key] = self.request[key]
        if "external" in self.request:
            status["window_id"] = self.request["external"]["window"]["id"]
            status["owner_pid"] = self.request["external"]["window"]["pid"]
        status.update(values)
        if self.timings is not update_timing.DISABLED:
            status["timing"] = self.timings.status()
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
        target, pinned = self.latest_image("fish")
        result = dict(target, argv0=saved["arguments"][0] if saved else target["source"], executable=pinned)
        if saved:
            result["original_executable"] = self.pin(saved)
        return result

    def latest_image(self, name):
        source = worker.latest_binary(name)
        image = worker.file_identity(source)
        return dict(name=name, source=source, image=image), self.pin_image(source, image)

    def require_target(self, target):
        if (worker.latest_binary(target["name"]) != target["source"]
                or worker.file_identity(target["source"]) != target["image"]):
            raise worker.Refused("An installed application changed after preview; nothing further was stopped")

    def capture_tabs(self, snapshot):
        records = snapshot["records"]
        if not 0 < len(records) <= 32 or sum(bool(row["active"]) for row in records) != 1:
            raise worker.Refused("The terminal tab topology is ambiguous")
        tabs = []
        for record in records:
            root, active = worker.foreground(record["child_pid"], record["foreground_pgid"])
            if worker.kind(active) == "pi":
                raise worker.Refused("A non-tmux Pi restart cannot yet preserve the complete loaded runtime/extensions build; no application was stopped")
            original = self.pin(active)
            target, pinned = self.latest_image(worker.kind(active))
            if worker.kind(active) == "tmux":
                worker.require_dedicated_tree(root, active)
                server, session, name = worker.tmux_client(active)
                if server["binary"] != target["source"] or server["installed"] != target["image"]:
                    raise worker.Refused("The installed tmux client changed during planning; no application was stopped")
                client = worker.client_connection(server, active["pid"])
                worker.require_client_server(dict(server=server, client=client), installed=True)
                if worker.client_connection(server, active["pid"], installed=True) != client:
                    raise worker.Refused("The tmux client changed during its compatibility check; no client was detached")
                program = dict(kind="tmux", process=active, installed=target["image"], cwd=active["cwd"],
                    argv=[pinned, *worker.client_attach_arguments(server, client)[1:]],
                    server=server, session=session, session_name=name, created=str(client["session_created"]), client=client)
            else:
                program = worker.program_plan(active, executable=pinned)
                require_idle_codex(program)
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
                       application_target=target, original_executable=original,
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

    def revalidate(self):
        self.remote("Preflight")
        for tab in self.tabs:
            if "application_target" in tab:
                self.require_target(tab["application_target"])
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
                if worker.plan_identity(fresh) != worker.plan_identity(tab["program"]):
                    raise worker.Refused("The exact application session or restart options changed")
                require_idle_codex(fresh)
            worker.require_dedicated_tree(root, active, tab["program"])
            if "return_shell" in tab:
                shell = tab["return_shell"]
                self.require_target(dict(shell, name="fish"))

    def verify_tmux(self, program, active):
        server, session, _ = worker.tmux_client(active)
        original = program["server"]
        if (server["socket"], server["process"], session) != (original["socket"], original["process"], program["session"]):
            raise worker.Refused("The tmux client did not attach to the exact existing server/session")
        if worker.run_tmux(server, "display-message", "-p", "-t", session, "#{session_created}").strip() != program["created"]:
            raise worker.Refused("The tmux session identity changed")
        if "client" in program:
            worker.require_client_server(program, installed=True)
            connection = worker.client_connection(server, active["pid"], installed=True)
            if connection != dict(program["client"], tty=connection["tty"]):
                raise worker.Refused("The tmux client attachment options changed")

    def stop_tab(self, tab, retire_tmux=False):
        program = tab["program"]
        require_idle_codex(program)
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
        # Keep dedicated return shells until the verified terminal handover.
        # Fish can be waiting for terminal replies with input held; signalling it
        # here can time out after Codex has already exited and destroy recovery.
        tab["retained_shells"] = []
        for saved in tab["ancestors"]:
            try:
                current = worker.process(saved["pid"])
            except (FileNotFoundError, ProcessLookupError):
                continue
            if current["start"] != saved["start"] or current["tty"] != saved["tty"]:
                raise worker.Refused("A captured process identity changed during graceful shutdown")
            allowed = worker.SHELLS | {"htop", "nvtop"} | ({"tmux"} if retire_tmux else set())
            returning = worker.kind(current) in worker.SHELLS and (
                saved["pid"] != program["process"]["pid"] or program["kind"] in {"codex", "pi", "tmux"})
            if returning:
                tab["retained_shells"].append(current)
                continue
            if worker.kind(current) not in allowed or worker.children(current["pid"]):
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
        self.verify_return_shells(tab)

    def verify_return_shells(self, tab):
        remaining = []
        for saved in tab.get("retained_shells", []):
            try:
                current = worker.process(saved["pid"])
            except (FileNotFoundError, ProcessLookupError):
                continue
            if (any(current[key] != saved[key] for key in ("start", "image", "tty", "parent", "group"))
                    or worker.kind(current) not in worker.SHELLS):
                raise worker.Refused("A return shell changed before terminal handover; the original was retained")
            remaining.append(current)
        for current in remaining:
            allowed = {other["pid"] for other in remaining if other["parent"] == current["pid"]}
            for child in set(worker.children(current["pid"])) - allowed:
                try:
                    worker.process(child)
                except (FileNotFoundError, ProcessLookupError):
                    continue  # The stopped application can await its shell's waitpid.
                raise worker.Refused(f"A return shell started another job (shell pid {current['pid']}, child pid {child}); the original window was retained")

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
                if active["image"] != program["installed"]:
                    raise worker.Refused("The replacement tmux client is not running the selected installed build")
                self.verify_tmux(program, active)
            else:
                worker.verify_program(program, record["child_pid"])
        self.event("applications_verified")

    def run(self):
        try:
            self.publish("preparing", "The lifecycle backend is preparing this exact terminal update")
            with self.timings.span("capture", route="compatibility" if self.backend else "native"):
                self.snapshot = self.remote("Capture")
            with self.timings.span("tab_plan"):
                self.tabs = self.capture_tabs(self.snapshot)
            if (self.backend is None and not self.snapshot.get("capabilities", {}).get("hold")
                    and any(tab["program"]["kind"] != "tmux" for tab in self.tabs)):
                original_scope = scope_fingerprint(self.tabs)
                with self.timings.span("fallback_select", route="compatibility"):
                    self.backend = self.compatibility_backend()
                    self.owns_backend = True
                    self.snapshot = self.remote("Capture")
                    self.tabs = self.capture_tabs(self.snapshot)
                if scope_fingerprint(self.tabs) != original_scope:
                    raise worker.Refused("The exact application/session scope changed during compatibility handover; nothing was stopped")
                self.event("compatibility_selected", reason="missing-window-hold-api")
            if (self.request.get("scope_fingerprint") is not None
                    and scope_fingerprint(self.tabs) != self.request["scope_fingerprint"]):
                raise worker.Refused("The exact application/session scope changed after preview; nothing was stopped")
            descriptions = [f"{tab['program']['kind']}: {tab['cwd']}" + (
                f"; conversation {tab['program']['argv'][2]}" if tab["program"]["kind"] == "codex" else "") for tab in self.tabs]
            self.publish("ready", "\n".join(descriptions) + "\nThe terminal and supported applications use the latest installed builds. tmux servers and pane applications stay running.",
                         preserves_processes=all(tab["program"]["kind"] == "tmux" for tab in self.tabs))
            with self.timings.span("confirmation_wait"):
                self.remote("AwaitConfirmation", timeout=310000)
            with worker.operation_lock():
                with self.timings.span("revalidate", attempt=1):
                    self.revalidate()
                with self.timings.span("restart_plan"):
                    atomic_record(self.directory / "restart-plan.json", dict(protocol=1, request=self.request,
                        snapshot=self.snapshot, tabs=self.tabs, scope="terminal and supported applications; preserve tmux servers and panes",
                        image_retention="Open descriptors during handover only; no executable backups"))
                self.event("confirmed")
                self.publish("running", "Staging the replacement terminal; original window remains open")
                with self.timings.span("stage_replacement"):
                    self.remote("Stage", dict(tabs=[{key: tab[key] for key in ("argv", "executable", "cwd", "active", "title", "environment") if key in tab}
                                                  for tab in self.tabs]))
                with self.timings.span("revalidate", attempt=2):
                    self.revalidate()
                if (any(tab["program"]["kind"] != "tmux" for tab in self.tabs)
                        and self.snapshot.get("capabilities", {}).get("hold")):
                    self.remote("Hold")
                for tab in self.tabs:
                    with self.timings.span("stop_application"):
                        self.stop_tab(tab)
                self.event("applications_stopped")
                with self.timings.span("release"):
                    self.remote("Release")
                self.released = True
                with self.timings.span("verify"):
                    self.verify_replacement()
                if self.backend is not None:
                    # Retire old clients only after their replacement attachment
                    # is verified. The server and its pane applications stay live.
                    for tab in self.tabs:
                        if tab["program"]["kind"] == "tmux": self.stop_tab(tab, retire_tmux=True)
                for tab in self.tabs:
                    self.verify_return_shells(tab)
                self.publish("closing", "Applications verified; handing over to the replacement window")
                with self.timings.span("commit"):
                    committed = self.remote("Commit")
                self.original_closed = bool(committed.get("original_closed"))
                self.publish("complete", "Terminal and applications updated in place; exact application sessions preserved", **committed)
                self.event("complete")
                return dict(ok=True, message="Terminal update completed and verified")
        except Exception as error:
            message = str(error)
            failure = worker.failure_details(error)
            recovery_failure = None
            self.event("failed", interrupted=self.interrupted, released=self.released, error=message, failure=failure)
            try:
                with self.timings.span("recover"):
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
            self.timings.close()
            try:
                if self.owns_backend:
                    self.backend.close()
            finally:
                for descriptor in self.descriptors:
                    os.close(descriptor)
