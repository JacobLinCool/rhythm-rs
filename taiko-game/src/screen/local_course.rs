use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap};

use crate::app::{App, CourseSettingFocus};
use crate::local_multiplayer::LocalPlayerId;
use crate::localization::{truncate_to_width, UiMessage, UiText};
use crate::tui::Frame;

pub fn render(app: &App, frame: &mut Frame<'_>, area: Rect) {
    let Some(song) = app.selected_song() else {
        frame.render_widget(
            Paragraph::new(Span::styled(
                app.text(UiText::NoSelectedSong),
                app.theme.error,
            ))
            .block(themed_block(app, app.text(UiText::LocalTwoPlayer))),
            area,
        );
        return;
    };

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(8),
            Constraint::Length(11),
        ])
        .split(area);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(format!("{}: ", app.text(UiText::Song)), app.theme.label),
            Span::styled(song.title.clone(), app.theme.value),
            Span::styled("  ", app.theme.text_primary),
            Span::styled(song.subtitle.clone(), app.theme.text_secondary),
        ]))
        .block(themed_block(app, app.text(UiText::LocalChooseCourses))),
        rows[0],
    );

    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(rows[1]);
    for (player, area) in LocalPlayerId::ALL.into_iter().zip(columns.iter().copied()) {
        render_player_courses(app, frame, area, player);
    }

    let help = Paragraph::new(vec![
        Line::from(Span::styled(
            app.text(UiText::LocalReadyControls),
            app.theme.text_primary,
        )),
        Line::from(Span::styled(
            app.text(UiText::LocalReadyExplanation),
            app.theme.metadata,
        )),
        setting_line(
            app,
            app.text(UiText::Music),
            format!("{}%", app.args.songvol),
            CourseSettingFocus::SongVolume,
        ),
        setting_line(
            app,
            app.text(UiText::SeVolume),
            format!("{}%", app.args.sevol),
            CourseSettingFocus::SeVolume,
        ),
        Line::from(vec![
            setting_span(
                app,
                app.text(UiText::Calibration),
                app.calibration_offset_label(),
                CourseSettingFocus::CalibrationOffset,
            ),
            Span::styled("  ", app.theme.text_primary),
            setting_span(
                app,
                app.text(UiText::Scroll),
                app.scroll_speed_label(),
                CourseSettingFocus::ScrollSpeed,
            ),
        ]),
        Line::from(Span::styled(
            app.text(UiText::SharedSettingsHelp),
            app.theme.metadata,
        )),
    ])
    .block(themed_block(app, app.text(UiText::Controls)))
    .wrap(Wrap { trim: true });
    frame.render_widget(help, rows[2]);
}

fn setting_line(app: &App, label: &str, value: String, focus: CourseSettingFocus) -> Line<'static> {
    Line::from(setting_span(app, label, value, focus))
}

fn setting_span(app: &App, label: &str, value: String, focus: CourseSettingFocus) -> Span<'static> {
    let marker = if app.course_setting_focus == focus {
        "▶ "
    } else {
        "  "
    };
    Span::styled(
        format!("{marker}{label}: {value}"),
        if app.course_setting_focus == focus {
            app.theme.selection
        } else {
            app.theme.text_secondary
        },
    )
}

fn render_player_courses(app: &App, frame: &mut Frame<'_>, area: Rect, player: LocalPlayerId) {
    let Some(song) = app.selected_song() else {
        return;
    };
    let ready = app.local_course_selection.is_ready(player);
    let items = song
        .courses
        .iter()
        .map(|course| {
            let level = course
                .level
                .map_or_else(|| "?".to_owned(), |level| level.to_string());
            let entry = app.localizer().message(UiMessage::LocalCourseListEntry {
                name: &course.name,
                level: &level,
                notes: course.object_count,
            });
            ListItem::new(Line::from(Span::styled(
                truncate_to_width(&entry, usize::from(area.width.saturating_sub(5))),
                app.theme.text_primary,
            )))
        })
        .collect::<Vec<_>>();
    let status = app.text(if ready {
        UiText::Ready
    } else {
        UiText::Choosing
    });
    let title = format!("{} — {status}", player.label());
    let list = List::new(items)
        .block(themed_block(app, &title))
        .highlight_style(if ready {
            app.theme.success
        } else {
            app.theme.selection
        })
        .highlight_symbol(">> ");
    let mut state = ListState::default();
    state.select(
        (!song.courses.is_empty()).then_some(app.local_course_selection.course_index(player)),
    );
    frame.render_stateful_widget(list, area, &mut state);
}

fn themed_block<'a>(app: &App, title: &'a str) -> Block<'a> {
    Block::default()
        .borders(Borders::ALL)
        .border_style(app.theme.border)
        .title(title)
        .title_style(app.theme.title)
}
