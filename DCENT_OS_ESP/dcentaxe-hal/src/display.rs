//! Display driver for DCENT_axe boards.
//!
//! When `display-ssd1306` feature is active: full SSD1306 OLED driver (128x32, I2C).
//! When `display-none` feature is active: no-op stub with the same API (headless boards).

// ============================================================================
// Headless stub — all display calls become no-ops, saves ~20KB flash
// ============================================================================
#[cfg(feature = "display-none")]
mod headless {
    use crate::i2c::I2cBus;

    #[derive(Debug, Clone)]
    pub struct OledNotification {
        pub line0: String,
        pub line1: String,
        pub line2: String,
        pub line3: String,
        pub urgent: bool,
    }

    pub struct Ssd1306Display {
        pub initialized: bool,
        pub inverted: bool,
        pub notifications: Vec<OledNotification>,
    }

    impl Ssd1306Display {
        pub fn new() -> Self {
            Self {
                initialized: false,
                inverted: false,
                notifications: Vec::new(),
            }
        }
        pub fn notify(&mut self, _l0: &str, _l1: &str, _l2: &str, _l3: &str) {}
        pub fn notify_urgent(&mut self, _l0: &str, _l1: &str, _l2: &str, _l3: &str) {}
        pub fn has_urgent_notification(&self) -> bool {
            false
        }
        pub fn show_notification_if_pending(&mut self, _i2c: &mut I2cBus<'_>) -> bool {
            false
        }
        pub fn init(&mut self, _i2c: &mut I2cBus<'_>) -> Result<(), crate::i2c::I2cError> {
            log::info!("Headless board — no display");
            Ok(())
        }
        pub fn set_flip(
            &mut self,
            _i2c: &mut I2cBus<'_>,
            _flip: bool,
        ) -> Result<(), crate::i2c::I2cError> {
            Ok(())
        }
        pub fn clear(&mut self) {}
        pub fn flush(&self, _i2c: &mut I2cBus<'_>) -> Result<(), crate::i2c::I2cError> {
            Ok(())
        }
        pub fn draw_char(&mut self, _x: usize, _y: usize, _ch: char) {}
        pub fn draw_text(&mut self, _x: usize, _y: usize, _text: &str) {}
        pub fn draw_centered(&mut self, _y: usize, _text: &str) {}
        pub fn set_pixel(&mut self, _x: usize, _y: usize, _on: bool) {}
        pub fn draw_hline(&mut self, _x: usize, _y: usize, _len: usize) {}
        pub fn draw_vline(&mut self, _x: usize, _y: usize, _len: usize) {}
        pub fn invert_display(&self, _i2c: &mut I2cBus<'_>, _invert: bool) {}
        pub fn set_contrast(&self, _i2c: &mut I2cBus<'_>, _brightness: u8) {}
        pub fn draw_bitmap(&mut self, _x: usize, _y: usize, _data: &[u8], _w: usize, _h: usize) {}
        pub fn draw_bitmap_2x(&mut self, _x: usize, _y: usize, _data: &[u8], _w: usize, _h: usize) {
        }
        pub fn draw_sparkline(&mut self, _x: usize, _y: usize, _values: &[f32], _bar_width: usize) {
        }
        pub fn draw_progress_bar(&mut self, _x: usize, _y: usize, _width: usize, _ratio: f32) {}
        pub fn show_status(
            &mut self,
            _i2c: &mut I2cBus<'_>,
            _l0: &str,
            _l1: &str,
            _l2: &str,
            _l3: &str,
        ) {
        }
    }
}

#[cfg(feature = "display-none")]
pub use headless::*;

// ============================================================================
// Full SSD1306 OLED driver
// ============================================================================
#[cfg(feature = "display-ssd1306")]
use crate::i2c::{I2cBus, I2cError};

#[cfg(feature = "display-ssd1306")]
/// SSD1306 I2C address (0x3C is standard for BitAxe OLED)
const SSD1306_ADDR: u8 = 0x3C;

#[cfg(feature = "display-ssd1306")]
const WIDTH: usize = 128;
#[cfg(feature = "display-ssd1306")]
const HEIGHT: usize = 32;
#[cfg(feature = "display-ssd1306")]
const PAGES: usize = HEIGHT / 8;

#[cfg(feature = "display-ssd1306")]
const CMD_SINGLE: u8 = 0x00;
#[cfg(feature = "display-ssd1306")]
const CMD_STREAM: u8 = 0x00;
#[cfg(feature = "display-ssd1306")]
const DATA_STREAM: u8 = 0x40;

#[cfg(feature = "display-ssd1306")]
#[derive(Debug, Clone)]
pub struct OledNotification {
    pub line0: String,
    pub line1: String,
    pub line2: String,
    pub line3: String,
    pub urgent: bool,
}

#[cfg(feature = "display-ssd1306")]
pub struct Ssd1306Display {
    /// Framebuffer: 4 pages of 128 bytes each (1 bit per pixel, column-major)
    framebuf: [u8; WIDTH * PAGES],
    /// Whether the display was successfully initialized
    pub initialized: bool,
    /// Whether the display is flipped 180 degrees
    pub inverted: bool,
    /// Notification queue — shown for 1 display tick then removed
    pub notifications: Vec<OledNotification>,
}

#[cfg(feature = "display-ssd1306")]
impl Ssd1306Display {
    /// Create a new display instance (uninitialized).
    pub fn new() -> Self {
        Self {
            framebuf: [0; WIDTH * PAGES],
            initialized: false,
            inverted: false,
            notifications: Vec::new(),
        }
    }

    /// Push a non-urgent notification to display on a later OLED update tick.
    /// Keep this queue small so status pages are not starved by flavor chatter.
    pub fn notify(&mut self, line0: &str, line1: &str, line2: &str, line3: &str) {
        if !self.initialized {
            return;
        }
        // Keep newest normal events, but never let them bury IP/hashrate/block pages.
        if self.notifications.len() >= 3 {
            if let Some(pos) = self.notifications.iter().position(|notif| !notif.urgent) {
                self.notifications.remove(pos);
            } else {
                return;
            }
        }
        self.notifications.push(OledNotification {
            line0: line0.to_string(),
            line1: line1.to_string(),
            line2: line2.to_string(),
            line3: line3.to_string(),
            urgent: false,
        });
    }

    /// Push an urgent notification that preempts position 0 in the queue.
    /// Use for safety-critical messages (fan stall, thermal) that must not be dropped.
    pub fn notify_urgent(&mut self, line0: &str, line1: &str, line2: &str, line3: &str) {
        if !self.initialized {
            return;
        }
        let notif = OledNotification {
            line0: line0.to_string(),
            line1: line1.to_string(),
            line2: line2.to_string(),
            line3: line3.to_string(),
            urgent: true,
        };
        self.notifications.insert(0, notif);
        // Trim excess from the back (drop gamification, keep safety)
        while self.notifications.len() > 5 {
            self.notifications.pop();
        }
    }

    /// Whether an urgent notification is waiting at the front of the queue.
    pub fn has_urgent_notification(&self) -> bool {
        self.notifications
            .first()
            .map(|notif| notif.urgent)
            .unwrap_or(false)
    }

    /// Check if there's a pending notification. If so, display it and remove it.
    /// Returns true if a notification was shown (caller should skip normal display).
    pub fn show_notification_if_pending(&mut self, i2c: &mut I2cBus<'_>) -> bool {
        if !self.initialized {
            self.notifications.clear();
            return false;
        }
        if let Some(notif) = self.notifications.first().cloned() {
            self.notifications.remove(0);
            self.show_status(i2c, &notif.line0, &notif.line1, &notif.line2, &notif.line3);
            true
        } else {
            false
        }
    }

    /// Initialize the SSD1306 display. Returns Ok(()) if the display responds.
    /// Fails gracefully if no display is connected — mining continues without it.
    pub fn init(&mut self, i2c: &mut I2cBus<'_>) -> Result<(), I2cError> {
        // Check if display is present
        if !i2c.probe(SSD1306_ADDR) {
            log::warn!("No SSD1306 OLED found at 0x{:02X}", SSD1306_ADDR);
            return Err(I2cError::DeviceNotFound(SSD1306_ADDR));
        }

        // SSD1306 init sequence for 128x32
        let init_cmds: &[u8] = &[
            0xAE, // Display OFF
            0xD5, 0x80, // Set display clock divide ratio (default)
            0xA8, 0x1F, // Set multiplex ratio = 31 (32 rows - 1)
            0xD3, 0x00, // Set display offset = 0
            0x40, // Set display start line = 0
            0x8D, 0x14, // Enable charge pump (0x14 = enabled)
            0x20, 0x00, // Set memory addressing mode = horizontal
            0xA0, // Set segment re-map (column 0 = SEG0) — correct for BitAxe Gamma
            0xC0, // Set COM output scan direction (normal) — correct for BitAxe Gamma
            0xDA, 0x02, // Set COM pins hardware config (sequential, no remap)
            0x81, 0xCF, // Set contrast = 0xCF (high — good daylight readability)
            0xD9, 0xF1, // Set pre-charge period
            0xDB, 0x40, // Set VCOMH deselect level
            0xA4, // Display follows RAM content
            0xA6, // Normal display (not inverted)
            0xAF, // Display ON
        ];

        // Send init commands one at a time
        for &cmd in init_cmds {
            i2c.write(SSD1306_ADDR, &[CMD_SINGLE, cmd])?;
        }

        self.initialized = true;
        self.clear();
        self.flush(i2c)?;

        log::info!("SSD1306 OLED display initialized (128x32)");
        Ok(())
    }

    /// Flip the display 180 degrees (invert orientation).
    /// Useful when the BitAxe is mounted upside-down.
    pub fn set_flip(&mut self, i2c: &mut I2cBus<'_>, flip: bool) -> Result<(), I2cError> {
        if !self.initialized {
            return Ok(());
        }
        self.inverted = flip;
        if flip {
            // Flipped 180 degrees from default
            i2c.write(SSD1306_ADDR, &[CMD_SINGLE, 0xA1])?; // SEG re-map: col 127 = SEG0
            i2c.write(SSD1306_ADDR, &[CMD_SINGLE, 0xC8])?; // COM scan: remapped
        } else {
            // Default orientation (correct for BitAxe Gamma)
            i2c.write(SSD1306_ADDR, &[CMD_SINGLE, 0xA0])?; // SEG re-map: col 0 = SEG0
            i2c.write(SSD1306_ADDR, &[CMD_SINGLE, 0xC0])?; // COM scan: normal
        }
        log::info!(
            "Display orientation: {}",
            if flip { "flipped 180" } else { "normal" }
        );
        Ok(())
    }

    /// Clear the framebuffer (all pixels off).
    pub fn clear(&mut self) {
        self.framebuf.fill(0);
    }

    /// Flush the entire framebuffer to the display over I2C.
    pub fn flush(&self, i2c: &mut I2cBus<'_>) -> Result<(), I2cError> {
        if !self.initialized {
            return Ok(());
        }

        // Set column and page address range for full screen
        i2c.write(SSD1306_ADDR, &[CMD_STREAM, 0x21, 0x00, 0x7F])?; // Column 0-127
        i2c.write(SSD1306_ADDR, &[CMD_STREAM, 0x22, 0x00, (PAGES - 1) as u8])?; // Page 0-3

        // Send framebuffer data page by page (I2C max transaction ~128 bytes safe)
        for page in 0..PAGES {
            let start = page * WIDTH;
            let end = start + WIDTH;
            let mut buf = [0u8; WIDTH + 1];
            buf[0] = DATA_STREAM;
            buf[1..].copy_from_slice(&self.framebuf[start..end]);
            i2c.write(SSD1306_ADDR, &buf)?;
        }

        Ok(())
    }

    /// Draw a character at pixel position (x, y) using the built-in 5x7 font.
    /// y should be 0, 8, 16, or 24 for the 4 text rows.
    fn draw_char(&mut self, x: usize, y: usize, ch: char) {
        let idx = ch as usize;
        let glyph = if idx >= 32 && idx < 128 {
            &FONT_5X7[idx - 32]
        } else {
            &FONT_5X7[0] // space for unknown chars
        };

        let page = y / 8;
        let bit_offset = y % 8;

        for col in 0..5 {
            let px = x + col;
            if px >= WIDTH {
                break;
            }

            let col_data = glyph[col];

            if bit_offset == 0 {
                // Aligned to page boundary — fast path
                if page < PAGES {
                    self.framebuf[page * WIDTH + px] |= col_data;
                }
            } else {
                // Spans two pages
                if page < PAGES {
                    self.framebuf[page * WIDTH + px] |= col_data << bit_offset;
                }
                if page + 1 < PAGES {
                    self.framebuf[(page + 1) * WIDTH + px] |= col_data >> (8 - bit_offset);
                }
            }
        }
    }

    /// Draw a text string at pixel position (x, y).
    /// Characters are 6px wide (5px glyph + 1px spacing).
    pub fn draw_text(&mut self, x: usize, y: usize, text: &str) {
        let mut cx = x;
        for ch in text.chars() {
            if cx + 5 > WIDTH {
                break;
            }
            self.draw_char(cx, y, ch);
            cx += 6; // 5px char + 1px gap
        }
    }

    /// Draw centered text on a given row (y = 0, 8, 16, or 24).
    pub fn draw_centered(&mut self, y: usize, text: &str) {
        let text_width = text.len() * 6;
        let x = if text_width < WIDTH {
            (WIDTH - text_width) / 2
        } else {
            0
        };
        self.draw_text(x, y, text);
    }

    /// Set a single pixel in the framebuffer.
    pub fn set_pixel(&mut self, x: usize, y: usize, on: bool) {
        if x >= WIDTH || y >= HEIGHT {
            return;
        }
        let page = y / 8;
        let bit = y % 8;
        let idx = page * WIDTH + x;
        if on {
            self.framebuf[idx] |= 1 << bit;
        } else {
            self.framebuf[idx] &= !(1 << bit);
        }
    }

    /// Draw a horizontal line from (x, y) for `len` pixels.
    pub fn draw_hline(&mut self, x: usize, y: usize, len: usize) {
        for i in 0..len {
            self.set_pixel(x + i, y, true);
        }
    }

    /// Draw a vertical line from (x, y) for `len` pixels.
    pub fn draw_vline(&mut self, x: usize, y: usize, len: usize) {
        for i in 0..len {
            self.set_pixel(x, y + i, true);
        }
    }

    /// Invert the display (swap black/white) via SSD1306 command.
    /// Zero framebuffer cost — just a display mode toggle.
    pub fn invert_display(&self, i2c: &mut I2cBus<'_>, invert: bool) {
        if !self.initialized {
            return;
        }
        let cmd = if invert { 0xA7 } else { 0xA6 };
        let _ = i2c.write(SSD1306_ADDR, &[CMD_SINGLE, cmd]);
    }

    /// Set display contrast (brightness) via SSD1306 command (0x00-0xFF).
    /// Useful for breathing effects, dramatic reveals, and visual "sound effects".
    pub fn set_contrast(&self, i2c: &mut I2cBus<'_>, brightness: u8) {
        if !self.initialized {
            return;
        }
        // Must be a single I2C transaction — 0x81 expects its parameter
        // byte within the same STOP. Split writes cause silent failure.
        let _ = i2c.write(SSD1306_ADDR, &[CMD_STREAM, 0x81, brightness]);
    }

    /// Draw a 1-bit bitmap at pixel position (x, y).
    /// `data` is column-major: each byte is 8 vertical pixels, LSB = top.
    /// `w` = width in pixels, `h` = height in pixels (must be multiple of 8).
    pub fn draw_bitmap(&mut self, x: usize, y: usize, data: &[u8], w: usize, h: usize) {
        let pages = h / 8;
        for page in 0..pages {
            for col in 0..w {
                let src_idx = page * w + col;
                if src_idx >= data.len() {
                    break;
                }
                let px = x + col;
                let py = y + page * 8;
                if px >= WIDTH || py >= HEIGHT {
                    continue;
                }
                let dst_page = py / 8;
                let bit_offset = py % 8;
                let col_data = data[src_idx];
                if bit_offset == 0 {
                    if dst_page < PAGES {
                        self.framebuf[dst_page * WIDTH + px] |= col_data;
                    }
                } else {
                    if dst_page < PAGES {
                        self.framebuf[dst_page * WIDTH + px] |= col_data << bit_offset;
                    }
                    if dst_page + 1 < PAGES {
                        self.framebuf[(dst_page + 1) * WIDTH + px] |= col_data >> (8 - bit_offset);
                    }
                }
            }
        }
    }

    /// Draw a 1-bit bitmap scaled 2x. Used for full-height companion moments.
    pub fn draw_bitmap_2x(&mut self, x: usize, y: usize, data: &[u8], w: usize, h: usize) {
        for col in 0..w {
            for row in 0..h {
                let src_idx = (row / 8) * w + col;
                if src_idx >= data.len() || (data[src_idx] & (1u8 << (row % 8))) == 0 {
                    continue;
                }
                let px = x + col * 2;
                let py = y + row * 2;
                self.set_pixel(px, py, true);
                self.set_pixel(px + 1, py, true);
                self.set_pixel(px, py + 1, true);
                self.set_pixel(px + 1, py + 1, true);
            }
        }
    }

    /// Draw a sparkline using block characters (▁▂▃▄▅▆▇█).
    /// `values` is a slice of normalized values (0.0 - 1.0).
    /// Drawn at pixel row `y` (top of the sparkline area), spanning 8 pixels tall.
    /// Each value gets `bar_width` pixels wide.
    pub fn draw_sparkline(&mut self, x: usize, y: usize, values: &[f32], bar_width: usize) {
        let max_height: usize = 7; // max 7 pixels tall
        for (i, &val) in values.iter().enumerate() {
            let bar_height = ((val.clamp(0.0, 1.0) * max_height as f32) as usize).max(1);
            let bx = x + i * bar_width;
            // Draw bar from bottom up
            for w in 0..bar_width.min(2) {
                for h in 0..bar_height {
                    self.set_pixel(bx + w, y + max_height - h, true);
                }
            }
        }
    }

    /// Draw a progress bar at (x, y) with given width and fill ratio (0.0-1.0).
    pub fn draw_progress_bar(&mut self, x: usize, y: usize, width: usize, ratio: f32) {
        let filled = ((ratio.clamp(0.0, 1.0) * width as f32) as usize).min(width);
        // Top border
        self.draw_hline(x, y, width);
        // Bottom border
        self.draw_hline(x, y + 4, width);
        // Left/right borders
        self.set_pixel(x, y + 1, true);
        self.set_pixel(x, y + 2, true);
        self.set_pixel(x, y + 3, true);
        self.set_pixel(x + width - 1, y + 1, true);
        self.set_pixel(x + width - 1, y + 2, true);
        self.set_pixel(x + width - 1, y + 3, true);
        // Fill
        for fx in 0..filled {
            self.set_pixel(x + 1 + fx, y + 1, true);
            self.set_pixel(x + 1 + fx, y + 2, true);
            self.set_pixel(x + 1 + fx, y + 3, true);
        }
    }

    /// XOR a rectangular region — toggles all pixels in the area.
    /// Used for inverted text boxes (e.g., Matrix rain stat overlays).
    pub fn xor_rect(&mut self, x: usize, y: usize, w: usize, h: usize) {
        if !self.initialized {
            return;
        }
        for py in y..y.min(32).max(y) + h.min(32 - y.min(31)) {
            for px in x..x.min(128).max(x) + w.min(128 - x.min(127)) {
                if px < 128 && py < 32 {
                    let page = py / 8;
                    let bit = py % 8;
                    self.framebuf[page * 128 + px] ^= 1 << bit;
                }
            }
        }
    }

    /// Show a simple 4-line status screen. Each line is centered.
    /// Pass empty string "" to skip a line.
    pub fn show_status(
        &mut self,
        i2c: &mut I2cBus<'_>,
        line0: &str,
        line1: &str,
        line2: &str,
        line3: &str,
    ) {
        if !self.initialized {
            return;
        }
        self.clear();
        if !line0.is_empty() {
            self.draw_centered(0, line0);
        }
        if !line1.is_empty() {
            self.draw_centered(8, line1);
        }
        if !line2.is_empty() {
            self.draw_centered(16, line2);
        }
        if !line3.is_empty() {
            self.draw_centered(24, line3);
        }
        let _ = self.flush(i2c);
    }
}

// ==========================================================================
// Built-in 5x7 pixel font (ASCII 32-127, 96 characters)
// Each character is 5 bytes — one byte per column, LSB = top pixel.
// ==========================================================================
#[cfg(feature = "display-ssd1306")]
#[rustfmt::skip]
const FONT_5X7: [[u8; 5]; 96] = [
    [0x00,0x00,0x00,0x00,0x00], // 32 ' '
    [0x00,0x00,0x5F,0x00,0x00], // 33 '!'
    [0x00,0x07,0x00,0x07,0x00], // 34 '"'
    [0x14,0x7F,0x14,0x7F,0x14], // 35 '#'
    [0x24,0x2A,0x7F,0x2A,0x12], // 36 '$'
    [0x23,0x13,0x08,0x64,0x62], // 37 '%'
    [0x36,0x49,0x55,0x22,0x50], // 38 '&'
    [0x00,0x05,0x03,0x00,0x00], // 39 '''
    [0x00,0x1C,0x22,0x41,0x00], // 40 '('
    [0x00,0x41,0x22,0x1C,0x00], // 41 ')'
    [0x08,0x2A,0x1C,0x2A,0x08], // 42 '*'
    [0x08,0x08,0x3E,0x08,0x08], // 43 '+'
    [0x00,0x50,0x30,0x00,0x00], // 44 ','
    [0x08,0x08,0x08,0x08,0x08], // 45 '-'
    [0x00,0x60,0x60,0x00,0x00], // 46 '.'
    [0x20,0x10,0x08,0x04,0x02], // 47 '/'
    [0x3E,0x51,0x49,0x45,0x3E], // 48 '0'
    [0x00,0x42,0x7F,0x40,0x00], // 49 '1'
    [0x42,0x61,0x51,0x49,0x46], // 50 '2'
    [0x21,0x41,0x45,0x4B,0x31], // 51 '3'
    [0x18,0x14,0x12,0x7F,0x10], // 52 '4'
    [0x27,0x45,0x45,0x45,0x39], // 53 '5'
    [0x3C,0x4A,0x49,0x49,0x30], // 54 '6'
    [0x01,0x71,0x09,0x05,0x03], // 55 '7'
    [0x36,0x49,0x49,0x49,0x36], // 56 '8'
    [0x06,0x49,0x49,0x29,0x1E], // 57 '9'
    [0x00,0x36,0x36,0x00,0x00], // 58 ':'
    [0x00,0x56,0x36,0x00,0x00], // 59 ';'
    [0x00,0x08,0x14,0x22,0x41], // 60 '<'
    [0x14,0x14,0x14,0x14,0x14], // 61 '='
    [0x41,0x22,0x14,0x08,0x00], // 62 '>'
    [0x02,0x01,0x51,0x09,0x06], // 63 '?'
    [0x32,0x49,0x79,0x41,0x3E], // 64 '@'
    [0x7E,0x11,0x11,0x11,0x7E], // 65 'A'
    [0x7F,0x49,0x49,0x49,0x36], // 66 'B'
    [0x3E,0x41,0x41,0x41,0x22], // 67 'C'
    [0x7F,0x41,0x41,0x22,0x1C], // 68 'D'
    [0x7F,0x49,0x49,0x49,0x41], // 69 'E'
    [0x7F,0x09,0x09,0x01,0x01], // 70 'F'
    [0x3E,0x41,0x41,0x51,0x32], // 71 'G'
    [0x7F,0x08,0x08,0x08,0x7F], // 72 'H'
    [0x00,0x41,0x7F,0x41,0x00], // 73 'I'
    [0x20,0x40,0x41,0x3F,0x01], // 74 'J'
    [0x7F,0x08,0x14,0x22,0x41], // 75 'K'
    [0x7F,0x40,0x40,0x40,0x40], // 76 'L'
    [0x7F,0x02,0x04,0x02,0x7F], // 77 'M'
    [0x7F,0x04,0x08,0x10,0x7F], // 78 'N'
    [0x3E,0x41,0x41,0x41,0x3E], // 79 'O'
    [0x7F,0x09,0x09,0x09,0x06], // 80 'P'
    [0x3E,0x41,0x51,0x21,0x5E], // 81 'Q'
    [0x7F,0x09,0x19,0x29,0x46], // 82 'R'
    [0x46,0x49,0x49,0x49,0x31], // 83 'S'
    [0x01,0x01,0x7F,0x01,0x01], // 84 'T'
    [0x3F,0x40,0x40,0x40,0x3F], // 85 'U'
    [0x1F,0x20,0x40,0x20,0x1F], // 86 'V'
    [0x7F,0x20,0x18,0x20,0x7F], // 87 'W'
    [0x63,0x14,0x08,0x14,0x63], // 88 'X'
    [0x03,0x04,0x78,0x04,0x03], // 89 'Y'
    [0x61,0x51,0x49,0x45,0x43], // 90 'Z'
    [0x00,0x00,0x7F,0x41,0x41], // 91 '['
    [0x02,0x04,0x08,0x10,0x20], // 92 '\'
    [0x41,0x41,0x7F,0x00,0x00], // 93 ']'
    [0x04,0x02,0x01,0x02,0x04], // 94 '^'
    [0x40,0x40,0x40,0x40,0x40], // 95 '_'
    [0x00,0x01,0x02,0x04,0x00], // 96 '`'
    [0x20,0x54,0x54,0x54,0x78], // 97 'a'
    [0x7F,0x48,0x44,0x44,0x38], // 98 'b'
    [0x38,0x44,0x44,0x44,0x20], // 99 'c'
    [0x38,0x44,0x44,0x48,0x7F], // 100 'd'
    [0x38,0x54,0x54,0x54,0x18], // 101 'e'
    [0x08,0x7E,0x09,0x01,0x02], // 102 'f'
    [0x08,0x14,0x54,0x54,0x3C], // 103 'g'
    [0x7F,0x08,0x04,0x04,0x78], // 104 'h'
    [0x00,0x44,0x7D,0x40,0x00], // 105 'i'
    [0x20,0x40,0x44,0x3D,0x00], // 106 'j'
    [0x00,0x7F,0x10,0x28,0x44], // 107 'k'
    [0x00,0x41,0x7F,0x40,0x00], // 108 'l'
    [0x7C,0x04,0x18,0x04,0x78], // 109 'm'
    [0x7C,0x08,0x04,0x04,0x78], // 110 'n'
    [0x38,0x44,0x44,0x44,0x38], // 111 'o'
    [0x7C,0x14,0x14,0x14,0x08], // 112 'p'
    [0x08,0x14,0x14,0x18,0x7C], // 113 'q'
    [0x7C,0x08,0x04,0x04,0x08], // 114 'r'
    [0x48,0x54,0x54,0x54,0x20], // 115 's'
    [0x04,0x3F,0x44,0x40,0x20], // 116 't'
    [0x3C,0x40,0x40,0x20,0x7C], // 117 'u'
    [0x1C,0x20,0x40,0x20,0x1C], // 118 'v'
    [0x3C,0x40,0x30,0x40,0x3C], // 119 'w'
    [0x44,0x28,0x10,0x28,0x44], // 120 'x'
    [0x0C,0x50,0x50,0x50,0x3C], // 121 'y'
    [0x44,0x64,0x54,0x4C,0x44], // 122 'z'
    [0x00,0x08,0x36,0x41,0x00], // 123 '{'
    [0x00,0x00,0x7F,0x00,0x00], // 124 '|'
    [0x00,0x41,0x36,0x08,0x00], // 125 '}'
    [0x08,0x08,0x2A,0x1C,0x08], // 126 '~'
    [0x08,0x1C,0x2A,0x08,0x08], // 127 DEL
];
