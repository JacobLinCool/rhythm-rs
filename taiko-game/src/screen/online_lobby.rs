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
        .constraints([Constraint::Percentage(56), Constraint::Percentage(44)])
        .split(area);

    // Left: song list (reuse app's filtered songs)
    let items: Vec<ListItem<'_>> = app
        .filtered_song_indices
        .iter()
        .map(|&idx| {
            let song = &app.songs[idx];
            let locked = online
                .snapshot
                .as_ref()
                .and_then(|s| s.song.as_ref())
                .is_some_and(|sel| {
                    app.songs.iter().position(|s| s.title == sel.title) == Some(idx)
                });
            let label = if locked {
                format!("[LOCKED] {}", song.title)
            } else {
                song.title.clone()
            };
            ListItem::new(label)
        })
        .collect();

    let title = if online.is_local_host() {
        "Songs (↑↓ select, Enter lock)"
    } else {
        "Songs (waiting for host)"
    };

    let mut list_state = ListState::default();
    list_state.select(Some(app.song_index));
    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(app.theme.border)
                .title(title)
                .title_style(app.theme.title),
        )
        .highlight_symbol(">> ")
        .highlight_style(app.theme.selection);
    frame.render_stateful_widget(list, chunks[0], &mut list_state);

    // Right: room info + player list
    let mut lines: Vec<Line<'_>> = Vec::new();

    if let Some(room_code) = &online.room_code {
        lines.push(Line::from(vec![
            Span::styled("Room: ", app.theme.metadata),
            Span::styled(room_code.clone(), app.theme.text_primary),
        ]));
    }

    lines.push(Line::from(vec![
        Span::styled("Phase: ", app.theme.metadata),
        Span::styled(
            format!("{:?}", online.current_phase()),
            app.theme.text_primary,
        ),
    ]));

    lines.push(Line::from(vec![
        Span::styled("Status: ", app.theme.metadata),
        Span::styled(online.status_message.clone(), app.theme.text_secondary),
    ]));

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled("Players:", app.theme.metadata)));

    if let Some(snapshot) = &online.snapshot {
        for player in &snapshot.players {
            let status = if player.dnf {
                " [DNF]"
            } else if player.ready {
                " [READY]"
            } else if !player.online {
                " [OFFLINE]"
            } else {
                ""
            };
            let host_marker = if player.is_host { " (host)" } else { "" };
            lines.push(Line::from(Span::styled(
                format!("  {} {}{}{}", player.player_id, player.name, host_marker, status),
                app.theme.text_primary,
            )));
        }
    }

    // Selected song info
    if let Some(song) = app.selected_song() {
        lines.push(Line::from(""));
        lines.push(Line::from(vec![
            Span::styled("Title: ", app.theme.metadata),
            Span::styled(&*song.title, app.theme.text_primary),
        ]));
        if !song.subtitle.is_empty() {
            lines.push(Line::from(vec![
                Span::styled("Subtitle: ", app.theme.metadata),
                Span::styled(&*song.subtitle, app.theme.text_secondary),
            ]));
        }
        lines.push(Line::from(vec![
            Span::styled("Courses: ", app.theme.metadata),
            Span::styled(
                song.courses
                    .iter()
                    .map(|c| format!("{} ({})", c.name, c.level.map_or("?".to_owned(), |l| l.to_string())))
                    .collect::<Vec<_>>()
                    .join(", "),
                app.theme.text_secondary,
            ),
        ]));
    }

    // Search query
    if !app.song_query.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(vec![
            Span::styled("Filter: ", app.theme.metadata),
            Span::styled(&*app.song_query, app.theme.text_primary),
        ]));
    }

    let info = Paragraph::new(lines)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(app.theme.border)
                .title("Room Info")
                .title_style(app.theme.title),
        )
        .wrap(Wrap { trim: true });
    frame.render_widget(info, chunks[1]);
}
