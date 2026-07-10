//! Built-in LED blink pattern library for "Find My Miner" and celebrations.
//!
//! Each pattern encodes a recognizable rhythm using green (D5) and red (D6) LEDs.
//! Green = higher pitch / treble feel. Red = lower pitch / bass feel. Both = accent.

use crate::led::{BlinkSequence, LedFrame};

// Helper const functions for terse frame definition in static arrays.
const fn g(ms: u16) -> LedFrame {
    LedFrame {
        green: true,
        red: false,
        duration_ms: ms,
    }
}
const fn r(ms: u16) -> LedFrame {
    LedFrame {
        green: false,
        red: true,
        duration_ms: ms,
    }
}
const fn b(ms: u16) -> LedFrame {
    LedFrame {
        green: true,
        red: true,
        duration_ms: ms,
    }
}
const fn o(ms: u16) -> LedFrame {
    LedFrame {
        green: false,
        red: false,
        duration_ms: ms,
    }
}

// ---------------------------------------------------------------------------
// 1. Imperial March (Star Wars)
// DUM DUM DUM, DUM-da DUM, DUM-da DUM
// ---------------------------------------------------------------------------
pub static IMPERIAL_MARCH_FRAMES: [LedFrame; 20] = [
    // DUM DUM DUM
    r(400),
    o(100),
    r(400),
    o(100),
    r(400),
    o(200),
    // DUM-da DUM
    r(250),
    g(100),
    r(400),
    o(200),
    // DUM-da DUM
    r(250),
    g(100),
    r(400),
    o(400),
    // da-da-da DUM-da DUM
    g(100),
    g(100),
    g(100),
    r(250),
    g(100),
    r(400),
];

pub static IMPERIAL_MARCH: BlinkSequence = BlinkSequence {
    name: "Imperial March",
    id: "imperial_march",
    description: "Star Wars villain theme — heavy red bass notes",
    frames: &IMPERIAL_MARCH_FRAMES,
};

// ---------------------------------------------------------------------------
// 2. Zelda Secret Found
// Ascending staccato notes getting shorter, ending with a long both-on hold
// ---------------------------------------------------------------------------
pub static ZELDA_SECRET_FRAMES: [LedFrame; 14] = [
    g(200),
    o(80),
    g(160),
    o(60),
    g(120),
    o(50),
    g(100),
    o(40),
    g(80),
    o(30),
    g(60),
    o(20),
    b(600),
    o(200),
];

pub static ZELDA_SECRET: BlinkSequence = BlinkSequence {
    name: "Zelda Secret Found",
    id: "zelda_secret",
    description: "That satisfying discovery jingle — ascending green staccato",
    frames: &ZELDA_SECRET_FRAMES,
};

// ---------------------------------------------------------------------------
// 3. Mario Coin
// Quick double-tap "bling bling"
// ---------------------------------------------------------------------------
pub static MARIO_COIN_FRAMES: [LedFrame; 6] = [g(80), o(40), g(200), o(300), b(60), o(200)];

pub static MARIO_COIN: BlinkSequence = BlinkSequence {
    name: "Mario Coin",
    id: "mario_coin",
    description: "Super Mario Bros coin collect — quick green double-tap",
    frames: &MARIO_COIN_FRAMES,
};

// ---------------------------------------------------------------------------
// 4. Morse SOS
// ... --- ... (3 short, 3 long, 3 short)
// Green = dots (short), Red = dashes (long)
// ---------------------------------------------------------------------------
pub static MORSE_SOS_FRAMES: [LedFrame; 18] = [
    // S: ...
    g(100),
    o(100),
    g(100),
    o(100),
    g(100),
    o(300),
    // O: ---
    r(300),
    o(100),
    r(300),
    o(100),
    r(300),
    o(300),
    // S: ...
    g(100),
    o(100),
    g(100),
    o(100),
    g(100),
    o(600),
];

pub static MORSE_SOS: BlinkSequence = BlinkSequence {
    name: "Morse SOS",
    id: "morse_sos",
    description: "Classic distress signal — green dots, red dashes",
    frames: &MORSE_SOS_FRAMES,
};

// ---------------------------------------------------------------------------
// 5. Heartbeat (Medical monitor)
// Quick double-pulse: lub-DUB ... lub-DUB
// ---------------------------------------------------------------------------
pub static HEARTBEAT_FRAMES: [LedFrame; 8] = [
    b(80),
    o(80), // lub
    b(120),
    o(600), // DUB (louder/longer)
    b(80),
    o(80), // lub
    b(120),
    o(600), // DUB
];

pub static HEARTBEAT: BlinkSequence = BlinkSequence {
    name: "Heartbeat",
    id: "heartbeat",
    description: "Cardiac rhythm — double-pulse like a real heartbeat",
    frames: &HEARTBEAT_FRAMES,
};

// ---------------------------------------------------------------------------
// 6. Cylon Scanner (Battlestar Galactica)
// Sweep: green to red and back, with off gaps
// ---------------------------------------------------------------------------
pub static CYLON_FRAMES: [LedFrame; 12] = [
    g(200),
    o(50),
    b(100),
    o(50),
    r(200),
    o(50),
    r(200),
    o(50),
    b(100),
    o(50),
    g(200),
    o(50),
];

pub static CYLON: BlinkSequence = BlinkSequence {
    name: "Cylon Scanner",
    id: "cylon",
    description: "Battlestar Galactica eye sweep — green to red and back",
    frames: &CYLON_FRAMES,
};

// ---------------------------------------------------------------------------
// 7. Matrix Rain
// Rapid green flicker at varying speeds, like digital rain
// ---------------------------------------------------------------------------
pub static MATRIX_FRAMES: [LedFrame; 20] = [
    g(50),
    o(30),
    g(120),
    o(20),
    g(30),
    o(80),
    g(60),
    o(40),
    g(150),
    o(10),
    g(40),
    o(60),
    g(80),
    o(30),
    g(20),
    o(100),
    g(100),
    o(50),
    g(30),
    o(70),
];

pub static MATRIX: BlinkSequence = BlinkSequence {
    name: "Matrix Rain",
    id: "matrix",
    description: "Digital rain — rapid green flicker at varying speeds",
    frames: &MATRIX_FRAMES,
};

// ---------------------------------------------------------------------------
// 8. Bitcoin Mined! (Celebration)
// Ascending tempo crescendo ending with a long both-on hold
// ---------------------------------------------------------------------------
pub static BITCOIN_MINED_FRAMES: [LedFrame; 18] = [
    // Slow start
    g(300),
    o(200),
    r(300),
    o(200),
    // Speed up
    g(200),
    o(100),
    r(200),
    o(100),
    // Faster
    g(100),
    o(50),
    r(100),
    o(50),
    // Frantic
    b(60),
    o(30),
    b(60),
    o(30),
    // Grand finale
    b(800),
    o(200),
];

pub static BITCOIN_MINED: BlinkSequence = BlinkSequence {
    name: "Bitcoin Mined!",
    id: "bitcoin_mined",
    description: "Celebration crescendo — ascending tempo into grand finale flash",
    frames: &BITCOIN_MINED_FRAMES,
};

// ---------------------------------------------------------------------------
// 9. Police Siren
// Fast alternating red-green like emergency lights
// ---------------------------------------------------------------------------
pub static POLICE_FRAMES: [LedFrame; 16] = [
    r(80),
    o(20),
    r(80),
    o(20),
    r(80),
    o(100),
    g(80),
    o(20),
    g(80),
    o(20),
    g(80),
    o(100),
    r(80),
    o(20),
    g(80),
    o(20),
];

pub static POLICE: BlinkSequence = BlinkSequence {
    name: "Police Siren",
    id: "police",
    description: "Emergency lights — fast alternating red and green bursts",
    frames: &POLICE_FRAMES,
};

// ---------------------------------------------------------------------------
// 10. Close Encounters (of the Third Kind)
// The famous 5-note sequence: Re-Mi-Do-Do(low)-Sol
// ---------------------------------------------------------------------------
pub static CLOSE_ENCOUNTERS_FRAMES: [LedFrame; 10] = [
    g(400),
    o(100), // Re (high)
    g(400),
    o(100), // Mi (higher)
    r(400),
    o(100), // Do (low)
    r(400),
    o(100), // Do (low octave)
    b(800),
    o(300), // Sol (final, both LEDs)
];

pub static CLOSE_ENCOUNTERS: BlinkSequence = BlinkSequence {
    name: "Close Encounters",
    id: "close_encounters",
    description: "5-note alien greeting — green high notes, red low notes",
    frames: &CLOSE_ENCOUNTERS_FRAMES,
};

// ---------------------------------------------------------------------------
// Pattern registry
// ---------------------------------------------------------------------------

/// All available blink patterns.
pub static BLINK_PATTERNS: &[&BlinkSequence] = &[
    &IMPERIAL_MARCH,
    &ZELDA_SECRET,
    &MARIO_COIN,
    &MORSE_SOS,
    &HEARTBEAT,
    &CYLON,
    &MATRIX,
    &BITCOIN_MINED,
    &POLICE,
    &CLOSE_ENCOUNTERS,
];

/// Look up a blink pattern by its string ID.
pub fn find_pattern(id: &str) -> Option<&'static BlinkSequence> {
    BLINK_PATTERNS.iter().find(|p| p.id == id).copied()
}
