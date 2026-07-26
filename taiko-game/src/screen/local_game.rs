use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

use super::game_screen::{judge_feedback, render_lane_view, LaneRenderOptions};
use super::render_gauge_bar_line;
use crate::app::App;
use crate::local_multiplayer::LocalPlayerId;
use crate::localization::UiText;
use crate::tui::Frame;

pub fn render(app: &App, frame: &mut Frame<'_>, area: Rect) {
    let Some(game) = app.local_game.as_ref() else {
        frame.render_widget(
            Paragraph::new(Span::styled(
                app.text(UiText::LocalSessionMissing),
                app.theme.error,
            ))
            .block(themed_block(app, app.text(UiText::LocalTwoPlayer))),
            area,
        );
        return;
    };

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(11), Constraint::Length(2)])
        .split(area);
    let player_areas = split_player_areas(rows[0]);

    for (player_id, player_area) in LocalPlayerId::ALL.into_iter().zip(player_areas) {
        let player_rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(5), Constraint::Min(6)])
            .split(player_area);
        let player = &game.players[player_id.index()];
        let score = &player.last_output.score;
        let (feedback, feedback_style) = judge_feedback(
            app.localizer(),
            &app.theme,
            game.paused,
            player.judge_flash.map(|(judge, _)| judge),
        );
        let player_title = format!(" {} // {} ", player_id.label(), player.course_name);
        let hud = Paragraph::new(vec![
            Line::from(vec![
                Span::styled(format!("{}  ", app.text(UiText::Score)), app.theme.label),
                Span::styled(format!("{:09}", score.score), app.theme.value),
                Span::styled(
                    format!("    {}  ", app.text(UiText::Combo)),
                    app.theme.label,
                ),
                Span::styled(format!("{:04}", score.combo), app.theme.selection),
            ]),
            render_gauge_bar_line(
                app,
                score.gauge,
                score.pass_threshold,
                player_rows[0].width.saturating_sub(2),
                false,
            ),
            Line::from(Span::styled(feedback, feedback_style)),
        ])
        .block(themed_block(app, &player_title));
        frame.render_widget(hud, player_rows[0]);

        render_lane_view(
            &app.theme,
            frame,
            player_rows[1],
            &player.last_output.frame_view,
            LaneRenderOptions {
                scroll_speed: app.effective_scroll_speed(),
                paused: game.paused,
                paused_label: app.text(UiText::PauseLane),
                gogo_label: app.text(UiText::GoGoLane),
                judge_flash: player.judge_flash.map(|(judge, _)| judge),
                input_flash: player.input_flash.map(|(action, _)| action),
            },
        );
    }

    let help = Paragraph::new(vec![
        Line::from(vec![
            Span::styled("P1  ", app.theme.title),
            Span::styled(
                binding_summary(app, app.preferences.player_one),
                app.theme.metadata,
            ),
        ]),
        Line::from(vec![
            Span::styled("P2  ", app.theme.title),
            Span::styled(
                binding_summary(app, app.preferences.player_two),
                app.theme.metadata,
            ),
            Span::styled(
                format!("    {}", app.text(UiText::GameControlsHelp)),
                app.theme.text_secondary,
            ),
        ]),
    ]);
    frame.render_widget(help, rows[1]);
}

fn binding_summary(app: &App, bindings: crate::preferences::DrumBindings) -> String {
    format!(
        "{}={}  {}={}  {}={}  {}={}",
        bindings.left_kat.to_ascii_uppercase(),
        app.text(UiText::BindingLeftKat).to_uppercase(),
        bindings.left_don.to_ascii_uppercase(),
        app.text(UiText::BindingLeftDon).to_uppercase(),
        bindings.right_don.to_ascii_uppercase(),
        app.text(UiText::BindingRightDon).to_uppercase(),
        bindings.right_kat.to_ascii_uppercase(),
        app.text(UiText::BindingRightKat).to_uppercase(),
    )
}

fn split_player_areas(area: Rect) -> [Rect; 2] {
    let areas = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(area);
    [areas[0], areas[1]]
}

fn themed_block<'a>(app: &App, title: &'a str) -> Block<'a> {
    Block::default()
        .borders(Borders::ALL)
        .border_style(app.theme.border)
        .title(title)
        .title_style(app.theme.title)
}

#[cfg(test)]
mod tests {
    use ratatui::layout::{Constraint, Direction, Layout, Rect};

    use super::split_player_areas;

    #[test]
    fn local_players_stack_vertically_and_keep_the_full_highway_width() {
        let area = Rect::new(4, 3, 180, 40);
        let [p1, p2] = split_player_areas(area);

        assert_eq!(p1.x, area.x);
        assert_eq!(p2.x, area.x);
        assert_eq!(p1.width, area.width);
        assert_eq!(p2.width, area.width);
        assert_eq!(p1.y, area.y);
        assert_eq!(p2.y, p1.y + p1.height);
        assert_eq!(p1.height + p2.height, area.height);
    }

    #[test]
    fn minimum_local_game_height_preserves_both_complete_five_row_lanes() {
        // A 27-row terminal leaves 26 rows after the global top bar.
        let content = Rect::new(0, 1, 80, 26);
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(11), Constraint::Length(2)])
            .split(content);
        let players = split_player_areas(rows[0]);

        for player in players {
            let player_rows = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Length(5), Constraint::Min(6)])
                .split(player);
            assert_eq!(player_rows[0].height, 5);
            assert!(
                player_rows[1].height >= 7,
                "lane requires 2 border rows plus all 5 rendered highway rows"
            );
        }
    }
}
