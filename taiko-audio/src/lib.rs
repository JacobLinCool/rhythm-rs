//! Bounded, fail-closed decoding for static gameplay audio.
//!
//! The client and resource server deliberately use this one decoder and one
//! limit set. The server validates without retaining decoded samples, while a
//! client may retain the same validated samples for playback.

use std::fs::File;
use std::io::{Cursor, Read, Seek, SeekFrom};
use std::mem::size_of;
use std::path::Path;
use std::sync::Arc;

use kira::sound::static_sound::{StaticSoundData, StaticSoundSettings};
use kira::Frame;
use symphonia::core::audio::{AudioBuffer, AudioBufferRef, Channels, Layout, Signal};
use symphonia::core::conv::{FromSample, IntoSample};
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::io::{MediaSource, MediaSourceStream};
use symphonia::core::probe::Hint;
use symphonia::core::sample::Sample;

/// Version of the bounded, fail-closed audio decoding semantics.
pub const AUDIO_DECODER_SEMANTICS_VERSION: u32 = 1;
/// Canonical descriptor hashed by [`AUDIO_DECODER_SEMANTICS_SHA256`].
///
/// This descriptor is part of multiplayer resource identity. Any change to
/// decoder dependencies, accepted stream shape, limits, sample conversion, or
/// cooperative cancellation boundaries must update the descriptor, version,
/// and pinned digest together.
pub const AUDIO_DECODER_SEMANTICS_DESCRIPTOR: &str = concat!(
    "taiko-audio-decoder/v1\n",
    "engine=symphonia@0.5.5;default-features=false;",
    "enabled-features=flac,mp3,ogg,pcm,vorbis,wav;",
    "options=media-source-stream-default+format-default+metadata-default+decoder-default\n",
    "track=default-track-only;decode=every-default-track-packet-through-eof;",
    "non-default-track-packets=ignored\n",
    "encoded-input=nonempty;encoded-bytes<=268435456\n",
    "stream=sample-rate-hz:8000..=96000+constant;",
    "channel-layout:mono-or-stereo+constant;duration-seconds<=900\n",
    "decoded-output=interleaved-stereo-f32;decoded-bytes<=268435456;",
    "decoded-frames<=33554432\n",
    "sample-conversion=symphonia-0.5.5-IntoSample-f32;",
    "samples=finite-only;mono=duplicate-to-left+right;",
    "stereo=channel-0-to-left+channel-1-to-right\n",
    "cancellation=cooperative-packet-boundary;",
    "checks=before-probe+after-track-validation+before-each-demux-packet-read",
    "+after-each-default-track-packet-decode;",
    "no-mid-demux-or-mid-decode-interruption;error=cancelled\n",
    "failure-policy=reject-empty+missing-default-track+unknown-rate",
    "+unsupported-layout+rate-or-layout-change+limit-exceeded",
    "+nonfinite+allocation-failure+io-error+codec-error\n",
);
/// SHA-256 of [`AUDIO_DECODER_SEMANTICS_DESCRIPTOR`].
pub const AUDIO_DECODER_SEMANTICS_SHA256: &str =
    "6a71c64d00a05434402767950a7250ffa5d1a83ccc64ddd81e30575cd83e5eb5";

/// Encoded audio is bounded before a decoder is allowed to inspect it.
pub const MAX_ENCODED_AUDIO_BYTES: u64 = 256 * 1024 * 1024;
/// Gameplay audio must fit within fifteen minutes.
pub const MAX_AUDIO_DURATION_SECONDS: u64 = 15 * 60;
/// Rates below telephone-band audio are not supported gameplay assets.
pub const MIN_AUDIO_SAMPLE_RATE: u32 = 8_000;
/// Higher rates multiply decode memory and CPU without helping terminal play.
pub const MAX_AUDIO_SAMPLE_RATE: u32 = 96_000;
/// One decoded stereo frame is two `f32` samples.
pub const MAX_DECODED_AUDIO_BYTES: usize = 256 * 1024 * 1024;
pub const MAX_DECODED_AUDIO_FRAMES: usize = MAX_DECODED_AUDIO_BYTES / size_of::<Frame>();

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioMetadata {
    pub sample_rate: u32,
    pub channels: u8,
    pub frames: usize,
}

impl AudioMetadata {
    pub fn duration_seconds(self) -> f64 {
        self.frames as f64 / f64::from(self.sample_rate)
    }
}

/// Fully decoded, immutable stereo samples ready to hand to Kira.
#[derive(Debug, Clone, PartialEq)]
pub struct DecodedAudio {
    metadata: AudioMetadata,
    frames: Arc<[Frame]>,
}

impl DecodedAudio {
    pub fn metadata(&self) -> AudioMetadata {
        self.metadata
    }

    pub fn into_static_sound_data(self) -> StaticSoundData {
        StaticSoundData {
            sample_rate: self.metadata.sample_rate,
            frames: self.frames,
            settings: StaticSoundSettings::default(),
            slice: None,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum AudioDecodeError {
    #[error("audio input is empty")]
    EmptyInput,
    #[error("encoded audio is {actual} bytes; maximum is {maximum}")]
    EncodedTooLarge { actual: u64, maximum: u64 },
    #[error("audio has no default track")]
    NoDefaultTrack,
    #[error("audio sample rate is unknown")]
    UnknownSampleRate,
    #[error(
        "audio sample rate {actual} Hz is outside the supported {minimum}..={maximum} Hz range"
    )]
    SampleRateOutOfRange {
        actual: u32,
        minimum: u32,
        maximum: u32,
    },
    #[error(
        "audio has unsupported channel layout {actual}; only mono (front-left) and stereo \
         (front-left + front-right) are accepted"
    )]
    UnsupportedChannelLayout { actual: Channels },
    #[error("audio channel layout changes within the stream")]
    ChannelLayoutChanged,
    #[error("audio sample rate changes within the stream")]
    SampleRateChanged,
    #[error("decoded audio exceeds {MAX_AUDIO_DURATION_SECONDS} seconds")]
    DurationTooLong,
    #[error("decoded audio exceeds {MAX_DECODED_AUDIO_BYTES} bytes")]
    DecodedTooLarge,
    #[error("decoded audio contains a non-finite sample")]
    NonFiniteSample,
    #[error("audio decoding was cancelled")]
    Cancelled,
    #[error("not enough memory to decode bounded audio")]
    AllocationFailed,
    #[error("failed to read audio: {0}")]
    Io(#[from] std::io::Error),
    #[error("unsupported or corrupt audio: {0}")]
    Codec(String),
}

pub fn decode_file(path: impl AsRef<Path>) -> Result<DecodedAudio, AudioDecodeError> {
    decode_file_cancellable(path, &|| false)
}

pub fn decode_file_cancellable(
    path: impl AsRef<Path>,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<DecodedAudio, AudioDecodeError> {
    let file = open_bounded_file(path.as_ref())?;
    match decode_media_source(file, DecodeMode::Retain, is_cancelled)? {
        DecodeOutput::Decoded(audio) => Ok(audio),
        DecodeOutput::Validated(_) => unreachable!("retain mode returns decoded audio"),
    }
}

pub fn decode_bytes(bytes: Arc<[u8]>) -> Result<DecodedAudio, AudioDecodeError> {
    decode_bytes_cancellable(bytes, &|| false)
}

pub fn decode_bytes_cancellable(
    bytes: Arc<[u8]>,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<DecodedAudio, AudioDecodeError> {
    ensure_encoded_size(u64::try_from(bytes.len()).unwrap_or(u64::MAX))?;
    match decode_media_source(Cursor::new(bytes), DecodeMode::Retain, is_cancelled)? {
        DecodeOutput::Decoded(audio) => Ok(audio),
        DecodeOutput::Validated(_) => unreachable!("retain mode returns decoded audio"),
    }
}

pub fn validate_file(path: impl AsRef<Path>) -> Result<AudioMetadata, AudioDecodeError> {
    let file = open_bounded_file(path.as_ref())?;
    match decode_media_source(file, DecodeMode::ValidateOnly, &|| false)? {
        DecodeOutput::Validated(metadata) => Ok(metadata),
        DecodeOutput::Decoded(_) => unreachable!("validation mode returns metadata"),
    }
}

/// Validates one owned, immutable encoded-audio snapshot.
///
/// Callers that also derive a content identity should hash `bytes.as_ref()`
/// before moving the same value into this function. This prevents a pathname
/// replacement between separate validation and hashing opens.
pub fn validate_bytes<T>(bytes: T) -> Result<AudioMetadata, AudioDecodeError>
where
    T: AsRef<[u8]> + Send + Sync + 'static,
{
    ensure_encoded_size(u64::try_from(bytes.as_ref().len()).unwrap_or(u64::MAX))?;
    match decode_media_source(Cursor::new(bytes), DecodeMode::ValidateOnly, &|| false)? {
        DecodeOutput::Validated(metadata) => Ok(metadata),
        DecodeOutput::Decoded(_) => unreachable!("validation mode returns metadata"),
    }
}

fn open_bounded_file(path: &Path) -> Result<BoundedFile, AudioDecodeError> {
    let file = File::open(path)?;
    let len = file.metadata()?.len();
    ensure_encoded_size(len)?;
    Ok(BoundedFile {
        file,
        len,
        position: 0,
    })
}

struct BoundedFile {
    file: File,
    len: u64,
    position: u64,
}

impl Read for BoundedFile {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let remaining = self.len.saturating_sub(self.position);
        let bounded_len = buffer
            .len()
            .min(usize::try_from(remaining).unwrap_or(usize::MAX));
        if bounded_len == 0 {
            return Ok(0);
        }
        let read = self.file.read(&mut buffer[..bounded_len])?;
        self.position = self
            .position
            .checked_add(read as u64)
            .expect("a bounded read position fits u64");
        Ok(read)
    }
}

impl Seek for BoundedFile {
    fn seek(&mut self, position: SeekFrom) -> std::io::Result<u64> {
        let target = match position {
            SeekFrom::Start(offset) => i128::from(offset),
            SeekFrom::Current(offset) => i128::from(self.position) + i128::from(offset),
            SeekFrom::End(offset) => i128::from(self.len) + i128::from(offset),
        };
        if !(0..=i128::from(self.len)).contains(&target) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "audio decoder seek exceeds the bounded input",
            ));
        }
        let target = u64::try_from(target).expect("validated seek target fits u64");
        self.position = self.file.seek(SeekFrom::Start(target))?;
        Ok(self.position)
    }
}

impl MediaSource for BoundedFile {
    fn is_seekable(&self) -> bool {
        true
    }

    fn byte_len(&self) -> Option<u64> {
        Some(self.len)
    }
}

fn ensure_encoded_size(len: u64) -> Result<(), AudioDecodeError> {
    if len == 0 {
        return Err(AudioDecodeError::EmptyInput);
    }
    if len > MAX_ENCODED_AUDIO_BYTES {
        return Err(AudioDecodeError::EncodedTooLarge {
            actual: len,
            maximum: MAX_ENCODED_AUDIO_BYTES,
        });
    }
    Ok(())
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum DecodeMode {
    Retain,
    ValidateOnly,
}

enum DecodeOutput {
    Decoded(DecodedAudio),
    Validated(AudioMetadata),
}

fn decode_media_source(
    media_source: impl MediaSource + 'static,
    mode: DecodeMode,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<DecodeOutput, AudioDecodeError> {
    ensure_not_cancelled(is_cancelled)?;
    let stream = MediaSourceStream::new(Box::new(media_source), Default::default());
    let mut probed = symphonia::default::get_probe()
        .format(
            &Hint::new(),
            stream,
            &Default::default(),
            &Default::default(),
        )
        .map_err(codec_error)?;
    let track = probed
        .format
        .default_track()
        .ok_or(AudioDecodeError::NoDefaultTrack)?;
    let track_id = track.id;
    let codec_params = track.codec_params.clone();
    let sample_rate = codec_params
        .sample_rate
        .ok_or(AudioDecodeError::UnknownSampleRate)?;
    validate_sample_rate(sample_rate)?;
    if let Some(channels) = codec_params.channels {
        validate_channel_layout(channels)?;
    }
    if let Some(frames) = codec_params.n_frames {
        validate_total_frames(frames, sample_rate)?;
    }
    ensure_not_cancelled(is_cancelled)?;

    let mut decoder = symphonia::default::get_codecs()
        .make(&codec_params, &Default::default())
        .map_err(codec_error)?;
    let mut total_frames = 0_usize;
    let mut observed_layout = None;
    let mut retained = (mode == DecodeMode::Retain).then(Vec::<Frame>::new);

    loop {
        ensure_not_cancelled(is_cancelled)?;
        let packet = match probed.format.next_packet() {
            Ok(packet) => packet,
            Err(SymphoniaError::IoError(error))
                if error.kind() == std::io::ErrorKind::UnexpectedEof =>
            {
                break;
            }
            Err(error) => return Err(codec_error(error)),
        };
        if packet.track_id() != track_id {
            continue;
        }

        let buffer = decoder.decode(&packet).map_err(codec_error)?;
        ensure_not_cancelled(is_cancelled)?;
        let spec = *buffer.spec();
        if spec.rate != sample_rate {
            return Err(AudioDecodeError::SampleRateChanged);
        }
        let layout = spec.channels;
        let channels = validate_channel_layout(layout)?;
        if observed_layout.is_some_and(|observed| observed != layout) {
            return Err(AudioDecodeError::ChannelLayoutChanged);
        }
        observed_layout = Some(layout);

        total_frames = total_frames
            .checked_add(buffer.frames())
            .ok_or(AudioDecodeError::DecodedTooLarge)?;
        validate_total_frames(total_frames as u64, sample_rate)?;
        append_validated_frames(buffer, channels, retained.as_mut())?;
    }

    if total_frames == 0 {
        return Err(AudioDecodeError::EmptyInput);
    }
    let channels = observed_layout
        .map(Channels::count)
        .ok_or(AudioDecodeError::EmptyInput)?;
    let metadata = AudioMetadata {
        sample_rate,
        channels: u8::try_from(channels)
            .expect("validated mono or stereo channel count always fits in u8"),
        frames: total_frames,
    };

    match retained {
        Some(frames) => Ok(DecodeOutput::Decoded(DecodedAudio {
            metadata,
            frames: frames.into(),
        })),
        None => Ok(DecodeOutput::Validated(metadata)),
    }
}

fn ensure_not_cancelled(is_cancelled: &dyn Fn() -> bool) -> Result<(), AudioDecodeError> {
    if is_cancelled() {
        Err(AudioDecodeError::Cancelled)
    } else {
        Ok(())
    }
}

fn validate_sample_rate(sample_rate: u32) -> Result<(), AudioDecodeError> {
    if !(MIN_AUDIO_SAMPLE_RATE..=MAX_AUDIO_SAMPLE_RATE).contains(&sample_rate) {
        return Err(AudioDecodeError::SampleRateOutOfRange {
            actual: sample_rate,
            minimum: MIN_AUDIO_SAMPLE_RATE,
            maximum: MAX_AUDIO_SAMPLE_RATE,
        });
    }
    Ok(())
}

fn validate_channel_layout(layout: Channels) -> Result<usize, AudioDecodeError> {
    if layout == Layout::Mono.into_channels() {
        Ok(1)
    } else if layout == Layout::Stereo.into_channels() {
        Ok(2)
    } else {
        Err(AudioDecodeError::UnsupportedChannelLayout { actual: layout })
    }
}

fn validate_total_frames(frames: u64, sample_rate: u32) -> Result<(), AudioDecodeError> {
    let duration_limit = u64::from(sample_rate)
        .checked_mul(MAX_AUDIO_DURATION_SECONDS)
        .expect("bounded sample rate and duration fit u64");
    if frames > duration_limit {
        return Err(AudioDecodeError::DurationTooLong);
    }
    if frames > MAX_DECODED_AUDIO_FRAMES as u64 {
        return Err(AudioDecodeError::DecodedTooLarge);
    }
    Ok(())
}

fn append_validated_frames(
    buffer: AudioBufferRef<'_>,
    channels: usize,
    retained: Option<&mut Vec<Frame>>,
) -> Result<(), AudioDecodeError> {
    match buffer {
        AudioBufferRef::U8(buffer) => append_typed_frames(&buffer, channels, retained),
        AudioBufferRef::U16(buffer) => append_typed_frames(&buffer, channels, retained),
        AudioBufferRef::U24(buffer) => append_typed_frames(&buffer, channels, retained),
        AudioBufferRef::U32(buffer) => append_typed_frames(&buffer, channels, retained),
        AudioBufferRef::S8(buffer) => append_typed_frames(&buffer, channels, retained),
        AudioBufferRef::S16(buffer) => append_typed_frames(&buffer, channels, retained),
        AudioBufferRef::S24(buffer) => append_typed_frames(&buffer, channels, retained),
        AudioBufferRef::S32(buffer) => append_typed_frames(&buffer, channels, retained),
        AudioBufferRef::F32(buffer) => append_typed_frames(&buffer, channels, retained),
        AudioBufferRef::F64(buffer) => append_typed_frames(&buffer, channels, retained),
    }
}

fn append_typed_frames<S>(
    buffer: &AudioBuffer<S>,
    channels: usize,
    mut retained: Option<&mut Vec<Frame>>,
) -> Result<(), AudioDecodeError>
where
    S: Sample,
    f32: FromSample<S>,
{
    if let Some(output) = retained.as_mut() {
        output
            .try_reserve(buffer.frames())
            .map_err(|_| AudioDecodeError::AllocationFailed)?;
    }
    match channels {
        1 => {
            for sample in buffer.chan(0) {
                let sample: f32 = (*sample).into_sample();
                if !sample.is_finite() {
                    return Err(AudioDecodeError::NonFiniteSample);
                }
                if let Some(output) = retained.as_mut() {
                    output.push(Frame::from_mono(sample));
                }
            }
        }
        2 => {
            for (left, right) in buffer.chan(0).iter().zip(buffer.chan(1)) {
                let left: f32 = (*left).into_sample();
                let right: f32 = (*right).into_sample();
                if !left.is_finite() || !right.is_finite() {
                    return Err(AudioDecodeError::NonFiniteSample);
                }
                if let Some(output) = retained.as_mut() {
                    output.push(Frame::new(left, right));
                }
            }
        }
        _ => unreachable!("channel count was validated before conversion"),
    }
    Ok(())
}

fn codec_error(error: SymphoniaError) -> AudioDecodeError {
    AudioDecodeError::Codec(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};

    #[test]
    fn decoder_semantics_fingerprint_is_pinned_to_fail_closed_contract() {
        assert_eq!(AUDIO_DECODER_SEMANTICS_VERSION, 1);
        assert_eq!(MAX_ENCODED_AUDIO_BYTES, 268_435_456);
        assert_eq!(MAX_DECODED_AUDIO_BYTES, 268_435_456);
        assert_eq!(MAX_DECODED_AUDIO_FRAMES, 33_554_432);
        assert_eq!(MAX_AUDIO_DURATION_SECONDS, 900);
        assert_eq!(MIN_AUDIO_SAMPLE_RATE, 8_000);
        assert_eq!(MAX_AUDIO_SAMPLE_RATE, 96_000);
        assert_eq!(
            format!(
                "{:x}",
                Sha256::digest(AUDIO_DECODER_SEMANTICS_DESCRIPTOR.as_bytes())
            ),
            AUDIO_DECODER_SEMANTICS_SHA256
        );
    }

    fn pcm_wav(channels: u16, sample_rate: u32, frames: u32) -> Arc<[u8]> {
        let bytes_per_sample = 2_u16;
        let block_align = channels
            .checked_mul(bytes_per_sample)
            .expect("test channel count fits");
        let data_len = frames
            .checked_mul(u32::from(block_align))
            .expect("test WAV size fits");
        let riff_len = 36_u32.checked_add(data_len).expect("test WAV size fits");
        let mut bytes = Vec::with_capacity(44 + data_len as usize);
        bytes.extend_from_slice(b"RIFF");
        bytes.extend_from_slice(&riff_len.to_le_bytes());
        bytes.extend_from_slice(b"WAVEfmt ");
        bytes.extend_from_slice(&16_u32.to_le_bytes());
        bytes.extend_from_slice(&1_u16.to_le_bytes());
        bytes.extend_from_slice(&channels.to_le_bytes());
        bytes.extend_from_slice(&sample_rate.to_le_bytes());
        bytes.extend_from_slice(
            &sample_rate
                .checked_mul(u32::from(block_align))
                .expect("test byte rate fits")
                .to_le_bytes(),
        );
        bytes.extend_from_slice(&block_align.to_le_bytes());
        bytes.extend_from_slice(&16_u16.to_le_bytes());
        bytes.extend_from_slice(b"data");
        bytes.extend_from_slice(&data_len.to_le_bytes());
        bytes.resize(44 + data_len as usize, 0);
        bytes.into()
    }

    #[test]
    fn valid_mono_audio_decodes_to_immutable_stereo_frames() {
        let decoded = decode_bytes(pcm_wav(1, 44_100, 441)).expect("valid WAV");
        assert_eq!(
            decoded.metadata(),
            AudioMetadata {
                sample_rate: 44_100,
                channels: 1,
                frames: 441,
            }
        );
        let static_data = decoded.into_static_sound_data();
        assert_eq!(static_data.frames.len(), 441);
        assert!(static_data
            .frames
            .iter()
            .all(|frame| frame.left == frame.right));
    }

    #[test]
    fn corrupt_audio_fails_closed() {
        let error = decode_bytes(Arc::from(&b"not an audio stream"[..]))
            .expect_err("corrupt data must fail");
        assert!(matches!(error, AudioDecodeError::Codec(_)));
    }

    #[test]
    fn owned_audio_snapshot_validation_uses_the_supplied_bytes() {
        let metadata = validate_bytes(pcm_wav(1, 44_100, 441))
            .expect("the immutable encoded snapshot is valid");
        assert_eq!(
            metadata,
            AudioMetadata {
                sample_rate: 44_100,
                channels: 1,
                frames: 441,
            }
        );

        let error = validate_bytes(b"not an audio stream".to_vec())
            .expect_err("corrupt snapshot must fail closed");
        assert!(matches!(error, AudioDecodeError::Codec(_)));
    }

    #[test]
    fn unsupported_channel_layout_fails_closed() {
        let error =
            decode_bytes(pcm_wav(3, 44_100, 16)).expect_err("three-channel audio must fail");
        assert!(matches!(
            error,
            AudioDecodeError::UnsupportedChannelLayout { actual }
                if actual.count() == 3
        ));
    }

    #[test]
    fn nonstandard_two_channel_mask_is_not_treated_as_stereo() {
        let layout = Channels::LFE1 | Channels::REAR_LEFT;
        let error =
            validate_channel_layout(layout).expect_err("two non-stereo channels must fail closed");
        assert!(matches!(
            &error,
            AudioDecodeError::UnsupportedChannelLayout { actual }
                if *actual == layout
        ));
        assert!(error.to_string().contains(&layout.to_string()));
    }

    #[test]
    fn out_of_range_sample_rate_fails_during_real_decode() {
        let error = decode_bytes(pcm_wav(1, MAX_AUDIO_SAMPLE_RATE + 1, 16))
            .expect_err("high-rate audio must fail before retaining samples");
        assert!(matches!(
            error,
            AudioDecodeError::SampleRateOutOfRange { .. }
        ));
    }

    #[test]
    fn shape_limits_reject_high_rate_long_and_expansive_audio() {
        assert!(matches!(
            validate_sample_rate(MAX_AUDIO_SAMPLE_RATE + 1),
            Err(AudioDecodeError::SampleRateOutOfRange { .. })
        ));
        assert!(matches!(
            validate_sample_rate(MIN_AUDIO_SAMPLE_RATE - 1),
            Err(AudioDecodeError::SampleRateOutOfRange { .. })
        ));
        assert!(matches!(
            validate_total_frames(
                u64::from(MAX_AUDIO_SAMPLE_RATE) * MAX_AUDIO_DURATION_SECONDS + 1,
                MAX_AUDIO_SAMPLE_RATE
            ),
            Err(AudioDecodeError::DurationTooLong)
        ));
        assert!(matches!(
            validate_total_frames(MAX_DECODED_AUDIO_FRAMES as u64 + 1, 44_100),
            Err(AudioDecodeError::DecodedTooLarge)
        ));
    }

    #[test]
    fn cancellation_stops_decode_before_retaining_samples() {
        let error = decode_bytes_cancellable(pcm_wav(1, 44_100, 441), &|| true)
            .expect_err("cancelled decode must stop");
        assert!(matches!(error, AudioDecodeError::Cancelled));
    }
}
