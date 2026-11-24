//! One bundled logo for native windows and desktop integration.

use std::sync::{Arc, OnceLock};

use eframe::egui::{self, IconData};

/// Keep native focus identities distinct while giving every window the same logo.
/// On KDE Wayland the bundled window rule maps this app-ID family to our desktop
/// entry; the icon pixels are used by backends with native window-icon support.
pub(crate) fn viewport(id: egui::ViewportId) -> egui::ViewportBuilder {
    egui::ViewportBuilder::default()
        .with_app_id(crate::popups::foreground::app_id(std::process::id(), id))
        .with_icon(window_icon())
}

pub(crate) fn window_icon() -> Arc<IconData> {
    static ICON: OnceLock<Arc<IconData>> = OnceLock::new();
    ICON.get_or_init(|| {
        let image = egui_extras::image::load_svg_bytes(
            include_bytes!("assets/applicationlauncher.svg"),
            &Default::default(),
        )
        .expect("bundled Application Launcher logo must be a valid SVG");
        Arc::new(IconData {
            rgba: image
                .pixels
                .iter()
                .flat_map(|pixel| pixel.to_srgba_unmultiplied())
                .collect(),
            width: image.size[0] as u32,
            height: image.size[1] as u32,
        })
    })
    .clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logo_is_decoded_once_and_shared_between_windows() {
        assert!(Arc::ptr_eq(&window_icon(), &window_icon()));
    }

    #[test]
    fn desktop_entry_matches_wayland_identity_and_installed_icon_name() {
        let desktop = include_str!("assets/applicationlauncher.desktop");
        let id = crate::popups::foreground::app_id(42, eframe::egui::ViewportId::ROOT);
        assert_eq!(id, "applicationlauncher");
        for entry in [
            "Type=Application".to_owned(),
            "Exec=applicationlauncher".into(),
            format!("Icon={id}"),
            format!("StartupWMClass={id}"),
            "Terminal=false".into(),
        ] {
            assert!(desktop.lines().any(|line| line == entry), "missing {entry}");
        }
    }
}
