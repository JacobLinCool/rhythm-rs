use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

use crate::app::App;
use crate::localization::UiText;
use crate::offline_preparation::OfflinePreparationMode;
use crate::tui::Frame;

pub fn render(app: &App, frame: &mut Frame<'_>, area: Rect) {
    let heading = app.text(match app.offline_preparation_mode() {
        Some(OfflinePreparationMode::Single) => UiText::PreparingSinglePlayerMatch,
        Some(OfflinePreparationMode::LocalTwoPlayer) => UiText::PreparingLocalTwoPlayerMatch,
        None => UiText::PreparingGenericMatch,
    });
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled(heading, app.theme.title)),
            Line::from(""),
            Line::from(Span::styled(
                app.text(UiText::LoadingAndValidatingChart),
                app.theme.text_primary,
            )),
            Line::from(Span::styled(
                app.text(UiText::DecodingChartAudio),
                app.theme.text_primary,
            )),
            Line::from(""),
            Line::from(Span::styled(
                app.text(UiText::CancelPreparationHelp),
                app.theme.metadata,
            )),
        ])
        .centered()
        .wrap(Wrap { trim: true })
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(app.theme.warning)
                .title(format!(" {} ", app.text(UiText::Preparing))),
        ),
        area,
    );
}
