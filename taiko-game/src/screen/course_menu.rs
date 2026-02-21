use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap};
use rhythm_chart::{format_accuracy_threshold, BranchDecisionHint};

use crate::app::{App, CourseSettingFocus};
use crate::tui::Frame;

pub fn render(app: &App, frame: &mut Frame<'_>, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(45), Constraint::Percentage(55)])
        .split(area);

    let Some(song) = app.selected_song() else {
        let empty = Paragraph::new(line_value(app, "No selected song"))
            .block(themed_block(app, "Course Menu"))
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
            ListItem::new(Line::from(Span::styled(
                format!(
                    "{:>2}. {:<12} Lv {}  notes={}",
                    course.index + 1,
                    course.name,
                    level,
                    course.object_count
                ),
                app.theme.text_primary,
            )))
        })
        .collect::<Vec<_>>();

    let list = List::new(items)
        .block(themed_block(
            app,
            "Course Menu (Enter/Don to start, Esc to back)",
        ))
        .highlight_style(app.theme.selection)
        .highlight_symbol(">> ");

    let mut state = ListState::default();
    state.select((!song.courses.is_empty()).then_some(app.course_index));
    frame.render_stateful_widget(list, chunks[0], &mut state);

    let mut lines = vec![
        kv_line(app, "Song", song.title.clone()),
        kv_line(app, "Subtitle", song.subtitle.clone()),
        kv_line(app, "Artist", song.artist.clone()),
        line_value(app, ""),
        line_value(app, "Settings (Tab/Shift+Tab focus, Left/Right adjust):"),
        setting_line(
            app,
            "Auto Play",
            if app.auto_play {
                "ON".to_owned()
            } else {
                "OFF".to_owned()
            },
            CourseSettingFocus::AutoPlay,
        ),
        setting_line(
            app,
            "Music Volume",
            format!("{}%", app.args.songvol),
            CourseSettingFocus::SongVolume,
        ),
        setting_line(
            app,
            "SE Volume",
            format!("{}%", app.args.sevol),
            CourseSettingFocus::SeVolume,
        ),
        setting_line(
            app,
            "Note Offset",
            app.note_offset_label(),
            CourseSettingFocus::NoteOffset,
        ),
        setting_line(
            app,
            "Music Offset",
            app.music_offset_label(),
            CourseSettingFocus::MusicOffset,
        ),
        setting_line(
            app,
            "Scroll Speed",
            app.scroll_speed_label(),
            CourseSettingFocus::ScrollSpeed,
        ),
        Line::from(vec![
            Span::styled("Total Offset: ", app.theme.label),
            Span::styled(app.total_offset_label(), app.theme.value),
        ]),
        Line::from(vec![
            Span::styled("Tip: ", app.theme.label),
            Span::styled(
                "Tab focus setting, Left/Right adjust (5ms step, ±500ms), Up/Down select course",
                app.theme.metadata,
            ),
        ]),
        line_value(app, ""),
    ];

    if let Some(course) = app.selected_course() {
        lines.push(kv_line(app, "Selected", course.name.clone()));
        lines.push(kv_line(app, "Objects", course.object_count.to_string()));
        lines.push(kv_line(
            app,
            "Branch Segments",
            course.branch_segment_count.to_string(),
        ));
        lines.push(line_value(app, ""));

        if course.branch_decisions.is_empty() {
            lines.push(line_value(app, "Branch Decision: (none)"));
        } else {
            lines.push(line_value(app, "Branch Decision Table:"));
            for decision in &course.branch_decisions {
                lines.push(Line::from(vec![
                    Span::styled("- ", app.theme.text_secondary),
                    Span::styled(
                        format!(
                            "seg={} tick={:.3}s route_count={} hint={}",
                            decision.segment_id,
                            decision.decision_tick as f64 / 1_000_000.0,
                            decision.route_count,
                            format_hint(decision.hint.as_ref())
                        ),
                        app.theme.metadata,
                    ),
                ]));
            }
        }
    }

    let info = Paragraph::new(lines)
        .block(themed_block(app, "Course Info"))
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

fn format_hint(hint: Option<&BranchDecisionHint>) -> String {
    match hint {
        Some(BranchDecisionHint::Accuracy { low, high }) => format!(
            "p,{},{}",
            format_accuracy_threshold(*low),
            format_accuracy_threshold(*high)
        ),
        Some(BranchDecisionHint::Roll { low, high }) => format!("r,{low},{high}"),
        Some(BranchDecisionHint::Score { low, high }) => format!("s,{low},{high}"),
        Some(BranchDecisionHint::Raw(raw)) => format!("raw:{raw}"),
        None => "none".to_owned(),
    }
}
