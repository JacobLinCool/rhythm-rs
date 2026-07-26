# Player UI localization

`taiko-game` treats language selection as persisted player data and UI copy as
typed presentation data. `UiLanguage` is stored in the strict preferences
schema. A preferences file with an unknown schema or language is rejected
instead of being interpreted through a compatibility fallback.

All fixed player-facing copy is declared in `localization.rs`. Every `UiText`
key must provide English, Traditional Chinese, and Japanese text in the same
declaration. The generated language/key matches are exhaustive and contain no
fallback arm. Sentence-shaped dynamic copy uses typed `UiMessage` variants;
server messages, song metadata, file paths, hashes, and technical error details
remain source data and are not translated.

Layouts measure terminal display columns rather than UTF-8 bytes. Truncation is
grapheme-aware, preserves valid Unicode, and reserves the display width of its
ellipsis. Minimum supported layouts are tested at 80×24, local play at 80×27,
and the normal 120×36 viewport for both CJK languages.
