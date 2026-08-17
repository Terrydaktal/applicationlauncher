pub mod diagnostic_capture;
pub mod observability;
pub mod process;
pub mod tracker;

pub const BUILD_ID: &str = env!("APPLICATIONLAUNCHER_BUILD_ID");
