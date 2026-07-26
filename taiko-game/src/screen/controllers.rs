use std::net::IpAddr;
use std::time::Duration;

use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap};

use crate::app::{App, ControllerSetupItem};
use crate::controller::ControllerSlot;
use crate::controller_qr::encode_controller_url;
use crate::lan_controller::ControllerSlotStatus;
use crate::localization::{pad_or_truncate_to_width, UiText};
use crate::tui::Frame;

use super::render_terminal_drum_surface;

const TEST_FLASH_DURATION: Duration = Duration::from_millis(120);

pub fn render(app: &mut App, frame: &mut Frame<'_>, area: Rect) {
    if let Some(slot) = selected_controller_slot(app.controller_setup.selected_item()) {
        if app.controller_setup.invite_revealed[slot.index()] {
            if let Some(invite) = app.controller_pairing_invite(slot) {
                render_pairing_qr(app, frame, area, slot, invite.expose());
                return;
            }
        }
    }

    let panes = if area.width < 110 {
        Layout::vertical([Constraint::Length(8), Constraint::Min(0)]).split(area)
    } else {
        Layout::horizontal([Constraint::Percentage(44), Constraint::Percentage(56)]).split(area)
    };
    let statuses = app.controller_slot_statuses();
    let items = ControllerSetupItem::ALL
        .into_iter()
        .map(|item| ListItem::new(item_line(app, item, statuses)))
        .collect::<Vec<_>>();
    let mut state = ListState::default();
    state.select(Some(app.controller_setup.selected));
    frame.render_stateful_widget(
        List::new(items)
            .highlight_symbol(">> ")
            .highlight_style(app.theme.selection)
            .block(themed_block(app, app.text(UiText::ControllerSetup))),
        panes[0],
        &mut state,
    );

    let pointer_slot = app.controller_setup.pointer_slot;
    let right_sections = if pointer_slot.is_some() && panes[1].height >= 9 {
        Layout::vertical([Constraint::Min(0), Constraint::Length(5)]).split(panes[1])
    } else {
        Layout::vertical([Constraint::Min(0), Constraint::Length(0)]).split(panes[1])
    };
    let detail_area = right_sections[0];
    let mut detail = vec![
        Line::from(Span::styled(
            app.text(UiText::ControllerSetupDescription),
            app.theme.text_primary,
        )),
        Line::from(""),
    ];
    if let Some((message, is_error)) = &app.controller_setup.notice {
        detail.push(Line::from(Span::styled(
            message,
            if *is_error {
                app.theme.error
            } else {
                app.theme.success
            },
        )));
        detail.push(Line::from(""));
    }
    if let Some(slot) = selected_controller_slot(app.controller_setup.selected_item()) {
        append_slot_details(app, &mut detail, slot, statuses[slot.index()]);
        detail.push(Line::from(""));
    }
    if let Some(endpoint) = app.controller_endpoint() {
        detail.push(Line::from(vec![
            Span::styled(
                format!("{}: ", app.text(UiText::ControllerEndpoint)),
                app.theme.label,
            ),
            Span::styled(endpoint, app.theme.value),
        ]));
        detail.push(Line::from(""));
    }
    if app
        .controller_setup
        .bind_ip
        .parse::<IpAddr>()
        .is_ok_and(|address| address.is_loopback())
    {
        detail.push(Line::from(Span::styled(
            app.text(UiText::ControllerLoopbackOnly),
            app.theme.warning,
        )));
        detail.push(Line::from(""));
    }
    if !app.keyboard_repeat_is_distinguishable {
        detail.push(Line::from(Span::styled(
            app.text(UiText::ControllerKeyboardRepeatLimited),
            app.theme.warning,
        )));
        detail.push(Line::from(""));
    }

    detail.extend([
        Line::from(Span::styled(
            app.text(UiText::TrustedLanOnly),
            app.theme.warning,
        )),
        Line::from(Span::styled(
            app.text(UiText::TrustedLanWarning),
            app.theme.metadata,
        )),
        Line::from(""),
        Line::from(Span::styled(
            app.text(UiText::ControllerNavigationHelp),
            app.theme.text_primary,
        )),
        Line::from(Span::styled(
            app.text(UiText::ControllerPairingHelp),
            app.theme.text_primary,
        )),
        Line::from(Span::styled(
            app.text(UiText::ControllerTrackpadHelp),
            app.theme.metadata,
        )),
        Line::from(Span::styled(
            app.text(UiText::ControllerPhoneHelp),
            app.theme.metadata,
        )),
        Line::from(Span::styled(
            app.text(UiText::ControllerTestHelp),
            app.theme.metadata,
        )),
    ]);

    frame.render_widget(
        Paragraph::new(detail)
            .wrap(Wrap { trim: true })
            .block(themed_block(app, app.text(UiText::HowItWorks))),
        detail_area,
    );

    if let Some(slot) = pointer_slot {
        let active_action = app.controller_setup.last_test_action[slot.index()]
            .filter(|(_, observed_at)| observed_at.elapsed() <= TEST_FLASH_DURATION)
            .map(|(action, _)| action);
        if let Some(surface) =
            render_terminal_drum_surface(app, frame, right_sections[1], slot, active_action)
        {
            app.set_pointer_surface(surface);
        }
    }
}

fn render_pairing_qr(
    app: &App,
    frame: &mut Frame<'_>,
    area: Rect,
    slot: ControllerSlot,
    url: &str,
) {
    let player = match slot {
        ControllerSlot::One => "P1",
        ControllerSlot::Two => "P2",
    };
    let qr = match encode_controller_url(url) {
        Ok(qr) => qr,
        Err(_) => {
            frame.render_widget(
                Paragraph::new(app.text(UiText::ControllerPairingQrFailed))
                    .alignment(Alignment::Center)
                    .block(themed_block(app, app.text(UiText::ControllerPairingQr))),
                area,
            );
            return;
        }
    };
    let qr_width = u16::try_from(qr.width).unwrap_or(u16::MAX);
    let qr_height = u16::try_from(qr.height).unwrap_or(u16::MAX);
    let required_width = qr_width.saturating_add(2);
    let required_height = qr_height.saturating_add(3);
    if area.width < required_width || area.height < required_height {
        frame.render_widget(
            Paragraph::new(format!(
                "{}  {}×{}",
                app.text(UiText::ControllerPairingQrNeedsSpace),
                required_width,
                required_height
            ))
            .alignment(Alignment::Center)
            .wrap(Wrap { trim: true })
            .block(themed_block(app, app.text(UiText::ControllerPairingQr))),
            area,
        );
        return;
    }

    let modal = centered_rect(required_width, required_height, area);
    let rows =
        Layout::vertical([Constraint::Length(qr_height + 2), Constraint::Length(1)]).split(modal);
    frame.render_widget(
        Paragraph::new(qr.lines)
            .style(Style::default().fg(Color::Black).bg(Color::White))
            .block(
                themed_block(app, app.text(UiText::ControllerPairingQr)).title(format!(
                    " {player} • {} ",
                    app.text(UiText::ControllerPairingQr)
                )),
            ),
        rows[0],
    );
    frame.render_widget(
        Paragraph::new(app.text(UiText::ControllerPairingQrHelp)).alignment(Alignment::Center),
        rows[1],
    );
}

fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    Rect::new(
        area.x + area.width.saturating_sub(width) / 2,
        area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    )
}

fn item_line(
    app: &App,
    item: ControllerSetupItem,
    statuses: [ControllerSlotStatus; 2],
) -> Line<'static> {
    let (label, value, value_style) = match item {
        ControllerSetupItem::BindAddress => (
            app.text(UiText::ControllerBindAddress),
            format!("{}_", app.controller_setup.bind_ip),
            app.theme.value,
        ),
        ControllerSetupItem::LanServer => (
            app.text(UiText::ControllerLanServer),
            if app.controller_server_running() {
                app.text(UiText::ControllerRunning)
            } else {
                app.text(UiText::ControllerStopped)
            }
            .to_owned(),
            if app.controller_server_running() {
                app.theme.success
            } else {
                app.theme.metadata
            },
        ),
        ControllerSetupItem::TerminalPointer => (
            app.text(UiText::ControllerTerminalPointer),
            match app.controller_setup.pointer_slot {
                Some(ControllerSlot::One) => app.text(UiText::ControllerPointerPlayerOne),
                Some(ControllerSlot::Two) => app.text(UiText::ControllerPointerPlayerTwo),
                None => app.text(UiText::ControllerPointerOff),
            }
            .to_owned(),
            if app.controller_setup.pointer_slot.is_some() {
                app.theme.success
            } else {
                app.theme.metadata
            },
        ),
        ControllerSetupItem::PlayerOne => controller_row(
            app,
            UiText::ControllerPlayerOne,
            statuses[ControllerSlot::One.index()],
        ),
        ControllerSetupItem::PlayerTwo => controller_row(
            app,
            UiText::ControllerPlayerTwo,
            statuses[ControllerSlot::Two.index()],
        ),
        ControllerSetupItem::Back => (
            app.text(UiText::ControllerBack),
            app.text(UiText::Enter).to_owned(),
            app.theme.value,
        ),
    };
    Line::from(vec![
        Span::styled(pad_or_truncate_to_width(label, 27), app.theme.label),
        Span::styled(value, value_style),
    ])
}

fn controller_row(
    app: &App,
    label: UiText,
    status: ControllerSlotStatus,
) -> (&'static str, String, ratatui::style::Style) {
    if status.connected {
        (
            app.text(label),
            app.text(UiText::ControllerConnected).to_owned(),
            app.theme.success,
        )
    } else if status.paired {
        (
            app.text(label),
            app.text(UiText::ControllerPaired).to_owned(),
            app.theme.warning,
        )
    } else if app.controller_server_running() {
        (
            app.text(label),
            app.text(UiText::ControllerWaitingPair).to_owned(),
            app.theme.value,
        )
    } else {
        (
            app.text(label),
            app.text(UiText::ControllerStopped).to_owned(),
            app.theme.metadata,
        )
    }
}

fn append_slot_details<'a>(
    app: &'a App,
    detail: &mut Vec<Line<'a>>,
    slot: ControllerSlot,
    status: ControllerSlotStatus,
) {
    let player = match slot {
        ControllerSlot::One => "P1",
        ControllerSlot::Two => "P2",
    };
    detail.push(Line::from(Span::styled(player, app.theme.title)));
    detail.push(Line::from(vec![
        Span::styled(
            format!("{}: ", app.text(UiText::ControllerHits)),
            app.theme.label,
        ),
        Span::styled(
            format!("{} / {}", status.accepted_hits, status.rejected_hits),
            app.theme.value,
        ),
    ]));
    detail.push(Line::from(Span::styled(
        format!(
            "{}  •  {}",
            if status.paired {
                app.text(UiText::ControllerPaired)
            } else {
                app.text(UiText::ControllerWaitingPair)
            },
            if status.connected {
                app.text(UiText::ControllerConnected)
            } else {
                app.text(UiText::ControllerDisconnected)
            }
        ),
        if status.connected {
            app.theme.success
        } else {
            app.theme.metadata
        },
    )));
    let invite = app.controller_pairing_invite(slot);
    detail.push(Line::from(vec![
        Span::styled(
            format!("{}: ", app.text(UiText::ControllerPairingLink)),
            app.theme.label,
        ),
        match invite {
            Some(_) => Span::styled(app.text(UiText::ControllerLinkMasked), app.theme.metadata),
            None if status.paired => Span::styled(
                app.text(UiText::ControllerNoUnusedInvite),
                app.theme.warning,
            ),
            None => Span::styled(
                app.text(UiText::ControllerStartServerFirst),
                app.theme.metadata,
            ),
        },
    ]));
}

fn selected_controller_slot(item: ControllerSetupItem) -> Option<ControllerSlot> {
    match item {
        ControllerSetupItem::PlayerOne => Some(ControllerSlot::One),
        ControllerSetupItem::PlayerTwo => Some(ControllerSlot::Two),
        ControllerSetupItem::BindAddress
        | ControllerSetupItem::LanServer
        | ControllerSetupItem::TerminalPointer
        | ControllerSetupItem::Back => None,
    }
}

fn themed_block<'a>(app: &App, title: &'a str) -> Block<'a> {
    Block::default()
        .borders(Borders::ALL)
        .border_style(app.theme.border)
        .title(title)
        .title_style(app.theme.title)
}
