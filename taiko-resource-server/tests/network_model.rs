mod support;

use std::collections::{BTreeMap, HashSet};

use support::network::{
    AdversarialLink, AdversarialProfile, MessageFault, TcpLikeLink, TcpProfile,
};
use taiko_multiplayer_protocol::{
    ActorId, CommandAck, CommandOutcome, CommandSeq, ErrorMessage, InputAck, InputOutcome,
    InputSeq, MatchId, PlayerId, ProtocolError, ProtocolErrorCode, RoomRevision, SpectatorId,
    FIRST_COMMAND_SEQ, FIRST_INPUT_SEQ,
};

#[derive(Debug, Clone, PartialEq, Eq)]
enum ReplicatedFrame {
    CommandAck(CommandAck),
    Snapshot {
        revision: RoomRevision,
        actors: Vec<ActorId>,
    },
}

#[derive(Debug)]
struct RoomReplica {
    actor_id: ActorId,
    last_revision: RoomRevision,
    last_command_seq: CommandSeq,
    pending_applied_revision: Option<RoomRevision>,
}

impl RoomReplica {
    fn new(actor_id: ActorId) -> Self {
        Self {
            actor_id,
            last_revision: RoomRevision(0),
            last_command_seq: CommandSeq(FIRST_COMMAND_SEQ.0 - 1),
            pending_applied_revision: None,
        }
    }

    fn apply(&mut self, frame: ReplicatedFrame, profile: &str, node: usize) {
        match frame {
            ReplicatedFrame::CommandAck(ack) => {
                assert_eq!(
                    ack.seq.0,
                    self.last_command_seq.0 + 1,
                    "{profile} node {node}: command acknowledgements must be contiguous"
                );
                assert_eq!(
                    ack.next_expected_seq.0,
                    ack.seq.0 + 1,
                    "{profile} node {node}: acknowledgement must advertise the next sequence"
                );
                let CommandOutcome::Applied {
                    room_revision: Some(revision),
                } = ack.outcome
                else {
                    panic!("{profile} node {node}: generated trace contains a rejected command");
                };
                assert_eq!(
                    revision.0,
                    self.last_revision.0 + 1,
                    "{profile} node {node}: applied command must identify the next room revision"
                );
                assert!(
                    self.pending_applied_revision.replace(revision).is_none(),
                    "{profile} node {node}: a second ack arrived before its snapshot"
                );
                self.last_command_seq = ack.seq;
            }
            ReplicatedFrame::Snapshot { revision, actors } => {
                assert_eq!(
                    revision.0,
                    self.last_revision.0 + 1,
                    "{profile} node {node}: snapshots must be strictly revision ordered"
                );
                assert_eq!(
                    actors.iter().collect::<HashSet<_>>().len(),
                    actors.len(),
                    "{profile} node {node}: snapshot actor identities must be unique"
                );
                assert!(
                    actors.contains(&self.actor_id),
                    "{profile} node {node}: a replica must not lose its own actor identity"
                );
                if self.pending_applied_revision == Some(revision) {
                    self.pending_applied_revision = None;
                }
                self.last_revision = revision;
            }
        }
    }
}

fn applied_ack(seq: u64, revision: u64) -> CommandAck {
    CommandAck {
        seq: CommandSeq(seq),
        next_expected_seq: CommandSeq(seq + 1),
        outcome: CommandOutcome::Applied {
            room_revision: Some(RoomRevision(revision)),
        },
    }
}

fn profile_name(profile: TcpProfile) -> &'static str {
    if profile == TcpProfile::PERFECT {
        "perfect"
    } else if profile == TcpProfile::LAN {
        "lan"
    } else if profile == TcpProfile::WAN {
        "wan"
    } else if profile == TcpProfile::POOR_WAN {
        "poor-wan"
    } else {
        "custom"
    }
}

#[test]
fn tcp_profiles_preserve_websocket_order_and_exactly_once_delivery() {
    for (profile_index, profile) in [
        TcpProfile::PERFECT,
        TcpProfile::LAN,
        TcpProfile::WAN,
        TcpProfile::POOR_WAN,
    ]
    .into_iter()
    .enumerate()
    {
        let mut link = TcpLikeLink::new(profile, 0xfeed_u64 + profile_index as u64);
        let mut last_delivery = 0;
        for sequence in 1_u64..=200 {
            last_delivery = link.send(sequence * 100, 4_096, sequence);
        }

        let delivered = link
            .drain_ready(last_delivery.saturating_add(1))
            .into_iter()
            .map(|delivery| delivery.payload)
            .collect::<Vec<_>>();
        assert_eq!(delivered, (1_u64..=200).collect::<Vec<_>>());
        assert!(link.is_empty());
    }
}

#[test]
fn tcp_bandwidth_and_stalls_create_backpressure_without_message_loss() {
    let profile = TcpProfile {
        bandwidth_bytes_per_second: 1_000,
        stall_every_messages: Some(2),
        stall_us: 500_000,
        ..TcpProfile::PERFECT
    };
    let mut link = TcpLikeLink::new(profile, 1);
    let first = link.send(0, 1_000, 1);
    let second = link.send(0, 1_000, 2);
    let third = link.send(0, 1_000, 3);

    assert!(first >= 1_000_000);
    assert!(second >= first.saturating_add(1_000_000));
    assert!(third >= second);
    assert_eq!(
        link.drain_ready(third)
            .into_iter()
            .map(|delivery| delivery.payload)
            .collect::<Vec<_>>(),
        vec![1, 2, 3]
    );
}

#[test]
fn adversarial_layer_reproduces_drop_duplicate_and_reorder_faults() {
    let mut link = AdversarialLink::new(AdversarialProfile::PERFECT, 7);
    link.script_fault(1, MessageFault::Delay { extra_us: 100 });
    link.script_fault(
        3,
        MessageFault::Duplicate {
            duplicate_extra_us: 5,
        },
    );
    link.script_fault(4, MessageFault::Drop);

    link.send(0, 1_u64);
    link.send(0, 2_u64);
    link.send(0, 3_u64);
    link.send(0, 4_u64);

    assert_eq!(
        link.drain_ready(10)
            .into_iter()
            .map(|delivery| delivery.payload)
            .collect::<Vec<_>>(),
        vec![2, 3, 3]
    );
    assert_eq!(
        link.drain_ready(100)
            .into_iter()
            .map(|delivery| delivery.payload)
            .collect::<Vec<_>>(),
        vec![1]
    );
}

#[test]
fn seeded_probabilistic_faults_are_reproducible() {
    let profile = AdversarialProfile {
        base_delay_us: 10_000,
        jitter_us: 5_000,
        drop_ppm: 100_000,
        duplicate_ppm: 200_000,
        reorder_extra_delay_us: 50_000,
    };
    let run = || {
        let mut link = AdversarialLink::new(profile, 0x1234_5678);
        for sequence in 0_u64..100 {
            link.send(sequence * 1_000, sequence);
        }
        link.drain_ready(u64::MAX)
    };
    let first = run();
    assert_eq!(first, run());

    let delivered = first
        .iter()
        .map(|delivery| delivery.payload)
        .collect::<Vec<_>>();
    let unique = delivered.iter().copied().collect::<HashSet<_>>();
    assert!(
        unique.len() < 100,
        "the pinned probabilistic trace must actually drop at least one request"
    );
    assert!(
        unique.len() < delivered.len(),
        "the pinned probabilistic trace must actually duplicate at least one request"
    );
    assert!(
        delivered.windows(2).any(|pair| pair[0] > pair[1]),
        "the pinned probabilistic trace must actually reorder at least one request"
    );
}

#[test]
fn four_tcp_profiles_keep_multi_node_room_replicas_ordered_under_backpressure() {
    const REVISIONS: u64 = 96;
    const SEND_INTERVAL_US: u64 = 2_000;
    const SNAPSHOT_BYTES: usize = 16 * 1024;

    let actors = vec![
        ActorId::Player(PlayerId(1)),
        ActorId::Player(PlayerId(2)),
        ActorId::Spectator(SpectatorId(1)),
    ];
    let mut profile_completion_us = Vec::new();

    for (profile_index, profile) in [
        TcpProfile::PERFECT,
        TcpProfile::LAN,
        TcpProfile::WAN,
        TcpProfile::POOR_WAN,
    ]
    .into_iter()
    .enumerate()
    {
        let name = profile_name(profile);
        let mut links = actors
            .iter()
            .enumerate()
            .map(|(node, _)| {
                TcpLikeLink::new(
                    profile,
                    0x7461_696b_6f00_0000_u64 + (profile_index as u64 * 16) + node as u64,
                )
            })
            .collect::<Vec<_>>();
        let mut command_sequences = [0_u64; 3];
        let mut completion_us = 0_u64;

        for revision in 1..=REVISIONS {
            let command_node = usize::try_from((revision - 1) % actors.len() as u64)
                .expect("node index fits usize");
            command_sequences[command_node] += 1;
            let now_us = revision * SEND_INTERVAL_US;

            completion_us = completion_us.max(links[command_node].send(
                now_us,
                256,
                ReplicatedFrame::CommandAck(applied_ack(command_sequences[command_node], revision)),
            ));
            for link in &mut links {
                completion_us = completion_us.max(link.send(
                    now_us,
                    SNAPSHOT_BYTES,
                    ReplicatedFrame::Snapshot {
                        revision: RoomRevision(revision),
                        actors: actors.clone(),
                    },
                ));
            }
        }

        let mut replicas = actors
            .iter()
            .cloned()
            .map(RoomReplica::new)
            .collect::<Vec<_>>();
        for (node, (link, replica)) in links.iter_mut().zip(&mut replicas).enumerate() {
            let deliveries = link.drain_ready(completion_us);
            assert_eq!(
                deliveries
                    .windows(2)
                    .filter(|pair| pair[0].at_us > pair[1].at_us)
                    .count(),
                0,
                "{name} node {node}: TCP trace reordered application frames"
            );
            for delivery in deliveries {
                replica.apply(delivery.payload, name, node);
            }
            assert!(
                link.is_empty(),
                "{name} node {node}: trace did not fully drain"
            );
            assert_eq!(
                replica.last_revision,
                RoomRevision(REVISIONS),
                "{name} node {node}: replica did not converge"
            );
            assert!(
                replica.pending_applied_revision.is_none(),
                "{name} node {node}: final applied revision has no matching snapshot"
            );
            assert_eq!(
                replica.last_command_seq.0, command_sequences[node],
                "{name} node {node}: actor command identity changed or an ack was lost"
            );
        }
        profile_completion_us.push((name, completion_us));
    }

    assert!(
        profile_completion_us
            .windows(2)
            .all(|pair| pair[0].1 < pair[1].1),
        "expected progressively slower completion under deterministic profiles, got \
         {profile_completion_us:?}"
    );
    let offered_end_us = REVISIONS * SEND_INTERVAL_US;
    assert!(
        profile_completion_us.last().expect("poor WAN result").1 > offered_end_us + 1_000_000,
        "poor WAN must build observable serialization/retransmission backpressure: \
         {profile_completion_us:?}"
    );
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum RetryRequest {
    Command {
        actor_id: ActorId,
        seq: CommandSeq,
        fingerprint: u64,
    },
    Input {
        actor_id: ActorId,
        match_id: MatchId,
        events: Vec<(InputSeq, u64)>,
    },
}

#[derive(Debug)]
struct SequenceModel {
    actor_id: ActorId,
    match_id: MatchId,
    revision: RoomRevision,
    next_command_seq: CommandSeq,
    command_history: BTreeMap<u64, (u64, CommandAck)>,
    next_input_seq: InputSeq,
    input_history: BTreeMap<u64, u64>,
    applied_command_sequences: Vec<CommandSeq>,
    applied_input_sequences: Vec<InputSeq>,
}

impl SequenceModel {
    fn new(actor_id: ActorId, match_id: MatchId) -> Self {
        Self {
            actor_id,
            match_id,
            revision: RoomRevision(0),
            next_command_seq: FIRST_COMMAND_SEQ,
            command_history: BTreeMap::new(),
            next_input_seq: FIRST_INPUT_SEQ,
            input_history: BTreeMap::new(),
            applied_command_sequences: Vec::new(),
            applied_input_sequences: Vec::new(),
        }
    }

    fn process(&mut self, request: RetryRequest) -> ModelReply {
        match request {
            RetryRequest::Command {
                actor_id,
                seq,
                fingerprint,
            } => {
                assert_eq!(
                    actor_id, self.actor_id,
                    "retry crossed actors instead of transports"
                );
                if seq < self.next_command_seq {
                    let Some((recorded_fingerprint, ack)) = self.command_history.get(&seq.0) else {
                        return ModelReply::Command(rejected_command_ack(
                            seq,
                            self.next_command_seq,
                            ProtocolErrorCode::SequenceGap,
                            false,
                        ));
                    };
                    if *recorded_fingerprint == fingerprint {
                        return ModelReply::Command(ack.clone());
                    }
                    return ModelReply::Command(rejected_command_ack(
                        seq,
                        self.next_command_seq,
                        ProtocolErrorCode::InvalidMessage,
                        false,
                    ));
                }
                if seq > self.next_command_seq {
                    return ModelReply::Command(rejected_command_ack(
                        seq,
                        self.next_command_seq,
                        ProtocolErrorCode::SequenceGap,
                        true,
                    ));
                }

                self.revision = RoomRevision(self.revision.0 + 1);
                self.next_command_seq = CommandSeq(seq.0 + 1);
                let ack = applied_ack(seq.0, self.revision.0);
                self.command_history
                    .insert(seq.0, (fingerprint, ack.clone()));
                self.applied_command_sequences.push(seq);
                ModelReply::Command(ack)
            }
            RetryRequest::Input {
                actor_id,
                match_id,
                events,
            } => {
                assert_eq!(
                    actor_id, self.actor_id,
                    "input retry crossed actors instead of transports"
                );
                if match_id != self.match_id {
                    return ModelReply::Input(rejected_input_ack(
                        match_id,
                        self.next_input_seq,
                        ProtocolErrorCode::StaleMatch,
                        false,
                    ));
                }
                if events.is_empty() {
                    return ModelReply::Input(rejected_input_ack(
                        match_id,
                        self.next_input_seq,
                        ProtocolErrorCode::InvalidInput,
                        false,
                    ));
                }

                let mut expected = self.next_input_seq;
                for (seq, fingerprint) in &events {
                    if *seq < self.next_input_seq {
                        if self.input_history.get(&seq.0) != Some(fingerprint) {
                            return ModelReply::Input(rejected_input_ack(
                                match_id,
                                self.next_input_seq,
                                ProtocolErrorCode::InvalidInput,
                                false,
                            ));
                        }
                        continue;
                    }
                    if *seq != expected {
                        return ModelReply::Input(rejected_input_ack(
                            match_id,
                            self.next_input_seq,
                            ProtocolErrorCode::SequenceGap,
                            true,
                        ));
                    }
                    expected = InputSeq(expected.0 + 1);
                }

                for (seq, fingerprint) in events {
                    if seq < self.next_input_seq {
                        continue;
                    }
                    self.input_history.insert(seq.0, fingerprint);
                    self.applied_input_sequences.push(seq);
                    self.next_input_seq = InputSeq(seq.0 + 1);
                }
                ModelReply::Input(InputAck {
                    match_id,
                    highest_contiguous_seq: Some(InputSeq(self.next_input_seq.0 - 1)),
                    next_expected_seq: self.next_input_seq,
                    server_tick: 0,
                    outcome: InputOutcome::Accepted,
                })
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ModelReply {
    Command(CommandAck),
    Input(InputAck),
}

fn protocol_error(code: ProtocolErrorCode, retryable: bool) -> ProtocolError {
    ProtocolError {
        code,
        message: ErrorMessage::new(format!("model rejection: {code:?}"))
            .expect("bounded model error"),
        retryable,
    }
}

fn rejected_command_ack(
    seq: CommandSeq,
    next_expected_seq: CommandSeq,
    code: ProtocolErrorCode,
    retryable: bool,
) -> CommandAck {
    CommandAck {
        seq,
        next_expected_seq,
        outcome: CommandOutcome::Rejected {
            error: protocol_error(code, retryable),
            current_room_revision: None,
        },
    }
}

fn rejected_input_ack(
    match_id: MatchId,
    next_expected_seq: InputSeq,
    code: ProtocolErrorCode,
    retryable: bool,
) -> InputAck {
    InputAck {
        match_id,
        highest_contiguous_seq: (next_expected_seq.0 > FIRST_INPUT_SEQ.0)
            .then(|| InputSeq(next_expected_seq.0 - 1)),
        next_expected_seq,
        server_tick: 0,
        outcome: InputOutcome::Rejected {
            error: protocol_error(code, retryable),
        },
    }
}

#[test]
fn reconnect_retries_converge_across_drop_duplicate_and_reorder_faults() {
    let actor_id = ActorId::Player(PlayerId(7));
    let match_id = MatchId(3);
    let mut model = SequenceModel::new(actor_id.clone(), match_id);

    let first_connection = [
        RetryRequest::Command {
            actor_id: actor_id.clone(),
            seq: CommandSeq(1),
            fingerprint: 0x1001,
        },
        RetryRequest::Input {
            actor_id: actor_id.clone(),
            match_id,
            events: vec![(InputSeq(1), 0x2001), (InputSeq(2), 0x2002)],
        },
    ];
    for request in first_connection {
        let reply = model.process(request);
        if let ModelReply::Input(ack) = &reply {
            ack.validate().expect("initial input ack is valid");
        }
    }
    assert_eq!(model.revision, RoomRevision(1));
    assert_eq!(model.next_input_seq, InputSeq(3));

    let mut resumed = AdversarialLink::new(AdversarialProfile::PERFECT, 0x7265_7375_6d65);
    resumed.script_fault(1, MessageFault::Drop);
    resumed.script_fault(
        3,
        MessageFault::Duplicate {
            duplicate_extra_us: 1,
        },
    );
    resumed.script_fault(5, MessageFault::Delay { extra_us: 50 });
    resumed.script_fault(6, MessageFault::Drop);
    resumed.script_fault(
        8,
        MessageFault::Duplicate {
            duplicate_extra_us: 1,
        },
    );
    resumed.script_fault(10, MessageFault::Delay { extra_us: 100 });

    let requests = [
        // Sequence two is lost before the server, so sequence three arrives as a gap.
        RetryRequest::Command {
            actor_id: actor_id.clone(),
            seq: CommandSeq(2),
            fingerprint: 0x1002,
        },
        RetryRequest::Command {
            actor_id: actor_id.clone(),
            seq: CommandSeq(3),
            fingerprint: 0x1003,
        },
        // Response loss on the old connection causes an exact replay, duplicated in transit.
        RetryRequest::Command {
            actor_id: actor_id.clone(),
            seq: CommandSeq(1),
            fingerprint: 0x1001,
        },
        RetryRequest::Command {
            actor_id: actor_id.clone(),
            seq: CommandSeq(2),
            fingerprint: 0x1002,
        },
        RetryRequest::Command {
            actor_id: actor_id.clone(),
            seq: CommandSeq(3),
            fingerprint: 0x1003,
        },
        RetryRequest::Input {
            actor_id: actor_id.clone(),
            match_id,
            events: vec![(InputSeq(3), 0x2003)],
        },
        RetryRequest::Input {
            actor_id: actor_id.clone(),
            match_id,
            events: vec![(InputSeq(4), 0x2004)],
        },
        RetryRequest::Input {
            actor_id: actor_id.clone(),
            match_id,
            events: vec![(InputSeq(1), 0x2001), (InputSeq(2), 0x2002)],
        },
        RetryRequest::Input {
            actor_id: actor_id.clone(),
            match_id,
            events: vec![(InputSeq(3), 0x2003)],
        },
        RetryRequest::Input {
            actor_id: actor_id.clone(),
            match_id,
            events: vec![(InputSeq(4), 0x2004)],
        },
    ];
    for (index, request) in requests.into_iter().enumerate() {
        let now_us = if matches!(index, 3 | 4 | 8 | 9) {
            20
        } else {
            0
        };
        resumed.send(now_us, request);
    }

    let mut replies = Vec::new();
    for delivery in resumed.drain_ready(u64::MAX) {
        let reply = model.process(delivery.payload);
        match &reply {
            ModelReply::Command(ack) => {
                assert!(
                    ack.next_expected_seq > ack.seq
                        || matches!(ack.outcome, CommandOutcome::Rejected { .. }),
                    "command ack cannot move the sequence window backwards: {ack:?}"
                );
            }
            ModelReply::Input(ack) => ack
                .validate()
                .unwrap_or_else(|error| panic!("invalid input ack {ack:?}: {error}")),
        }
        replies.push(reply);
    }

    assert_eq!(
        model.applied_command_sequences,
        vec![CommandSeq(1), CommandSeq(2), CommandSeq(3)],
        "dropped and duplicated requests must apply each command exactly once"
    );
    assert_eq!(
        model.applied_input_sequences,
        vec![InputSeq(1), InputSeq(2), InputSeq(3), InputSeq(4)],
        "reordered input retries must close the gap without double-scoring"
    );
    assert_eq!(model.revision, RoomRevision(3));
    assert_eq!(model.next_command_seq, CommandSeq(4));
    assert_eq!(model.next_input_seq, InputSeq(5));

    let sequence_gap_rejections = replies
        .iter()
        .filter(|reply| match reply {
            ModelReply::Command(CommandAck {
                outcome: CommandOutcome::Rejected { error, .. },
                ..
            })
            | ModelReply::Input(InputAck {
                outcome: InputOutcome::Rejected { error },
                ..
            }) => error.code == ProtocolErrorCode::SequenceGap && error.retryable,
            _ => false,
        })
        .count();
    assert!(
        sequence_gap_rejections >= 2,
        "both the command and input reorder must expose a retryable gap; replies={replies:?}"
    );

    let cached_command_one = replies
        .iter()
        .filter(|reply| {
            matches!(
                reply,
                ModelReply::Command(CommandAck {
                    seq: CommandSeq(1),
                    outcome: CommandOutcome::Applied {
                        room_revision: Some(RoomRevision(1))
                    },
                    ..
                })
            )
        })
        .count();
    assert_eq!(
        cached_command_one, 2,
        "duplicated replay should return the same cached applied result twice"
    );
}
