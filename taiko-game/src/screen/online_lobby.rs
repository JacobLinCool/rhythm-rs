use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap};

use crate::app::{App, InviteCopyStatus};
use crate::localization::{UiMessage, UiText};
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
                .current_song()
                .is_some_and(|manifest| song.song_id() == Some(manifest.song_id.as_str()));
            let label = if locked {
                format!("[{}] {}", app.text(UiText::Locked), song.title)
            } else {
                song.title.clone()
            };
            ListItem::new(label)
        })
        .collect();

    let title = if online.is_local_leader() {
        app.text(UiText::LobbySongsLeader)
    } else {
        app.text(UiText::LobbySongsWaiting)
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

    if let Some(room_code) = online.room_code() {
        lines.push(Line::from(vec![
            Span::styled(format!("{}: ", app.text(UiText::Room)), app.theme.metadata),
            Span::styled(room_code.as_str().to_owned(), app.theme.text_primary),
        ]));
    }

    lines.push(Line::from(vec![
        Span::styled(format!("{}: ", app.text(UiText::Phase)), app.theme.metadata),
        Span::styled(
            app.localizer().online_phase(online.phase()),
            app.theme.text_primary,
        ),
    ]));

    if let Some(status) = app.demo_preview_status_text() {
        lines.push(Line::from(vec![
            Span::styled(
                format!("{}: ", app.text(UiText::Preview)),
                app.theme.metadata,
            ),
            Span::styled(status, app.theme.warning),
        ]));
    }

    if let Some(invite) = online.invite() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            app.text(UiText::InviteSecretNotice),
            app.theme.metadata,
        )));
        if app.invite_revealed {
            lines.push(Line::from(Span::styled(
                invite.to_string(),
                app.theme.text_secondary,
            )));
            lines.push(Line::from(Span::styled(
                app.text(UiText::HideAndCopyInvite),
                app.theme.metadata,
            )));
        } else {
            lines.push(Line::from(Span::styled(
                app.text(UiText::HiddenInvite),
                app.theme.warning,
            )));
            lines.push(Line::from(Span::styled(
                app.text(UiText::RevealAndCopyInvite),
                app.theme.metadata,
            )));
        }
        if let Some(status) = &app.invite_copy_status {
            let (message, style) = match status {
                InviteCopyStatus::Copied => {
                    (app.text(UiText::InviteCopied).to_owned(), app.theme.success)
                }
                InviteCopyStatus::Failed(error) => (
                    format!("{}: {error}", app.text(UiText::CopyFailed)),
                    app.theme.error,
                ),
            };
            lines.push(Line::from(Span::styled(message, style)));
        }
    }

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        format!("{}:", app.text(UiText::Players)),
        app.theme.metadata,
    )));

    if let Some(snapshot) = &online.snapshot {
        for player in &snapshot.players {
            let status = if matches!(
                &player.connection,
                taiko_multiplayer_protocol::PlayerConnection::Dnf
            ) {
                format!(" [{}]", app.text(UiText::Dnf))
            } else if matches!(
                &player.preparation,
                taiko_multiplayer_protocol::PlayerPreparation::Ready { .. }
            ) {
                format!(" [{}]", app.text(UiText::Ready))
            } else if matches!(
                &player.connection,
                taiko_multiplayer_protocol::PlayerConnection::Reconnecting { .. }
            ) {
                format!(" [{}]", app.text(UiText::Reconnecting))
            } else {
                String::new()
            };
            let leader_marker = if player.is_leader {
                format!(" ({})", app.text(UiText::Leader))
            } else {
                String::new()
            };
            lines.push(Line::from(Span::styled(
                format!(
                    "  {} {}{}{}",
                    player.player_id.0, player.name, leader_marker, status
                ),
                app.theme.text_primary,
            )));
        }
    }

    // The room manifest is the authoritative source, including for spectators
    // that intentionally do not install the playable resource library.
    if let Some(song) = online.current_song() {
        lines.push(Line::from(""));
        lines.push(Line::from(vec![
            Span::styled(format!("{}: ", app.text(UiText::Title)), app.theme.metadata),
            Span::styled(song.title.as_str().to_owned(), app.theme.text_primary),
        ]));
        if !song.subtitle.as_str().is_empty() {
            lines.push(Line::from(vec![
                Span::styled(
                    format!("{}: ", app.text(UiText::Subtitle)),
                    app.theme.metadata,
                ),
                Span::styled(song.subtitle.as_str().to_owned(), app.theme.text_secondary),
            ]));
        }
        if !song.artist.as_str().is_empty() {
            lines.push(Line::from(vec![
                Span::styled(
                    format!("{}: ", app.text(UiText::Artist)),
                    app.theme.metadata,
                ),
                Span::styled(song.artist.as_str().to_owned(), app.theme.text_secondary),
            ]));
        }
        lines.push(Line::from(vec![
            Span::styled(
                format!("{}: ", app.text(UiText::Courses)),
                app.theme.metadata,
            ),
            Span::styled(
                song.courses
                    .iter()
                    .map(|course| {
                        let level = course
                            .level
                            .map_or("?".to_owned(), |level| level.to_string());
                        app.localizer().message(UiMessage::CourseWithLevel {
                            name: course.name.as_str(),
                            level: &level,
                        })
                    })
                    .collect::<Vec<_>>()
                    .join(", "),
                app.theme.text_secondary,
            ),
        ]));
    } else if let Some(song) = app.selected_song() {
        lines.push(Line::from(""));
        lines.push(Line::from(vec![
            Span::styled(format!("{}: ", app.text(UiText::Title)), app.theme.metadata),
            Span::styled(&*song.title, app.theme.text_primary),
        ]));
        if !song.subtitle.is_empty() {
            lines.push(Line::from(vec![
                Span::styled(
                    format!("{}: ", app.text(UiText::Subtitle)),
                    app.theme.metadata,
                ),
                Span::styled(&*song.subtitle, app.theme.text_secondary),
            ]));
        }
        lines.push(Line::from(vec![
            Span::styled(
                format!("{}: ", app.text(UiText::Courses)),
                app.theme.metadata,
            ),
            Span::styled(
                song.courses
                    .iter()
                    .map(|c| {
                        let level = c.level.map_or("?".to_owned(), |l| l.to_string());
                        app.localizer().message(UiMessage::CourseWithLevel {
                            name: &c.name,
                            level: &level,
                        })
                    })
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
            Span::styled(
                format!("{}: ", app.text(UiText::Filter)),
                app.theme.metadata,
            ),
            Span::styled(&*app.song_query, app.theme.text_primary),
        ]));
    }

    let info = Paragraph::new(lines)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(app.theme.border)
                .title(app.text(UiText::RoomInfo))
                .title_style(app.theme.title),
        )
        .wrap(Wrap { trim: true });
    frame.render_widget(info, chunks[1]);
}
