//! Standalone service for private-bus integration tests, not an installed daemon.
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let service = applicationlauncher::freshness::Service::default();
    let connection = zbus::blocking::connection::Builder::session()?
        .name(applicationlauncher::tracker::SERVICE_NAME)?
        .serve_at(applicationlauncher::freshness::PATH, service.clone())?
        .build()?;
    let _worker = service.start(connection.inner().clone())?;
    println!("READY");
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    Ok(())
}
