use eframe::egui;
use std::collections::HashSet;

#[derive(Default)]
pub(super) struct ImeInputFilter {
    composing: HashSet<egui::ViewportId>,
}

impl ImeInputFilter {
    pub(super) fn filter(&mut self, input: &mut egui::RawInput) -> usize {
        self.composing.retain(|id| input.viewports.contains_key(id));
        let viewport = input.viewport_id;
        let before = input.events.len();
        input.events.retain(|event| match event {
            egui::Event::Ime(egui::ImeEvent::Enabled | egui::ImeEvent::Preedit(_)) => {
                self.composing.insert(viewport);
                true
            }
            egui::Event::Ime(egui::ImeEvent::Commit(text)) => {
                if text != "\n" && text != "\r" {
                    self.composing.remove(&viewport);
                }
                true
            }
            egui::Event::Ime(egui::ImeEvent::Disabled) => {
                // Wayland replies to cursor-area commits with an empty preedit.
                // Forwarding its no-op disable makes egui-winit commit the same
                // area again, creating an idle, focused redraw feedback loop.
                self.composing.remove(&viewport)
            }
            _ => true,
        });
        before - input.events.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(events: Vec<egui::Event>) -> egui::RawInput {
        egui::RawInput {
            events,
            ..Default::default()
        }
    }

    #[test]
    fn redundant_ime_disable_does_not_count_as_user_input() {
        let mut filter = ImeInputFilter::default();
        let text = egui::Event::Text("search".into());
        let mut raw = input(vec![
            egui::Event::Ime(egui::ImeEvent::Disabled),
            text.clone(),
            egui::Event::Ime(egui::ImeEvent::Disabled),
            egui::Event::Copy,
        ]);
        assert_eq!(filter.filter(&mut raw), 2);
        assert_eq!(raw.events, vec![text, egui::Event::Copy]);
    }

    #[test]
    fn real_composition_and_its_cancellation_are_preserved_in_order() {
        let mut filter = ImeInputFilter::default();
        let events = vec![
            egui::Event::Ime(egui::ImeEvent::Enabled),
            egui::Event::Ime(egui::ImeEvent::Preedit("candidate".into())),
            egui::Event::WindowFocused(false),
            egui::Event::Ime(egui::ImeEvent::Disabled),
        ];
        let mut raw = input(events.clone());
        assert_eq!(filter.filter(&mut raw), 0);
        assert_eq!(raw.events, events);
        let mut duplicate = input(vec![egui::Event::Ime(egui::ImeEvent::Disabled)]);
        assert_eq!(filter.filter(&mut duplicate), 1);
    }

    #[test]
    fn committed_text_is_preserved_but_its_empty_acknowledgement_is_not() {
        let mut filter = ImeInputFilter::default();
        let mut start = input(vec![egui::Event::Ime(egui::ImeEvent::Enabled)]);
        filter.filter(&mut start);
        let commit = egui::Event::Ime(egui::ImeEvent::Commit("result".into()));
        let mut finish = input(vec![
            commit.clone(),
            egui::Event::Ime(egui::ImeEvent::Disabled),
        ]);
        assert_eq!(filter.filter(&mut finish), 1);
        assert_eq!(finish.events, vec![commit]);
    }

    #[test]
    fn ignored_newline_commit_does_not_lose_composition_cancellation() {
        let mut filter = ImeInputFilter::default();
        let mut raw = input(vec![
            egui::Event::Ime(egui::ImeEvent::Enabled),
            egui::Event::Ime(egui::ImeEvent::Commit("\n".into())),
            egui::Event::Ime(egui::ImeEvent::Disabled),
        ]);
        assert_eq!(filter.filter(&mut raw), 0);
    }

    #[test]
    fn composition_state_is_separate_for_each_popup_and_removed_on_close() {
        let mut filter = ImeInputFilter::default();
        let popup = egui::ViewportId::from_hash_of("popup");
        let mut raw = input(vec![egui::Event::Ime(egui::ImeEvent::Enabled)]);
        raw.viewports.insert(popup, Default::default());
        filter.filter(&mut raw);
        raw.viewport_id = popup;
        raw.events = vec![egui::Event::Ime(egui::ImeEvent::Disabled)];
        assert_eq!(filter.filter(&mut raw), 1);
        raw.viewport_id = egui::ViewportId::ROOT;
        raw.events = vec![egui::Event::Ime(egui::ImeEvent::Disabled)];
        assert_eq!(filter.filter(&mut raw), 0);
        raw.viewport_id = popup;
        raw.events = vec![egui::Event::Ime(egui::ImeEvent::Enabled)];
        filter.filter(&mut raw);
        raw.viewports.remove(&popup);
        raw.viewport_id = egui::ViewportId::ROOT;
        raw.events.clear();
        filter.filter(&mut raw);
        assert!(filter.composing.is_empty());
    }

    // Model egui-winit's actual cursor-area update predicate and Wayland's
    // empty-preedit reply using the linked egui, without a compositor or input.
    fn cursor_area_feedback_frames(filtered: bool) -> usize {
        let ctx = egui::Context::default();
        let mut filter = ImeInputFilter::default();
        let mut text = String::new();
        let mut previous_rect = None;
        for frame in 0..32 {
            let mut raw = input(vec![egui::Event::Ime(egui::ImeEvent::Disabled)]);
            raw.time = Some(frame as f64 / 60.0);
            if filtered {
                filter.filter(&mut raw);
            }
            let output = ctx.run(raw, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let response = ui.text_edit_singleline(&mut text);
                    response.request_focus();
                });
            });
            let Some(ime) = output.platform_output.ime else {
                continue;
            };
            let should_commit =
                previous_rect != Some(ime.rect) || ctx.input(|input| !input.events.is_empty());
            previous_rect = Some(ime.rect);
            if !should_commit {
                return frame + 1;
            }
        }
        32
    }

    #[test]
    fn focused_idle_cursor_area_feedback_quiesces_instead_of_repainting_forever() {
        assert_eq!(cursor_area_feedback_frames(false), 32);
        assert!(cursor_area_feedback_frames(true) <= 3);
    }
}
