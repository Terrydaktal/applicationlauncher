"""Desktop branding contracts, without changing the user's KWin session."""

import configparser
from pathlib import Path
import re
import unittest


ASSETS = Path(__file__).resolve().parents[1] / "src" / "assets"
RULE_ID = "applicationlauncher-window-branding"


class WindowBrandingTests(unittest.TestCase):
    def rule(self):
        config = configparser.ConfigParser(interpolation=None)
        with (ASSETS / "applicationlauncher.kwinrule").open() as source:
            config.read_file(source)
        self.assertEqual(config.sections(), [RULE_ID])
        return config[RULE_ID]

    def test_rule_changes_only_desktop_identity_not_class_geometry_or_focus(self):
        rule = self.rule()
        self.assertEqual(
            set(rule),
            {
                "description", "wmclass", "wmclassmatch", "wmclasscomplete",
                "desktopfile", "desktopfilerule",
            },
        )
        self.assertEqual(rule["desktopfile"], "applicationlauncher")
        self.assertEqual(rule.getint("desktopfilerule"), 2)
        self.assertEqual(rule.getint("wmclassmatch"), 3)
        self.assertFalse(rule.getboolean("wmclasscomplete"))

    def test_rule_matches_root_and_unique_subwindows_not_other_applications(self):
        pattern = re.compile(self.rule()["wmclass"])
        for identity in [
            "applicationlauncher",
            "applicationlauncher-info-42-0123456789abcdef",
            "applicationlauncher-info-98765-0000000000000000",
            "applicationlauncher-info-1-ffffffffffffffff",
        ]:
            with self.subTest(identity=identity):
                self.assertIsNotNone(pattern.fullmatch(identity))
        for identity in [
            "applicationlauncherd", "progs", "settings", "myapplicationlauncher",
            "applicationlauncher-other", "applicationlauncher-info-42-too-short",
            "applicationlauncher-info-42-0123456789abcdef-extra",
            "applicationlauncher-info-notpid-0123456789abcdef",
        ]:
            with self.subTest(identity=identity):
                self.assertIsNone(pattern.search(identity))

    def test_rule_uses_the_same_desktop_entry_and_icon_as_the_main_window(self):
        desktop = configparser.ConfigParser(interpolation=None)
        desktop.read(ASSETS / "applicationlauncher.desktop")
        self.assertEqual(
            self.rule()["desktopfile"], desktop["Desktop Entry"]["icon"]
        )
        self.assertEqual(desktop["Desktop Entry"]["startupwmclass"], "applicationlauncher")


if __name__ == "__main__":
    unittest.main()
