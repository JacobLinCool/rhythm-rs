mod protocol;
mod server;

use std::net::{IpAddr, Ipv4Addr, UdpSocket};
use std::sync::mpsc::{Receiver, TryRecvError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
#[cfg(unix)]
use std::{ffi::CStr, ptr};

use anyhow::{anyhow, Context, Result};
use tokio::sync::{oneshot, watch};

use crate::controller::{ControllerSlot, ControllerStrike};
use crate::preferences::UiLanguage;

pub(crate) const MAX_DRAIN_PER_SLOT_PER_TICK: usize = 64;
pub(crate) const MAX_DISPATCH_AGE: Duration = Duration::from_millis(100);
pub(super) const PER_SLOT_QUEUE_CAPACITY: usize = 128;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ActiveControllerSlots(u8);

impl ActiveControllerSlots {
    pub(crate) const NONE: Self = Self(0);
    pub(crate) const ONE: Self = Self(1);
    #[cfg(test)]
    pub(crate) const TWO: Self = Self(2);
    pub(crate) const BOTH: Self = Self(3);

    pub(crate) const fn contains(self, slot: ControllerSlot) -> bool {
        self.0 & (1 << slot.index()) != 0
    }

    pub(super) const fn bits(self) -> u8 {
        self.0
    }

    pub(super) const fn from_bits(bits: u8) -> Self {
        Self(bits & Self::BOTH.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LanControllerConfig {
    pub(crate) bind_ip: IpAddr,
    pub(crate) generation: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ControllerSlotStatus {
    pub(crate) paired: bool,
    pub(crate) connected: bool,
    pub(crate) accepted_hits: u64,
    pub(crate) rejected_hits: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ControllerIngressSnapshot {
    pub(crate) in_flight: usize,
    pub(crate) completed: u64,
}

pub(crate) struct DrainedControllerInputs {
    pub(crate) strikes: [Vec<ControllerStrike>; 2],
    pub(crate) saturated: bool,
}

impl ControllerIngressSnapshot {
    pub(crate) const fn is_stable_since(self, earlier: Self) -> bool {
        self.in_flight == 0 && self.completed == earlier.completed
    }
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct PairingInvite {
    url: String,
}

impl PairingInvite {
    fn new(endpoint: &str, token: &str, language: UiLanguage) -> Self {
        Self {
            url: format!(
                "{endpoint}/controller/?lang={}#token={token}",
                language.web_language_tag()
            ),
        }
    }

    pub(crate) fn expose(&self) -> &str {
        &self.url
    }
}

impl std::fmt::Debug for PairingInvite {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("PairingInvite([REDACTED])")
    }
}

pub(crate) struct LanControllers {
    endpoint: String,
    control: server::ServerControl,
    input_receivers: [Receiver<ControllerStrike>; 2],
    shutdown: Option<oneshot::Sender<()>>,
    shutdown_broadcast: watch::Sender<bool>,
    thread: Option<JoinHandle<Result<()>>>,
}

impl LanControllers {
    pub(crate) fn start(config: LanControllerConfig) -> Result<Self> {
        server::start(config)
    }

    pub(crate) fn endpoint(&self) -> String {
        self.endpoint.clone()
    }

    pub(crate) fn pairing_invite(
        &self,
        slot: ControllerSlot,
        language: UiLanguage,
    ) -> Option<PairingInvite> {
        let token = self.control.pairing_token(slot)?;
        Some(PairingInvite::new(&self.endpoint, &token, language))
    }

    pub(crate) fn rotate_pairing(&self, slot: ControllerSlot) -> Result<()> {
        self.control.rotate_pairing(slot)?;
        Ok(())
    }

    pub(crate) fn set_active_slots(&self, slots: ActiveControllerSlots) {
        self.control.set_active_slots(slots);
    }

    pub(crate) fn status(&self) -> [ControllerSlotStatus; 2] {
        self.control.status()
    }

    pub(crate) fn ingress_snapshot(&self) -> ControllerIngressSnapshot {
        let snapshot = self.control.ingress_snapshot();
        ControllerIngressSnapshot {
            in_flight: snapshot.in_flight,
            completed: snapshot.completed,
        }
    }

    #[cfg(test)]
    pub(crate) fn begin_test_in_flight_frame(&self) -> server::FrameIngressGuard {
        self.control.begin_frame_ingress()
    }

    #[cfg(test)]
    pub(crate) fn test_slot_is_active(&self, slot: ControllerSlot) -> bool {
        self.control.is_active(slot)
    }

    pub(crate) fn drain_inputs(
        &mut self,
        dispatch_slots: ActiveControllerSlots,
    ) -> DrainedControllerInputs {
        let now = Instant::now();
        let mut saturated = false;
        let strikes = std::array::from_fn(|index| {
            let slot = ControllerSlot::ALL[index];
            let mut drained = Vec::with_capacity(MAX_DRAIN_PER_SLOT_PER_TICK);
            let mut received = 0;
            for _ in 0..MAX_DRAIN_PER_SLOT_PER_TICK {
                let strike = match self.input_receivers[index].try_recv() {
                    Ok(strike) => strike,
                    Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
                };
                received += 1;
                if self.control.authorize_dispatch(
                    &strike,
                    slot,
                    dispatch_slots,
                    now,
                    MAX_DISPATCH_AGE,
                ) {
                    drained.push(strike);
                }
            }
            saturated |= received == MAX_DRAIN_PER_SLOT_PER_TICK;
            drained
        });
        DrainedControllerInputs { strikes, saturated }
    }

    pub(crate) fn shutdown_and_join(mut self) -> Result<()> {
        self.shutdown_inner()
    }

    fn shutdown_inner(&mut self) -> Result<()> {
        self.control.stop();
        self.shutdown_broadcast.send_replace(true);
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        let Some(thread) = self.thread.take() else {
            return Ok(());
        };
        thread
            .join()
            .map_err(|_| anyhow!("LAN controller thread panicked"))?
            .context("LAN controller stopped with an error")
    }
}

impl Drop for LanControllers {
    fn drop(&mut self) {
        let _ = self.shutdown_inner();
    }
}

pub(crate) fn discover_lan_ip() -> Option<IpAddr> {
    let candidates = system_interface_candidates();
    select_lan_ipv4(candidates, route_probe_ipv4()).map(IpAddr::V4)
}

fn route_probe_ipv4() -> Option<Ipv4Addr> {
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).ok()?;
    // UDP connect selects a route and local interface without transmitting a
    // datagram. It is only a preference among already-safe interface
    // candidates, so a VPN route can never become the bind address by itself.
    socket.connect((Ipv4Addr::new(192, 0, 2, 1), 9)).ok()?;
    match socket.local_addr().ok()?.ip() {
        IpAddr::V4(address) => Some(address),
        IpAddr::V6(_) => None,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct LanInterfaceCandidate {
    name: String,
    address: Ipv4Addr,
    is_up: bool,
    is_loopback: bool,
    is_point_to_point: bool,
    supports_broadcast: bool,
}

fn select_lan_ipv4(
    mut candidates: Vec<LanInterfaceCandidate>,
    route_hint: Option<Ipv4Addr>,
) -> Option<Ipv4Addr> {
    candidates.retain(|candidate| {
        candidate.is_up
            && candidate.supports_broadcast
            && !candidate.is_loopback
            && !candidate.is_point_to_point
            && !is_known_virtual_interface(&candidate.name)
            && !candidate.address.is_unspecified()
            && !candidate.address.is_loopback()
            && !candidate.address.is_multicast()
            && !candidate.address.is_broadcast()
            && !candidate.address.is_link_local()
            && is_local_network_ipv4(candidate.address)
            && is_likely_physical_interface(&candidate.name)
    });
    candidates.sort_by_key(|candidate| {
        (
            route_hint != Some(candidate.address),
            candidate.name.clone(),
            candidate.address.octets(),
        )
    });
    candidates.first().map(|candidate| candidate.address)
}

fn is_known_virtual_interface(name: &str) -> bool {
    [
        "awdl",
        "bridge",
        "docker",
        "gif",
        "llw",
        "p2p",
        "stf",
        "tap",
        "tailscale",
        "tun",
        "utun",
        "vpn",
        "vboxnet",
        "veth",
        "virbr",
        "vmnet",
        "wg",
        "zt",
        "br",
        "br-",
    ]
    .into_iter()
    .any(|prefix| name.starts_with(prefix))
}

fn is_local_network_ipv4(address: Ipv4Addr) -> bool {
    let [first, second, _, _] = address.octets();
    address.is_private() || (first == 100 && (64..=127).contains(&second))
}

fn is_likely_physical_interface(name: &str) -> bool {
    ["en", "eth", "wl"]
        .into_iter()
        .any(|prefix| name.starts_with(prefix))
}

#[cfg(unix)]
fn system_interface_candidates() -> Vec<LanInterfaceCandidate> {
    struct IfAddrs(*mut libc::ifaddrs);

    impl Drop for IfAddrs {
        fn drop(&mut self) {
            // SAFETY: `self.0` is the exact list allocated by `getifaddrs` and
            // this guard owns the single corresponding `freeifaddrs` call.
            unsafe { libc::freeifaddrs(self.0) };
        }
    }

    let mut head = ptr::null_mut();
    // SAFETY: `head` is a valid out-pointer. A successful call transfers one
    // linked-list allocation to the guard below.
    if unsafe { libc::getifaddrs(&mut head) } != 0 || head.is_null() {
        return Vec::new();
    }
    let _guard = IfAddrs(head);
    let mut candidates = Vec::new();
    let mut current = head;
    while !current.is_null() {
        // SAFETY: every node is part of the live list owned by `_guard`.
        let interface = unsafe { &*current };
        let address = interface.ifa_addr;
        if !address.is_null()
            // SAFETY: `ifa_addr` points to a sockaddr for this live node.
            && unsafe { (*address).sa_family as i32 } == libc::AF_INET
            && !interface.ifa_name.is_null()
        {
            // SAFETY: `ifa_name` is a NUL-terminated interface name owned by
            // the live getifaddrs list.
            let name = unsafe { CStr::from_ptr(interface.ifa_name) };
            if let Ok(name) = name.to_str() {
                // SAFETY: AF_INET above guarantees a sockaddr_in layout.
                let socket_address = unsafe { &*(address.cast::<libc::sockaddr_in>()) };
                let flags = interface.ifa_flags as libc::c_uint;
                candidates.push(LanInterfaceCandidate {
                    name: name.to_owned(),
                    address: Ipv4Addr::from(u32::from_be(socket_address.sin_addr.s_addr)),
                    is_up: flags & libc::IFF_UP as libc::c_uint != 0,
                    is_loopback: flags & libc::IFF_LOOPBACK as libc::c_uint != 0,
                    is_point_to_point: flags & libc::IFF_POINTOPOINT as libc::c_uint != 0,
                    supports_broadcast: flags & libc::IFF_BROADCAST as libc::c_uint != 0,
                });
            }
        }
        current = interface.ifa_next;
    }
    candidates
}

#[cfg(not(unix))]
fn system_interface_candidates() -> Vec<LanInterfaceCandidate> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn active_slot_masks_cover_p1_p2_independently() {
        assert!(!ActiveControllerSlots::NONE.contains(ControllerSlot::One));
        assert!(!ActiveControllerSlots::NONE.contains(ControllerSlot::Two));
        assert!(ActiveControllerSlots::ONE.contains(ControllerSlot::One));
        assert!(!ActiveControllerSlots::ONE.contains(ControllerSlot::Two));
        assert!(!ActiveControllerSlots::TWO.contains(ControllerSlot::One));
        assert!(ActiveControllerSlots::TWO.contains(ControllerSlot::Two));
        assert!(ActiveControllerSlots::BOTH.contains(ControllerSlot::One));
        assert!(ActiveControllerSlots::BOTH.contains(ControllerSlot::Two));
    }

    #[test]
    fn quiescent_snapshot_rejects_in_flight_or_concurrently_completed_frames() {
        let earlier = ControllerIngressSnapshot {
            in_flight: 0,
            completed: 41,
        };
        assert!(earlier.is_stable_since(earlier));
        assert!(!ControllerIngressSnapshot {
            in_flight: 1,
            completed: 41,
        }
        .is_stable_since(earlier));
        assert!(!ControllerIngressSnapshot {
            in_flight: 0,
            completed: 42,
        }
        .is_stable_since(earlier));
    }

    #[test]
    fn pairing_invite_debug_never_exposes_fragment_token() {
        let invite = PairingInvite::new("http://127.0.0.1:1", &"a".repeat(64), UiLanguage::English);
        let debug = format!("{invite:?}");
        assert_eq!(debug, "PairingInvite([REDACTED])");
        assert!(!debug.contains(&"a".repeat(64)));
        assert!(invite.expose().contains("#token="));
    }

    #[test]
    fn pairing_invite_localizes_with_query_and_keeps_token_in_fragment() {
        let token = "a".repeat(64);
        for language in UiLanguage::ALL {
            let invite = PairingInvite::new("http://192.0.2.10:1234", &token, language);
            let (request_url, fragment) = invite
                .expose()
                .split_once('#')
                .expect("pairing URL fragment");
            assert_eq!(
                request_url,
                format!(
                    "http://192.0.2.10:1234/controller/?lang={}",
                    language.web_language_tag()
                )
            );
            assert_eq!(fragment, format!("token={token}"));
            assert!(!request_url.contains(&token));
        }
    }

    fn candidate(
        name: &str,
        address: [u8; 4],
        is_loopback: bool,
        is_point_to_point: bool,
        supports_broadcast: bool,
    ) -> LanInterfaceCandidate {
        LanInterfaceCandidate {
            name: name.to_owned(),
            address: Ipv4Addr::from(address),
            is_up: true,
            is_loopback,
            is_point_to_point,
            supports_broadcast,
        }
    }

    #[test]
    fn physical_broadcast_lan_beats_point_to_point_vpn_route() {
        let wifi = Ipv4Addr::new(192, 168, 50, 82);
        let vpn = Ipv4Addr::new(172, 16, 0, 2);
        let selected = select_lan_ipv4(
            vec![
                candidate("utun4", vpn.octets(), false, true, false),
                candidate("en0", wifi.octets(), false, false, true),
            ],
            Some(vpn),
        );
        assert_eq!(selected, Some(wifi));
    }

    #[test]
    fn broadcast_vpn_or_bridge_route_never_beats_a_physical_lan() {
        let wifi = Ipv4Addr::new(192, 168, 1, 20);
        for (name, address) in [
            ("vpn0", Ipv4Addr::new(10, 8, 0, 2)),
            ("br0", Ipv4Addr::new(192, 168, 122, 1)),
        ] {
            let selected = select_lan_ipv4(
                vec![
                    candidate("en0", wifi.octets(), false, false, true),
                    candidate(name, address.octets(), false, false, true),
                ],
                Some(address),
            );
            assert_eq!(selected, Some(wifi), "{name} must not receive the bind");
        }
    }

    #[test]
    fn unknown_interface_names_require_an_explicit_bind_address() {
        assert_eq!(
            select_lan_ipv4(
                vec![candidate(
                    "mystery0",
                    Ipv4Addr::new(192, 168, 7, 3).octets(),
                    false,
                    false,
                    true,
                )],
                Some(Ipv4Addr::new(192, 168, 7, 3)),
            ),
            None
        );
    }

    #[test]
    fn route_hint_selects_the_active_physical_interface() {
        let wifi = Ipv4Addr::new(192, 168, 1, 20);
        let ethernet = Ipv4Addr::new(10, 0, 0, 20);
        let selected = select_lan_ipv4(
            vec![
                candidate("en0", wifi.octets(), false, false, true),
                candidate("en5", ethernet.octets(), false, false, true),
            ],
            Some(ethernet),
        );
        assert_eq!(selected, Some(ethernet));
    }

    #[test]
    fn loopback_only_environment_has_no_phone_reachable_default() {
        assert_eq!(
            select_lan_ipv4(
                vec![candidate(
                    "lo0",
                    Ipv4Addr::LOCALHOST.octets(),
                    true,
                    false,
                    false,
                )],
                Some(Ipv4Addr::LOCALHOST),
            ),
            None
        );
    }

    #[test]
    fn virtual_and_globally_routable_interfaces_are_never_automatic_bind_targets() {
        assert_eq!(
            select_lan_ipv4(
                vec![
                    candidate(
                        "bridge100",
                        Ipv4Addr::new(192, 168, 64, 1).octets(),
                        false,
                        false,
                        true,
                    ),
                    candidate(
                        "en0",
                        Ipv4Addr::new(203, 0, 113, 8).octets(),
                        false,
                        false,
                        true,
                    ),
                ],
                None,
            ),
            None
        );
    }
}
