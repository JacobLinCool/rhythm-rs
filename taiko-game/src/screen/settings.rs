use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap};

use crate::app::{App, SettingsItem};
use crate::localization::{pad_or_truncate_to_width, UiMessage, UiText};
use crate::preferences::StoredScrollSpeed;
use crate::tui::Frame;

pub fn render(app: &App, frame: &mut Frame<'_>, area: Rect) {
    let columns =
        Layout::horizontal([Constraint::Percentage(58), Constraint::Percentage(42)]).split(area);
    let items = SettingsItem::ALL
        .into_iter()
        .map(|item| ListItem::new(item_line(app, item)))
        .collect::<Vec<_>>();
    let mut state = ListState::default();
    state.select(Some(app.settings.selected));
    frame.render_stateful_widget(
        List::new(items)
            .highlight_symbol(">> ")
            .highlight_style(app.theme.selection)
            .block(themed_block(app, app.text(UiText::PlayerPreferences))),
        columns[0],
        &mut state,
    );

    let mut help = vec![
        Line::from(Span::styled(
            app.text(UiText::PersistentSettings),
            app.theme.title,
        )),
        Line::from(""),
        Line::from(Span::styled(
            app.text(UiText::SettingsAdjustHelp),
            app.theme.text_primary,
        )),
        Line::from(Span::styled(
            app.text(UiText::SettingsBindingHelp),
            app.theme.text_primary,
        )),
        Line::from(Span::styled(
            app.text(UiText::SettingsTestHelp),
            app.theme.metadata,
        )),
        Line::from(""),
    ];
    if let Some((message, is_error)) = &app.settings.status {
        help.push(Line::from(Span::styled(
            message,
            if *is_error {
                app.theme.error
            } else {
                app.theme.success
            },
        )));
        help.push(Line::from(""));
    }
    if let Some((player_index, slot)) = app.settings.capture {
        let binding = app.localizer().binding_slot(slot);
        help.push(Line::from(Span::styled(
            app.localizer().message(UiMessage::CapturingBinding {
                player: player_index + 1,
                binding,
            }),
            app.theme.warning,
        )));
    } else {
        help.push(Line::from(Span::styled(
            app.text(UiText::SettingsSaveHelp),
            app.theme.metadata,
        )));
        help.push(Line::from(Span::styled(
            app.text(UiText::SettingsDiscardHelp),
            app.theme.metadata,
        )));
    }
    frame.render_widget(
        Paragraph::new(help)
            .wrap(Wrap { trim: true })
            .block(themed_block(app, app.text(UiText::ControlsAndKeyTest))),
        columns[1],
    );
}

fn item_line(app: &App, item: SettingsItem) -> Line<'static> {
    let preferences = &app.settings.draft;
    let (label, value) = match item {
        SettingsItem::Language => (
            app.text(UiText::Language).to_owned(),
            app.localizer()
                .language_name(preferences.ui_language)
                .to_owned(),
        ),
        SettingsItem::SongVolume => (
            app.text(UiText::MusicVolume).to_owned(),
            format!("{}%", preferences.song_volume),
        ),
        SettingsItem::SeVolume => (
            app.text(UiText::DrumVolume).to_owned(),
            format!("{}%", preferences.se_volume),
        ),
        SettingsItem::Calibration => (
            app.text(UiText::Calibration).to_owned(),
            format!("{:+} ms", preferences.calibration_offset_ms),
        ),
        SettingsItem::ScrollSpeed => (
            app.text(UiText::ScrollSpeed).to_owned(),
            match preferences.scroll_speed {
                StoredScrollSpeed::Manual(speed) => format!("{speed:.1}x"),
                StoredScrollSpeed::VelocitySync => app.text(UiText::VelocitySync).to_owned(),
            },
        ),
        SettingsItem::Demo => (
            app.text(UiText::SongPreview).to_owned(),
            if preferences.demo_enabled {
                app.text(UiText::On)
            } else {
                app.text(UiText::Off)
            }
            .to_owned(),
        ),
        SettingsItem::PlayerName => (
            app.text(UiText::OnlineName).to_owned(),
            format!("{}_", preferences.player_name),
        ),
        SettingsItem::Binding { player_index, slot } => {
            let bindings = if player_index == 0 {
                preferences.player_one
            } else {
                preferences.player_two
            };
            (
                format!(
                    "P{} {}",
                    player_index + 1,
                    app.localizer().binding_slot(slot)
                ),
                bindings.key(slot).to_ascii_uppercase().to_string(),
            )
        }
        SettingsItem::Save => (
            app.text(UiText::SaveAndReturn).to_owned(),
            app.text(UiText::Enter).to_owned(),
        ),
    };
    Line::from(vec![
        Span::styled(pad_or_truncate_to_width(&label, 20), app.theme.label),
        Span::styled(value, app.theme.value),
    ])
}

fn themed_block<'a>(app: &App, title: &'a str) -> Block<'a> {
    Block::default()
        .borders(Borders::ALL)
        .border_style(app.theme.border)
        .title(title)
        .title_style(app.theme.title)
}
