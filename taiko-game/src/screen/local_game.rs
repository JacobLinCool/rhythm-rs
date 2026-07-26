use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

use super::game_screen::{judge_feedback, render_lane_view, LaneRenderOptions, LANE_BLOCK_HEIGHT};
use super::{render_controller_drum_surface, render_gauge_bar_line};
use crate::app::App;
use crate::controller::ControllerSlot;
use crate::drum_surface::DrumSurfaceLayout;
use crate::local_multiplayer::LocalPlayerId;
use crate::localization::UiText;
use crate::tui::Frame;

const PLAYER_HUD_HEIGHT: u16 = 5;
const LOCAL_PLAY_AREA_MIN_HEIGHT: u16 = 2 * (PLAYER_HUD_HEIGHT + LANE_BLOCK_HEIGHT);

pub fn render(app: &App, frame: &mut Frame<'_>, area: Rect) -> Option<DrumSurfaceLayout> {
    let Some(game) = app.local_game.as_ref() else {
        frame.render_widget(
            Paragraph::new(Span::styled(
                app.text(UiText::LocalSessionMissing),
                app.theme.error,
            ))
            .block(themed_block(app, app.text(UiText::LocalTwoPlayer))),
            area,
        );
        return None;
    };

    let pointer_slot = app.terminal_pointer_slot();
    let controller_surface_slots = if !game.paused && app.leave_confirmation.is_none() {
        distinct_controller_slots(pointer_slot, app.mac_trackpad_slot())
    } else {
        [None, None]
    };
    let controller_surface_count = controller_surface_slots.iter().flatten().count();
    let controls_height = 2 + u16::from(!app.keyboard_repeat_is_distinguishable);
    let footer_height = if controller_surface_count > 0 {
        3 + controls_height
    } else {
        controls_height
    };
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(LOCAL_PLAY_AREA_MIN_HEIGHT),
            Constraint::Length(footer_height),
        ])
        .split(area);
    let player_areas = split_player_areas(rows[0]);

    for (player_id, player_area) in LocalPlayerId::ALL.into_iter().zip(player_areas) {
        let player_rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(PLAYER_HUD_HEIGHT),
                Constraint::Min(LANE_BLOCK_HEIGHT),
            ])
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
                judge_flash: player.judge_flash.map(|(judge, _)| judge),
                input_flash: player.input_flash.map(|(action, _)| action),
            },
        );
    }

    if controller_surface_count > 0 {
        let footer = Layout::vertical([Constraint::Length(3), Constraint::Length(controls_height)])
            .split(rows[1]);
        let mut help_lines = vec![
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
        ];
        append_keyboard_warning(app, &mut help_lines);
        frame.render_widget(Paragraph::new(help_lines), footer[1]);
        let surface_areas = if controller_surface_count == 2 {
            let areas =
                Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)])
                    .split(footer[0]);
            [areas[0], areas[1]]
        } else {
            [footer[0], Rect::default()]
        };
        let mut pointer_surface = None;
        for (slot, surface_area) in controller_surface_slots
            .into_iter()
            .flatten()
            .zip(surface_areas)
        {
            let active_action = game.players[slot.index()]
                .input_flash
                .map(|(action, _)| action);
            let surface =
                render_controller_drum_surface(app, frame, surface_area, slot, active_action);
            if pointer_slot == Some(slot) {
                pointer_surface = surface;
            }
        }
        return pointer_surface;
    }

    let mut help_lines = vec![
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
    ];
    append_keyboard_warning(app, &mut help_lines);
    frame.render_widget(Paragraph::new(help_lines), rows[1]);
    None
}

fn append_keyboard_warning(app: &App, lines: &mut Vec<Line<'static>>) {
    if !app.keyboard_repeat_is_distinguishable {
        lines.push(Line::from(Span::styled(
            app.text(UiText::ControllerKeyboardRepeatGameplay),
            app.theme.warning,
        )));
    }
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

fn distinct_controller_slots(
    first: Option<ControllerSlot>,
    second: Option<ControllerSlot>,
) -> [Option<ControllerSlot>; 2] {
    [ControllerSlot::One, ControllerSlot::Two]
        .map(|slot| (first == Some(slot) || second == Some(slot)).then_some(slot))
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

    use super::{
        distinct_controller_slots, split_player_areas, LANE_BLOCK_HEIGHT,
        LOCAL_PLAY_AREA_MIN_HEIGHT, PLAYER_HUD_HEIGHT,
    };
    use crate::controller::ControllerSlot;

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
    fn controller_surfaces_deduplicate_and_stay_in_player_order() {
        assert_eq!(distinct_controller_slots(None, None), [None, None]);
        assert_eq!(
            distinct_controller_slots(Some(ControllerSlot::Two), None),
            [None, Some(ControllerSlot::Two)]
        );
        assert_eq!(
            distinct_controller_slots(Some(ControllerSlot::Two), Some(ControllerSlot::Two)),
            [None, Some(ControllerSlot::Two)]
        );
        assert_eq!(
            distinct_controller_slots(Some(ControllerSlot::Two), Some(ControllerSlot::One)),
            [Some(ControllerSlot::One), Some(ControllerSlot::Two)]
        );
    }

    #[test]
    fn minimum_local_game_height_preserves_both_complete_seven_row_lane_canvases() {
        // A 35-row terminal leaves 34 rows after the global top bar. The
        // worst-case footer contains both a controller surface and the
        // keyboard-repeat warning.
        let content = Rect::new(0, 1, 80, 34);
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(LOCAL_PLAY_AREA_MIN_HEIGHT),
                Constraint::Length(6),
            ])
            .split(content);
        let players = split_player_areas(rows[0]);

        for player in players {
            let player_rows = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(PLAYER_HUD_HEIGHT),
                    Constraint::Min(LANE_BLOCK_HEIGHT),
                ])
                .split(player);
            assert_eq!(player_rows[0].height, PLAYER_HUD_HEIGHT);
            assert_eq!(player_rows[1].height, LANE_BLOCK_HEIGHT);
        }
    }
}
