use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use taiko_multiplayer_protocol::RoomPlayerSnapshot;

use crate::app::App;
use crate::screen::game_screen::{render_lane_view, LaneRenderOptions};
use crate::tui::Frame;

pub fn render(app: &App, frame: &mut Frame<'_>, area: Rect) {
    let Some(online) = &app.online else {
        return;
    };

    let Some(snapshot) = &online.snapshot else {
        let widget = Paragraph::new("Waiting for room snapshot")
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(app.theme.border),
            )
            .wrap(Wrap { trim: true });
        frame.render_widget(widget, area);
        return;
    };

    let player_count = snapshot.players.len().max(1) as u32;
    let row_constraints: Vec<_> = (0..player_count)
        .map(|_| Constraint::Ratio(1, player_count))
        .collect();
    let row_areas = Layout::default()
        .direction(Direction::Vertical)
        .constraints(row_constraints)
        .split(area);

    for (i, player) in snapshot.players.iter().enumerate() {
        render_player_tile(app, frame, row_areas[i], player);
    }
}

fn render_player_tile(app: &App, frame: &mut Frame<'_>, area: Rect, player: &RoomPlayerSnapshot) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(app.theme.border)
        .title(format!(
            "{}{}{}",
            player.name,
            if player.is_host { " [HOST]" } else { "" },
            if player.dnf { " [DNF]" } else { "" }
        ))
        .title_style(app.theme.title);
    frame.render_widget(block, area);

    let inner = Rect {
        x: area.x + 1,
        y: area.y + 1,
        width: area.width.saturating_sub(2),
        height: area.height.saturating_sub(2),
    };
    if inner.width < 4 || inner.height < 6 {
        return;
    }

    let Some(online) = &app.online else {
        return;
    };

    // For the local player, use local engine output; for others, use live_states
    let is_local = online.actor_id.as_deref() == Some(player.player_id.as_str());
    let (frame_view, score, lane_opts) = if is_local {
        if let Some(runtime) = &online.local_player {
            (
                runtime.last_output.frame_view.clone(),
                runtime.last_output.score.clone(),
                LaneRenderOptions {
                    scroll_speed: 1.0,
                    paused: false,
                    judge_flash: runtime.judge_flash.map(|f| f.judge),
                    input_flash: runtime.input_flash.map(|f| f.action),
                },
            )
        } else {
            render_waiting(app, frame, inner);
            return;
        }
    } else if let Some(state) = online
        .live_states
        .get(&player.player_id)
        .or(player.last_state.as_ref())
    {
        let now_tick = state.now_tick;
        let flash = online.remote_flash_for(&player.player_id, now_tick);
        let judge_flash = flash.and_then(|f| {
            if f.judge_until > now_tick {
                f.judge
            } else {
                None
            }
        });
        let input_flash = flash.and_then(|f| {
            if f.input_until > now_tick {
                f.input_action
            } else {
                None
            }
        });
        (
            state.frame_view.clone(),
            state.score.clone(),
            LaneRenderOptions {
                scroll_speed: 1.0,
                paused: false,
                judge_flash,
                input_flash,
            },
        )
    } else {
        render_waiting(app, frame, inner);
        return;
    };

    let split = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(2), Constraint::Min(5)])
        .split(inner);

    let header = Paragraph::new(vec![Line::from(vec![
        Span::styled("Score ", app.theme.label),
        Span::styled(format!("{}", score.score), app.theme.value),
        Span::styled(" | Combo ", app.theme.label),
        Span::styled(format!("{}", score.combo), app.theme.value),
        Span::styled(" | Gauge ", app.theme.label),
        Span::styled(format!("{:.1}%", score.gauge * 100.0), app.theme.value),
    ])]);
    frame.render_widget(header, split[0]);

    render_lane_view(&app.theme, frame, split[1], &frame_view, lane_opts);
}

fn render_waiting(app: &App, frame: &mut Frame<'_>, area: Rect) {
    let widget = Paragraph::new("waiting...")
        .style(app.theme.metadata)
        .wrap(Wrap { trim: true });
    frame.render_widget(widget, area);
}
