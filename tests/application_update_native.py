"""Real restarts on a private bus and Xvfb, never the user's desktop.

Usage: uv run --no-project --python /usr/bin/python3 python3 tests/application_update_native.py /path/to/xfce4-terminal
"""
import importlib.util
import difflib
import json
import os
from pathlib import Path
import shlex
import shutil
import signal
import subprocess
import sys
import tempfile
import time
from unittest.mock import patch


def wait_for(callback, seconds=12):
    deadline = time.monotonic() + seconds
    last = None
    while time.monotonic() < deadline:
        try:
            value = callback()
            if value:
                return value
        except Exception as error:
            last = error
        time.sleep(0.05)
    raise AssertionError(f"Fixture condition did not become true: {last}")


def session(binary):
    source = Path(__file__).resolve().parents[1]
    spec = importlib.util.spec_from_file_location("worker", source / "src/lifecycle/worker.py")
    worker = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(worker)
    system = worker.System()
    root = Path(os.environ["APPLICATION_UPDATE_TEST_ROOT"])
    owned = []
    compositor = None
    tmux = worker.latest_binary("tmux")
    socket = str(root / "tmux.socket")

    def run_plan(plan, directory):
        path = directory / "restart-plan.json"
        path.write_text(json.dumps(plan))
        path.chmod(0o600)
        with open(path, "rb") as stream:
            os.fsync(stream.fileno())
        result = subprocess.run([sys.executable, "-I", "-c", (source / "src/lifecycle/worker.py").read_text(), "run", str(path)],
            stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=60)
        if result.returncode:
            fresh = worker.inspect(plan["input"], system)
            current = next((value for value in fresh["plans"].values() if value["kind"] == plan["kind"]), fresh)
            print("Preview change diagnostics:\n" + "\n".join(difflib.unified_diff(
                json.dumps(plan, sort_keys=True, indent=2).splitlines(),
                json.dumps(current, sort_keys=True, indent=2).splitlines(),
                fromfile="preview", tofile="after refusal")), file=sys.stderr)
        assert result.returncode == 0, (result.stdout.decode(), result.stderr.decode())
        report = json.loads(result.stdout)
        if not report.get("ok", False):
            print(result.stderr.decode(errors="replace"), file=sys.stderr)
        return report

    def command(*arguments):
        return subprocess.check_output(arguments, timeout=5).decode().strip()

    def launch(argv):
        log = open(root / f"terminal-{len(owned)}.log", "wb")
        child = subprocess.Popen([str(binary), "--disable-server", "--working-directory", str(root), "--command", shlex.join(argv)], stdout=log, stderr=log)
        log.close()
        owned.append(child)
        return child

    def record(child):
        values = [value for value in system.terminals() if value["owner_pid"] == child.pid and value["child_pid"] > 1]
        return values[0] if len(values) == 1 else None

    def window(child):
        values = (command("kdotool", "search", "--all", "--pid", str(child.pid), "--class", "^xfce4-terminal$") if "--wayland" in sys.argv
                  else command("xdotool", "search", "--onlyvisible", "--pid", str(child.pid))).splitlines()
        assert len(values) == 1, values
        return values[0]

    def geometry(identifier):
        if "--wayland" in sys.argv:
            result = system.call("org.kde.KWin", "/KWin", "org.kde.KWin", "getWindowInfo", "(s)", [identifier])[0]
            return {key:result[key] for key in ("x", "y", "width", "height")}
        return command("xdotool", "getwindowgeometry", "--shell", identifier)

    def position(identifier, x, y, width, height):
        tool = "kdotool" if "--wayland" in sys.argv else "xdotool"
        command(tool, "windowmove", identifier, str(x), str(y), "windowsize", identifier, str(width), str(height))
        if "--wayland" in sys.argv:
            expected = dict(x=x, y=y, width=width, height=height)
            wait_for(lambda: geometry(identifier) == expected)

    def payload(child, selected):
        return dict(window_id="fixture-window", windows=[dict(id="fixture-window", pid=child.pid,
            title=selected["window_title"], **{"class":"xfce4-terminal"})])

    def program_payload(child, selected, application):
        result = payload(child, selected)
        _, active = worker.foreground(selected["child_pid"], selected["foreground_pgid"])
        if worker.kind(active) == "tmux" and application != "tmux":
            server, session, _ = worker.tmux_client(active)
            pid = int(worker.run_tmux(server, "display-message", "-p", "-t", session, "#{pane_pid}").strip())
            root_process = worker.process(pid)
            _, active = worker.foreground(pid, root_process["foreground"])
        result["process"] = dict(pid=active["pid"], start_ticks=active["start"], application=application)
        return result

    try:
        if "--wayland" in sys.argv:
            log = open(root / "kwin.log", "wb")
            compositor = subprocess.Popen(["kwin_wayland", "--virtual", "--width", "1280", "--height", "900", "--no-lockscreen", "--no-global-shortcuts", "--no-kactivities", "--socket", "launcher-update-test"], stdout=log, stderr=log)
            log.close()
            wait_for(lambda: system.dbus("NameHasOwner", "(s)", ["org.kde.KWin"])[0])
        assert not system.dbus("NameHasOwner", "(s)", ["com.terrydaktal.ApplicationLauncher"])[0]
        child = launch([worker.latest_binary("fish"), "--no-config"])
        before = wait_for(lambda: record(child))
        identifier = wait_for(lambda: window(child))
        position(identifier, 210, 170, 800, 560)
        saved_geometry = geometry(identifier)
        token = system.prepare(before, [worker.latest_binary("fish"), "--no-config"], str(root))
        assert worker.same_process(worker.process(before["child_pid"]))
        assert system.tab(before, token, "GetTabRestartStatus")["phase"] == "prepared"
        try:
            system.tab(before, "wrong-token", "CommitTabRestart")
        except system.GLib.Error:
            pass
        else:
            raise AssertionError("A wrong restart token was accepted")
        assert record(child)["child_pid"] == before["child_pid"]
        system.tab(before, token, "CancelTabRestart")
        wait_for(lambda: record(child)["input_enabled"])
        assert record(child)["child_pid"] == before["child_pid"]
        preview = worker.inspect(program_payload(child, before, "fish"), system)
        assert preview["actions"][0]["eligible"], preview
        plan = preview["plans"]["program"]
        backup = root / "fish-backup"
        backup.mkdir(mode=0o700)
        (backup / "restart-plan.json").write_text(json.dumps(plan))
        result = run_plan(plan, backup)
        assert result["ok"], result
        after = wait_for(lambda: record(child))
        assert before["child_pid"] != after["child_pid"]
        assert before["window_uuid"] == after["window_uuid"] and before["tab_uuid"] == after["tab_uuid"]
        assert geometry(identifier) == saved_geometry, (saved_geometry, geometry(identifier))
        print("PASS native fish restart/cancel preserve window/tab identity and exact geometry", flush=True)

        codex_id = "12345678-1234-5678-9abc-123456789abc"
        bypass = "--dangerously-bypass-approvals-and-sandbox"
        codex = launch([worker.latest_binary("codex"), "resume", codex_id, bypass])
        codex_record = wait_for(lambda: record(codex))
        codex_window = wait_for(lambda: window(codex))
        position(codex_window, 300, 210, 760, 540)
        codex_geometry = geometry(codex_window)
        preview = worker.inspect(program_payload(codex, codex_record, "codex"), system)
        plan = preview["plans"]["program"]
        codex_backup = root / "codex-backup"
        codex_backup.mkdir(mode=0o700)
        (codex_backup / "restart-plan.json").write_text(json.dumps(plan))
        result = run_plan(plan, codex_backup)
        assert result["ok"], result
        def resumed_codex():
            binding = record(codex)
            _, active = worker.foreground(binding["child_pid"], binding["foreground_pgid"])
            return active if worker.kind(active) == "codex" else None
        resumed = wait_for(resumed_codex)
        assert resumed["pid"] != codex_record["child_pid"]
        assert resumed["arguments"][1:] == ["resume", codex_id, bypass]
        assert geometry(codex_window) == codex_geometry, (codex_geometry, geometry(codex_window))
        print("PASS native Codex fixture resumes the exact session and permissions in the same window", flush=True)

        for name in ("one", "two"):
            program = [worker.latest_binary("codex"), "resume", codex_id, bypass] if name == "one" else [worker.latest_binary("fish"), "--no-config"]
            command(tmux, "-S", socket, "-f", "/dev/null", "new-session", "-d", "-s", name,
                    *program)
        first = launch([tmux, "-N", "-S", socket, "attach-session", "-t", "one"])
        second = launch([tmux, "-N", "-S", socket, "attach-session", "-t", "two"])
        first_record = wait_for(lambda: record(first))
        second_record = wait_for(lambda: record(second))
        first_window, second_window = wait_for(lambda: window(first)), wait_for(lambda: window(second))
        position(first_window, 115, 230, 730, 480)
        position(second_window, 370, 120, 880, 610)
        expected_geometry = [geometry(first_window), geometry(second_window)]
        def pane_preview():
            value = worker.inspect(program_payload(first, record(first), "codex"), system)
            return value if "program" in value["plans"] else None
        preview = wait_for(pane_preview)
        pane_plan = preview["plans"]["program"]
        server_pid = pane_plan["server"]["process"]["pid"]
        other_pane_pid = command(tmux, "-N", "-S", socket, "display-message", "-p", "-t", "two", "#{pane_pid}")
        pane_backup = root / "pane-backup"
        pane_backup.mkdir(mode=0o700)
        result = run_plan(pane_plan, pane_backup)
        assert result["ok"], result
        assert worker.same_process(pane_plan["server"]["process"])
        assert int(command(tmux, "-N", "-S", socket, "display-message", "-p", "#{pid}")) == server_pid
        assert command(tmux, "-N", "-S", socket, "display-message", "-p", "-t", "two", "#{pane_pid}") == other_pane_pid
        assert not worker.same_process(pane_plan["program"]["process"])
        for child, before, identifier, expected in zip((first, second), (first_record, second_record),
                (first_window, second_window), expected_geometry):
            after = record(child)
            assert after["child_pid"] == before["child_pid"] and after["tab_uuid"] == before["tab_uuid"]
            assert geometry(identifier) == expected
        print("PASS selected tmux Codex pane restart leaves the server, both clients and the other pane untouched", flush=True)
        fish_record = record(second)
        fish_preview = worker.inspect(program_payload(second, fish_record, "fish"), system)
        assert [action["id"] for action in fish_preview["actions"]] == ["program"], fish_preview
        fish_plan = fish_preview["plans"]["program"]
        unchanged_codex = command(tmux, "-N", "-S", socket, "display-message", "-p", "-t", "one", "#{pane_pid}")
        fish_backup = root / "tmux-fish-only-backup"
        fish_backup.mkdir(mode=0o700)
        assert run_plan(fish_plan, fish_backup)["ok"]
        assert command(tmux, "-N", "-S", socket, "display-message", "-p", "-t", "one", "#{pane_pid}") == unchanged_codex
        assert int(command(tmux, "-N", "-S", socket, "display-message", "-p", "#{pid}")) == server_pid
        assert record(first)["child_pid"] == first_record["child_pid"] and record(second)["child_pid"] == second_record["child_pid"]
        assert geometry(first_window) == expected_geometry[0] and geometry(second_window) == expected_geometry[1]
        print("PASS selected tmux Fish update leaves Codex, the server and both exact native windows untouched", flush=True)
        for name, shell in (("direct", None), ("fish", worker.latest_binary("fish")), ("bash", "/usr/bin/bash")):
            argv = [tmux, "-N", "-S", socket, "attach-session", "-E", "-t", "one"]
            launch_argv = argv
            if shell:
                launch_argv = (worker.fish_command(tmux, argv, shell) if name == "fish"
                               else [shell, "--noprofile", "--norc", "-ic", shlex.join(argv) + "; exec /proc/self/exe -i"])
            client = launch(launch_argv)
            before = wait_for(lambda: record(client))
            identifier = wait_for(lambda: window(client))
            position(identifier, 205, 150, 760, 510)
            expected = geometry(identifier)
            def client_preview():
                value = worker.inspect(program_payload(client, record(client), "tmux"), system)
                return value if "program" in value["plans"] else None
            plan = wait_for(client_preview)["plans"]["program"]
            original_client = plan["program"]["process"]
            pane_processes = command(tmux, "-N", "-S", socket, "list-panes", "-a", "-F", "#{pane_id}:#{pane_pid}")
            backup = root / f"tmux-client-{name}"
            backup.mkdir(mode=0o700)
            result = run_plan(plan, backup)
            if not result["ok"]:
                current = record(client)
                print("Client handover failure:", name, result, current, flush=True)
                if current and current["child_pid"] > 1:
                    print("Remaining root:", worker.process(current["child_pid"]), flush=True)
            assert result["ok"], result
            after = record(client)
            assert before["window_uuid"] == after["window_uuid"] and before["tab_uuid"] == after["tab_uuid"]
            assert after["input_enabled"] and geometry(identifier) == expected
            assert worker.same_process(plan["server"]["process"]) and not worker.same_process(original_client)
            assert command(tmux, "-N", "-S", socket, "list-panes", "-a", "-F", "#{pane_id}:#{pane_pid}") == pane_processes
            assert record(first)["child_pid"] == first_record["child_pid"] and record(second)["child_pid"] == second_record["child_pid"]
            if shell:
                new_shell = worker.process(after["child_pid"])
                assert new_shell["image"] == plan["root"]["image"], "Client update silently upgraded the return shell"
                assert Path(f"/proc/{new_shell['pid']}/comm").read_text().strip() == name, "Pinned shell lost its process name"
                _, active = worker.foreground(new_shell["pid"], new_shell["foreground"])
                connection = worker.client_connection(plan["server"], active["pid"])
                worker.run_tmux(plan["server"], "detach-client", "-t", connection["tty"])
                wait_for(lambda: record(client)["child_pid"] == new_shell["pid"] and not worker.children(new_shell["pid"]))
                assert geometry(identifier) == expected
                assert Path(f"/proc/{new_shell['pid']}/comm").read_text().strip() == name
            client.terminate()
            client.wait(timeout=5)
        print("PASS tmux client-only updates preserve native window/tab, geometry, server, panes, other clients and return-to-shell behavior", flush=True)
        previous, changed_at = None, time.monotonic()
        def server_preview():
            nonlocal previous, changed_at
            value = worker.inspect(payload(first, first_record), system)
            plan = value["plans"].get("tmux_server")
            if not plan:
                return None
            current = worker.signature(plan)
            if previous != current:
                previous, changed_at = current, time.monotonic()
                return None
            return value if time.monotonic() - changed_at > 0.15 else None
        preview = wait_for(server_preview)
        plan = preview["plans"]["tmux_server"]
        old_server = plan["server"]["process"]["pid"]
        assert len(plan["bindings"]) == 2
        action = next(action for action in preview["actions"] if action["id"] == "tmux_server")
        scope = action["tmux_server"]
        assert action["eligible"] and scope["identity"] == dict(pid=old_server, start_ticks=plan["server"]["process"]["start"], socket_path=socket)
        assert set(scope["sessions"]) == {"one", "two"}
        assert scope["terminal_tabs"] == 2 and {pane["application"] for pane in scope["panes"]} == {"codex", "fish"}, scope
        backup = root / "tmux-backup"
        backup.mkdir(mode=0o700)
        (backup / "restart-plan.json").write_text(json.dumps(plan))
        result = run_plan(plan, backup)
        assert result["ok"], result
        assert int(command(tmux, "-N", "-S", socket, "display-message", "-p", "#{pid}")) != old_server
        for child, before, identifier, expected, name in zip((first, second), (first_record, second_record),
                (first_window, second_window), expected_geometry, ("one", "two")):
            after = wait_for(lambda: record(child))
            assert before["child_pid"] != after["child_pid"]
            assert before["window_uuid"] == after["window_uuid"] and before["tab_uuid"] == after["tab_uuid"]
            assert geometry(identifier) == expected, (expected, geometry(identifier))
            clients = command(tmux, "-N", "-S", socket, "list-clients", "-F", "#{client_pid}:#{session_name}").splitlines()
            assert f"{after['child_pid']}:{name}" in clients
        print("PASS native tmux server replacement preserves both windows, exact geometry, tab identity and session bindings", flush=True)
        def pane_codex():
            pane = command(tmux, "-N", "-S", socket, "display-message", "-p", "-t", "one", "#{pane_id}")
            pid = int(command(tmux, "-N", "-S", socket, "display-message", "-p", "-t", pane, "#{pane_pid}"))
            current = worker.process(pid)
            _, active = worker.foreground(pid, current["foreground"])
            return active if worker.kind(active) == "codex" else None
        assert wait_for(pane_codex)["arguments"][1:] == ["resume", codex_id, bypass]
        assert worker.same_process(resumed), "Restarting the tmux server touched an unrelated native Codex fixture"
        first_record, second_record = record(first), record(second)
        plan = wait_for(server_preview)["plans"]["tmux_server"]
        recovery = root / "tmux-startup-failure-backup"
        recovery.mkdir(mode=0o700)
        (recovery / "restart-plan.json").write_text(json.dumps(plan))
        (recovery / "restart-plan.json").chmod(0o600)
        original = worker.run_tmux
        failed = []
        def crash_replacement_before_startup_acknowledgement(server, *arguments, **options):
            if (server["socket"] == socket and arguments[0] == "new-session" and options.get("create")
                    and not failed and not worker.same_process(plan["server"]["process"])):
                failed.append(True)
                original(server, *arguments, **options)
                stopped = worker.replacement_server(server)
                original(stopped, "kill-server")
                wait_for(lambda: not worker.same_process(stopped["process"]))
                raise worker.Refused("tmux new-session failed: server exited unexpectedly")
            return original(server, *arguments, **options)
        with patch.object(worker, "run_tmux", side_effect=crash_replacement_before_startup_acknowledgement):
            result = worker.execute(plan, system, recovery)
        assert len(failed) == 1 and result["ok"], result
        clients = command(tmux, "-N", "-S", socket, "list-clients", "-F", "#{client_pid}:#{session_name}").splitlines()
        for child, before, identifier, expected, name in zip((first, second), (first_record, second_record),
                (first_window, second_window), expected_geometry, ("one", "two")):
            after = record(child)
            assert before["tab_uuid"] == after["tab_uuid"] and before["window_uuid"] == after["window_uuid"]
            assert after["input_enabled"] and geometry(identifier) == expected
            assert f"{after['child_pid']}:{name}" in clients
        assert wait_for(pane_codex)["arguments"][1:] == ["resume", codex_id, bypass]
        assert worker.same_process(resumed), "Startup recovery touched an unrelated Codex process"
        print("PASS replacement startup exit recovers exact conversations in both original native windows without duplicate clients", flush=True)
        first_record, second_record = record(first), record(second)
        plan = wait_for(server_preview)["plans"]["tmux_server"]
        failed_backup = root / "tmux-permanent-startup-failure"
        failed_backup.mkdir(mode=0o700)
        failures = []
        def refuse_every_replacement_start(server, *arguments, **options):
            if server["socket"] == socket and arguments[0] == "new-session" and options.get("create"):
                failures.append(True)
                assert not worker.same_process(plan["server"]["process"])
                raise worker.Refused("tmux new-session failed: server exited unexpectedly")
            return original(server, *arguments, **options)
        with patch.object(worker, "run_tmux", side_effect=refuse_every_replacement_start):
            result = worker.execute(plan, system, failed_backup)
        assert len(failures) == 3 and not result["ok"], result
        remaining = {value["tab_uuid"]: value for value in system.terminals()}
        for before, identifier, expected in zip((first_record, second_record), (first_window, second_window), expected_geometry):
            after = remaining[before["tab_uuid"]]
            assert after["window_uuid"] == before["window_uuid"] and after["input_enabled"]
            assert geometry(identifier) == expected
        assert worker.same_process(resumed), "Failed startup cleanup touched unrelated work"
        print("PASS persistent post-shutdown startup failure preserves both original windows and releases native tab input holds", flush=True)
        kept_tab = record(codex)
        os.kill(resumed["pid"], signal.SIGUSR1)
        def idle_codex_shell():
            binding = record(codex)
            shell = worker.process(binding["child_pid"])
            return (binding["child_pid"] == kept_tab["child_pid"] and binding["tab_uuid"] == kept_tab["tab_uuid"]
                    and worker.kind(shell) == "fish" and not worker.children(shell["pid"]))
        wait_for(idle_codex_shell)
        assert geometry(codex_window) == codex_geometry
        print("PASS exiting individually updated Codex retains idle Fish in the same native window/tab", flush=True)
    finally:
        subprocess.run([tmux, "-N", "-S", socket, "kill-server"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=5)
        for child in owned:
            if child.poll() is None:
                child.terminate()
            try:
                child.wait(timeout=5)
            except subprocess.TimeoutExpired:
                child.kill()
                child.wait(timeout=5)
        if compositor is not None:
            compositor.terminate()
            compositor.wait(timeout=10)


def main():
    binary = Path(sys.argv[-1]).resolve()
    if "--session" in sys.argv:
        session(binary)
        return
    for command in ("Xvfb", "xvfb-run", "xdotool", "tmux", "dbus-run-session", "cc"):
        if not shutil.which(command):
            raise SystemExit(f"Required isolated test tool is unavailable: {command}")
    with tempfile.TemporaryDirectory(prefix="launcher-native-update-") as directory:
        root = Path(directory)
        front = root / "bin"
        front.mkdir()
        subprocess.run(["cc", "-O2", str(Path(__file__).parent / "fixtures/application_update_codex.c"), "-o", str(front / "codex")], check=True, timeout=15)
        environment = os.environ.copy()
        for key in ("DBUS_SESSION_BUS_ADDRESS", "DISPLAY", "WAYLAND_DISPLAY", "TMUX", "TMUX_PANE", "XDG_ACTIVATION_TOKEN", "DESKTOP_STARTUP_ID", "SESSION_MANAGER"):
            environment.pop(key, None)
        for key, name in (("HOME", "home"), ("XDG_RUNTIME_DIR", "runtime"), ("XDG_CONFIG_HOME", "config"),
                          ("XDG_DATA_HOME", "data"), ("XDG_STATE_HOME", "state"), ("XDG_CACHE_HOME", "cache")):
            path = root / name
            path.mkdir(mode=0o700)
            environment[key] = str(path)
        environment.update(APPLICATION_UPDATE_TEST_ROOT=str(root), GDK_BACKEND="wayland" if "--wayland" in sys.argv else "x11", GIO_USE_VFS="local", NO_AT_BRIDGE="1", LIBGL_ALWAYS_SOFTWARE="1")
        environment["PATH"] = str(front) + ":" + environment["PATH"]
        arguments = ["dbus-run-session", "--"]
        if "--wayland" in sys.argv:
            environment.update(QT_QPA_PLATFORM="offscreen", WAYLAND_DISPLAY="launcher-update-test", KWIN_COMPOSE="Q",
                __EGL_VENDOR_LIBRARY_FILENAMES="/usr/share/glvnd/egl_vendor.d/50_mesa.json")
        else:
            arguments.extend(["xvfb-run", "-a"])
        arguments.extend([sys.executable, __file__, "--session", *(["--wayland"] if "--wayland" in sys.argv else []), str(binary)])
        result = subprocess.run(arguments, env=environment, timeout=90)
        if result.returncode:
            for path in root.glob("terminal-*.log"):
                print(path.name + ": " + path.read_text(errors="replace")[-5000:], file=sys.stderr)
            if (root / "kwin.log").exists():
                print((root / "kwin.log").read_text(errors="replace")[-5000:], file=sys.stderr)
        raise SystemExit(result.returncode)


if __name__ == "__main__":
    main()
