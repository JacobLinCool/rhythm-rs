use anyhow::{Context, Result};

/// Owns the platform clipboard for the lifetime of the application.
///
/// Keeping the handle alive is required on Linux, where the process that placed
/// text on the clipboard may also serve that text to other applications.
#[derive(Default)]
pub(crate) struct SystemClipboard {
    clipboard: Option<arboard::Clipboard>,
}

impl SystemClipboard {
    pub(crate) fn copy_text(&mut self, text: &str) -> Result<()> {
        if self.clipboard.is_none() {
            self.clipboard = Some(
                arboard::Clipboard::new()
                    .context("the operating-system clipboard is unavailable")?,
            );
        }

        self.clipboard
            .as_mut()
            .expect("clipboard is initialized above")
            .set_text(text.to_owned())
            .context("the operating-system clipboard rejected the invite")
    }
}
