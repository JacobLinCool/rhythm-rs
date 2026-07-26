use std::{fmt, marker::PhantomData, ops::Deref};

use serde::{
    de::{Error as _, IgnoredAny, SeqAccess, Visitor},
    Deserialize, Deserializer, Serialize,
};
use thiserror::Error;

use crate::{MAX_ROOM_CODE_BYTES, MAX_SECRET_TOKEN_BYTES};

const ROOM_CODE_ALPHABET: &str = "ABCDEFGHJKLMNPQRSTUVWXYZ23456789";

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ValueError {
    #[error("value cannot be empty")]
    Empty,
    #[error("value contains a control character")]
    ControlCharacter,
    #[error("value cannot start or end with whitespace")]
    SurroundingWhitespace,
    #[error("value exceeds {max} bytes (got {actual})")]
    TooLong { actual: usize, max: usize },
    #[error("value must be exactly {expected} bytes (got {actual})")]
    InvalidLength { actual: usize, expected: usize },
    #[error("value contains characters outside the required alphabet")]
    InvalidAlphabet,
    #[error("collection exceeds {max} items (got {actual})")]
    TooManyItems { actual: usize, max: usize },
    #[error("value exceeds maximum {max} (got {actual})")]
    OutOfRange { actual: u64, max: u64 },
}

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct BoundedString<const MAX: usize>(String);

impl<const MAX: usize> BoundedString<MAX> {
    pub fn new(value: impl Into<String>) -> Result<Self, ValueError> {
        let value = value.into();
        if value.is_empty() {
            return Err(ValueError::Empty);
        }
        if value.trim() != value {
            return Err(ValueError::SurroundingWhitespace);
        }
        if value.len() > MAX {
            return Err(ValueError::TooLong {
                actual: value.len(),
                max: MAX,
            });
        }
        if value.chars().any(char::is_control) {
            return Err(ValueError::ControlCharacter);
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_string(self) -> String {
        self.0
    }
}

impl<const MAX: usize> fmt::Debug for BoundedString<MAX> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl<const MAX: usize> fmt::Display for BoundedString<MAX> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl<const MAX: usize> AsRef<str> for BoundedString<MAX> {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl<'de, const MAX: usize> Deserialize<'de> for BoundedString<MAX> {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(D::Error::custom)
    }
}

/// UTF-8 text with a byte limit that deliberately permits the empty string.
///
/// This is reserved for optional presentation metadata. Identifiers, names,
/// build labels, and error messages use [`BoundedString`] instead.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct BoundedText<const MAX: usize>(String);

impl<const MAX: usize> BoundedText<MAX> {
    pub fn new(value: impl Into<String>) -> Result<Self, ValueError> {
        let value = value.into();
        if value.len() > MAX {
            return Err(ValueError::TooLong {
                actual: value.len(),
                max: MAX,
            });
        }
        if value.chars().any(char::is_control) {
            return Err(ValueError::ControlCharacter);
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_string(self) -> String {
        self.0
    }
}

impl<const MAX: usize> fmt::Debug for BoundedText<MAX> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl<const MAX: usize> fmt::Display for BoundedText<MAX> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl<const MAX: usize> AsRef<str> for BoundedText<MAX> {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl<'de, const MAX: usize> Deserialize<'de> for BoundedText<MAX> {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(D::Error::custom)
    }
}

/// A collection whose maximum length is enforced during construction and
/// deserialization, before it reaches room state or an engine queue.
#[derive(Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct BoundedVec<T, const MAX: usize>(Vec<T>);

impl<T, const MAX: usize> BoundedVec<T, MAX> {
    pub fn new(values: Vec<T>) -> Result<Self, ValueError> {
        if values.len() > MAX {
            return Err(ValueError::TooManyItems {
                actual: values.len(),
                max: MAX,
            });
        }
        Ok(Self(values))
    }

    pub fn as_slice(&self) -> &[T] {
        &self.0
    }

    pub fn into_vec(self) -> Vec<T> {
        self.0
    }
}

impl<T, const MAX: usize> Default for BoundedVec<T, MAX> {
    fn default() -> Self {
        Self(Vec::new())
    }
}

impl<T, const MAX: usize> fmt::Debug for BoundedVec<T, MAX>
where
    T: fmt::Debug,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl<T, const MAX: usize> Deref for BoundedVec<T, MAX> {
    type Target = [T];

    fn deref(&self) -> &Self::Target {
        self.as_slice()
    }
}

impl<T, const MAX: usize> TryFrom<Vec<T>> for BoundedVec<T, MAX> {
    type Error = ValueError;

    fn try_from(value: Vec<T>) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl<T, const MAX: usize> IntoIterator for BoundedVec<T, MAX> {
    type Item = T;
    type IntoIter = std::vec::IntoIter<T>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

impl<'a, T, const MAX: usize> IntoIterator for &'a BoundedVec<T, MAX> {
    type Item = &'a T;
    type IntoIter = std::slice::Iter<'a, T>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.iter()
    }
}

impl<'de, T, const MAX: usize> Deserialize<'de> for BoundedVec<T, MAX>
where
    T: Deserialize<'de>,
{
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_seq(BoundedVecVisitor::<T, MAX>(PhantomData))
    }
}

struct BoundedVecVisitor<T, const MAX: usize>(PhantomData<T>);

impl<'de, T, const MAX: usize> Visitor<'de> for BoundedVecVisitor<T, MAX>
where
    T: Deserialize<'de>,
{
    type Value = BoundedVec<T, MAX>;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "a sequence containing at most {MAX} elements")
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut values = Vec::with_capacity(sequence.size_hint().unwrap_or(0).min(MAX));
        while values.len() < MAX {
            let Some(value) = sequence.next_element()? else {
                return Ok(BoundedVec(values));
            };
            values.push(value);
        }
        if sequence.next_element::<IgnoredAny>()?.is_some() {
            return Err(A::Error::custom(format_args!(
                "collection exceeds {MAX} items"
            )));
        }
        Ok(BoundedVec(values))
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct ProgressMilli(u16);

impl ProgressMilli {
    pub const MAX: u16 = 1_000;

    pub fn new(value: u16) -> Result<Self, ValueError> {
        if value > Self::MAX {
            return Err(ValueError::OutOfRange {
                actual: u64::from(value),
                max: u64::from(Self::MAX),
            });
        }
        Ok(Self(value))
    }

    pub const fn get(self) -> u16 {
        self.0
    }
}

impl<'de> Deserialize<'de> for ProgressMilli {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = u16::deserialize(deserializer)?;
        Self::new(value).map_err(D::Error::custom)
    }
}

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct RoomCode(String);

impl RoomCode {
    pub fn parse(value: impl Into<String>) -> Result<Self, ValueError> {
        let value = value.into().to_ascii_uppercase();
        if value.is_empty() {
            return Err(ValueError::Empty);
        }
        if value.len() != MAX_ROOM_CODE_BYTES {
            return Err(ValueError::InvalidLength {
                actual: value.len(),
                expected: MAX_ROOM_CODE_BYTES,
            });
        }
        if !value
            .bytes()
            .all(|byte| ROOM_CODE_ALPHABET.as_bytes().contains(&byte))
        {
            return Err(ValueError::InvalidAlphabet);
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for RoomCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl fmt::Display for RoomCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl AsRef<str> for RoomCode {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl<'de> Deserialize<'de> for RoomCode {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(value).map_err(D::Error::custom)
    }
}

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct ContentHash(String);

impl ContentHash {
    pub fn parse(value: impl Into<String>) -> Result<Self, ValueError> {
        let value = value.into();
        if value.len() != 64 {
            return Err(ValueError::InvalidLength {
                actual: value.len(),
                expected: 64,
            });
        }
        if !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(ValueError::InvalidAlphabet);
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for ContentHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl fmt::Display for ContentHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl AsRef<str> for ContentHash {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl<'de> Deserialize<'de> for ContentHash {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(value).map_err(D::Error::custom)
    }
}

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct SongId(ContentHash);

impl SongId {
    pub fn parse(value: impl Into<String>) -> Result<Self, ValueError> {
        ContentHash::parse(value).map(Self)
    }

    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

impl fmt::Debug for SongId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl fmt::Display for SongId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl AsRef<str> for SongId {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl<'de> Deserialize<'de> for SongId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(value).map_err(D::Error::custom)
    }
}

macro_rules! secret_token {
    ($name:ident) => {
        #[derive(Clone, PartialEq, Eq, Hash, Serialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            pub fn parse(value: impl Into<String>) -> Result<Self, ValueError> {
                let value = value.into();
                if value.len() != MAX_SECRET_TOKEN_BYTES {
                    return Err(ValueError::InvalidLength {
                        actual: value.len(),
                        expected: MAX_SECRET_TOKEN_BYTES,
                    });
                }
                if !value
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
                {
                    return Err(ValueError::InvalidAlphabet);
                }
                Ok(Self(value))
            }

            pub fn expose(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(concat!(stringify!($name), "(REDACTED)"))
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                let value = String::deserialize(deserializer)?;
                Self::parse(value).map_err(D::Error::custom)
            }
        }
    };
}

secret_token!(ResumeToken);
secret_token!(InvitationToken);
secret_token!(ClockProbeToken);

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;

    thread_local! {
        static DESERIALIZED_ELEMENTS: Cell<usize> = const { Cell::new(0) };
    }

    struct CountedElement;

    impl<'de> Deserialize<'de> for CountedElement {
        fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
        where
            D: Deserializer<'de>,
        {
            u8::deserialize(deserializer)?;
            DESERIALIZED_ELEMENTS.with(|count| count.set(count.get() + 1));
            Ok(Self)
        }
    }

    #[test]
    fn bounded_string_rejects_oversize_and_control_text() {
        assert!(matches!(
            BoundedString::<4>::new("12345"),
            Err(ValueError::TooLong { .. })
        ));
        assert_eq!(
            BoundedString::<16>::new("a\nb"),
            Err(ValueError::ControlCharacter)
        );
        assert_eq!(
            BoundedString::<16>::new(" alice"),
            Err(ValueError::SurroundingWhitespace)
        );
        assert!(serde_json::from_str::<BoundedString<16>>(r#""""#).is_err());
        assert!(serde_json::from_str::<BoundedText<16>>(r#""""#).is_ok());
    }

    #[test]
    fn bounded_vec_rejects_oversize_during_deserialization() {
        assert!(serde_json::from_str::<BoundedVec<u8, 2>>("[1,2]").is_ok());
        assert!(serde_json::from_str::<BoundedVec<u8, 2>>("[1,2,3]").is_err());

        DESERIALIZED_ELEMENTS.with(|count| count.set(0));
        assert!(serde_json::from_str::<BoundedVec<CountedElement, 2>>("[1,2,3,4]").is_err());
        assert_eq!(
            DESERIALIZED_ELEMENTS.with(Cell::get),
            2,
            "the element type must not be materialized after the declared maximum"
        );
    }

    #[test]
    fn progress_is_bounded_at_the_wire_boundary() {
        assert_eq!(ProgressMilli::new(1_000).expect("valid").get(), 1_000);
        assert!(ProgressMilli::new(1_001).is_err());
        assert!(serde_json::from_str::<ProgressMilli>("1001").is_err());
    }

    #[test]
    fn ids_validate_at_deserialization_boundary() {
        assert!(serde_json::from_str::<RoomCode>(r#""ABCD""#).is_ok());
        assert!(serde_json::from_str::<RoomCode>(r#""A0CD""#).is_err());
        assert!(serde_json::from_str::<ContentHash>(&format!(r#""{}""#, "a".repeat(64))).is_ok());
        assert!(serde_json::from_str::<ContentHash>(&format!(r#""{}""#, "A".repeat(64))).is_err());
    }

    #[test]
    fn secret_debug_is_redacted() {
        let token = ResumeToken::parse("a".repeat(MAX_SECRET_TOKEN_BYTES)).expect("token");
        assert_eq!(format!("{token:?}"), "ResumeToken(REDACTED)");

        let probe =
            ClockProbeToken::parse("b".repeat(MAX_SECRET_TOKEN_BYTES)).expect("probe token");
        assert_eq!(format!("{probe:?}"), "ClockProbeToken(REDACTED)");
    }
}
