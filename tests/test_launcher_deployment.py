"""Isolated checks for the installed wrapper; never use the desktop service."""

import os
from pathlib import Path
import shutil
import signal
import subprocess
import tempfile
import time
import unittest


REPO = Path(__file__).resolve().parents[1]


class LauncherDeploymentTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory(prefix="launcher-deployment-")
        self.root = Path(self.directory.name)
        self.repo = self.root / "applicationlauncher"
        self.release = self.repo / "target/release"
        self.runtime = self.root / "runtime"
        self.commands = self.root / "commands"
        self.home = self.root / "home"
        for directory in (self.repo / "scripts/lib", self.release, self.runtime,
                          self.commands, self.home / ".local/bin"):
            directory.mkdir(parents=True)
        shutil.copy2(REPO / "scripts/applicationlauncher", self.repo / "scripts/applicationlauncher")
        helper = REPO / "scripts/lib/release-compatibility.sh"
        if helper.exists():
            shutil.copy2(helper, self.repo / "scripts/lib" / helper.name)
        (self.repo / "Cargo.toml").write_text('[package]\nname = "fixture"\n')
        self.events_file = self.root / "events"
        self.processes = []
        self.env = dict(os.environ, HOME=str(self.home), XDG_RUNTIME_DIR=str(self.runtime),
                        XDG_STATE_HOME=str(self.root / "state"),
                        PATH=f"{self.commands}:/usr/bin:/bin",
                        FIXTURE_ROOT=str(self.root), FIXTURE_RELEASE=str(self.release))
        for name in ("APPLICATIONLAUNCHER_SOURCE_CHANGES_PENDING",
                     "APPLICATIONLAUNCHER_SYMBOLS_UNARCHIVED",
                     "APPLICATIONLAUNCHER_BUILD_REPAIR_PENDING"):
            self.env.pop(name, None)
        self.executable(self.commands / "systemctl", """
printf 'systemctl %s\n' "$*" >> "$FIXTURE_ROOT/events"
case "$*" in
  *MainPID*) printf '999999999\n' ;;
esac
""")
        # Hide every real process from the wrapper's executable-name scan. These
        # stubs are fixture-only and cannot stop the user's launcher or daemon.
        self.executable(self.commands / "readlink", """
case "${@: -1}" in
  /proc/*/exe) printf '/fixture/unrelated-process\n' ;;
  *) exec /usr/bin/readlink "$@" ;;
esac
""")
        self.executable(self.commands / "stat", """
args=("$@")
if [[ "${args[-1]}" == /proc/999999999/exe ]]; then
  if [[ -f "$FIXTURE_ROOT/stale-running-daemon" ]]; then
    printf '0:0\n'
    exit 0
  fi
  cat "$FIXTURE_ROOT/running-daemon-identity"
  exit 0
fi
exec /usr/bin/stat "${args[@]}"
""")
        self.executable(self.commands / "readelf", "exit 1\n")
        self.executable(self.commands / "notify-send", """
printf 'notification %s\n' "$*" >> "$FIXTURE_ROOT/events"
""")
        self.executable(self.commands / "cargo", """
printf 'cargo %s\n' "$*" >> "$FIXTURE_ROOT/events"
while [[ -f "$FIXTURE_ROOT/hold-build" ]]; do sleep 0.02; done
if [[ -f "$FIXTURE_ROOT/fail-build" ]]; then
  if [[ -f "$FIXTURE_ROOT/large-build-output" ]]; then
    head -c 400000 /dev/zero | tr '\\0' x
  fi
  printf 'fixture compiler failure\n' >&2
  exit 1
fi
cp "$FIXTURE_ROOT/new-gui" "$FIXTURE_RELEASE/applicationlauncher.next"
mv "$FIXTURE_RELEASE/applicationlauncher.next" "$FIXTURE_RELEASE/applicationlauncher"
if [[ ! -f "$FIXTURE_ROOT/partial-build" ]]; then
  cp "$FIXTURE_ROOT/new-daemon" "$FIXTURE_RELEASE/applicationlauncherd.next"
  mv "$FIXTURE_RELEASE/applicationlauncherd.next" "$FIXTURE_RELEASE/applicationlauncherd"
fi
""")
        self.set_pair("0.4.1-release-old", "0.4.1-release-old")
        self.binary(self.root / "new-gui", "gui", "0.4.1-release-repaired")
        self.binary(self.root / "new-daemon", "daemon", "0.4.1-release-repaired")
        (self.home / ".local/bin/applicationlauncherd").symlink_to(self.release / "applicationlauncherd")

    def tearDown(self):
        (self.root / "hold-build").unlink(missing_ok=True)
        for process in self.processes:
            # The fixture wrapper and its children have their own process group.
            try:
                os.killpg(process.pid, signal.SIGTERM)
            except ProcessLookupError:
                pass
            process.wait(timeout=5)
        self.directory.cleanup()

    def executable(self, path, body):
        path.write_text("#!/usr/bin/env bash\nset -euo pipefail\n" + body)
        path.chmod(0o700)

    def binary(self, path, component, build_id):
        self.executable(path, f"""
case "${{1:-}}" in
  --build-id)
    printf 'probe-{component}\\n' >> "$FIXTURE_ROOT/events"
    printf '%s\\n' '{build_id}'
    ;;
  --install|--shutdown)
    printf '{component} %s\\n' "$*" >> "$FIXTURE_ROOT/events"
    if [[ '{component}' == daemon && "$1" == --install ]]; then
      /usr/bin/stat -Lc '%d:%i' "$FIXTURE_RELEASE/applicationlauncherd" > "$FIXTURE_ROOT/running-daemon-identity"
    fi
    ;;
  *)
    printf 'launch-{component} source=%s repair=%s args=%s\\n' \\
      "${{APPLICATIONLAUNCHER_SOURCE_CHANGES_PENDING:-0}}" \\
      "${{APPLICATIONLAUNCHER_BUILD_REPAIR_PENDING:-0}}" "$*" >> "$FIXTURE_ROOT/events"
    ;;
esac
""")

    def set_pair(self, gui_id, daemon_id):
        self.binary(self.release / "applicationlauncher", "gui", gui_id)
        self.binary(self.release / "applicationlauncherd", "daemon", daemon_id)
        metadata = (self.release / "applicationlauncherd").stat()
        (self.root / "running-daemon-identity").write_text(f"{metadata.st_dev}:{metadata.st_ino}\n")

    def run_wrapper(self, *args):
        process = subprocess.Popen(["bash", str(self.repo / "scripts/applicationlauncher"), *args],
                                   env=self.env, stdout=subprocess.DEVNULL,
                                   stderr=subprocess.DEVNULL, start_new_session=True)
        self.processes.append(process)
        self.assertEqual(process.wait(timeout=5), 0)

    def events(self):
        if not self.events_file.exists():
            return []
        return self.events_file.read_text().splitlines()

    def wait_for(self, predicate):
        deadline = time.monotonic() + 8
        while time.monotonic() < deadline:
            if predicate():
                return
            time.sleep(0.02)
        self.fail(f"fixture deadline exceeded; events: {self.events()}")

    def builds(self):
        return [event for event in self.events() if event.startswith("cargo ")]

    def stops(self):
        return [event for event in self.events() if event == "systemctl --user stop applicationlauncherd.service"]

    def test_same_version_different_builds_are_repaired_together(self):
        self.set_pair("0.4.1-release-older", "0.4.1-release-newer")
        self.run_wrapper("--theme", "Breeze")
        self.wait_for(lambda: "daemon --install" in self.events())
        self.assertEqual(len(self.builds()), 1)
        self.assertIn("--release --locked --bins", self.builds()[0])
        self.wait_for(lambda: any("args=--theme Breeze" in event for event in self.events()
                                 if event.startswith("launch-gui ")))

    def test_healthy_pair_does_not_build_and_reuses_identity_cache(self):
        self.run_wrapper()
        self.run_wrapper()
        self.assertEqual(self.builds(), [])
        self.assertEqual(self.events().count("probe-gui"), 1)
        self.assertEqual(self.events().count("probe-daemon"), 1)

    def test_newer_source_is_a_warning_not_a_build(self):
        self.repo.joinpath("src").mkdir()
        source = self.repo / "src/main.rs"
        source.write_text("// newer source\n")
        newer = time.time() + 10
        os.utime(source, (newer, newer))
        self.run_wrapper()
        self.assertEqual(self.builds(), [])
        self.assertTrue(any("source=1" in event for event in self.events() if event.startswith("launch-gui ")))

    def test_repair_is_background_single_flight_and_does_not_stop_a_working_daemon(self):
        self.set_pair("0.4.1-release-older", "0.4.1-release-newer")
        (self.root / "hold-build").touch()
        self.run_wrapper()
        self.wait_for(lambda: len(self.builds()) == 1)
        for _ in range(3):
            self.run_wrapper()
        self.assertEqual(len(self.builds()), 1)
        self.assertEqual(self.stops(), [])
        self.assertNotIn("daemon --install", self.events())
        self.assertTrue(any("repair=1" in event for event in self.events() if event.startswith("launch-gui ")))
        (self.root / "hold-build").unlink()
        self.wait_for(lambda: "daemon --install" in self.events())

    def test_compiler_failure_preserves_processes_and_does_not_retry_every_shortcut(self):
        self.set_pair("0.4.1-release-older", "0.4.1-release-newer")
        (self.root / "fail-build").touch()
        self.run_wrapper()
        self.wait_for(lambda: any("repair failed" in event.lower() for event in self.events()))
        self.run_wrapper()
        self.assertEqual(len(self.builds()), 1)
        self.assertEqual(self.stops(), [])
        self.assertNotIn("daemon --install", self.events())

    def test_successful_cargo_with_a_still_incompatible_pair_does_not_activate(self):
        self.set_pair("0.4.1-release-older", "0.4.1-release-newer")
        (self.root / "partial-build").touch()
        self.run_wrapper()
        self.wait_for(lambda: any("repair failed" in event.lower() for event in self.events()))
        self.assertEqual(len(self.builds()), 1)
        self.assertEqual(self.stops(), [])
        self.assertNotIn("daemon --install", self.events())
        self.run_wrapper()
        self.assertEqual(len(self.builds()), 1)

    def test_new_binary_allows_an_immediate_retry_after_failure(self):
        self.set_pair("0.4.1-release-older", "0.4.1-release-newer")
        (self.root / "fail-build").touch()
        self.run_wrapper()
        self.wait_for(lambda: any("repair failed" in event.lower() for event in self.events()))
        (self.root / "fail-build").unlink()
        self.binary(self.release / "applicationlauncherd.next", "daemon", "0.4.1-release-replaced")
        self.release.joinpath("applicationlauncherd.next").replace(self.release / "applicationlauncherd")
        self.run_wrapper()
        self.wait_for(lambda: "daemon --install" in self.events())
        self.assertEqual(len(self.builds()), 2)

    def test_identical_failed_pair_can_retry_after_the_cooldown(self):
        self.set_pair("0.4.1-release-older", "0.4.1-release-newer")
        (self.root / "fail-build").touch()
        self.run_wrapper()
        self.wait_for(lambda: any("repair failed" in event.lower() for event in self.events()))
        failure = self.root / "state/applicationlauncher/launcher-repair-failed"
        records = failure.read_text().splitlines()
        records[0] = str(int(time.time()) - 301)
        failure.write_text("\n".join(records) + "\n")
        (self.root / "fail-build").unlink()
        self.run_wrapper()
        self.wait_for(lambda: "daemon --install" in self.events())
        self.assertEqual(len(self.builds()), 2)

    def test_failed_build_output_is_bounded_and_private(self):
        self.set_pair("0.4.1-release-older", "0.4.1-release-newer")
        (self.root / "fail-build").touch()
        (self.root / "large-build-output").touch()
        self.run_wrapper()
        self.wait_for(lambda: any("repair failed" in event.lower() for event in self.events()))
        log = self.root / "state/applicationlauncher/launcher-build.log"
        self.assertLessEqual(log.stat().st_size, 262144 + 1024)
        self.assertEqual(log.stat().st_mode & 0o777, 0o600)
        self.assertEqual(self.stops(), [])

    def test_timed_out_build_cannot_activate_a_replacement(self):
        self.set_pair("0.4.1-release-older", "0.4.1-release-newer")
        self.executable(self.commands / "timeout", """
if [[ "$*" == *"15m cargo build"* ]]; then
  printf 'build-timeout\\n' >> "$FIXTURE_ROOT/events"
  exit 124
fi
exec /usr/bin/timeout "$@"
""")
        self.run_wrapper()
        self.wait_for(lambda: any("repair failed" in event.lower() for event in self.events()))
        self.assertIn("build-timeout", self.events())
        self.assertEqual(self.stops(), [])
        self.assertNotIn("daemon --install", self.events())

    def test_replacing_one_binary_invalidates_the_cache(self):
        self.run_wrapper()
        self.binary(self.release / "applicationlauncherd.next", "daemon", "0.4.1-release-newer")
        self.release.joinpath("applicationlauncherd.next").replace(self.release / "applicationlauncherd")
        self.run_wrapper()
        self.wait_for(lambda: len(self.builds()) == 1)

    def test_unreadable_build_identity_is_not_treated_as_a_confirmed_mismatch(self):
        self.set_pair("0.4.1-release-older", "")
        self.run_wrapper()
        self.assertEqual(self.builds(), [])
        self.assertEqual(self.stops(), [])
        self.assertNotIn("daemon --install", self.events())

    def test_stale_running_daemon_with_a_compatible_disk_pair_only_restarts(self):
        (self.root / "stale-running-daemon").touch()
        self.run_wrapper()
        self.assertEqual(self.builds(), [])
        self.assertIn("daemon --install", self.events())

    def test_diagnostic_and_control_commands_do_not_repair_or_restart(self):
        self.set_pair("0.4.1-release-older", "0.4.1-release-newer")
        for args in (("--diagnose", "auto"), ("debug-doctor",), ("--build-id",), ("--shutdown",)):
            self.run_wrapper(*args)
        self.assertEqual(self.builds(), [])
        self.assertEqual(self.stops(), [])
        self.assertNotIn("daemon --install", self.events())


if __name__ == "__main__":
    unittest.main()
