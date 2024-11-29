use std::{ops::Range, path::PathBuf};

use color_eyre::eyre::Result;
use ratatui::widgets::ListState;

use crate::init::project_directory;

const VERSION_MESSAGE: &str = concat!(
    env!("CARGO_PKG_VERSION"),
    "-",
    env!("VERGEN_GIT_DESCRIBE"),
    " (",
    env!("VERGEN_BUILD_DATE"),
    ")"
);

/// Similar to the `std::dbg!` macro, but generates `tracing` events rather
/// than printing to stdout.
///
/// By default, the verbosity level for the generated events is `DEBUG`, but
/// this can be customized.
#[macro_export]
macro_rules! trace_dbg {
    (target: $target:expr, level: $level:expr, $ex:expr) => {{
        match $ex {
            value => {
                tracing::event!(target: $target, $level, ?value, stringify!($ex));
                value
            }
        }
    }};
    (level: $level:expr, $ex:expr) => {
        trace_dbg!(target: module_path!(), level: $level, $ex)
    };
    (target: $target:expr, $ex:expr) => {
        trace_dbg!(target: $target, level: tracing::Level::DEBUG, $ex)
    };
    ($ex:expr) => {
        trace_dbg!(level: tracing::Level::DEBUG, $ex)
    };
}

pub fn version() -> String {
    let author = clap::crate_authors!();

    let current_exe_path = PathBuf::from(clap::crate_name!());
    let current_exe_path = current_exe_path.display();
    let project_dirs = project_directory();

    format!(
        "\
{VERSION_MESSAGE}

Authors: {author}

Executable: {current_exe_path}
Directories: {project_dirs:?}
"
    )
}

pub fn read_utf8_or_shiftjis<P: AsRef<std::path::Path>>(path: P) -> Result<String> {
    let path = path.as_ref();
    let bytes = std::fs::read(path)?;
    let encoding = if !encoding_rs::UTF_8.decode_without_bom_handling(&bytes).1 {
        encoding_rs::UTF_8
    } else {
        encoding_rs::SHIFT_JIS
    };

    let (cow, _, _) = encoding.decode(&bytes);
    Ok(cow.into_owned())
}

pub fn select_next(list: &mut ListState, range: Range<usize>) -> Result<()> {
    list.select(Some((list.selected().unwrap_or(0) + 1) % range.end));
    Ok(())
}

pub fn select_prev(list: &mut ListState, range: Range<usize>) -> Result<()> {
    list.select(Some(
        (list.selected().unwrap_or(0) + range.end - 1) % range.end,
    ));
    Ok(())
}
