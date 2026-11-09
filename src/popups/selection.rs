use std::sync::{Arc, Mutex, OnceLock};

use eframe::egui;

use super::InfoText;

#[derive(Clone, Debug, PartialEq, Eq)]
struct Selection {
    plain: String,
    html: String,
}

#[derive(Clone, Default)]
struct CopyMenu {
    selection: Option<Arc<Selection>>,
    error: Option<String>,
}

impl InfoText {
    pub(super) fn preserve_secondary_selection(
        ui: &egui::Ui,
        editor: &mut egui::text_edit::TextEditOutput,
        before: Option<egui::text::CCursorRange>,
    ) {
        if editor.response.contains_pointer()
            && ui.input(|input| {
                input.pointer.button_pressed(egui::PointerButton::Secondary)
                    || input
                        .pointer
                        .button_released(egui::PointerButton::Secondary)
            })
            && let Some(range) = before.filter(|range| !range.is_empty())
        {
            // TextEdit treats any pressed button as a new selection; keep right-click non-destructive.
            editor.state.cursor.set_char_range(Some(range));
            editor.state.clone().store(ui.ctx(), editor.response.id);
            editor.cursor_range = Some(range);
            ui.ctx().request_repaint();
        }
    }

    fn selection(&self, cursors: egui::text::CCursorRange) -> Option<Selection> {
        let range = cursors.as_sorted_char_range();
        if range.is_empty() {
            return None;
        }
        let mut boundaries = self
            .job
            .text
            .char_indices()
            .map(|(byte, _)| byte)
            .chain(std::iter::once(self.job.text.len()));
        let start = boundaries.nth(range.start)?;
        let end = boundaries.nth(range.end - range.start - 1)?;
        let plain = if let Some(raw) = &self.raw {
            raw.get(*self.raw_boundaries.get(range.start)?..*self.raw_boundaries.get(range.end)?)?
                .to_owned()
        } else {
            self.job.text.get(start..end)?.to_owned()
        };
        let mut html = String::from(
            "<pre style=\"white-space:pre;font-family:monospace;background-color:#141414;color:#ffffff\">",
        );
        for section in &self.job.sections {
            let left = start.max(section.byte_range.start);
            let right = end.min(section.byte_range.end);
            if left >= right {
                continue;
            }
            let format = &section.format;
            let colour = format.color;
            let family = if format.font_id.family == egui::FontFamily::Monospace {
                "monospace"
            } else {
                "sans-serif"
            };
            let size = if format.font_id.size.is_finite() {
                format.font_id.size.clamp(4.0, 256.0)
            } else {
                13.0
            };
            html.push_str(&format!(
                "<span style=\"color:#{:02x}{:02x}{:02x};font-family:{family};font-size:{size}px",
                colour.r(),
                colour.g(),
                colour.b()
            ));
            if format.italics {
                html.push_str(";font-style:italic");
            }
            if format.underline.width > 0.0 || format.strikethrough.width > 0.0 {
                html.push_str(";text-decoration-line:");
                if format.underline.width > 0.0 {
                    html.push_str("underline ");
                }
                if format.strikethrough.width > 0.0 {
                    html.push_str("line-through");
                }
            }
            html.push_str("\">");
            escape_html(&mut html, &self.job.text[left..right]);
            html.push_str("</span>");
        }
        html.push_str("</pre>");
        Some(Selection { plain, html })
    }

    pub(super) fn show_copy_menu(
        &self,
        ui: &mut egui::Ui,
        editor: &egui::text_edit::TextEditOutput,
    ) {
        self.copy_menu_with(ui, editor, write_html);
    }

    fn copy_menu_with(
        &self,
        ui: &mut egui::Ui,
        editor: &egui::text_edit::TextEditOutput,
        mut copy_html: impl FnMut(&Selection) -> Result<(), String>,
    ) {
        let response = &editor.response;
        if !response.secondary_clicked() && !response.context_menu_opened() {
            return;
        }
        let id = response.id.with("info-copy-selection");
        let mut menu = if response.secondary_clicked() {
            CopyMenu {
                selection: editor
                    .state
                    .cursor
                    .char_range()
                    .and_then(|range| self.selection(range))
                    .map(Arc::new),
                error: None,
            }
        } else {
            ui.ctx()
                .data_mut(|data| data.get_temp::<CopyMenu>(id))
                .unwrap_or_default()
        };
        // Snapshot the selection on opening: live metadata must not retarget an open menu.
        egui::Popup::context_menu(response)
            .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
            .show(|ui| {
                if ui
                    .add_enabled(menu.selection.is_some(), egui::Button::new("Copy"))
                    .clicked()
                {
                    ui.ctx()
                        .copy_text(menu.selection.as_ref().unwrap().plain.clone());
                    ui.close();
                }
                if ui
                    .add_enabled(menu.selection.is_some(), egui::Button::new("Copy HTML"))
                    .clicked()
                {
                    match copy_html(menu.selection.as_ref().unwrap()) {
                        Ok(()) => {
                            ui.close();
                        }
                        Err(error) => {
                            menu.error = Some(format!("Could not copy HTML: {error}"));
                        }
                    }
                }
                if let Some(error) = &menu.error {
                    ui.colored_label(egui::Color32::LIGHT_RED, error);
                }
            });
        let menu_open = response.context_menu_opened();
        ui.ctx().data_mut(|data| {
            if menu_open {
                data.insert_temp(id, menu);
            } else {
                data.remove::<CopyMenu>(id);
            }
        });
    }
}

fn escape_html(output: &mut String, text: &str) {
    for character in text.chars() {
        match character {
            '&' => output.push_str("&amp;"),
            '<' => output.push_str("&lt;"),
            '>' => output.push_str("&gt;"),
            '"' => output.push_str("&quot;"),
            '\'' => output.push_str("&#39;"),
            _ => output.push(character),
        }
    }
}

fn write_html(selection: &Selection) -> Result<(), String> {
    // X11 clipboard ownership must outlive the click handler. Wayland offers both MIME types.
    static CLIPBOARD: OnceLock<Mutex<Option<arboard::Clipboard>>> = OnceLock::new();
    let mut clipboard = CLIPBOARD
        .get_or_init(|| Mutex::new(None))
        .lock()
        .map_err(|error| error.to_string())?;
    if clipboard.is_none() {
        *clipboard = Some(arboard::Clipboard::new().map_err(|error| error.to_string())?);
    }
    let result = clipboard
        .as_mut()
        .unwrap()
        .set_html(selection.html.clone(), Some(selection.plain.clone()))
        .map_err(|error| error.to_string());
    if result.is_err() {
        *clipboard = None;
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn range(start: usize, end: usize) -> egui::text::CCursorRange {
        egui::text::CCursorRange::two(
            egui::text::CCursor::new(start),
            egui::text::CCursor::new(end),
        )
    }

    fn text(value: &str) -> InfoText {
        InfoText::new(egui::text::LayoutJob::simple(
            value.into(),
            egui::FontId::monospace(13.0),
            egui::Color32::WHITE,
            f32::INFINITY,
        ))
    }

    #[test]
    fn html_contains_only_the_selected_unicode_text_and_escapes_active_content() {
        let selected = "\u{03c0} <script>&\"'\n  second line";
        let document = text(&format!("before {selected} after"));
        let selection = document
            .selection(range(7, 7 + selected.chars().count()))
            .unwrap();
        assert_eq!(selection.plain, selected);
        assert!(
            selection
                .html
                .contains("\u{03c0} &lt;script&gt;&amp;&quot;&#39;\n  second line")
        );
        assert!(!selection.html.contains("before") && !selection.html.contains("after"));
        assert!(!selection.html.contains("<script>"));
        assert_eq!(
            document.selection(range(7 + selected.chars().count(), 7)),
            Some(selection)
        );
        assert!(document.selection(range(2, 2)).is_none());
        assert!(document.selection(range(0, 10_000)).is_none());
    }

    #[test]
    fn html_preserves_colours_and_strikes_while_plain_copy_keeps_the_raw_marks() {
        let struck: String = "codex".chars().flat_map(|ch| [ch, '\u{0336}']).collect();
        let mut job = egui::text::LayoutJob::default();
        job.append(
            &struck,
            0.0,
            egui::TextFormat::simple(
                egui::FontId::monospace(13.0),
                egui::Color32::from_rgb(130, 225, 145),
            ),
        );
        job.append(
            " - old",
            0.0,
            egui::TextFormat::simple(
                egui::FontId::monospace(13.0),
                egui::Color32::from_rgb(255, 110, 110),
            ),
        );
        let document = InfoText::new(job);
        let selection = document.selection(range(1, 9)).unwrap();
        assert_eq!(
            selection.plain,
            format!("{} - o", struck.chars().skip(2).collect::<String>())
        );
        assert!(
            selection.html.contains("color:#82e191") && selection.html.contains("color:#ff6e6e")
        );
        assert!(selection.html.contains("text-decoration-line:line-through"));
        assert!(selection.html.contains(">odex</span>"));
        assert!(!selection.html.contains('\u{0336}'));
    }

    fn frame(
        ctx: &egui::Context,
        document: &InfoText,
        singleline: bool,
        events: Vec<egui::Event>,
        copy_html: &mut impl FnMut(&Selection) -> Result<(), String>,
    ) -> (egui::FullOutput, egui::text_edit::TextEditOutput) {
        let mut editor = None;
        let output = ctx.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1000.0, 800.0),
                )),
                events,
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let mut value = document.job.text.as_str();
                    let mut layouter = |ui: &egui::Ui, _: &dyn egui::TextBuffer, _: f32| {
                        ui.fonts_mut(|fonts| fonts.layout_job(document.job.clone()))
                    };
                    let edit = if singleline {
                        egui::TextEdit::singleline(&mut value)
                    } else {
                        egui::TextEdit::multiline(&mut value).desired_rows(5)
                    };
                    let id = egui::Id::new("copy-menu-fixture");
                    let before = egui::text_edit::TextEditState::load(ui.ctx(), id)
                        .and_then(|state| state.cursor.char_range());
                    let mut edit = edit.id(id).layouter(&mut layouter).show(ui);
                    InfoText::preserve_secondary_selection(ui, &mut edit, before);
                    document.copy_menu_with(ui, &edit, &mut *copy_html);
                    editor = Some(edit);
                });
            },
        );
        (output, editor.unwrap())
    }

    fn click(point: egui::Pos2, button: egui::PointerButton) -> Vec<egui::Event> {
        vec![
            egui::Event::PointerMoved(point),
            egui::Event::PointerButton {
                pos: point,
                button,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            },
            egui::Event::PointerButton {
                pos: point,
                button,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            },
        ]
    }

    fn button(output: &egui::FullOutput, label: &str) -> egui::Pos2 {
        output
            .shapes
            .iter()
            .find_map(|shape| match &shape.shape {
                egui::Shape::Text(text) if text.galley.job.text == label => {
                    Some(text.pos + text.galley.size() * 0.5)
                }
                _ => None,
            })
            .unwrap_or_else(|| panic!("Missing copy-menu button: {label}"))
    }

    #[test]
    fn right_click_preserves_selection_and_copy_actions_in_headers_and_details() {
        for singleline in [true, false] {
            for html in [false, true] {
                let ctx = egui::Context::default();
                let document = text("fish (pid 456416) [tmux pane]");
                let copied = std::cell::RefCell::new(None);
                let mut sink = |selection: &Selection| {
                    copied.replace(Some(selection.clone()));
                    Ok(())
                };
                let (_, editor) = frame(&ctx, &document, singleline, vec![], &mut sink);
                let mut state = editor.state;
                state.cursor.set_char_range(Some(range(0, 4)));
                state.store(&ctx, editor.response.id);
                ctx.memory_mut(|memory| memory.request_focus(editor.response.id));
                let mut events = click(
                    editor.galley_pos + egui::vec2(10.0, 7.0),
                    egui::PointerButton::Secondary,
                );
                let release = events.pop().unwrap();
                frame(&ctx, &document, singleline, events, &mut sink);
                frame(&ctx, &document, singleline, vec![release], &mut sink);
                let (menu, editor) = frame(&ctx, &document, singleline, vec![], &mut sink);
                assert_eq!(
                    editor
                        .state
                        .cursor
                        .char_range()
                        .unwrap()
                        .as_sorted_char_range(),
                    0..4
                );
                button(&menu, "Copy");
                button(&menu, "Copy HTML");
                // A metadata refresh while the menu is open cannot change what will be copied.
                let changed = text("replacement metadata");
                let target = button(&menu, if html { "Copy HTML" } else { "Copy" });
                let (output, _) = frame(
                    &ctx,
                    &changed,
                    singleline,
                    click(target, egui::PointerButton::Primary),
                    &mut sink,
                );
                let plain: Vec<_> = output
                    .platform_output
                    .commands
                    .iter()
                    .filter_map(|command| match command {
                        egui::OutputCommand::CopyText(value) => Some(value.as_str()),
                        _ => None,
                    })
                    .collect();
                if html {
                    assert_eq!(copied.borrow().as_ref().unwrap().plain, "fish");
                    assert!(
                        copied
                            .borrow()
                            .as_ref()
                            .unwrap()
                            .html
                            .contains(">fish</span>")
                    );
                    assert!(
                        plain.is_empty(),
                        "A plain copy would overwrite the HTML MIME selection"
                    );
                } else {
                    assert_eq!(plain, ["fish"]);
                    assert!(copied.borrow().is_none());
                }
            }
        }
    }

    #[test]
    fn empty_selection_does_not_copy_and_clipboard_failures_are_visible() {
        for selected in [false, true] {
            let ctx = egui::Context::default();
            let document = text("fish");
            let count = std::cell::Cell::new(0);
            let mut sink = |_: &Selection| {
                count.set(count.get() + 1);
                Err("fixture failure".into())
            };
            let (_, editor) = frame(&ctx, &document, true, vec![], &mut sink);
            let mut state = editor.state;
            state
                .cursor
                .set_char_range(Some(range(0, if selected { 4 } else { 0 })));
            state.store(&ctx, editor.response.id);
            frame(
                &ctx,
                &document,
                true,
                click(
                    editor.galley_pos + egui::vec2(10.0, 7.0),
                    egui::PointerButton::Secondary,
                ),
                &mut sink,
            );
            let (menu, _) = frame(&ctx, &document, true, vec![], &mut sink);
            let target = button(&menu, "Copy HTML");
            let (output, editor) = frame(
                &ctx,
                &document,
                true,
                click(target, egui::PointerButton::Primary),
                &mut sink,
            );
            assert_eq!(count.get(), usize::from(selected));
            assert!(
                !output
                    .platform_output
                    .commands
                    .iter()
                    .any(|command| matches!(command, egui::OutputCommand::CopyText(_)))
            );
            if selected {
                assert!(editor.response.context_menu_opened());
                let (output, _) = frame(&ctx, &document, true, vec![], &mut sink);
                button(&output, "Could not copy HTML: fixture failure");
            }
        }
    }

    fn private_clipboard_roundtrip(mode: &str, test: &str) {
        const PRIVATE: &str = "APPLICATIONLAUNCHER_CLIPBOARD_TEST_PRIVATE";
        if std::env::var(PRIVATE).as_deref() != Ok(mode) {
            struct PrivateDirectory(std::path::PathBuf);
            impl Drop for PrivateDirectory {
                fn drop(&mut self) {
                    let _ = std::fs::remove_dir_all(&self.0);
                }
            }
            let root = PrivateDirectory(std::env::temp_dir().join(format!(
                    "launcher-info-clipboard-{}-{}",
                    std::process::id(),
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap()
                        .as_nanos()
                )));
            std::fs::create_dir(&root.0).unwrap();
            let mut command = std::process::Command::new("dbus-run-session");
            command.arg("--");
            if mode == "x11" {
                command.args(["xvfb-run", "-a"]);
            }
            command.arg(std::env::current_exe().unwrap()).args([
                "--exact",
                &format!("popups::selection::tests::{test}"),
                "--ignored",
                "--nocapture",
                "--test-threads=1",
            ]);
            for name in [
                "DISPLAY",
                "WAYLAND_DISPLAY",
                "DBUS_SESSION_BUS_ADDRESS",
                "TMUX",
                "SESSION_MANAGER",
                "XDG_ACTIVATION_TOKEN",
                "DESKTOP_STARTUP_ID",
            ] {
                command.env_remove(name);
            }
            for (variable, name) in [
                ("HOME", "home"),
                ("XDG_RUNTIME_DIR", "runtime"),
                ("XDG_CONFIG_HOME", "config"),
                ("XDG_CACHE_HOME", "cache"),
                ("XDG_DATA_HOME", "data"),
                ("XDG_STATE_HOME", "state"),
            ] {
                use std::os::unix::fs::DirBuilderExt;
                let directory = root.0.join(name);
                std::fs::DirBuilder::new()
                    .mode(0o700)
                    .create(&directory)
                    .unwrap();
                command.env(variable, directory);
            }
            command.env(PRIVATE, mode);
            if mode == "wayland" {
                command
                    .env("WAYLAND_DISPLAY", "info-clipboard-test")
                    .env("QT_QPA_PLATFORM", "offscreen")
                    .env("KWIN_COMPOSE", "Q")
                    .env("LIBGL_ALWAYS_SOFTWARE", "1")
                    .env(
                        "__EGL_VENDOR_LIBRARY_FILENAMES",
                        "/usr/share/glvnd/egl_vendor.d/50_mesa.json",
                    );
            }
            let result = applicationlauncher::process::output_with_timeout(
                command,
                std::time::Duration::from_secs(30),
            )
            .unwrap();
            assert!(
                result.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&result.stdout),
                String::from_utf8_lossy(&result.stderr)
            );
            return;
        }
        struct Compositor(Option<std::process::Child>);
        impl Drop for Compositor {
            fn drop(&mut self) {
                if let Some(child) = &mut self.0 {
                    let _ = child.kill();
                    let _ = child.wait();
                }
            }
        }
        let mut compositor = Compositor(None);
        if mode == "wayland" {
            compositor.0 = Some(
                std::process::Command::new("kwin_wayland")
                    .args([
                        "--virtual",
                        "--width",
                        "800",
                        "--height",
                        "600",
                        "--no-lockscreen",
                        "--no-global-shortcuts",
                        "--no-kactivities",
                        "--socket",
                        "info-clipboard-test",
                    ])
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .spawn()
                    .unwrap(),
            );
            let bus = zbus::blocking::Connection::session().unwrap();
            let proxy = zbus::blocking::Proxy::new(
                &bus,
                "org.freedesktop.DBus",
                "/org/freedesktop/DBus",
                "org.freedesktop.DBus",
            )
            .unwrap();
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            while !proxy
                .call::<_, _, bool>("NameHasOwner", &("org.kde.KWin",))
                .unwrap()
            {
                assert!(
                    std::time::Instant::now() < deadline,
                    "Private compositor startup timed out"
                );
                assert!(
                    compositor.0.as_mut().unwrap().try_wait().unwrap().is_none(),
                    "Private compositor exited"
                );
                std::thread::sleep(std::time::Duration::from_millis(25));
            }
        }
        let document = text("\u{03c0} <clipboard>\nsecond line");
        let selection = document
            .selection(range(0, document.job.text.chars().count()))
            .unwrap();
        write_html(&selection).unwrap();
        let mut reader = arboard::Clipboard::new().unwrap();
        assert_eq!(reader.get().html().unwrap(), selection.html);
        assert_eq!(reader.get_text().unwrap(), selection.plain);
    }

    #[test]
    #[ignore = "Runs clipboard transport against a disposable Xvfb session, never the user clipboard"]
    fn rich_clipboard_roundtrip_on_private_x11() {
        private_clipboard_roundtrip("x11", "rich_clipboard_roundtrip_on_private_x11");
    }

    #[test]
    #[ignore = "Runs clipboard transport against a disposable software KWin session, never the user clipboard"]
    fn rich_clipboard_roundtrip_on_private_wayland() {
        private_clipboard_roundtrip("wayland", "rich_clipboard_roundtrip_on_private_wayland");
    }
}
