fn main() {
    if std::env::args().any(|argument| argument == "--install") {
        if let Err(err) = applicationlauncher::tracker::ensure_tracker_installed() {
            eprintln!("applicationlauncherd: {err}");
            std::process::exit(1);
        }
        return;
    }
    if std::env::args().any(|argument| argument == "-h" || argument == "--help") {
        println!(
            "applicationlauncherd [--install]\n\nPersistent Application Launcher window history and session service.\n\n  --install  Install its symlink and systemd user service, then start it.\n  -h, --help Show this help."
        );
        return;
    }
    if let Err(err) = applicationlauncher::tracker::run_tracker_daemon() {
        eprintln!("applicationlauncherd: {err}");
        std::process::exit(1);
    }
}
