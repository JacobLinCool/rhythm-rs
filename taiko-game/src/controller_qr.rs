//! Terminal QR codes used to transfer controller-pairing URLs.

use std::{
    error::Error,
    fmt::{self, Display, Formatter},
};

use qrcode::{
    render::{unicode::Dense1x2, Renderer},
    types::QrError,
    QrCode,
};
use ratatui::text::Line;

const QUIET_ZONE_MODULES: u32 = 4;

/// A QR code rendered at one terminal column per module and two modules per row.
#[derive(Clone, Debug)]
pub(crate) struct ControllerQr {
    pub(crate) lines: Vec<Line<'static>>,
    pub(crate) width: usize,
    pub(crate) height: usize,
}

/// Failure to encode a controller-pairing URL as a QR code.
#[derive(Debug)]
pub(crate) struct ControllerQrError {
    source: QrError,
}

impl Display for ControllerQrError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "controller pairing URL cannot be encoded as a QR code: {}",
            self.source
        )
    }
}

impl Error for ControllerQrError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(&self.source)
    }
}

impl From<QrError> for ControllerQrError {
    fn from(source: QrError) -> Self {
        Self { source }
    }
}

/// Encodes a pairing URL with the QR-standard four-module quiet zone.
///
/// `width` and `height` are exact terminal-cell dimensions. Each output line
/// has exactly `width` cells and `lines.len() == height`.
pub(crate) fn encode_controller_url(url: &str) -> Result<ControllerQr, ControllerQrError> {
    let code = QrCode::new(url.as_bytes())?;
    let module_width = code.width() + (QUIET_ZONE_MODULES as usize * 2);
    let colors = code.to_colors();
    let rendered = Renderer::<Dense1x2>::new(&colors, code.width(), QUIET_ZONE_MODULES)
        .module_dimensions(1, 1)
        .build();
    let lines = rendered
        .split('\n')
        .map(|line| Line::from(line.to_owned()))
        .collect::<Vec<_>>();
    let height = module_width.div_ceil(2);

    debug_assert_eq!(lines.len(), height);
    debug_assert!(lines.iter().all(|line| line.width() == module_width));

    Ok(ControllerQr {
        lines,
        width: module_width,
        height,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIRST_URL: &str = "http://192.0.2.10:37841/controller/?lang=en#token=aaaaaaaa";
    const SECOND_URL: &str = "http://192.0.2.10:37841/controller/?lang=en#token=bbbbbbbb";

    fn rendered_rows(qr: &ControllerQr) -> Vec<String> {
        qr.lines.iter().map(ToString::to_string).collect()
    }

    #[test]
    fn dimensions_and_four_module_quiet_zone_are_exact() {
        let qr = encode_controller_url(FIRST_URL).expect("test URL should be encodable");
        let encoded_module_width = QrCode::new(FIRST_URL.as_bytes())
            .expect("test URL should be encodable")
            .width();
        let expected_width = encoded_module_width + 2 * QUIET_ZONE_MODULES as usize;
        let rows = rendered_rows(&qr);

        assert_eq!(qr.width, expected_width);
        assert_eq!(qr.height, expected_width.div_ceil(2));
        assert_eq!(rows.len(), qr.height);
        assert!(rows.iter().all(|row| row.chars().count() == qr.width));

        assert!(rows[..2].iter().all(|row| row.chars().all(|ch| ch == ' ')));
        assert!(rows[qr.height - 2..]
            .iter()
            .all(|row| row.chars().all(|ch| ch == ' ')));
        assert!(rows[2].chars().nth(4).is_some_and(|ch| ch != ' '));
        assert!(rows[2]
            .chars()
            .nth(qr.width - 5)
            .is_some_and(|ch| ch != ' '));
        assert!(rows[qr.height - 3]
            .chars()
            .nth(4)
            .is_some_and(|ch| ch != ' '));
        assert!(rows.iter().all(|row| {
            row.chars().take(4).all(|ch| ch == ' ') && row.chars().rev().take(4).all(|ch| ch == ' ')
        }));
    }

    #[test]
    fn rendered_content_changes_with_url() {
        let first = encode_controller_url(FIRST_URL).expect("first test URL should be encodable");
        let second =
            encode_controller_url(SECOND_URL).expect("second test URL should be encodable");

        assert!(
            first.lines != second.lines,
            "different URLs must not produce identical QR content"
        );
    }

    #[test]
    fn oversized_url_returns_typed_error() {
        let oversized_url = "x".repeat(10_000);

        let result = encode_controller_url(&oversized_url);

        assert!(matches!(
            result,
            Err(ControllerQrError {
                source: QrError::DataTooLong
            })
        ));
    }
}
