use std::{collections::BTreeMap, fmt::Debug};

use serde::{de::DeserializeOwned, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use taiko_multiplayer_protocol::*;

fn hash(byte: char) -> ContentHash {
    ContentHash::parse(byte.to_string().repeat(64)).expect("fixture hash must be valid")
}

fn song_id(byte: char) -> SongId {
    SongId::parse(byte.to_string().repeat(64)).expect("fixture song id must be valid")
}

fn resume_token() -> ResumeToken {
    ResumeToken::parse("1".repeat(MAX_SECRET_TOKEN_BYTES))
        .expect("fixture resume token must be valid")
}

fn invitation_token() -> InvitationToken {
    InvitationToken::parse("2".repeat(MAX_SECRET_TOKEN_BYTES))
        .expect("fixture invitation token must be valid")
}

fn clock_probe_token() -> ClockProbeToken {
    ClockProbeToken::parse("3".repeat(MAX_SECRET_TOKEN_BYTES))
        .expect("fixture clock probe token must be valid")
}

fn semantics() -> MatchSemantics {
    MatchSemantics {
        canonical_schema_version: 3,
        canonical_schema_digest: hash('8'),
        importer_semantics_version: 7,
        importer_semantics_digest: hash('9'),
        ruleset_version: 11,
        ruleset_digest: hash('c'),
        audio_decoder_semantics_version: 13,
        audio_decoder_semantics_digest: hash('f'),
    }
}

fn song_manifest() -> SongManifest {
    let mut manifest = SongManifest {
        song_id: song_id('7'),
        source_id: hash('a'),
        audio_id: Some(hash('b')),
        title: DisplayTitle::new("Contract Song").expect("fixture title"),
        subtitle: BoundedText::new("").expect("fixture subtitle"),
        artist: BoundedText::new("Protocol Artist").expect("fixture artist"),
        semantics: semantics(),
        courses: bounded_vec(vec![
            CourseManifest {
                course_id: CourseId(0),
                name: CourseName::new("Normal").expect("fixture course name"),
                level: Some(5),
                canonical_chart_hash: hash('d'),
            },
            CourseManifest {
                course_id: CourseId(1),
                name: CourseName::new("Oni").expect("fixture course name"),
                level: Some(9),
                canonical_chart_hash: hash('e'),
            },
        ]),
    };
    manifest.song_id = manifest.derive_song_id().expect("derived fixture song id");
    manifest
}

fn per_player_manifest() -> MatchManifest {
    MatchManifest {
        match_id: MatchId(42),
        song: song_manifest(),
        assignments: bounded_vec(vec![
            PlayerCourseAssignment {
                player_id: PlayerId(11),
                selection: PlayerSelection {
                    course_id: CourseId(0),
                },
                canonical_chart_hash: hash('d'),
            },
            PlayerCourseAssignment {
                player_id: PlayerId(22),
                selection: PlayerSelection {
                    course_id: CourseId(1),
                },
                canonical_chart_hash: hash('e'),
            },
        ]),
        countdown_ms: 3_000,
        input_lateness_ms: 150,
    }
}

fn score() -> ScoreSnapshot {
    ScoreSnapshot {
        score: 987_650,
        combo: 321,
        max_combo: 400,
        gauge_ppm: 875_000,
        pass_threshold_ppm: 800_000,
        great: 400,
        ok: 50,
        miss: 6,
        roll_hits: 77,
    }
}

fn final_result(player_id: PlayerId, course_id: CourseId, digest: char) -> FinalResult {
    FinalResult {
        player_id,
        course_id,
        score: score(),
        finish_tick: 12_345_678,
        passed: true,
        replay_digest: hash(digest),
        dnf: false,
    }
}

fn preparation_proof(course_hash: char) -> PreparationProof {
    PreparationProof {
        source_id: hash('a'),
        canonical_chart_hash: hash(course_hash),
        audio_id: Some(hash('b')),
        semantics: semantics(),
    }
}

fn room_snapshot(stage: RoomStage) -> RoomSnapshot {
    RoomSnapshot {
        room_code: RoomCode::parse("J7K9").expect("fixture room code"),
        revision: RoomRevision(17),
        server_now_us: 8_000_000,
        leader_player_id: PlayerId(11),
        players: bounded_vec(vec![
            PlayerSnapshot {
                player_id: PlayerId(11),
                name: DisplayName::new("Alice").expect("fixture display name"),
                is_leader: true,
                connection: PlayerConnection::Online,
                preparation: PlayerPreparation::Ready {
                    selection: PlayerSelection {
                        course_id: CourseId(0),
                    },
                },
                last_acked_input_seq: Some(InputSeq(40)),
            },
            PlayerSnapshot {
                player_id: PlayerId(22),
                name: DisplayName::new("Bob").expect("fixture display name"),
                is_leader: false,
                connection: PlayerConnection::Reconnecting {
                    grace_deadline_server_us: 8_500_000,
                },
                preparation: PlayerPreparation::Ready {
                    selection: PlayerSelection {
                        course_id: CourseId(1),
                    },
                },
                last_acked_input_seq: Some(InputSeq(35)),
            },
        ]),
        spectators: bounded_vec(vec![SpectatorSnapshot {
            spectator_id: SpectatorId(33),
            name: DisplayName::new("Eve").expect("fixture display name"),
            connection: PlayerConnection::Online,
        }]),
        stage,
    }
}

fn assert_round_trip<T>(value: &T)
where
    T: Serialize + DeserializeOwned + PartialEq + Debug,
{
    let encoded = serde_json::to_vec(value).expect("wire value must serialize");
    let decoded: T = serde_json::from_slice(&encoded).expect("wire value must deserialize");
    assert_eq!(&decoded, value);
    assert_eq!(
        serde_json::to_vec(&decoded).expect("decoded wire value must reserialize"),
        encoded,
        "serialization must be stable after a decode"
    );
}

fn bounded_vec<T, const MAX: usize>(values: Vec<T>) -> BoundedVec<T, MAX> {
    BoundedVec::new(values).expect("fixture collection must fit its wire bound")
}

#[test]
fn schema_fingerprint_is_pinned_well_formed_and_sent_by_hello() {
    const PINNED_SCHEMA_SHA256: &str =
        "0792569ac7ed14846ad8bfcff1b7568b917505b34997df668db641925f437257";

    assert_eq!(PROTOCOL_VERSION, 2);
    assert_eq!(
        format!("{:x}", Sha256::digest(WIRE_SCHEMA_DESCRIPTOR.as_bytes())),
        PINNED_SCHEMA_SHA256
    );
    assert_eq!(WIRE_SCHEMA_SHA256, PINNED_SCHEMA_SHA256);
    assert_eq!(WIRE_SCHEMA_SHA256.len(), 64);
    assert!(WIRE_SCHEMA_SHA256
        .bytes()
        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)));

    let message = ClientMessage::Hello(ClientHello {
        protocol_version: PROTOCOL_VERSION,
        wire_schema_sha256: ContentHash::parse(WIRE_SCHEMA_SHA256)
            .expect("published schema fingerprint must be a content hash"),
        client_build: ClientBuild::new("contract-test").expect("fixture client build"),
        display_name: DisplayName::new("Alice").expect("fixture display name"),
        resume: None,
    });
    let encoded = serde_json::to_value(&message).expect("hello must serialize");
    assert_eq!(
        encoded["payload"]["wire_schema_sha256"],
        Value::String(PINNED_SCHEMA_SHA256.to_owned())
    );
    assert_round_trip(&message);
}

#[test]
fn audio_decoder_semantics_are_required_and_nonzero() {
    let mut zero_version = semantics();
    zero_version.audio_decoder_semantics_version = 0;
    assert_eq!(
        zero_version.validate(),
        Err(ProtocolInvariantError::ZeroSemanticVersion("audio decoder"))
    );

    let mut missing_audio =
        serde_json::to_value(semantics()).expect("semantics fixture must serialize");
    missing_audio
        .as_object_mut()
        .expect("semantics serializes as an object")
        .remove("audio_decoder_semantics_digest");
    assert!(
        serde_json::from_value::<MatchSemantics>(missing_audio).is_err(),
        "pre-audio-semantics v1 shape must fail closed"
    );
}

#[test]
fn bounded_display_name_uses_utf8_bytes_and_rejects_control_text() {
    let ascii_at_limit = "a".repeat(MAX_DISPLAY_NAME_BYTES);
    let ascii_over_limit = "a".repeat(MAX_DISPLAY_NAME_BYTES + 1);
    assert!(DisplayName::new(ascii_at_limit.clone()).is_ok());
    assert!(DisplayName::new(ascii_over_limit.clone()).is_err());
    assert!(DisplayName::new("").is_err());
    assert!(DisplayName::new("Alice\nAdmin").is_err());

    let multibyte_within_limit = "鼓".repeat(10);
    let multibyte_over_limit = "鼓".repeat(11);
    assert_eq!(multibyte_within_limit.len(), 30);
    assert_eq!(multibyte_over_limit.len(), 33);
    assert!(DisplayName::new(multibyte_within_limit).is_ok());
    assert!(DisplayName::new(multibyte_over_limit).is_err());

    assert!(
        serde_json::from_value::<DisplayName>(Value::String(ascii_at_limit)).is_ok(),
        "the deserialization boundary must accept the exact byte limit"
    );
    assert!(
        serde_json::from_value::<DisplayName>(json!("")).is_err(),
        "the deserialization boundary must reject an empty display name"
    );
    assert!(
        serde_json::from_value::<DisplayName>(Value::String(ascii_over_limit)).is_err(),
        "the deserialization boundary must reject an oversized name"
    );
    assert!(
        serde_json::from_value::<DisplayName>(json!("Alice\u{0000}Admin")).is_err(),
        "the deserialization boundary must reject control text"
    );
}

#[test]
fn presentation_boundaries_match_the_shared_resource_contract() {
    assert!(DisplayTitle::new("t".repeat(MAX_TITLE_BYTES)).is_ok());
    assert!(DisplayTitle::new("t".repeat(MAX_TITLE_BYTES + 1)).is_err());
    assert!(DisplayTitle::new(" padded").is_err());
    assert!(CourseName::new("c".repeat(MAX_COURSE_NAME_BYTES)).is_ok());
    assert!(CourseName::new("c".repeat(MAX_COURSE_NAME_BYTES + 1)).is_err());
    assert!(CourseName::new("padded ").is_err());
    assert!(BoundedText::<MAX_SUBTITLE_BYTES>::new("s".repeat(MAX_SUBTITLE_BYTES)).is_ok());
    assert!(BoundedText::<MAX_ARTIST_BYTES>::new("a".repeat(MAX_ARTIST_BYTES)).is_ok());
}

#[test]
fn removed_client_branch_selection_is_rejected() {
    let stale = json!({
        "type": "command",
        "payload": {
            "seq": 1,
            "expected_room_revision": null,
            "command": {
                "command": "select_course",
                "data": {
                    "match_id": 7,
                    "selection": {
                        "course_id": 0,
                        "branch": {"policy": "automatic"}
                    }
                }
            }
        }
    });
    assert!(
        serde_json::from_value::<ClientMessage>(stale).is_err(),
        "protocol v2 must reject the removed branch selection field"
    );
}

#[test]
fn content_hash_is_exactly_lowercase_sha256_text() {
    let valid = "abcdef0123456789".repeat(4);
    let parsed = ContentHash::parse(valid.clone()).expect("64 lowercase hex bytes are valid");
    assert_eq!(parsed.as_str(), valid);
    assert_eq!(
        serde_json::to_value(&parsed).expect("hash must serialize"),
        json!(valid)
    );

    for invalid in [
        "a".repeat(63),
        "a".repeat(65),
        "A".repeat(64),
        "g".repeat(64),
        format!("{}-", "a".repeat(63)),
    ] {
        assert!(ContentHash::parse(invalid.clone()).is_err());
        assert!(serde_json::from_value::<ContentHash>(json!(invalid)).is_err());
    }
}

#[test]
fn song_resource_ids_reject_non_hash_wire_values() {
    let mut short_source =
        serde_json::to_value(song_manifest()).expect("song manifest fixture must serialize");
    short_source["source_id"] = json!("charts/song.tja");
    assert!(serde_json::from_value::<SongManifest>(short_source).is_err());

    let mut uppercase_audio =
        serde_json::to_value(song_manifest()).expect("song manifest fixture must serialize");
    uppercase_audio["audio_id"] = json!("A".repeat(64));
    assert!(serde_json::from_value::<SongManifest>(uppercase_audio).is_err());
}

#[test]
fn audio_identity_is_required_nullable_and_silent_manifests_validate() {
    let mut silent = song_manifest();
    silent.audio_id = None;
    silent.song_id = silent.derive_song_id().expect("derive silent identity");
    silent.validate().expect("silent manifest");
    assert_ne!(silent.song_id, song_manifest().song_id);

    let mut missing_song_audio =
        serde_json::to_value(song_manifest()).expect("song manifest fixture must serialize");
    missing_song_audio
        .as_object_mut()
        .expect("song object")
        .remove("audio_id");
    assert!(serde_json::from_value::<SongManifest>(missing_song_audio).is_err());

    let mut silent_proof = preparation_proof('d');
    silent_proof.audio_id = None;
    silent_proof
        .validate_for(
            &silent,
            &PlayerCourseAssignment {
                player_id: PlayerId(11),
                selection: PlayerSelection {
                    course_id: CourseId(0),
                },
                canonical_chart_hash: hash('d'),
            },
        )
        .expect("silent proof");

    let mut missing_proof_audio =
        serde_json::to_value(preparation_proof('d')).expect("proof fixture must serialize");
    missing_proof_audio
        .as_object_mut()
        .expect("proof object")
        .remove("audio_id");
    assert!(serde_json::from_value::<PreparationProof>(missing_proof_audio).is_err());
}

#[test]
fn secret_tokens_are_fixed_lowercase_hex_and_redacted() {
    let resume_text = "a".repeat(MAX_SECRET_TOKEN_BYTES);
    let invitation_text = "b".repeat(MAX_SECRET_TOKEN_BYTES);
    let clock_probe_text = "c".repeat(MAX_SECRET_TOKEN_BYTES);
    let resume = ResumeToken::parse(resume_text.clone()).expect("valid resume token");
    let invitation =
        InvitationToken::parse(invitation_text.clone()).expect("valid invitation token");
    let clock_probe =
        ClockProbeToken::parse(clock_probe_text.clone()).expect("valid clock probe token");

    assert_eq!(resume.expose(), resume_text);
    assert_eq!(invitation.expose(), invitation_text);
    assert_eq!(clock_probe.expose(), clock_probe_text);
    assert_eq!(format!("{resume:?}"), "ResumeToken(REDACTED)");
    assert_eq!(format!("{invitation:?}"), "InvitationToken(REDACTED)");
    assert_eq!(format!("{clock_probe:?}"), "ClockProbeToken(REDACTED)");
    assert_eq!(
        serde_json::to_value(&resume).expect("token must serialize"),
        json!(resume_text)
    );
    assert_eq!(
        serde_json::to_value(&clock_probe).expect("clock probe token must serialize"),
        json!(clock_probe_text)
    );

    for invalid in [
        "a".repeat(MAX_SECRET_TOKEN_BYTES - 1),
        "a".repeat(MAX_SECRET_TOKEN_BYTES + 1),
        "A".repeat(MAX_SECRET_TOKEN_BYTES),
        "z".repeat(MAX_SECRET_TOKEN_BYTES),
    ] {
        assert!(ResumeToken::parse(invalid.clone()).is_err());
        assert!(InvitationToken::parse(invalid.clone()).is_err());
        assert!(ClockProbeToken::parse(invalid.clone()).is_err());
        assert!(serde_json::from_value::<ResumeToken>(json!(invalid.clone())).is_err());
        assert!(serde_json::from_value::<InvitationToken>(json!(invalid.clone())).is_err());
        assert!(serde_json::from_value::<ClockProbeToken>(json!(invalid)).is_err());
    }
}

#[test]
fn room_code_is_fixed_length_normalized_and_excludes_ambiguous_characters() {
    assert_eq!(
        RoomCode::parse("j7k9").expect("valid room code").as_str(),
        "J7K9"
    );
    assert_eq!(
        serde_json::from_value::<RoomCode>(json!("j7k9"))
            .expect("wire room code must normalize")
            .as_str(),
        "J7K9"
    );

    for invalid in ["ABC", "ABCDE", "A0CD", "A1CD", "AICD", "AOCD", "A-CD"] {
        assert!(
            RoomCode::parse(invalid).is_err(),
            "{invalid} must be rejected"
        );
        assert!(
            serde_json::from_value::<RoomCode>(json!(invalid)).is_err(),
            "{invalid} must be rejected at the wire boundary"
        );
    }
}

#[test]
fn bounded_collections_reject_oversized_wire_payloads() {
    assert!(BoundedVec::<u8, 2>::new(vec![1, 2]).is_ok());
    assert!(BoundedVec::<u8, 2>::new(vec![1, 2, 3]).is_err());
    assert!(serde_json::from_value::<BoundedVec<u8, 2>>(json!([1, 2])).is_ok());
    assert!(serde_json::from_value::<BoundedVec<u8, 2>>(json!([1, 2, 3])).is_err());

    let events = (0..=MAX_INPUT_BATCH_EVENTS)
        .map(|seq| {
            json!({
                "seq": seq,
                "tick": seq,
                "action": {"side": "left", "zone": "don"},
            })
        })
        .collect::<Vec<_>>();
    let oversized_batch = json!({
        "match_id": 42,
        "events": events,
    });
    assert!(
        serde_json::from_value::<InputBatch>(oversized_batch).is_err(),
        "input batch limits must be enforced during deserialization"
    );
}

#[test]
fn every_client_message_variant_round_trips() {
    let messages = vec![
        ClientMessage::Hello(ClientHello {
            protocol_version: PROTOCOL_VERSION,
            wire_schema_sha256: ContentHash::parse(WIRE_SCHEMA_SHA256)
                .expect("fixture schema hash"),
            client_build: ClientBuild::new("taiko-contract/1").expect("fixture client build"),
            display_name: DisplayName::new("Alice").expect("fixture display name"),
            resume: Some(ResumeRequest {
                room_code: RoomCode::parse("J7K9").expect("fixture room code"),
                actor_id: ActorId::Player(PlayerId(11)),
                token: resume_token(),
                last_room_revision: RoomRevision(16),
                last_acked_command_seq: CommandSeq(7),
            }),
        }),
        ClientMessage::Command(CommandEnvelope {
            seq: CommandSeq(8),
            expected_room_revision: Some(RoomRevision(17)),
            command: ClientCommand::SetReady {
                match_id: MatchId(42),
                ready: true,
                proof: Some(preparation_proof('d')),
            },
        }),
        ClientMessage::Input(InputBatch {
            match_id: MatchId(42),
            events: bounded_vec(vec![
                InputEvent {
                    seq: InputSeq(40),
                    tick: 1_000_000,
                    action: DrumAction::LEFT_DON,
                },
                InputEvent {
                    seq: InputSeq(41),
                    tick: 1_010_000,
                    action: DrumAction::RIGHT_KAT,
                },
            ]),
        }),
        ClientMessage::TimeSync(TimeSyncRequest {
            nonce: 101,
            client_send_us: 7_900_000,
        }),
        ClientMessage::TimeSyncReceipt(TimeSyncReceipt {
            nonce: 101,
            probe_token: clock_probe_token(),
        }),
        ClientMessage::Heartbeat(Heartbeat { nonce: 102 }),
    ];

    for message in messages {
        assert_round_trip(&message);
    }
}

#[test]
fn clock_probe_challenge_response_has_stable_golden_wire_shapes() {
    let token = clock_probe_token();
    let response = ServerMessage::TimeSync(TimeSyncResponse {
        nonce: 101,
        client_send_us: 7_900_000,
        server_receive_us: 7_900_025,
        server_send_us: 7_900_030,
        probe_token: token.clone(),
    });
    assert_eq!(
        serde_json::to_string(&response).expect("time sync response must serialize"),
        r#"{"type":"time_sync","payload":{"nonce":101,"client_send_us":7900000,"server_receive_us":7900025,"server_send_us":7900030,"probe_token":"3333333333333333333333333333333333333333333333333333333333333333"}}"#
    );
    assert_round_trip(&response);

    let receipt = ClientMessage::TimeSyncReceipt(TimeSyncReceipt {
        nonce: 101,
        probe_token: token,
    });
    assert_eq!(
        serde_json::to_string(&receipt).expect("time sync receipt must serialize"),
        r#"{"type":"time_sync_receipt","payload":{"nonce":101,"probe_token":"3333333333333333333333333333333333333333333333333333333333333333"}}"#
    );
    assert_round_trip(&receipt);

    let acknowledgement = ServerMessage::ClockProbeAck(ClockProbeAck {
        nonce: 101,
        quality: ClockQuality {
            accepted_samples: 12,
            p95_rtt_ms: 34,
            jitter_ms: 5,
        },
        ready: true,
        valid_until_server_us: 7_910_000,
    });
    assert_eq!(
        serde_json::to_string(&acknowledgement).expect("clock probe ack must serialize"),
        r#"{"type":"clock_probe_ack","payload":{"nonce":101,"quality":{"accepted_samples":12,"p95_rtt_ms":34,"jitter_ms":5},"ready":true,"valid_until_server_us":7910000}}"#
    );
    assert_round_trip(&acknowledgement);
}

#[test]
fn lease_and_membership_messages_have_single_source_wire_shapes() {
    let heartbeat = ClientMessage::Heartbeat(Heartbeat { nonce: 102 });
    assert_eq!(
        serde_json::to_value(&heartbeat).expect("heartbeat must serialize"),
        json!({
            "type": "heartbeat",
            "payload": {"nonce": 102},
        })
    );
    assert_round_trip(&heartbeat);

    let heartbeat_ack = ServerMessage::HeartbeatAck(HeartbeatAck { nonce: 102 });
    assert_eq!(
        serde_json::to_value(&heartbeat_ack).expect("heartbeat acknowledgement must serialize"),
        json!({
            "type": "heartbeat_ack",
            "payload": {"nonce": 102},
        })
    );
    assert_round_trip(&heartbeat_ack);

    let welcome = ServerMessage::Welcome(ServerWelcome {
        protocol_version: PROTOCOL_VERSION,
        wire_schema_sha256: ContentHash::parse(WIRE_SCHEMA_SHA256).expect("fixture schema hash"),
        heartbeat_interval_ms: 2_000,
        reconnect_grace_ms: 10_000,
        resumed: true,
        next_expected_command_seq: CommandSeq(8),
    });
    assert_eq!(
        serde_json::to_value(&welcome).expect("welcome must serialize"),
        json!({
            "type": "welcome",
            "payload": {
                "protocol_version": 2,
                "wire_schema_sha256": WIRE_SCHEMA_SHA256,
                "heartbeat_interval_ms": 2_000,
                "reconnect_grace_ms": 10_000,
                "resumed": true,
                "next_expected_command_seq": 8,
            },
        })
    );
    assert_round_trip(&welcome);

    let membership = ServerMessage::MembershipGranted(MembershipGranted {
        room_code: RoomCode::parse("J7K9").expect("fixture room code"),
        actor_id: ActorId::Spectator(SpectatorId(33)),
        resume_token: resume_token(),
        invitation_token: invitation_token(),
    });
    assert_eq!(
        serde_json::to_value(&membership).expect("membership must serialize"),
        json!({
            "type": "membership_granted",
            "payload": {
                "room_code": "J7K9",
                "actor_id": {"kind": "spectator", "id": 33},
                "resume_token": "1".repeat(MAX_SECRET_TOKEN_BYTES),
                "invitation_token": "2".repeat(MAX_SECRET_TOKEN_BYTES),
            },
        })
    );
    assert_round_trip(&membership);

    for stale in [
        json!({
            "type": "heartbeat",
            "payload": {
                "nonce": 102,
                "last_room_revision": 17,
                "last_input_ack": 39,
            },
        }),
        json!({
            "type": "heartbeat_ack",
            "payload": {"nonce": 102, "server_now_us": 8_000_000},
        }),
        json!({
            "type": "welcome",
            "payload": {
                "protocol_version": 2,
                "wire_schema_sha256": WIRE_SCHEMA_SHA256,
                "session_id": 99,
                "heartbeat_interval_ms": 2_000,
                "reconnect_grace_ms": 10_000,
                "server_receive_us": 7_999_000,
                "server_send_us": 8_000_000,
                "resumed": true,
                "next_expected_command_seq": 8,
            },
        }),
        json!({
            "type": "membership_granted",
            "payload": {
                "room_code": "J7K9",
                "actor_id": {"kind": "spectator", "id": 33},
                "role": "spectator",
                "resume_token": "1".repeat(MAX_SECRET_TOKEN_BYTES),
                "invitation_token": "2".repeat(MAX_SECRET_TOKEN_BYTES),
            },
        }),
    ] {
        if stale["type"] == "heartbeat" {
            assert!(serde_json::from_value::<ClientMessage>(stale).is_err());
        } else {
            assert!(serde_json::from_value::<ServerMessage>(stale).is_err());
        }
    }
}

#[test]
fn every_client_command_and_preparation_progress_variant_round_trips() {
    let commands = vec![
        ClientCommand::CreateRoom,
        ClientCommand::JoinRoom {
            room_code: RoomCode::parse("J7K9").expect("fixture room code"),
            invitation_token: invitation_token(),
            role: JoinRole::Player,
        },
        ClientCommand::JoinRoom {
            room_code: RoomCode::parse("J7K9").expect("fixture room code"),
            invitation_token: invitation_token(),
            role: JoinRole::Spectator,
        },
        ClientCommand::LeaveRoom,
        ClientCommand::SelectSong {
            song_id: song_id('7'),
        },
        ClientCommand::SelectCourse {
            match_id: MatchId(42),
            selection: PlayerSelection {
                course_id: CourseId(1),
            },
        },
        ClientCommand::ReportPreparation {
            match_id: MatchId(42),
            progress: PreparationProgress::Downloading {
                selection: PlayerSelection {
                    course_id: CourseId(1),
                },
                progress_milli: ProgressMilli::new(500).expect("fixture progress"),
            },
        },
        ClientCommand::ReportPreparation {
            match_id: MatchId(42),
            progress: PreparationProgress::Verifying {
                selection: PlayerSelection {
                    course_id: CourseId(1),
                },
            },
        },
        ClientCommand::ReportPreparation {
            match_id: MatchId(42),
            progress: PreparationProgress::Loading {
                selection: PlayerSelection {
                    course_id: CourseId(1),
                },
            },
        },
        ClientCommand::ReportPreparation {
            match_id: MatchId(42),
            progress: PreparationProgress::Failed {
                selection: Some(PlayerSelection {
                    course_id: CourseId(1),
                }),
                reason: ErrorMessage::new("audio decode failed").expect("fixture error"),
            },
        },
        ClientCommand::SetReady {
            match_id: MatchId(42),
            ready: true,
            proof: Some(preparation_proof('d')),
        },
        ClientCommand::StartMatch {
            match_id: MatchId(42),
        },
        ClientCommand::Rematch {
            previous_match_id: MatchId(42),
        },
        ClientCommand::ReturnToLobby {
            match_id: MatchId(42),
        },
    ];

    for (index, command) in commands.into_iter().enumerate() {
        let message = ClientMessage::Command(CommandEnvelope {
            seq: CommandSeq(index as u64 + 1),
            expected_room_revision: (index % 2 == 0).then_some(RoomRevision(index as u64)),
            command,
        });
        assert_round_trip(&message);
    }
}

#[test]
fn every_server_message_variant_round_trips() {
    let manifest = per_player_manifest();
    let fatal = ProtocolError {
        code: ProtocolErrorCode::SlowConsumer,
        message: ErrorMessage::new("reliable queue is full").expect("fixture error"),
        retryable: true,
    };
    let messages = vec![
        ServerMessage::Welcome(ServerWelcome {
            protocol_version: PROTOCOL_VERSION,
            wire_schema_sha256: ContentHash::parse(WIRE_SCHEMA_SHA256)
                .expect("fixture schema hash"),
            heartbeat_interval_ms: 2_000,
            reconnect_grace_ms: 10_000,
            resumed: true,
            next_expected_command_seq: CommandSeq(8),
        }),
        ServerMessage::MembershipGranted(MembershipGranted {
            room_code: RoomCode::parse("J7K9").expect("fixture room code"),
            actor_id: ActorId::Spectator(SpectatorId(33)),
            resume_token: resume_token(),
            invitation_token: invitation_token(),
        }),
        ServerMessage::CommandAck(CommandAck {
            seq: CommandSeq(8),
            next_expected_seq: CommandSeq(9),
            outcome: CommandOutcome::Applied {
                room_revision: Some(RoomRevision(18)),
            },
        }),
        ServerMessage::RoomSnapshot(Box::new(room_snapshot(RoomStage::Countdown {
            manifest: manifest.clone(),
            start_at_server_us: 8_250_000,
        }))),
        ServerMessage::LiveState(LiveStateSnapshot {
            match_id: MatchId(42),
            state_seq: StateSeq(21),
            server_tick: 2_345_678,
            players: bounded_vec(vec![
                PlayerLiveState {
                    player_id: PlayerId(11),
                    score: score(),
                    finished: false,
                    dnf: false,
                },
                PlayerLiveState {
                    player_id: PlayerId(22),
                    score: score(),
                    finished: true,
                    dnf: false,
                },
            ]),
        }),
        ServerMessage::InputAck(InputAck {
            match_id: MatchId(42),
            highest_contiguous_seq: Some(InputSeq(41)),
            next_expected_seq: InputSeq(42),
            server_tick: 2_345_678,
            outcome: InputOutcome::Accepted,
        }),
        ServerMessage::TimeSync(TimeSyncResponse {
            nonce: 101,
            client_send_us: 7_900_000,
            server_receive_us: 7_900_025,
            server_send_us: 7_900_030,
            probe_token: clock_probe_token(),
        }),
        ServerMessage::ClockProbeAck(ClockProbeAck {
            nonce: 101,
            quality: ClockQuality {
                accepted_samples: 12,
                p95_rtt_ms: 34,
                jitter_ms: 5,
            },
            ready: true,
            valid_until_server_us: 7_910_000,
        }),
        ServerMessage::HeartbeatAck(HeartbeatAck { nonce: 102 }),
        ServerMessage::Fatal(fatal),
    ];

    for message in messages {
        assert_round_trip(&message);
    }
}

#[test]
fn supporting_tagged_enums_and_both_ack_outcomes_round_trip() {
    for actor in [
        ActorId::Player(PlayerId(11)),
        ActorId::Spectator(SpectatorId(33)),
    ] {
        assert_round_trip(&actor);
    }

    for preparation in [
        PlayerPreparation::Selecting,
        PlayerPreparation::Downloading {
            selection: PlayerSelection {
                course_id: CourseId(1),
            },
            progress_milli: ProgressMilli::new(500).expect("fixture progress"),
        },
        PlayerPreparation::Verifying {
            selection: PlayerSelection {
                course_id: CourseId(1),
            },
        },
        PlayerPreparation::Loading {
            selection: PlayerSelection {
                course_id: CourseId(1),
            },
        },
        PlayerPreparation::Prepared {
            selection: PlayerSelection {
                course_id: CourseId(1),
            },
        },
        PlayerPreparation::Ready {
            selection: PlayerSelection {
                course_id: CourseId(1),
            },
        },
        PlayerPreparation::Failed {
            selection: None,
            reason: ErrorMessage::new("resource unavailable").expect("fixture error"),
        },
    ] {
        assert_round_trip(&preparation);
    }

    for connection in [
        PlayerConnection::Online,
        PlayerConnection::Reconnecting {
            grace_deadline_server_us: 9_000_000,
        },
        PlayerConnection::Dnf,
    ] {
        assert_round_trip(&connection);
    }

    for outcome in [
        CommandOutcome::Applied {
            room_revision: Some(RoomRevision(18)),
        },
        CommandOutcome::Rejected {
            error: ProtocolError {
                code: ProtocolErrorCode::StaleRevision,
                message: ErrorMessage::new("expected revision 18").expect("fixture error"),
                retryable: true,
            },
            current_room_revision: Some(RoomRevision(18)),
        },
    ] {
        assert_round_trip(&CommandAck {
            seq: CommandSeq(8),
            next_expected_seq: CommandSeq(9),
            outcome,
        });
    }

    for outcome in [
        InputOutcome::Accepted,
        InputOutcome::Rejected {
            error: ProtocolError {
                code: ProtocolErrorCode::SequenceGap,
                message: ErrorMessage::new("expected input 42").expect("fixture error"),
                retryable: true,
            },
        },
    ] {
        assert_round_trip(&outcome);
    }

    for (action, expected) in [
        (DrumAction::LEFT_DON, json!({"side": "left", "zone": "don"})),
        (
            DrumAction::RIGHT_DON,
            json!({"side": "right", "zone": "don"}),
        ),
        (DrumAction::LEFT_KAT, json!({"side": "left", "zone": "kat"})),
        (
            DrumAction::RIGHT_KAT,
            json!({"side": "right", "zone": "kat"}),
        ),
    ] {
        assert_eq!(
            serde_json::to_value(action).expect("drum action must serialize"),
            expected
        );
        assert_round_trip(&action);
    }
}

#[test]
fn drum_action_requires_exactly_one_valid_side_and_zone() {
    for invalid in [
        json!("don"),
        json!({"zone": "don"}),
        json!({"side": "left"}),
        json!({"side": "center", "zone": "don"}),
        json!({"side": "left", "zone": "rim"}),
        json!({"side": "left", "zone": "don", "legacy": true}),
    ] {
        assert!(
            serde_json::from_value::<DrumAction>(invalid.clone()).is_err(),
            "invalid drum action was unexpectedly accepted: {invalid}"
        );
    }
}

#[test]
fn every_protocol_error_code_has_a_stable_snake_case_wire_name() {
    let cases = [
        (
            ProtocolErrorCode::UnsupportedProtocol,
            "unsupported_protocol",
        ),
        (ProtocolErrorCode::InvalidMessage, "invalid_message"),
        (ProtocolErrorCode::InvalidName, "invalid_name"),
        (ProtocolErrorCode::InvalidRoomCode, "invalid_room_code"),
        (ProtocolErrorCode::InvalidInvitation, "invalid_invitation"),
        (ProtocolErrorCode::RoomNotFound, "room_not_found"),
        (ProtocolErrorCode::RoomFull, "room_full"),
        (ProtocolErrorCode::SpectatorFull, "spectator_full"),
        (ProtocolErrorCode::AlreadyMember, "already_member"),
        (ProtocolErrorCode::NotMember, "not_member"),
        (ProtocolErrorCode::PermissionDenied, "permission_denied"),
        (ProtocolErrorCode::InvalidStage, "invalid_stage"),
        (ProtocolErrorCode::StaleRevision, "stale_revision"),
        (ProtocolErrorCode::StaleMatch, "stale_match"),
        (ProtocolErrorCode::InvalidCourse, "invalid_course"),
        (ProtocolErrorCode::NotPrepared, "not_prepared"),
        (ProtocolErrorCode::ClockNotReady, "clock_not_ready"),
        (ProtocolErrorCode::SequenceGap, "sequence_gap"),
        (ProtocolErrorCode::InvalidInput, "invalid_input"),
        (ProtocolErrorCode::RateLimited, "rate_limited"),
        (ProtocolErrorCode::SlowConsumer, "slow_consumer"),
        (ProtocolErrorCode::ResumeRejected, "resume_rejected"),
        (ProtocolErrorCode::SessionExpired, "session_expired"),
        (ProtocolErrorCode::SessionSuperseded, "session_superseded"),
        (ProtocolErrorCode::RoomClosed, "room_closed"),
        (ProtocolErrorCode::ServerBusy, "server_busy"),
        (ProtocolErrorCode::Internal, "internal"),
    ];

    for (code, wire_name) in cases {
        assert_eq!(
            serde_json::to_value(code).expect("error code must serialize"),
            json!(wire_name)
        );
        assert_eq!(
            serde_json::from_value::<ProtocolErrorCode>(json!(wire_name))
                .expect("error code must deserialize"),
            code
        );
    }
}

#[test]
fn stale_pre_redesign_v1_client_messages_are_rejected() {
    let stale_messages = [
        r#"{"type":"hello","payload":{"protocol_version":1,"name":"alice"}}"#,
        r#"{"type":"create_room"}"#,
        r#"{"type":"join_room","payload":{"room_code":"ABC123","spectate":false}}"#,
        r#"{"type":"ready","payload":{"ready":true}}"#,
        r#"{"type":"host_select_song","payload":{"source_id":"chart-1","course_index":0}}"#,
        r#"{"type":"start_match"}"#,
        r#"{"type":"input_event","payload":{"seq":1,"tick":100,"action":"don"}}"#,
        r#"{"type":"player_state_update","payload":{"seq":2}}"#,
        r#"{"type":"final_result","payload":{"seq":3}}"#,
        r#"{"type":"ping","payload":{"nonce":9,"client_send_ms":1000,"server_send_ms":null}}"#,
    ];

    for stale in stale_messages {
        assert!(
            serde_json::from_str::<ClientMessage>(stale).is_err(),
            "stale v1 client shape was unexpectedly accepted: {stale}"
        );
    }
}

#[test]
fn stale_pre_redesign_v1_server_messages_are_rejected() {
    let stale_messages = [
        r#"{"type":"hello","payload":{"protocol_version":1,"session":{"session_id":"s1","name":"alice"}}}"#,
        r#"{"type":"error","payload":{"code":"bad_request","message":"invalid"}}"#,
        r#"{"type":"room_created","payload":{"room_code":"ABC123"}}"#,
        r#"{"type":"room_joined","payload":{"room_code":"ABC123"}}"#,
        r#"{"type":"song_selected","payload":{"source_id":"chart-1","course_index":0}}"#,
        r#"{"type":"match_countdown","payload":{"room_code":"ABC123","start_at_ms":2000}}"#,
        r#"{"type":"match_started","payload":{"room_code":"ABC123","start_at_ms":2000}}"#,
        r#"{"type":"input_event","payload":{"player_id":"p1"}}"#,
        r#"{"type":"player_state_update","payload":{"player_id":"p1"}}"#,
        r#"{"type":"final_result","payload":{"player_id":"p1"}}"#,
        r#"{"type":"pong","payload":{"nonce":9,"client_send_ms":1000,"server_send_ms":1020}}"#,
    ];

    for stale in stale_messages {
        assert!(
            serde_json::from_str::<ServerMessage>(stale).is_err(),
            "stale v1 server shape was unexpectedly accepted: {stale}"
        );
    }
}

#[test]
fn match_manifest_has_a_stable_golden_wire_shape() {
    let manifest = per_player_manifest();
    let expected_song_id = manifest.song.song_id.to_string();
    let expected = json!({
        "match_id": 42,
        "song": {
            "song_id": expected_song_id,
            "source_id": "a".repeat(64),
            "audio_id": "b".repeat(64),
            "title": "Contract Song",
            "subtitle": "",
            "artist": "Protocol Artist",
        "semantics": {
                "canonical_schema_version": 3,
                "canonical_schema_digest": "8".repeat(64),
                "importer_semantics_version": 7,
                "importer_semantics_digest": "9".repeat(64),
                "ruleset_version": 11,
                "ruleset_digest": "c".repeat(64),
                "audio_decoder_semantics_version": 13,
                "audio_decoder_semantics_digest": "f".repeat(64),
            },
            "courses": [
                {
                    "course_id": 0,
                    "name": "Normal",
                    "level": 5,
                    "canonical_chart_hash": "d".repeat(64),
                },
                {
                    "course_id": 1,
                    "name": "Oni",
                    "level": 9,
                    "canonical_chart_hash": "e".repeat(64),
                },
            ],
        },
        "assignments": [
            {
                "player_id": 11,
                "selection": {
                    "course_id": 0,
                },
                "canonical_chart_hash": "d".repeat(64),
            },
            {
                "player_id": 22,
                "selection": {
                    "course_id": 1,
                },
                "canonical_chart_hash": "e".repeat(64),
            },
        ],
        "countdown_ms": 3_000,
        "input_lateness_ms": 150,
    });

    assert_eq!(
        serde_json::to_value(&manifest).expect("manifest must serialize"),
        expected
    );
    assert_round_trip(&manifest);
}

#[test]
fn room_stage_variants_have_stable_golden_tags_and_preserve_match_epoch() {
    let manifest = per_player_manifest();
    let song = manifest.song.clone();
    let results = vec![
        final_result(PlayerId(11), CourseId(0), '1'),
        final_result(PlayerId(22), CourseId(1), '2'),
    ];
    let cases = vec![
        (RoomStage::Lobby, json!({"stage": "lobby"}), None),
        (
            RoomStage::Preparing {
                match_id: MatchId(42),
                song: song.clone(),
            },
            json!({
                "stage": "preparing",
                "data": {
                    "match_id": 42,
                    "song": serde_json::to_value(&song).expect("song fixture must serialize"),
                },
            }),
            Some(MatchId(42)),
        ),
        (
            RoomStage::Countdown {
                manifest: manifest.clone(),
                start_at_server_us: 8_250_000,
            },
            json!({
                "stage": "countdown",
                "data": {
                    "manifest": serde_json::to_value(&manifest)
                        .expect("manifest fixture must serialize"),
                    "start_at_server_us": 8_250_000_u64,
                },
            }),
            Some(MatchId(42)),
        ),
        (
            RoomStage::Playing {
                manifest: manifest.clone(),
                start_at_server_us: 8_250_000,
                server_tick: 2_345_678,
            },
            json!({
                "stage": "playing",
                "data": {
                    "manifest": serde_json::to_value(&manifest)
                        .expect("manifest fixture must serialize"),
                    "start_at_server_us": 8_250_000_u64,
                    "server_tick": 2_345_678,
                },
            }),
            Some(MatchId(42)),
        ),
        (
            RoomStage::Finalizing {
                manifest: manifest.clone(),
                server_tick: 12_345_678,
                deadline_server_us: 20_000_000,
            },
            json!({
                "stage": "finalizing",
                "data": {
                    "manifest": serde_json::to_value(&manifest)
                        .expect("manifest fixture must serialize"),
                    "server_tick": 12_345_678,
                    "deadline_server_us": 20_000_000_u64,
                },
            }),
            Some(MatchId(42)),
        ),
        (
            RoomStage::Finished {
                manifest: manifest.clone(),
                results: bounded_vec(results.clone()),
            },
            json!({
                "stage": "finished",
                "data": {
                    "manifest": serde_json::to_value(&manifest)
                        .expect("manifest fixture must serialize"),
                    "results": serde_json::to_value(&results)
                        .expect("result fixtures must serialize"),
                },
            }),
            Some(MatchId(42)),
        ),
    ];

    for (stage, golden, expected_match_id) in cases {
        assert_eq!(
            serde_json::to_value(&stage).expect("room stage must serialize"),
            golden
        );
        assert_round_trip(&stage);

        let actual_match_id = match &stage {
            RoomStage::Lobby => None,
            RoomStage::Preparing { match_id, .. } => Some(*match_id),
            RoomStage::Countdown { manifest, .. }
            | RoomStage::Playing { manifest, .. }
            | RoomStage::Finalizing { manifest, .. }
            | RoomStage::Finished { manifest, .. } => Some(manifest.match_id),
        };
        assert_eq!(actual_match_id, expected_match_id);
    }
}

#[test]
fn final_result_has_a_stable_golden_shape_and_is_not_a_client_message() {
    let result = final_result(PlayerId(11), CourseId(1), 'f');
    let expected = json!({
        "player_id": 11,
        "course_id": 1,
        "score": {
            "score": 987_650,
            "combo": 321,
            "max_combo": 400,
            "gauge_ppm": 875_000,
            "pass_threshold_ppm": 800_000,
            "great": 400,
            "ok": 50,
            "miss": 6,
            "roll_hits": 77,
        },
        "finish_tick": 12_345_678,
        "passed": true,
        "replay_digest": "f".repeat(64),
        "dnf": false,
    });
    assert_eq!(
        serde_json::to_value(&result).expect("final result must serialize"),
        expected
    );
    assert_round_trip(&result);

    let stale_client_final = json!({
        "type": "final_result",
        "payload": expected,
    });
    assert!(
        serde_json::from_value::<ClientMessage>(stale_client_final).is_err(),
        "final results are server-owned and must not be accepted as a client message"
    );
}

#[test]
fn manifest_assignments_unambiguously_encode_shared_and_per_player_courses() {
    let per_player = per_player_manifest();
    assert_manifest_fixture_invariants(&per_player);
    let per_player_map = assignment_map(&per_player);
    assert_eq!(
        per_player_map,
        BTreeMap::from([
            (PlayerId(11), (CourseId(0), hash('d'))),
            (PlayerId(22), (CourseId(1), hash('e'))),
        ])
    );

    let mut shared = per_player.clone();
    shared.match_id = MatchId(43);
    shared.assignments = bounded_vec(vec![
        PlayerCourseAssignment {
            player_id: PlayerId(11),
            selection: PlayerSelection {
                course_id: CourseId(1),
            },
            canonical_chart_hash: hash('e'),
        },
        PlayerCourseAssignment {
            player_id: PlayerId(22),
            selection: PlayerSelection {
                course_id: CourseId(1),
            },
            canonical_chart_hash: hash('e'),
        },
    ]);
    assert_manifest_fixture_invariants(&shared);
    let shared_map = assignment_map(&shared);
    assert_eq!(
        shared_map,
        BTreeMap::from([
            (PlayerId(11), (CourseId(1), hash('e'))),
            (PlayerId(22), (CourseId(1), hash('e'))),
        ])
    );
    assert_round_trip(&shared);
}

#[test]
fn manifest_and_preparation_invariants_reject_ambiguous_state() {
    let manifest = per_player_manifest();
    manifest.validate().expect("fixture manifest is valid");
    preparation_proof('d')
        .validate_for(&manifest.song, &manifest.assignments[0])
        .expect("matching proof");

    let mut wrong_source = preparation_proof('d');
    wrong_source.source_id = hash('0');
    assert_eq!(
        wrong_source.validate_for(&manifest.song, &manifest.assignments[0]),
        Err(ProtocolInvariantError::PreparationProofMismatch)
    );

    let mut wrong_audio = preparation_proof('d');
    wrong_audio.audio_id = Some(hash('1'));
    assert_eq!(
        wrong_audio.validate_for(&manifest.song, &manifest.assignments[0]),
        Err(ProtocolInvariantError::PreparationProofMismatch)
    );

    let mut mismatched_song_identity = manifest.clone();
    mismatched_song_identity.song.audio_id = Some(hash('0'));
    assert_eq!(
        mismatched_song_identity.validate(),
        Err(ProtocolInvariantError::SongIdentityMismatch)
    );

    let mut duplicate_player = manifest.clone();
    let mut duplicate_assignments = duplicate_player.assignments.clone().into_vec();
    duplicate_assignments[1].player_id = PlayerId(11);
    duplicate_player.assignments = bounded_vec(duplicate_assignments);
    assert!(matches!(
        duplicate_player.validate(),
        Err(ProtocolInvariantError::DuplicatePlayerAssignment(PlayerId(
            11
        )))
    ));

    let mut wrong_hash = manifest.clone();
    let mut wrong_assignments = wrong_hash.assignments.clone().into_vec();
    wrong_assignments[0].canonical_chart_hash = hash('f');
    wrong_hash.assignments = bounded_vec(wrong_assignments);
    assert!(matches!(
        wrong_hash.validate(),
        Err(ProtocolInvariantError::AssignedCourseHashMismatch(
            PlayerId(11)
        ))
    ));

    let mut non_dense_course = manifest.clone();
    let mut non_dense_courses = non_dense_course.song.courses.clone().into_vec();
    non_dense_courses[1].course_id = CourseId(9);
    non_dense_course.song.courses = bounded_vec(non_dense_courses);
    assert!(matches!(
        non_dense_course.validate(),
        Err(ProtocolInvariantError::NonDenseCourseId {
            expected: 1,
            actual: 9
        })
    ));
}

#[test]
fn room_snapshot_invariants_bind_leader_players_and_active_assignments() {
    let manifest = per_player_manifest();
    let snapshot = room_snapshot(RoomStage::Countdown {
        manifest,
        start_at_server_us: 8_250_000,
    });
    snapshot.validate().expect("fixture snapshot is valid");

    let mut bad_leader = snapshot.clone();
    let mut players = bad_leader.players.clone().into_vec();
    players[0].is_leader = false;
    bad_leader.players = bounded_vec(players);
    assert!(matches!(
        bad_leader.validate(),
        Err(ProtocolInvariantError::InvalidRoomLeader)
    ));

    let mut bad_preparation = snapshot;
    let mut players = bad_preparation.players.clone().into_vec();
    players[1].preparation = PlayerPreparation::Selecting;
    bad_preparation.players = bounded_vec(players);
    assert!(matches!(
        bad_preparation.validate(),
        Err(ProtocolInvariantError::ActivePreparationMismatch(PlayerId(
            22
        )))
    ));
}

#[test]
fn live_state_invariants_bind_exactly_one_valid_state_to_each_assignment() {
    let manifest = per_player_manifest();
    let live = LiveStateSnapshot {
        match_id: manifest.match_id,
        state_seq: StateSeq(1),
        server_tick: 123_456,
        players: bounded_vec(vec![
            PlayerLiveState {
                player_id: PlayerId(11),
                score: score(),
                finished: false,
                dnf: false,
            },
            PlayerLiveState {
                player_id: PlayerId(22),
                score: score(),
                finished: true,
                dnf: false,
            },
        ]),
    };
    live.validate_for(&manifest)
        .expect("fixture live state is valid");

    let mut zero_sequence = live.clone();
    zero_sequence.state_seq = StateSeq(0);
    assert!(matches!(
        zero_sequence.validate_for(&manifest),
        Err(ProtocolInvariantError::ZeroStateSequence)
    ));

    let mut negative_tick = live.clone();
    negative_tick.server_tick = -1;
    assert!(matches!(
        negative_tick.validate_for(&manifest),
        Err(ProtocolInvariantError::NegativeServerTick)
    ));

    let mut duplicate_player = live.clone();
    let mut players = duplicate_player.players.into_vec();
    players[1].player_id = players[0].player_id;
    duplicate_player.players = bounded_vec(players);
    assert!(matches!(
        duplicate_player.validate_for(&manifest),
        Err(ProtocolInvariantError::DuplicateLivePlayer(PlayerId(11)))
    ));

    let mut missing_player = live.clone();
    missing_player.players = bounded_vec(vec![missing_player.players[0].clone()]);
    assert!(matches!(
        missing_player.validate_for(&manifest),
        Err(ProtocolInvariantError::LivePlayerMismatch)
    ));

    let mut impossible_score = live.clone();
    let mut players = impossible_score.players.into_vec();
    players[0].score.combo = players[0].score.max_combo.saturating_add(1);
    impossible_score.players = bounded_vec(players);
    assert!(matches!(
        impossible_score.validate_for(&manifest),
        Err(ProtocolInvariantError::InvalidScore)
    ));

    let mut unfinished_dnf = live;
    let mut players = unfinished_dnf.players.into_vec();
    players[0].dnf = true;
    players[0].finished = false;
    unfinished_dnf.players = bounded_vec(players);
    assert!(matches!(
        unfinished_dnf.validate_for(&manifest),
        Err(ProtocolInvariantError::DnfPlayerNotFinished)
    ));
}

#[test]
fn final_results_are_server_score_consistent_and_match_assignments() {
    let manifest = per_player_manifest();
    let finished = room_snapshot(RoomStage::Finished {
        manifest: manifest.clone(),
        results: bounded_vec(vec![
            final_result(PlayerId(11), CourseId(0), '1'),
            final_result(PlayerId(22), CourseId(1), '2'),
        ]),
    });
    finished
        .validate()
        .expect("fixture finished snapshot is valid");

    let mut contradictory_pass = finished.clone();
    let RoomStage::Finished { results, .. } = &mut contradictory_pass.stage else {
        unreachable!("fixture is finished");
    };
    let mut changed_results = results.clone().into_vec();
    changed_results[0].passed = false;
    *results = bounded_vec(changed_results);
    assert!(matches!(
        contradictory_pass.validate(),
        Err(ProtocolInvariantError::InvalidFinalResult)
    ));

    let mut dnf_pass = finished.clone();
    let RoomStage::Finished { results, .. } = &mut dnf_pass.stage else {
        unreachable!("fixture is finished");
    };
    let mut changed_results = results.clone().into_vec();
    changed_results[0].dnf = true;
    *results = bounded_vec(changed_results);
    assert!(matches!(
        dnf_pass.validate(),
        Err(ProtocolInvariantError::InvalidFinalResult)
    ));

    let mut negative_finish = finished.clone();
    let RoomStage::Finished { results, .. } = &mut negative_finish.stage else {
        unreachable!("fixture is finished");
    };
    let mut changed_results = results.clone().into_vec();
    changed_results[0].finish_tick = -1;
    *results = bounded_vec(changed_results);
    assert!(matches!(
        negative_finish.validate(),
        Err(ProtocolInvariantError::InvalidFinalResult)
    ));

    let mut mismatched_course = finished;
    let RoomStage::Finished { results, .. } = &mut mismatched_course.stage else {
        unreachable!("fixture is finished");
    };
    let mut changed_results = results.clone().into_vec();
    changed_results[0].course_id = CourseId(1);
    *results = bounded_vec(changed_results);
    assert!(matches!(
        mismatched_course.validate(),
        Err(ProtocolInvariantError::FinishedResultMismatch)
    ));
}

#[test]
fn active_stage_snapshots_reject_negative_authoritative_ticks() {
    for stage in [
        RoomStage::Playing {
            manifest: per_player_manifest(),
            start_at_server_us: 8_000_000,
            server_tick: -1,
        },
        RoomStage::Finalizing {
            manifest: per_player_manifest(),
            server_tick: -1,
            deadline_server_us: 8_500_000,
        },
    ] {
        assert!(matches!(
            room_snapshot(stage).validate(),
            Err(ProtocolInvariantError::NegativeServerTick)
        ));
    }
}

#[test]
fn input_acknowledgement_watermark_is_self_consistent() {
    let accepted = InputAck {
        match_id: MatchId(42),
        highest_contiguous_seq: Some(InputSeq(9)),
        next_expected_seq: InputSeq(10),
        server_tick: 123,
        outcome: InputOutcome::Accepted,
    };
    accepted.validate().expect("ack watermark is valid");

    for invalid in [
        InputAck {
            next_expected_seq: InputSeq(9),
            ..accepted.clone()
        },
        InputAck {
            highest_contiguous_seq: None,
            next_expected_seq: InputSeq(2),
            ..accepted.clone()
        },
        InputAck {
            highest_contiguous_seq: Some(InputSeq(0)),
            next_expected_seq: FIRST_INPUT_SEQ,
            ..accepted.clone()
        },
        InputAck {
            server_tick: -1,
            ..accepted
        },
    ] {
        assert!(matches!(
            invalid.validate(),
            Err(ProtocolInvariantError::InvalidInputAcknowledgement)
        ));
    }
}

#[test]
fn player_input_watermark_uses_none_instead_of_a_zero_sentinel() {
    let mut snapshot = room_snapshot(RoomStage::Lobby);
    let players = snapshot
        .players
        .as_slice()
        .iter()
        .cloned()
        .map(|mut player| {
            player.last_acked_input_seq = None;
            player
        })
        .collect();
    snapshot.players = bounded_vec(players);
    snapshot
        .validate()
        .expect("players with no processed inputs have no watermark");
    let wire = serde_json::to_value(&snapshot).expect("snapshot must serialize");
    assert_eq!(wire["players"][0]["last_acked_input_seq"], Value::Null);

    let mut players = snapshot.players.into_vec();
    players[0].last_acked_input_seq = Some(InputSeq(0));
    snapshot.players = bounded_vec(players);
    assert!(matches!(
        snapshot.validate(),
        Err(ProtocolInvariantError::ZeroPlayerInputWatermark(PlayerId(
            11
        )))
    ));
}

#[test]
fn finished_match_fixture_remains_unchanged_when_a_rematch_gets_a_new_match_id() {
    let original_manifest = per_player_manifest();
    let finished = RoomStage::Finished {
        manifest: original_manifest.clone(),
        results: bounded_vec(vec![
            final_result(PlayerId(11), CourseId(0), '1'),
            final_result(PlayerId(22), CourseId(1), '2'),
        ]),
    };
    let finished_wire = serde_json::to_vec(&finished).expect("finished fixture must serialize");

    let mut rematch_manifest = original_manifest;
    rematch_manifest.match_id = MatchId(43);
    let rematch = RoomStage::Preparing {
        match_id: rematch_manifest.match_id,
        song: rematch_manifest.song,
    };

    assert_ne!(
        match_id_of(&finished).expect("finished stage has a match id"),
        match_id_of(&rematch).expect("rematch stage has a match id")
    );
    assert_eq!(
        serde_json::to_vec(&finished).expect("finished fixture must remain serializable"),
        finished_wire,
        "creating a rematch fixture must not mutate the immutable completed match"
    );
}

#[test]
fn missing_or_unknown_fields_do_not_silently_change_struct_contracts() {
    let missing_schema_hash = json!({
        "type": "hello",
        "payload": {
            "protocol_version": 2,
            "client_build": "contract-test",
            "display_name": "Alice",
            "resume": null,
        },
    });
    assert!(serde_json::from_value::<ClientMessage>(missing_schema_hash).is_err());

    let unknown_resume_field = json!({
        "room_code": "J7K9",
        "actor_id": {"kind": "player", "id": 11},
        "token": "1".repeat(MAX_SECRET_TOKEN_BYTES),
        "last_room_revision": 16,
        "last_acked_command_seq": 7,
        "legacy_session_id": "session-1",
    });
    assert!(serde_json::from_value::<ResumeRequest>(unknown_resume_field).is_err());

    let stale_resume_input_watermark = json!({
        "room_code": "J7K9",
        "actor_id": {"kind": "player", "id": 11},
        "token": "1".repeat(MAX_SECRET_TOKEN_BYTES),
        "last_room_revision": 16,
        "last_acked_command_seq": 7,
        "last_acked_input_seq": 39,
    });
    assert!(
        serde_json::from_value::<ResumeRequest>(stale_resume_input_watermark).is_err(),
        "input watermarks are match-scoped and must be reconciled from the resumed snapshot"
    );

    let missing_probe_token = json!({
        "nonce": 101,
        "client_send_us": 7_900_000,
        "server_receive_us": 7_900_025,
        "server_send_us": 7_900_030,
    });
    assert!(serde_json::from_value::<TimeSyncResponse>(missing_probe_token).is_err());

    let unknown_receipt_field = json!({
        "nonce": 101,
        "probe_token": "3".repeat(MAX_SECRET_TOKEN_BYTES),
        "client_reported_rtt_ms": 34,
    });
    assert!(serde_json::from_value::<TimeSyncReceipt>(unknown_receipt_field).is_err());

    let legacy_client_clock = {
        let mut value =
            serde_json::to_value(preparation_proof('d')).expect("proof fixture must serialize");
        value
            .as_object_mut()
            .expect("preparation proof serializes as an object")
            .insert(
                "clock".to_owned(),
                json!({
                    "accepted_samples": 12,
                    "p95_rtt_ms": 34,
                    "jitter_ms": 5,
                }),
            );
        value
    };
    assert!(serde_json::from_value::<PreparationProof>(legacy_client_clock).is_err());

    let legacy_duplicate_hashes = {
        let mut value =
            serde_json::to_value(song_manifest()).expect("song manifest fixture must serialize");
        let object = value
            .as_object_mut()
            .expect("song manifest serializes as an object");
        object.insert("raw_chart_hash".to_owned(), json!("a".repeat(64)));
        object.insert("audio_hash".to_owned(), json!("b".repeat(64)));
        value
    };
    assert!(serde_json::from_value::<SongManifest>(legacy_duplicate_hashes).is_err());

    let unknown_final_field = {
        let mut value = serde_json::to_value(final_result(PlayerId(11), CourseId(1), 'f'))
            .expect("final fixture must serialize");
        value
            .as_object_mut()
            .expect("final result serializes as an object")
            .insert("client_override".to_owned(), json!(true));
        value
    };
    assert!(serde_json::from_value::<FinalResult>(unknown_final_field).is_err());
}

fn assignment_map(manifest: &MatchManifest) -> BTreeMap<PlayerId, (CourseId, ContentHash)> {
    manifest
        .assignments
        .iter()
        .map(|assignment| {
            (
                assignment.player_id,
                (
                    assignment.selection.course_id,
                    assignment.canonical_chart_hash.clone(),
                ),
            )
        })
        .collect()
}

fn assert_manifest_fixture_invariants(manifest: &MatchManifest) {
    manifest.validate().expect("fixture manifest must be valid");
    let courses = manifest
        .song
        .courses
        .iter()
        .map(|course| (course.course_id, course.canonical_chart_hash.clone()))
        .collect::<BTreeMap<_, _>>();
    let assignments = assignment_map(manifest);

    assert_eq!(
        assignments.len(),
        manifest.assignments.len(),
        "each player must have exactly one assignment"
    );
    for assignment in &manifest.assignments {
        assert_eq!(
            courses.get(&assignment.selection.course_id),
            Some(&assignment.canonical_chart_hash),
            "an assignment must bind to the selected course's canonical digest"
        );
    }
}

fn match_id_of(stage: &RoomStage) -> Option<MatchId> {
    match stage {
        RoomStage::Lobby => None,
        RoomStage::Preparing { match_id, .. } => Some(*match_id),
        RoomStage::Countdown { manifest, .. }
        | RoomStage::Playing { manifest, .. }
        | RoomStage::Finalizing { manifest, .. }
        | RoomStage::Finished { manifest, .. } => Some(manifest.match_id),
    }
}
