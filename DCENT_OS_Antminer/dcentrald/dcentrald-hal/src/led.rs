//! Async LED engine for front-panel and diagnostic LEDs.
//!
//! Drives the 4 control board LEDs via a dedicated tokio task:
//!   D5 (Green):        User-facing status — mining heartbeat, share flash, locate
//!   D6 (Red):          User-facing alerts — errors, warnings, share rejected
//!   D7 (Red Internal): Daemon alive heartbeat (1Hz toggle, independent)
//!   D8:                Mining pipeline heartbeat (toggles on work dispatch)
//!
//! The engine receives commands via an mpsc channel and manages all timing
//! internally. Background patterns (mining heartbeat, error blink) run
//! continuously. Locate sequences and flash events temporarily override the
//! background, then resume it.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, watch};
use tokio_util::sync::CancellationToken;

use crate::gpio::{GpioController, Led};
use crate::led_patterns;

// ---------------------------------------------------------------------------
// Data types
// ---------------------------------------------------------------------------

/// A single frame in an LED blink sequence.
#[derive(Debug, Clone, Copy)]
pub struct LedFrame {
    pub green: bool,
    pub red: bool,
    pub duration_ms: u16,
}

/// A complete LED blink sequence (used for "Find My Miner" patterns).
#[derive(Debug, Clone)]
pub struct BlinkSequence {
    pub name: &'static str,
    pub id: &'static str,
    pub description: &'static str,
    pub frames: &'static [LedFrame],
}

impl BlinkSequence {
    /// Total duration of one playthrough in milliseconds.
    pub fn duration_ms(&self) -> u32 {
        self.frames.iter().map(|f| f.duration_ms as u32).sum()
    }
}

/// Background LED patterns that reflect daemon/mining state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LedPattern {
    /// Booting: quick green-red-both-off wipe animation.
    Booting,
    /// Initializing chains: green blinks 2Hz.
    Initializing,
    /// Normal mining: green blinks ~1Hz (heartbeat, temp-proportional).
    Mining,
    /// Error: red solid.
    Error,
    /// Fan failure: red blinks fast (3Hz).
    FanFailure,
    /// Thermal warning: red blinks slow (1Hz).
    ThermalWarning,
    /// Pool disconnected: green/red alternate (0.5Hz).
    PoolDisconnected,
    /// Shutdown: all off.
    Shutdown,
    /// Sleep mode: green blinks very slowly (0.2Hz, ~5s cycle).
    Sleep,
    /// Firmware update: alternating green/red fast.
    FirmwareUpdate,
}

/// Commands sent to the LED engine via mpsc channel.
#[derive(Debug, Clone)]
pub enum LedCommand {
    /// Change the persistent background pattern.
    SetPattern(LedPattern),
    /// Trigger "Find My Miner" — plays a named blink sequence then resumes background.
    Locate { pattern_id: String },
    /// Cancel an active locate sequence.
    StopLocate,
    /// Brief green flash (share accepted).
    FlashGreen { duration_ms: u16 },
    /// Brief red flash (share rejected).
    FlashRed { duration_ms: u16 },
    /// Brief both-LED flash (new block from pool).
    FlashBoth { duration_ms: u16 },
    /// Lucky share or block found celebration.
    Celebration,
    /// A chain came online during init (flash green N times for chain N).
    ChainOnline(u8),
    /// Update current chip temperature (adjusts heartbeat rate).
    SetTemperature(f32),
    /// Enable/disable night mode (turns off user-facing LEDs).
    NightMode(bool),
    /// Enter/exit firmware update mode.
    FirmwareUpdate(bool),
    /// Toggle D8 (called from work dispatch loop as pipeline heartbeat).
    TogglePipelineHeartbeat,
}

/// Current state of the LED engine, readable via API.
#[derive(Debug, Clone, Serialize)]
pub struct LedStatus {
    pub enabled: bool,
    pub current_pattern: LedPattern,
    pub locate_active: bool,
    pub locate_remaining_s: Option<u8>,
    pub night_mode_active: bool,
    pub temperature_c: f32,
}

// ---------------------------------------------------------------------------
// LED Engine
// ---------------------------------------------------------------------------

/// Configuration passed to the LED engine at startup.
#[derive(Debug, Clone)]
pub struct LedEngineConfig {
    pub enabled: bool,
    pub heartbeat_on_ms: u16,
    pub heartbeat_off_ms: u16,
    pub locate_pattern: String,
    pub locate_duration_s: u8,
    pub flash_on_accepted_share: bool,
    pub flash_on_rejected_share: bool,
    pub night_mode_disable: bool,
    pub celebration_on_lucky_share: bool,
    pub chain_status_blink_codes: bool,
}

impl Default for LedEngineConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            heartbeat_on_ms: 100,
            heartbeat_off_ms: 900,
            locate_pattern: "imperial_march".to_string(),
            locate_duration_s: 30,
            flash_on_accepted_share: true,
            flash_on_rejected_share: true,
            night_mode_disable: false,
            celebration_on_lucky_share: true,
            chain_status_blink_codes: true,
        }
    }
}

/// Async LED engine that runs as a tokio task.
pub struct LedEngine {
    gpio: Arc<GpioController>,
    cmd_rx: mpsc::Receiver<LedCommand>,
    cancel: CancellationToken,
    config: LedEngineConfig,

    // State
    current_pattern: LedPattern,
    night_mode: bool,
    temperature_c: f32,
    locate_active: bool,
    locate_started: Option<Instant>,

    // Heartbeat phase tracking
    heartbeat_on: bool,
    /// D7 heartbeat phase: 0=lub-ON, 1=lub-OFF, 2=DUB-ON, 3=DUB-OFF(pause)
    d7_phase: u8,

    // Watch channel for publishing live status to API consumers.
    status_tx: watch::Sender<LedStatus>,
}

impl LedEngine {
    /// Create a new LED engine.
    ///
    /// Returns `(engine, status_rx)` where `status_rx` is a watch channel
    /// receiver that always holds the latest `LedStatus` snapshot. Pass it
    /// to the API layer for `GET /api/led/status`.
    pub fn new(
        gpio: Arc<GpioController>,
        cmd_rx: mpsc::Receiver<LedCommand>,
        cancel: CancellationToken,
        config: LedEngineConfig,
    ) -> (Self, watch::Receiver<LedStatus>) {
        let initial_status = LedStatus {
            enabled: config.enabled,
            current_pattern: LedPattern::Booting,
            locate_active: false,
            locate_remaining_s: None,
            night_mode_active: false,
            temperature_c: 40.0,
        };
        let (status_tx, status_rx) = watch::channel(initial_status);

        let engine = Self {
            gpio,
            cmd_rx,
            cancel,
            config,
            current_pattern: LedPattern::Booting,
            night_mode: false,
            temperature_c: 40.0,
            locate_active: false,
            locate_started: None,
            heartbeat_on: false,
            d7_phase: 0,
            status_tx,
        };
        (engine, status_rx)
    }

    /// Get the current status snapshot.
    pub fn status(&self) -> LedStatus {
        let locate_remaining = self.locate_started.map(|start| {
            let elapsed = start.elapsed().as_secs() as u8;
            self.config.locate_duration_s.saturating_sub(elapsed)
        });

        LedStatus {
            enabled: self.config.enabled,
            current_pattern: self.current_pattern,
            locate_active: self.locate_active,
            locate_remaining_s: locate_remaining,
            night_mode_active: self.night_mode,
            temperature_c: self.temperature_c,
        }
    }

    /// Publish the current status snapshot to the watch channel.
    fn publish_status(&self) {
        let _ = self.status_tx.send(self.status());
    }

    /// Main engine loop. Runs until cancellation token fires.
    pub async fn run(&mut self) {
        tracing::info!("LED engine started");

        // Play boot animation
        if self.config.enabled {
            self.play_boot_animation().await;
        }

        // D7 daemon heartbeat — lub-DUB pattern like a real heartbeat.
        // Phase 0: ON  100ms (lub)
        // Phase 1: OFF 100ms
        // Phase 2: ON  150ms (DUB)
        // Phase 3: OFF 650ms (pause)
        // Total cycle: 1000ms
        let d7_sleep = tokio::time::sleep(Duration::from_millis(100));
        tokio::pin!(d7_sleep);

        // Background pattern tick — pinned sleep that persists across select iterations.
        // Without pinning, the D7 interval (500ms) would restart the sleep every tick,
        // preventing the 900ms off-phase from ever completing.
        let pattern_sleep = tokio::time::sleep(self.current_tick_duration());
        tokio::pin!(pattern_sleep);

        loop {
            tokio::select! {
                _ = self.cancel.cancelled() => {
                    tracing::info!("LED engine shutting down");
                    self.all_off();
                    break;
                }

                // D7 daemon heartbeat — lub-DUB double-pulse
                _ = &mut d7_sleep => {
                    let (on, next_ms) = match self.d7_phase {
                        0 => (true,  100), // lub ON
                        1 => (false, 100), // gap
                        2 => (true,  150), // DUB ON
                        _ => (false, 650), // pause
                    };
                    self.gpio.set_led(Led::RedInternal, on);
                    self.d7_phase = (self.d7_phase + 1) % 4;
                    d7_sleep.as_mut().reset(tokio::time::Instant::now() + Duration::from_millis(next_ms));
                }

                // Background pattern tick — only resets after it fires
                _ = &mut pattern_sleep => {
                    if self.config.enabled && !self.locate_active {
                        self.tick_background_pattern();
                    }
                    // Reset for next tick
                    pattern_sleep.as_mut().reset(tokio::time::Instant::now() + self.current_tick_duration());
                }

                // Command from daemon/API
                cmd = self.cmd_rx.recv() => {
                    match cmd {
                        Some(cmd) => self.handle_command(cmd).await,
                        None => {
                            tracing::warn!("LED command channel closed");
                            break;
                        }
                    }
                }
            }
        }
    }

    /// Handle an incoming command.
    async fn handle_command(&mut self, cmd: LedCommand) {
        match cmd {
            LedCommand::SetPattern(pattern) => {
                if !self.locate_active {
                    tracing::debug!(?pattern, "LED pattern changed");
                    self.current_pattern = pattern;
                    // Apply immediately
                    self.tick_background_pattern();
                } else {
                    // Queue for after locate finishes
                    self.current_pattern = pattern;
                }
                self.publish_status();
            }

            LedCommand::Locate { pattern_id } => {
                let id = if pattern_id.is_empty() {
                    self.config.locate_pattern.clone()
                } else {
                    pattern_id
                };
                tracing::info!(pattern = %id, "Find My Miner triggered");
                self.publish_status();
                self.run_locate(&id).await;
                // run_locate sets locate_active=false when done; publish final state
                self.publish_status();
            }

            LedCommand::StopLocate => {
                if self.locate_active {
                    tracing::info!("Locate stopped by user");
                    self.locate_active = false;
                    self.locate_started = None;
                    self.tick_background_pattern();
                    self.publish_status();
                }
            }

            LedCommand::FlashGreen { duration_ms } => {
                if self.config.enabled && self.config.flash_on_accepted_share && !self.locate_active
                {
                    self.flash(Led::Green, duration_ms).await;
                }
            }

            LedCommand::FlashRed { duration_ms } => {
                if self.config.enabled && self.config.flash_on_rejected_share && !self.locate_active
                {
                    self.flash(Led::Red, duration_ms).await;
                }
            }

            LedCommand::FlashBoth { duration_ms } => {
                if self.config.enabled && !self.locate_active {
                    self.flash_both(duration_ms).await;
                }
            }

            LedCommand::Celebration => {
                if self.config.enabled
                    && self.config.celebration_on_lucky_share
                    && !self.locate_active
                {
                    self.play_celebration().await;
                }
            }

            LedCommand::ChainOnline(chain_index) => {
                if self.config.enabled && self.config.chain_status_blink_codes {
                    self.play_chain_online(chain_index).await;
                }
            }

            LedCommand::SetTemperature(temp) => {
                self.temperature_c = temp;
                // Publish periodically on temp update (avoids stale status)
                self.publish_status();
            }

            LedCommand::NightMode(enabled) => {
                self.night_mode = enabled && self.config.night_mode_disable;
                if self.night_mode && !self.locate_active {
                    // Turn off user-facing LEDs
                    self.gpio.set_led(Led::Green, false);
                    self.gpio.set_led(Led::Red, false);
                }
                self.publish_status();
            }

            LedCommand::FirmwareUpdate(active) => {
                if active {
                    self.current_pattern = LedPattern::FirmwareUpdate;
                } else {
                    self.current_pattern = LedPattern::Mining;
                }
                self.publish_status();
            }

            LedCommand::TogglePipelineHeartbeat => {
                self.gpio.toggle_led(Led::D8);
            }
        }
    }

    /// Calculate how long before the next background tick based on pattern and temperature.
    fn current_tick_duration(&self) -> Duration {
        match self.current_pattern {
            LedPattern::Mining => {
                // Temperature-proportional heartbeat.
                // At 55C (target): ~1Hz (1000ms cycle = 100on + 900off)
                // At 65C (hot): ~2Hz (500ms cycle)
                // At 75C (dangerous): ~4Hz (250ms cycle)
                let base_cycle =
                    (self.config.heartbeat_on_ms + self.config.heartbeat_off_ms) as f32;
                let scale = if self.temperature_c <= 55.0 {
                    1.0
                } else if self.temperature_c >= 75.0 {
                    0.25
                } else {
                    // Linear interpolation: 1.0 at 55C, 0.25 at 75C
                    1.0 - (self.temperature_c - 55.0) / 20.0 * 0.75
                };
                let cycle_ms = (base_cycle * scale).max(100.0) as u64;
                if self.heartbeat_on {
                    // Currently on, wait the proportional on-time
                    let on_ratio = self.config.heartbeat_on_ms as f32 / base_cycle;
                    Duration::from_millis((cycle_ms as f32 * on_ratio).max(30.0) as u64)
                } else {
                    // Currently off, wait the proportional off-time
                    let off_ratio = self.config.heartbeat_off_ms as f32 / base_cycle;
                    Duration::from_millis((cycle_ms as f32 * off_ratio).max(50.0) as u64)
                }
            }
            LedPattern::Initializing => {
                // 2Hz blink: 250ms on, 250ms off
                Duration::from_millis(250)
            }
            LedPattern::FanFailure => {
                // 3Hz blink: ~167ms per phase
                Duration::from_millis(167)
            }
            LedPattern::ThermalWarning => {
                // 1Hz blink: 500ms per phase
                Duration::from_millis(500)
            }
            LedPattern::PoolDisconnected => {
                // 0.5Hz alternate: 1000ms per phase
                Duration::from_millis(1000)
            }
            LedPattern::Sleep => {
                // 0.2Hz: 2500ms per phase (5s cycle)
                Duration::from_millis(2500)
            }
            LedPattern::FirmwareUpdate => {
                // Fast alternate: 300ms per phase
                Duration::from_millis(300)
            }
            LedPattern::Booting | LedPattern::Error | LedPattern::Shutdown => {
                // Static patterns, tick slowly just to stay responsive
                Duration::from_millis(500)
            }
        }
    }

    /// Advance the background pattern by one tick.
    fn tick_background_pattern(&mut self) {
        if self.night_mode && !self.locate_active {
            self.gpio.set_led(Led::Green, false);
            self.gpio.set_led(Led::Red, false);
            return;
        }

        match self.current_pattern {
            LedPattern::Mining => {
                self.heartbeat_on = !self.heartbeat_on;
                self.gpio.set_led(Led::Green, self.heartbeat_on);
                self.gpio.set_led(Led::Red, false);
            }
            LedPattern::Initializing => {
                self.heartbeat_on = !self.heartbeat_on;
                self.gpio.set_led(Led::Green, self.heartbeat_on);
                self.gpio.set_led(Led::Red, false);
            }
            LedPattern::Error => {
                self.gpio.set_led(Led::Green, false);
                self.gpio.set_led(Led::Red, true);
            }
            LedPattern::FanFailure => {
                self.heartbeat_on = !self.heartbeat_on;
                self.gpio.set_led(Led::Green, false);
                self.gpio.set_led(Led::Red, self.heartbeat_on);
            }
            LedPattern::ThermalWarning => {
                self.heartbeat_on = !self.heartbeat_on;
                self.gpio.set_led(Led::Green, false);
                self.gpio.set_led(Led::Red, self.heartbeat_on);
            }
            LedPattern::PoolDisconnected => {
                // Alternate green/red
                self.heartbeat_on = !self.heartbeat_on;
                self.gpio.set_led(Led::Green, self.heartbeat_on);
                self.gpio.set_led(Led::Red, !self.heartbeat_on);
            }
            LedPattern::Shutdown => {
                self.gpio.set_led(Led::Green, false);
                self.gpio.set_led(Led::Red, false);
            }
            LedPattern::Sleep => {
                self.heartbeat_on = !self.heartbeat_on;
                self.gpio.set_led(Led::Green, self.heartbeat_on);
                self.gpio.set_led(Led::Red, false);
            }
            LedPattern::FirmwareUpdate => {
                self.heartbeat_on = !self.heartbeat_on;
                self.gpio.set_led(Led::Green, self.heartbeat_on);
                self.gpio.set_led(Led::Red, !self.heartbeat_on);
            }
            LedPattern::Booting => {
                // Static during boot — animation is played once in play_boot_animation()
            }
        }
    }

    /// Play the boot animation (once, at startup).
    async fn play_boot_animation(&self) {
        let frames: &[(bool, bool, u64)] = &[
            (true, false, 200),  // Green
            (true, true, 200),   // Both
            (false, true, 200),  // Red
            (false, false, 200), // Off
        ];
        for &(green, red, ms) in frames {
            self.gpio.set_led(Led::Green, green);
            self.gpio.set_led(Led::Red, red);
            tokio::time::sleep(Duration::from_millis(ms)).await;
        }
    }

    /// Run a "Find My Miner" locate sequence.
    async fn run_locate(&mut self, pattern_id: &str) {
        let sequence = match led_patterns::find_pattern(pattern_id) {
            Some(seq) => seq,
            None => {
                tracing::warn!(pattern = %pattern_id, "Unknown locate pattern, using imperial_march");
                led_patterns::find_pattern("imperial_march").unwrap()
            }
        };

        self.locate_active = true;
        let start = Instant::now();
        self.locate_started = Some(start);
        let max_duration = Duration::from_secs(self.config.locate_duration_s as u64);

        tracing::info!(
            pattern = sequence.name,
            duration_s = self.config.locate_duration_s,
            "Playing locate sequence"
        );

        // Loop the pattern until duration expires or StopLocate received
        'outer: while start.elapsed() < max_duration && self.locate_active {
            for frame in sequence.frames {
                if !self.locate_active || start.elapsed() >= max_duration {
                    break 'outer;
                }

                self.gpio.set_led(Led::Green, frame.green);
                self.gpio.set_led(Led::Red, frame.red);

                // Sleep for frame duration, but check for commands
                let frame_dur = Duration::from_millis(frame.duration_ms as u64);
                let sleep_start = Instant::now();

                while sleep_start.elapsed() < frame_dur {
                    let remaining = frame_dur.saturating_sub(sleep_start.elapsed());
                    // Check for StopLocate or cancel every 50ms within a frame
                    let check_interval = remaining.min(Duration::from_millis(50));

                    tokio::select! {
                        _ = self.cancel.cancelled() => {
                            self.locate_active = false;
                            break 'outer;
                        }
                        cmd = self.cmd_rx.recv() => {
                            match cmd {
                                Some(LedCommand::StopLocate) => {
                                    self.locate_active = false;
                                    break 'outer;
                                }
                                Some(LedCommand::SetPattern(p)) => {
                                    // Queue for after locate
                                    self.current_pattern = p;
                                }
                                Some(LedCommand::SetTemperature(t)) => {
                                    self.temperature_c = t;
                                }
                                Some(LedCommand::TogglePipelineHeartbeat) => {
                                    self.gpio.toggle_led(Led::D8);
                                }
                                Some(_) => {} // Ignore other commands during locate
                                None => {
                                    self.locate_active = false;
                                    break 'outer;
                                }
                            }
                        }
                        _ = tokio::time::sleep(check_interval) => {}
                    }
                }
            }
        }

        self.locate_active = false;
        self.locate_started = None;
        tracing::info!("Locate sequence finished, resuming background pattern");

        // Resume background
        self.tick_background_pattern();
    }

    /// Brief single-LED flash (non-blocking for caller via channel, but blocks engine briefly).
    async fn flash(&mut self, led: Led, duration_ms: u16) {
        if self.night_mode {
            return;
        }
        // Save current state
        let was_on = self.gpio.read_led(led);

        // Flash
        self.gpio.set_led(led, true);
        tokio::time::sleep(Duration::from_millis(duration_ms as u64)).await;

        // Restore
        self.gpio.set_led(led, was_on);
    }

    /// Brief both-LED flash (new block from pool, both green + red).
    async fn flash_both(&mut self, duration_ms: u16) {
        if self.night_mode {
            return;
        }
        let was_green = self.gpio.read_led(Led::Green);
        let was_red = self.gpio.read_led(Led::Red);

        self.gpio.set_led(Led::Green, true);
        self.gpio.set_led(Led::Red, true);
        tokio::time::sleep(Duration::from_millis(duration_ms as u64)).await;

        self.gpio.set_led(Led::Green, was_green);
        self.gpio.set_led(Led::Red, was_red);
    }

    /// Play a celebration pattern (lucky share / block found).
    async fn play_celebration(&mut self) {
        if self.night_mode {
            return;
        }
        // 3 rapid both-LED flashes
        for _ in 0..3 {
            self.gpio.set_led(Led::Green, true);
            self.gpio.set_led(Led::Red, true);
            tokio::time::sleep(Duration::from_millis(100)).await;
            self.gpio.set_led(Led::Green, false);
            self.gpio.set_led(Led::Red, false);
            tokio::time::sleep(Duration::from_millis(80)).await;
        }
        // Long hold
        self.gpio.set_led(Led::Green, true);
        self.gpio.set_led(Led::Red, true);
        tokio::time::sleep(Duration::from_millis(400)).await;

        // Resume
        self.tick_background_pattern();
    }

    /// Flash green N times when chain N comes online during init.
    async fn play_chain_online(&mut self, chain_index: u8) {
        // chain_index: 6, 7, or 8 → flash 1, 2, or 3 times
        let flashes = match chain_index {
            6 => 1,
            7 => 2,
            8 => 3,
            _ => 1,
        };

        for _ in 0..flashes {
            self.gpio.set_led(Led::Green, true);
            tokio::time::sleep(Duration::from_millis(150)).await;
            self.gpio.set_led(Led::Green, false);
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    /// Turn all LEDs off.
    fn all_off(&self) {
        self.gpio.set_led(Led::Green, false);
        self.gpio.set_led(Led::Red, false);
        self.gpio.set_led(Led::RedInternal, false);
        self.gpio.set_led(Led::D8, false);
    }
}
