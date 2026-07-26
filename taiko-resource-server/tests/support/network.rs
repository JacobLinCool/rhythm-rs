use std::collections::{BTreeMap, VecDeque};

const MICROS_PER_SECOND: u64 = 1_000_000;
const PPM_SCALE: u32 = 1_000_000;
const TCP_SEGMENT_BYTES: usize = 1_200;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TcpProfile {
    pub one_way_latency_us: u64,
    pub jitter_us: u64,
    pub bandwidth_bytes_per_second: u64,
    pub packet_loss_ppm: u32,
    pub retransmit_timeout_us: u64,
    pub stall_every_messages: Option<u64>,
    pub stall_us: u64,
}

impl TcpProfile {
    pub const PERFECT: Self = Self {
        one_way_latency_us: 0,
        jitter_us: 0,
        bandwidth_bytes_per_second: u64::MAX,
        packet_loss_ppm: 0,
        retransmit_timeout_us: 0,
        stall_every_messages: None,
        stall_us: 0,
    };

    pub const LAN: Self = Self {
        one_way_latency_us: 2_000,
        jitter_us: 1_000,
        bandwidth_bytes_per_second: 100 * 1024 * 1024,
        packet_loss_ppm: 100,
        retransmit_timeout_us: 20_000,
        stall_every_messages: None,
        stall_us: 0,
    };

    pub const WAN: Self = Self {
        one_way_latency_us: 40_000,
        jitter_us: 15_000,
        bandwidth_bytes_per_second: 10 * 1024 * 1024,
        packet_loss_ppm: 5_000,
        retransmit_timeout_us: 120_000,
        stall_every_messages: None,
        stall_us: 0,
    };

    pub const POOR_WAN: Self = Self {
        one_way_latency_us: 120_000,
        jitter_us: 80_000,
        bandwidth_bytes_per_second: 512 * 1024,
        packet_loss_ppm: 30_000,
        retransmit_timeout_us: 300_000,
        stall_every_messages: Some(17),
        stall_us: 750_000,
    };
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Delivery<T> {
    pub at_us: u64,
    pub payload: T,
}

/// Models the observable message timing of a TCP/WebSocket link.
///
/// Packet loss adds retransmission delay; it never drops, duplicates, or
/// reorders application messages. That distinction keeps network tests honest
/// about what real WebSockets can and cannot do.
pub struct TcpLikeLink<T> {
    profile: TcpProfile,
    rng: DeterministicRng,
    serialize_available_us: u64,
    last_delivery_us: u64,
    sent_messages: u64,
    queued: VecDeque<Delivery<T>>,
}

impl<T> TcpLikeLink<T> {
    pub fn new(profile: TcpProfile, seed: u64) -> Self {
        Self {
            profile,
            rng: DeterministicRng::new(seed),
            serialize_available_us: 0,
            last_delivery_us: 0,
            sent_messages: 0,
            queued: VecDeque::new(),
        }
    }

    pub fn send(&mut self, now_us: u64, encoded_len: usize, payload: T) -> u64 {
        self.sent_messages = self.sent_messages.saturating_add(1);

        let serialization_us = if self.profile.bandwidth_bytes_per_second == u64::MAX {
            0
        } else {
            ceil_div(
                (encoded_len as u64).saturating_mul(MICROS_PER_SECOND),
                self.profile.bandwidth_bytes_per_second.max(1),
            )
        };
        let serialization_start = now_us.max(self.serialize_available_us);
        self.serialize_available_us = serialization_start.saturating_add(serialization_us);

        let jitter = self.rng.symmetric(self.profile.jitter_us);
        let base_arrival = add_signed(
            self.serialize_available_us
                .saturating_add(self.profile.one_way_latency_us),
            jitter,
        );

        let segment_count = encoded_len.max(1).div_ceil(TCP_SEGMENT_BYTES);
        let lost_segments = (0..segment_count)
            .filter(|_| self.rng.chance(self.profile.packet_loss_ppm))
            .count() as u64;
        let retransmit_delay = lost_segments.saturating_mul(self.profile.retransmit_timeout_us);
        let periodic_stall = self
            .profile
            .stall_every_messages
            .filter(|interval| self.sent_messages.is_multiple_of(*interval))
            .map_or(0, |_| self.profile.stall_us);

        let candidate = base_arrival
            .saturating_add(retransmit_delay)
            .saturating_add(periodic_stall);
        let at_us = candidate.max(self.last_delivery_us);
        self.last_delivery_us = at_us;
        self.queued.push_back(Delivery { at_us, payload });
        at_us
    }

    pub fn drain_ready(&mut self, now_us: u64) -> Vec<Delivery<T>> {
        let ready = self
            .queued
            .partition_point(|delivery| delivery.at_us <= now_us);
        self.queued.drain(..ready).collect()
    }

    pub fn is_empty(&self) -> bool {
        self.queued.is_empty()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdversarialProfile {
    pub base_delay_us: u64,
    pub jitter_us: u64,
    pub drop_ppm: u32,
    pub duplicate_ppm: u32,
    pub reorder_extra_delay_us: u64,
}

impl AdversarialProfile {
    pub const PERFECT: Self = Self {
        base_delay_us: 0,
        jitter_us: 0,
        drop_ppm: 0,
        duplicate_ppm: 0,
        reorder_extra_delay_us: 0,
    };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageFault {
    Drop,
    Delay { extra_us: u64 },
    Duplicate { duplicate_extra_us: u64 },
}

#[derive(Debug)]
struct Scheduled<T> {
    at_us: u64,
    insertion_order: u64,
    payload: T,
}

/// A message-layer fault injector for retry/idempotency testing.
///
/// Unlike [`TcpLikeLink`], this deliberately can drop, duplicate, and reorder
/// logical requests in a scripted trace. It has no transport epoch and does
/// not prove stale-connection fencing; nor does it make claims about an
/// established WebSocket's delivery semantics.
pub struct AdversarialLink<T> {
    profile: AdversarialProfile,
    rng: DeterministicRng,
    send_index: u64,
    insertion_order: u64,
    scripted: BTreeMap<u64, MessageFault>,
    queued: Vec<Scheduled<T>>,
}

impl<T: Clone> AdversarialLink<T> {
    pub fn new(profile: AdversarialProfile, seed: u64) -> Self {
        Self {
            profile,
            rng: DeterministicRng::new(seed),
            send_index: 0,
            insertion_order: 0,
            scripted: BTreeMap::new(),
            queued: Vec::new(),
        }
    }

    pub fn script_fault(&mut self, send_index: u64, fault: MessageFault) {
        self.scripted.insert(send_index, fault);
    }

    pub fn send(&mut self, now_us: u64, payload: T) {
        self.send_index = self.send_index.saturating_add(1);
        let scripted = self.scripted.get(&self.send_index).copied();
        if matches!(scripted, Some(MessageFault::Drop))
            || (scripted.is_none() && self.rng.chance(self.profile.drop_ppm))
        {
            return;
        }

        let mut at_us = add_signed(
            now_us.saturating_add(self.profile.base_delay_us),
            self.rng.symmetric(self.profile.jitter_us),
        );
        if let Some(MessageFault::Delay { extra_us }) = scripted {
            at_us = at_us.saturating_add(extra_us);
        } else if scripted.is_none()
            && self.profile.reorder_extra_delay_us > 0
            && self.rng.chance(PPM_SCALE / 4)
        {
            at_us = at_us.saturating_add(self.profile.reorder_extra_delay_us);
        }
        self.schedule(at_us, payload.clone());

        let duplicate_delay = match scripted {
            Some(MessageFault::Duplicate { duplicate_extra_us }) => Some(duplicate_extra_us),
            None if self.rng.chance(self.profile.duplicate_ppm) => Some(1),
            _ => None,
        };
        if let Some(extra_us) = duplicate_delay {
            self.schedule(at_us.saturating_add(extra_us), payload);
        }
    }

    pub fn drain_ready(&mut self, now_us: u64) -> Vec<Delivery<T>> {
        self.queued
            .sort_by_key(|scheduled| (scheduled.at_us, scheduled.insertion_order));
        let ready = self
            .queued
            .partition_point(|scheduled| scheduled.at_us <= now_us);
        self.queued
            .drain(..ready)
            .map(|scheduled| Delivery {
                at_us: scheduled.at_us,
                payload: scheduled.payload,
            })
            .collect()
    }

    fn schedule(&mut self, at_us: u64, payload: T) {
        self.insertion_order = self.insertion_order.saturating_add(1);
        self.queued.push(Scheduled {
            at_us,
            insertion_order: self.insertion_order,
            payload,
        });
    }
}

#[derive(Debug, Clone)]
struct DeterministicRng {
    state: u64,
}

impl DeterministicRng {
    fn new(seed: u64) -> Self {
        Self {
            state: if seed == 0 {
                0x9e37_79b9_7f4a_7c15
            } else {
                seed
            },
        }
    }

    fn next_u64(&mut self) -> u64 {
        let mut value = self.state;
        value ^= value >> 12;
        value ^= value << 25;
        value ^= value >> 27;
        self.state = value;
        value.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    fn chance(&mut self, probability_ppm: u32) -> bool {
        if probability_ppm == 0 {
            return false;
        }
        if probability_ppm >= PPM_SCALE {
            return true;
        }
        self.next_u64() % u64::from(PPM_SCALE) < u64::from(probability_ppm)
    }

    fn symmetric(&mut self, magnitude: u64) -> i64 {
        if magnitude == 0 {
            return 0;
        }
        let width = magnitude.saturating_mul(2).saturating_add(1);
        let sampled = self.next_u64() % width;
        sampled as i64 - magnitude as i64
    }
}

fn ceil_div(numerator: u64, denominator: u64) -> u64 {
    numerator / denominator + u64::from(!numerator.is_multiple_of(denominator))
}

fn add_signed(value: u64, delta: i64) -> u64 {
    if delta >= 0 {
        value.saturating_add(delta as u64)
    } else {
        value.saturating_sub(delta.unsigned_abs())
    }
}
