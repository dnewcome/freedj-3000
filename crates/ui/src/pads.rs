//! Hot cue pad rendering via egui Painter.
//!
//! 8 pads arranged in a 4×2 grid.  Each pad:
//!   - Colored rectangle (cue color, or dim grey if unset)
//!   - Label text (cue label or slot number)
//!   - Brightness pulse on trigger

use opendeck_types::CueMap;

/// Draw the 8 hot cue pads into the given egui UI region.
pub fn draw_pads(ui: &mut egui::Ui, cues: &CueMap, active_slot: Option<u8>) {
    let pad_size = egui::vec2(60.0, 40.0);
    let spacing = 4.0;

    egui::Grid::new("pads").spacing([spacing, spacing]).show(ui, |ui| {
        for (i, cue) in cues.hot_cues.iter().enumerate() {
            let is_active = active_slot == Some(i as u8);

            let color = cue.as_ref().map(|c| {
                egui::Color32::from_rgb(c.color.r, c.color.g, c.color.b)
            }).unwrap_or(egui::Color32::from_gray(30));

            let display_color = if is_active {
                brighten(color, 1.5)
            } else {
                color
            };

            let label = cue.as_ref()
                .map(|c| if c.label.is_empty() { format!("{}", i + 1) } else { c.label.clone() })
                .unwrap_or_else(|| format!("{}", i + 1));

            let btn = egui::Button::new(
                egui::RichText::new(&label)
                    .color(egui::Color32::WHITE)
                    .strong()
            )
            .fill(display_color)
            .min_size(pad_size);

            ui.add(btn);

            if (i + 1) % 4 == 0 {
                ui.end_row();
            }
        }
    });
}

fn brighten(c: egui::Color32, factor: f32) -> egui::Color32 {
    egui::Color32::from_rgb(
        (c.r() as f32 * factor).min(255.0) as u8,
        (c.g() as f32 * factor).min(255.0) as u8,
        (c.b() as f32 * factor).min(255.0) as u8,
    )
}
