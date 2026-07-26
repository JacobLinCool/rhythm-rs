use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use taiko_multiplayer_protocol::{
    ActorId, ClientBuild, ClientCommand, ClientHello, ClientMessage, CommandAck, CommandEnvelope,
    CommandOutcome, CommandSeq, ContentHash, DisplayName, JoinRole, MembershipGranted,
    ProtocolErrorCode, ResumeRequest, RoomRevision, RoomSnapshot, ServerMessage, ServerWelcome,
    FIRST_COMMAND_SEQ, MAX_PLAYERS, PROTOCOL_VERSION, WIRE_SCHEMA_SHA256,
};
use taiko_resource_server::{start_server_background_controlled, ServerArgs};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_tungstenite::{connect_async, tungstenite::Message, MaybeTlsStream, WebSocketStream};

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

static NEXT_FIXTURE_ID: AtomicU64 = AtomicU64::new(1);

struct SongFixture {
    path: PathBuf,
}

impl SongFixture {
    fn create() -> Self {
        let path = std::env::temp_dir().join(format!(
            "taiko-multiplayer-loopback-{}-{}",
            std::process::id(),
            NEXT_FIXTURE_ID.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&path).expect("create isolated song fixture");
        std::fs::write(
            path.join("song.wav"),
            include_bytes!("../../taiko-game/assets/don.wav"),
        )
        .expect("write fixture audio");
        std::fs::write(
            path.join("song.tja"),
            concat!(
                "TITLE:Loopback\n",
                "BPM:120\n",
                "WAVE:song.wav\n",
                "COURSE:Oni\n",
                "LEVEL:1\n",
                "#START\n",
                "1,\n",
                "#END\n",
            ),
        )
        .expect("write fixture chart");
        Self { path }
    }
}

impl Drop for SongFixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.path).expect("remove isolated song fixture");
    }
}

async fn connect(url: &str) -> Socket {
    let (socket, response) = connect_async(url).await.expect("connect websocket");
    assert_eq!(response.status(), 101);
    socket
}

async fn http_status(address: std::net::SocketAddr, path: &str) -> u16 {
    let mut stream = TcpStream::connect(address)
        .await
        .expect("connect HTTP socket");
    let request = format!("GET {path} HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\n\r\n");
    stream
        .write_all(request.as_bytes())
        .await
        .expect("write HTTP request");
    let mut response = Vec::new();
    stream
        .read_to_end(&mut response)
        .await
        .expect("read HTTP response");
    let status_line = std::str::from_utf8(&response)
        .expect("HTTP response is text")
        .lines()
        .next()
        .expect("HTTP response has a status line");
    status_line
        .split_whitespace()
        .nth(1)
        .expect("HTTP status line has a status")
        .parse()
        .expect("HTTP status is numeric")
}

fn hello(name: &str, resume: Option<ResumeRequest>) -> ClientMessage {
    ClientMessage::Hello(ClientHello {
        protocol_version: PROTOCOL_VERSION,
        wire_schema_sha256: ContentHash::parse(WIRE_SCHEMA_SHA256).expect("wire schema hash"),
        client_build: ClientBuild::new("loopback-integration-test").expect("client build"),
        display_name: DisplayName::new(name).expect("display name"),
        resume,
    })
}

async fn send(socket: &mut Socket, message: ClientMessage) {
    let raw = serde_json::to_string(&message).expect("encode client message");
    socket
        .send(Message::Text(raw.into()))
        .await
        .expect("send client message");
}

async fn close_client(mut socket: Socket) {
    let _ = socket.send(Message::Close(None)).await;
    let _ = tokio::time::timeout(Duration::from_secs(1), async {
        while let Some(frame) = socket.next().await {
            if matches!(frame, Ok(Message::Close(_))) {
                break;
            }
        }
    })
    .await;
}

async fn receive_matching(
    socket: &mut Socket,
    mut predicate: impl FnMut(&ServerMessage) -> bool,
) -> ServerMessage {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let frame = socket
                .next()
                .await
                .expect("server keeps websocket open")
                .expect("receive websocket frame");
            let Message::Text(raw) = frame else {
                if matches!(frame, Message::Close(_)) {
                    panic!("server closed websocket before expected message");
                }
                continue;
            };
            let message =
                serde_json::from_str::<ServerMessage>(&raw).expect("decode server message");
            if let ServerMessage::Fatal(error) = &message {
                panic!("server returned fatal protocol error: {error:?}");
            }
            if predicate(&message) {
                return message;
            }
        }
    })
    .await
    .expect("expected server message within timeout")
}

async fn establish(
    socket: &mut Socket,
    name: &str,
    resume: Option<ResumeRequest>,
) -> ServerWelcome {
    send(socket, hello(name, resume)).await;
    let welcome = receive_matching(socket, |message| {
        matches!(message, ServerMessage::Welcome(_))
    })
    .await;
    let ServerMessage::Welcome(welcome) = welcome else {
        unreachable!("predicate guarantees welcome");
    };
    assert_eq!(welcome.protocol_version, PROTOCOL_VERSION);
    assert_eq!(welcome.wire_schema_sha256.as_str(), WIRE_SCHEMA_SHA256);
    welcome
}

async fn membership(socket: &mut Socket) -> MembershipGranted {
    let message = receive_matching(socket, |message| {
        matches!(message, ServerMessage::MembershipGranted(_))
    })
    .await;
    let ServerMessage::MembershipGranted(granted) = message else {
        unreachable!("predicate guarantees membership");
    };
    granted
}

async fn command_ack(socket: &mut Socket, seq: CommandSeq) -> CommandAck {
    let message = receive_matching(
        socket,
        |message| matches!(message, ServerMessage::CommandAck(ack) if ack.seq == seq),
    )
    .await;
    let ServerMessage::CommandAck(ack) = message else {
        unreachable!("predicate guarantees command ack");
    };
    ack
}

async fn snapshot_with(socket: &mut Socket, players: usize, spectators: usize) -> RoomSnapshot {
    let message = receive_matching(socket, |message| {
        matches!(
            message,
            ServerMessage::RoomSnapshot(snapshot)
                if snapshot.players.len() == players
                    && snapshot.spectators.len() == spectators
        )
    })
    .await;
    let ServerMessage::RoomSnapshot(snapshot) = message else {
        unreachable!("predicate guarantees snapshot");
    };
    snapshot.validate().expect("server snapshot is valid");
    *snapshot
}

fn applied_revision(ack: &CommandAck) -> RoomRevision {
    assert_eq!(
        ack.next_expected_seq,
        CommandSeq(ack.seq.0 + 1),
        "applied command ack must advance exactly one sequence"
    );
    let CommandOutcome::Applied {
        room_revision: Some(revision),
    } = ack.outcome
    else {
        panic!("expected applied command acknowledgement, got {ack:?}");
    };
    revision
}

fn actor_set(snapshot: &RoomSnapshot) -> HashSet<ActorId> {
    snapshot
        .players
        .iter()
        .map(|player| ActorId::Player(player.player_id))
        .chain(
            snapshot
                .spectators
                .iter()
                .map(|spectator| ActorId::Spectator(spectator.spectator_id)),
        )
        .collect()
}

fn join_command(granted: &MembershipGranted, role: JoinRole) -> ClientMessage {
    ClientMessage::Command(CommandEnvelope {
        seq: FIRST_COMMAND_SEQ,
        expected_room_revision: None,
        command: ClientCommand::JoinRoom {
            room_code: granted.room_code.clone(),
            invitation_token: granted.invitation_token.clone(),
            role,
        },
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn multiplayer_v2_routes_do_not_retain_v1_aliases() {
    let fixture = SongFixture::create();
    let (address, server, shutdown) = start_server_background_controlled(ServerArgs {
        songdir: fixture.path.clone(),
        host: "127.0.0.1".to_owned(),
        port: 0,
    })
    .await
    .expect("start loopback server");

    assert_eq!(http_status(address, "/v2/multiplayer/healthz").await, 200);
    assert_eq!(http_status(address, "/v1/multiplayer/healthz").await, 404);
    assert_eq!(http_status(address, "/v1/multiplayer/ws").await, 404);

    shutdown.shutdown();
    tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .expect("server shuts down promptly")
        .expect("server task joins")
        .expect("server exits cleanly");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn real_websocket_capacity_resume_and_mixed_roles_preserve_room_identity() {
    let fixture = SongFixture::create();
    let (address, server, shutdown) = start_server_background_controlled(ServerArgs {
        songdir: fixture.path.clone(),
        host: "127.0.0.1".to_owned(),
        port: 0,
    })
    .await
    .expect("start loopback server");
    let url = format!("ws://{address}/v2/multiplayer/ws");

    let mut host = connect(&url).await;
    let host_welcome = establish(&mut host, "host", None).await;
    assert!(!host_welcome.resumed);
    assert_eq!(host_welcome.next_expected_command_seq, FIRST_COMMAND_SEQ);
    send(
        &mut host,
        ClientMessage::Command(CommandEnvelope {
            seq: FIRST_COMMAND_SEQ,
            expected_room_revision: None,
            command: ClientCommand::CreateRoom,
        }),
    )
    .await;
    let host_membership = membership(&mut host).await;
    assert!(matches!(host_membership.actor_id, ActorId::Player(_)));
    let host_ack = command_ack(&mut host, FIRST_COMMAND_SEQ).await;
    assert_eq!(applied_revision(&host_ack), RoomRevision(1));
    let host_snapshot = snapshot_with(&mut host, 1, 0).await;
    assert_eq!(host_snapshot.revision, RoomRevision(1));

    let mut guests = Vec::new();
    for guest_number in 2..=MAX_PLAYERS {
        let mut guest = connect(&url).await;
        let guest_name = format!("guest-{guest_number}");
        let welcome = establish(&mut guest, &guest_name, None).await;
        assert!(!welcome.resumed);
        send(&mut guest, join_command(&host_membership, JoinRole::Player)).await;
        let granted = membership(&mut guest).await;
        assert!(matches!(granted.actor_id, ActorId::Player(_)));
        let ack = command_ack(&mut guest, FIRST_COMMAND_SEQ).await;
        let expected_revision =
            RoomRevision(u64::try_from(guest_number).expect("player count fits u64"));
        assert_eq!(
            applied_revision(&ack),
            expected_revision,
            "player {guest_number} join applied at an unexpected revision"
        );
        let snapshot = snapshot_with(&mut guest, guest_number, 0).await;
        assert_eq!(snapshot.revision, expected_revision);
        guests.push((guest, granted, ack, snapshot));
    }

    let player_ids = std::iter::once(&host_membership)
        .chain(guests.iter().map(|(_, granted, _, _)| granted))
        .map(|granted| granted.actor_id.clone())
        .collect::<HashSet<_>>();
    assert_eq!(
        player_ids.len(),
        MAX_PLAYERS,
        "every admitted player must receive a unique actor identity"
    );

    let mut spectator = connect(&url).await;
    establish(&mut spectator, "spectator", None).await;
    send(
        &mut spectator,
        join_command(&host_membership, JoinRole::Spectator),
    )
    .await;
    let spectator_membership = membership(&mut spectator).await;
    assert!(matches!(
        spectator_membership.actor_id,
        ActorId::Spectator(_)
    ));
    let spectator_ack = command_ack(&mut spectator, FIRST_COMMAND_SEQ).await;
    let spectator_revision = applied_revision(&spectator_ack);
    assert_eq!(
        spectator_revision,
        RoomRevision(u64::try_from(MAX_PLAYERS + 1).expect("capacity fits u64"))
    );
    let host_full_snapshot = snapshot_with(&mut host, MAX_PLAYERS, 1).await;
    let spectator_snapshot = snapshot_with(&mut spectator, MAX_PLAYERS, 1).await;
    assert_eq!(host_full_snapshot.revision, spectator_revision);
    assert_eq!(spectator_snapshot.revision, spectator_revision);
    assert_eq!(
        actor_set(&host_full_snapshot),
        actor_set(&spectator_snapshot),
        "player and spectator replicas must converge on the same actor identities"
    );
    assert!(actor_set(&spectator_snapshot).contains(&spectator_membership.actor_id));

    let mut overflow = connect(&url).await;
    establish(&mut overflow, "overflow-player", None).await;
    let overflow_join = join_command(&host_membership, JoinRole::Player);
    send(&mut overflow, overflow_join.clone()).await;
    let overflow_ack = command_ack(&mut overflow, FIRST_COMMAND_SEQ).await;
    assert_eq!(overflow_ack.next_expected_seq, CommandSeq(2));
    let CommandOutcome::Rejected {
        error,
        current_room_revision,
    } = &overflow_ack.outcome
    else {
        panic!("fifth player unexpectedly joined: {overflow_ack:?}");
    };
    assert_eq!(error.code, ProtocolErrorCode::RoomFull);
    assert!(!error.retryable);
    assert_eq!(*current_room_revision, None);

    send(&mut overflow, overflow_join).await;
    let duplicate_overflow_ack = command_ack(&mut overflow, FIRST_COMMAND_SEQ).await;
    assert_eq!(
        duplicate_overflow_ack, overflow_ack,
        "retrying the capacity-rejected admission must return the cached typed result"
    );

    let (guest, guest_membership, guest_join_ack, joined_snapshot) = guests.remove(0);
    close_client(guest).await;

    let mut resumed_guest = connect(&url).await;
    let resume_welcome = establish(
        &mut resumed_guest,
        "guest-2",
        Some(ResumeRequest {
            room_code: guest_membership.room_code.clone(),
            actor_id: guest_membership.actor_id.clone(),
            token: guest_membership.resume_token.clone(),
            last_room_revision: joined_snapshot.revision,
            last_acked_command_seq: CommandSeq(1),
        }),
    )
    .await;
    assert!(resume_welcome.resumed);
    assert_eq!(resume_welcome.next_expected_command_seq, CommandSeq(2));
    let resumed_membership = membership(&mut resumed_guest).await;
    assert_eq!(resumed_membership.actor_id, guest_membership.actor_id);
    assert_eq!(
        resumed_membership.resume_token,
        guest_membership.resume_token
    );
    let resumed_snapshot = snapshot_with(&mut resumed_guest, MAX_PLAYERS, 1).await;
    assert!(
        resumed_snapshot.revision > spectator_revision,
        "disconnect and resume connection-state transitions must advance room revision"
    );
    assert_eq!(
        actor_set(&resumed_snapshot),
        actor_set(&spectator_snapshot),
        "resume must restore the same actor rather than adding a replacement identity"
    );
    assert_eq!(
        resumed_snapshot
            .players
            .iter()
            .filter(|player| player.is_leader)
            .count(),
        1,
        "room must retain exactly one leader after resume"
    );
    send(
        &mut resumed_guest,
        join_command(&host_membership, JoinRole::Player),
    )
    .await;
    let replayed_join_ack = command_ack(&mut resumed_guest, FIRST_COMMAND_SEQ).await;
    assert_eq!(
        replayed_join_ack, guest_join_ack,
        "an admission replay on the resumed transport must return the original applied revision"
    );

    close_client(overflow).await;
    for (guest, _, _, _) in guests {
        close_client(guest).await;
    }
    close_client(spectator).await;
    close_client(resumed_guest).await;
    close_client(host).await;
    shutdown.shutdown();
    tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .expect("server shuts down promptly")
        .expect("server task joins")
        .expect("server exits cleanly");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn schema_mismatch_emits_exactly_one_fatal_before_close() {
    let fixture = SongFixture::create();
    let (address, server, shutdown) = start_server_background_controlled(ServerArgs {
        songdir: fixture.path.clone(),
        host: "127.0.0.1".to_owned(),
        port: 0,
    })
    .await
    .expect("start loopback server");
    let url = format!("ws://{address}/v2/multiplayer/ws");
    let mut socket = connect(&url).await;

    let mut incompatible = hello("incompatible-client", None);
    let ClientMessage::Hello(client_hello) = &mut incompatible else {
        unreachable!("hello helper always returns a hello");
    };
    client_hello.wire_schema_sha256 =
        ContentHash::parse("0".repeat(64)).expect("different valid content hash");
    send(&mut socket, incompatible).await;

    let observed = tokio::time::timeout(Duration::from_secs(5), async {
        let mut messages = Vec::new();
        loop {
            let frame = socket
                .next()
                .await
                .expect("server sends a close frame")
                .expect("terminal websocket frame is valid");
            match frame {
                Message::Text(raw) => {
                    messages.push(
                        serde_json::from_str::<ServerMessage>(&raw)
                            .expect("terminal text is a protocol message"),
                    );
                }
                Message::Close(_) => break,
                Message::Ping(_) | Message::Pong(_) => {}
                other => panic!("unexpected terminal frame before close: {other:?}"),
            }
        }
        messages
    })
    .await
    .expect("schema rejection reaches close promptly");

    assert_eq!(
        observed.len(),
        1,
        "terminal delivery must contain exactly one message before close: {observed:?}"
    );
    let ServerMessage::Fatal(error) = &observed[0] else {
        panic!("terminal message must be Fatal, got {:?}", observed[0]);
    };
    assert_eq!(error.code, ProtocolErrorCode::UnsupportedProtocol);
    assert!(!error.retryable);

    shutdown.shutdown();
    tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .expect("server shuts down promptly")
        .expect("server task joins")
        .expect("server exits cleanly");
}
