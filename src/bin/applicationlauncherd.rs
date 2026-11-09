const PRODUCT_METADATA_TEXT: &str = concat!(
    "PROGRAM-METADATA-V1\napplicationlauncherd\n",
    env!("CARGO_PKG_VERSION"),
    "\nservice\n"
);
#[used]
#[unsafe(link_section = ".program_metadata")]
static PRODUCT_METADATA: [u8; PRODUCT_METADATA_TEXT.len()] =
    applicationlauncher::metadata_bytes(PRODUCT_METADATA_TEXT);

fn main() {
    std::hint::black_box(&PRODUCT_METADATA);
    if std::env::args().nth(1).as_deref() == Some("--lifecycle-gate") {
        use std::os::unix::process::CommandExt;
        let arguments: Vec<_> = std::env::args_os().skip(2).collect();
        let result = if let [path] = arguments.as_slice() {
            applicationlauncher::lifecycle::worker_command("gate", std::path::Path::new(path))
                .map(|mut command| command.exec().to_string())
        } else {
            Err("The private startup gate requires one prepared request".into())
        };
        eprintln!(
            "applicationlauncherd startup gate: {}",
            result.unwrap_or_else(|e| e)
        );
        std::process::exit(125);
    }
    if std::env::args().nth(1).as_deref() == Some("--lifecycle-worker") {
        use std::os::unix::process::CommandExt;
        let args: Vec<_> = std::env::args_os().skip(2).collect();
        let result: Result<(), String> = if args.len() == 2 {
            applicationlauncher::lifecycle::worker_command(
                &args[0].to_string_lossy(),
                std::path::Path::new(&args[1]),
            )
            .and_then(|mut command| Err(command.exec().to_string()))
        } else {
            Err("The private lifecycle worker needs a mode and a private request".into())
        };
        if let Err(error) = result {
            eprintln!("applicationlauncherd: {error}");
        }
        std::process::exit(1);
    }
    if std::env::args().any(|argument| argument == "--build-id") {
        println!("{}", applicationlauncher::BUILD_ID);
        return;
    }
    applicationlauncher::observability::initialize(
        applicationlauncher::observability::Component::Daemon,
    );
    applicationlauncher::observability::install_panic_hook(
        applicationlauncher::observability::Component::Daemon,
    );
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
