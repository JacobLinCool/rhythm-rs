use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use rhythm_mode_taiko::TaikoJudge;

use super::render_gauge_bar_line;
use crate::app::App;
use crate::local_multiplayer::LocalPlayerId;
use crate::localization::{LocalOutcome, UiMessage, UiText};
use crate::tui::Frame;

pub fn render(app: &App, frame: &mut Frame<'_>, area: Rect) {
    let Some(result) = app.local_result.as_ref() else {
        frame.render_widget(
            Paragraph::new(Span::styled(
                app.text(UiText::NoLocalResult),
                app.theme.error,
            ))
            .block(themed_block(app, app.text(UiText::LocalResultTitle))),
            area,
        );
        return;
    };

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(10),
            Constraint::Length(3),
        ])
        .split(area);
    let score_one = result.players[0].final_result.score;
    let score_two = result.players[1].final_result.score;
    let same_course = result.players[0].course_name == result.players[1].course_name;
    let outcome = match (same_course, score_one.cmp(&score_two)) {
        (true, std::cmp::Ordering::Greater) => LocalOutcome::WinnerP1,
        (true, std::cmp::Ordering::Less) => LocalOutcome::WinnerP2,
        (true, std::cmp::Ordering::Equal) => LocalOutcome::Tie,
        (false, std::cmp::Ordering::Greater) => LocalOutcome::HigherP1DifferentCourses,
        (false, std::cmp::Ordering::Less) => LocalOutcome::HigherP2DifferentCourses,
        (false, std::cmp::Ordering::Equal) => LocalOutcome::EqualDifferentCourses,
    };
    let outcome = app.localizer().message(UiMessage::LocalOutcome { outcome });
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(result.title.clone(), app.theme.title),
            Span::styled("  ", app.theme.text_primary),
            Span::styled(result.subtitle.clone(), app.theme.text_secondary),
            Span::styled("  •  ", app.theme.text_primary),
            Span::styled(outcome, app.theme.success),
        ]))
        .block(themed_block(app, app.text(UiText::LocalTwoPlayerResult))),
        rows[0],
    );

    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(rows[1]);
    for ((player_id, player), player_area) in LocalPlayerId::ALL
        .into_iter()
        .zip(result.players.iter())
        .zip(columns.iter().copied())
    {
        let score = &player.final_result;
        let pass_style = if score.passed {
            app.theme.success
        } else {
            app.theme.error
        };
        let mut lines = vec![
            Line::from(vec![
                Span::styled(
                    format!("{} ", app.text(UiText::SelectedCourse)),
                    app.theme.label,
                ),
                Span::styled(player.course_name.clone(), app.theme.value),
            ]),
            Line::from(vec![
                Span::styled(format!("{} ", app.text(UiText::Score)), app.theme.label),
                Span::styled(score.score.to_string(), app.theme.value),
                Span::styled(
                    format!("  {} ", app.text(UiText::MaxCombo)),
                    app.theme.label,
                ),
                Span::styled(score.max_combo.to_string(), app.theme.value),
            ]),
            render_gauge_bar_line(
                app,
                score.gauge,
                score.pass_threshold,
                player_area.width.saturating_sub(2),
                true,
            ),
            Line::from(vec![
                Span::styled(format!("{} ", app.text(UiText::Great)), app.theme.label),
                Span::styled(
                    score.great.to_string(),
                    app.theme.judge_style(TaikoJudge::Great { delta_tick: 0 }),
                ),
                Span::styled(format!("  {} ", app.text(UiText::Ok)), app.theme.label),
                Span::styled(
                    score.ok.to_string(),
                    app.theme.judge_style(TaikoJudge::Ok { delta_tick: 0 }),
                ),
                Span::styled(format!("  {} ", app.text(UiText::Miss)), app.theme.label),
                Span::styled(
                    score.miss.to_string(),
                    app.theme.judge_style(TaikoJudge::Miss { delta_tick: 0 }),
                ),
            ]),
            Line::from(vec![
                Span::styled(format!("{} ", app.text(UiText::RollHits)), app.theme.label),
                Span::styled(score.roll_hits.to_string(), app.theme.value),
            ]),
            Line::from(vec![
                Span::styled(format!("{} ", app.text(UiText::Result)), app.theme.label),
                Span::styled(
                    app.text(if score.passed {
                        UiText::Pass
                    } else {
                        UiText::Fail
                    }),
                    pass_style,
                ),
            ]),
        ];
        if app.result_details_visible {
            lines.push(Line::from(vec![
                Span::styled(format!("{} ", app.text(UiText::Replay)), app.theme.label),
                Span::styled(format!("{:016x}", player.replay_hash), app.theme.metadata),
                Span::styled(
                    format!("  {} ", app.text(UiText::BranchControls)),
                    app.theme.label,
                ),
                Span::styled(player.branch_controls.to_string(), app.theme.metadata),
            ]));
        }
        frame.render_widget(
            Paragraph::new(lines)
                .block(themed_block(app, player_id.label()))
                .wrap(Wrap { trim: true }),
            player_area,
        );
    }

    frame.render_widget(
        Paragraph::new(Span::styled(
            if app.result_details_visible {
                app.text(UiText::LocalResultHideDetails)
            } else {
                app.text(UiText::LocalResultShowDetails)
            },
            app.theme.metadata,
        ))
        .block(themed_block(app, app.text(UiText::Next))),
        rows[2],
    );
}

fn themed_block<'a>(app: &App, title: &'a str) -> Block<'a> {
    Block::default()
        .borders(Borders::ALL)
        .border_style(app.theme.border)
        .title(title)
        .title_style(app.theme.title)
}
