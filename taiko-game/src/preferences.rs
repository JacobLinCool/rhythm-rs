use std::collections::{BTreeMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{anyhow, bail, Context, Result};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use taiko_multiplayer_protocol::MAX_DISPLAY_NAME_BYTES;

const PREFERENCES_SCHEMA_VERSION: u32 = 3;
const MIN_CALIBRATION_OFFSET_MS: i32 = -500;
const MAX_CALIBRATION_OFFSET_MS: i32 = 500;
const MIN_SCROLL_SPEED: f32 = 0.5;
const MAX_SCROLL_SPEED: f32 = 4.0;
const MAX_PERSONAL_BESTS: usize = 4_096;
pub(crate) const MAX_STORED_QUERY_BYTES: usize = 256;

static NEXT_TEMP_FILE_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum UiLanguage {
    #[default]
    English,
    TraditionalChinese,
    Japanese,
}

impl UiLanguage {
    #[cfg(test)]
    pub(crate) const ALL: [Self; 3] = [Self::English, Self::TraditionalChinese, Self::Japanese];

    pub(crate) fn cycle(self, delta: i32) -> Self {
        if delta > 0 {
            match self {
                Self::English => Self::TraditionalChinese,
                Self::TraditionalChinese => Self::Japanese,
                Self::Japanese => Self::English,
            }
        } else if delta < 0 {
            match self {
                Self::English => Self::Japanese,
                Self::TraditionalChinese => Self::English,
                Self::Japanese => Self::TraditionalChinese,
            }
        } else {
            self
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum BindingSlot {
    LeftKat,
    LeftDon,
    RightDon,
    RightKat,
}

impl BindingSlot {
    pub(crate) const ALL: [Self; 4] =
        [Self::LeftKat, Self::LeftDon, Self::RightDon, Self::RightKat];
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DrumBindings {
    pub(crate) left_kat: char,
    pub(crate) left_don: char,
    pub(crate) right_don: char,
    pub(crate) right_kat: char,
}

impl DrumBindings {
    pub(crate) const fn player_one_default() -> Self {
        Self {
            left_kat: 'a',
            left_don: 's',
            right_don: 'd',
            right_kat: 'f',
        }
    }

    pub(crate) const fn player_two_default() -> Self {
        Self {
            left_kat: 'j',
            left_don: 'k',
            right_don: 'l',
            right_kat: ';',
        }
    }

    pub(crate) const fn key(self, slot: BindingSlot) -> char {
        match slot {
            BindingSlot::LeftKat => self.left_kat,
            BindingSlot::LeftDon => self.left_don,
            BindingSlot::RightDon => self.right_don,
            BindingSlot::RightKat => self.right_kat,
        }
    }

    pub(crate) fn set_key(&mut self, slot: BindingSlot, key: char) {
        match slot {
            BindingSlot::LeftKat => self.left_kat = key,
            BindingSlot::LeftDon => self.left_don = key,
            BindingSlot::RightDon => self.right_don = key,
            BindingSlot::RightKat => self.right_kat = key,
        }
    }

    pub(crate) fn slot_for_key(self, key: char) -> Option<BindingSlot> {
        let key = key.to_ascii_lowercase();
        BindingSlot::ALL
            .into_iter()
            .find(|slot| self.key(*slot).to_ascii_lowercase() == key)
    }

    fn keys(self) -> [char; 4] {
        [self.left_kat, self.left_don, self.right_don, self.right_kat]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "mode",
    content = "speed",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub(crate) enum StoredScrollSpeed {
    Manual(f32),
    VelocitySync,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PlayerPreferences {
    schema_version: u32,
    pub(crate) ui_language: UiLanguage,
    pub(crate) song_volume: u8,
    pub(crate) se_volume: u8,
    pub(crate) calibration_offset_ms: i32,
    pub(crate) scroll_speed: StoredScrollSpeed,
    pub(crate) player_name: String,
    pub(crate) demo_enabled: bool,
    pub(crate) player_one: DrumBindings,
    pub(crate) player_two: DrumBindings,
    pub(crate) personal_bests: BTreeMap<String, PersonalBest>,
    pub(crate) last_mode: Option<StoredGameMode>,
    pub(crate) recent_song: Option<RecentSongSelection>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum StoredGameMode {
    SinglePlayer,
    LocalTwoPlayer,
    OnlineMultiplayer,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RecentSongSelection {
    pub(crate) song_identity: String,
    pub(crate) query: String,
    pub(crate) course_identity: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PersonalBest {
    pub(crate) score: u32,
    pub(crate) accuracy_ppm: u32,
    pub(crate) cleared: bool,
    pub(crate) full_combo: bool,
}

impl Default for PlayerPreferences {
    fn default() -> Self {
        Self {
            schema_version: PREFERENCES_SCHEMA_VERSION,
            ui_language: UiLanguage::default(),
            song_volume: 100,
            se_volume: 100,
            calibration_offset_ms: 0,
            scroll_speed: StoredScrollSpeed::Manual(1.0),
            player_name: "Player".to_owned(),
            demo_enabled: true,
            player_one: DrumBindings::player_one_default(),
            player_two: DrumBindings::player_two_default(),
            personal_bests: BTreeMap::new(),
            last_mode: None,
            recent_song: None,
        }
    }
}

impl PlayerPreferences {
    pub(crate) fn validate(&self) -> Result<()> {
        if self.schema_version != PREFERENCES_SCHEMA_VERSION {
            bail!(
                "unsupported preferences schema version {}; expected {}",
                self.schema_version,
                PREFERENCES_SCHEMA_VERSION
            );
        }
        if self.song_volume > 100 || self.se_volume > 100 {
            bail!("volume preferences must be within 0..=100");
        }
        if !(MIN_CALIBRATION_OFFSET_MS..=MAX_CALIBRATION_OFFSET_MS)
            .contains(&self.calibration_offset_ms)
        {
            bail!(
                "calibration offset must be within {MIN_CALIBRATION_OFFSET_MS}..={MAX_CALIBRATION_OFFSET_MS} ms"
            );
        }
        if let StoredScrollSpeed::Manual(speed) = self.scroll_speed {
            if !speed.is_finite() || !(MIN_SCROLL_SPEED..=MAX_SCROLL_SPEED).contains(&speed) {
                bail!(
                    "manual scroll speed must be finite and within {MIN_SCROLL_SPEED}..={MAX_SCROLL_SPEED}"
                );
            }
        }

        let trimmed_name = self.player_name.trim();
        if trimmed_name.is_empty() {
            bail!("player name cannot be empty");
        }
        if trimmed_name.len() > MAX_DISPLAY_NAME_BYTES {
            bail!("player name exceeds {MAX_DISPLAY_NAME_BYTES} UTF-8 bytes");
        }
        if self.player_name.chars().any(char::is_control) {
            bail!("player name cannot contain control characters");
        }

        let mut keys = HashSet::with_capacity(8);
        for key in self
            .player_one
            .keys()
            .into_iter()
            .chain(self.player_two.keys())
        {
            validate_binding_key(key)?;
            if !keys.insert(key.to_ascii_lowercase()) {
                bail!("drum binding '{}' is assigned more than once", key);
            }
        }
        if self.personal_bests.len() > MAX_PERSONAL_BESTS {
            bail!("personal best history exceeds {MAX_PERSONAL_BESTS} charts");
        }
        for (chart_identity, best) in &self.personal_bests {
            if chart_identity.len() != 64
                || !chart_identity
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
            {
                bail!("personal best chart identities must be lowercase SHA-256 hex");
            }
            if best.accuracy_ppm > 1_000_000 {
                bail!("personal best accuracy must be within 0..=1,000,000 ppm");
            }
        }
        if let Some(recent) = &self.recent_song {
            validate_chart_identity(&recent.song_identity)
                .context("invalid recent song identity")?;
            validate_chart_identity(&recent.course_identity)
                .context("invalid recent course identity")?;
            if recent.query.len() > MAX_STORED_QUERY_BYTES
                || recent.query.chars().any(char::is_control)
            {
                bail!(
                    "recent song query must be control-free and at most {MAX_STORED_QUERY_BYTES} UTF-8 bytes"
                );
            }
        }
        Ok(())
    }

    pub(crate) fn record_personal_best(
        &mut self,
        chart_identity: &str,
        result: PersonalBest,
    ) -> Result<Option<PersonalBest>> {
        validate_chart_identity(chart_identity).context("invalid personal best chart identity")?;
        if result.accuracy_ppm > 1_000_000 {
            bail!("personal best accuracy must be within 0..=1,000,000 ppm");
        }
        if self.personal_bests.len() >= MAX_PERSONAL_BESTS
            && !self.personal_bests.contains_key(chart_identity)
        {
            bail!("personal best history is full ({MAX_PERSONAL_BESTS} charts)");
        }
        let previous = self.personal_bests.get(chart_identity).copied();
        let merged = previous.map_or(result, |previous| PersonalBest {
            score: previous.score.max(result.score),
            accuracy_ppm: previous.accuracy_ppm.max(result.accuracy_ppm),
            cleared: previous.cleared || result.cleared,
            full_combo: previous.full_combo || result.full_combo,
        });
        self.personal_bests
            .insert(chart_identity.to_owned(), merged);
        Ok(previous)
    }

    pub(crate) fn set_binding(
        &mut self,
        player_index: usize,
        slot: BindingSlot,
        key: char,
    ) -> Result<()> {
        validate_binding_key(key)?;
        let normalized = key.to_ascii_lowercase();
        let current = match player_index {
            0 => self.player_one.key(slot),
            1 => self.player_two.key(slot),
            _ => bail!("player index must be 0 or 1"),
        };
        if current.to_ascii_lowercase() == normalized {
            return Ok(());
        }

        let assigned_elsewhere = [self.player_one, self.player_two]
            .into_iter()
            .enumerate()
            .flat_map(|(candidate_player, bindings)| {
                BindingSlot::ALL
                    .into_iter()
                    .map(move |candidate_slot| (candidate_player, candidate_slot, bindings))
            })
            .any(|(candidate_player, candidate_slot, bindings)| {
                (candidate_player, candidate_slot) != (player_index, slot)
                    && bindings.key(candidate_slot).to_ascii_lowercase() == normalized
            });
        if assigned_elsewhere {
            bail!("key '{}' is already assigned", key);
        }

        let previous = self.clone();
        match player_index {
            0 => self.player_one.set_key(slot, normalized),
            1 => self.player_two.set_key(slot, normalized),
            _ => unreachable!("validated above"),
        }
        if let Err(error) = self.validate() {
            *self = previous;
            return Err(error);
        }
        Ok(())
    }
}

fn validate_chart_identity(identity: &str) -> Result<()> {
    if identity.len() != 64
        || !identity
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    {
        bail!("chart identity must be lowercase SHA-256 hex");
    }
    Ok(())
}

#[derive(Debug, Clone)]
pub(crate) struct PreferencesStore {
    path: PathBuf,
}

impl PreferencesStore {
    pub(crate) fn for_current_user() -> Result<Self> {
        let directories = ProjectDirs::from("com", "rhythm-rs", "taiko-game").ok_or_else(|| {
            anyhow!("the operating system did not provide an application config directory")
        })?;
        Ok(Self {
            path: directories.config_dir().join("preferences.json"),
        })
    }

    #[cfg(test)]
    fn at_path(path: PathBuf) -> Self {
        Self { path }
    }

    pub(crate) fn load(&self) -> Result<Option<PlayerPreferences>> {
        let bytes = match fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("failed to read {}", self.path.display()));
            }
        };
        let preferences: PlayerPreferences = serde_json::from_slice(&bytes)
            .with_context(|| format!("invalid preferences file {}", self.path.display()))?;
        preferences
            .validate()
            .with_context(|| format!("invalid preferences file {}", self.path.display()))?;
        Ok(Some(preferences))
    }

    pub(crate) fn save(&self, preferences: &PlayerPreferences) -> Result<()> {
        preferences.validate()?;
        let parent = self
            .path
            .parent()
            .ok_or_else(|| anyhow!("preferences path has no parent directory"))?;
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;

        let bytes = serde_json::to_vec_pretty(preferences)?;
        let mut temporary = TemporaryFile::create_next_to(&self.path)?;
        temporary
            .file
            .write_all(&bytes)
            .with_context(|| format!("failed to write {}", temporary.path.display()))?;
        temporary
            .file
            .write_all(b"\n")
            .with_context(|| format!("failed to finish {}", temporary.path.display()))?;
        temporary
            .file
            .sync_all()
            .with_context(|| format!("failed to sync {}", temporary.path.display()))?;
        fs::rename(&temporary.path, &self.path).with_context(|| {
            format!(
                "failed to atomically replace {} with {}",
                self.path.display(),
                temporary.path.display()
            )
        })?;
        temporary.persisted = true;
        sync_directory(parent)?;
        Ok(())
    }
}

struct TemporaryFile {
    path: PathBuf,
    file: File,
    persisted: bool,
}

impl TemporaryFile {
    fn create_next_to(destination: &Path) -> Result<Self> {
        let parent = destination
            .parent()
            .ok_or_else(|| anyhow!("preferences path has no parent directory"))?;
        let name = destination
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| anyhow!("preferences filename is not valid UTF-8"))?;

        for _ in 0..32 {
            let id = NEXT_TEMP_FILE_ID.fetch_add(1, Ordering::Relaxed);
            let path = parent.join(format!(".{name}.{}.{}.tmp", std::process::id(), id));
            match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(file) => {
                    return Ok(Self {
                        path,
                        file,
                        persisted: false,
                    });
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => {
                    return Err(error)
                        .with_context(|| format!("failed to create {}", path.display()));
                }
            }
        }
        bail!("could not allocate a unique preferences temporary file")
    }
}

impl Drop for TemporaryFile {
    fn drop(&mut self) {
        if !self.persisted {
            let _ = fs::remove_file(&self.path);
        }
    }
}

fn validate_binding_key(key: char) -> Result<()> {
    if !key.is_ascii() || key.is_ascii_control() || key.is_ascii_whitespace() {
        bail!("drum bindings must use one visible ASCII character");
    }
    if key.eq_ignore_ascii_case(&'p') {
        bail!("the P key is reserved for pause");
    }
    Ok(())
}

fn sync_directory(path: &Path) -> Result<()> {
    File::open(path)
        .with_context(|| format!("failed to open {} for sync", path.display()))?
        .sync_all()
        .with_context(|| format!("failed to sync {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temporary_path(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "taiko-preferences-{label}-{}-{}.json",
            std::process::id(),
            NEXT_TEMP_FILE_ID.fetch_add(1, Ordering::Relaxed)
        ))
    }

    fn valid_preferences_json() -> serde_json::Value {
        serde_json::to_value(PlayerPreferences::default()).expect("serialize valid preferences")
    }

    fn assert_preferences_json_rejected(
        label: &str,
        value: &serde_json::Value,
        expected_error: &str,
    ) {
        let path = temporary_path(label);
        let store = PreferencesStore::at_path(path.clone());
        fs::write(
            &path,
            serde_json::to_vec_pretty(value).expect("serialize fixture"),
        )
        .expect("write invalid fixture");

        let error = store
            .load()
            .expect_err("invalid preferences must be rejected");
        let error_chain = format!("{error:#}");
        fs::remove_file(path).expect("clean fixture");
        assert!(
            error_chain.contains(expected_error),
            "expected error containing {expected_error:?}, got {error_chain:?}"
        );
    }

    #[test]
    fn preferences_round_trip_through_atomic_store() {
        let path = temporary_path("round-trip");
        let store = PreferencesStore::at_path(path.clone());
        let preferences = PlayerPreferences {
            ui_language: UiLanguage::Japanese,
            song_volume: 42,
            calibration_offset_ms: -35,
            player_name: "Donko".to_owned(),
            ..PlayerPreferences::default()
        };

        store.save(&preferences).expect("save");
        assert_eq!(store.load().expect("load"), Some(preferences));
        fs::remove_file(path).expect("clean fixture");
    }

    #[test]
    fn language_cycle_is_typed_and_wraps_in_both_directions() {
        assert_eq!(UiLanguage::English.cycle(1), UiLanguage::TraditionalChinese);
        assert_eq!(
            UiLanguage::TraditionalChinese.cycle(1),
            UiLanguage::Japanese
        );
        assert_eq!(UiLanguage::Japanese.cycle(1), UiLanguage::English);
        assert_eq!(UiLanguage::English.cycle(-1), UiLanguage::Japanese);
        assert_eq!(
            UiLanguage::Japanese.cycle(-1),
            UiLanguage::TraditionalChinese
        );
        assert_eq!(
            UiLanguage::TraditionalChinese.cycle(-1),
            UiLanguage::English
        );
        assert_eq!(
            UiLanguage::TraditionalChinese.cycle(0),
            UiLanguage::TraditionalChinese
        );
    }

    #[test]
    fn missing_preferences_are_distinct_from_invalid_preferences() {
        let missing = PreferencesStore::at_path(temporary_path("missing"));
        assert_eq!(missing.load().expect("missing"), None);

        let invalid_path = temporary_path("invalid");
        fs::write(&invalid_path, br#"{"schema_version":999}"#).expect("write invalid fixture");
        let invalid = PreferencesStore::at_path(invalid_path.clone());
        assert!(invalid.load().is_err());
        fs::remove_file(invalid_path).expect("clean fixture");
    }

    #[test]
    fn persisted_preferences_require_ui_language() {
        let mut value = valid_preferences_json();
        value
            .as_object_mut()
            .expect("preferences serialize as an object")
            .remove("ui_language");

        assert_preferences_json_rejected(
            "missing-ui-language",
            &value,
            "missing field `ui_language`",
        );
    }

    #[test]
    fn persisted_preferences_reject_unknown_ui_language() {
        let mut value = valid_preferences_json();
        value["ui_language"] = serde_json::Value::String("klingon".to_owned());

        assert_preferences_json_rejected(
            "unknown-ui-language",
            &value,
            "unknown variant `klingon`",
        );
    }

    #[test]
    fn persisted_preferences_reject_previous_schema_version() {
        let mut value = valid_preferences_json();
        value["schema_version"] = serde_json::Value::from(2);

        assert_preferences_json_rejected(
            "previous-schema-version",
            &value,
            "unsupported preferences schema version 2; expected 3",
        );
    }

    #[test]
    fn persisted_preferences_reject_unknown_top_level_fields() {
        let mut value = valid_preferences_json();
        value
            .as_object_mut()
            .expect("preferences serialize as an object")
            .insert("future_field".to_owned(), serde_json::Value::Bool(true));

        assert_preferences_json_rejected(
            "unknown-top-level-field",
            &value,
            "unknown field `future_field`",
        );
    }

    #[test]
    fn persisted_preferences_reject_unknown_scroll_speed_fields() {
        let mut value = valid_preferences_json();
        value["scroll_speed"]
            .as_object_mut()
            .expect("scroll speed serializes as an object")
            .insert("future_field".to_owned(), serde_json::Value::Bool(true));

        assert_preferences_json_rejected(
            "unknown-scroll-speed-field",
            &value,
            "invalid value: string \"future_field\", expected \"mode\" or \"speed\"",
        );
    }

    #[test]
    fn duplicate_bindings_are_rejected_across_local_players() {
        let mut preferences = PlayerPreferences::default();
        preferences.player_two.left_kat = 'a';
        assert!(preferences.validate().is_err());
    }

    #[test]
    fn rebinding_rejects_conflicts_and_non_visible_keys() {
        let mut preferences = PlayerPreferences::default();
        assert!(preferences
            .set_binding(0, BindingSlot::LeftKat, 'k')
            .is_err());
        assert!(preferences
            .set_binding(0, BindingSlot::LeftKat, '\n')
            .is_err());
        assert!(preferences
            .set_binding(0, BindingSlot::LeftKat, 'P')
            .is_err());
    }
}
