use crate::{
    MediaPacket, FLAG_END_OF_FRAME, FLAG_FEC, FLAG_FEC_PROTECTED, HEADER_LEN, LEGACY_MEDIA_PAYLOAD,
    MAX_MEDIA_PAYLOAD, STREAM_VIDEO,
};

const FEC_MAGIC: &[u8; 4] = b"FEC1";
const FEC_VERSION: u8 = 1;
pub(crate) const FEC_METADATA_LEN: usize = 13;
pub const FEC_DATA_PAYLOAD_SIZE: usize = LEGACY_MEDIA_PAYLOAD - FEC_METADATA_LEN;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FecMode {
    Off,
    SingleXor,
}

impl FecMode {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::SingleXor => "single-xor",
        }
    }
}

#[derive(Clone, Debug)]
pub struct FecParity {
    pub data_packet_count: u16,
    pub data_payload_size: u16,
    pub last_data_payload_len: u16,
    pub parity: Vec<u8>,
}

impl FecParity {
    pub fn encode(&self) -> Result<Vec<u8>, String> {
        if self.data_packet_count == 0 {
            return Err("FEC data packet count must be greater than zero".to_string());
        }
        if usize::from(self.data_packet_count) > crate::MAX_VIDEO_PACKET_COUNT {
            return Err("FEC data packet count exceeds video frame limit".to_string());
        }
        if self.data_payload_size == 0 || self.parity.len() > self.data_payload_size as usize {
            return Err("FEC parity length exceeds data payload size".to_string());
        }
        if self.last_data_payload_len > self.data_payload_size {
            return Err("FEC last payload length exceeds data payload size".to_string());
        }
        let parity_len = u16::try_from(self.parity.len())
            .map_err(|_| "FEC parity length exceeds u16".to_string())?;
        let mut payload = Vec::with_capacity(FEC_METADATA_LEN + self.parity.len());
        payload.extend_from_slice(FEC_MAGIC);
        payload.push(FEC_VERSION);
        payload.extend_from_slice(&self.data_packet_count.to_be_bytes());
        payload.extend_from_slice(&self.data_payload_size.to_be_bytes());
        payload.extend_from_slice(&self.last_data_payload_len.to_be_bytes());
        payload.extend_from_slice(&parity_len.to_be_bytes());
        payload.extend_from_slice(&self.parity);
        if payload.len() > MAX_MEDIA_PAYLOAD {
            return Err("FEC payload exceeds AGM1 media payload limit".to_string());
        }
        Ok(payload)
    }

    pub fn decode_owned(mut payload: Vec<u8>) -> Result<Self, String> {
        let (data_packet_count, data_payload_size, last_data_payload_len, _) =
            Self::decode_metadata(&payload)?;
        let parity = payload.split_off(FEC_METADATA_LEN);
        Ok(Self {
            data_packet_count,
            data_payload_size,
            last_data_payload_len,
            parity,
        })
    }

    fn decode_metadata(payload: &[u8]) -> Result<(u16, u16, u16, usize), String> {
        if payload.len() < FEC_METADATA_LEN || &payload[..4] != FEC_MAGIC {
            return Err("invalid FEC1 payload".to_string());
        }
        if payload[4] != FEC_VERSION {
            return Err(format!("unsupported FEC version: {}", payload[4]));
        }
        let data_packet_count = u16::from_be_bytes([payload[5], payload[6]]);
        let data_payload_size = u16::from_be_bytes([payload[7], payload[8]]);
        let last_data_payload_len = u16::from_be_bytes([payload[9], payload[10]]);
        let parity_len = u16::from_be_bytes([payload[11], payload[12]]) as usize;
        if data_packet_count == 0
            || usize::from(data_packet_count) > crate::MAX_VIDEO_PACKET_COUNT
            || data_payload_size == 0
            || last_data_payload_len > data_payload_size
            || parity_len > data_payload_size as usize
            || FEC_METADATA_LEN + parity_len != payload.len()
        {
            return Err("invalid FEC1 metadata".to_string());
        }
        Ok((
            data_packet_count,
            data_payload_size,
            last_data_payload_len,
            parity_len,
        ))
    }

    pub fn expected_payload_len(&self, packet_index: usize) -> Result<usize, String> {
        if packet_index >= self.data_packet_count as usize {
            return Err("FEC packet index is out of range".to_string());
        }
        if packet_index + 1 == self.data_packet_count as usize {
            Ok(self.last_data_payload_len as usize)
        } else {
            Ok(self.data_payload_size as usize)
        }
    }
}

pub struct PacketizedFrame {
    pub packets: Vec<Vec<u8>>,
    pub data_packet_count: usize,
    pub fec_packet_count: usize,
    pub fec_bytes: usize,
}

pub fn packetize_frame(
    session_id: u64,
    frame_id: u64,
    timestamp_ms: u64,
    payload: &[u8],
    frame_flags: u16,
    mode: FecMode,
    udp_payload_size: usize,
) -> Result<PacketizedFrame, String> {
    if payload.len() > crate::MAX_VIDEO_FRAME_BYTES {
        return Err(format!(
            "encoded frame exceeds byte limit: {} > {}",
            payload.len(),
            crate::MAX_VIDEO_FRAME_BYTES
        ));
    }
    crate::validate_udp_payload_size(udp_payload_size)?;
    let media_payload_size = udp_payload_size - HEADER_LEN;
    let data_payload_size = match mode {
        FecMode::Off => media_payload_size,
        FecMode::SingleXor => media_payload_size
            .checked_sub(FEC_METADATA_LEN)
            .ok_or_else(|| "UDP payload size is too small for FEC metadata".to_string())?,
    };
    let data_packet_count = payload.len().div_ceil(data_payload_size).max(1);
    if data_packet_count > crate::MAX_VIDEO_PACKET_COUNT {
        return Err(format!(
            "encoded frame too large: {data_packet_count} packets"
        ));
    }

    let mut packets =
        Vec::with_capacity(data_packet_count + usize::from(mode == FecMode::SingleXor));
    let mut data_payloads = Vec::with_capacity(data_packet_count);
    for packet_index in 0..data_packet_count {
        let start = packet_index * data_payload_size;
        let end = (start + data_payload_size).min(payload.len());
        let chunk = payload.get(start..end).unwrap_or_default();
        data_payloads.push(chunk);
        let mut flags = if packet_index == 0 { frame_flags } else { 0 };
        if mode == FecMode::SingleXor {
            flags |= FLAG_FEC_PROTECTED;
        }
        if packet_index + 1 == data_packet_count {
            flags |= FLAG_END_OF_FRAME;
        }
        packets.push(
            MediaPacket {
                stream_id: STREAM_VIDEO,
                flags,
                session_id,
                frame_id,
                packet_index: packet_index as u16,
                packet_count: data_packet_count as u16,
                timestamp_ms,
                payload: chunk.to_vec(),
            }
            .encode_with_udp_payload_size(udp_payload_size)?,
        );
    }

    let mut fec_bytes = 0usize;
    if mode == FecMode::SingleXor {
        let parity_len = data_payloads
            .iter()
            .map(|chunk| chunk.len())
            .max()
            .unwrap_or(0);
        let mut parity = vec![0u8; parity_len];
        for chunk in &data_payloads {
            for (index, byte) in chunk.iter().enumerate() {
                parity[index] ^= byte;
            }
        }
        let last_data_payload_len = data_payloads.last().map_or(0, |chunk| chunk.len());
        let fec_payload = FecParity {
            data_packet_count: data_packet_count as u16,
            data_payload_size: data_payload_size as u16,
            last_data_payload_len: last_data_payload_len as u16,
            parity,
        }
        .encode()?;
        let fec_packet = MediaPacket {
            stream_id: STREAM_VIDEO,
            flags: FLAG_FEC | frame_flags,
            session_id,
            frame_id,
            packet_index: 0,
            packet_count: data_packet_count as u16,
            timestamp_ms,
            payload: fec_payload,
        }
        .encode_with_udp_payload_size(udp_payload_size)?;
        fec_bytes = fec_packet.len();
        packets.push(fec_packet);
    }

    Ok(PacketizedFrame {
        packets,
        data_packet_count,
        fec_packet_count: usize::from(mode == FecMode::SingleXor),
        fec_bytes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fec_count_over_limit_rejected() {
        let count = u16::try_from(crate::MAX_VIDEO_PACKET_COUNT + 1).unwrap();
        let parity = FecParity {
            data_packet_count: count,
            data_payload_size: 1,
            last_data_payload_len: 1,
            parity: vec![0],
        };
        assert!(parity.encode().is_err());
    }
}
