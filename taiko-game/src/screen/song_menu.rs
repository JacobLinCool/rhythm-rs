use std::path::Path;

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap};

use crate::app::App;
use crate::tui::Frame;

pub fn render(app: &App, frame: &mut Frame<'_>, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(56), Constraint::Percentage(44)])
        .split(area);

    let list_items = if app.filtered_song_indices.is_empty() {
        vec![ListItem::new(Line::from(Span::styled(
            "(no matching songs)",
            app.theme.warning,
        )))]
    } else {
        app.filtered_song_indices
            .iter()
            .filter_map(|index| app.songs.get(*index))
            .map(|song| {
                let subtitle = if song.subtitle.trim().is_empty() {
                    String::new()
                } else {
                    format!(" - {}", song.subtitle)
                };
                ListItem::new(Line::from(vec![
                    Span::styled(song.title.clone(), app.theme.text_primary),
                    Span::styled(subtitle, app.theme.text_secondary),
                ]))
            })
            .collect::<Vec<_>>()
    };

    let list = List::new(list_items)
        .block(themed_block(
            app,
            "Song Menu (Type to filter, Arrow to move, Enter to select)",
        ))
        .highlight_style(app.theme.selection)
        .highlight_symbol(">> ");

    let mut state = ListState::default();
    state.select((!app.filtered_song_indices.is_empty()).then_some(app.song_index));
    frame.render_stateful_widget(list, chunks[0], &mut state);

    let search_query = if app.song_query.is_empty() {
        "<empty>".to_owned()
    } else {
        app.song_query.clone()
    };
    let matches_line = format!("{}/{}", app.visible_song_count(), app.songs.len());

    let mut info_lines = vec![
        kv_line(app, "Search", search_query),
        kv_line(app, "Matches", matches_line),
    ];

    if let Some(error) = app.song_filter_error.as_ref() {
        info_lines.push(Line::from(vec![
            Span::styled("Filter error: ", app.theme.label),
            Span::styled(error.clone(), app.theme.error),
        ]));
        info_lines.push(line_value(app, ""));
    }

    if let Some(song) = app.selected_song() {
        let bpm = song
            .courses
            .first()
            .and_then(|course| course.base_bpm)
            .unwrap_or(0.0);

        info_lines.extend(vec![
            line_value(app, ""),
            kv_line(app, "Title", song.title.clone()),
            kv_line(app, "Subtitle", song.subtitle.clone()),
            kv_line(app, "Artist", song.artist.clone()),
            kv_line(app, "BPM", format!("{bpm:.2}")),
            kv_line(
                app,
                "Chart",
                relative_to_songdir(&app.args.songdir, &song.source_path),
            ),
            kv_line(
                app,
                "Audio",
                relative_to_songdir(&app.args.songdir, &song.audio_path),
            ),
            kv_line(app, "Courses", song.courses.len().to_string()),
            kv_line(app, "Branching", song.has_branching().to_string()),
            kv_line(app, "Demo", format!("{:.2}s", song.demo_start_seconds)),
            line_value(app, ""),
            if app.load_warnings.is_empty() {
                kv_line(app, "Load warnings", "0".to_owned())
            } else {
                Line::from(vec![
                    Span::styled("Load warnings: ", app.theme.label),
                    Span::styled(app.load_warnings.len().to_string(), app.theme.warning),
                ])
            },
            line_value(app, ""),
            line_value(app, "Keys:"),
            line_value(app, "- Filter: type text, Backspace/Delete edit, Esc clear"),
            line_value(app, "- Confirm: Enter"),
            line_value(app, "- Navigate: Arrow keys"),
            line_value(app, "- Load warnings: Ctrl+W"),
            line_value(app, "- Quit: Esc or Ctrl+C"),
            line_value(app, ""),
            line_value(app, "Magic words examples:"),
            line_value(app, "- oni=9"),
            line_value(app, "- oni=8,9,10"),
            line_value(app, "- hard=4-7"),
            line_value(app, "- lvl=8-10  branch  bpm>=180"),
        ]);
    } else {
        info_lines.push(line_value(app, "No song matched current filter"));
        info_lines.push(line_value(app, ""));
        info_lines.push(line_value(app, "Magic words examples:"));
        info_lines.push(line_value(app, "- oni=9"));
        info_lines.push(line_value(app, "- oni=8,9,10"));
        info_lines.push(line_value(app, "- hard=4-7"));
        info_lines.push(line_value(app, "- lvl=8-10  branch  bpm>=180"));
        info_lines.push(line_value(app, "- nobranch  bpm=120-180"));
    }

    let info = Paragraph::new(info_lines)
        .block(themed_block(app, "Song Info"))
        .style(app.theme.text_primary)
        .wrap(Wrap { trim: true });
    frame.render_widget(info, chunks[1]);
}

fn themed_block<'a>(app: &App, title: &'a str) -> Block<'a> {
    Block::default()
        .borders(Borders::ALL)
        .border_style(app.theme.border)
        .title(title)
        .title_style(app.theme.title)
}

fn kv_line(app: &App, label: &str, value: String) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{label}: "), app.theme.label),
        Span::styled(value, app.theme.value),
    ])
}

fn line_value(app: &App, value: &str) -> Line<'static> {
    Line::from(Span::styled(value.to_owned(), app.theme.text_primary))
}

fn relative_to_songdir(songdir: &Path, path: &Path) -> String {
    path.strip_prefix(songdir)
        .unwrap_or(path)
        .display()
        .to_string()
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::relative_to_songdir;

    #[test]
    fn path_is_relative_to_songdir_when_possible() {
        let songdir = Path::new("/songs");
        let chart = Path::new("/songs/pack1/demo.tja");

        assert_eq!(relative_to_songdir(songdir, chart), "pack1/demo.tja");
    }

    #[test]
    fn path_falls_back_to_original_when_outside_songdir() {
        let songdir = Path::new("/songs");
        let chart = Path::new("/other/demo.tja");

        assert_eq!(relative_to_songdir(songdir, chart), "/other/demo.tja");
    }
}
