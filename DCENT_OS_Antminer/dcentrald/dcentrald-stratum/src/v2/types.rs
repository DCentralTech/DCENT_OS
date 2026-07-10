//! Stratum V2 message types and protocol constants.

/// SV2 protocol identifiers used in Common `SetupConnection.protocol`.
pub const PROTOCOL_MINING: u8 = 0;
pub const PROTOCOL_JOB_DECLARATION: u8 = 1;
pub const PROTOCOL_TEMPLATE_DISTRIBUTION: u8 = 2;

/// Common SV2 message types shared by all protocols.
pub mod common {
    pub const SETUP_CONNECTION: u8 = 0x00;
    pub const SETUP_CONNECTION_SUCCESS: u8 = 0x01;
    pub const SETUP_CONNECTION_ERROR: u8 = 0x02;
    pub const CHANNEL_ENDPOINT_CHANGED: u8 = 0x03;
    pub const RECONNECT: u8 = 0x04;
}

/// SV2 Message Types — Mining Protocol
/// Values from SRI (Stratum Reference Implementation) v1.0+
pub mod mining {
    pub const SETUP_CONNECTION: u8 = 0x00;
    pub const SETUP_CONNECTION_SUCCESS: u8 = 0x01;
    pub const SETUP_CONNECTION_ERROR: u8 = 0x02;
    pub const OPEN_STANDARD_MINING_CHANNEL: u8 = 0x10;
    pub const OPEN_STANDARD_MINING_CHANNEL_SUCCESS: u8 = 0x11;
    pub const OPEN_MINING_CHANNEL_ERROR: u8 = 0x12;
    pub const OPEN_EXTENDED_MINING_CHANNEL: u8 = 0x13;
    pub const OPEN_EXTENDED_MINING_CHANNEL_SUCCESS: u8 = 0x14;
    pub const NEW_MINING_JOB: u8 = 0x15;
    pub const UPDATE_CHANNEL: u8 = 0x16;
    pub const UPDATE_CHANNEL_ERROR: u8 = 0x17;
    pub const CLOSE_CHANNEL: u8 = 0x18;
    pub const SET_EXTRANONCE_PREFIX: u8 = 0x19;
    pub const SUBMIT_SHARES_STANDARD: u8 = 0x1a;
    pub const SUBMIT_SHARES_EXTENDED: u8 = 0x1b;
    pub const SUBMIT_SHARES_SUCCESS: u8 = 0x1c;
    pub const SUBMIT_SHARES_ERROR: u8 = 0x1d;
    pub const NEW_EXTENDED_MINING_JOB: u8 = 0x1f;
    pub const SET_NEW_PREV_HASH: u8 = 0x20;
    pub const SET_TARGET: u8 = 0x21;
    pub const SET_CUSTOM_MINING_JOB: u8 = 0x22;
    pub const SET_CUSTOM_MINING_JOB_SUCCESS: u8 = 0x23;
    pub const SET_CUSTOM_MINING_JOB_ERROR: u8 = 0x24;
    pub const SET_GROUP_CHANNEL: u8 = 0x25;
}

/// SV2 Message Types - Job Declaration Protocol.
pub mod job_declaration {
    pub const ALLOCATE_MINING_JOB_TOKEN: u8 = 0x50;
    pub const ALLOCATE_MINING_JOB_TOKEN_SUCCESS: u8 = 0x51;
    pub const PROVIDE_MISSING_TRANSACTIONS: u8 = 0x55;
    pub const PROVIDE_MISSING_TRANSACTIONS_SUCCESS: u8 = 0x56;
    pub const DECLARE_MINING_JOB: u8 = 0x57;
    pub const DECLARE_MINING_JOB_SUCCESS: u8 = 0x58;
    pub const DECLARE_MINING_JOB_ERROR: u8 = 0x59;
    pub const PUSH_SOLUTION: u8 = 0x60;
}

/// SV2 Message Types - Template Distribution Protocol.
pub mod template_distribution {
    pub const COINBASE_OUTPUT_CONSTRAINTS: u8 = 0x70;
    pub const NEW_TEMPLATE: u8 = 0x71;
    pub const SET_NEW_PREV_HASH: u8 = 0x72;
    pub const REQUEST_TRANSACTION_DATA: u8 = 0x73;
    pub const REQUEST_TRANSACTION_DATA_SUCCESS: u8 = 0x74;
    pub const REQUEST_TRANSACTION_DATA_ERROR: u8 = 0x75;
    pub const SUBMIT_SOLUTION: u8 = 0x76;
}

/// SV2 Extension Types
pub const EXTENSION_TYPE_MINING: u16 = 0x0000;
pub const EXTENSION_TYPE_CORE: u16 = 0x0000;
pub const EXTENSION_TYPE_CHANNEL_MSG: u16 = 0x8000;

/// SV2 Setup Connection flags (Mining Protocol)
/// Bit 0: REQUIRES_STANDARD_JOBS
/// Bit 1: REQUIRES_WORK_SELECTION (for Job Declarator clients — NOT for standard mining)
/// Bit 2: REQUIRES_VERSION_ROLLING
pub const REQUIRES_STANDARD_JOBS: u32 = 1 << 0;
pub const REQUIRES_WORK_SELECTION: u32 = 1 << 1;
pub const REQUIRES_VERSION_ROLLING: u32 = 1 << 2; // was 1<<1 (WRONG — that's WORK_SELECTION)

/// Job Declaration `SetupConnection.flags`.
pub const DECLARE_TX_DATA: u32 = 1 << 0;

/// SV2 Mining Protocol — SetupConnection message
#[derive(Debug, Clone)]
pub struct SetupConnection {
    /// Protocol identifier — u8 per SV2 spec (0 = Mining Protocol)
    pub protocol: u8,
    pub min_version: u16,
    pub max_version: u16,
    pub flags: u32,
    pub endpoint_host: String,
    pub endpoint_port: u16,
    pub vendor: String,
    pub hardware_version: String,
    pub firmware: String,
    pub device_id: String,
}

/// SV2 Mining Protocol — OpenStandardMiningChannel
#[derive(Debug, Clone)]
pub struct OpenStandardMiningChannel {
    pub request_id: u32,
    pub user_identity: String,
    pub nominal_hash_rate: f32,
    pub max_target: [u8; 32],
}

/// SV2 Mining Protocol — OpenExtendedMiningChannel
#[derive(Debug, Clone)]
pub struct OpenExtendedMiningChannel {
    pub request_id: u32,
    pub user_identity: String,
    pub nominal_hash_rate: f32,
    pub max_target: [u8; 32],
    pub min_extranonce_size: u16,
}

/// SV2 Mining Protocol — NewMiningJob
#[derive(Debug, Clone)]
pub struct NewMiningJob {
    pub channel_id: u32,
    pub job_id: u32,
    pub future_job: bool,
    pub version: u32,
    pub merkle_root: [u8; 32],
}

/// SV2 Mining Protocol — NewExtendedMiningJob (msg_type 0x1f).
///
/// Sent by the pool on extended channels in place of `NewMiningJob`. Carries
/// the coinbase transaction split (prefix/suffix), merkle path, and a
/// `version_rolling_allowed` flag. The miner reconstructs the coinbase using
/// `coinbase_tx_prefix + extranonce_prefix + extranonce + coinbase_tx_suffix`,
/// then walks `merkle_path` to derive the merkle root for hashing.
///
/// Wire format (matches SRI / `NewMiningJob` layout convention):
///   channel_id              u32          4 B
///   job_id                  u32          4 B
///   future_job              u8 (bool)    1 B
///   version                 u32          4 B
///   version_rolling_allowed u8 (bool)    1 B
///   merkle_path             SEQ0_255[U256] (1 B count + count*32 B)
///   coinbase_tx_prefix      B0_64K       (2 B length + bytes)
///   coinbase_tx_suffix      B0_64K       (2 B length + bytes)
#[derive(Debug, Clone)]
pub struct NewExtendedMiningJob {
    pub channel_id: u32,
    pub job_id: u32,
    pub future_job: bool,
    pub version: u32,
    pub version_rolling_allowed: bool,
    pub merkle_path: Vec<[u8; 32]>,
    pub coinbase_tx_prefix: Vec<u8>,
    pub coinbase_tx_suffix: Vec<u8>,
}

/// SV2 Mining Protocol — SetNewPrevHash
#[derive(Debug, Clone)]
pub struct SetNewPrevHash {
    pub channel_id: u32,
    pub job_id: u32,
    pub prev_hash: [u8; 32],
    pub min_ntime: u32,
    pub nbits: u32,
}

/// SV2 Mining Protocol — SubmitSharesStandard
#[derive(Debug, Clone)]
pub struct SubmitSharesStandard {
    pub channel_id: u32,
    pub sequence_number: u32,
    pub job_id: u32,
    pub nonce: u32,
    pub ntime: u32,
    pub version: u32,
}

/// SV2 Mining Protocol — SubmitSharesExtended
#[derive(Debug, Clone)]
pub struct SubmitSharesExtended {
    pub channel_id: u32,
    pub sequence_number: u32,
    pub job_id: u32,
    pub nonce: u32,
    pub ntime: u32,
    pub version: u32,
    pub extranonce: Vec<u8>,
}

impl SetupConnection {
    /// Serialize to SV2 binary format
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(128);
        buf.push(self.protocol); // u8 — 1 byte per SV2 spec
        buf.extend_from_slice(&self.min_version.to_le_bytes());
        buf.extend_from_slice(&self.max_version.to_le_bytes());
        buf.extend_from_slice(&self.flags.to_le_bytes());
        // SV2 strings: u8 length prefix + UTF-8 bytes
        Self::write_sv2_str(&mut buf, &self.endpoint_host);
        buf.extend_from_slice(&self.endpoint_port.to_le_bytes());
        Self::write_sv2_str(&mut buf, &self.vendor);
        Self::write_sv2_str(&mut buf, &self.hardware_version);
        Self::write_sv2_str(&mut buf, &self.firmware);
        Self::write_sv2_str(&mut buf, &self.device_id);
        buf
    }

    fn write_sv2_str(buf: &mut Vec<u8>, s: &str) {
        let bytes = s.as_bytes();
        buf.push(bytes.len().min(255) as u8);
        buf.extend_from_slice(&bytes[..bytes.len().min(255)]);
    }
}

impl SubmitSharesStandard {
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(24);
        buf.extend_from_slice(&self.channel_id.to_le_bytes());
        buf.extend_from_slice(&self.sequence_number.to_le_bytes());
        buf.extend_from_slice(&self.job_id.to_le_bytes());
        buf.extend_from_slice(&self.nonce.to_le_bytes());
        buf.extend_from_slice(&self.ntime.to_le_bytes());
        buf.extend_from_slice(&self.version.to_le_bytes());
        buf
    }
}

impl SubmitSharesExtended {
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(25 + self.extranonce.len().min(32));
        buf.extend_from_slice(&self.channel_id.to_le_bytes());
        buf.extend_from_slice(&self.sequence_number.to_le_bytes());
        buf.extend_from_slice(&self.job_id.to_le_bytes());
        buf.extend_from_slice(&self.nonce.to_le_bytes());
        buf.extend_from_slice(&self.ntime.to_le_bytes());
        buf.extend_from_slice(&self.version.to_le_bytes());
        let len = self.extranonce.len().min(32);
        buf.push(len as u8);
        buf.extend_from_slice(&self.extranonce[..len]);
        buf
    }
}

impl OpenStandardMiningChannel {
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(64);
        buf.extend_from_slice(&self.request_id.to_le_bytes());
        // user_identity as SV2 string (u8 len prefix)
        let user_bytes = self.user_identity.as_bytes();
        buf.push(user_bytes.len().min(255) as u8);
        buf.extend_from_slice(&user_bytes[..user_bytes.len().min(255)]);
        buf.extend_from_slice(&self.nominal_hash_rate.to_le_bytes());
        buf.extend_from_slice(&self.max_target);
        buf
    }
}

impl OpenExtendedMiningChannel {
    pub fn to_bytes(&self) -> Vec<u8> {
        let standard = OpenStandardMiningChannel {
            request_id: self.request_id,
            user_identity: self.user_identity.clone(),
            nominal_hash_rate: self.nominal_hash_rate,
            max_target: self.max_target,
        };
        let mut buf = standard.to_bytes();
        buf.extend_from_slice(&self.min_extranonce_size.to_le_bytes());
        buf
    }
}

impl NewMiningJob {
    pub fn from_bytes(data: &[u8]) -> Result<Self, &'static str> {
        if data.len() < 45 {
            return Err("NewMiningJob too short");
        }
        let channel_id = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
        let job_id = u32::from_le_bytes([data[4], data[5], data[6], data[7]]);
        let future_job = data[8] != 0;
        let version = u32::from_le_bytes([data[9], data[10], data[11], data[12]]);
        let mut merkle_root = [0u8; 32];
        merkle_root.copy_from_slice(&data[13..45]);
        Ok(Self {
            channel_id,
            job_id,
            future_job,
            version,
            merkle_root,
        })
    }
}

impl NewExtendedMiningJob {
    /// Parse a `NewExtendedMiningJob` payload. Returns an error string instead
    /// of `&'static str` so the caller can surface the offset that failed.
    pub fn from_bytes(data: &[u8]) -> Result<Self, String> {
        // Minimum: channel_id(4) + job_id(4) + future_job(1) + version(4)
        // + version_rolling_allowed(1) + merkle_path_count(1)
        // + coinbase_tx_prefix_len(2) + coinbase_tx_suffix_len(2) = 19 bytes.
        if data.len() < 19 {
            return Err(format!(
                "NewExtendedMiningJob too short: need >=19 bytes, have {}",
                data.len()
            ));
        }
        let channel_id = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
        let job_id = u32::from_le_bytes([data[4], data[5], data[6], data[7]]);
        let future_job = data[8] != 0;
        let version = u32::from_le_bytes([data[9], data[10], data[11], data[12]]);
        let version_rolling_allowed = data[13] != 0;

        let merkle_path_count = data[14] as usize;
        let mut offset = 15;
        let merkle_bytes = merkle_path_count
            .checked_mul(32)
            .ok_or_else(|| "NewExtendedMiningJob merkle_path overflow".to_string())?;
        if data.len() < offset + merkle_bytes {
            return Err(format!(
                "NewExtendedMiningJob merkle_path truncated: need {} bytes at offset {}, have {}",
                merkle_bytes,
                offset,
                data.len() - offset
            ));
        }
        let mut merkle_path = Vec::with_capacity(merkle_path_count);
        for _ in 0..merkle_path_count {
            let mut hash = [0u8; 32];
            hash.copy_from_slice(&data[offset..offset + 32]);
            merkle_path.push(hash);
            offset += 32;
        }

        if data.len() < offset + 2 {
            return Err(format!(
                "NewExtendedMiningJob coinbase_tx_prefix length missing at offset {} (len={})",
                offset,
                data.len()
            ));
        }
        let prefix_len = u16::from_le_bytes([data[offset], data[offset + 1]]) as usize;
        offset += 2;
        if data.len() < offset + prefix_len {
            return Err(format!(
                "NewExtendedMiningJob coinbase_tx_prefix truncated: need {} bytes at offset {}, have {}",
                prefix_len,
                offset,
                data.len() - offset
            ));
        }
        let coinbase_tx_prefix = data[offset..offset + prefix_len].to_vec();
        offset += prefix_len;

        if data.len() < offset + 2 {
            return Err(format!(
                "NewExtendedMiningJob coinbase_tx_suffix length missing at offset {} (len={})",
                offset,
                data.len()
            ));
        }
        let suffix_len = u16::from_le_bytes([data[offset], data[offset + 1]]) as usize;
        offset += 2;
        if data.len() < offset + suffix_len {
            return Err(format!(
                "NewExtendedMiningJob coinbase_tx_suffix truncated: need {} bytes at offset {}, have {}",
                suffix_len,
                offset,
                data.len() - offset
            ));
        }
        let coinbase_tx_suffix = data[offset..offset + suffix_len].to_vec();

        Ok(Self {
            channel_id,
            job_id,
            future_job,
            version,
            version_rolling_allowed,
            merkle_path,
            coinbase_tx_prefix,
            coinbase_tx_suffix,
        })
    }
}

impl SetNewPrevHash {
    pub fn from_bytes(data: &[u8]) -> Result<Self, &'static str> {
        if data.len() < 44 {
            return Err("SetNewPrevHash too short");
        }
        let channel_id = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
        let job_id = u32::from_le_bytes([data[4], data[5], data[6], data[7]]);
        let mut prev_hash = [0u8; 32];
        prev_hash.copy_from_slice(&data[8..40]);
        let min_ntime = u32::from_le_bytes([data[40], data[41], data[42], data[43]]);
        let nbits = if data.len() >= 48 {
            u32::from_le_bytes([data[44], data[45], data[46], data[47]])
        } else {
            0
        };
        Ok(Self {
            channel_id,
            job_id,
            prev_hash,
            min_ntime,
            nbits,
        })
    }
}

/// Record of an SV2 protocol message (for protocol inspector).
#[derive(Debug, Clone)]
pub struct Sv2MessageRecord {
    pub direction: &'static str, // "sent" or "recv"
    pub msg_type: u8,
    pub msg_name: String,
    pub timestamp_ms: u64,
    pub payload_size: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // SV2 outbound message serialization (to_bytes) — wire-format pins.
    //
    // These to_bytes impls produce the bytes the SV2 client sends to the
    // pool. A silent layout drift here would silently mis-frame every
    // outbound message and the pool would reject them. Pin the exact
    // byte layout for each message type so a refactor of the encoder
    // is caught.
    // -----------------------------------------------------------------------

    #[test]
    fn protocol_constants_are_pinned() {
        // SV2 protocol identifiers used in `SetupConnection.protocol`.
        assert_eq!(PROTOCOL_MINING, 0);
        assert_eq!(PROTOCOL_JOB_DECLARATION, 1);
        assert_eq!(PROTOCOL_TEMPLATE_DISTRIBUTION, 2);
    }

    #[test]
    fn extension_type_constants_are_pinned() {
        assert_eq!(EXTENSION_TYPE_MINING, 0x0000);
        assert_eq!(EXTENSION_TYPE_CORE, 0x0000);
        assert_eq!(EXTENSION_TYPE_CHANNEL_MSG, 0x8000);
    }

    #[test]
    fn setup_connection_flag_constants_are_pinned() {
        // BUG FIX (existing): REQUIRES_VERSION_ROLLING was previously
        // 1<<1 (collided with REQUIRES_WORK_SELECTION). Pin the corrected
        // values so the bug doesn't return.
        assert_eq!(REQUIRES_STANDARD_JOBS, 1 << 0);
        assert_eq!(REQUIRES_WORK_SELECTION, 1 << 1);
        assert_eq!(REQUIRES_VERSION_ROLLING, 1 << 2);
    }

    #[test]
    fn declare_tx_data_flag_is_pinned() {
        assert_eq!(DECLARE_TX_DATA, 1 << 0);
    }

    #[test]
    fn setup_connection_to_bytes_layout() {
        let msg = SetupConnection {
            protocol: 0,
            min_version: 2,
            max_version: 2,
            flags: REQUIRES_VERSION_ROLLING | REQUIRES_STANDARD_JOBS,
            endpoint_host: "p".into(),
            endpoint_port: 3336,
            vendor: "v".into(),
            hardware_version: "h".into(),
            firmware: "f".into(),
            device_id: "d".into(),
        };
        let bytes = msg.to_bytes();

        // Layout per SV2 spec:
        //   protocol      u8       (1 byte)
        //   min_version   u16 LE   (2 bytes)
        //   max_version   u16 LE   (2 bytes)
        //   flags         u32 LE   (4 bytes)
        //   endpoint_host STR0_255 (1 byte len + bytes)
        //   endpoint_port u16 LE   (2 bytes)
        //   vendor        STR0_255
        //   hardware_version STR0_255
        //   firmware      STR0_255
        //   device_id     STR0_255
        assert_eq!(bytes[0], 0); // protocol = mining
        assert_eq!(&bytes[1..3], &2u16.to_le_bytes()); // min_version
        assert_eq!(&bytes[3..5], &2u16.to_le_bytes()); // max_version
        assert_eq!(
            &bytes[5..9],
            &(REQUIRES_VERSION_ROLLING | REQUIRES_STANDARD_JOBS).to_le_bytes()
        );
        assert_eq!(bytes[9], 1); // host len
        assert_eq!(bytes[10], b'p'); // host
        assert_eq!(&bytes[11..13], &3336u16.to_le_bytes()); // port
    }

    #[test]
    fn setup_connection_to_bytes_clamps_strings_at_255_bytes() {
        // SV2 STR0_255 has a u8 length prefix → values > 255 must be
        // silently truncated. Pin so a refactor that promoted to STR0_64K
        // would catch the call sites that depend on the truncation.
        let long = "x".repeat(300);
        let msg = SetupConnection {
            protocol: 0,
            min_version: 2,
            max_version: 2,
            flags: 0,
            endpoint_host: long.clone(),
            endpoint_port: 3336,
            vendor: String::new(),
            hardware_version: String::new(),
            firmware: String::new(),
            device_id: String::new(),
        };
        let bytes = msg.to_bytes();
        // After the fixed-size header (1+2+2+4 = 9 bytes), the host length
        // byte must be 255 (clamped).
        assert_eq!(bytes[9], 255);
        // Followed by exactly 255 'x' bytes.
        assert_eq!(&bytes[10..10 + 255], &vec![b'x'; 255][..]);
    }

    #[test]
    fn submit_shares_standard_to_bytes_is_24_bytes_fixed_layout() {
        let msg = SubmitSharesStandard {
            channel_id: 0xAABBCCDD,
            sequence_number: 0x11223344,
            job_id: 0x55667788,
            nonce: 0xDEADBEEF,
            ntime: 0x65A7E340,
            version: 0x20000000,
        };
        let bytes = msg.to_bytes();
        assert_eq!(bytes.len(), 24);
        // Each field is u32 LE in a fixed slot.
        assert_eq!(&bytes[0..4], &0xAABBCCDDu32.to_le_bytes());
        assert_eq!(&bytes[4..8], &0x11223344u32.to_le_bytes());
        assert_eq!(&bytes[8..12], &0x55667788u32.to_le_bytes());
        assert_eq!(&bytes[12..16], &0xDEADBEEFu32.to_le_bytes());
        assert_eq!(&bytes[16..20], &0x65A7E340u32.to_le_bytes());
        assert_eq!(&bytes[20..24], &0x20000000u32.to_le_bytes());
    }

    #[test]
    fn submit_shares_extended_to_bytes_appends_extranonce_with_byte_length_prefix() {
        let extranonce = vec![0xAA, 0xBB, 0xCC, 0xDD];
        let msg = SubmitSharesExtended {
            channel_id: 1,
            sequence_number: 2,
            job_id: 3,
            nonce: 4,
            ntime: 5,
            version: 6,
            extranonce: extranonce.clone(),
        };
        let bytes = msg.to_bytes();
        // Standard fixed prefix = 24 bytes, then 1 byte length + N extranonce bytes.
        assert_eq!(bytes.len(), 24 + 1 + extranonce.len());
        assert_eq!(bytes[24], extranonce.len() as u8);
        assert_eq!(&bytes[25..25 + extranonce.len()], extranonce.as_slice());
    }

    #[test]
    fn submit_shares_extended_to_bytes_clamps_extranonce_at_32_bytes() {
        // Extranonce length is a u8 in SV2 wire form, but the impl
        // additionally clamps to 32 bytes (B0_32). Pin the cap so a
        // refactor to a wider extranonce field is caught.
        let extranonce = vec![0xFFu8; 50];
        let msg = SubmitSharesExtended {
            channel_id: 1,
            sequence_number: 2,
            job_id: 3,
            nonce: 4,
            ntime: 5,
            version: 6,
            extranonce,
        };
        let bytes = msg.to_bytes();
        // Length byte clamped at 32, only 32 extranonce bytes follow.
        assert_eq!(bytes[24], 32);
        assert_eq!(bytes.len(), 24 + 1 + 32);
    }

    #[test]
    fn open_standard_mining_channel_to_bytes_layout() {
        let msg = OpenStandardMiningChannel {
            request_id: 0x11223344,
            user_identity: "worker".into(),
            nominal_hash_rate: 13_500_000_000_000.0,
            max_target: [0xFFu8; 32],
        };
        let bytes = msg.to_bytes();
        // request_id (4) + str_len (1) + str (6) + nominal_hash_rate (4) + max_target (32)
        assert_eq!(bytes.len(), 4 + 1 + 6 + 4 + 32);
        assert_eq!(&bytes[0..4], &0x11223344u32.to_le_bytes());
        assert_eq!(bytes[4], 6); // string length
        assert_eq!(&bytes[5..11], b"worker");
        // hashrate is f32 LE
        assert_eq!(&bytes[11..15], &13_500_000_000_000.0_f32.to_le_bytes());
        assert_eq!(&bytes[15..47], &[0xFFu8; 32]);
    }

    #[test]
    fn open_extended_mining_channel_appends_min_extranonce_size_after_standard() {
        let msg = OpenExtendedMiningChannel {
            request_id: 1,
            user_identity: "w".into(),
            nominal_hash_rate: 1.0,
            max_target: [0u8; 32],
            min_extranonce_size: 4,
        };
        let bytes = msg.to_bytes();
        // Standard portion = 4 + 1 + 1 + 4 + 32 = 42 bytes
        // Extended adds u16 LE = 44 total.
        assert_eq!(bytes.len(), 44);
        assert_eq!(&bytes[42..44], &4u16.to_le_bytes());
    }

    // -----------------------------------------------------------------------
    // SV2 inbound message parsing (from_bytes).
    // -----------------------------------------------------------------------

    #[test]
    fn new_mining_job_from_bytes_rejects_short_payload() {
        // Need at least 45 bytes (4+4+1+4+32). Anything less is malformed.
        let result = NewMiningJob::from_bytes(&[0u8; 44]);
        assert!(result.is_err());

        let result = NewMiningJob::from_bytes(&[]);
        assert!(result.is_err());
    }

    #[test]
    fn new_mining_job_from_bytes_round_trips_field_layout() {
        let mut buf = Vec::new();
        buf.extend_from_slice(&0x11223344u32.to_le_bytes()); // channel_id
        buf.extend_from_slice(&0xAABBCCDDu32.to_le_bytes()); // job_id
        buf.push(1); // future_job = true
        buf.extend_from_slice(&0x20004000u32.to_le_bytes()); // version
        buf.extend_from_slice(&[0x42; 32]); // merkle_root

        let job = NewMiningJob::from_bytes(&buf).unwrap();
        assert_eq!(job.channel_id, 0x11223344);
        assert_eq!(job.job_id, 0xAABBCCDD);
        assert!(job.future_job);
        assert_eq!(job.version, 0x20004000);
        assert_eq!(job.merkle_root, [0x42; 32]);
    }

    #[test]
    fn set_new_prev_hash_from_bytes_at_min_payload_length_44_sets_nbits_to_zero() {
        // The parser is tolerant of 44-byte payloads (no nbits field) —
        // some pool implementations ship truncated SetNewPrevHash. Pin
        // that the parser accepts 44 bytes and sets nbits=0.
        let mut buf = Vec::new();
        buf.extend_from_slice(&7u32.to_le_bytes()); // channel_id
        buf.extend_from_slice(&55u32.to_le_bytes()); // job_id
        buf.extend_from_slice(&[0x77; 32]); // prev_hash
        buf.extend_from_slice(&1_700_000_000u32.to_le_bytes()); // min_ntime
        assert_eq!(buf.len(), 44);

        let ph = SetNewPrevHash::from_bytes(&buf).unwrap();
        assert_eq!(ph.channel_id, 7);
        assert_eq!(ph.job_id, 55);
        assert_eq!(ph.prev_hash, [0x77; 32]);
        assert_eq!(ph.min_ntime, 1_700_000_000);
        // nbits=0 sentinel for the legacy 44-byte form.
        assert_eq!(ph.nbits, 0);
    }

    #[test]
    fn set_new_prev_hash_from_bytes_at_full_48_byte_payload_parses_nbits() {
        let mut buf = Vec::new();
        buf.extend_from_slice(&7u32.to_le_bytes());
        buf.extend_from_slice(&55u32.to_le_bytes());
        buf.extend_from_slice(&[0x88; 32]);
        buf.extend_from_slice(&1_700_001_000u32.to_le_bytes());
        buf.extend_from_slice(&0x1903_a30cu32.to_le_bytes()); // nbits

        let ph = SetNewPrevHash::from_bytes(&buf).unwrap();
        assert_eq!(ph.nbits, 0x1903_a30c);
    }

    #[test]
    fn set_new_prev_hash_from_bytes_rejects_short_payload() {
        // 43 bytes = one short of the 44-byte minimum.
        let result = SetNewPrevHash::from_bytes(&[0u8; 43]);
        assert!(result.is_err());

        let result = SetNewPrevHash::from_bytes(&[]);
        assert!(result.is_err());
    }

    #[test]
    fn new_extended_mining_job_from_bytes_specifies_failure_offset_in_error() {
        // The parser returns String errors that name the truncation point.
        // Pin so a refactor that switched to a generic error loses the
        // diagnostic detail.
        let mut buf = Vec::new();
        buf.extend_from_slice(&1u32.to_le_bytes()); // channel_id
        buf.extend_from_slice(&2u32.to_le_bytes()); // job_id
        buf.push(0); // future_job
        buf.extend_from_slice(&0u32.to_le_bytes()); // version
        buf.push(0); // version_rolling_allowed
        buf.push(2); // merkle_path_count = 2
        buf.extend_from_slice(&[0u8; 32]); // only 1 of 2 hashes

        let err = NewExtendedMiningJob::from_bytes(&buf).unwrap_err();
        assert!(err.contains("merkle_path"), "err was: {err}");
        assert!(err.contains("truncated"), "err was: {err}");
    }
}
