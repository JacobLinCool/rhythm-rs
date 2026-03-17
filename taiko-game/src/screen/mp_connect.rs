use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

use crate::app::{App, ConnectField, ConnectMode};
use crate::tui::Frame;

pub fn render(app: &App, frame: &mut Frame<'_>, area: Rect) {
    let mp = &app.mp_connect;
    let theme = &app.theme;

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(0),
            Constraint::Length(12),
            Constraint::Min(0),
        ])
        .split(area);

    let center = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Min(0),
            Constraint::Length(56),
            Constraint::Min(0),
        ])
        .split(chunks[1])[1];

    let focus = mp.focus;

    let mode_label = match mp.mode {
        ConnectMode::Create => "[ Create ]   Join  ",
        ConnectMode::Join => "  Create   [ Join ]",
    };

    let server_display = format_editable(&mp.server, focus == ConnectField::Server);
    let room_display = format_editable(&mp.room_code, focus == ConnectField::RoomCode);
    let name_display = format_editable(&mp.name, focus == ConnectField::Name);

    let mut lines: Vec<Line<'_>> = Vec::new();
    lines.push(Line::from(""));
    lines.push(field_line(
        "  Mode:      ",
        mode_label,
        focus == ConnectField::Mode,
        theme,
    ));
    lines.push(field_line(
        "  Server:    ",
        &server_display,
        focus == ConnectField::Server,
        theme,
    ));

    if mp.mode == ConnectMode::Join {
        lines.push(field_line(
            "  Room Code: ",
            &room_display,
            focus == ConnectField::RoomCode,
            theme,
        ));
    }

    lines.push(field_line(
        "  Name:      ",
        &name_display,
        focus == ConnectField::Name,
        theme,
    ));
    lines.push(Line::from(""));

    let connect_style = if focus == ConnectField::Confirm {
        theme.selection
    } else {
        theme.text_secondary
    };
    lines.push(Line::from(Span::styled("  [ Connect ]", connect_style)));

    if let Some(error) = &mp.error {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(format!("  {error}"), theme.error)));
    }

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme.border)
        .title(" Multiplayer ")
        .title_style(theme.title);

    let widget = Paragraph::new(lines).block(block);
    frame.render_widget(widget, center);
}

fn field_line<'a>(
    label: &'a str,
    value: &'a str,
    focused: bool,
    theme: &crate::theme::Theme,
) -> Line<'a> {
    let style = if focused {
        theme.selection
    } else {
        theme.text_primary
    };
    Line::from(vec![
        Span::styled(label, theme.metadata),
        Span::styled(value.to_owned(), style),
    ])
}

fn format_editable(text: &str, focused: bool) -> String {
    if focused {
        format!("{text}_")
    } else {
        text.to_owned()
    }
}
