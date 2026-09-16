import importlib.util
import pathlib
import tempfile
import unittest

ROOT = pathlib.Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location("installer", ROOT / "scripts/install-firefox-audio-bridge.py")
INSTALLER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(INSTALLER)


class InstallTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="al-firefox-install-test-")
        self.root = pathlib.Path(self.temp.name)
        (self.root / "defaults/pref").mkdir(parents=True)
        (self.root / "application.ini").write_text("[App]\nName=Firefox\n")

    def tearDown(self):
        self.temp.cleanup()

    def test_existing_dictionary_configuration_survives_and_install_is_idempotent(self):
        original = "// Existing dictionary configuration.\nkeepDictionarySettings();\n"
        config = self.root / "dictionary.cfg"
        config.write_text(original)
        (self.root / "defaults/pref/dictionary.js").write_text('pref("general.config.filename", "dictionary.cfg");\n')
        for _ in range(2):
            INSTALLER.install(self.root, ROOT / "firefox/audio-bridge.js")
        self.assertEqual(INSTALLER.without_loader(config.read_text()), original)
        self.assertEqual(config.read_text().count(INSTALLER.BEGIN), 1)
        self.assertEqual(config.with_name(config.name + ".before-applicationlauncher-audio").read_text(), original)

    def test_fresh_install_has_private_bridge_loader_but_does_not_touch_profiles(self):
        path = INSTALLER.install(self.root, ROOT / "firefox/audio-bridge.js")
        self.assertTrue(path.read_text().startswith("//"))
        self.assertIn(INSTALLER.LOADER, path.read_text())
        self.assertFalse((self.root / "profiles.ini").exists())

    def test_conflicting_configuration_and_symlinks_are_rejected(self):
        prefs = self.root / "defaults/pref"
        (prefs / "one.js").write_text('pref("general.config.filename", "one.cfg");')
        (prefs / "two.js").write_text('pref("general.config.filename", "two.cfg");')
        with self.assertRaises(ValueError):
            INSTALLER.install(self.root, ROOT / "firefox/audio-bridge.js")
        (prefs / "two.js").unlink()
        (self.root / "one.cfg").symlink_to(ROOT / "README.md")
        with self.assertRaises(ValueError):
            INSTALLER.install(self.root, ROOT / "firefox/audio-bridge.js")


if __name__ == "__main__":
    unittest.main()
