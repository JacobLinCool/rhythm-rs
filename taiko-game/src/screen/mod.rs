pub mod controllers;
pub mod course_menu;
pub mod error_screen;
pub mod game_screen;
pub mod load_warnings_screen;
pub mod local_course;
pub mod local_game;
pub mod local_result;
pub mod mode_select;
pub mod mp_connect;
pub mod offline_preparation;
pub mod online_course;
pub mod online_lobby;
pub mod online_match;
pub mod online_result;
pub mod result_screen;
pub mod settings;
pub mod song_menu;

use std::sync::OnceLock;
use std::time::Instant;

use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};

use crate::app::{App, LeaveTarget, Page};
use crate::controller::ControllerSlot;
use crate::drum_surface::{DrumSurfaceLabels, DrumSurfaceLayout, DrumSurfaceView};
use crate::localization::{truncate_to_width, UiMessage, UiText};
use crate::tui::Frame;

const GAUGE_BAR_MIN_WIDTH: usize = 12;
const GAUGE_BAR_MAX_WIDTH: usize = 60;
const GAUGE_FIXED_COLUMNS: usize = 24;
pub(crate) const MIN_TERMINAL_WIDTH: u16 = 80;
pub(crate) const MIN_TERMINAL_HEIGHT: u16 = 24;
pub(crate) const MIN_ONLINE_MATCH_HEIGHT: u16 = 25;
pub(crate) const MIN_LOCAL_GAME_HEIGHT: u16 = 31;
const LOCAL_GAME_CONTROLLER_SURFACE_HEIGHT: u16 = 3;

pub(crate) const fn minimum_terminal_size(page: Page) -> (u16, u16) {
    let height = match page {
        Page::LocalGame => MIN_LOCAL_GAME_HEIGHT,
        Page::OnlineMatch => MIN_ONLINE_MATCH_HEIGHT,
        _ => MIN_TERMINAL_HEIGHT,
    };
    (MIN_TERMINAL_WIDTH, height)
}

pub(crate) fn minimum_terminal_size_for_app(app: &App) -> (u16, u16) {
    let (width, mut height) = minimum_terminal_size(app.page);
    if app.page == Page::LocalGame {
        height = height.saturating_add(u16::from(!app.keyboard_repeat_is_distinguishable));
        if app.terminal_pointer_slot().is_some() || app.mac_trackpad_slot().is_some() {
            height = height.saturating_add(LOCAL_GAME_CONTROLLER_SURFACE_HEIGHT);
        }
    }
    (width, height)
}

pub(crate) fn terminal_is_too_small_for_app(app: &App, area: Rect) -> bool {
    let (required_width, required_height) = minimum_terminal_size_for_app(app);
    area.width < required_width || area.height < required_height
}

pub(crate) fn render_terminal_guard(app: &App, frame: &mut Frame<'_>, area: Rect) {
    let (required_width, required_height) = minimum_terminal_size_for_app(app);
    let message = vec![
        Line::from(Span::styled(
            app.text(UiText::TerminalTooSmall),
            app.theme.error,
        )),
        Line::from(""),
        Line::from(Span::styled(
            app.localizer().message(UiMessage::CurrentTerminalSize {
                width: area.width,
                height: area.height,
            }),
            app.theme.value,
        )),
        Line::from(Span::styled(
            app.localizer().message(UiMessage::RequiredTerminalSize {
                width: required_width,
                height: required_height,
            }),
            app.theme.metadata,
        )),
        Line::from(""),
        Line::from(Span::styled(
            app.text(UiText::ResizeTerminal),
            app.theme.text_primary,
        )),
    ];
    frame.render_widget(
        Paragraph::new(message)
            .centered()
            .wrap(Wrap { trim: true })
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(app.theme.error)
                    .title(format!(" {} ", app.text(UiText::TaikoBrand))),
            ),
        area,
    );
}

pub(crate) fn render_leave_confirmation(
    app: &App,
    frame: &mut Frame<'_>,
    area: Rect,
    target: LeaveTarget,
) {
    let width = area.width.saturating_sub(4).min(66);
    let height = 7.min(area.height.saturating_sub(2));
    let modal = centered_rect(area, width, height);
    frame.render_widget(Clear, modal);
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled(
                app.text(UiText::LeaveThisMatch),
                app.theme.warning,
            )),
            Line::from(""),
            Line::from(vec![
                Span::styled(
                    format!("{}: ", app.text(UiText::Destination)),
                    app.theme.label,
                ),
                Span::styled(app.text(target.destination_key()), app.theme.value),
            ]),
            Line::from(Span::styled(
                app.text(UiText::LeaveConfirmHint),
                app.theme.metadata,
            )),
        ])
        .centered()
        .wrap(Wrap { trim: true })
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(app.theme.warning)
                .title(app.text(UiText::ConfirmLeave)),
        ),
        modal,
    );
}

fn centered_rect(area: Rect, width: u16, height: u16) -> Rect {
    Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 2,
        width: width.min(area.width),
        height: height.min(area.height),
    }
}

pub fn render_topbar(app: &App, frame: &mut Frame<'_>, area: Rect) {
    let chunks =
        Layout::horizontal([Constraint::Percentage(65), Constraint::Percentage(35)]).split(area);

    let (left, right, right_style) = match app.page {
        Page::ModeSelect => (
            app.text(UiText::TopModeSelect).to_owned(),
            app.text(UiText::TopModeSelectHelp).to_owned(),
            app.theme.metadata,
        ),
        Page::Controllers => (
            app.text(UiText::TopControllers).to_owned(),
            app.text(UiText::TopControllersHelp).to_owned(),
            app.theme.metadata,
        ),
        Page::Settings => (
            app.text(UiText::TopSettings).to_owned(),
            app.text(UiText::TopSettingsHelp).to_owned(),
            app.theme.metadata,
        ),
        Page::SongMenu => (
            format!(
                "{} // {} // {}",
                app.text(UiText::TaikoBrand),
                app.active_mode
                    .map_or(app.text(UiText::Offline), |mode| {
                        app.text(mode.label_key())
                    })
                    .to_uppercase(),
                app.text(UiText::SongSelect)
            ),
            app.localizer().message(UiMessage::SongCount {
                visible: app.visible_song_count(),
                total: app.songs.len(),
            }),
            app.theme.metadata,
        ),
        Page::LoadWarnings => (
            app.text(UiText::TopLoadWarnings).to_owned(),
            app.text(UiText::EscBack).to_owned(),
            app.theme.metadata,
        ),
        Page::CourseMenu => {
            let title = app
                .selected_song()
                .map_or(app.text(UiText::None), |song| song.title.as_str());
            (
                format!("♪ {title} // {}", app.text(UiText::SelectCourse)),
                if app.auto_play {
                    app.text(UiText::AutoPlay).to_owned()
                } else {
                    app.text(UiText::ManualPlay).to_owned()
                },
                if app.auto_play {
                    app.theme.warning
                } else {
                    app.theme.metadata
                },
            )
        }
        Page::OfflinePreparation => (
            app.text(UiText::TopPreparingMatch).to_owned(),
            app.text(UiText::PreparingMatchHelp).to_owned(),
            app.theme.warning,
        ),
        Page::Game => {
            if let Some(game) = app.game.as_ref() {
                let title = app
                    .songs
                    .get(game.song_index)
                    .map_or(app.text(UiText::UnknownSong), |song| song.title.as_str());
                let (status, status_style) = if game.paused {
                    (app.text(UiText::Paused).to_owned(), app.theme.warning)
                } else if app.auto_play {
                    (app.text(UiText::AutoPlay).to_owned(), app.theme.warning)
                } else {
                    (app.text(UiText::GameHelp).to_owned(), app.theme.metadata)
                };
                (
                    format!("♪ {title}  //  {}", game.course_name.to_uppercase()),
                    status,
                    status_style,
                )
            } else {
                (
                    app.text(UiText::TopPlay).to_owned(),
                    String::new(),
                    app.theme.metadata,
                )
            }
        }
        Page::Result => (
            app.text(UiText::TopResult).to_owned(),
            app.text(UiText::ResultHelp).to_owned(),
            app.theme.metadata,
        ),
        Page::LocalCourseSelect => (
            app.text(UiText::TopLocalCourses).to_owned(),
            "P1 W/S/F  •  P2 ↑/↓/J".to_owned(),
            app.theme.metadata,
        ),
        Page::LocalGame => {
            let paused = app.local_game.as_ref().is_some_and(|game| game.paused);
            (
                app.text(UiText::TopLocalVersus).to_owned(),
                if paused {
                    app.text(UiText::Paused).to_owned()
                } else {
                    format!(
                        "P1 {}  •  P2 {}",
                        binding_summary(app.preferences.player_one),
                        binding_summary(app.preferences.player_two),
                    )
                },
                if paused {
                    app.theme.warning
                } else {
                    app.theme.metadata
                },
            )
        }
        Page::LocalResult => (
            app.text(UiText::TopLocalResult).to_owned(),
            app.text(UiText::LocalResultHelp).to_owned(),
            app.theme.metadata,
        ),
        Page::Error => (
            app.text(UiText::TopError).to_owned(),
            app.error_state.as_ref().map_or_else(
                || app.text(UiText::ErrorDefaultHelp).to_owned(),
                |state| {
                    let destination = app.text(state.recovery.label_key()).to_uppercase();
                    app.localizer().message(UiMessage::ErrorTopbar {
                        has_retry: state.retry.is_some(),
                        destination: &destination,
                    })
                },
            ),
            app.theme.error,
        ),
        Page::MultiplayerConnect => (
            app.text(UiText::TopOnline).to_owned(),
            app.text(UiText::OnlineModes).to_owned(),
            app.theme.metadata,
        ),
        Page::OnlineLobby => {
            let room = app
                .online
                .as_ref()
                .and_then(|online| online.room_code())
                .map(taiko_multiplayer_protocol::RoomCode::as_str)
                .unwrap_or("...");
            (
                app.text(UiText::TopOnlineLobby).to_owned(),
                app.localizer().message(UiMessage::RoomCode { room }),
                app.theme.selection,
            )
        }
        Page::OnlineCourseSelect => (
            app.text(UiText::TopOnlineCourse).to_owned(),
            app.online
                .as_ref()
                .map_or(app.text(UiText::Waiting), |online| {
                    app.localizer().online_phase(online.phase())
                })
                .to_uppercase(),
            app.theme.metadata,
        ),
        Page::OnlineMatch => (
            app.text(UiText::TopOnlineMatch).to_owned(),
            app.online
                .as_ref()
                .map_or(app.text(UiText::Waiting), |online| {
                    app.localizer().online_phase(online.phase())
                })
                .to_uppercase(),
            app.theme.selection,
        ),
        Page::OnlineResult => (
            app.text(UiText::TopOnlineResult).to_owned(),
            app.online
                .as_ref()
                .map_or(app.text(UiText::Result), |online| {
                    app.localizer().online_phase(online.phase())
                })
                .to_uppercase(),
            app.theme.metadata,
        ),
    };

    let left = truncate_to_width(&left, usize::from(chunks[0].width));
    let right = truncate_to_width(&right, usize::from(chunks[1].width));

    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(left, app.theme.title))),
        chunks[0],
    );
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(right, right_style))).right_aligned(),
        chunks[1],
    );
}

pub(crate) fn render_controller_drum_surface(
    app: &App,
    frame: &mut Frame<'_>,
    area: Rect,
    slot: ControllerSlot,
    active_action: Option<rhythm_mode_taiko::TaikoAction>,
) -> Option<DrumSurfaceLayout> {
    let layout = DrumSurfaceLayout::new(area, slot)?;
    let player = match slot {
        ControllerSlot::One => "P1",
        ControllerSlot::Two => "P2",
    };
    let label = |side: char, name: &'static str, style| {
        Line::from(vec![
            Span::styled(format!("{player}{side}-"), app.theme.title),
            Span::styled(name.to_uppercase(), style),
        ])
    };
    let labels = DrumSurfaceLabels::new(
        label('L', app.text(UiText::Kat), app.theme.lane_note_kat),
        label('L', app.text(UiText::Don), app.theme.lane_note_don),
        label('R', app.text(UiText::Don), app.theme.lane_note_don),
        label('R', app.text(UiText::Kat), app.theme.lane_note_kat),
    );
    layout.render(
        frame,
        DrumSurfaceView::new(labels, app.theme.border, app.theme.selection, active_action),
    );
    Some(layout)
}

fn binding_summary(bindings: crate::preferences::DrumBindings) -> String {
    format!(
        "{}/{}/{}/{}",
        bindings.left_kat.to_ascii_uppercase(),
        bindings.left_don.to_ascii_uppercase(),
        bindings.right_don.to_ascii_uppercase(),
        bindings.right_kat.to_ascii_uppercase(),
    )
}

pub(crate) fn render_gauge_bar_line(
    app: &App,
    gauge: f32,
    pass_threshold: f32,
    available_columns: u16,
    show_status: bool,
) -> Line<'static> {
    let gauge = gauge.clamp(0.0, 1.0);
    let pass_threshold = pass_threshold.clamp(0.0, 1.0);
    let full_blink_on = full_gauge_blink_on();
    let bar_width = gauge_bar_width(available_columns);
    let fill_count = (gauge * bar_width as f32).round() as usize;
    let pass_idx = threshold_index(bar_width, pass_threshold);
    let full_idx = bar_width.saturating_sub(1);

    let mut spans = Vec::with_capacity(bar_width + 10);
    spans.push(Span::styled(
        format!("{}  [", app.text(UiText::Soul)),
        app.theme.label,
    ));

    for idx in 0..bar_width {
        let mut symbol = if idx < fill_count { "=" } else { "-" };
        let mut style = if idx < fill_count {
            gauge_fill_style(app, gauge, pass_threshold, full_blink_on)
        } else {
            app.theme.text_secondary
        };

        if idx == pass_idx {
            symbol = "|";
            style = if gauge >= pass_threshold {
                app.theme.warning
            } else {
                app.theme.text_secondary
            };
        }
        if idx == full_idx {
            symbol = "|";
            style = if gauge >= 1.0 {
                gauge_fill_style(app, gauge, pass_threshold, full_blink_on)
            } else {
                app.theme.metadata
            };
        }

        spans.push(Span::styled(symbol, style));
    }

    spans.push(Span::styled("] ", app.theme.label));
    spans.push(Span::styled(
        format!("{:>6.2}%", gauge * 100.0),
        gauge_fill_style(app, gauge, pass_threshold, full_blink_on),
    ));
    if show_status {
        spans.push(Span::styled(" ", app.theme.text_primary));

        let (status, status_style) = if gauge >= 1.0 {
            (
                app.text(UiText::Full),
                gauge_fill_style(app, gauge, pass_threshold, full_blink_on),
            )
        } else if gauge >= pass_threshold {
            (app.text(UiText::Pass), app.theme.warning)
        } else {
            (app.text(UiText::Fail), app.theme.error)
        };
        spans.push(Span::styled(status, status_style));
    }

    Line::from(spans)
}

fn gauge_fill_style(app: &App, gauge: f32, pass_threshold: f32, full_blink_on: bool) -> Style {
    if gauge >= 1.0 {
        if full_blink_on {
            app.theme.gauge_full
        } else {
            app.theme.warning
        }
    } else {
        app.theme.gauge_style(gauge, pass_threshold)
    }
}

fn full_gauge_blink_on() -> bool {
    static START: OnceLock<Instant> = OnceLock::new();
    let elapsed = START.get_or_init(Instant::now).elapsed();
    (elapsed.as_millis() / 180).is_multiple_of(2)
}

pub(crate) fn gauge_bar_width(available_columns: u16) -> usize {
    let dynamic = usize::from(available_columns).saturating_sub(GAUGE_FIXED_COLUMNS);
    dynamic.clamp(GAUGE_BAR_MIN_WIDTH, GAUGE_BAR_MAX_WIDTH)
}

fn threshold_index(bar_width: usize, threshold: f32) -> usize {
    if bar_width == 0 {
        return 0;
    }
    let last = bar_width - 1;
    ((threshold * last as f32).round() as usize).min(last)
}

#[cfg(test)]
mod tests {
    use super::{gauge_bar_width, minimum_terminal_size};
    use crate::app::Page;

    #[test]
    fn gauge_bar_width_is_clamped() {
        assert_eq!(gauge_bar_width(10), 12);
        assert_eq!(gauge_bar_width(36), 12);
        assert_eq!(gauge_bar_width(54), 30);
        assert_eq!(gauge_bar_width(200), 60);
    }

    #[test]
    fn page_specific_minimums_reserve_complete_lane_canvases() {
        assert_eq!(minimum_terminal_size(Page::SongMenu), (80, 24));
        assert_eq!(minimum_terminal_size(Page::LocalGame), (80, 31));
        assert_eq!(minimum_terminal_size(Page::OnlineMatch), (80, 25));
    }
}
