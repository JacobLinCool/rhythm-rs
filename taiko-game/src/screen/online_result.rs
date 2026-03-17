use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

use crate::app::App;
use crate::tui::Frame;

pub fn render(app: &App, frame: &mut Frame<'_>, area: Rect) {
    let Some(online) = &app.online else {
        return;
    };

    let mut lines: Vec<Line<'_>> = Vec::new();
    lines.push(Line::from(Span::styled(
        "Match Finished!",
        app.theme.text_primary,
    )));
    lines.push(Line::from(""));

    if let Some(snapshot) = &online.snapshot {
        for player in &snapshot.players {
            if let Some(result) = online.final_results.get(&player.player_id) {
                lines.push(Line::from(vec![
                    Span::styled(format!("{}: ", player.name), app.theme.metadata),
                    Span::styled(
                        format!(
                            "score={} combo={}",
                            result.result.score, result.result.max_combo
                        ),
                        app.theme.text_primary,
                    ),
                ]));
            } else if player.dnf {
                lines.push(Line::from(vec![
                    Span::styled(format!("{}: ", player.name), app.theme.metadata),
                    Span::styled("DNF", app.theme.error),
                ]));
            }
        }
    }

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "Press any key to return to lobby",
        app.theme.text_secondary,
    )));

    let widget = Paragraph::new(lines)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(app.theme.border)
                .title("Online Result")
                .title_style(app.theme.title),
        )
        .wrap(Wrap { trim: true });
    frame.render_widget(widget, area);
}
