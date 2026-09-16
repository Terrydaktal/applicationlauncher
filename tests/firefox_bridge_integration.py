#!/usr/bin/env python3
"""Opt-in real Firefox bridge test in a disposable installation/profile and bus.

Requires APPLICATIONLAUNCHER_TEST_PULSEAUDIO and optionally
APPLICATIONLAUNCHER_TEST_PULSE_MODULES. Never connects to a running browser or
the desktop's audio server. Run under dbus-run-session.
"""
import contextlib
import importlib.util
import json
import os
import pathlib
import shutil
import subprocess
import tempfile
import time

ROOT = pathlib.Path(__file__).resolve().parents[1]


def until(check, timeout=15):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        value = check()
        if value:
            return value
        time.sleep(0.05)
    raise AssertionError("fixture condition timed out")


@contextlib.contextmanager
def child(command, env, log):
    with log.open("wb") as output:
        process = subprocess.Popen(command, env=env, stdin=subprocess.DEVNULL,
                                   stdout=output, stderr=subprocess.STDOUT)
        try:
            yield process
        finally:
            # Only the exact child this fixture created, never a process-name kill.
            if process.poll() is None:
                process.terminate()
                try:
                    process.wait(timeout=8)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait(timeout=3)


def run():
    pulse = os.environ["APPLICATIONLAUNCHER_TEST_PULSEAUDIO"]
    spec = importlib.util.spec_from_file_location("installer", ROOT / "scripts/install-firefox-audio-bridge.py")
    installer = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(installer)
    with tempfile.TemporaryDirectory(prefix="al-firefox-integration-") as temp:
        root = pathlib.Path(temp)
        stage = root / "firefox"
        stage.mkdir()
        # A disposable runtime/configuration; remaining immutable libraries stay
        # symlinked. The real installation and user profile are never edited.
        for source in pathlib.Path("/usr/lib/firefox").iterdir():
            target = stage / source.name
            if source.name == "defaults":
                shutil.copytree(source, target)
            elif source.name in ("firefox", "application.ini", "libxul.so") or source.suffix == ".cfg":
                shutil.copy2(source, target)
            elif source.name != installer.BRIDGE_NAME:
                target.symlink_to(source)
        config = installer.install(stage, ROOT / "firefox/audio-bridge.js")
        driver_name = "applicationlauncher-audio-test-driver.js"
        shutil.copyfile(ROOT / "tests/firefox_audio_test_driver.js", stage / driver_name)
        with config.open("a") as fixture:
            fixture.write('pref("browser.dom.window.dump.enabled", true);\n'
                          'var alTestConsole = Components.classes["@mozilla.org/consoleservice;1"].getService(Components.interfaces.nsIConsoleService);\n'
                          'alTestConsole.registerListener({observe(message) { dump("Fixture console: " + message.message + "\\n"); }});\n'
                          'for (const message of alTestConsole.getMessageArray()) dump("Fixture startup: " + message.message + "\\n");\n')
            fixture.write(installer.LOADER.replace(installer.BRIDGE_NAME, driver_name))
        profile = root / "profile"
        runtime = root / "runtime"
        home = root / "home"
        for directory in (profile, runtime, home):
            directory.mkdir(mode=0o700)
        (profile / "user.js").write_text(
            'user_pref("media.autoplay.default", 0);\n'
            'user_pref("media.autoplay.blocking_policy", 0);\n'
            'user_pref("media.autoplay.block-webaudio", false);\n'
            'user_pref("media.block-autoplay-until-in-foreground", false);\n'
            'user_pref("app.normandy.enabled", false);\n'
            'user_pref("app.shield.optoutstudies.enabled", false);\n'
            'user_pref("browser.dom.window.dump.enabled", true);\n'
            'user_pref("browser.shell.checkDefaultBrowser", false);\n'
            'user_pref("browser.tabs.warnOnClose", false);\n'
            'user_pref("datareporting.policy.dataSubmissionEnabled", false);\n'
        )
        env = {**os.environ, "HOME": str(home), "XDG_RUNTIME_DIR": str(runtime),
               "XDG_CONFIG_HOME": str(root / "config"), "XDG_CACHE_HOME": str(root / "cache"),
               "PULSE_SERVER": "unix:" + str(root / "pulse"), "PULSE_STATE_PATH": str(root / "pulse-state"),
               "PULSE_RUNTIME_PATH": str(root / "pulse-runtime"), "MOZ_HEADLESS": "1", "MOZ_ENABLE_WAYLAND": "0",
               "APPLICATIONLAUNCHER_FIREFOX_TEST_ROOT": str(root), "GTK_USE_PORTAL": "0", "MOZ_LOG": "MCD:5"}
        command = [pulse, "-n", "--daemonize=no", "--use-pid-file=no", "--exit-idle-time=-1",
                   "--disable-shm=yes", "--log-target=stderr", "--log-level=error",
                   f"--load=module-native-protocol-unix socket={root / 'pulse'} auth-anonymous=1",
                   "--load=module-null-sink sink_name=fixture_output rate=48000 channels=2"]
        if modules := os.environ.get("APPLICATIONLAUNCHER_TEST_PULSE_MODULES"):
            command.append("--dl-search-path=" + modules)
        try:
            with child(command, env, root / "pulse.log"):
                until(lambda: (root / "pulse").exists())
                with child([str(stage / "firefox"), "--headless", "--no-remote", "--profile", str(profile),
                            "about:blank"], env, root / "firefox.log") as browser:
                    def snapshot():
                        assert browser.poll() is None, "fixture Firefox exited"
                        files = list((runtime / "applicationlauncher-firefox-audio").glob("firefox-*.json"))
                        return json.loads(files[0].read_text()) if files else None
                    def command(value):
                        (root / "command.tmp").write_text(value)
                        (root / "command.tmp").replace(root / "command")
                    first = until(lambda: (s := snapshot()) and len(s["windows"]) == 1 and s)
                    assert first["complete"] and first["pid"] == browser.pid
                    assert len(first["windows"]) == 1 and not first["windows"][0]["audible"]
                    command("start")
                    playing = until(lambda: (s := snapshot()) and any(w["audible"] for w in s["windows"]) and s)
                    assert "Fixture Music" in playing["windows"][0]["audible"]
                    command("background")
                    background = until(lambda: (s := snapshot()) and s["windows"][0]["title"] != playing["windows"][0]["title"] and s)
                    assert "Fixture Music" in background["windows"][0]["audible"]
                    command("second-window")
                    two = until(lambda: (s := snapshot()) and len(s["windows"]) == 2 and s)
                    assert sum(bool(w["audible"]) for w in two["windows"]) == 1
                    command("mute")
                    until(lambda: (s := snapshot()) and not any(w["audible"] for w in s["windows"]))
                    command("unmute")
                    until(lambda: (s := snapshot()) and sum(bool(w["audible"]) for w in s["windows"]) == 1)
                    command("stop")
                    until(lambda: (s := snapshot()) and not any(w["audible"] for w in s["windows"]))
                    assert (runtime / "applicationlauncher-firefox-audio").stat().st_mode & 0o077 == 0
                    for path in (runtime / "applicationlauncher-firefox-audio").glob("*.json"):
                        assert path.stat().st_mode & 0o077 == 0
                    print("PASS: real AutoConfig startup, private snapshot, live playback, background tab, two windows, mute/unmute and stop")
        except Exception:
            if (profile / "compatibility.ini").exists():
                print((profile / "compatibility.ini").read_text())
            state = runtime / "applicationlauncher-firefox-audio"
            print("Bridge snapshot files:", [p.name for p in state.glob("*")])
            for path in state.glob("*.json"):
                print(path.name, path.read_text()[:2000])
            for name in ("firefox.log", "pulse.log"):
                path = root / name
                if path.exists():
                    print(name + ":\n" + path.read_text(errors="replace")[-10000:])
            raise


if __name__ == "__main__":
    run()
