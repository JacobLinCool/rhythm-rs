use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap};

use crate::app::App;
use crate::localization::UiText;
use crate::tui::Frame;

pub fn render(app: &App, frame: &mut Frame<'_>, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(56), Constraint::Percentage(44)])
        .split(area);

    let list_items = if app.filtered_song_indices.is_empty() {
        vec![ListItem::new(Line::from(Span::styled(
            app.text(UiText::NoMatchingSongs),
            app.theme.warning,
        )))]
    } else {
        app.filtered_song_indices
            .iter()
            .filter_map(|index| app.songs.get(*index))
            .map(|song| {
                let subtitle = if song.subtitle.trim().is_empty() {
                    String::new()
                } else {
                    format!(" - {}", song.subtitle)
                };
                ListItem::new(Line::from(vec![
                    Span::styled(song.title.clone(), app.theme.text_primary),
                    Span::styled(subtitle, app.theme.text_secondary),
                ]))
            })
            .collect::<Vec<_>>()
    };

    let list = List::new(list_items)
        .block(themed_block(app, app.text(UiText::SongMenuTitle)))
        .highlight_style(app.theme.selection)
        .highlight_symbol(">> ");

    let mut state = ListState::default();
    state.select((!app.filtered_song_indices.is_empty()).then_some(app.song_index));
    frame.render_stateful_widget(list, chunks[0], &mut state);

    let search_query = if app.song_query.is_empty() {
        app.text(UiText::Empty).to_owned()
    } else {
        app.song_query.clone()
    };
    let matches_line = format!("{}/{}", app.visible_song_count(), app.songs.len());

    let mut info_lines = vec![
        kv_line(app, app.text(UiText::Search), search_query),
        kv_line(app, app.text(UiText::Matches), matches_line),
    ];

    if let Some(status) = app.demo_preview_status_text() {
        info_lines.push(Line::from(vec![
            Span::styled(format!("{}: ", app.text(UiText::Preview)), app.theme.label),
            Span::styled(status, app.theme.warning),
        ]));
    }

    if let Some(status) = app.offline_library_status_text() {
        info_lines.push(Line::from(vec![
            Span::styled(
                format!("{}: ", app.text(UiText::OfflineLibrary)),
                app.theme.label,
            ),
            Span::styled(status, app.theme.warning),
        ]));
        info_lines.push(Line::from(Span::styled(
            app.text(UiText::OnlineStillAvailable),
            app.theme.text_primary,
        )));
        info_lines.push(line_value(app, ""));
    }

    if let Some(error) = app.song_filter_error.as_ref() {
        info_lines.push(Line::from(vec![
            Span::styled(
                format!("{}: ", app.text(UiText::FilterError)),
                app.theme.label,
            ),
            Span::styled(error.clone(), app.theme.error),
        ]));
        info_lines.push(line_value(app, ""));
    }

    if let Some(song) = app.selected_song() {
        let bpm = song
            .courses
            .first()
            .and_then(|course| course.base_bpm)
            .unwrap_or(0.0);

        info_lines.extend(vec![
            line_value(app, ""),
            kv_line(app, app.text(UiText::Title), song.title.clone()),
            kv_line(app, app.text(UiText::Subtitle), song.subtitle.clone()),
            kv_line(app, app.text(UiText::Artist), song.artist.clone()),
            kv_line(app, "BPM", format!("{bpm:.2}")),
            kv_line(
                app,
                app.text(UiText::Courses),
                song.courses.len().to_string(),
            ),
            kv_line(
                app,
                app.text(UiText::Branching),
                app.text(if song.has_branching() {
                    UiText::Yes
                } else {
                    UiText::No
                })
                .to_owned(),
            ),
            kv_line(
                app,
                app.text(UiText::Audio),
                app.text(if song.audio_path().is_some() {
                    UiText::Available
                } else {
                    UiText::Silent
                })
                .to_owned(),
            ),
            line_value(app, ""),
            if app.load_warnings.is_empty() {
                kv_line(app, app.text(UiText::LoadWarnings), "0".to_owned())
            } else {
                Line::from(vec![
                    Span::styled(
                        format!("{}: ", app.text(UiText::LoadWarnings)),
                        app.theme.label,
                    ),
                    Span::styled(app.load_warnings.len().to_string(), app.theme.warning),
                ])
            },
            line_value(app, ""),
            line_value(app, &format!("{}:", app.text(UiText::Keys))),
            line_value(app, &format!("- {}", app.text(UiText::SongFilterHelp))),
            line_value(app, &format!("- {}", app.text(UiText::ConfirmEnterHelp))),
            line_value(app, &format!("- {}", app.text(UiText::NavigateArrowsHelp))),
            line_value(app, &format!("- {}", app.text(UiText::LoadWarningsHelp))),
            line_value(app, &format!("- {}", app.text(UiText::BackToModesHelp))),
            line_value(app, &format!("- {}", app.text(UiText::QuitHelp))),
            line_value(app, ""),
            line_value(app, &format!("{}:", app.text(UiText::FilterExamples))),
            line_value(app, "- oni=9"),
            line_value(app, "- oni=8,9,10"),
            line_value(app, "- hard=4-7"),
            line_value(app, "- lvl=8-10  branch  bpm>=180"),
        ]);
    } else {
        info_lines.push(line_value(app, app.text(UiText::NoPlayableSong)));
        info_lines.push(line_value(app, ""));
        info_lines.push(line_value(app, &format!("{}:", app.text(UiText::Keys))));
        info_lines.push(line_value(
            app,
            &format!("- {}", app.text(UiText::LoadWarningsHelp)),
        ));
        info_lines.push(line_value(
            app,
            &format!("- {}", app.text(UiText::BackToModesHelp)),
        ));
        info_lines.push(line_value(
            app,
            &format!("- {}", app.text(UiText::QuitHelp)),
        ));
        info_lines.push(line_value(app, ""));
        info_lines.push(line_value(
            app,
            &format!("{}:", app.text(UiText::FilterExamples)),
        ));
        info_lines.push(line_value(app, "- oni=9"));
        info_lines.push(line_value(app, "- oni=8,9,10"));
        info_lines.push(line_value(app, "- hard=4-7"));
        info_lines.push(line_value(app, "- lvl=8-10  branch  bpm>=180"));
        info_lines.push(line_value(app, "- nobranch  bpm=120-180"));
    }

    let info = Paragraph::new(info_lines)
        .block(themed_block(app, app.text(UiText::SongInfo)))
        .style(app.theme.text_primary)
        .wrap(Wrap { trim: true });
    frame.render_widget(info, chunks[1]);
}

fn themed_block<'a>(app: &App, title: &'a str) -> Block<'a> {
    Block::default()
        .borders(Borders::ALL)
        .border_style(app.theme.border)
        .title(title)
        .title_style(app.theme.title)
}

fn kv_line(app: &App, label: &str, value: String) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{label}: "), app.theme.label),
        Span::styled(value, app.theme.value),
    ])
}

fn line_value(app: &App, value: &str) -> Line<'static> {
    Line::from(Span::styled(value.to_owned(), app.theme.text_primary))
}
