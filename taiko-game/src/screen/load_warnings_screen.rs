use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

use crate::app::App;
use crate::localization::UiText;
use crate::tui::Frame;

pub fn render(app: &App, frame: &mut Frame<'_>, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(1)])
        .split(area);

    let count_style = if app.load_warnings.is_empty() {
        app.theme.value
    } else {
        app.theme.warning
    };
    let summary = Paragraph::new(vec![
        Line::from(vec![
            Span::styled(format!("{}: ", app.text(UiText::Warnings)), app.theme.label),
            Span::styled(app.load_warnings.len().to_string(), count_style),
        ]),
        Line::from(vec![
            Span::styled(format!("{}: ", app.text(UiText::Keys)), app.theme.label),
            Span::styled(app.text(UiText::LoadWarningsControls), app.theme.metadata),
        ]),
    ])
    .block(themed_block(app, app.text(UiText::LoadWarnings)))
    .style(app.theme.text_primary)
    .wrap(Wrap { trim: true });
    frame.render_widget(summary, chunks[0]);

    let lines = if app.load_warnings.is_empty() {
        vec![Line::from(Span::styled(
            app.text(UiText::NoLoadWarnings),
            app.theme.text_secondary,
        ))]
    } else {
        app.load_warnings
            .iter()
            .enumerate()
            .map(|(index, warning)| {
                Line::from(vec![
                    Span::styled(format!("{:>4}. ", index + 1), app.theme.metadata),
                    Span::styled(warning.clone(), app.theme.value),
                ])
            })
            .collect::<Vec<_>>()
    };

    let body_height = usize::from(chunks[1].height.saturating_sub(2)).max(1);
    let max_scroll = lines.len().saturating_sub(body_height) as u16;
    let scroll = app.load_warnings_scroll.min(max_scroll);

    let body = Paragraph::new(lines)
        .block(themed_block(app, app.text(UiText::Entries)))
        .style(app.theme.text_primary)
        .wrap(Wrap { trim: false })
        .scroll((scroll, 0));
    frame.render_widget(body, chunks[1]);
}

fn themed_block<'a>(app: &App, title: &'a str) -> Block<'a> {
    Block::default()
        .borders(Borders::ALL)
        .border_style(app.theme.border)
        .title(title)
        .title_style(app.theme.title)
}
