#!/usr/bin/env python3
"""Install the optional AutoConfig bridge without replacing existing AutoConfig."""

import argparse
import pathlib
import re
import shutil
import tempfile

BEGIN = "// BEGIN applicationlauncher audio bridge"
END = "// END applicationlauncher audio bridge"
BRIDGE_NAME = "applicationlauncher-audio-bridge.js"
PREF_NAME = "zz-applicationlauncher-audio-bridge.js"
LOADER = """// BEGIN applicationlauncher audio bridge
try {
  const alDirectory = Components.classes["@mozilla.org/file/directory_service;1"]
    .getService(Components.interfaces.nsIProperties)
    .get("GreD", Components.interfaces.nsIFile);
  alDirectory.append("applicationlauncher-audio-bridge.js");
  const alIO = Components.classes["@mozilla.org/network/io-service;1"]
    .getService(Components.interfaces.nsIIOService);
  Components.classes["@mozilla.org/moz/jssubscript-loader;1"]
    .getService(Components.interfaces.mozIJSSubScriptLoader)
    .loadSubScript(alIO.newFileURI(alDirectory).spec);
} catch (alError) {
  Components.utils.reportError("Application Launcher audio bridge: " + alError);
}
// END applicationlauncher audio bridge
"""


def without_loader(text):
    if BEGIN not in text and END not in text:
        return text
    if text.count(BEGIN) != 1 or text.count(END) != 1:
        raise ValueError("ambiguous bridge markers; refusing to edit AutoConfig")
    start = text.index(BEGIN)
    end = text.index(END) + len(END)
    if end < start:
        raise ValueError("out-of-order bridge markers")
    if text[end:end + 1] == "\n":
        end += 1
    return text[:start] + text[end:]


def regular_or_absent(path):
    if path.is_symlink() or (path.exists() and not path.is_file()):
        raise ValueError(f"refusing non-regular destination: {path}")


def atomic_write(path, content):
    regular_or_absent(path)
    if path.exists() and path.read_bytes() == content:
        return
    with tempfile.NamedTemporaryFile(dir=path.parent, prefix=".al-audio-", delete=False) as temp:
        temporary = pathlib.Path(temp.name)
        try:
            temp.write(content)
            temp.flush()
            temporary.chmod(0o644)
            temporary.replace(path)
        finally:
            temporary.unlink(missing_ok=True)


def install(directory, source):
    pref_dir = directory / "defaults/pref"
    if not (directory / "application.ini").is_file() or not pref_dir.is_dir():
        raise ValueError(f"not a Firefox installation: {directory}")
    definitions = []
    for pref in sorted(pref_dir.glob("*.js")):
        # Ignore comments; do not execute existing preference JavaScript as root.
        text = re.sub(r"/\*.*?\*/|//[^\n]*", "", pref.read_text(), flags=re.S)
        definitions.extend(re.findall(
            r'\b(?:pref|defaultPref|lockPref)\(\s*[\'"]general\.config\.filename[\'"]\s*,\s*[\'"]([^\'"]+)[\'"]\s*\)', text))
    if len(set(definitions)) > 1:
        raise ValueError("conflicting AutoConfig filenames; refusing to change them")
    name = definitions[0] if definitions else "applicationlauncher-autoconfig.cfg"
    if not re.fullmatch(r"[A-Za-z0-9_.-]+", name) or name in (".", ".."):
        raise ValueError("unsafe AutoConfig filename")
    config = directory / name
    for path in (config, directory / BRIDGE_NAME, pref_dir / PREF_NAME):
        regular_or_absent(path)
    if definitions and not config.exists():
        raise ValueError("existing AutoConfig file is missing")
    original = config.read_text() if config.exists() else "// Firefox AutoConfig.\n"
    if not original.startswith("//"):
        raise ValueError("unsupported/obscured AutoConfig; refusing to replace it")
    content = without_loader(original)
    if not content.endswith("\n"):
        content += "\n"
    content += LOADER
    # One permanent recovery copy, not an ever-growing set of backups.
    backup = config.with_name(config.name + ".before-applicationlauncher-audio")
    if config.exists() and BEGIN not in original and not backup.exists():
        regular_or_absent(backup)
        shutil.copyfile(config, backup)
        backup.chmod(0o600)
    preferences = (
        '// Optional privileged playback ownership bridge; does not disable the content sandbox.\n'
        f'pref("general.config.filename", "{name}");\n'
        'pref("general.config.obscure_value", 0);\n'
        'pref("general.config.sandbox_enabled", false);\n'
    )
    atomic_write(directory / BRIDGE_NAME, source.read_bytes())
    atomic_write(config, content.encode())
    atomic_write(pref_dir / PREF_NAME, preferences.encode())
    return config


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--firefox-dir", type=pathlib.Path, default=pathlib.Path("/usr/lib/firefox"))
    args = parser.parse_args()
    source = pathlib.Path(__file__).resolve().parents[1] / "firefox/audio-bridge.js"
    try:
        config = install(args.firefox_dir, source)
    except (OSError, ValueError) as error:
        parser.exit(1, f"Installation failed: {error}\n")
    print(f"Installed playback ownership bridge; preserved {config}.")
    print("Takes effect at the next Firefox start. No running browser was restarted.")


if __name__ == "__main__":
    main()
