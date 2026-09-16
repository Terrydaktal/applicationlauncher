pub(crate) const WINDOW_REMOVAL_CONFIRMATION_POLLS: usize = 2;
pub(crate) const ATSPI_LOCATION_PROBE: &str = include_str!("atspi_location_probe.py");
pub(crate) const WINDOW_SEARCH_REFRESH_INTERVAL_MS: u64 = 180;
pub(crate) const WINDOW_SNAPSHOTS_PER_FRAME: usize = 4;
pub(crate) const SETTINGS_VIEWPORT_SIZE: [f32; 2] = [380.0, 760.0];
pub(crate) const SETTINGS_VIEWPORT_MIN_SIZE: [f32; 2] = [340.0, 500.0];
pub(crate) const UI_EVENTS_PER_FRAME: usize = 8;
pub(crate) const CONTROL_REQUEST_LIMIT: usize = 128;
pub(crate) const TERMINAL_DBUS_SERVICE: &str = "org.xfce.Terminal5";
pub(crate) const TERMINAL_DBUS_PATH: &str = "/org/xfce/Terminal";
pub(crate) const TERMINAL_DBUS_INTERFACE: &str = "org.xfce.Terminal5";
pub(crate) const TERMINAL_METADATA_RETRY_SECS: u64 = 5;
pub(crate) const TERMINAL_ACTION_MESSAGE_SECS: u64 = 4;

pub(crate) fn live_repaint_backoff(frame_cpu_micros: u32) -> std::time::Duration {
    const FRAME_BUDGET_MICROS: u32 = 16_000;
    const MAX_BACKOFF_MS: u64 = 100;

    if frame_cpu_micros <= FRAME_BUDGET_MICROS {
        return std::time::Duration::ZERO;
    }

    std::time::Duration::from_millis(u64::from(frame_cpu_micros / 1_000).clamp(1, MAX_BACKOFF_MS))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_repaints_remain_immediate_while_frames_fit_the_budget() {
        assert_eq!(live_repaint_backoff(16_000), std::time::Duration::ZERO);
    }

    #[test]
    fn expensive_frames_back_off_bounded_live_animation_work() {
        assert_eq!(
            live_repaint_backoff(40_000),
            std::time::Duration::from_millis(40)
        );
        assert_eq!(
            live_repaint_backoff(u32::MAX),
            std::time::Duration::from_millis(100)
        );
    }
}
