use eframe::egui;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

#[derive(Clone, Debug)]
struct WindowInfo {
    id: String,
    title: String,
    class: String,
    icon_path: Option<PathBuf>,
    active_process: Option<String>,
    exe_path: Option<PathBuf>,
}

#[derive(Clone, Debug)]
struct AppInfo {
    name: String,
    exec: String,
    icon_path: Option<PathBuf>,
    comment: Option<String>,
    desktop_file_path: PathBuf,
    is_settings_module: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LauncherMode {
    Windows,
    Apps,
}

enum LoadResult {
    Success(Vec<WindowInfo>),
    AppsSuccess(Vec<AppInfo>),
    Error(String),
}

struct App {
    mode: LauncherMode,
    windows: Vec<WindowInfo>,
    apps: Vec<AppInfo>,
    pinned_apps: Vec<PathBuf>,
    search_query: String,
    selected_index: usize,
    kdotool_path: Option<PathBuf>,
    error_message: Option<String>,
    start_time: Instant,
    close_on_blur: bool,
    force_theme: Option<String>,
    loading: bool,
    receiver: Option<std::sync::mpsc::Receiver<LoadResult>>,
    width: f32,
    height: f32,
    icon_only: bool,
    show_settings_menu: bool,
    show_system_settings_modules: bool,
}

fn print_help() {
    println!(
        r#"NAME
    applicationlauncher - A sleek application launcher for KDE Wayland in Rust

SYNOPSIS
    applicationlauncher [OPTIONS]

DESCRIPTION
    applicationlauncher is a fast, visually stunning GUI application launcher
    designed for KDE Plasma Wayland. It queries the list of all open window
    objects using kdotool, allows searching them via a fuzzy-matching interface,
    and switches focus to the selected window.

OPTIONS
    -h, --help
        Print this help information and exit.

    -a, --apps
        Open the launcher in application mode to show and launch installed
        desktop applications rather than active window objects.

    -i, --icon-only
        Show only application icons in a grid format without names (only
        applicable in application launcher mode).

    --close-on-blur
        Close the launcher window automatically when it loses focus.

    --theme <THEME>
        Force a specific icon theme (default: automatically detected).

OPERATION
    When launched, the application retrieves a list of all open windows using
    kdotool. It renders a frameless GUI window containing a search input.
    As you type, the list is filtered using a fuzzy matcher.
    
    Keyboard Navigation:
        - Up/Down Arrows: Move selection.
        - Enter: Activate selected window (or launch selected application).
        - Escape: Close launcher.
        - F5: Refresh list.
        - Ctrl+P: Toggle pin/unpin status for the selected application.

EXAMPLES
    applicationlauncher
        Launch the application launcher.

    applicationlauncher --no-close-on-blur
        Launch the application launcher without closing on focus loss.

FILES
    $HOME/.config/applicationlauncher/config.toml
        Optional configuration file (reserved for future use).

    $HOME/.config/applicationlauncher/window_size.txt
        Stores the persisted width and height of the launcher window.

    $HOME/.config/applicationlauncher/pinned_apps.txt
        Stores absolute paths of pinned desktop applications.

PATHS
    /usr/share/icons
        System icon themes.
    /usr/share/pixmaps
        Legacy system application icons.

SECURITY NOTES
    Wayland isolates windows from querying each other directly. This tool relies on
    kdotool, which utilizes internal KWin D-Bus scripting interfaces to securely
    interact with KWin.

EXIT STATUS
    0   Success.
    1   Failure (e.g., kdotool not found or D-Bus communication failed).

AUTHORS
    Terrydaktal <9lewis9@gmail.com>"#
    );
}

fn get_kdotool_path() -> PathBuf {
    if let Ok(home) = std::env::var("HOME") {
        let p = PathBuf::from(format!("{}/.cargo/bin/kdotool", home));
        if p.exists() {
            return p;
        }
    }
    // Fallback to searching in system PATH
    PathBuf::from("kdotool")
}

fn load_window_size() -> (f32, f32) {
    if let Ok(home) = std::env::var("HOME") {
        let path = PathBuf::from(format!("{}/.config/applicationlauncher/window_size.txt", home));
        if path.exists() {
            if let Ok(content) = std::fs::read_to_string(path) {
                let lines: Vec<&str> = content.lines().collect();
                if lines.len() >= 2 {
                    if let (Ok(w), Ok(h)) = (lines[0].trim().parse::<f32>(), lines[1].trim().parse::<f32>()) {
                        let w = w.clamp(300.0, 1920.0);
                        let h = h.clamp(200.0, 1080.0);
                        return (w, h);
                    }
                }
            }
        }
    }
    (650.0, 480.0) // Default size
}

fn load_show_system_settings_modules() -> bool {
    if let Ok(home) = std::env::var("HOME") {
        let path = PathBuf::from(format!("{}/.config/applicationlauncher/settings.txt", home));
        if path.exists() {
            if let Ok(content) = std::fs::read_to_string(path) {
                for line in content.lines() {
                    let parts: Vec<&str> = line.split('=').collect();
                    if parts.len() >= 2 && parts[0].trim() == "show_system_settings_modules" {
                        return parts[1].trim().parse::<bool>().unwrap_or(true);
                    }
                }
            }
        }
    }
    true // Default is true
}

fn save_show_system_settings_modules(value: bool) {
    if let Ok(home) = std::env::var("HOME") {
        let dir = PathBuf::from(format!("{}/.config/applicationlauncher", home));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("settings.txt");
        let content = format!("show_system_settings_modules={}\n", value);
        let _ = std::fs::write(path, content);
    }
}

fn parse_icon_from_desktop(path: &Path) -> Option<String> {
    let content = std::fs::read_to_string(path).ok()?;
    for line in content.lines() {
        let line = line.trim();
        if line.starts_with("Icon=") {
            let val = line.strip_prefix("Icon=")?;
            return Some(val.trim().to_string());
        }
    }
    None
}

fn find_icon(theme: &str, class: &str) -> Option<PathBuf> {
    if class.is_empty() {
        return None;
    }

    let lower = class.to_lowercase();
    let mut names = vec![lower.clone(), class.to_string()];

    // Handle reverse-DNS formats (e.g., org.xfce.mousepad -> mousepad)
    if lower.contains('.') {
        if let Some(last) = lower.split('.').last() {
            names.push(last.to_string());
        }
    }

    // Try finding the .desktop file to see if it has a hardcoded icon path or an override name
    let mut app_dirs = vec![PathBuf::from("/usr/share/applications")];
    if let Ok(home) = std::env::var("HOME") {
        app_dirs.push(PathBuf::from(format!("{}/.local/share/applications", home)));
    }

    let mut overrides = Vec::new();
    for dir in &app_dirs {
        for name in &names {
            let desktop_path = dir.join(format!("{}.desktop", name));
            if desktop_path.exists() {
                if let Some(icon_val) = parse_icon_from_desktop(&desktop_path) {
                    let p = PathBuf::from(&icon_val);
                    if p.is_absolute() && p.exists() {
                        return Some(p);
                    }
                    if !names.contains(&icon_val) && !overrides.contains(&icon_val) {
                        overrides.push(icon_val);
                    }
                }
            }
        }
    }

    // Insert overrides at the front of the names vector (highest specificity)
    for ovr in overrides.into_iter().rev() {
        names.insert(0, ovr);
    }

    // Keyword fallbacks for generic application categories
    if lower.contains("terminal") {
        names.push("utilities-terminal".to_string());
        names.push("terminal".to_string());
    }
    if lower.contains("mousepad") || lower.contains("editor") || lower.contains("text") {
        names.push("accessories-text-editor".to_string());
        names.push("mousepad".to_string());
    }
    if lower.contains("file-manager")
        || lower.contains("pcmanfm")
        || lower.contains("thunar")
        || lower.contains("dolphin")
    {
        names.push("system-file-manager".to_string());
        names.push("folder-open".to_string());
    }
    if lower.contains("web") || lower.contains("browser") || lower.contains("firefox") {
        names.push("web-browser".to_string());
    }
    if lower.contains("copyq") {
        names.push("copyq".to_string());
        names.push("edit-paste".to_string());
    }

    // Try finding in specified theme and standard fallbacks
    let themes_to_check = if theme == "breeze-dark" {
        vec!["breeze-dark", "breeze", "hicolor"]
    } else if theme == "breeze" {
        vec!["breeze", "breeze-dark", "hicolor"]
    } else {
        vec![theme, "breeze-dark", "breeze", "hicolor"]
    };

    for t in themes_to_check {
        for name in &names {
            if let Some(path) = freedesktop_icons::lookup(name)
                .with_theme(t)
                .with_size(48)
                .find()
            {
                return Some(path);
            }
        }
    }

    // Look in legacy /usr/share/pixmaps as a final fallback
    for name in &names {
        let pixmap = PathBuf::from(format!("/usr/share/pixmaps/{}.png", name));
        if pixmap.exists() {
            return Some(pixmap);
        }
    }
    None
}



fn match_app(query: &str, target: &str) -> Option<i64> {
    let query_lower = query.to_lowercase();
    let target_lower = target.to_lowercase();

    let q_len = query_lower.chars().count();
    if q_len == 0 {
        return Some(0);
    }

    let t_len = target_lower.chars().count();
    let limit = q_len / 2;

    if t_len < q_len.saturating_sub(limit) {
        return None;
    }

    let inf = limit + 1;
    let query_chars: Vec<char> = query_lower.chars().collect();
    let target_chars: Vec<char> = target_lower.chars().collect();

    let mut prev_prev = vec![inf; t_len + 1];
    let mut prev = vec![0; t_len + 1]; // Substring search start cost is 0
    let mut curr = vec![inf; t_len + 1];

    for i in 1..=q_len {
        curr.fill(inf);
        curr[0] = i;

        let mut row_min = inf;
        for j in 1..=t_len {
            let cost = usize::from(query_chars[i - 1] != target_chars[j - 1]);
            let deletion = prev[j] + 1;
            let insertion = curr[j - 1] + 1;
            let substitution = prev[j - 1] + cost;
            let mut cell = deletion.min(insertion).min(substitution);

            if i > 1 && j > 1 && query_chars[i - 1] == target_chars[j - 2] && query_chars[i - 2] == target_chars[j - 1] {
                cell = cell.min(prev_prev[j - 2] + 1);
            }

            curr[j] = cell;
            row_min = row_min.min(cell);
        }

        if row_min > limit {
            return None;
        }

        std::mem::swap(&mut prev_prev, &mut prev);
        std::mem::swap(&mut prev, &mut curr);
    }

    let mut min_distance = inf;
    let mut best_end_idx = 0;

    let search_start = q_len.saturating_sub(limit).max(1);
    let search_end = t_len;

    for j in search_start..=search_end {
        if prev[j] < min_distance {
            min_distance = prev[j];
            best_end_idx = j;
        }
    }

    if min_distance <= limit {
        let start_idx = best_end_idx.saturating_sub(q_len);
        
        let is_word_boundary = start_idx == 0 || {
            let prev_char = target_chars.get(start_idx - 1);
            prev_char.map_or(true, |c| c.is_whitespace() || *c == '-' || *c == '_' || *c == '/' || *c == '.')
        };

        // Base score starts at 1000, drops by 300 per typo (distance), and 20 per character offset from start.
        // A penalty of 150 is applied if it doesn't match at a word boundary.
        let mut score = 1000 - (min_distance as i64 * 300) - (start_idx as i64 * 20) - (t_len as i64);
        if !is_word_boundary {
            score -= 150;
        }
        Some(score)
    } else {
        None
    }
}

fn clean_exec_cmd(exec: &str) -> String {
    let mut cleaned = exec.to_string();
    for placeholder in &["%f", "%F", "%u", "%U", "%d", "%D", "%n", "%N", "%i", "%c", "%k", "%v"] {
        cleaned = cleaned.replace(placeholder, "");
    }
    cleaned.trim().to_string()
}

fn launch_app(exec: &str) {
    let cmd_str = clean_exec_cmd(exec);
    std::thread::spawn(move || {
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg(&cmd_str);

        // Clean Python environment variables to prevent version mismatch crashes in launched apps
        cmd.env_remove("PYTHONPATH");
        cmd.env_remove("PYTHONHOME");

        if let Ok(venv) = std::env::var("VIRTUAL_ENV") {
            cmd.env_remove("VIRTUAL_ENV");
            if let Ok(path_val) = std::env::var("PATH") {
                let venv_bin = std::path::PathBuf::from(venv).join("bin");
                let new_paths: Vec<_> = std::env::split_paths(&path_val)
                    .filter(|p| p != &venv_bin)
                    .collect();
                if let Ok(joined) = std::env::join_paths(new_paths) {
                    cmd.env("PATH", joined);
                }
            }
        }

        let _ = cmd.spawn();
    });
}

fn parse_desktop_file(path: &Path, theme: &str) -> Option<AppInfo> {
    let content = std::fs::read_to_string(path).ok()?;
    
    let mut in_desktop_entry = false;
    let mut name = None;
    let mut exec = None;
    let mut icon = None;
    let mut comment = None;
    let mut no_display = false;
    let mut is_application = false;
    let mut is_settings_module = false;

    if let Some(file_name) = path.file_name().and_then(|n| n.to_str()) {
        let name_lower = file_name.to_lowercase();
        if name_lower.starts_with("kcm_") || name_lower.contains("settings") {
            is_settings_module = true;
        }
    }

    // Use current locale language code if available
    let lang = std::env::var("LANG").ok()
        .and_then(|l| l.split('.').next().map(|s| s.to_string()))
        .and_then(|l| l.split('_').next().map(|s| s.to_string()));
    
    let name_key = lang.as_ref().map(|l| format!("Name[{}]", l));
    let comment_key = lang.as_ref().map(|l| format!("Comment[{}]", l));

    for line in content.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            if line == "[Desktop Entry]" {
                in_desktop_entry = true;
            } else {
                in_desktop_entry = false;
            }
            continue;
        }

        if !in_desktop_entry {
            continue;
        }

        if let Some(pos) = line.find('=') {
            let key = line[..pos].trim();
            let val = line[pos + 1..].trim();

            if key == "Name" && name.is_none() {
                name = Some(val.to_string());
            } else if let Some(ref nk) = name_key {
                if key == nk {
                    name = Some(val.to_string());
                }
            }

            if key == "Comment" && comment.is_none() {
                comment = Some(val.to_string());
            } else if let Some(ref ck) = comment_key {
                if key == ck {
                    comment = Some(val.to_string());
                }
            }

            if key == "Exec" {
                exec = Some(val.to_string());
            }
            if key == "Icon" {
                icon = Some(val.to_string());
            }
            if key == "NoDisplay" && val.to_lowercase() == "true" {
                no_display = true;
            }
            if key == "Type" && val == "Application" {
                is_application = true;
            }
            if key == "Categories" && val.split(';').any(|c| c == "Settings" || c == "SettingsPanel" || c == "System") {
                is_settings_module = true;
            }
            if key == "X-KDE-AliasFor" && val == "systemsettings" {
                is_settings_module = true;
            }
        }
    }

    if (no_display && !is_settings_module) || !is_application {
        return None;
    }

    let name = name?;
    let exec = exec?;

    let icon_path = icon.and_then(|i| {
        let p = PathBuf::from(&i);
        if p.is_absolute() && p.exists() {
            return Some(p);
        }
        find_icon(theme, &i)
    });

    Some(AppInfo {
        name,
        exec,
        icon_path,
        comment,
        desktop_file_path: path.to_path_buf(),
        is_settings_module,
    })
}

fn get_installed_apps(theme: &str) -> Vec<AppInfo> {
    let mut apps = Vec::new();
    let mut app_dirs = vec![PathBuf::from("/usr/share/applications")];
    if let Ok(home) = std::env::var("HOME") {
        app_dirs.push(PathBuf::from(format!("{}/.local/share/applications", home)));
    }
    let flatpak_dir = PathBuf::from("/var/lib/flatpak/exports/share/applications");
    if flatpak_dir.exists() {
        app_dirs.push(flatpak_dir);
    }
    let user_flatpak_dir = if let Ok(home) = std::env::var("HOME") {
        Some(PathBuf::from(format!("{}/.local/share/flatpak/exports/share/applications", home)))
    } else {
        None
    };
    if let Some(dir) = user_flatpak_dir {
        if dir.exists() {
            app_dirs.push(dir);
        }
    }

    let mut seen_entries = std::collections::HashSet::new();

    for dir in app_dirs {
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_file() && path.extension().map_or(false, |ext| ext == "desktop") {
                    if let Some(file_name) = path.file_name().and_then(|n| n.to_str()) {
                        if seen_entries.contains(file_name) {
                            continue;
                        }
                        seen_entries.insert(file_name.to_string());
                    }

                    if let Some(app) = parse_desktop_file(&path, theme) {
                        apps.push(app);
                    }
                }
            }
        }
    }

    apps.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    apps
}
fn parse_proc_stat(stat_content: &str) -> Option<(i32, String, i32)> {
    let last_paren = stat_content.rfind(')')?;
    let (left, right) = stat_content.split_at(last_paren);
    
    let pid_part = left.split_whitespace().next()?;
    let pid: i32 = pid_part.parse().ok()?;
    
    let name_start = left.find('(')? + 1;
    let name = left[name_start..].to_string();
    
    let tokens: Vec<&str> = right[1..].split_whitespace().collect();
    if tokens.len() < 2 {
        return None;
    }
    let ppid: i32 = tokens[1].parse().ok()?;
    
    Some((pid, name, ppid))
}

fn get_process_tree() -> (HashMap<i32, Vec<i32>>, HashMap<i32, String>) {
    let mut ppid_to_children = HashMap::new();
    let mut pid_to_name = HashMap::new();
    
    if let Ok(entries) = std::fs::read_dir("/proc") {
        for entry in entries {
            if let Ok(entry) = entry {
                let path = entry.path();
                if path.is_dir() {
                    if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                        if name.chars().all(|c| c.is_ascii_digit()) {
                            let stat_path = path.join("stat");
                            if let Ok(content) = std::fs::read_to_string(stat_path) {
                                if let Some((pid, proc_name, ppid)) = parse_proc_stat(&content) {
                                    pid_to_name.insert(pid, proc_name);
                                    ppid_to_children.entry(ppid).or_insert_with(Vec::new).push(pid);
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    
    (ppid_to_children, pid_to_name)
}

fn is_shell(name: &str) -> bool {
    let n = name.to_lowercase();
    n == "bash" || n == "fish" || n == "zsh" || n == "sh" || n == "dash" || n == "tcsh" || n == "ksh"
}

fn find_terminal_leaf(
    terminal_pid: i32,
    ppid_to_children: &HashMap<i32, Vec<i32>>,
    pid_to_name: &HashMap<i32, String>,
) -> Option<(i32, String)> {
    let mut current_pid = terminal_pid;
    loop {
        if let Some(children) = ppid_to_children.get(&current_pid) {
            let mut valid_children = Vec::new();
            for &child in children {
                if let Some(name) = pid_to_name.get(&child) {
                    valid_children.push((child, name.clone()));
                }
            }
            
            if valid_children.is_empty() {
                break;
            }
            
            valid_children.sort_by(|a, b| {
                let a_is_shell = is_shell(&a.1);
                let b_is_shell = is_shell(&b.1);
                match (a_is_shell, b_is_shell) {
                    (true, false) => std::cmp::Ordering::Less,
                    (false, true) => std::cmp::Ordering::Greater,
                    _ => a.0.cmp(&b.0),
                }
            });
            
            if let Some(&(best_child, _)) = valid_children.last() {
                current_pid = best_child;
            } else {
                break;
            }
        } else {
            break;
        }
    }
    
    if current_pid == terminal_pid {
        None
    } else {
        pid_to_name.get(&current_pid).map(|name| (current_pid, name.clone()))
    }
}

fn get_open_windows(kdotool_path: &Path, theme: &str) -> Vec<WindowInfo> {
    // 1. Fetch all window IDs using kdotool search
    let output = match Command::new(kdotool_path)
        .arg("search")
        .arg("--title")
        .arg("")
        .output()
    {
        Ok(o) => o,
        Err(e) => {
            eprintln!("Failed to execute kdotool search: {:?}", e);
            return Vec::new();
        }
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut ids = Vec::new();
    for line in stdout.lines() {
        let line = line.trim();
        if !line.is_empty() {
            ids.push(line.to_string());
        }
    }

    if ids.is_empty() {
        return Vec::new();
    }

    // 2. Query all window metadata in a single chained kdotool invocation!
    // This reduces process spawning from N*3 down to exactly 1, eliminating startup lag.
    let mut cmd = Command::new(kdotool_path);
    for id in &ids {
        cmd.arg("getwindowid")
            .arg(id)
            .arg("getwindowname")
            .arg(id)
            .arg("getwindowclassname")
            .arg(id)
            .arg("getwindowpid")
            .arg(id);
    }

    let meta_output = match cmd.output() {
        Ok(o) => o,
        Err(e) => {
            eprintln!("Failed to execute chained kdotool metadata command: {:?}", e);
            return Vec::new();
        }
    };

    let meta_stdout = String::from_utf8_lossy(&meta_output.stdout);
    let lines: Vec<&str> = meta_stdout.lines().collect();

    // 3. Scan /proc once to build process tree before querying PIDs
    let (ppid_to_children, pid_to_name) = get_process_tree();

    let mut windows = Vec::new();
    let theme_str = theme.to_string();
    let mut icon_cache = HashMap::new();

    // Parse blocks of metadata. Since invalid windows get skipped, we search for UUID patterns
    // to identify the start of each valid window's metadata block.
    let mut idx = 0;
    while idx < lines.len() {
        let line = lines[idx].trim();
        // Check if line matches window UUID format (enclosed in curly braces)
        if line.starts_with('{') && line.ends_with('}') {
            let id = line.to_string();
            let title = lines.get(idx + 1).map(|s| s.trim().to_string()).unwrap_or_default();
            let class = lines.get(idx + 2).map(|s| s.trim().to_string()).unwrap_or_default();
            let pid_str = lines.get(idx + 3).map(|s| s.trim().to_string()).unwrap_or_default();
            let pid: Option<i32> = pid_str.parse().ok();

            idx += 4; // Advance to the next expected block

            let class_lower = class.to_lowercase();
            let my_pid = std::process::id() as i32;

            // Filter out system panels, desktops, window manager shells, and the launcher itself
            if class_lower.contains("plasmashell")
                || class_lower == "kwin_wayland"
                || class_lower.is_empty()
                || class_lower == "applicationlauncher"
                || title == "Open Application Windows"
                || pid == Some(my_pid)
            {
                continue;
            }

            // Fallback for empty title
            let display_title = if title.is_empty() {
                class.clone()
            } else {
                title
            };

            // Detect active process running in terminal and resolve binary exe path
            let mut active_process = None;
            let mut exe_path = None;
            if let Some(pid) = pid {
                let is_terminal = class_lower.contains("terminal")
                    || class_lower == "konsole"
                    || class_lower == "kitty"
                    || class_lower == "alacritty"
                    || class_lower == "wezterm"
                    || class_lower == "foot";

                let mut target_pid = pid;
                if is_terminal {
                    if let Some((leaf_pid, leaf_name)) = find_terminal_leaf(pid, &ppid_to_children, &pid_to_name) {
                        active_process = Some(leaf_name);
                        target_pid = leaf_pid;
                    }
                }

                if let Ok(path) = std::fs::read_link(format!("/proc/{}/exe", target_pid)) {
                    exe_path = Some(path);
                }
            }

            // Insert active process name between terminal name and working directory in the title
            let mut final_title = display_title;
            if let Some(ref proc_name) = active_process {
                let separators = [" - ", " — ", " – ", " : ", " | "];
                let mut split_found = false;
                for sep in separators {
                    if let Some(pos) = final_title.find(sep) {
                        let (left, right) = final_title.split_at(pos);
                        let right_clean = &right[sep.len()..];
                        final_title = format!("{}{}{}{}{}", left.trim(), sep, proc_name, sep, right_clean.trim());
                        split_found = true;
                        break;
                    }
                }
                if !split_found {
                    final_title = format!("{} - {}", final_title, proc_name);
                }
            }

            // Caching icon resolution to avoid repeated filesystem scans for identical applications
            let icon_key = active_process.as_ref().unwrap_or(&class).clone();
            let icon_path = icon_cache.entry(icon_key.clone()).or_insert_with(|| {
                let mut path = None;
                if let Some(ref proc_name) = active_process {
                    path = find_icon(&theme_str, proc_name);
                }
                if path.is_none() {
                    path = find_icon(&theme_str, &class);
                }
                path
            }).clone();

            windows.push(WindowInfo {
                id,
                title: final_title,
                class,
                icon_path,
                active_process,
                exe_path,
            });
        } else {
            idx += 1;
        }
    }

    windows
}

fn load_pinned_apps() -> Vec<PathBuf> {
    let mut pinned = Vec::new();
    if let Ok(home) = std::env::var("HOME") {
        let path = PathBuf::from(format!("{}/.config/applicationlauncher/pinned_apps.txt", home));
        if path.exists() {
            if let Ok(content) = std::fs::read_to_string(path) {
                for line in content.lines() {
                    let line = line.trim();
                    if !line.is_empty() {
                        let p = PathBuf::from(line);
                        if !pinned.contains(&p) {
                            pinned.push(p);
                        }
                    }
                }
            }
        }
    }
    pinned
}

fn get_window_geometry(kpath: &Path, id: &str) -> Option<(f32, f32, f32, f32)> {
    let output = Command::new(kpath)
        .args(["getwindowgeometry", id])
        .output()
        .ok()?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    
    let mut x = None;
    let mut y = None;
    let mut width = None;
    let mut height = None;
    
    for line in stdout.lines() {
        let line = line.trim();
        if line.starts_with("Position:") {
            let pos_part = line.strip_prefix("Position:")?.trim();
            let coords: Vec<&str> = pos_part.split(',').collect();
            if coords.len() >= 2 {
                x = coords[0].parse::<f32>().ok();
                y = coords[1].parse::<f32>().ok();
            }
        } else if line.starts_with("Geometry:") {
            let geom_part = line.strip_prefix("Geometry:")?.trim();
            let dims: Vec<&str> = geom_part.split('x').collect();
            if dims.len() >= 2 {
                width = dims[0].parse::<f32>().ok();
                height = dims[1].parse::<f32>().ok();
            }
        }
    }
    
    Some((x?, y?, width?, height?))
}

impl App {
    fn new(
        cc: &eframe::CreationContext<'_>,
        close_on_blur: bool,
        force_theme: Option<String>,
        mode: LauncherMode,
        icon_only: bool,
    ) -> Self {
        // Install loaders to enable SVG and PNG image support
        egui_extras::install_image_loaders(&cc.egui_ctx);

        // Styling the theme for custom dark acrylic style
        let mut visuals = egui::Visuals::dark();
        visuals.window_corner_radius = egui::CornerRadius::same(12);
        visuals.widgets.active.corner_radius = egui::CornerRadius::same(8);
        visuals.widgets.hovered.corner_radius = egui::CornerRadius::same(8);
        visuals.widgets.inactive.corner_radius = egui::CornerRadius::same(8);

        visuals.widgets.inactive.weak_bg_fill = egui::Color32::from_rgba_unmultiplied(255, 255, 255, 6);
        visuals.widgets.hovered.weak_bg_fill = egui::Color32::from_rgba_unmultiplied(255, 255, 255, 16);
        visuals.widgets.active.weak_bg_fill = egui::Color32::from_rgba_unmultiplied(255, 255, 255, 30);
        visuals.override_text_color = Some(egui::Color32::WHITE);

        cc.egui_ctx.set_visuals(visuals);

        let kdotool_path = get_kdotool_path();
        let (width, height) = load_window_size();
        let pinned_apps = load_pinned_apps();
        let mut app = Self {
            mode,
            windows: Vec::new(),
            apps: Vec::new(),
            pinned_apps,
            search_query: String::new(),
            selected_index: 0,
            kdotool_path: Some(kdotool_path),
            error_message: None,
            start_time: Instant::now(),
            close_on_blur,
            force_theme,
            loading: false,
            receiver: None,
            width,
            height,
            icon_only,
            show_settings_menu: false,
            show_system_settings_modules: load_show_system_settings_modules(),
        };

        match app.mode {
            LauncherMode::Apps => app.refresh_apps(),
            LauncherMode::Windows => app.refresh_windows(),
        }

        app
    }

    fn save_window_size(&self) {
        if let Ok(home) = std::env::var("HOME") {
            let dir = PathBuf::from(format!("{}/.config/applicationlauncher", home));
            let _ = std::fs::create_dir_all(&dir);
            let path = dir.join("window_size.txt");
            let content = format!("{}\n{}", self.width, self.height);
            let _ = std::fs::write(path, content);
        }
    }

    fn save_pinned_apps(&self) {
        if let Ok(home) = std::env::var("HOME") {
            let dir = PathBuf::from(format!("{}/.config/applicationlauncher", home));
            let _ = std::fs::create_dir_all(&dir);
            let path = dir.join("pinned_apps.txt");
            let mut content = String::new();
            for p in &self.pinned_apps {
                content.push_str(&p.to_string_lossy());
                content.push('\n');
            }
            let _ = std::fs::write(path, content);
        }
    }

    fn refresh_windows(&mut self) {
        if let Some(ref kpath) = self.kdotool_path {
            let kpath = kpath.clone();
            let theme = self.force_theme.as_deref().unwrap_or("breeze-dark").to_string();
            let (tx, rx) = std::sync::mpsc::channel();
            self.loading = true;
            self.receiver = Some(rx);

            std::thread::spawn(move || {
                match Command::new(&kpath).arg("--version").output() {
                    Ok(_) => {
                        let windows = get_open_windows(&kpath, &theme);
                        let _ = tx.send(LoadResult::Success(windows));
                    }
                    Err(_) => {
                        let _ = tx.send(LoadResult::Error(format!(
                            "kdotool utility not found.\n\nPlease install it using cargo:\n\ncargo install kdotool"
                        )));
                    }
                }
            });
        }
    }

    fn refresh_apps(&mut self) {
        let theme = self.force_theme.as_deref().unwrap_or("breeze-dark").to_string();
        let (tx, rx) = std::sync::mpsc::channel();
        self.loading = true;
        self.receiver = Some(rx);

        std::thread::spawn(move || {
            let apps = get_installed_apps(&theme);
            let _ = tx.send(LoadResult::AppsSuccess(apps));
        });
    }

    fn launch_app_and_exit(&self, exec: String, ctx: &egui::Context) {
        launch_app(&exec);
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }

    fn activate_and_exit(&self, id: String, ctx: &egui::Context) {
        if let Some(ref kpath) = self.kdotool_path {
            let kpath = kpath.clone();
            let id_clone = id.clone();
            std::thread::spawn(move || {
                // Query geometry first
                let geom = get_window_geometry(&kpath, &id_clone);
                
                // Activate the window and raise it to make sure it comes to the top
                let _ = Command::new(&kpath)
                    .args(["windowactivate", &id_clone, "windowraise", &id_clone])
                    .status();
                    
                // If we got geometry, spawn the border overlay process
                if let Some((x, y, w, h)) = geom {
                    if let Ok(current_exe) = std::env::current_exe() {
                        let _ = Command::new(current_exe)
                            .args([
                                "--draw-border",
                                &x.to_string(),
                                &y.to_string(),
                                &w.to_string(),
                                &h.to_string(),
                                &id_clone,
                            ])
                            .spawn();
                    }
                }
            });
        }
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Track window size changes in memory
        let current_size = ctx.viewport_rect().size();
        if (current_size.x - self.width).abs() > 1.0 || (current_size.y - self.height).abs() > 1.0 {
            self.width = current_size.x;
            self.height = current_size.y;
        }

        // Check background receiver for window query results
        if self.loading {
            ctx.request_repaint(); // Keep repainting until loaded to check channel promptly
            if let Some(ref rx) = self.receiver {
                if let Ok(result) = rx.try_recv() {
                    self.loading = false;
                    match result {
                        LoadResult::Success(windows) => {
                            self.windows = windows;
                            self.selected_index = 0;
                        }
                        LoadResult::AppsSuccess(apps) => {
                            self.apps = apps;
                            self.selected_index = 0;
                        }
                        LoadResult::Error(err) => {
                            self.error_message = Some(err);
                            self.kdotool_path = None;
                        }
                    }
                }
            }
        }

        // Focus loss auto-close
        if self.close_on_blur
            && self.start_time.elapsed().as_millis() > 500
            && !ctx.input(|i| i.focused)
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }

        // Main transparent window frame
        let panel_frame = egui::Frame {
            fill: egui::Color32::TRANSPARENT,
            ..Default::default()
        };

        egui::CentralPanel::default()
            .frame(panel_frame)
            .show(ctx, |ui| {
                // Translucent acrylic-like container
                let container_frame = egui::Frame {
                    fill: egui::Color32::from_rgba_unmultiplied(22, 23, 27, 240), // Dark glass fill
                    corner_radius: egui::CornerRadius::same(12),
                    stroke: egui::Stroke::new(
                        1.0,
                        egui::Color32::from_rgba_unmultiplied(255, 255, 255, 18),
                    ),
                    inner_margin: egui::Margin::same(14),
                    ..Default::default()
                };

                container_frame.show(ui, |ui| {
                    if let Some(ref err) = self.error_message {
                        ui.vertical_centered(|ui| {
                            ui.add_space(20.0);
                            ui.label(
                                egui::RichText::new("⚠️ Error")
                                    .color(egui::Color32::from_rgb(218, 68, 83))
                                    .strong()
                                    .size(24.0),
                            );
                            ui.add_space(10.0);
                            ui.label(egui::RichText::new(err).size(14.0));
                            ui.add_space(20.0);
                            if ui.button("Exit").clicked() {
                                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                            }
                        });
                        return;
                    }

                    // 1. Search Bar Container
                    let search_bar_frame = egui::Frame {
                        fill: egui::Color32::from_rgba_unmultiplied(255, 255, 255, 10),
                        corner_radius: egui::CornerRadius::same(8),
                        inner_margin: egui::Margin::symmetric(12, 8),
                        stroke: egui::Stroke::new(
                            1.0,
                            egui::Color32::from_rgba_unmultiplied(255, 255, 255, 15),
                        ),
                        ..Default::default()
                    };

                    let mut text_edit_response = None;

                    search_bar_frame.show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(
                                egui::RichText::new("🔍")
                                    .size(18.0)
                                    .color(egui::Color32::from_rgba_unmultiplied(255, 255, 255, 160)),
                            );
                            ui.add_space(4.0);
                            let hint_text = match self.mode {
                                LauncherMode::Apps => "Search applications...",
                                LauncherMode::Windows => "Search open windows...",
                            };
                            let text_edit = egui::TextEdit::singleline(&mut self.search_query)
                                .hint_text(hint_text)
                                .desired_width(ui.available_width())
                                .frame(false)
                                .font(egui::FontId::proportional(16.0));
                            
                            text_edit_response = Some(ui.add(text_edit));
                        });
                    });

                    // Force focus on text edit at launch
                    if let Some(ref resp) = text_edit_response {
                        if self.start_time.elapsed().as_millis() < 400 && !self.show_settings_menu {
                            resp.request_focus();
                        }
                    }

                    ui.add_space(10.0);

                    // 2. Filtering list
                    let mut filtered_apps: Vec<(AppInfo, i64, bool)> = Vec::new();
                    let mut filtered_windows: Vec<(WindowInfo, i64)> = Vec::new();

                    match self.mode {
                        LauncherMode::Apps => {
                            filtered_apps = self.apps
                                .iter()
                                .filter(|app| self.show_system_settings_modules || !app.is_settings_module)
                                .filter_map(|app| {
                                    let is_pinned = self.pinned_apps.contains(&app.desktop_file_path);
                                    if self.search_query.is_empty() {
                                        Some((app.clone(), 0, is_pinned))
                                    } else {
                                        let name_score = match_app(&self.search_query, &app.name);
                                        let comment_score = app.comment.as_ref()
                                            .and_then(|c| match_app(&self.search_query, c))
                                            .map(|s| s - 300);
                                        
                                        let score = match (name_score, comment_score) {
                                            (Some(ns), Some(cs)) => Some(std::cmp::max(ns, cs)),
                                            (Some(ns), None) => Some(ns),
                                            (None, Some(cs)) => Some(cs),
                                            (None, None) => None,
                                        };

                                        if let Some(score) = score {
                                            Some((app.clone(), score, is_pinned))
                                        } else {
                                            None
                                        }
                                    }
                                })
                                .collect();

                            // Sort: pinned first. Within groups: alphabetical if search query is empty, else score descending.
                            // If both are pinned, preserve their custom order from self.pinned_apps.
                            filtered_apps.sort_by(|a, b| {
                                // Sort by match score first (descending)
                                match b.1.cmp(&a.1) {
                                    std::cmp::Ordering::Equal => {
                                        // If scores are equal, sort by pinned status (pinned first)
                                        match (a.2, b.2) {
                                            (true, false) => std::cmp::Ordering::Less,
                                            (false, true) => std::cmp::Ordering::Greater,
                                            (true, true) => {
                                                // If both are pinned, sort by custom pinned order
                                                let pos_a = self.pinned_apps.iter().position(|x| x == &a.0.desktop_file_path).unwrap_or(usize::MAX);
                                                let pos_b = self.pinned_apps.iter().position(|x| x == &b.0.desktop_file_path).unwrap_or(usize::MAX);
                                                pos_a.cmp(&pos_b)
                                            }
                                            (false, false) => {
                                                // If both are unpinned, sort alphabetically
                                                a.0.name.to_lowercase().cmp(&b.0.name.to_lowercase())
                                            }
                                        }
                                    }
                                    other => other,
                                }
                            });
                        }
                        LauncherMode::Windows => {
                            filtered_windows = self.windows
                                .iter()
                                .filter_map(|w| {
                                    if self.search_query.is_empty() {
                                        Some((w.clone(), 0))
                                    } else {
                                        let title_score = match_app(&self.search_query, &w.title);
                                        let class_score = match_app(&self.search_query, &w.class)
                                            .map(|s| s - 100);
                                        
                                        let score = match (title_score, class_score) {
                                            (Some(ts), Some(cs)) => Some(std::cmp::max(ts, cs)),
                                            (Some(ts), None) => Some(ts),
                                            (None, Some(cs)) => Some(cs),
                                            (None, None) => None,
                                        };

                                        if let Some(score) = score {
                                            Some((w.clone(), score))
                                        } else {
                                            None
                                        }
                                    }
                                })
                                .collect();
                            if !self.search_query.is_empty() {
                                filtered_windows.sort_by(|a, b| b.1.cmp(&a.1));
                            }
                        }
                    }

                    let total_items = match self.mode {
                        LauncherMode::Apps => filtered_apps.len(),
                        LauncherMode::Windows => filtered_windows.len(),
                    };

                    // Safety bounds check for list changes (run early to prevent index out of bounds)
                    if self.selected_index >= total_items {
                        self.selected_index = 0;
                    }

                    let mut scroll_to_selected = false;
                    let columns = if self.icon_only && self.mode == LauncherMode::Apps {
                        (((ui.available_width() + 12.0) / 84.0).floor() as usize).max(1)
                    } else {
                        1
                    };

                    if ctx.input(|i| i.key_pressed(egui::Key::F10)) {
                        self.show_settings_menu = !self.show_settings_menu;
                    }

                    if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
                        if self.show_settings_menu {
                            self.show_settings_menu = false;
                        } else {
                            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                        }
                    }

                    if !self.show_settings_menu {
                        // Keyboard navigation inputs
                        if self.icon_only && self.mode == LauncherMode::Apps {
                            if ctx.input(|i| i.key_pressed(egui::Key::ArrowRight)) && total_items > 0 {
                                self.selected_index = (self.selected_index + 1) % total_items;
                                scroll_to_selected = true;
                            }
                            if ctx.input(|i| i.key_pressed(egui::Key::ArrowLeft)) && total_items > 0 {
                                self.selected_index = if self.selected_index == 0 {
                                    total_items - 1
                                } else {
                                    self.selected_index - 1
                                };
                                scroll_to_selected = true;
                            }
                            if ctx.input(|i| i.key_pressed(egui::Key::ArrowDown)) && total_items > 0 {
                                self.selected_index = (self.selected_index + columns) % total_items;
                                scroll_to_selected = true;
                            }
                            if ctx.input(|i| i.key_pressed(egui::Key::ArrowUp)) && total_items > 0 {
                                self.selected_index = if self.selected_index < columns {
                                    total_items - 1
                                } else {
                                    self.selected_index - columns
                                };
                                scroll_to_selected = true;
                            }
                        } else {
                            if ctx.input(|i| i.key_pressed(egui::Key::ArrowDown)) && total_items > 0 {
                                self.selected_index = (self.selected_index + 1) % total_items;
                                scroll_to_selected = true;
                            }
                            if ctx.input(|i| i.key_pressed(egui::Key::ArrowUp)) && total_items > 0 {
                                self.selected_index = if self.selected_index == 0 {
                                    total_items - 1
                                } else {
                                    self.selected_index - 1
                                };
                                scroll_to_selected = true;
                            }
                        }

                        if ctx.input(|i| i.key_pressed(egui::Key::Enter)) && total_items > 0 {
                            match self.mode {
                                LauncherMode::Apps => {
                                    let app = &filtered_apps[self.selected_index].0;
                                    self.launch_app_and_exit(app.exec.clone(), ctx);
                                }
                                LauncherMode::Windows => {
                                    let win = &filtered_windows[self.selected_index].0;
                                    self.activate_and_exit(win.id.clone(), ctx);
                                }
                            }
                        }
                        if ctx.input(|i| i.key_pressed(egui::Key::F5)) {
                            match self.mode {
                                LauncherMode::Apps => self.refresh_apps(),
                                LauncherMode::Windows => self.refresh_windows(),
                            }
                        }
                        if ctx.input(|i| i.modifiers.command && i.key_pressed(egui::Key::P)) && total_items > 0 {
                            if let LauncherMode::Apps = self.mode {
                                let app = &filtered_apps[self.selected_index].0;
                                let path = app.desktop_file_path.clone();
                                if let Some(pos) = self.pinned_apps.iter().position(|x| x == &path) {
                                    self.pinned_apps.remove(pos);
                                } else {
                                    self.pinned_apps.push(path);
                                }
                                self.save_pinned_apps();
                            }
                        }
                        if let LauncherMode::Apps = self.mode {
                            if total_items > 0 {
                                let app = &filtered_apps[self.selected_index].0;
                                let path = app.desktop_file_path.clone();
                                if self.pinned_apps.contains(&path) {
                                    if self.icon_only {
                                        if ctx.input(|i| i.modifiers.command && i.modifiers.shift && i.key_pressed(egui::Key::ArrowLeft)) {
                                            if let Some(pos) = self.pinned_apps.iter().position(|x| x == &path) {
                                                if pos > 0 {
                                                    self.pinned_apps.swap(pos, pos - 1);
                                                    self.selected_index -= 1;
                                                    self.save_pinned_apps();
                                                    scroll_to_selected = true;
                                                }
                                            }
                                        }
                                        if ctx.input(|i| i.modifiers.command && i.modifiers.shift && i.key_pressed(egui::Key::ArrowRight)) {
                                            if let Some(pos) = self.pinned_apps.iter().position(|x| x == &path) {
                                                if pos + 1 < self.pinned_apps.len() {
                                                    self.pinned_apps.swap(pos, pos + 1);
                                                    self.selected_index += 1;
                                                    self.save_pinned_apps();
                                                    scroll_to_selected = true;
                                                }
                                            }
                                        }
                                        if ctx.input(|i| i.modifiers.command && i.modifiers.shift && i.key_pressed(egui::Key::ArrowUp)) {
                                            if let Some(pos) = self.pinned_apps.iter().position(|x| x == &path) {
                                                if pos >= columns {
                                                    self.pinned_apps.swap(pos, pos - columns);
                                                    self.selected_index -= columns;
                                                    self.save_pinned_apps();
                                                    scroll_to_selected = true;
                                                }
                                            }
                                        }
                                        if ctx.input(|i| i.modifiers.command && i.modifiers.shift && i.key_pressed(egui::Key::ArrowDown)) {
                                            if let Some(pos) = self.pinned_apps.iter().position(|x| x == &path) {
                                                if pos + columns < self.pinned_apps.len() {
                                                    self.pinned_apps.swap(pos, pos + columns);
                                                    self.selected_index += columns;
                                                    self.save_pinned_apps();
                                                    scroll_to_selected = true;
                                                }
                                            }
                                        }
                                    } else {
                                        if ctx.input(|i| i.modifiers.command && i.modifiers.shift && i.key_pressed(egui::Key::ArrowUp)) {
                                            if let Some(pos) = self.pinned_apps.iter().position(|x| x == &path) {
                                                if pos > 0 {
                                                    self.pinned_apps.swap(pos, pos - 1);
                                                    self.selected_index -= 1;
                                                    self.save_pinned_apps();
                                                    scroll_to_selected = true;
                                                }
                                            }
                                        }
                                        if ctx.input(|i| i.modifiers.command && i.modifiers.shift && i.key_pressed(egui::Key::ArrowDown)) {
                                            if let Some(pos) = self.pinned_apps.iter().position(|x| x == &path) {
                                                if pos + 1 < self.pinned_apps.len() {
                                                    self.pinned_apps.swap(pos, pos + 1);
                                                    self.selected_index += 1;
                                                    self.save_pinned_apps();
                                                    scroll_to_selected = true;
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }

                    // 3. Render Items ScrollArea
                    let list_height = (ui.available_height() - 32.0).max(100.0);
                    let row_height = 52.0;

                    if total_items == 0 {
                        ui.allocate_ui(egui::vec2(ui.available_width(), list_height), |ui| {
                            ui.vertical_centered(|ui| {
                                ui.add_space(80.0);
                                if self.loading {
                                    ui.add(egui::Spinner::new().size(24.0));
                                    ui.add_space(10.0);
                                    ui.label(
                                        egui::RichText::new(match self.mode {
                                            LauncherMode::Apps => "Loading installed applications...",
                                            LauncherMode::Windows => "Loading open windows...",
                                        })
                                        .color(egui::Color32::from_rgba_unmultiplied(255, 255, 255, 120))
                                        .size(14.0),
                                    );
                                } else {
                                    ui.label(
                                        egui::RichText::new(match self.mode {
                                            LauncherMode::Apps => "No matching installed applications found",
                                            LauncherMode::Windows => "No matching open windows found",
                                        })
                                        .color(egui::Color32::from_rgba_unmultiplied(255, 255, 255, 120))
                                        .size(15.0),
                                    );
                                }
                            });
                        });
                    } else if self.icon_only && self.mode == LauncherMode::Apps {
                        let scroll_delta = ctx.input(|i| i.smooth_scroll_delta);
                        egui::ScrollArea::vertical()
                            .max_height(list_height)
                            .show(ui, |ui| {
                                if scroll_delta.y != 0.0 {
                                    ui.scroll_with_delta(egui::vec2(0.0, scroll_delta.y * 6.0));
                                }
                                
                                ui.spacing_mut().item_spacing = egui::vec2(12.0, 12.0);
                                ui.horizontal_wrapped(|ui| {
                                    for index in 0..total_items {
                                        let is_selected = index == self.selected_index;
                                        let app = &filtered_apps[index].0;

                                        let (rect, response) = ui.allocate_exact_size(
                                            egui::vec2(72.0, 72.0),
                                            egui::Sense::click(),
                                        );

                                        if response.hovered() {
                                            self.selected_index = index;
                                        }

                                        let response = response.on_hover_text(&app.name);

                                        response.clone().context_menu(|ui| {
                                            let path = app.desktop_file_path.clone();
                                            let is_pinned = self.pinned_apps.contains(&path);
                                            let label = if is_pinned { "📌 Unpin application" } else { "📌 Pin application" };
                                            if ui.button(label).clicked() {
                                                if is_pinned {
                                                    if let Some(pos) = self.pinned_apps.iter().position(|x| x == &path) {
                                                        self.pinned_apps.remove(pos);
                                                    }
                                                } else {
                                                    self.pinned_apps.push(path.clone());
                                                }
                                                self.save_pinned_apps();
                                                ui.close();
                                            }

                                            if is_pinned {
                                                if let Some(pos) = self.pinned_apps.iter().position(|x| x == &path) {
                                                    if pos >= columns {
                                                        if ui.button("⬆ Move up").clicked() {
                                                            self.pinned_apps.swap(pos, pos - columns);
                                                            self.save_pinned_apps();
                                                            ui.close();
                                                        }
                                                    }
                                                    if pos + columns < self.pinned_apps.len() {
                                                        if ui.button("⬇ Move down").clicked() {
                                                            self.pinned_apps.swap(pos, pos + columns);
                                                            self.save_pinned_apps();
                                                            ui.close();
                                                        }
                                                    }
                                                    if pos > 0 {
                                                        if ui.button("⬅ Move left").clicked() {
                                                            self.pinned_apps.swap(pos, pos - 1);
                                                            self.save_pinned_apps();
                                                            ui.close();
                                                        }
                                                    }
                                                    if pos + 1 < self.pinned_apps.len() {
                                                        if ui.button("➡ Move right").clicked() {
                                                            self.pinned_apps.swap(pos, pos + 1);
                                                            self.save_pinned_apps();
                                                            ui.close();
                                                        }
                                                    }
                                                }
                                            }
                                        });

                                        if is_selected && scroll_to_selected {
                                            response.scroll_to_me(None);
                                        }

                                        if response.clicked() {
                                            self.launch_app_and_exit(app.exec.clone(), ctx);
                                        }

                                        let bg_color = if is_selected {
                                            egui::Color32::from_rgba_unmultiplied(255, 255, 255, 18)
                                        } else {
                                            egui::Color32::from_rgba_unmultiplied(255, 255, 255, 4)
                                        };

                                        ui.painter().rect_filled(
                                            rect,
                                            egui::CornerRadius::same(12),
                                            bg_color,
                                        );

                                        if is_selected {
                                            ui.painter().rect_stroke(
                                                rect,
                                                egui::CornerRadius::same(12),
                                                egui::Stroke::new(1.5, egui::Color32::from_rgb(61, 174, 233)),
                                                egui::StrokeKind::Inside,
                                            );
                                        } else {
                                            ui.painter().rect_stroke(
                                                rect,
                                                egui::CornerRadius::same(12),
                                                egui::Stroke::new(1.0, egui::Color32::from_rgba_unmultiplied(255, 255, 255, 10)),
                                                egui::StrokeKind::Inside,
                                            );
                                        }

                                        let icon_size = egui::vec2(48.0, 48.0);
                                        let icon_rect = egui::Rect::from_center_size(rect.center(), icon_size);

                                        if let Some(ref path) = app.icon_path {
                                            let uri = format!("file://{}", path.to_string_lossy());
                                            let mut child_ui = ui.new_child(
                                                egui::UiBuilder::new()
                                                    .max_rect(icon_rect)
                                                    .layout(egui::Layout::left_to_right(egui::Align::Center)),
                                            );
                                            child_ui.add(
                                                egui::Image::new(uri).max_size(icon_size),
                                            );
                                        } else {
                                            ui.painter().rect_filled(
                                                icon_rect,
                                                egui::CornerRadius::same(8),
                                                egui::Color32::from_rgba_unmultiplied(255, 255, 255, 12),
                                            );
                                            let first_char = app.name.chars().next().unwrap_or('?').to_uppercase().to_string();
                                            ui.painter().text(
                                                icon_rect.center(),
                                                egui::Align2::CENTER_CENTER,
                                                first_char,
                                                egui::FontId::proportional(20.0),
                                                egui::Color32::from_rgba_unmultiplied(255, 255, 255, 180),
                                            );
                                        }

                                        if self.pinned_apps.contains(&app.desktop_file_path) {
                                            let badge_pos = egui::pos2(rect.max.x - 12.0, rect.min.y + 12.0);
                                            ui.painter().text(
                                                badge_pos,
                                                egui::Align2::CENTER_CENTER,
                                                "📌",
                                                egui::FontId::proportional(11.0),
                                                egui::Color32::WHITE,
                                            );
                                        }
                                    }
                                });
                            });
                    } else {
                        let scroll_delta = ctx.input(|i| i.smooth_scroll_delta);
                        egui::ScrollArea::vertical()
                            .max_height(list_height)
                            .show(ui, |ui| {
                                if scroll_delta.y != 0.0 {
                                    ui.scroll_with_delta(egui::vec2(0.0, scroll_delta.y * 6.0));
                                }
                                for index in 0..total_items {
                                    let is_selected = index == self.selected_index;

                                    let (rect, response) = ui.allocate_exact_size(
                                        egui::vec2(ui.available_width(), row_height),
                                        egui::Sense::click(),
                                    );

                                    response.clone().context_menu(|ui| {
                                        if let LauncherMode::Apps = self.mode {
                                            let app = &filtered_apps[index].0;
                                            let path = app.desktop_file_path.clone();
                                            let is_pinned = self.pinned_apps.contains(&path);
                                            let label = if is_pinned { "📌 Unpin application" } else { "📌 Pin application" };
                                            if ui.button(label).clicked() {
                                                if is_pinned {
                                                    if let Some(pos) = self.pinned_apps.iter().position(|x| x == &path) {
                                                        self.pinned_apps.remove(pos);
                                                    }
                                                } else {
                                                    self.pinned_apps.push(path.clone());
                                                }
                                                self.save_pinned_apps();
                                                ui.close();
                                            }

                                            if is_pinned {
                                                if let Some(pos) = self.pinned_apps.iter().position(|x| x == &path) {
                                                    if pos > 0 {
                                                        if ui.button("⬆ Move up").clicked() {
                                                            self.pinned_apps.swap(pos, pos - 1);
                                                            self.save_pinned_apps();
                                                            ui.close();
                                                        }
                                                    }
                                                    if pos + 1 < self.pinned_apps.len() {
                                                        if ui.button("⬇ Move down").clicked() {
                                                            self.pinned_apps.swap(pos, pos + 1);
                                                            self.save_pinned_apps();
                                                            ui.close();
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    });

                                     if is_selected && scroll_to_selected {
                                         response.scroll_to_me(None);
                                     }

                                    // Selection highlights
                                    if response.hovered() {
                                        self.selected_index = index;
                                    }

                                    if response.clicked() {
                                        match self.mode {
                                            LauncherMode::Apps => {
                                                let app = &filtered_apps[index].0;
                                                self.launch_app_and_exit(app.exec.clone(), ctx);
                                            }
                                            LauncherMode::Windows => {
                                                let win = &filtered_windows[index].0;
                                                self.activate_and_exit(win.id.clone(), ctx);
                                            }
                                        }
                                    }

                                    let bg_color = if is_selected {
                                        egui::Color32::from_rgba_unmultiplied(255, 255, 255, 18)
                                    } else {
                                        egui::Color32::TRANSPARENT
                                    };

                                    ui.painter().rect_filled(
                                        rect,
                                        egui::CornerRadius::same(8),
                                        bg_color,
                                    );

                                    // Premium left accent highlight bar
                                    if is_selected {
                                        let accent_rect = egui::Rect::from_min_size(
                                            egui::pos2(rect.min.x + 2.0, rect.min.y + (rect.height() - 24.0) / 2.0),
                                            egui::vec2(3.0, 24.0),
                                        );
                                        ui.painter().rect_filled(
                                            accent_rect,
                                            egui::CornerRadius::same(2),
                                            egui::Color32::from_rgb(61, 174, 233), // KDE blue theme accent
                                        );
                                    }

                                    // Content placement
                                    let content_rect = rect.shrink2(egui::vec2(12.0, 6.0));
                                    let mut child_ui = ui.new_child(
                                        egui::UiBuilder::new()
                                            .max_rect(content_rect)
                                            .layout(egui::Layout::left_to_right(egui::Align::Center)),
                                    );

                                    match self.mode {
                                        LauncherMode::Apps => {
                                            let app = &filtered_apps[index].0;

                                            // Icon render
                                            if let Some(ref path) = app.icon_path {
                                                let uri = format!("file://{}", path.to_string_lossy());
                                                child_ui.add(
                                                    egui::Image::new(uri).max_size(egui::vec2(32.0, 32.0)),
                                                );
                                            } else {
                                                let (icon_rect, _) = child_ui.allocate_exact_size(
                                                    egui::vec2(32.0, 32.0),
                                                    egui::Sense::hover(),
                                                );
                                                child_ui.painter().rect_filled(
                                                    icon_rect,
                                                    egui::CornerRadius::same(6),
                                                    egui::Color32::from_rgba_unmultiplied(255, 255, 255, 12),
                                                );
                                                let first_char = app.name.chars().next().unwrap_or('?').to_uppercase().to_string();
                                                child_ui.painter().text(
                                                    icon_rect.center(),
                                                    egui::Align2::CENTER_CENTER,
                                                    first_char,
                                                    egui::FontId::proportional(15.0),
                                                    egui::Color32::from_rgba_unmultiplied(255, 255, 255, 160),
                                                );
                                            }

                                            child_ui.add_space(10.0);

                                            child_ui.vertical(|ui| {
                                                ui.horizontal(|ui| {
                                                    ui.add(egui::Label::new(
                                                        egui::RichText::new(&app.name)
                                                            .color(egui::Color32::WHITE)
                                                            .strong()
                                                            .size(13.0),
                                                    ));
                                                    if self.pinned_apps.contains(&app.desktop_file_path) {
                                                        ui.add_space(4.0);
                                                        ui.label(
                                                            egui::RichText::new("📌")
                                                                .size(11.0)
                                                                .color(egui::Color32::from_rgb(61, 174, 233)),
                                                        );
                                                    }
                                                });
                                                let is_link = std::fs::symlink_metadata(&app.desktop_file_path)
                                                    .map(|m| m.file_type().is_symlink())
                                                    .unwrap_or(false);
                                                let mut subtext = app.desktop_file_path.to_string_lossy().to_string();
                                                if is_link {
                                                    subtext.push('@');
                                                }
                                                let display_subtext = if subtext.len() > 80 {
                                                    format!("{}...", &subtext[..77])
                                                } else {
                                                    subtext
                                                };
                                                ui.add(egui::Label::new(
                                                    egui::RichText::new(display_subtext)
                                                        .color(egui::Color32::from_rgba_unmultiplied(255, 255, 255, 130))
                                                        .size(10.5),
                                                ));
                                            });
                                        }
                                        LauncherMode::Windows => {
                                            let win = &filtered_windows[index].0;

                                            // Icon render
                                            if let Some(ref path) = win.icon_path {
                                                let uri = format!("file://{}", path.to_string_lossy());
                                                child_ui.add(
                                                    egui::Image::new(uri).max_size(egui::vec2(32.0, 32.0)),
                                                );
                                            } else {
                                                let (icon_rect, _) = child_ui.allocate_exact_size(
                                                    egui::vec2(32.0, 32.0),
                                                    egui::Sense::hover(),
                                                );
                                                child_ui.painter().rect_filled(
                                                    icon_rect,
                                                    egui::CornerRadius::same(6),
                                                    egui::Color32::from_rgba_unmultiplied(255, 255, 255, 12),
                                                );

                                                let first_char = win.class
                                                    .chars()
                                                    .next()
                                                    .unwrap_or('?')
                                                    .to_uppercase()
                                                    .to_string();

                                                child_ui.painter().text(
                                                    icon_rect.center(),
                                                    egui::Align2::CENTER_CENTER,
                                                    first_char,
                                                    egui::FontId::proportional(15.0),
                                                    egui::Color32::from_rgba_unmultiplied(255, 255, 255, 160),
                                                );
                                            }

                                            child_ui.add_space(10.0);

                                            child_ui.vertical(|ui| {
                                                let display_title = if win.title.len() > 65 {
                                                    format!("{}...", &win.title[..62])
                                                } else {
                                                    win.title.clone()
                                                };

                                                ui.add(egui::Label::new(
                                                    egui::RichText::new(display_title)
                                                        .color(egui::Color32::WHITE)
                                                        .strong()
                                                        .size(13.0),
                                                ));

                                                let subtext = if let Some(ref path) = win.exe_path {
                                                    let is_link = std::fs::symlink_metadata(path)
                                                        .map(|m| m.file_type().is_symlink())
                                                        .unwrap_or(false);
                                                    let mut path_str = path.to_string_lossy().to_string();
                                                    if is_link {
                                                        path_str.push('@');
                                                    }
                                                    path_str
                                                } else if let Some(ref proc_name) = win.active_process {
                                                    format!("{} (running: {})", win.class, proc_name)
                                                } else {
                                                    win.class.clone()
                                                };
                                                ui.add(egui::Label::new(
                                                    egui::RichText::new(subtext)
                                                        .color(egui::Color32::from_rgba_unmultiplied(255, 255, 255, 130))
                                                        .size(10.5),
                                                ));
                                            });
                                        }
                                    }
                                }
                            });
                    }

                    ui.add_space(8.0);

                    // 4. Subtle shortcut menu footer
                    ui.separator();
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let label_text = match self.mode {
                                LauncherMode::Apps => format!("{} apps", total_items),
                                LauncherMode::Windows => format!("{} windows", total_items),
                            };
                            ui.label(
                                egui::RichText::new(label_text)
                                    .color(egui::Color32::from_rgba_unmultiplied(255, 255, 255, 100))
                                    .size(9.5),
                            );
                        });
                    });

                    // Custom drag-resize handle at the bottom-right corner
                    let resize_handle_size = egui::vec2(16.0, 16.0);
                    let resize_rect = egui::Rect::from_min_size(
                        ui.max_rect().max - resize_handle_size,
                        resize_handle_size,
                    );
                    
                    let resize_response = ui.allocate_rect(resize_rect, egui::Sense::drag());
                    
                    // Draw visual resize handle (diagonal grip lines)
                    let color = egui::Color32::from_rgba_unmultiplied(255, 255, 255, 100);
                    let br = resize_rect.max - egui::vec2(2.0, 2.0);
                    for i in 0..3 {
                        let offset = i as f32 * 4.0;
                        ui.painter().line_segment(
                            [
                                br - egui::vec2(offset + 4.0, 0.0),
                                br - egui::vec2(0.0, offset + 4.0),
                            ],
                            egui::Stroke::new(1.0, color),
                        );
                    }

                    if resize_response.hovered() {
                        ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeSouthEast);
                    }

                    if resize_response.drag_started() {
                        ui.ctx().send_viewport_cmd(egui::ViewportCommand::BeginResize(
                            egui::ResizeDirection::SouthEast,
                        ));
                    }

                    if self.show_settings_menu {
                        let area = egui::Area::new(egui::Id::new("settings_overlay"))
                            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
                            .order(egui::Order::Foreground);
                        
                        area.show(ctx, |ui| {
                            let frame = egui::Frame::window(&ui.style())
                                .fill(egui::Color32::from_rgba_unmultiplied(20, 20, 20, 240))
                                .stroke(egui::Stroke::new(1.0, egui::Color32::from_rgba_unmultiplied(255, 255, 255, 20)))
                                .corner_radius(egui::CornerRadius::same(12));
                            
                            frame.show(ui, |ui| {
                                ui.set_width(320.0);
                                ui.vertical(|ui| {
                                    ui.add_space(8.0);
                                    ui.vertical_centered(|ui| {
                                        ui.heading(
                                            egui::RichText::new("Launcher Settings")
                                                .color(egui::Color32::WHITE)
                                                .strong(),
                                        );
                                    });
                                    ui.add_space(8.0);
                                    
                                    ui.add(egui::Separator::default());
                                    ui.add_space(12.0);

                                    ui.horizontal(|ui| {
                                        let mut show_val = self.show_system_settings_modules;
                                        let checkbox_response = ui.checkbox(
                                            &mut show_val,
                                            egui::RichText::new("Show system settings modules (KCM)")
                                                .color(egui::Color32::from_rgba_unmultiplied(255, 255, 255, 220))
                                                .size(13.0),
                                        );
                                        if checkbox_response.changed() {
                                            self.show_system_settings_modules = show_val;
                                            save_show_system_settings_modules(show_val);
                                        }
                                    });
                                    
                                    ui.add_space(16.0);
                                    ui.vertical_centered(|ui| {
                                        if ui.add(
                                            egui::Button::new(
                                                egui::RichText::new("Close Settings (F10)")
                                                    .color(egui::Color32::WHITE)
                                                    .size(13.0)
                                            )
                                            .fill(egui::Color32::from_rgba_unmultiplied(61, 174, 233, 200))
                                        ).clicked() {
                                            self.show_settings_menu = false;
                                        }
                                    });
                                    ui.add_space(8.0);
                                });
                            });
                        });
                    }
                });
            });
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.save_window_size();
    }
}

struct SingleInstanceLock {
    _listener: std::os::unix::net::UnixListener,
    path: PathBuf,
}

impl Drop for SingleInstanceLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn get_socket_path(mode: LauncherMode) -> PathBuf {
    let filename = match mode {
        LauncherMode::Apps => "applicationlauncher-apps.sock",
        LauncherMode::Windows => "applicationlauncher-windows.sock",
    };
    if let Ok(runtime_dir) = std::env::var("XDG_RUNTIME_DIR") {
        PathBuf::from(runtime_dir).join(filename)
    } else {
        std::env::temp_dir().join(filename)
    }
}

struct BorderOverlay {
    start_time: Instant,
    duration: std::time::Duration,
    local_x: f32,
    local_y: f32,
    tw: f32,
    th: f32,
}

impl eframe::App for BorderOverlay {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0, 0.0, 0.0, 0.0]
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if self.start_time.elapsed() >= self.duration {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }
        ctx.request_repaint();
        
        let panel_frame = egui::Frame {
            fill: egui::Color32::TRANSPARENT,
            ..Default::default()
        };
        
        egui::CentralPanel::default()
            .frame(panel_frame)
            .show(ctx, |ui| {
                let elapsed = self.start_time.elapsed().as_secs_f32();
                let total_secs = self.duration.as_secs_f32();
                let progress = (elapsed / total_secs).clamp(0.0, 1.0);
                let alpha = ((1.0 - progress) * 255.0) as u8;
                
                let rect = egui::Rect::from_min_size(
                    egui::pos2(self.local_x, self.local_y),
                    egui::vec2(self.tw, self.th),
                );
                
                ui.painter().rect_stroke(
                    rect,
                    egui::CornerRadius::same(6),
                    egui::Stroke::new(3.0, egui::Color32::from_rgba_unmultiplied(235, 40, 40, alpha)),
                    egui::StrokeKind::Inside,
                );
            });
    }
}

struct MonitorInfo {
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    scale: f32,
}

fn get_monitors() -> Vec<MonitorInfo> {
    let mut monitors = Vec::new();
    let output = match Command::new("kscreen-doctor").arg("-j").output() {
        Ok(o) => o,
        Err(_) => return monitors,
    };
    let stdout = String::from_utf8_lossy(&output.stdout);
    
    let blocks: Vec<&str> = stdout.split("\"name\":").collect();
    if blocks.len() <= 1 {
        return monitors;
    }
    
    for block in &blocks[1..] {
        let mut x = None;
        let mut y = None;
        let mut width = None;
        let mut height = None;
        let mut scale = Some(1.0);
        
        if let Some(pos_idx) = block.find("\"pos\":") {
            let pos_str = &block[pos_idx..];
            if let Some(brace_open) = pos_str.find('{') {
                if let Some(brace_close) = pos_str.find('}') {
                    let pos_content = &pos_str[brace_open + 1..brace_close];
                    for line in pos_content.lines() {
                        let line = line.trim();
                        if line.starts_with("\"x\":") {
                            x = line.strip_prefix("\"x\":")
                                .map(|s| s.trim_matches(|c| c == ',' || c == ' ' || c == '\n'))
                                .and_then(|s| s.parse::<f32>().ok());
                        } else if line.starts_with("\"y\":") {
                            y = line.strip_prefix("\"y\":")
                                .map(|s| s.trim_matches(|c| c == ',' || c == ' ' || c == '\n'))
                                .and_then(|s| s.parse::<f32>().ok());
                        }
                    }
                }
            }
        }
        
        if let Some(size_idx) = block.find("\"size\":") {
            let size_str = &block[size_idx..];
            if let Some(brace_open) = size_str.find('{') {
                if let Some(brace_close) = size_str.find('}') {
                    let size_content = &size_str[brace_open + 1..brace_close];
                    for line in size_content.lines() {
                        let line = line.trim();
                        if line.starts_with("\"width\":") {
                            width = line.strip_prefix("\"width\":")
                                .map(|s| s.trim_matches(|c| c == ',' || c == ' ' || c == '\n'))
                                .and_then(|s| s.parse::<f32>().ok());
                        } else if line.starts_with("\"height\":") {
                            height = line.strip_prefix("\"height\":")
                                .map(|s| s.trim_matches(|c| c == ',' || c == ' ' || c == '\n'))
                                .and_then(|s| s.parse::<f32>().ok());
                        }
                    }
                }
            }
        }
        
        for line in block.lines() {
            let line = line.trim();
            if line.starts_with("\"scale\":") {
                if let Some(s_val) = line.strip_prefix("\"scale\":")
                    .map(|s| s.trim_matches(|c| c == ',' || c == ' ' || c == '\n'))
                    .and_then(|s| s.parse::<f32>().ok()) {
                    scale = Some(s_val);
                }
            }
        }
        
        if let (Some(x), Some(y), Some(width), Some(height), Some(scale)) = (x, y, width, height, scale) {
            monitors.push(MonitorInfo { x, y, width, height, scale });
        }
    }
    monitors
}

fn main() -> eframe::Result {
    let args: Vec<String> = std::env::args().collect();
    
    if args.len() >= 7 && args[1] == "--draw-border" {
        let tx: f32 = args[2].parse().unwrap_or(0.0);
        let ty: f32 = args[3].parse().unwrap_or(0.0);
        let tw: f32 = args[4].parse().unwrap_or(100.0);
        let th: f32 = args[5].parse().unwrap_or(100.0);
        
        // Find which monitor contains the target window center
        let target_center_x = tx + tw / 2.0;
        let target_center_y = ty + th / 2.0;
        
        let mut mx = 0.0;
        let mut my = 0.0;
        
        for monitor in get_monitors() {
            let logical_w = monitor.width / monitor.scale;
            let logical_h = monitor.height / monitor.scale;
            if target_center_x >= monitor.x && target_center_x <= monitor.x + logical_w
               && target_center_y >= monitor.y && target_center_y <= monitor.y + logical_h {
                mx = monitor.x;
                my = monitor.y;
                break;
            }
        }
        
        let local_x = tx - mx;
        let local_y = ty - my;
        
        let options = eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default()
                .with_title("Border Overlay")
                .with_decorations(false)
                .with_transparent(true)
                .with_always_on_top()
                .with_fullscreen(true)
                .with_mouse_passthrough(true),
            ..Default::default()
        };
        
        let _ = eframe::run_native(
            "Border Overlay",
            options,
            Box::new(move |_cc| {
                Ok(Box::new(BorderOverlay {
                    start_time: Instant::now(),
                    duration: std::time::Duration::from_millis(250),
                    local_x,
                    local_y,
                    tw,
                    th,
                }))
            }),
        );
        return Ok(());
    }
    
    // Pre-parse mode to determine which socket path to use
    let mut mode = LauncherMode::Windows;
    for arg in &args {
        if arg == "-a" || arg == "--apps" {
            mode = LauncherMode::Apps;
            break;
        }
    }

    // Single instance check using Unix domain socket (mode-specific)
    let socket_path = get_socket_path(mode);
    if socket_path.exists() {
        if std::os::unix::net::UnixStream::connect(&socket_path).is_ok() {
            // Another instance in the same mode is already running
            return Ok(());
        }
        let _ = std::fs::remove_file(&socket_path);
    }

    let listener = match std::os::unix::net::UnixListener::bind(&socket_path) {
        Ok(l) => l,
        Err(_) => return Ok(()),
    };

    let _lock = SingleInstanceLock {
        _listener: listener,
        path: socket_path,
    };

    let mut close_on_blur = false;
    let mut force_theme = None;
    let mut icon_only = false;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "-h" | "--help" => {
                print_help();
                return Ok(());
            }
            "-a" | "--apps" => {
                // Already processed in pre-parse step
                i += 1;
            }
            "-i" | "--icon-only" => {
                icon_only = true;
                i += 1;
            }
            "--close-on-blur" => {
                close_on_blur = true;
                i += 1;
            }
            "--theme" => {
                if i + 1 < args.len() {
                    force_theme = Some(args[i + 1].clone());
                    i += 2;
                } else {
                    eprintln!("Error: --theme requires a value");
                    std::process::exit(1);
                }
            }
            _ => {
                eprintln!("Error: Unknown argument: {}", args[i]);
                print_help();
                std::process::exit(1);
            }
        }
    }

    let (width, height) = load_window_size();

    let title = match mode {
        LauncherMode::Apps => "Search Applications",
        LauncherMode::Windows => "Open Application Windows",
    };

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(title)
            .with_decorations(false)
            .with_transparent(true)
            .with_always_on_top()
            .with_inner_size([width, height])
            .with_resizable(true),
        ..Default::default()
    };

    eframe::run_native(
        title,
        options,
        Box::new(move |cc| {
            Ok(Box::new(App::new(cc, close_on_blur, force_theme, mode, icon_only)))
        }),
    )
}
