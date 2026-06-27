import argparse
import os
import re
import sys
import warnings

import gi

warnings.filterwarnings("ignore", category=DeprecationWarning)
gi.require_version("Atspi", "2.0")
from gi.repository import Atspi


WINDOW_ROLES = {"frame", "window", "dialog"}
BUTTON_ROLES = {"push button", "button", "radio button", "toggle button", "label", "table cell"}
URI_RE = re.compile(r"(?:file|trash|ftp|sftp|smb|mtp|afc|dav|davs)://[^\s\"'<>]+")
PATH_RE = re.compile(r"(?:~|/)[^\s\"'<>]+")
BUTTON_SKIP_KEYS = {
    "",
    "back",
    "forward",
    "up",
    "search",
    "refresh",
    "reload",
    "close",
    "view",
    "help",
    "jumpto",
}


def has_control(text):
    return any(ch in "\r\n\t" for ch in text)


def is_location_bar_node(name, desc):
    text = f"{name} {desc}".lower()
    return "folder location bar" in text or "location bar" in text


def has_location_bar_child(node):
    try:
        child_count = node.get_child_count()
    except Exception:
        return False
    for index in range(child_count):
        child = node.get_child_at_index(index)
        if is_location_bar_node(safe_name(child), safe_description(child)):
            return True
    return False


def safe_name(node):
    try:
        return node.get_name() or ""
    except Exception:
        return ""


def safe_description(node):
    try:
        return node.get_description() or ""
    except Exception:
        return ""


def safe_text(node):
    try:
        return node.get_text(0, -1) or ""
    except Exception:
        return ""


def normalize(text):
    return "".join(ch.lower() for ch in text if ch.isalnum())


def title_tokens(title):
    tokens = []
    for part in re.split(r"\s+[—–]\s+|\s+-\s+|\s+\|\s+|\s+:\s+|[—–]", title):
        part = part.strip()
        if not part:
            continue
        if normalize(part) in {"pcmanfm", "dolphin"}:
            continue
        tokens.append(part)
    return tokens


def iter_windows():
    desktop = Atspi.get_desktop(0)
    stack = []
    for index in range(desktop.get_child_count()):
        app = desktop.get_child_at_index(index)
        stack.append((safe_name(app), app))
    while stack:
        app_name, node = stack.pop()
        if node.get_role_name() in WINDOW_ROLES:
            yield app_name, node
        for index in range(node.get_child_count()):
            stack.append((app_name, node.get_child_at_index(index)))


def score_window(app_name, node, pid, title, class_name):
    score = 0
    node_name = safe_name(node)
    node_key = normalize(node_name)
    title_key = normalize(title)
    class_key = normalize(class_name)
    app_key = normalize(app_name)
    try:
        node_pid = node.get_process_id()
    except Exception:
        node_pid = None

    if pid is not None and node_pid == pid:
        score += 1000
    if title and node_name == title:
        score += 400
    if title_key and node_key == title_key:
        score += 320
    if title_key and title_key in node_key:
        score += 120
    if class_key and class_key in app_key:
        score += 80
    return score


def normalize_candidate(text):
    return text.strip().strip("\"'()[]{};,")


def is_usable_location(value):
    if not value or has_control(value):
        return False
    if "No such file or directory" in value:
        return False
    return value.startswith("/") or value.startswith("~") or "://" in value


def location_score(value, source_weight):
    score = source_weight + len(value)
    if value.startswith("/"):
        score += 180
    elif value.startswith("~"):
        score += 140
    elif "://" in value:
        score += 120

    expanded = os.path.expanduser(value)
    if value.startswith("/") or value.startswith("~"):
        if os.path.isdir(expanded):
            score += 200
        elif os.path.exists(expanded):
            score += 40
    return score


def push_location_candidates(text, source_weight, locations):
    if has_control(text):
        return
    for regex in (URI_RE, PATH_RE):
        for match in regex.finditer(text):
            value = normalize_candidate(match.group(0))
            if not is_usable_location(value):
                continue
            locations[value] = max(
                locations.get(value, 0),
                location_score(value, source_weight),
            )


def breadcrumb_candidates(button_names, title):
    title_keys = {normalize(token) for token in title_tokens(title)}
    if not title_keys:
        return []

    home = os.path.expanduser("~")
    cleaned = []
    for name in button_names:
        token = name.strip()
        key = normalize(token)
        if (key in BUTTON_SKIP_KEYS and token != "/") or has_control(token):
            continue
        cleaned.append(token)

    candidates = []
    for end_index, token in enumerate(cleaned):
        token_key = normalize(token)
        if token_key not in title_keys:
            continue
        for start_index in range(max(0, end_index - 8), end_index + 1):
            head = cleaned[start_index]
            head_key = normalize(head)
            middle = [part for part in cleaned[start_index + 1 : end_index + 1] if part]
            if head == "/":
                candidates.append("/" + "/".join(middle))
            elif head in {"~", "Home"}:
                candidates.append(os.path.join(home, *middle))
            elif head_key in {"filesystem", "root"}:
                candidates.append("/" + "/".join(middle))
    return candidates


def collect_locations(node, title, locations, button_names, in_location_bar=False):
    name = safe_name(node)
    desc = safe_description(node)
    text = safe_text(node)
    role = node.get_role_name()
    in_location_bar = in_location_bar or is_location_bar_node(name, desc) or has_location_bar_child(node)

    if in_location_bar:
        if name:
            push_location_candidates(name, 300, locations)
        if desc:
            push_location_candidates(desc, 220, locations)
        if text and text != name:
            push_location_candidates(text, 260, locations)
        if role in BUTTON_ROLES and name:
            button_names.append(name)
    elif role in WINDOW_ROLES and name:
        # Only trust explicit paths outside the location bar. File lists expose
        # dates like 19/06/26 that otherwise look path-like.
        push_location_candidates(name, 120, locations)

    for index in range(node.get_child_count()):
        collect_locations(
            node.get_child_at_index(index),
            title,
            locations,
            button_names,
            in_location_bar,
        )


def best_window(pid, title, class_name):
    winner = None
    best_score = 0
    for app_name, node in iter_windows():
        score = score_window(app_name, node, pid, title, class_name)
        if score > best_score:
            best_score = score
            winner = node
    if best_score < 300:
        return None
    return winner


def best_location(window, title):
    locations = {}
    button_names = []
    collect_locations(window, title, locations, button_names)

    for value in breadcrumb_candidates(button_names, title):
        value = normalize_candidate(value)
        if not is_usable_location(value):
            continue
        if value.startswith("/") or value.startswith("~") or "://" in value:
            locations[value] = max(locations.get(value, 0), location_score(value, 280))

    if not locations:
        return None

    return max(locations.items(), key=lambda item: item[1])[0]


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--pid", type=int)
    parser.add_argument("--title", default="")
    parser.add_argument("--class", dest="class_name", default="")
    args = parser.parse_args()

    window = best_window(args.pid, args.title, args.class_name)
    if window is None:
        return 1

    location = best_location(window, args.title)
    if not location:
        return 1

    print(location)
    return 0


if __name__ == "__main__":
    sys.exit(main())
