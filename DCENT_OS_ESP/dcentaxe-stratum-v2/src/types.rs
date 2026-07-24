//! Stratum V2 message types and protocol constants.

/// SV2 Message Types — Mining Protocol
/// Values from SRI (Stratum Reference Implementation) v1.0+
pub mod mining {
    pub const SETUP_CONNECTION: u8 = 0x00;
    pub const SETUP_CONNECTION_SUCCESS: u8 = 0x01;
    pub const SETUP_CONNECTION_ERROR: u8 = 0x02;
    pub const OPEN_STANDARD_MINING_CHANNEL: u8 = 0x10;
    pub const OPEN_STANDARD_MINING_CHANNEL_SUCCESS: u8 = 0x11;
    pub const OPEN_MINING_CHANNEL_ERROR: u8 = 0x12;
    pub const NEW_MINING_JOB: u8 = 0x15;
    pub const UPDATE_CHANNEL: u8 = 0x16;
    pub const CLOSE_CHANNEL: u8 = 0x18;
    pub const SET_EXTRANONCE_PREFIX: u8 = 0x19;
    pub const SUBMIT_SHARES_STANDARD: u8 = 0x1a;
    pub const SUBMIT_SHARES_SUCCESS: u8 = 0x1c;
    pub const SUBMIT_SHARES_ERROR: u8 = 0x1d;
    pub const NEW_EXTENDED_MINING_JOB: u8 = 0x1f;
    pub const SET_NEW_PREV_HASH: u8 = 0x20;
    pub const SET_TARGET: u8 = 0x21;
    pub const RECONNECT: u8 = 0x25;
}

/// SV2 Extension Types
pub const EXTENSION_TYPE_MINING: u16 = 0x0000;
pub const EXTENSION_TYPE_CHANNEL_MSG: u16 = 0x8000;

/// SV2 Setup Connection flags (Mining Protocol)
/// Bit 0: REQUIRES_STANDARD_JOBS
/// Bit 1: REQUIRES_WORK_SELECTION (for Job Declarator clients — NOT for standard mining)
/// Bit 2: REQUIRES_VERSION_ROLLING
pub const REQUIRES_STANDARD_JOBS: u32 = 1 << 0;
pub const REQUIRES_VERSION_ROLLING: u32 = 1 << 2; // was 1<<1 (WRONG — that's WORK_SELECTION)

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

/// SV2 Mining Protocol — NewMiningJob
#[derive(Debug, Clone)]
pub struct NewMiningJob {
    pub channel_id: u32,
    pub job_id: u32,
    pub min_ntime: Option<u32>,
    pub version: u32,
    pub merkle_root: [u8; 32],
}

impl NewMiningJob {
    pub fn is_future(&self) -> bool {
        self.min_ntime.is_none()
    }
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

impl NewMiningJob {
    pub fn from_bytes(data: &[u8]) -> Result<Self, &'static str> {
        if data.len() < 9 {
            return Err("NewMiningJob too short");
        }
        let channel_id = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
        let job_id = u32::from_le_bytes([data[4], data[5], data[6], data[7]]);
        let mut offset = 8;
        let min_ntime = match data[offset] {
            0 => {
                offset += 1;
                None
            }
            1 => {
                if data.len() < offset + 5 {
                    return Err("NewMiningJob min_ntime too short");
                }
                let value = u32::from_le_bytes([
                    data[offset + 1],
                    data[offset + 2],
                    data[offset + 3],
                    data[offset + 4],
                ]);
                offset += 5;
                Some(value)
            }
            _ => return Err("NewMiningJob invalid OPTION[u32] discriminator"),
        };
        if data.len() < offset + 36 {
            return Err("NewMiningJob too short");
        }
        let version = u32::from_le_bytes([
            data[offset],
            data[offset + 1],
            data[offset + 2],
            data[offset + 3],
        ]);
        offset += 4;
        let mut merkle_root = [0u8; 32];
        merkle_root.copy_from_slice(&data[offset..offset + 32]);
        Ok(Self {
            channel_id,
            job_id,
            min_ntime,
            version,
            merkle_root,
        })
    }
}

impl SetNewPrevHash {
    pub fn from_bytes(data: &[u8]) -> Result<Self, &'static str> {
        // SetNewPrevHash is a FIXED 48-byte payload:
        //   channel_id(4) + job_id(4) + prev_hash(32) + min_ntime(4) + nbits(4).
        // nbits is REQUIRED: it is the network target that goes into the block
        // header's `bits` field (dispatcher header[72..76]). The earlier
        // `< 48 => nbits = 0` fallback accepted a truncated message and
        // fabricated nbits=0, which produced a header that differs from the
        // pool's for that tip → EVERY share silently rejected (and bogus local
        // block-found detection). A short SetNewPrevHash is unmineable, so fail
        // closed and let the caller log + drop it instead of mining garbage.
        if data.len() < 48 {
            return Err("SetNewPrevHash too short (need 48 bytes incl. nbits)");
        }
        let channel_id = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
        let job_id = u32::from_le_bytes([data[4], data[5], data[6], data[7]]);
        let mut prev_hash = [0u8; 32];
        prev_hash.copy_from_slice(&data[8..40]);
        let min_ntime = u32::from_le_bytes([data[40], data[41], data[42], data[43]]);
        let nbits = u32::from_le_bytes([data[44], data[45], data[46], data[47]]);
        Ok(Self {
            channel_id,
            job_id,
            prev_hash,
            min_ntime,
            nbits,
        })
    }
}

pub fn sv2_target_le_to_be(target_le: &[u8]) -> Result<[u8; 32], &'static str> {
    if target_le.len() < 32 {
        return Err("SV2 target too short");
    }
    let mut target = [0u8; 32];
    for (dst, src) in target.iter_mut().zip(target_le[..32].iter().rev()) {
        *dst = *src;
    }
    Ok(target)
}

fn be256_to_f64(bytes: &[u8; 32]) -> f64 {
    let mut value = 0.0f64;
    for byte in bytes {
        value = value * 256.0 + (*byte as f64);
    }
    value
}

pub fn target_be_to_difficulty(target_be: &[u8; 32]) -> f64 {
    let target = be256_to_f64(target_be);
    if target <= 0.0 {
        return f64::INFINITY;
    }
    let diff1 = be256_to_f64(&DIFF1_TARGET_BE);
    diff1 / target
}

/// The pdiff-1 (diff-1) target in big-endian: 4 leading zero bytes then 28
/// `0xFF` bytes. Used as the reference numerator in
/// [`target_be_to_difficulty`].
const DIFF1_TARGET_BE: [u8; 32] = [
    0x00, 0x00, 0x00, 0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF,
    0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF,
];

#[cfg(test)]
mod tests {
    use super::*;

    // --- target_be_to_difficulty -------------------------------------------

    #[test]
    fn test_target_be_to_difficulty_diff1() {
        // The diff-1 reference target divides itself to exactly 1.0.
        let d = target_be_to_difficulty(&DIFF1_TARGET_BE);
        assert!(
            (d - 1.0).abs() < 1e-9,
            "diff-1 target should map to difficulty 1.0, got {d}"
        );
    }

    #[test]
    fn test_target_be_to_difficulty_zero_is_infinite() {
        // A zero target (target <= 0.0) fails to INFINITY rather than dividing
        // by zero / returning a garbage finite difficulty.
        let d = target_be_to_difficulty(&[0u8; 32]);
        assert!(d.is_infinite(), "target=0 must yield INFINITY, got {d}");
        assert!(d.is_sign_positive());
    }

    // --- sv2_target_le_to_be -----------------------------------------------

    #[test]
    fn test_sv2_target_le_to_be_too_short() {
        // 31 bytes is below the 32-byte minimum.
        let short = [0u8; 31];
        assert!(sv2_target_le_to_be(&short).is_err());
    }

    #[test]
    fn test_sv2_target_le_to_be_reverses() {
        // A deterministic LE buffer [0,1,..,31] reverses to BE [31,30,..,0].
        let mut le = [0u8; 32];
        for (i, b) in le.iter_mut().enumerate() {
            *b = i as u8;
        }
        let be = sv2_target_le_to_be(&le).expect("32 bytes must convert");
        let mut expected = [0u8; 32];
        for (i, b) in expected.iter_mut().enumerate() {
            *b = (31 - i) as u8;
        }
        assert_eq!(be, expected);
    }

    // --- NewMiningJob::from_bytes (3-way OPTION discriminator) --------------

    /// Build a NewMiningJob byte buffer. `min_ntime = None` => discriminator 0;
    /// `Some(v)` => discriminator 1 + LE u32.
    fn build_new_mining_job(channel_id: u32, job_id: u32, min_ntime: Option<u32>) -> Vec<u8> {
        let mut buf = Vec::new();
        buf.extend_from_slice(&channel_id.to_le_bytes());
        buf.extend_from_slice(&job_id.to_le_bytes());
        match min_ntime {
            None => buf.push(0),
            Some(v) => {
                buf.push(1);
                buf.extend_from_slice(&v.to_le_bytes());
            }
        }
        buf.extend_from_slice(&0x2000_0000u32.to_le_bytes()); // version
        buf.extend_from_slice(&[0xABu8; 32]); // merkle_root
        buf
    }

    #[test]
    fn test_new_mining_job_discriminator_none() {
        let buf = build_new_mining_job(7, 9, None);
        let job = NewMiningJob::from_bytes(&buf).expect("None job parses");
        assert_eq!(job.channel_id, 7);
        assert_eq!(job.job_id, 9);
        assert_eq!(job.min_ntime, None);
        assert!(job.is_future());
        assert_eq!(job.version, 0x2000_0000);
        assert_eq!(job.merkle_root, [0xABu8; 32]);
    }

    #[test]
    fn test_new_mining_job_discriminator_some() {
        let buf = build_new_mining_job(1, 2, Some(0x1234_5678));
        let job = NewMiningJob::from_bytes(&buf).expect("Some job parses");
        assert_eq!(job.min_ntime, Some(0x1234_5678));
        assert!(!job.is_future());
        assert_eq!(job.version, 0x2000_0000);
    }

    #[test]
    fn test_new_mining_job_invalid_discriminator() {
        // channel_id(4) + job_id(4) + discriminator byte = 2 (invalid).
        let mut buf = vec![0u8; 8];
        buf.push(2);
        assert!(NewMiningJob::from_bytes(&buf).is_err());
    }

    #[test]
    fn test_new_mining_job_truncated_min_ntime() {
        // discriminator = 1 but the 4 min_ntime bytes are not all present.
        let mut buf = vec![0u8; 8];
        buf.push(1); // discriminator
        buf.extend_from_slice(&[0x00, 0x11, 0x22]); // only 3 of 4 min_ntime bytes
        assert_eq!(buf.len(), 12);
        assert!(NewMiningJob::from_bytes(&buf).is_err());
    }

    // --- SetNewPrevHash::from_bytes ----------------------------------------

    #[test]
    fn test_set_new_prev_hash_44_to_47_bytes_rejected_no_nbits() {
        // A SetNewPrevHash missing (part of) its required nbits field must be
        // REJECTED, not parsed with a fabricated nbits=0 — nbits=0 lands in the
        // hashed header and silently rejects every share for the tip.
        let mut base = Vec::new();
        base.extend_from_slice(&3u32.to_le_bytes()); // channel_id
        base.extend_from_slice(&4u32.to_le_bytes()); // job_id
        base.extend_from_slice(&[0xCDu8; 32]); // prev_hash
        base.extend_from_slice(&0x6543_2100u32.to_le_bytes()); // min_ntime
        assert_eq!(base.len(), 44);
        // 44 (no nbits) through 47 (nbits truncated by 1 byte) all fail closed.
        for extra in 0..4usize {
            let mut buf = base.clone();
            buf.extend(std::iter::repeat(0u8).take(extra));
            assert_eq!(buf.len(), 44 + extra);
            assert!(
                SetNewPrevHash::from_bytes(&buf).is_err(),
                "{}-byte SetNewPrevHash (nbits incomplete) must be rejected",
                44 + extra
            );
        }
    }

    #[test]
    fn test_set_new_prev_hash_48_bytes_real_nbits() {
        let mut buf = Vec::new();
        buf.extend_from_slice(&3u32.to_le_bytes()); // channel_id
        buf.extend_from_slice(&4u32.to_le_bytes()); // job_id
        buf.extend_from_slice(&[0xCDu8; 32]); // prev_hash
        buf.extend_from_slice(&0x6543_2100u32.to_le_bytes()); // min_ntime
        buf.extend_from_slice(&0x1d00_ffffu32.to_le_bytes()); // nbits
        assert_eq!(buf.len(), 48);
        let m = SetNewPrevHash::from_bytes(&buf).expect("48 bytes parses");
        assert_eq!(m.nbits, 0x1d00_ffff, "real nbits must be parsed");
    }

    #[test]
    fn test_set_new_prev_hash_too_short() {
        // 43 bytes is below the 44-byte minimum.
        assert!(SetNewPrevHash::from_bytes(&[0u8; 43]).is_err());
    }
}
