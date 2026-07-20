use std::collections::{HashMap, HashSet, VecDeque};
use std::time::{Duration, Instant};

pub const MAX_NACK_ITEMS: usize = 64;
const NACK_MAGIC: &[u8; 4] = b"NACK";
const NACK_VERSION: u8 = 1;
const NACK_HEADER_LEN: usize = 15;
const NACK_ITEM_LEN: usize = 10;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RepairMode {
    Off,
    Nack,
}

impl RepairMode {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Nack => "nack",
        }
    }

    pub fn parse(value: &str) -> Result<Self, String> {
        match value.to_ascii_lowercase().as_str() {
            "off" => Ok(Self::Off),
            "nack" => Ok(Self::Nack),
            _ => Err("repair must be off or nack".to_string()),
        }
    }
}

impl Default for RepairMode {
    fn default() -> Self {
        Self::Off
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct PacketKey {
    pub frame_id: u64,
    pub packet_index: u16,
}

#[derive(Default)]
pub struct PacketUniquenessTracker {
    seen: HashSet<PacketKey>,
    unique: u64,
    duplicate: u64,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct RepairSuppressionStats {
    pub requests: u64,
    pub allowed: u64,
    pub suppressed: u64,
    pub evicted: u64,
}

pub struct RepairSuppression {
    interval: Duration,
    last_sent: HashMap<PacketKey, Instant>,
    order: VecDeque<(Instant, PacketKey)>,
    stats: RepairSuppressionStats,
}

impl RepairSuppression {
    pub fn new(interval: Duration) -> Result<Self, String> {
        if interval.is_zero() {
            return Err("repair suppression interval must be greater than zero".to_string());
        }
        Ok(Self {
            interval,
            last_sent: HashMap::new(),
            order: VecDeque::new(),
            stats: RepairSuppressionStats::default(),
        })
    }

    pub fn should_send(&mut self, key: PacketKey, now: Instant) -> bool {
        self.evict(now);
        self.stats.requests = self.stats.requests.saturating_add(1);
        if self
            .last_sent
            .get(&key)
            .is_some_and(|last| now.saturating_duration_since(*last) < self.interval)
        {
            self.stats.suppressed = self.stats.suppressed.saturating_add(1);
            return false;
        }
        self.last_sent.insert(key, now);
        self.order.push_back((now, key));
        self.stats.allowed = self.stats.allowed.saturating_add(1);
        true
    }

    fn evict(&mut self, now: Instant) {
        let retention = self.interval.saturating_mul(4);
        while let Some((sent_at, key)) = self.order.front().copied() {
            if now.saturating_duration_since(sent_at) <= retention {
                break;
            }
            self.order.pop_front();
            if self
                .last_sent
                .get(&key)
                .is_some_and(|last| *last == sent_at)
            {
                self.last_sent.remove(&key);
                self.stats.evicted = self.stats.evicted.saturating_add(1);
            }
        }
    }

    pub fn stats(&self) -> RepairSuppressionStats {
        self.stats
    }
}

impl PacketUniquenessTracker {
    pub fn observe(&mut self, key: PacketKey) -> bool {
        if self.seen.insert(key) {
            self.unique += 1;
            true
        } else {
            self.duplicate += 1;
            false
        }
    }

    pub fn unique(&self) -> u64 {
        self.unique
    }

    pub fn duplicate(&self) -> u64 {
        self.duplicate
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NackPacket {
    pub session_id: u64,
    pub items: Vec<PacketKey>,
}

impl NackPacket {
    pub fn encode(&self) -> Result<Vec<u8>, String> {
        if self.items.is_empty() || self.items.len() > MAX_NACK_ITEMS {
            return Err(format!(
                "NACK item count must be between 1 and {MAX_NACK_ITEMS}"
            ));
        }
        let mut output = Vec::with_capacity(NACK_HEADER_LEN + self.items.len() * NACK_ITEM_LEN);
        output.extend_from_slice(NACK_MAGIC);
        output.push(NACK_VERSION);
        output.extend_from_slice(&self.session_id.to_be_bytes());
        output.extend_from_slice(&(self.items.len() as u16).to_be_bytes());
        for item in &self.items {
            output.extend_from_slice(&item.frame_id.to_be_bytes());
            output.extend_from_slice(&item.packet_index.to_be_bytes());
        }
        Ok(output)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() < NACK_HEADER_LEN || &bytes[..4] != NACK_MAGIC {
            return Err("not a NACK datagram".to_string());
        }
        if bytes[4] != NACK_VERSION {
            return Err(format!("unsupported NACK version: {}", bytes[4]));
        }
        let session_id = u64::from_be_bytes(bytes[5..13].try_into().unwrap());
        let count = u16::from_be_bytes(bytes[13..15].try_into().unwrap()) as usize;
        if count == 0 || count > MAX_NACK_ITEMS {
            return Err("invalid NACK item count".to_string());
        }
        if bytes.len() != NACK_HEADER_LEN + count * NACK_ITEM_LEN {
            return Err("NACK datagram length mismatch".to_string());
        }
        let mut items = Vec::with_capacity(count);
        for chunk in bytes[NACK_HEADER_LEN..].chunks_exact(NACK_ITEM_LEN) {
            items.push(PacketKey {
                frame_id: u64::from_be_bytes(chunk[..8].try_into().unwrap()),
                packet_index: u16::from_be_bytes(chunk[8..10].try_into().unwrap()),
            });
        }
        Ok(Self { session_id, items })
    }
}

struct CacheEntry {
    bytes: Vec<u8>,
    sent_at: Instant,
}

pub struct RepairCache {
    ttl: Duration,
    packets: HashMap<PacketKey, CacheEntry>,
    order: VecDeque<(Instant, PacketKey)>,
    bytes: usize,
    evictions: u64,
}

impl RepairCache {
    pub fn new(ttl: Duration) -> Result<Self, String> {
        if ttl.is_zero() {
            return Err("repair cache TTL must be greater than zero".to_string());
        }
        Ok(Self {
            ttl,
            packets: HashMap::new(),
            order: VecDeque::new(),
            bytes: 0,
            evictions: 0,
        })
    }

    pub fn insert(&mut self, key: PacketKey, bytes: Vec<u8>, now: Instant) {
        self.evict_expired(now);
        if let Some(previous) = self.packets.insert(
            key,
            CacheEntry {
                bytes,
                sent_at: now,
            },
        ) {
            self.bytes = self.bytes.saturating_sub(previous.bytes.len());
        }
        self.bytes += self.packets.get(&key).map_or(0, |entry| entry.bytes.len());
        self.order.push_back((now, key));
    }

    pub fn get(&mut self, key: PacketKey, now: Instant) -> Option<Vec<u8>> {
        self.evict_expired(now);
        self.packets.get(&key).map(|entry| entry.bytes.clone())
    }

    pub fn evict_expired(&mut self, now: Instant) {
        while let Some((sent_at, key)) = self.order.front().copied() {
            if now.saturating_duration_since(sent_at) <= self.ttl {
                break;
            }
            self.order.pop_front();
            if self
                .packets
                .get(&key)
                .is_some_and(|entry| entry.sent_at == sent_at)
            {
                if let Some(entry) = self.packets.remove(&key) {
                    self.bytes = self.bytes.saturating_sub(entry.bytes.len());
                    self.evictions += 1;
                }
            }
        }
    }

    pub fn len(&self) -> usize {
        self.packets.len()
    }

    pub fn bytes(&self) -> usize {
        self.bytes
    }

    pub fn evictions(&self) -> u64 {
        self.evictions
    }

    pub fn clear(&mut self) {
        self.packets.clear();
        self.order.clear();
        self.bytes = 0;
    }
}

pub fn media_packet_key(datagram: &[u8]) -> Option<(u64, PacketKey, u16)> {
    if datagram.len() < crate::HEADER_LEN || &datagram[..4] != b"AGM1" {
        return None;
    }
    Some((
        u64::from_be_bytes(datagram[8..16].try_into().ok()?),
        PacketKey {
            frame_id: u64::from_be_bytes(datagram[16..24].try_into().ok()?),
            packet_index: u16::from_be_bytes(datagram[24..26].try_into().ok()?),
        },
        u16::from_be_bytes(datagram[6..8].try_into().ok()?),
    ))
}

pub fn run_self_test() -> Result<(), String> {
    let packet = NackPacket {
        session_id: 42,
        items: vec![
            PacketKey {
                frame_id: 7,
                packet_index: 3,
            },
            PacketKey {
                frame_id: 8,
                packet_index: 1,
            },
        ],
    };
    if NackPacket::decode(&packet.encode()?)? != packet {
        return Err("NACK encode/decode roundtrip failed".to_string());
    }
    let too_many = NackPacket {
        session_id: 1,
        items: vec![
            PacketKey {
                frame_id: 1,
                packet_index: 0
            };
            MAX_NACK_ITEMS + 1
        ],
    };
    if too_many.encode().is_ok() {
        return Err("NACK item limit was not enforced".to_string());
    }
    let now = Instant::now();
    let key = PacketKey {
        frame_id: 4,
        packet_index: 2,
    };
    let mut cache = RepairCache::new(Duration::from_millis(10))?;
    cache.insert(key, vec![1, 2, 3], now);
    if cache.get(key, now) != Some(vec![1, 2, 3]) {
        return Err("repair cache hit failed".to_string());
    }
    if cache.get(key, now + Duration::from_millis(11)).is_some() {
        return Err("expired repair cache entry remained available".to_string());
    }
    let mut uniqueness = PacketUniquenessTracker::default();
    let second_key = PacketKey {
        frame_id: 4,
        packet_index: 3,
    };
    if !uniqueness.observe(key)
        || uniqueness.observe(key)
        || !uniqueness.observe(second_key)
        || uniqueness.observe(key)
        || uniqueness.unique() != 2
        || uniqueness.duplicate() != 2
    {
        return Err("multi-round repair unique/duplicate accounting failed".to_string());
    }
    let mut suppression = RepairSuppression::new(Duration::from_millis(30))?;
    if !suppression.should_send(key, now)
        || suppression.should_send(key, now + Duration::from_millis(10))
        || !suppression.should_send(key, now + Duration::from_millis(31))
        || suppression.stats().suppressed != 1
    {
        return Err("repair resend suppression failed".to_string());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn deterministic_repair_regressions() {
        super::run_self_test().expect("repair self-test");
    }
}
