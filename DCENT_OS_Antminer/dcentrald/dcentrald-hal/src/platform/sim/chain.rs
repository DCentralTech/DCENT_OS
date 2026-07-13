use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard};

use crate::chain_backend::Bm1397PlusChainBackend;
use crate::platform::ChainAccess;
use crate::{HalError, Result};
use serde::{Deserialize, Serialize};

use super::SimBoardProfile;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SimNoncePolicy {
    Valid,
    Invalid,
    Silent,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum TraceEvent {
    Command {
        chain_id: u8,
        bytes: Vec<u8>,
    },
    Work {
        chain_id: u8,
        bytes: Vec<u8>,
    },
    BaudChanged {
        chain_id: u8,
        baud: u32,
    },
    ResponseLengthChanged {
        chain_id: u8,
        body_len: usize,
    },
    EnumerationRequested {
        chain_id: u8,
        chip_id: u16,
        count: Option<u8>,
    },
    RegisterWrite {
        chain_id: u8,
        chip_addr: Option<u8>,
        reg: u8,
        value: u32,
    },
    RegisterRead {
        chain_id: u8,
        chip_addr: u8,
        reg: u8,
    },
    NonceGenerated {
        chain_id: u8,
        valid: bool,
    },
    I2cWrite {
        bus: u8,
        addr: u8,
        bytes: Vec<u8>,
    },
    I2cRead {
        bus: u8,
        addr: u8,
        bytes: Vec<u8>,
    },
    I2cWriteRead {
        bus: u8,
        addr: u8,
        write: Vec<u8>,
        read: Vec<u8>,
    },
    I2cTimeoutChanged {
        bus: u8,
        timeout_jiffies: u32,
    },
    I2cRecovery {
        bus: u8,
    },
    ControllerWatchdogExpired {
        at_ms: u64,
    },
}

#[derive(Debug)]
struct SimChainState {
    profile: SimBoardProfile,
    baud: u32,
    response_body_len: usize,
    registers: HashMap<(u8, u8), u32>,
    /// First word of a two-word BM1397+ register-write FIFO frame. The real
    /// FpgaChain API writes each FIFO word separately, so the simulator joins
    /// them here without changing the raw ordered command trace.
    pending_register_write: Option<(Option<u8>, u8)>,
    responses: VecDeque<Vec<u8>>,
    nonces: VecDeque<Vec<u8>>,
    trace: Vec<TraceEvent>,
    nonce_policy: SimNoncePolicy,
    next_nonce: u32,
}

#[derive(Clone)]
pub struct SimChain {
    chain_id: u8,
    state: Arc<Mutex<SimChainState>>,
}

impl SimChain {
    pub fn new(chain_id: u8, profile: SimBoardProfile) -> Self {
        Self {
            chain_id,
            state: Arc::new(Mutex::new(SimChainState {
                profile,
                baud: profile.default_baud,
                response_body_len: profile.response_length.saturating_sub(2),
                registers: HashMap::new(),
                pending_register_write: None,
                responses: VecDeque::new(),
                nonces: VecDeque::new(),
                trace: Vec::new(),
                nonce_policy: SimNoncePolicy::Silent,
                next_nonce: 1,
            })),
        }
    }

    fn lock(&self) -> Result<MutexGuard<'_, SimChainState>> {
        self.state
            .lock()
            .map_err(|_| HalError::Other("sim chain state lock poisoned".to_string()))
    }

    pub fn set_nonce_policy(&self, policy: SimNoncePolicy) -> Result<()> {
        self.lock()?.nonce_policy = policy;
        Ok(())
    }

    /// Seed the next nonce emitted by [`SimNoncePolicy::Valid`].
    ///
    /// The simulator intentionally does not invent its own SHA-256 target
    /// semantics. The headless Stratum proof validates a candidate against
    /// the exact job target, then injects that candidate here so the same
    /// nonce crosses the production FPGA work/nonce byte path.
    pub fn set_next_nonce(&self, nonce: u32) -> Result<()> {
        self.lock()?.next_nonce = nonce;
        Ok(())
    }

    pub fn baud(&self) -> Result<u32> {
        Ok(self.lock()?.baud)
    }

    pub fn drain_trace(&self) -> Result<Vec<TraceEvent>> {
        Ok(std::mem::take(&mut self.lock()?.trace))
    }

    pub fn has_nonce(&self) -> Result<bool> {
        Ok(!self.lock()?.nonces.is_empty())
    }

    pub fn has_response(&self) -> Result<bool> {
        Ok(!self.lock()?.responses.is_empty())
    }

    pub fn clear_responses(&self) -> Result<()> {
        self.lock()?.responses.clear();
        Ok(())
    }

    fn enqueue_work_nonce(state: &mut SimChainState, chain_id: u8) {
        let valid = match state.nonce_policy {
            SimNoncePolicy::Silent => return,
            SimNoncePolicy::Valid => true,
            SimNoncePolicy::Invalid => false,
        };
        let nonce = if valid { state.next_nonce } else { u32::MAX };
        state.next_nonce = state.next_nonce.wrapping_add(1);
        state.nonces.push_back(nonce.to_be_bytes().to_vec());
        state
            .trace
            .push(TraceEvent::NonceGenerated { chain_id, valid });
    }

    fn copy_next(queue: &mut VecDeque<Vec<u8>>, out: &mut [u8]) -> usize {
        let Some(frame) = queue.pop_front() else {
            return 0;
        };
        let copied = frame.len().min(out.len());
        out[..copied].copy_from_slice(&frame[..copied]);
        copied
    }
}

impl ChainAccess for SimChain {
    fn send_command(&self, data: &[u8]) -> Result<()> {
        let mut state = self.lock()?;
        state.trace.push(TraceEvent::Command {
            chain_id: self.chain_id,
            bytes: data.to_vec(),
        });

        // Concrete FpgaChain BM1387/BM1397+ register traffic arrives as raw four-byte
        // FIFO words. Track the same sparse register state used by the typed
        // backend so production-driver readback succeeds instead of timing out.
        if data.len() == 4 {
            if let Some((chip_addr, reg)) = state.pending_register_write.take() {
                let value = u32::from_be_bytes(data.try_into().expect("four bytes"));
                state
                    .registers
                    .insert((chip_addr.unwrap_or(u8::MAX), reg), value);
            } else {
                let legacy_bm1387 = state.profile.chip_id == 0x1387;
                match (data[0], data[1], legacy_bm1387) {
                    (0x51, 0x09, _) | (0x58, 0x09, _) => {
                        state.pending_register_write = Some((None, data[3]))
                    }
                    (0x41, 0x09, _) | (0x48, 0x09, _) => {
                        state.pending_register_write = Some((Some(data[2]), data[3]))
                    }
                    // Each generation ignores the other generation's read
                    // header. Enumeration deliberately transmits both 0x54
                    // and 0x52, so accepting 0x52 on BM1387 would manufacture
                    // a 64th S9 response after the 63 real-model replies.
                    (0x42, 0x05, false) | (0x52, 0x05, false) | (0x44, 0x05, true) => {
                        let chip_addr = if matches!(data[0], 0x42 | 0x44) {
                            data[2]
                        } else {
                            0
                        };
                        let reg = data[3];
                        let value = state
                            .registers
                            .get(&(chip_addr, reg))
                            .or_else(|| state.registers.get(&(u8::MAX, reg)))
                            .copied()
                            .unwrap_or_default();
                        // FpgaChain consumes two 32-bit response words. Its
                        // driver unpacks word 0 LSB-first into the register's
                        // big-endian byte value; word 1 contains metadata/CRC.
                        state.responses.push_back(value.to_be_bytes().to_vec());
                        state.responses.push_back(0_u32.to_le_bytes().to_vec());
                    }
                    _ => {}
                }
            }
        }

        // The legacy S9 path writes the unchanged FPGA FIFO word 0x00000554
        // (BM1387 GetAddress, LSB first). Feed its two-word response through
        // the same queue read by FpgaChain::read_cmd_response so HashChain's
        // concrete production enumeration path remains byte-for-byte intact.
        // Response word 0 is the live-verified S9 FPGA layout documented in
        // fpga_chain.rs: 0x00908713 decodes to chip ID 0x1387.
        if state.profile.chip_id == 0x1387 && data == 0x0000_0554_u32.to_le_bytes().as_slice() {
            let chip_id = state.profile.chip_id;
            let count = state.profile.chips_per_chain;
            state.trace.push(TraceEvent::EnumerationRequested {
                chain_id: self.chain_id,
                chip_id,
                count,
            });
            if let Some(count) = count {
                for _ in 0..count {
                    state
                        .responses
                        .push_back(0x0090_8713_u32.to_le_bytes().to_vec());
                    state.responses.push_back(0_u32.to_le_bytes().to_vec());
                }
            }
        }
        Ok(())
    }

    fn read_response(&self, buf: &mut [u8]) -> Result<usize> {
        Ok(Self::copy_next(&mut self.lock()?.responses, buf))
    }

    fn send_work(&self, data: &[u8]) -> Result<()> {
        let mut state = self.lock()?;
        state.trace.push(TraceEvent::Work {
            chain_id: self.chain_id,
            bytes: data.to_vec(),
        });
        Self::enqueue_work_nonce(&mut state, self.chain_id);
        Ok(())
    }

    fn read_nonce(&self, buf: &mut [u8]) -> Result<usize> {
        Ok(Self::copy_next(&mut self.lock()?.nonces, buf))
    }

    fn set_baud(&self, baud: u32) -> Result<()> {
        let mut state = self.lock()?;
        state.baud = baud;
        state.trace.push(TraceEvent::BaudChanged {
            chain_id: self.chain_id,
            baud,
        });
        Ok(())
    }

    fn wait_for_nonce(&self) -> Result<()> {
        Ok(())
    }
}

pub struct SimBm1397PlusBackend {
    chain: SimChain,
}

impl SimBm1397PlusBackend {
    pub fn new(chain: SimChain) -> Self {
        Self { chain }
    }

    pub fn chain(&self) -> &SimChain {
        &self.chain
    }
}

impl Bm1397PlusChainBackend for SimBm1397PlusBackend {
    fn set_baud_rate(&self, baud: u32) -> Result<()> {
        self.chain.set_baud(baud)
    }

    fn set_response_body_len(&self, body_len: usize) -> Result<()> {
        let mut state = self.chain.lock()?;
        state.response_body_len = body_len;
        state.trace.push(TraceEvent::ResponseLengthChanged {
            chain_id: self.chain.chain_id,
            body_len,
        });
        Ok(())
    }

    fn send_get_address_bm1397plus(&self) -> Result<()> {
        let mut state = self.chain.lock()?;
        let chip_id = state.profile.chip_id;
        let count = state.profile.chips_per_chain;
        state.trace.push(TraceEvent::EnumerationRequested {
            chain_id: self.chain.chain_id,
            chip_id,
            count,
        });
        if let Some(count) = count {
            for _ in 0..count {
                let mut body = vec![0_u8; state.response_body_len];
                if body.len() >= 4 {
                    body[0..4].copy_from_slice(&state.profile.enumeration_identity.to_be_bytes());
                }
                if body.len() >= 7 {
                    body[4..7].copy_from_slice(&state.profile.enumeration_suffix);
                }
                state.responses.push_back(body);
            }
        }
        Ok(())
    }

    fn send_chain_inactive_bm1397plus(&self) -> Result<()> {
        self.chain.send_command(&[0x53])
    }

    fn send_set_address_bm1397plus(&self, addr: u8) -> Result<()> {
        self.chain.send_command(&[0x40, addr])
    }

    fn send_write_reg_broadcast_bm1397plus(&self, reg: u8, value: u32) -> Result<()> {
        let mut state = self.chain.lock()?;
        state.registers.insert((u8::MAX, reg), value);
        state.trace.push(TraceEvent::RegisterWrite {
            chain_id: self.chain.chain_id,
            chip_addr: None,
            reg,
            value,
        });
        Ok(())
    }

    fn send_write_reg_bm1397plus(&self, chip_addr: u8, reg: u8, value: u32) -> Result<()> {
        let mut state = self.chain.lock()?;
        state.registers.insert((chip_addr, reg), value);
        state.trace.push(TraceEvent::RegisterWrite {
            chain_id: self.chain.chain_id,
            chip_addr: Some(chip_addr),
            reg,
            value,
        });
        Ok(())
    }

    fn send_read_reg_bm1397plus(&self, chip_addr: u8, reg: u8) -> Result<()> {
        let mut state = self.chain.lock()?;
        let value = state
            .registers
            .get(&(chip_addr, reg))
            .or_else(|| state.registers.get(&(u8::MAX, reg)))
            .copied()
            .unwrap_or_default();
        let mut body = vec![0_u8; state.response_body_len];
        if body.len() >= 6 {
            body[0] = chip_addr;
            body[1] = reg;
            body[2..6].copy_from_slice(&value.to_be_bytes());
        }
        state.responses.push_back(body);
        state.trace.push(TraceEvent::RegisterRead {
            chain_id: self.chain.chain_id,
            chip_addr,
            reg,
        });
        Ok(())
    }

    fn read_response_frame(&self, out: &mut [u8], _timeout_ms: u64) -> Result<usize> {
        self.chain.read_response(out)
    }

    fn read_all_responses(&self, _max_wait_ms: u64) -> Result<Vec<Vec<u8>>> {
        Ok(self.chain.lock()?.responses.drain(..).collect())
    }

    fn send_work_frame(&self, frame: &[u8]) -> Result<()> {
        self.chain.send_work(frame)
    }

    fn poll_nonce_frame(&self, out: &mut [u8], _timeout_ms: u64) -> Result<usize> {
        self.chain.read_nonce(out)
    }

    fn chain_id(&self) -> u8 {
        self.chain.chain_id
    }

    fn transport_label(&self) -> &'static str {
        "sim-loopback"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::sim::{SimBoardProfile, SimModel};

    #[test]
    fn enumeration_uses_selected_model_geometry() {
        let chain = SimChain::new(0, SimBoardProfile::for_model(SimModel::S19Pro));
        let backend = SimBm1397PlusBackend::new(chain);
        backend
            .send_get_address_bm1397plus()
            .expect("enumeration request");
        let replies = backend.read_all_responses(0).expect("enumeration replies");
        assert_eq!(replies.len(), 114);
        assert_eq!(&replies[0][..2], &0x1398_u16.to_be_bytes());
        assert!(replies
            .iter()
            .all(|reply| &reply[..4] == 0x1398_1800_u32.to_be_bytes().as_slice()));
    }

    #[test]
    fn s17_enumeration_matches_held_saleae_identity_frame() {
        let chain = SimChain::new(0, SimBoardProfile::for_model(SimModel::S17));
        let backend = SimBm1397PlusBackend::new(chain);
        backend
            .send_get_address_bm1397plus()
            .expect("enumeration request");
        let replies = backend.read_all_responses(0).expect("enumeration replies");
        assert_eq!(replies.len(), 48);
        assert!(replies
            .iter()
            .all(|reply| reply.as_slice() == [0x13, 0x97, 0x18, 0x00, 0x00, 0x00, 0x06]));
    }

    #[test]
    fn sparse_register_file_round_trips() {
        let chain = SimChain::new(0, SimBoardProfile::for_model(SimModel::S19Pro));
        let backend = SimBm1397PlusBackend::new(chain);
        backend
            .send_write_reg_bm1397plus(4, 0x08, 0x1234_5678)
            .expect("register write");
        backend
            .send_read_reg_bm1397plus(4, 0x08)
            .expect("register read");
        let mut response = [0_u8; 16];
        let len = backend
            .read_response_frame(&mut response, 0)
            .expect("response");
        assert!(len >= 6);
        assert_eq!(&response[2..6], &0x1234_5678_u32.to_be_bytes());
    }

    #[test]
    fn concrete_fpga_fifo_register_write_then_read_round_trips() {
        let chain = SimChain::new(0, SimBoardProfile::for_model(SimModel::S19Pro));
        chain
            .send_command(&[0x51, 0x09, 0x00, 0x08])
            .expect("write header");
        chain
            .send_command(&0x4068_0221_u32.to_be_bytes())
            .expect("write value");
        chain
            .send_command(&[0x42, 0x05, 0x00, 0x08])
            .expect("read command");
        let mut first = [0_u8; 4];
        let mut second = [0_u8; 4];
        assert_eq!(chain.read_response(&mut first).unwrap(), 4);
        assert_eq!(chain.read_response(&mut second).unwrap(), 4);
        assert_eq!(first, 0x4068_0221_u32.to_be_bytes());
        assert_eq!(second, [0; 4]);
    }

    #[test]
    fn nonce_policy_covers_valid_invalid_and_silent() {
        let chain = SimChain::new(0, SimBoardProfile::for_model(SimModel::S19Pro));
        let backend = SimBm1397PlusBackend::new(chain.clone());
        let mut out = [0_u8; 8];

        chain
            .set_nonce_policy(SimNoncePolicy::Silent)
            .expect("silent policy");
        backend.send_work_frame(&[1]).expect("silent work");
        assert_eq!(
            backend.poll_nonce_frame(&mut out, 0).expect("silent poll"),
            0
        );

        chain
            .set_nonce_policy(SimNoncePolicy::Valid)
            .expect("valid policy");
        backend.send_work_frame(&[2]).expect("valid work");
        assert_eq!(
            backend.poll_nonce_frame(&mut out, 0).expect("valid poll"),
            4
        );
        assert_ne!(&out[..4], &u32::MAX.to_be_bytes());

        chain
            .set_nonce_policy(SimNoncePolicy::Invalid)
            .expect("invalid policy");
        backend.send_work_frame(&[3]).expect("invalid work");
        assert_eq!(
            backend.poll_nonce_frame(&mut out, 0).expect("invalid poll"),
            4
        );
        assert_eq!(&out[..4], &u32::MAX.to_be_bytes());
    }

    #[test]
    fn trace_event_has_stable_tagged_json_shape() {
        let event = TraceEvent::BaudChanged {
            chain_id: 2,
            baud: 3_125_000,
        };
        let json = serde_json::to_value(event).expect("serialize trace event");
        assert_eq!(json["event"], "baud_changed");
        assert_eq!(json["chain_id"], 2);
        assert_eq!(json["baud"], 3_125_000);
    }
}
