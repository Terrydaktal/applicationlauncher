/// Shared comparison policy for the daemon and the launcher's metadata adapter.
pub fn same_native_build(
    running: Option<&str>,
    current: Option<&str>,
    same_file: bool,
) -> Option<bool> {
    if same_file {
        Some(true)
    } else {
        Some(running? == current?)
    }
}
