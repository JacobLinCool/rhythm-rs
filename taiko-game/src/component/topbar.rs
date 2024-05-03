use crate::{app::AppGlobalState, tui::Frame};
use color_eyre::eyre::Result;
use ratatui::{prelude::*, widgets::*};
use taiko_core::constant::COURSE_TYPE;

use super::Component;

pub struct TopBar {}

impl Component for TopBar {
    fn new() -> Self {
        Self {}
    }

    fn render(&mut self, app: &mut AppGlobalState, f: &mut Frame<'_>, area: Rect) -> Result<()> {
        let content = if app.selected_song.is_none() {
            format!(
                "Taiko on Terminal! v{} {}",
                env!("CARGO_PKG_VERSION"),
                env!("VERGEN_GIT_DESCRIBE")
            )
        } else if app.taiko.is_none() {
            format!(
                "{} {}",
                app.selected_song
                    .as_ref()
                    .unwrap()
                    .tja()
                    .header
                    .title
                    .as_ref()
                    .unwrap(),
                app.selected_song
                    .as_ref()
                    .unwrap()
                    .tja()
                    .header
                    .subtitle
                    .as_ref()
                    .unwrap()
            )
        } else {
            format!(
                "{} ({}) | {:.1} secs | {} pts | {} combo (max: {})",
                app.selected_song
                    .as_ref()
                    .unwrap()
                    .tja()
                    .header
                    .title
                    .as_ref()
                    .unwrap(),
                if app.selected_course.as_ref().unwrap().course < COURSE_TYPE.len() as i32 {
                    COURSE_TYPE[app.selected_course.as_ref().unwrap().course as usize]
                } else {
                    "Unknown"
                },
                app.player_time(),
                app.output.score,
                app.output.current_combo,
                app.output.max_combo
            )
        };

        let size = f.size();
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(1), Constraint::Fill(size.height - 1)].as_ref())
            .split(size);

        let topbar_right = Block::default().title(
            block::Title::from(format!("{:.2} ms", app.lm.latency_ms()).dim())
                .alignment(Alignment::Right),
        );
        f.render_widget(topbar_right, area);

        let topbar_left =
            Block::default().title(block::Title::from(content.dim()).alignment(Alignment::Left));
        f.render_widget(topbar_left, area);

        Ok(())
    }
}
