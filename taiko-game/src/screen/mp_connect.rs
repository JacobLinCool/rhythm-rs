use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

use crate::app::{App, ConnectField, ConnectMode};
use crate::localization::{pad_or_truncate_to_width, truncate_tail_to_width, UiText};
use crate::tui::Frame;

pub fn render(app: &App, frame: &mut Frame<'_>, area: Rect) {
    let mp = &app.mp_connect;
    let theme = &app.theme;

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(0),
            Constraint::Length(17),
            Constraint::Min(0),
        ])
        .split(area);

    let center = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Min(0),
            Constraint::Length(76),
            Constraint::Min(0),
        ])
        .split(chunks[1])[1];

    let focus = mp.focus;

    let mode_label = match mp.mode {
        ConnectMode::Host => app.text(UiText::ConnectModesHost),
        ConnectMode::Create => app.text(UiText::ConnectModesCreate),
        ConnectMode::Join => app.text(UiText::ConnectModesJoin),
        ConnectMode::Spectate => app.text(UiText::ConnectModesSpectate),
    };

    let server_display = format_editable(&mp.server, focus == ConnectField::Server);
    let invite_display = if mp.invite_revealed {
        format_editable(&mp.invite, focus == ConnectField::Invite)
    } else {
        format_secret(&mp.invite, focus == ConnectField::Invite)
    };
    let name_display = format_editable(&mp.name, focus == ConnectField::Name);

    let mut lines: Vec<Line<'_>> = Vec::new();
    lines.push(Line::from(""));
    lines.push(field_line(
        &field_label(app, UiText::Mode),
        mode_label,
        focus == ConnectField::Mode,
        theme,
    ));
    if mp.mode == ConnectMode::Host {
        lines.push(Line::from(Span::styled(
            format!("  {}", app.text(UiText::HostDescription)),
            theme.metadata,
        )));
    } else if mp.mode == ConnectMode::Create {
        lines.push(field_line(
            &field_label(app, UiText::Server),
            &server_display,
            focus == ConnectField::Server,
            theme,
        ));
    } else {
        lines.push(field_line(
            &field_label(app, UiText::Invite),
            &invite_display,
            focus == ConnectField::Invite,
            theme,
        ));
        lines.push(Line::from(Span::styled(
            if mp.invite_revealed {
                format!("  {}", app.text(UiText::HideInviteSecret))
            } else {
                format!("  {}", app.text(UiText::RevealInviteSecret))
            },
            theme.metadata,
        )));
    }

    lines.push(field_line(
        &field_label(app, UiText::Name),
        &name_display,
        focus == ConnectField::Name,
        theme,
    ));
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        format!("  {}", app.text(UiText::ConnectControls)),
        theme.metadata,
    )));
    lines.push(Line::from(""));

    let connect_style = if focus == ConnectField::Confirm && mp.status.is_none() {
        theme.selection
    } else {
        theme.text_secondary
    };
    let connect_label = if mp.status.is_some() {
        app.text(UiText::ConnectingButton)
    } else if mp.mode == ConnectMode::Host {
        app.text(UiText::HostButton)
    } else {
        app.text(UiText::ConnectButton)
    };
    lines.push(Line::from(Span::styled(
        format!("  {connect_label}"),
        connect_style,
    )));

    if let Some(status) = app.multiplayer_connect_status_text() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            format!("  {status}"),
            theme.warning,
        )));
    }

    if let Some(error) = app.multiplayer_connect_error_text() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(format!("  {error}"), theme.error)));
    }

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme.border)
        .title(app.text(UiText::OnlineMultiplayerTitle))
        .title_style(theme.title);

    let widget = Paragraph::new(lines)
        .block(block)
        .wrap(ratatui::widgets::Wrap { trim: false });
    frame.render_widget(widget, center);
}

fn field_line(
    label: &str,
    value: &str,
    focused: bool,
    theme: &crate::theme::Theme,
) -> Line<'static> {
    let style = if focused {
        theme.selection
    } else {
        theme.text_primary
    };
    Line::from(vec![
        Span::styled(label.to_owned(), theme.metadata),
        Span::styled(value.to_owned(), style),
    ])
}

fn field_label(app: &App, key: UiText) -> String {
    let label = format!("{}:", app.text(key));
    format!("  {}", pad_or_truncate_to_width(&label, 11))
}

fn format_editable(text: &str, focused: bool) -> String {
    let width = if focused { 55 } else { 56 };
    let mut visible = truncate_tail_to_width(text, width);
    if focused {
        visible.push('_');
    }
    visible
}

fn format_secret(text: &str, focused: bool) -> String {
    let visible_length = text.chars().count().min(48);
    let mut masked = "•".repeat(visible_length);
    if text.chars().count() > visible_length {
        masked.push('…');
    }
    if focused {
        masked.push('_');
    }
    masked
}

#[cfg(test)]
mod tests {
    use super::format_secret;
    use crate::localization::{display_width, truncate_tail_to_width};

    #[test]
    fn hidden_invite_never_contains_secret_text() {
        let secret = "taiko://join?token=super-secret";
        let hidden = format_secret(secret, true);
        assert!(!hidden.contains("taiko"));
        assert!(!hidden.contains("super-secret"));
        assert!(hidden.ends_with('_'));
    }

    #[test]
    fn long_editable_values_keep_the_cursor_end_visible() {
        let text = "https://example.com/".to_owned() + &"path/".repeat(30);
        let clipped = truncate_tail_to_width(&text, 20);
        assert_eq!(display_width(&clipped), 20);
        assert!(clipped.starts_with('…'));
        assert!(clipped.ends_with("path/"));
    }
}
