use crate::tui::Frame;
use color_eyre::eyre::Result;
use ratatui::{prelude::*, widgets::*};

use super::Component;

#[derive(Debug, Clone)]
pub struct TopBar {
    pub text: String,
}

impl TopBar {
    pub fn set_text(&mut self, text: String) {
        self.text = text;
    }

    pub fn set_default_text(&mut self) {
        self.text = format!(
            "Taiko on Terminal! v{} {}",
            env!("CARGO_PKG_VERSION"),
            env!("VERGEN_GIT_DESCRIBE")
        );
    }

    pub fn set_song_text(&mut self, song: &str, subtitle: &str) {
        self.text = format!("{} {}", song, subtitle);
    }

    pub fn set_game_text(
        &mut self,
        song: &str,
        course: &str,
        time: f64,
        score: u32,
        combo: u32,
        max_combo: u32,
    ) {
        self.text = format!(
            "{} ({}) | {:.1} secs | {} pts | {} combo (max: {})",
            song, course, time, score, combo, max_combo
        );
    }
}

impl Component for TopBar {
    fn new() -> Self {
        Self {
            text: format!(
                "Taiko on Terminal! v{} {}",
                env!("CARGO_PKG_VERSION"),
                env!("VERGEN_GIT_DESCRIBE")
            ),
        }
    }

    fn render(&self, f: &mut Frame<'_>, area: Rect) -> Result<()> {
        let topbar_left = Block::default()
            .title(block::Title::from(self.text.clone().dim()).alignment(Alignment::Left));
        f.render_widget(topbar_left, area);

        Ok(())
    }
}
