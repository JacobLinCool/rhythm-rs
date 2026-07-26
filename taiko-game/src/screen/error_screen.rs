use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

use crate::app::App;
use crate::localization::{UiMessage, UiText};
use crate::tui::Frame;

pub fn render(app: &App, frame: &mut Frame<'_>, area: Rect) {
    let state = app.error_state.as_ref();
    let message = state.map_or(app.text(UiText::TaikoCouldNotContinue), |state| {
        app.text(state.summary)
    });
    let destination = state.map_or(app.text(UiText::PlayModeSelection), |state| {
        app.text(state.recovery.label_key())
    });
    let primary_action = state.and_then(|state| state.retry).map_or_else(
        || app.localizer().message(UiMessage::ReturnTo { destination }),
        |retry| {
            app.localizer().message(UiMessage::RetryAction {
                action: app.text(retry.label_key()),
            })
        },
    );

    let paragraph = Paragraph::new(vec![
        Line::from(Span::styled(
            app.text(UiText::RecoverableErrorOccurred),
            app.theme.error,
        )),
        Line::from(""),
        Line::from(Span::styled(message.to_owned(), app.theme.value)),
        Line::from(""),
        Line::from(vec![
            Span::styled(
                if state.and_then(|state| state.retry).is_some() {
                    app.text(UiText::PressEnter)
                } else {
                    app.text(UiText::PressEnterOrEsc)
                },
                app.theme.warning,
            ),
            Span::styled(primary_action, app.theme.text_primary),
        ]),
        Line::from(vec![
            Span::styled(app.text(UiText::PressEsc), app.theme.warning),
            Span::styled(
                app.localizer().message(UiMessage::ReturnTo { destination }),
                app.theme.text_primary,
            ),
        ]),
        Line::from(vec![
            Span::styled(app.text(UiText::PressD), app.theme.warning),
            Span::styled(
                app.text(UiText::ToggleTechnicalDetails),
                app.theme.text_primary,
            ),
        ]),
        if state.is_some_and(|state| state.details_visible) {
            Line::from(Span::styled(
                state
                    .map(|state| state.technical_details.clone())
                    .unwrap_or_default(),
                app.theme.metadata,
            ))
        } else {
            Line::from("")
        },
        Line::from(vec![
            Span::styled(app.text(UiText::PressCtrlC), app.theme.warning),
            Span::styled(app.text(UiText::QuitSentence), app.theme.text_primary),
        ]),
    ])
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(app.theme.error)
            .title(app.text(UiText::ErrorTitle))
            .title_style(app.theme.error),
    )
    .style(app.theme.text_primary)
    .wrap(Wrap { trim: true });

    frame.render_widget(paragraph, area);
}
