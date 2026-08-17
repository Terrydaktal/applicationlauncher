fn main() {
    applicationlauncher::observability::initialize(
        applicationlauncher::observability::Component::Daemon,
    );
    applicationlauncher::observability::install_panic_hook(
        applicationlauncher::observability::Component::Daemon,
    );
    if std::env::args().any(|argument| argument == "--build-id") {
        println!("{}", applicationlauncher::BUILD_ID);
        return;
    }
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
    let _diagnostic_server = match applicationlauncher::observability::start_diagnostic_server(
        applicationlauncher::observability::Component::Daemon,
    ) {
        Ok(server) => Some(server),
        Err(err) => {
            eprintln!("applicationlauncherd: diagnostic endpoint unavailable: {err}");
            None
        }
    };
    let _main_worker = applicationlauncher::observability::register_worker("daemon-main");
    if let Err(err) = applicationlauncher::tracker::run_tracker_daemon() {
        eprintln!("applicationlauncherd: {err}");
        std::process::exit(1);
    }
}
