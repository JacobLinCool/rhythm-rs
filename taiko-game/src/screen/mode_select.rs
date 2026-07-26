use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap};

use crate::app::{App, GameMode};
use crate::audio::AudioNotice;
use crate::localization::UiText;
use crate::tui::Frame;

pub fn render(app: &App, frame: &mut Frame<'_>, area: Rect) {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),
            Constraint::Length(15),
            Constraint::Min(1),
        ])
        .split(area);
    let center = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Min(1),
            Constraint::Length(72),
            Constraint::Min(1),
        ])
        .split(vertical[1])[1];
    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(29), Constraint::Min(30)])
        .split(center);

    let items = GameMode::ALL
        .into_iter()
        .map(|mode| {
            ListItem::new(Line::from(Span::styled(
                app.text(mode.label_key()),
                app.theme.text_primary,
            )))
        })
        .collect::<Vec<_>>();
    let list = List::new(items)
        .block(themed_block(app, app.text(UiText::ChoosePlayMode)))
        .highlight_style(app.theme.selection)
        .highlight_symbol(">> ");
    let mut state = ListState::default();
    state.select(Some(app.mode_selection));
    frame.render_stateful_widget(list, columns[0], &mut state);

    let mode = GameMode::ALL[app.mode_selection.min(GameMode::ALL.len() - 1)];
    let mut detail_lines = vec![
        Line::from(Span::styled(app.text(mode.label_key()), app.theme.title)),
        Line::from(""),
        Line::from(Span::styled(
            app.text(mode.description_key()),
            app.theme.text_primary,
        )),
        Line::from(""),
        Line::from(Span::styled(
            app.text(mode.controls_key()),
            app.theme.metadata,
        )),
        Line::from(""),
        Line::from(Span::styled(
            app.text(UiText::ModeSelectControls),
            app.theme.metadata,
        )),
    ];
    if let Some(status) = app.offline_library_status_text() {
        detail_lines.push(Line::from(""));
        detail_lines.push(Line::from(vec![
            Span::styled(
                format!("{}: ", app.text(UiText::OfflineLibrary)),
                app.theme.label,
            ),
            Span::styled(status, app.theme.warning),
        ]));
    }
    if let Some(notice) = app.audio_notice() {
        let (label, reason) = match notice {
            AudioNotice::OutputUnavailable { reason } => {
                (UiText::AudioUnavailable, reason.as_ref())
            }
            AudioNotice::SoundEffectsDisabled { reason } => {
                (UiText::SoundEffectsDisabled, reason.as_ref())
            }
        };
        detail_lines.push(Line::from(""));
        detail_lines.push(Line::from(Span::styled(app.text(label), app.theme.warning)));
        detail_lines.push(Line::from(Span::styled(
            reason.to_owned(),
            app.theme.metadata,
        )));
    }
    let details = Paragraph::new(detail_lines)
        .block(themed_block(app, app.text(UiText::HowItWorks)))
        .wrap(Wrap { trim: true });
    frame.render_widget(details, columns[1]);
}

fn themed_block<'a>(app: &App, title: &'a str) -> Block<'a> {
    Block::default()
        .borders(Borders::ALL)
        .border_style(app.theme.border)
        .title(title)
        .title_style(app.theme.title)
}
