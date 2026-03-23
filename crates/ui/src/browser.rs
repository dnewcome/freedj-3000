//! Track browser — scrollable list with search.
//!
//! Backed by the library database.  Updates are async — search queries run on
//! a background thread and results are published via a channel.

use opendeck_types::TrackInfo;

pub struct Browser {
    pub query:       String,
    pub results:     Vec<TrackInfo>,
    pub selected:    Option<usize>,
}

impl Browser {
    pub fn new() -> Self {
        Self { query: String::new(), results: Vec::new(), selected: None }
    }

    /// Draw the browser panel.  Returns the track the user wants to load, if any.
    pub fn draw(&mut self, ui: &mut egui::Ui) -> Option<&TrackInfo> {
        let mut load_request = None;

        ui.horizontal(|ui| {
            ui.label("🔍");
            ui.text_edit_singleline(&mut self.query);
        });

        ui.separator();

        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for (i, track) in self.results.iter().enumerate() {
                    let title  = track.title.as_deref().unwrap_or("Unknown");
                    let artist = track.artist.as_deref().unwrap_or("Unknown Artist");
                    let bpm    = track.bpm.map(|b| format!("{:.1}", b)).unwrap_or_default();

                    let selected = self.selected == Some(i);
                    let label = format!("{} — {}  {}", title, artist, bpm);

                    if ui.selectable_label(selected, &label).double_clicked() {
                        self.selected = Some(i);
                        load_request = Some(i);
                    }
                }
            });

        load_request.and_then(|i| self.results.get(i))
    }
}

impl Default for Browser {
    fn default() -> Self { Self::new() }
}
