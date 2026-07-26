use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

use crate::app::App;
use crate::localization::{Localizer, UiText};
use crate::tui::Frame;

pub fn render(app: &App, frame: &mut Frame<'_>, area: Rect) {
    let Some(online) = &app.online else {
        return;
    };

    let mut lines: Vec<Line<'_>> = vec![
        Line::from(vec![
            Span::styled(format!("{}: ", app.text(UiText::Phase)), app.theme.metadata),
            Span::styled(
                app.localizer().online_phase(online.phase()),
                app.theme.text_primary,
            ),
        ]),
        Line::from(""),
        Line::from(Span::styled(
            app.text(UiText::MatchFinished),
            app.theme.text_primary,
        )),
        Line::from(""),
    ];

    if let Some(snapshot) = &online.snapshot {
        for player in &snapshot.players {
            if let Some(result) = online.final_results.get(&player.player_id) {
                let outcome = result_outcome(app.localizer(), result.dnf, result.passed);
                lines.push(Line::from(vec![
                    Span::styled(format!("{}: ", player.name), app.theme.metadata),
                    Span::styled(format!("{}=", app.text(UiText::Score)), app.theme.label),
                    Span::styled(result.score.score.to_string(), app.theme.value),
                    Span::styled("  ", app.theme.text_primary),
                    Span::styled(format!("{}=", app.text(UiText::MaxCombo)), app.theme.label),
                    Span::styled(result.score.max_combo.to_string(), app.theme.value),
                    Span::styled("  ", app.theme.text_primary),
                    Span::styled(
                        outcome,
                        if result.dnf {
                            app.theme.error
                        } else {
                            app.theme.text_primary
                        },
                    ),
                ]));
            }
        }
    }

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        controls_hint(
            app.localizer(),
            online.is_local_leader(),
            online.room_controls_enabled(),
        ),
        app.theme.text_secondary,
    )));

    let widget = Paragraph::new(lines)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(app.theme.border)
                .title(app.text(UiText::OnlineResultTitle))
                .title_style(app.theme.title),
        )
        .wrap(Wrap { trim: true });
    frame.render_widget(widget, area);
}

fn result_outcome(localizer: Localizer, dnf: bool, passed: bool) -> &'static str {
    if dnf {
        localizer.text(UiText::Dnf)
    } else if passed {
        localizer.text(UiText::Pass)
    } else {
        localizer.text(UiText::Failed)
    }
}

fn controls_hint(localizer: Localizer, is_leader: bool, controls_enabled: bool) -> &'static str {
    if !controls_enabled {
        localizer.text(UiText::ResultControlsPaused)
    } else if is_leader {
        localizer.text(UiText::LeaderResultControls)
    } else {
        localizer.text(UiText::WaitingLeaderResultControls)
    }
}

#[cfg(test)]
mod tests {
    use super::{controls_hint, result_outcome};
    use crate::localization::Localizer;
    use crate::preferences::UiLanguage;

    #[test]
    fn only_leader_hint_advertises_match_controls() {
        let localizer = Localizer::new(UiLanguage::English);
        assert!(controls_hint(localizer, true, true).contains("rematch"));
        assert!(controls_hint(localizer, true, true).contains("return to lobby"));
        assert!(!controls_hint(localizer, false, true).contains("rematch"));
        assert!(!controls_hint(localizer, false, true).contains("return to lobby"));
        assert!(controls_hint(localizer, false, true).contains("Waiting for the leader"));
        assert!(controls_hint(localizer, true, true).contains("Ctrl+C"));
        assert!(controls_hint(localizer, false, true).contains("Ctrl+C"));
    }

    #[test]
    fn reconnecting_hint_never_advertises_result_commands() {
        let localizer = Localizer::new(UiLanguage::English);
        assert!(controls_hint(localizer, true, false).contains("Reconnecting"));
        assert!(!controls_hint(localizer, true, false).contains("rematch"));
        assert!(!controls_hint(localizer, true, false).contains("return to lobby"));
    }

    #[test]
    fn dnf_takes_precedence_over_failed_pass_state() {
        let localizer = Localizer::new(UiLanguage::English);
        assert_eq!(result_outcome(localizer, true, false), "DNF");
        assert_eq!(result_outcome(localizer, false, false), "FAILED");
        assert_eq!(result_outcome(localizer, false, true), "PASS");
    }
}
