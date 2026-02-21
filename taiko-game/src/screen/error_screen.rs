use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

use crate::app::App;
use crate::tui::Frame;

pub fn render(app: &App, frame: &mut Frame<'_>, area: Rect) {
    let message = app
        .error_message
        .as_deref()
        .unwrap_or("unknown runtime error");

    let paragraph = Paragraph::new(vec![
        Line::from(Span::styled(
            "A recoverable error occurred.",
            app.theme.error,
        )),
        Line::from(""),
        Line::from(Span::styled(message.to_owned(), app.theme.value)),
        Line::from(""),
        Line::from(vec![
            Span::styled("Press Enter/Esc ", app.theme.warning),
            Span::styled("to return to Song Menu.", app.theme.text_primary),
        ]),
        Line::from(vec![
            Span::styled("Press Ctrl+C ", app.theme.warning),
            Span::styled("to quit.", app.theme.text_primary),
        ]),
    ])
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(app.theme.error)
            .title("Error")
            .title_style(app.theme.error),
    )
    .style(app.theme.text_primary)
    .wrap(Wrap { trim: true });

    frame.render_widget(paragraph, area);
}
