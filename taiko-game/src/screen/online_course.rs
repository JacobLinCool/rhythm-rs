use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap};

use crate::app::App;
use crate::tui::Frame;

pub fn render(app: &App, frame: &mut Frame<'_>, area: Rect) {
    let Some(online) = &app.online else {
        return;
    };

    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(area);

    // Left: course list for the selected song
    let song = app.selected_song();
    let courses: Vec<ListItem<'_>> = song
        .map(|s| {
            s.courses
                .iter()
                .map(|c| {
                    let level = c.level.map_or("?".to_owned(), |l| l.to_string());
                    ListItem::new(format!("{} (Lv.{})", c.name, level))
                })
                .collect()
        })
        .unwrap_or_default();

    let mut list_state = ListState::default();
    list_state.select(Some(online.local_course_index));
    let list = List::new(courses)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(app.theme.border)
                .title("Select Course (↑↓ select, Enter confirm, Esc back)")
                .title_style(app.theme.title),
        )
        .highlight_symbol(">> ")
        .highlight_style(app.theme.selection);
    frame.render_stateful_widget(list, chunks[0], &mut list_state);

    // Right: player ready status
    let mut lines: Vec<Line<'_>> = Vec::new();

    if let Some(song) = song {
        lines.push(Line::from(vec![
            Span::styled("Song: ", app.theme.metadata),
            Span::styled(&*song.title, app.theme.text_primary),
        ]));
        lines.push(Line::from(""));
    }

    lines.push(Line::from(Span::styled("Players:", app.theme.metadata)));
    if let Some(snapshot) = &online.snapshot {
        for player in &snapshot.players {
            let status = if player.ready { "READY" } else { "selecting..." };
            let style = if player.ready {
                app.theme.judge_great
            } else {
                app.theme.text_secondary
            };
            lines.push(Line::from(vec![
                Span::styled(format!("  {} ", player.name), app.theme.text_primary),
                Span::styled(status, style),
            ]));
        }
    }

    let info = Paragraph::new(lines)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(app.theme.border)
                .title("Status")
                .title_style(app.theme.title),
        )
        .wrap(Wrap { trim: true });
    frame.render_widget(info, chunks[1]);
}
