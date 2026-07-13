use std::sync::{Arc, Mutex};

use crate::platform::GpioAccess;

#[derive(Clone)]
pub struct SimGpio {
    present: [bool; 3],
    reset: Arc<Mutex<[bool; 3]>>,
}

impl SimGpio {
    pub fn new(chain_count: u8) -> Self {
        let mut present = [false; 3];
        for slot in present.iter_mut().take(usize::from(chain_count.min(3))) {
            *slot = true;
        }
        Self {
            present,
            reset: Arc::new(Mutex::new([false; 3])),
        }
    }
}

impl GpioAccess for SimGpio {
    fn read_plug_detect(&self) -> [bool; 3] {
        self.present
    }

    fn set_board_reset(&self, chain: u8, assert_reset: bool) {
        if let Ok(mut reset) = self.reset.lock() {
            if let Some(state) = reset.get_mut(chain as usize) {
                *state = assert_reset;
            }
        }
    }
}
