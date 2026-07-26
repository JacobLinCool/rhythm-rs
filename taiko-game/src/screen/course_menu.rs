use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap};

use crate::app::{App, CourseSettingFocus};
use crate::localization::{truncate_to_width, UiMessage, UiText};
use crate::tui::Frame;

pub fn render(app: &App, frame: &mut Frame<'_>, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(45), Constraint::Percentage(55)])
        .split(area);

    let Some(song) = app.selected_song() else {
        let empty = Paragraph::new(line_value(app, app.text(UiText::NoSelectedSong)))
            .block(themed_block(app, app.text(UiText::CourseMenu)))
            .style(app.theme.text_primary);
        frame.render_widget(empty, area);
        return;
    };

    let items = song
        .courses
        .iter()
        .map(|course| {
            let level = course
                .level
                .map_or_else(|| "?".to_owned(), |v| v.to_string());
            let entry = app.localizer().message(UiMessage::CourseListEntry {
                index: course.index + 1,
                name: &course.name,
                level: &level,
                objects: course.object_count,
            });
            ListItem::new(Line::from(Span::styled(
                truncate_to_width(&entry, usize::from(chunks[0].width.saturating_sub(5))),
                app.theme.text_primary,
            )))
        })
        .collect::<Vec<_>>();

    let list = List::new(items)
        .block(themed_block(app, app.text(UiText::CourseMenuTitle)))
        .highlight_style(app.theme.selection)
        .highlight_symbol(">> ");

    let mut state = ListState::default();
    state.select((!song.courses.is_empty()).then_some(app.course_index));
    frame.render_stateful_widget(list, chunks[0], &mut state);

    let mut lines = vec![
        kv_line(app, app.text(UiText::Song), song.title.clone()),
        kv_line(app, app.text(UiText::Subtitle), song.subtitle.clone()),
        kv_line(app, app.text(UiText::Artist), song.artist.clone()),
        line_value(app, ""),
        line_value(app, app.text(UiText::CourseSettingsHelp)),
        setting_line(
            app,
            app.text(UiText::AutoPlaySetting),
            if app.auto_play {
                app.text(UiText::On).to_owned()
            } else {
                app.text(UiText::Off).to_owned()
            },
            CourseSettingFocus::AutoPlay,
        ),
        setting_line(
            app,
            app.text(UiText::MusicVolume),
            format!("{}%", app.args.songvol),
            CourseSettingFocus::SongVolume,
        ),
        setting_line(
            app,
            app.text(UiText::SeVolume),
            format!("{}%", app.args.sevol),
            CourseSettingFocus::SeVolume,
        ),
        setting_line(
            app,
            app.text(UiText::Calibration),
            app.calibration_offset_label(),
            CourseSettingFocus::CalibrationOffset,
        ),
        setting_line(
            app,
            app.text(UiText::ScrollSpeed),
            app.scroll_speed_label(),
            CourseSettingFocus::ScrollSpeed,
        ),
        Line::from(vec![
            Span::styled(format!("{}: ", app.text(UiText::Tip)), app.theme.label),
            Span::styled(app.text(UiText::CourseSettingsTip), app.theme.metadata),
        ]),
        line_value(app, ""),
    ];

    if let Some(course) = app.selected_course() {
        let stars = course
            .level
            .map_or_else(|| "?".to_owned(), |level| level.to_string());
        lines.push(kv_line(
            app,
            app.text(UiText::SelectedCourse),
            course.name.clone(),
        ));
        lines.push(kv_line(app, app.text(UiText::Stars), stars));
        lines.push(kv_line(
            app,
            app.text(UiText::NoteObjectCount),
            course.object_count.to_string(),
        ));
        lines.push(kv_line(
            app,
            app.text(UiText::Branching),
            app.text(if course.branch_segment_count > 0 {
                UiText::Yes
            } else {
                UiText::No
            })
            .to_owned(),
        ));
    }

    let info = Paragraph::new(lines)
        .block(themed_block(app, app.text(UiText::CourseInfo)))
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

fn setting_line(app: &App, label: &str, value: String, focus: CourseSettingFocus) -> Line<'static> {
    let (label_style, value_style): (Style, Style) = if app.course_setting_focus == focus {
        (app.theme.selection, app.theme.selection)
    } else {
        (app.theme.label, app.theme.value)
    };
    Line::from(vec![
        Span::styled(format!("{label}: "), label_style),
        Span::styled(value, value_style),
    ])
}
