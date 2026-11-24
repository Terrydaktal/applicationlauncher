use crate::models::{TerminalDbusRecord, UiEvent, WindowInfo};
use crate::windows::{
    fetch_terminal_dbus_records, get_open_windows_fast, is_terminal_class,
    terminal_record_for_window,
};
use eframe::egui;
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::mpsc::Sender;
use std::time::Duration;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TerminalInfoRequest {
    pid: i32,
    window_uuid: String,
}

#[derive(Default)]
pub(crate) struct InfoOnlyHost {
    pub(crate) enabled: bool,
    pending: usize,
}

impl InfoOnlyHost {
    pub(crate) fn request_started(&mut self) {
        self.pending += 1;
    }

    pub(crate) fn request_finished(&mut self) {
        self.pending = self.pending.saturating_sub(1);
    }

    pub(crate) fn can_exit(&self, has_popups: bool) -> bool {
        self.enabled && self.pending == 0 && !has_popups
    }
}

impl TerminalInfoRequest {
    fn parse(pid: &str, uuid: &str) -> Result<Self, String> {
        let pid = pid
            .parse::<i32>()
            .ok()
            .filter(|pid| *pid > 0)
            .ok_or("Show Info requires a positive terminal PID")?;
        if uuid.len() != 36
            || !uuid.bytes().enumerate().all(|(index, byte)| {
                if [8, 13, 18, 23].contains(&index) {
                    byte == b'-'
                } else {
                    byte.is_ascii_hexdigit()
                }
            })
        {
            return Err("Show Info requires a terminal window UUID".into());
        }
        Ok(Self {
            pid,
            window_uuid: uuid.to_owned(),
        })
    }

    pub(crate) fn from_args(args: &[String]) -> Result<Option<Self>, String> {
        let mut positions = args
            .iter()
            .enumerate()
            .filter(|(_, arg)| arg.as_str() == "--show-terminal-info");
        let Some((position, _)) = positions.next() else {
            return Ok(None);
        };
        if positions.next().is_some() {
            return Err("Specify --show-terminal-info only once".into());
        }
        let pid = args
            .get(position + 1)
            .ok_or("--show-terminal-info requires PID and WINDOW_UUID")?;
        let uuid = args
            .get(position + 2)
            .ok_or("--show-terminal-info requires PID and WINDOW_UUID")?;
        Self::parse(pid, uuid).map(Some)
    }

    pub(crate) fn from_control(request: &str) -> Result<Self, String> {
        let words = request.split_whitespace().collect::<Vec<_>>();
        if words.len() != 3 || words[0] != "show-terminal-info" {
            return Err("Expected show-terminal-info PID WINDOW_UUID".into());
        }
        Self::parse(words[1], words[2])
    }

    pub(crate) fn control_request(&self) -> String {
        format!("show-terminal-info {} {}\n", self.pid, self.window_uuid)
    }

    pub(crate) fn forward_to(&self, socket: &Path) -> Result<bool, String> {
        let stream = match UnixStream::connect(socket) {
            Ok(stream) => stream,
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
                ) =>
            {
                return Ok(false);
            }
            Err(error) => return Err(format!("Could not contact the running launcher: {error}")),
        };
        self.forward_connected(stream)?;
        Ok(true)
    }

    fn forward_connected(&self, mut stream: UnixStream) -> Result<(), String> {
        let exchange = || -> std::io::Result<String> {
            stream.set_read_timeout(Some(Duration::from_secs(3)))?;
            stream.set_write_timeout(Some(Duration::from_secs(3)))?;
            stream.write_all(self.control_request().as_bytes())?;
            stream.shutdown(std::net::Shutdown::Write)?;
            let mut response = String::new();
            stream.take(128).read_to_string(&mut response)?;
            Ok(response)
        };
        let response = exchange().map_err(|error| format!(
            "The running launcher did not acknowledge Show Info: {error}; its socket was left untouched"
        ))?;
        if response.trim() != "info-requested" {
            return Err("The running Application Launcher does not support terminal Show Info; restart its GUI once".into());
        }
        Ok(())
    }
}

fn select_window(
    request: &TerminalInfoRequest,
    windows: Vec<WindowInfo>,
    records: &[TerminalDbusRecord],
) -> Result<WindowInfo, String> {
    if !records.iter().any(|record| {
        record.active
            && record.terminal_pid == request.pid as u32
            && record.window_uuid == request.window_uuid
    }) {
        return Err(
            "The requested terminal window has closed or its metadata API is unavailable".into(),
        );
    }
    let mut matches = windows.into_iter().filter(|window| {
        window.pid == Some(request.pid)
            && is_terminal_class(&window.class.to_lowercase())
            && terminal_record_for_window(request.pid, &window.raw_title, records).is_some_and(
                |record| {
                    record.terminal_pid == request.pid as u32
                        && record.window_uuid == request.window_uuid
                },
            )
    });
    let window = matches
        .next()
        .ok_or("Could not safely identify the requested terminal window")?;
    if matches.next().is_some() {
        return Err(
            "Multiple windows match the requested terminal; refusing to show the wrong window"
                .into(),
        );
    }
    Ok(window)
}

pub(crate) fn request_info(
    request: TerminalInfoRequest,
    events: Sender<UiEvent>,
    ctx: egui::Context,
) {
    if events.send(UiEvent::ShowWindowInfoRequested).is_err() {
        return;
    }
    applicationlauncher::spawn_worker!("terminal-show-info", move |_worker| {
        let result = (|| {
            let kdotool = applicationlauncher::process::executable_path("kdotool")
                .ok_or("kdotool is required to identify the terminal window")?;
            let windows = get_open_windows_fast(&kdotool, "breeze-dark")
                .ok_or("Could not read the open windows from KWin")?;
            let records = fetch_terminal_dbus_records()?;
            select_window(&request, windows, &records)
        })();
        let _ = events.send(UiEvent::ShowWindowInfo(result));
        ctx.request_repaint();
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    const UUID: &str = "82b23168-0580-4091-9769-27b5b5582599";

    #[test]
    fn info_only_host_waits_for_all_requests_and_popups_before_exiting() {
        let mut host = InfoOnlyHost {
            enabled: true,
            ..Default::default()
        };
        host.request_started();
        host.request_started();
        assert!(!host.can_exit(false));
        host.request_finished();
        assert!(!host.can_exit(false));
        host.request_finished();
        assert!(!host.can_exit(true));
        assert!(host.can_exit(false));
        host.enabled = false;
        assert!(
            !host.can_exit(false),
            "opening the launcher keeps its host alive"
        );
    }

    fn window(id: &str, pid: i32, title: &str) -> WindowInfo {
        WindowInfo {
            id: id.into(),
            title: title.into(),
            raw_title: title.into(),
            class: "xfce4-terminal".into(),
            desktop_file_name: None,
            minimized: None,
            demands_attention: false,
            icon_path: None,
            active_process: None,
            exe_path: None,
            cwd_path: None,
            command_line: None,
            command_summary: None,
            geometry: None,
            process_chain: Vec::new(),
            tmux_pane: None,
            pid: Some(pid),
            last_activated_at_ms: None,
            activation_sequence: 0,
        }
    }

    fn record(pid: u32, uuid: &str, title: &str) -> TerminalDbusRecord {
        TerminalDbusRecord {
            terminal_pid: pid,
            window_uuid: uuid.into(),
            window_title: title.into(),
            active: true,
            child_pid: pid + 1,
            ..Default::default()
        }
    }

    #[test]
    fn request_validates_and_round_trips() {
        let request = TerminalInfoRequest::parse("42", UUID).unwrap();
        assert_eq!(
            TerminalInfoRequest::from_control(&request.control_request()).unwrap(),
            request
        );
        let args = ["applicationlauncher", "--show-terminal-info", "42", UUID].map(String::from);
        assert_eq!(
            TerminalInfoRequest::from_args(&args).unwrap(),
            Some(request)
        );
        for (pid, uuid) in [
            ("0", UUID),
            ("-1", UUID),
            ("42", "not-a-uuid"),
            ("2147483648", UUID),
        ] {
            assert!(TerminalInfoRequest::parse(pid, uuid).is_err());
        }
        assert!(
            TerminalInfoRequest::from_control(&format!("show-terminal-info 42 {UUID} extra"))
                .is_err()
        );
        assert!(TerminalInfoRequest::from_args(&["--show-terminal-info".into()]).is_err());
    }

    #[test]
    fn forwarding_requires_an_explicit_info_acknowledgement() {
        let request = TerminalInfoRequest::parse("42", UUID).unwrap();
        for response in ["info-requested\n", "focus-requested\n", "error: rejected\n"] {
            let (client, mut server) = UnixStream::pair().unwrap();
            let expected = request.control_request();
            let owner = std::thread::spawn(move || {
                let mut received = String::new();
                server.read_to_string(&mut received).unwrap();
                assert_eq!(received, expected);
                server.write_all(response.as_bytes()).unwrap();
            });
            assert_eq!(
                request.forward_connected(client).is_ok(),
                response == "info-requested\n"
            );
            owner.join().unwrap();
        }
    }

    #[test]
    fn standalone_identity_does_not_depend_on_titles_or_foreground_programs() {
        let request = TerminalInfoRequest::parse("42", UUID).unwrap();
        let windows = vec![window("other", 99, "same"), window("target", 42, "same")];
        assert_eq!(
            select_window(&request, windows, &[record(42, UUID, "older title")])
                .unwrap()
                .id,
            "target"
        );
    }

    #[test]
    fn server_windows_use_the_requested_uuid_and_reject_ambiguity() {
        let request = TerminalInfoRequest::parse("42", UUID).unwrap();
        let records = [
            record(42, UUID, "first"),
            record(42, "other-window", "second"),
        ];
        let windows = vec![window("one", 42, "first"), window("two", 42, "second")];
        assert_eq!(
            select_window(&request, windows, &records).unwrap().id,
            "one"
        );
        let records = [record(42, UUID, "same"), record(42, "other-window", "same")];
        let windows = vec![window("one", 42, "same"), window("two", 42, "same")];
        assert!(select_window(&request, windows, &records).is_err());
    }

    #[test]
    fn closed_unknown_or_ambiguous_windows_are_not_guessed() {
        let request = TerminalInfoRequest::parse("42", UUID).unwrap();
        assert!(select_window(&request, vec![window("one", 42, "same")], &[]).is_err());
        assert!(select_window(&request, Vec::new(), &[record(42, UUID, "same")]).is_err());
        let windows = vec![window("one", 42, "same"), window("two", 42, "same")];
        assert!(select_window(&request, windows, &[record(42, UUID, "same")]).is_err());
    }
}
