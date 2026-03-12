use std::path::Path;

use crate::loader::{CourseEntry, SongEntry};

#[derive(Debug, Clone)]
pub struct SongFilter {
    terms: Vec<FilterTerm>,
}

#[derive(Debug, Clone)]
enum FilterTerm {
    Text(String),
    DifficultyLevel {
        difficulty: Difficulty,
        levels: LevelMask,
    },
    AnyLevel {
        levels: LevelMask,
    },
    HasBranch(bool),
    Bpm(BpmPredicate),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Difficulty {
    Easy,
    Normal,
    Hard,
    Oni,
    Ura,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LevelMask(u16);

#[derive(Debug, Clone, Copy)]
enum BpmPredicate {
    Eq(f64),
    Gt(f64),
    Gte(f64),
    Lt(f64),
    Lte(f64),
    Range { min: f64, max: f64 },
}

impl SongFilter {
    pub fn parse(query: &str) -> Result<Self, String> {
        let mut terms = Vec::new();
        for raw in query.split_whitespace() {
            if raw.is_empty() {
                continue;
            }
            terms.push(parse_term(raw)?);
        }

        Ok(Self { terms })
    }

    pub fn matches(&self, song: &SongEntry, songdir: &Path) -> bool {
        self.terms.iter().all(|term| term.matches(song, songdir))
    }
}

impl FilterTerm {
    fn matches(&self, song: &SongEntry, songdir: &Path) -> bool {
        match self {
            Self::Text(term) => match_text(song, songdir, term),
            Self::DifficultyLevel { difficulty, levels } => song.courses.iter().any(|course| {
                detect_course_difficulty(course).is_some_and(|key| key == *difficulty)
                    && course.level.is_some_and(|level| levels.contains(level))
            }),
            Self::AnyLevel { levels } => song
                .courses
                .iter()
                .any(|course| course.level.is_some_and(|level| levels.contains(level))),
            Self::HasBranch(expected) => song.has_branching() == *expected,
            Self::Bpm(predicate) => song_base_bpm(song).is_some_and(|bpm| predicate.matches(bpm)),
        }
    }
}

impl LevelMask {
    fn empty() -> Self {
        Self(0)
    }

    fn all() -> Self {
        let mut mask = Self::empty();
        for level in 1..=10 {
            mask.insert(level);
        }
        mask
    }

    fn is_empty(self) -> bool {
        self.0 == 0
    }

    fn contains(self, level: u8) -> bool {
        let shift = u32::from(level);
        if shift > 10 {
            return false;
        }
        (self.0 & (1_u16 << shift)) != 0
    }

    fn insert(&mut self, level: u8) {
        let shift = u32::from(level);
        self.0 |= 1_u16 << shift;
    }
}

impl BpmPredicate {
    fn matches(self, bpm: f64) -> bool {
        match self {
            Self::Eq(v) => (bpm - v).abs() <= 0.05,
            Self::Gt(v) => bpm > v,
            Self::Gte(v) => bpm >= v,
            Self::Lt(v) => bpm < v,
            Self::Lte(v) => bpm <= v,
            Self::Range { min, max } => bpm >= min && bpm <= max,
        }
    }
}

fn parse_term(raw: &str) -> Result<FilterTerm, String> {
    let lowered = raw.to_ascii_lowercase();

    if matches!(lowered.as_str(), "branch" | "has:branch") {
        return Ok(FilterTerm::HasBranch(true));
    }
    if matches!(
        lowered.as_str(),
        "nobranch" | "no-branch" | "no:branch" | "-branch"
    ) {
        return Ok(FilterTerm::HasBranch(false));
    }

    if let Some(term) = parse_bpm_term(raw)? {
        return Ok(term);
    }

    if let Some((lhs, rhs)) = raw.split_once('=') {
        if let Some(difficulty) = parse_difficulty_alias(lhs) {
            return Ok(FilterTerm::DifficultyLevel {
                difficulty,
                levels: parse_level_mask(rhs)
                    .map_err(|e| format!("invalid `{raw}` filter: {e}"))?,
            });
        }

        if matches!(lhs.to_ascii_lowercase().as_str(), "lvl" | "level") {
            return Ok(FilterTerm::AnyLevel {
                levels: parse_level_mask(rhs)
                    .map_err(|e| format!("invalid `{raw}` filter: {e}"))?,
            });
        }
    }

    Ok(FilterTerm::Text(lowered))
}

fn parse_bpm_term(raw: &str) -> Result<Option<FilterTerm>, String> {
    let lowered = raw.to_ascii_lowercase();
    if !lowered.starts_with("bpm") {
        return Ok(None);
    }

    let expr = raw
        .get(3..)
        .ok_or_else(|| format!("invalid bpm filter: `{raw}`"))?;
    if expr.is_empty() {
        return Err(
            "invalid bpm filter: missing comparator (use bpm=, bpm>=, bpm<=, bpm>, bpm<)"
                .to_owned(),
        );
    }

    if let Some(v) = expr.strip_prefix(">=") {
        return Ok(Some(FilterTerm::Bpm(BpmPredicate::Gte(
            parse_positive_f64(v, raw)?,
        ))));
    }
    if let Some(v) = expr.strip_prefix("<=") {
        return Ok(Some(FilterTerm::Bpm(BpmPredicate::Lte(
            parse_positive_f64(v, raw)?,
        ))));
    }
    if let Some(v) = expr.strip_prefix('>') {
        return Ok(Some(FilterTerm::Bpm(BpmPredicate::Gt(parse_positive_f64(
            v, raw,
        )?))));
    }
    if let Some(v) = expr.strip_prefix('<') {
        return Ok(Some(FilterTerm::Bpm(BpmPredicate::Lt(parse_positive_f64(
            v, raw,
        )?))));
    }
    if let Some(v) = expr.strip_prefix('=') {
        if let Some((min, max)) = v.split_once('-') {
            let min = parse_positive_f64(min, raw)?;
            let max = parse_positive_f64(max, raw)?;
            if min > max {
                return Err(format!("invalid bpm range in `{raw}`: min must be <= max"));
            }
            return Ok(Some(FilterTerm::Bpm(BpmPredicate::Range { min, max })));
        }
        return Ok(Some(FilterTerm::Bpm(BpmPredicate::Eq(parse_positive_f64(
            v, raw,
        )?))));
    }

    Err(format!(
        "invalid bpm filter `{raw}` (use bpm=, bpm>=, bpm<=, bpm>, bpm<)"
    ))
}

fn parse_positive_f64(raw: &str, token: &str) -> Result<f64, String> {
    let value = raw
        .trim()
        .parse::<f64>()
        .map_err(|_| format!("invalid numeric value in `{token}`: `{raw}`"))?;
    if !(value.is_finite() && value > 0.0) {
        return Err(format!("numeric value must be > 0 in `{token}`: `{raw}`"));
    }
    Ok(value)
}

fn parse_level_mask(raw: &str) -> Result<LevelMask, String> {
    let spec = raw.trim();
    if spec.is_empty() {
        return Err("missing level spec".to_owned());
    }
    if spec == "*" {
        return Ok(LevelMask::all());
    }

    let mut mask = LevelMask::empty();
    for part in spec.split(',') {
        let token = part.trim();
        if token.is_empty() {
            return Err("empty list item".to_owned());
        }

        if let Some((start_raw, end_raw)) = token.split_once('-') {
            let start = parse_level(start_raw)?;
            let end = parse_level(end_raw)?;
            if start > end {
                return Err(format!("invalid range `{token}`: start must be <= end"));
            }
            for level in start..=end {
                mask.insert(level);
            }
        } else {
            mask.insert(parse_level(token)?);
        }
    }

    if mask.is_empty() {
        return Err("no valid level values".to_owned());
    }
    Ok(mask)
}

fn parse_level(raw: &str) -> Result<u8, String> {
    let level = raw
        .trim()
        .parse::<u8>()
        .map_err(|_| format!("invalid level `{raw}`"))?;
    if (1..=10).contains(&level) {
        Ok(level)
    } else {
        Err(format!("level out of range `{raw}` (expected 1..=10)"))
    }
}

fn parse_difficulty_alias(raw: &str) -> Option<Difficulty> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "easy" | "e" => Some(Difficulty::Easy),
        "normal" | "n" => Some(Difficulty::Normal),
        "hard" | "h" => Some(Difficulty::Hard),
        "oni" | "o" => Some(Difficulty::Oni),
        "ura" | "u" | "edit" => Some(Difficulty::Ura),
        _ => None,
    }
}

fn detect_course_difficulty(course: &CourseEntry) -> Option<Difficulty> {
    let candidate = course.name.as_str();
    if let Some(difficulty) = parse_difficulty_alias(candidate) {
        return Some(difficulty);
    }

    let normalized = candidate.trim().to_ascii_lowercase();
    if let Ok(idx) = normalized.parse::<u8>() {
        return match idx {
            0 => Some(Difficulty::Easy),
            1 => Some(Difficulty::Normal),
            2 => Some(Difficulty::Hard),
            3 => Some(Difficulty::Oni),
            4 => Some(Difficulty::Ura),
            _ => None,
        };
    }

    if normalized.contains("easy") {
        return Some(Difficulty::Easy);
    }
    if normalized.contains("normal") {
        return Some(Difficulty::Normal);
    }
    if normalized.contains("hard") {
        return Some(Difficulty::Hard);
    }
    if normalized.contains("oni") {
        return Some(Difficulty::Oni);
    }
    if normalized.contains("ura") || normalized.contains("edit") {
        return Some(Difficulty::Ura);
    }

    None
}

fn song_base_bpm(song: &SongEntry) -> Option<f64> {
    song.courses.first().and_then(|course| course.base_bpm)
}

fn match_text(song: &SongEntry, songdir: &Path, text: &str) -> bool {
    if text.is_empty() {
        return true;
    }

    let chart_rel = song
        .source_path
        .strip_prefix(songdir)
        .unwrap_or(&song.source_path)
        .display()
        .to_string();
    let audio_rel = song
        .audio_path
        .strip_prefix(songdir)
        .unwrap_or(&song.audio_path)
        .display()
        .to_string();

    let needle = text.to_ascii_lowercase();
    let fields = [
        song.title.to_ascii_lowercase(),
        song.subtitle.to_ascii_lowercase(),
        song.artist.to_ascii_lowercase(),
        chart_rel.to_ascii_lowercase(),
        audio_rel.to_ascii_lowercase(),
    ];

    fields.iter().any(|field| field.contains(&needle))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use rhythm_chart::TICKS_PER_SECOND;
    use rhythm_importer_tja::BranchDecisionPoint;

    use super::*;
    use crate::loader::CourseEntry;

    fn course(name: &str, level: u8, bpm: f64, branch: bool) -> CourseEntry {
        CourseEntry {
            index: 0,
            name: name.to_owned(),
            level: Some(level),
            object_count: 1,
            branch_segment_count: usize::from(branch),
            base_bpm: Some(bpm),
            branch_decisions: if branch {
                vec![BranchDecisionPoint {
                    segment_id: 1,
                    decision_tick: TICKS_PER_SECOND,
                    route_count: 3,
                    hint: None,
                }]
            } else {
                Vec::new()
            },
        }
    }

    fn song(title: &str, artist: &str, branch: bool, courses: Vec<CourseEntry>) -> SongEntry {
        let mut courses = courses;
        if branch {
            for c in &mut courses {
                if c.branch_decisions.is_empty() {
                    c.branch_decisions.push(BranchDecisionPoint {
                        segment_id: 1,
                        decision_tick: TICKS_PER_SECOND,
                        route_count: 3,
                        hint: None,
                    });
                }
            }
        }

        SongEntry {
            source_locator: crate::loader::ResourceLocator::LocalPath(PathBuf::from(format!(
                "/songs/{title}.tja"
            ))),
            audio_locator: crate::loader::ResourceLocator::LocalPath(PathBuf::from(format!(
                "/songs/{title}.ogg"
            ))),
            chart_content_hash: None,
            audio_content_hash: None,
            source_path: PathBuf::from(format!("/songs/{title}.tja")),
            audio_path: PathBuf::from(format!("/songs/{title}.ogg")),
            title: title.to_owned(),
            subtitle: String::new(),
            artist: artist.to_owned(),
            demo_start_seconds: 0.0,
            courses,
        }
    }

    #[test]
    fn difficulty_and_level_magic_words_work() {
        let filter = SongFilter::parse("oni=8,9,10 hard=4-7").expect("filter");
        let a = song(
            "A",
            "alice",
            false,
            vec![
                course("Oni", 9, 180.0, false),
                course("Hard", 6, 180.0, false),
            ],
        );
        let b = song(
            "B",
            "bob",
            false,
            vec![
                course("Oni", 10, 180.0, false),
                course("Hard", 8, 180.0, false),
            ],
        );

        assert!(filter.matches(&a, Path::new("/songs")));
        assert!(!filter.matches(&b, Path::new("/songs")));
    }

    #[test]
    fn branch_and_bpm_magic_words_work() {
        let filter = SongFilter::parse("branch bpm>=180").expect("filter");
        let a = song("A", "alice", true, vec![course("Hard", 5, 200.0, true)]);
        let b = song("B", "bob", true, vec![course("Hard", 5, 160.0, true)]);
        let c = song("C", "carol", false, vec![course("Hard", 5, 200.0, false)]);

        assert!(filter.matches(&a, Path::new("/songs")));
        assert!(!filter.matches(&b, Path::new("/songs")));
        assert!(!filter.matches(&c, Path::new("/songs")));
    }

    #[test]
    fn text_and_any_level_magic_words_work() {
        let filter = SongFilter::parse("alice lvl=9").expect("filter");
        let a = song(
            "SongA",
            "alice",
            false,
            vec![course("Oni", 9, 180.0, false)],
        );
        let b = song(
            "SongB",
            "alice",
            false,
            vec![course("Hard", 7, 180.0, false)],
        );

        assert!(filter.matches(&a, Path::new("/songs")));
        assert!(!filter.matches(&b, Path::new("/songs")));
    }

    #[test]
    fn invalid_magic_word_fails_strictly() {
        let err = SongFilter::parse("oni=0").expect_err("must reject");
        assert!(err.contains("out of range"));
    }
}
