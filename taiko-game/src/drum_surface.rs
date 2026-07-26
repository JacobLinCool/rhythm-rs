use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Paragraph};
use rhythm_mode_taiko::TaikoAction;

use crate::controller::ControllerSlot;
use crate::tui::Frame;

const PAD_COUNT: u16 = 4;
const MIN_PAD_WIDTH: u16 = 3;
const MIN_SURFACE_HEIGHT: u16 = 3;

pub(crate) const DRUM_SURFACE_ACTIONS: [TaikoAction; 4] = [
    TaikoAction::LEFT_KAT,
    TaikoAction::LEFT_DON,
    TaikoAction::RIGHT_DON,
    TaikoAction::RIGHT_KAT,
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DrumSurfaceLabels<'a> {
    pub(crate) left_kat: Line<'a>,
    pub(crate) left_don: Line<'a>,
    pub(crate) right_don: Line<'a>,
    pub(crate) right_kat: Line<'a>,
}

impl<'a> DrumSurfaceLabels<'a> {
    pub(crate) fn new(
        left_kat: impl Into<Line<'a>>,
        left_don: impl Into<Line<'a>>,
        right_don: impl Into<Line<'a>>,
        right_kat: impl Into<Line<'a>>,
    ) -> Self {
        Self {
            left_kat: left_kat.into(),
            left_don: left_don.into(),
            right_don: right_don.into(),
            right_kat: right_kat.into(),
        }
    }

    fn into_array(self) -> [Line<'a>; 4] {
        [self.left_kat, self.left_don, self.right_don, self.right_kat]
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DrumSurfaceView<'a> {
    pub(crate) labels: DrumSurfaceLabels<'a>,
    pub(crate) border_style: Style,
    pub(crate) active_border_style: Style,
    pub(crate) active_action: Option<TaikoAction>,
}

impl<'a> DrumSurfaceView<'a> {
    pub(crate) const fn new(
        labels: DrumSurfaceLabels<'a>,
        border_style: Style,
        active_border_style: Style,
        active_action: Option<TaikoAction>,
    ) -> Self {
        Self {
            labels,
            border_style,
            active_border_style,
            active_action,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DrumSurfaceLayout {
    pub(crate) area: Rect,
    pub(crate) slot: ControllerSlot,
}

impl DrumSurfaceLayout {
    /// Creates one renderable four-pad surface.
    ///
    /// Each pad owns at least one content column between its borders. The
    /// rectangle must also have representable half-open right and bottom edges.
    pub(crate) fn new(area: Rect, slot: ControllerSlot) -> Option<Self> {
        let minimum_width = PAD_COUNT.checked_mul(MIN_PAD_WIDTH)?;
        if area.width < minimum_width
            || area.height < MIN_SURFACE_HEIGHT
            || area.x.checked_add(area.width).is_none()
            || area.y.checked_add(area.height).is_none()
        {
            return None;
        }
        Some(Self { area, slot })
    }

    /// Partitions the complete surface from left to right without gaps or overlap.
    ///
    /// Extra columns are assigned from the leftmost pad first.
    pub(crate) fn pad_areas(self) -> [Rect; 4] {
        let base_width = self.area.width / PAD_COUNT;
        let remainder = self.area.width % PAD_COUNT;
        let mut x = self.area.x;
        std::array::from_fn(|index| {
            let width = base_width + u16::from((index as u16) < remainder);
            let pad = Rect::new(x, self.area.y, width, self.area.height);
            x = x
                .checked_add(width)
                .expect("validated drum surface coordinates cannot overflow");
            pad
        })
    }

    /// Maps one primary-button press to the action rendered at that terminal cell.
    pub(crate) fn hit_test(self, event: MouseEvent) -> Option<TaikoAction> {
        if event.kind != MouseEventKind::Down(MouseButton::Left) {
            return None;
        }

        self.pad_areas()
            .into_iter()
            .zip(DRUM_SURFACE_ACTIONS)
            .find_map(|(area, action)| {
                area.contains((event.column, event.row).into())
                    .then_some(action)
            })
    }

    /// Renders labels and active feedback using the same pad rectangles as hit testing.
    pub(crate) fn render(self, frame: &mut Frame<'_>, view: DrumSurfaceView<'_>) {
        for ((area, action), label) in self
            .pad_areas()
            .into_iter()
            .zip(DRUM_SURFACE_ACTIONS)
            .zip(view.labels.into_array())
        {
            let border_style = if view.active_action == Some(action) {
                view.active_border_style
            } else {
                view.border_style
            };
            let block = Block::default()
                .borders(Borders::ALL)
                .border_style(border_style);
            let pad = Paragraph::new(label.centered()).block(block);
            frame.render_widget(pad, area);
        }
    }
}

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
    use ratatui::backend::TestBackend;
    use ratatui::layout::Rect;
    use ratatui::style::{Color, Style};
    use ratatui::Terminal;
    use rhythm_mode_taiko::TaikoAction;

    use super::{DrumSurfaceLabels, DrumSurfaceLayout, DrumSurfaceView, DRUM_SURFACE_ACTIONS};
    use crate::controller::ControllerSlot;

    fn mouse(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    #[test]
    fn layout_requires_four_bordered_pads_and_representable_coordinates() {
        assert!(DrumSurfaceLayout::new(Rect::new(0, 0, 11, 3), ControllerSlot::One).is_none());
        assert!(DrumSurfaceLayout::new(Rect::new(0, 0, 12, 2), ControllerSlot::One).is_none());
        assert!(
            DrumSurfaceLayout::new(Rect::new(u16::MAX - 5, 0, 12, 3), ControllerSlot::One)
                .is_none()
        );
        assert!(
            DrumSurfaceLayout::new(Rect::new(0, u16::MAX - 1, 12, 3), ControllerSlot::One)
                .is_none()
        );
        assert!(DrumSurfaceLayout::new(Rect::new(0, 0, 12, 3), ControllerSlot::One).is_some());
    }

    #[test]
    fn pad_geometry_covers_every_column_once_and_distributes_remainder_left_first() {
        let layout = DrumSurfaceLayout::new(Rect::new(7, 3, 81, 5), ControllerSlot::Two)
            .expect("valid layout");

        assert_eq!(
            layout.pad_areas(),
            [
                Rect::new(7, 3, 21, 5),
                Rect::new(28, 3, 20, 5),
                Rect::new(48, 3, 20, 5),
                Rect::new(68, 3, 20, 5),
            ]
        );
        assert_eq!(
            layout
                .pad_areas()
                .iter()
                .map(|area| area.width)
                .sum::<u16>(),
            layout.area.width
        );
    }

    #[test]
    fn every_pad_edge_maps_to_the_physical_left_to_right_action() {
        let layout = DrumSurfaceLayout::new(Rect::new(4, 2, 48, 4), ControllerSlot::One)
            .expect("valid layout");

        for (area, action) in layout.pad_areas().into_iter().zip(DRUM_SURFACE_ACTIONS) {
            for (column, row) in [
                (area.x, area.y),
                (area.right() - 1, area.y),
                (area.x, area.bottom() - 1),
                (area.right() - 1, area.bottom() - 1),
            ] {
                assert_eq!(
                    layout.hit_test(mouse(MouseEventKind::Down(MouseButton::Left), column, row)),
                    Some(action)
                );
            }
        }

        assert_eq!(
            DRUM_SURFACE_ACTIONS,
            [
                TaikoAction::LEFT_KAT,
                TaikoAction::LEFT_DON,
                TaikoAction::RIGHT_DON,
                TaikoAction::RIGHT_KAT,
            ]
        );
    }

    #[test]
    fn hit_test_rejects_outside_coordinates_and_every_non_primary_down_kind() {
        let layout = DrumSurfaceLayout::new(Rect::new(10, 5, 48, 3), ControllerSlot::One)
            .expect("valid layout");
        let inside = (11, 6);

        for kind in [
            MouseEventKind::Down(MouseButton::Right),
            MouseEventKind::Down(MouseButton::Middle),
            MouseEventKind::Up(MouseButton::Left),
            MouseEventKind::Drag(MouseButton::Left),
            MouseEventKind::Moved,
            MouseEventKind::ScrollDown,
            MouseEventKind::ScrollUp,
            MouseEventKind::ScrollLeft,
            MouseEventKind::ScrollRight,
        ] {
            assert_eq!(layout.hit_test(mouse(kind, inside.0, inside.1)), None);
        }

        for (column, row) in [(9, 6), (58, 6), (11, 4), (11, 8)] {
            assert_eq!(
                layout.hit_test(mouse(MouseEventKind::Down(MouseButton::Left), column, row)),
                None
            );
        }
    }

    #[test]
    fn render_labels_and_hit_testing_use_the_same_pad_rectangles() {
        let backend = TestBackend::new(48, 3);
        let mut terminal = Terminal::new(backend).expect("terminal");
        let layout = DrumSurfaceLayout::new(Rect::new(0, 0, 48, 3), ControllerSlot::One)
            .expect("valid layout");
        let labels = DrumSurfaceLabels::new("A", "B", "C", "D");
        let view = DrumSurfaceView::new(
            labels,
            Style::default().fg(Color::Blue),
            Style::default().fg(Color::Yellow),
            Some(TaikoAction::RIGHT_DON),
        );

        terminal
            .draw(|frame| layout.render(frame, view))
            .expect("draw surface");

        let buffer = terminal.backend().buffer();
        for ((area, action), expected_label) in layout
            .pad_areas()
            .into_iter()
            .zip(DRUM_SURFACE_ACTIONS)
            .zip(["A", "B", "C", "D"])
        {
            let label_cell = buffer
                .content()
                .iter()
                .enumerate()
                .find_map(|(offset, cell)| (cell.symbol() == expected_label).then_some(offset))
                .expect("rendered label");
            let column = u16::try_from(label_cell % 48).expect("column fits");
            let row = u16::try_from(label_cell / 48).expect("row fits");
            assert!(area.contains((column, row).into()));
            assert_eq!(
                layout.hit_test(mouse(MouseEventKind::Down(MouseButton::Left), column, row)),
                Some(action)
            );
        }

        let right_don = layout.pad_areas()[2];
        assert_eq!(
            buffer[(right_don.x, right_don.y)].style().fg,
            Some(Color::Yellow)
        );
        assert_eq!(
            buffer[(layout.pad_areas()[0].x, layout.pad_areas()[0].y)]
                .style()
                .fg,
            Some(Color::Blue)
        );
    }
}
