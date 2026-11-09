pub mod diagnostic_capture;
pub mod freshness;
pub mod observability;
pub mod process;
pub mod replay;
pub mod tracker;

pub const BUILD_ID: &str = env!("APPLICATIONLAUNCHER_BUILD_ID");
/// Copy a compile-time producer record into an immutable ELF byte-array section.
pub const fn metadata_bytes<const N: usize>(text: &str) -> [u8; N] {
    assert!(text.len() == N);
    let mut bytes = [0; N];
    let mut index = 0;
    while index < N {
        bytes[index] = text.as_bytes()[index];
        index += 1;
    }
    bytes
}
