use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use taiko_multiplayer_protocol::{PlayerConnection, PlayerSnapshot, RoomStage};

use crate::app::App;
use crate::localization::{Localizer, UiText};
use crate::preferences::DrumBindings;
use crate::screen::game_screen::{render_lane_view, LaneRenderOptions};
use crate::tui::Frame;

const START_CUE_DURATION_US: u64 = 750_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CountdownCue {
    Ready,
    Three,
    Two,
    One,
    Start,
}

impl CountdownCue {
    const fn label(self, localizer: Localizer) -> &'static str {
        match self {
            Self::Ready => localizer.text(UiText::GetReady),
            Self::Three => "3",
            Self::Two => "2",
            Self::One => "1",
            Self::Start => localizer.text(UiText::Start),
        }
    }
}

pub fn render(app: &App, frame: &mut Frame<'_>, area: Rect) {
    let Some(online) = &app.online else {
        return;
    };
    let cue = online
        .snapshot
        .as_ref()
        .and_then(|snapshot| countdown_cue(&snapshot.stage, online.estimated_server_now_us()));
    let header_height = if cue.is_some() { 6 } else { 3 };
    let sections = Layout::vertical([
        Constraint::Length(header_height),
        Constraint::Min(1),
        Constraint::Length(2),
    ])
    .split(area);
    render_match_header(app, frame, sections[0], cue);

    let Some(snapshot) = &online.snapshot else {
        render_waiting(
            app,
            frame,
            sections[1],
            app.text(UiText::WaitingForRoomSnapshot),
        );
        return;
    };

    let local_id = online.local_player_id();
    let local_player = local_id.and_then(|player_id| {
        snapshot
            .players
            .iter()
            .find(|player| player.player_id == player_id)
    });
    if let Some(local_player) = local_player {
        let columns = Layout::horizontal([Constraint::Percentage(72), Constraint::Percentage(28)])
            .split(sections[1]);
        render_local_player(app, frame, columns[0], local_player);
        let remotes = snapshot
            .players
            .iter()
            .filter(|player| Some(player.player_id) != local_id)
            .collect::<Vec<_>>();
        render_remote_column(app, frame, columns[1], &remotes);
    } else {
        render_spectator_scoreboard(app, frame, sections[1], &snapshot.players);
    }
    render_match_controls(app, frame, sections[2], local_player.is_some());
}

fn render_match_header(app: &App, frame: &mut Frame<'_>, area: Rect, cue: Option<CountdownCue>) {
    let Some(online) = &app.online else {
        return;
    };
    let mut lines = vec![
        Line::from(vec![
            Span::styled(format!("{}  ", app.text(UiText::Match)), app.theme.label),
            Span::styled(
                app.localizer().online_phase(online.phase()),
                app.theme.selection,
            ),
        ]),
        online.current_song().map_or_else(
            || {
                Line::from(Span::styled(
                    app.text(UiText::SongManifestPending),
                    app.theme.metadata,
                ))
            },
            |song| {
                Line::from(vec![
                    Span::styled("♪ ", app.theme.label),
                    Span::styled(song.title.as_str().to_owned(), app.theme.title),
                    if song.subtitle.as_str().is_empty() {
                        Span::raw("")
                    } else {
                        Span::styled(
                            format!("  {}", song.subtitle.as_str()),
                            app.theme.text_secondary,
                        )
                    },
                ])
            },
        ),
    ];
    if let Some(cue) = cue {
        lines.push(Line::from(""));
        lines.push(
            Line::from(Span::styled(cue.label(app.localizer()), app.theme.warning)).centered(),
        );
    }
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: true }), area);
}

fn render_match_controls(app: &App, frame: &mut Frame<'_>, area: Rect, is_player: bool) {
    let mut spans = Vec::new();
    if is_player {
        let bindings = app.preferences.player_one;
        spans.push(Span::styled(
            online_binding_summary(app.localizer(), bindings),
            app.theme.metadata,
        ));
        spans.push(Span::styled("    ", app.theme.text_secondary));
    }
    spans.push(Span::styled(
        app.text(UiText::OnlineGameControlsHelp),
        app.theme.text_secondary,
    ));
    frame.render_widget(
        Paragraph::new(Line::from(spans)).wrap(Wrap { trim: true }),
        area,
    );
}

fn online_binding_summary(localizer: Localizer, bindings: DrumBindings) -> String {
    format!(
        "{}={}  {}={}  {}={}  {}={}",
        bindings.left_kat.to_ascii_uppercase(),
        localizer.text(UiText::BindingLeftKat).to_uppercase(),
        bindings.left_don.to_ascii_uppercase(),
        localizer.text(UiText::BindingLeftDon).to_uppercase(),
        bindings.right_don.to_ascii_uppercase(),
        localizer.text(UiText::BindingRightDon).to_uppercase(),
        bindings.right_kat.to_ascii_uppercase(),
        localizer.text(UiText::BindingRightKat).to_uppercase(),
    )
}

fn render_local_player(app: &App, frame: &mut Frame<'_>, area: Rect, player: &PlayerSnapshot) {
    let dnf = matches!(player.connection, PlayerConnection::Dnf);
    let block = player_block(app, player, true, dnf);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.width < 4 || inner.height < 4 {
        return;
    }
    let Some(online) = &app.online else {
        return;
    };
    let Some(runtime) = &online.local_player else {
        render_waiting(
            app,
            frame,
            inner,
            app.localizer().online_phase(online.phase()),
        );
        return;
    };
    let split = Layout::vertical([Constraint::Length(2), Constraint::Min(1)]).split(inner);
    let score = &runtime.last_output.score;
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(format!("{}  ", app.text(UiText::Score)), app.theme.label),
            Span::styled(score.score.to_string(), app.theme.value),
            Span::styled(
                format!("    {}  ", app.text(UiText::Combo)),
                app.theme.label,
            ),
            Span::styled(score.combo.to_string(), app.theme.value),
            Span::styled(format!("    {}  ", app.text(UiText::Soul)), app.theme.label),
            Span::styled(format!("{:.1}%", score.gauge * 100.0), app.theme.value),
        ])),
        split[0],
    );
    render_lane_view(
        &app.theme,
        frame,
        split[1],
        &runtime.last_output.frame_view,
        LaneRenderOptions {
            scroll_speed: app.effective_scroll_speed(),
            paused: false,
            paused_label: app.text(UiText::PauseLane),
            gogo_label: app.text(UiText::GoGoLane),
            judge_flash: runtime.judge_flash.map(|flash| flash.judge),
            input_flash: runtime.input_flash.map(|flash| flash.action),
        },
    );
}

fn render_remote_column(app: &App, frame: &mut Frame<'_>, area: Rect, players: &[&PlayerSnapshot]) {
    if players.is_empty() {
        frame.render_widget(
            Paragraph::new(app.text(UiText::WaitingOtherPlayers))
                .style(app.theme.metadata)
                .block(themed_block(app, app.text(UiText::OtherPlayers))),
            area,
        );
        return;
    }
    let count = u32::try_from(players.len()).unwrap_or(1);
    let rows = Layout::vertical(
        players
            .iter()
            .map(|_| Constraint::Ratio(1, count))
            .collect::<Vec<_>>(),
    )
    .split(area);
    for (player, player_area) in players.iter().zip(rows.iter().copied()) {
        render_score_card(app, frame, player_area, player);
    }
}

fn render_spectator_scoreboard(
    app: &App,
    frame: &mut Frame<'_>,
    area: Rect,
    players: &[PlayerSnapshot],
) {
    if players.is_empty() {
        render_waiting(app, frame, area, app.text(UiText::WaitingPlayers));
        return;
    }
    let row_count = players.len().div_ceil(2);
    let rows = Layout::vertical(
        (0..row_count)
            .map(|_| Constraint::Ratio(1, u32::try_from(row_count).unwrap_or(1)))
            .collect::<Vec<_>>(),
    )
    .split(area);
    for (row_index, row_area) in rows.iter().copied().enumerate() {
        let start = row_index * 2;
        let end = (start + 2).min(players.len());
        let row_players = &players[start..end];
        let columns = Layout::horizontal(
            row_players
                .iter()
                .map(|_| Constraint::Ratio(1, u32::try_from(row_players.len()).unwrap_or(1)))
                .collect::<Vec<_>>(),
        )
        .split(row_area);
        for (player, player_area) in row_players.iter().zip(columns.iter().copied()) {
            render_score_card(app, frame, player_area, player);
        }
    }
}

fn render_score_card(app: &App, frame: &mut Frame<'_>, area: Rect, player: &PlayerSnapshot) {
    let dnf = matches!(player.connection, PlayerConnection::Dnf);
    let block = player_block(app, player, false, dnf);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let Some(online) = &app.online else {
        return;
    };
    if let Some(state) = online.live_states.get(&player.player_id) {
        let score = &state.score;
        let gauge_percent = f64::from(score.gauge_ppm) / 10_000.0;
        frame.render_widget(
            Paragraph::new(vec![
                Line::from(vec![
                    Span::styled(format!("{} ", app.text(UiText::Score)), app.theme.label),
                    Span::styled(score.score.to_string(), app.theme.value),
                ]),
                Line::from(vec![
                    Span::styled(format!("{} ", app.text(UiText::Combo)), app.theme.label),
                    Span::styled(score.combo.to_string(), app.theme.value),
                    Span::styled(format!("  {} ", app.text(UiText::Soul)), app.theme.label),
                    Span::styled(format!("{gauge_percent:.1}%"), app.theme.value),
                ]),
                Line::from(vec![
                    Span::styled(
                        format!(
                            "{}/{}/{} ",
                            app.text(UiText::Great),
                            app.text(UiText::Ok),
                            app.text(UiText::Miss),
                        ),
                        app.theme.label,
                    ),
                    Span::styled(
                        format!("{}/{}/{}", score.great, score.ok, score.miss),
                        app.theme.value,
                    ),
                ]),
            ])
            .wrap(Wrap { trim: true }),
            inner,
        );
    } else {
        render_waiting(app, frame, inner, app.text(UiText::WaitingLiveScore));
    }
}

fn player_block<'a>(app: &App, player: &'a PlayerSnapshot, local: bool, dnf: bool) -> Block<'a> {
    Block::default()
        .borders(Borders::ALL)
        .border_style(if local {
            app.theme.selection
        } else {
            app.theme.border
        })
        .title(format!(
            "{}{}{}{}",
            player.name,
            if local {
                format!(" [{}]", app.text(UiText::You))
            } else {
                String::new()
            },
            if player.is_leader {
                format!(" [{}]", app.text(UiText::Leader))
            } else {
                String::new()
            },
            if dnf {
                format!(" [{}]", app.text(UiText::Dnf))
            } else {
                String::new()
            }
        ))
        .title_style(if local {
            app.theme.selection
        } else {
            app.theme.title
        })
}

fn countdown_cue(stage: &RoomStage, server_now_us: u64) -> Option<CountdownCue> {
    match stage {
        RoomStage::Countdown {
            start_at_server_us, ..
        } => Some(countdown_cue_before_start(
            *start_at_server_us,
            server_now_us,
        )),
        RoomStage::Playing {
            start_at_server_us, ..
        } if server_now_us.saturating_sub(*start_at_server_us) < START_CUE_DURATION_US => {
            Some(CountdownCue::Start)
        }
        RoomStage::Lobby
        | RoomStage::Preparing { .. }
        | RoomStage::Playing { .. }
        | RoomStage::Finalizing { .. }
        | RoomStage::Finished { .. } => None,
    }
}

fn countdown_cue_before_start(start_at_server_us: u64, server_now_us: u64) -> CountdownCue {
    let remaining = start_at_server_us.saturating_sub(server_now_us);
    match remaining {
        0 => CountdownCue::Start,
        1..=1_000_000 => CountdownCue::One,
        1_000_001..=2_000_000 => CountdownCue::Two,
        2_000_001..=3_000_000 => CountdownCue::Three,
        _ => CountdownCue::Ready,
    }
}

fn themed_block<'a>(app: &App, title: &'a str) -> Block<'a> {
    Block::default()
        .borders(Borders::ALL)
        .border_style(app.theme.border)
        .title(title)
        .title_style(app.theme.title)
}

fn render_waiting(app: &App, frame: &mut Frame<'_>, area: Rect, message: &str) {
    frame.render_widget(
        Paragraph::new(message)
            .style(app.theme.metadata)
            .wrap(Wrap { trim: true }),
        area,
    );
}

#[cfg(test)]
mod tests {
    use super::{countdown_cue_before_start, online_binding_summary, CountdownCue};
    use crate::localization::{Localizer, UiText};
    use crate::preferences::{DrumBindings, UiLanguage};

    #[test]
    fn authoritative_countdown_has_exact_three_two_one_start_boundaries() {
        let start = 10_000_000;
        assert_eq!(
            countdown_cue_before_start(start, 6_999_999),
            CountdownCue::Ready
        );
        assert_eq!(
            countdown_cue_before_start(start, 7_000_000),
            CountdownCue::Three
        );
        assert_eq!(
            countdown_cue_before_start(start, 8_000_000),
            CountdownCue::Two
        );
        assert_eq!(
            countdown_cue_before_start(start, 9_000_000),
            CountdownCue::One
        );
        assert_eq!(
            countdown_cue_before_start(start, start),
            CountdownCue::Start
        );
    }

    #[test]
    fn online_player_footer_exposes_all_four_bindings_and_leave_control() {
        for language in UiLanguage::ALL {
            let localizer = Localizer::new(language);
            let summary = online_binding_summary(localizer, DrumBindings::player_one_default());
            for key in ['A', 'S', 'D', 'F'] {
                assert!(summary.contains(key));
            }
            assert!(!localizer.text(UiText::OnlineGameControlsHelp).is_empty());
        }
    }
}
