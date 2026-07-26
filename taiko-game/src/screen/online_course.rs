use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap};
use taiko_multiplayer_protocol::{PlayerConnection, PlayerPreparation, PlayerSnapshot};

use crate::app::App;
use crate::localization::{Localizer, UiMessage, UiText};
use crate::tui::Frame;

pub fn render(app: &App, frame: &mut Frame<'_>, area: Rect) {
    let Some(online) = &app.online else {
        return;
    };

    let (course_area, status_area, controls_area) = online_course_regions(area);

    // Left: course list for the selected song
    let song = app.selected_song();
    let courses: Vec<ListItem<'_>> = song
        .map(|s| {
            s.courses
                .iter()
                .map(|c| {
                    let level = c.level.map_or("?".to_owned(), |l| l.to_string());
                    ListItem::new(app.localizer().message(UiMessage::LocalCourseListEntry {
                        name: &c.name,
                        level: &level,
                        notes: c.object_count,
                    }))
                })
                .collect()
        })
        .unwrap_or_default();

    let mut list_state = ListState::default();
    list_state.select(Some(online.local_course_index));
    let instructions = course_controls_hint(
        app.localizer(),
        online.room_controls_enabled(),
        online.can_start_match(),
        online.is_local_ready(),
    );
    let list = List::new(courses)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(app.theme.border)
                .title(app.text(UiText::SelectCourse))
                .title_style(app.theme.title),
        )
        .highlight_symbol(">> ")
        .highlight_style(app.theme.selection);
    frame.render_stateful_widget(list, course_area, &mut list_state);

    // Right: player ready status
    let mut lines: Vec<Line<'_>> = Vec::new();

    lines.push(Line::from(vec![
        Span::styled(format!("{}: ", app.text(UiText::Phase)), app.theme.metadata),
        Span::styled(
            app.localizer().online_phase(online.phase()),
            app.theme.text_primary,
        ),
    ]));
    lines.push(Line::from(""));

    if let Some(song) = song {
        lines.push(Line::from(vec![
            Span::styled(format!("{}: ", app.text(UiText::Song)), app.theme.metadata),
            Span::styled(&*song.title, app.theme.text_primary),
        ]));
        lines.push(Line::from(""));
    }
    if let Some(reason) = app.online_preparation_failure() {
        lines.push(Line::from(Span::styled(
            format!("{}: {reason}", app.text(UiText::LocalPreparationFailed)),
            app.theme.warning,
        )));
        lines.push(Line::from(Span::styled(
            app.text(UiText::FixContentRetry),
            app.theme.text_secondary,
        )));
        lines.push(Line::from(""));
    }

    lines.push(Line::from(Span::styled(
        format!("{}:", app.text(UiText::Players)),
        app.theme.metadata,
    )));
    if let Some(snapshot) = &online.snapshot {
        for player in &snapshot.players {
            let status = player_course_status(app.localizer(), player);
            let style = match &player.connection {
                PlayerConnection::Dnf => app.theme.error,
                PlayerConnection::Reconnecting { .. } => app.theme.warning,
                PlayerConnection::Online
                    if matches!(&player.preparation, PlayerPreparation::Ready { .. }) =>
                {
                    app.theme.judge_great
                }
                PlayerConnection::Online => app.theme.text_secondary,
            };
            lines.push(Line::from(vec![
                Span::styled(format!("  {} ", player.name), app.theme.text_primary),
                Span::styled(status, style),
            ]));
        }
    }
    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        Span::styled(format!("{}: ", app.text(UiText::Clock)), app.theme.metadata),
        Span::styled(
            app.text(if online.clock_is_ready() {
                UiText::ClockReady
            } else {
                UiText::ClockChecking
            }),
            if online.clock_is_ready() {
                app.theme.success
            } else {
                app.theme.warning
            },
        ),
    ]));
    if !online.clock_is_ready() {
        lines.push(Line::from(Span::styled(
            app.text(UiText::ClockCheckHelp),
            app.theme.warning,
        )));
    }

    let info = Paragraph::new(lines)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(app.theme.border)
                .title(app.text(UiText::Status))
                .title_style(app.theme.title),
        )
        .wrap(Wrap { trim: true });
    frame.render_widget(info, status_area);

    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            instructions,
            if online.room_controls_enabled() {
                app.theme.text_primary
            } else {
                app.theme.warning
            },
        )))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(app.theme.border)
                .title(app.text(UiText::Controls))
                .title_style(app.theme.title),
        )
        .wrap(Wrap { trim: true }),
        controls_area,
    );
}

fn online_course_regions(area: Rect) -> (Rect, Rect, Rect) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(3)])
        .split(area);
    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(rows[0]);
    (columns[0], columns[1], rows[1])
}

fn course_controls_hint(
    localizer: Localizer,
    controls_enabled: bool,
    can_start_match: bool,
    is_local_ready: bool,
) -> &'static str {
    if !controls_enabled {
        localizer.text(UiText::CourseControlsPaused)
    } else if can_start_match {
        localizer.text(UiText::AllReadyStart)
    } else if is_local_ready {
        localizer.text(UiText::ReadyWaitingOnline)
    } else {
        localizer.text(UiText::SelectCoursePrepare)
    }
}

fn player_course_status(localizer: Localizer, player: &PlayerSnapshot) -> String {
    match &player.connection {
        PlayerConnection::Dnf => localizer.text(UiText::Dnf).to_owned(),
        PlayerConnection::Reconnecting { .. } => localizer.text(UiText::Reconnecting).to_owned(),
        PlayerConnection::Online => match &player.preparation {
            PlayerPreparation::Selecting => localizer.text(UiText::Selecting).to_owned(),
            PlayerPreparation::Downloading { progress_milli, .. } => {
                let progress_milli = u32::from(progress_milli.get());
                localizer.message(UiMessage::DownloadingProgress {
                    whole_percent: progress_milli / 10,
                    tenths_percent: progress_milli % 10,
                })
            }
            PlayerPreparation::Verifying { .. } => {
                localizer.text(UiText::VerifyingHashes).to_owned()
            }
            PlayerPreparation::Loading { .. } => {
                localizer.text(UiText::LoadingChartAudio).to_owned()
            }
            PlayerPreparation::Prepared { .. } => {
                localizer.text(UiText::PreparedCheckingClock).to_owned()
            }
            PlayerPreparation::Ready { .. } => localizer.text(UiText::Ready).to_owned(),
            PlayerPreparation::Failed { reason, .. } => {
                format!("{}: {reason}", localizer.text(UiText::Failed))
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::preferences::UiLanguage;
    use ratatui::layout::Rect;
    use taiko_multiplayer_protocol::{DisplayName, PlayerId};

    fn player(connection: PlayerConnection) -> PlayerSnapshot {
        PlayerSnapshot {
            player_id: PlayerId(1),
            name: DisplayName::new("alice").expect("name"),
            is_leader: true,
            connection,
            preparation: PlayerPreparation::Selecting,
            last_acked_input_seq: None,
        }
    }

    #[test]
    fn reconnecting_player_is_not_presented_as_ready() {
        assert_eq!(
            player_course_status(
                Localizer::new(UiLanguage::English),
                &player(PlayerConnection::Reconnecting {
                    grace_deadline_server_us: 42,
                }),
            ),
            "RECONNECTING"
        );
    }

    #[test]
    fn reconnecting_transport_pauses_course_controls() {
        let localizer = Localizer::new(UiLanguage::English);
        assert_eq!(
            course_controls_hint(localizer, false, true, true),
            "Reconnecting — room controls paused"
        );
        assert_eq!(
            course_controls_hint(localizer, true, true, true),
            "All online and ready — Enter starts match, Esc unready"
        );
    }

    #[test]
    fn controls_use_a_full_width_footer_at_the_minimum_terminal_width() {
        let area = Rect::new(0, 0, 80, 23);
        let (courses, status, controls) = online_course_regions(area);
        assert_eq!(courses.width + status.width, area.width);
        assert_eq!(controls.x, area.x);
        assert_eq!(controls.width, area.width);
        assert_eq!(controls.height, 3);
    }
}
