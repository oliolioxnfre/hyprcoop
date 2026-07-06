//! Controller-driven on-screen keyboard.
//!
//! Triggered by holding Start+Select (Options+Share / Menu+View / +&−) for
//! [`HOLD`], it lets a player type with the d-pad and face buttons. Text is
//! buffered while the player edits (the hyprcoop terminal is focused so
//! keystrokes can't reach the game yet), then replayed into the player's game
//! window via a uinput virtual keyboard once they pick **Done**.

use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use evdev::uinput::VirtualDevice;
use evdev::{AttributeSet, EventType, InputEvent, KeyCode};

use crate::input::PadButton;

/// How long Start+Select must be held to summon the keyboard.
pub const HOLD: Duration = Duration::from_secs(2);

/// Tracks the Start+Select hold on a single pad. Fed button edges; reports
/// once when the combo has been held long enough.
#[derive(Debug, Default)]
pub struct ComboTracker {
    start_down: bool,
    select_down: bool,
    since: Option<Instant>,
    fired: bool,
}

impl ComboTracker {
    pub fn set(&mut self, button: PadButton, pressed: bool, now: Instant) {
        match button {
            PadButton::Start => self.start_down = pressed,
            PadButton::Select => self.select_down = pressed,
            _ => return,
        }
        if self.start_down && self.select_down {
            self.since.get_or_insert(now);
        } else {
            self.since = None;
            self.fired = false;
        }
    }

    /// Returns true exactly once per hold, after [`HOLD`] has elapsed.
    pub fn fired(&mut self, now: Instant) -> bool {
        if self.fired {
            return false;
        }
        match self.since {
            Some(since) if now.duration_since(since) >= HOLD => {
                self.fired = true;
                true
            }
            _ => false,
        }
    }
}

/// A single keystroke to replay: a key code and whether shift is held.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stroke {
    pub code: KeyCode,
    pub shift: bool,
}

/// A cell in the keyboard grid.
#[derive(Debug, Clone)]
enum Cell {
    /// A character with distinct un-shifted and shifted forms.
    Char {
        lower: (char, KeyCode, bool),
        upper: (char, KeyCode, bool),
    },
    Space,
    Backspace,
    Enter,
    ShiftKey,
    Done,
    Cancel,
}

impl Cell {
    fn ch(lower: char, lo: KeyCode, upper: char, up: KeyCode, up_shift: bool) -> Cell {
        Cell::Char {
            lower: (lower, lo, false),
            upper: (upper, up, up_shift),
        }
    }

    /// Same physical key, shifted form is that key + shift (letters, `1`→`!`).
    fn simple(lower: char, code: KeyCode, upper: char) -> Cell {
        Cell::Char {
            lower: (lower, code, false),
            upper: (upper, code, true),
        }
    }

    fn label(&self, shifted: bool) -> String {
        match self {
            Cell::Char { lower, upper } => {
                let (c, _, _) = if shifted { upper } else { lower };
                c.to_string()
            }
            Cell::Space => "␣".into(),
            Cell::Backspace => "⌫".into(),
            Cell::Enter => "⏎".into(),
            Cell::ShiftKey => "⇧".into(),
            Cell::Done => "Done".into(),
            Cell::Cancel => "✕".into(),
        }
    }
}

/// Result of activating the currently-selected cell.
pub enum Activation {
    /// Keep the keyboard open.
    Continue,
    /// Commit the buffer: focus the target window and inject.
    Commit,
    /// Close without injecting.
    Cancel,
}

pub struct Osk {
    /// Which player (index) and game-window address to type into.
    pub player: usize,
    pub target_window: Option<String>,
    rows: Vec<Vec<Cell>>,
    cursor: (usize, usize),
    shifted: bool,
    display: String,
    strokes: Vec<Stroke>,
}

impl Osk {
    pub fn new(player: usize, target_window: Option<String>) -> Self {
        Osk {
            player,
            target_window,
            rows: default_layout(),
            cursor: (0, 0),
            shifted: false,
            display: String::new(),
            strokes: Vec::new(),
        }
    }

    pub fn shifted(&self) -> bool {
        self.shifted
    }

    pub fn display(&self) -> &str {
        &self.display
    }

    pub fn strokes(&self) -> &[Stroke] {
        &self.strokes
    }

    pub fn cursor(&self) -> (usize, usize) {
        self.cursor
    }

    /// Rendered grid: rows of (label, selected) for the current shift state.
    pub fn grid(&self) -> Vec<Vec<(String, bool)>> {
        self.rows
            .iter()
            .enumerate()
            .map(|(r, row)| {
                row.iter()
                    .enumerate()
                    .map(|(c, cell)| (cell.label(self.shifted), self.cursor == (r, c)))
                    .collect()
            })
            .collect()
    }

    pub fn move_cursor(&mut self, button: PadButton) {
        let (mut r, mut c) = self.cursor;
        match button {
            PadButton::Up => r = if r == 0 { self.rows.len() - 1 } else { r - 1 },
            PadButton::Down => r = (r + 1) % self.rows.len(),
            PadButton::Left => c = if c == 0 { self.rows[r].len() - 1 } else { c - 1 },
            PadButton::Right => c = (c + 1) % self.rows[r].len(),
            _ => {}
        }
        c = c.min(self.rows[r].len().saturating_sub(1));
        self.cursor = (r, c);
    }

    /// Handle a controller button. Directional buttons move the cursor;
    /// Accept activates the current cell; Erase/Shift/Back are shortcuts.
    pub fn on_button(&mut self, button: PadButton) -> Activation {
        match button {
            PadButton::Up | PadButton::Down | PadButton::Left | PadButton::Right => {
                self.move_cursor(button);
                Activation::Continue
            }
            PadButton::Accept => self.activate_current(),
            PadButton::Erase => {
                self.backspace();
                Activation::Continue
            }
            PadButton::Shift => {
                self.shifted = !self.shifted;
                Activation::Continue
            }
            PadButton::Back => Activation::Cancel,
            _ => Activation::Continue,
        }
    }

    fn activate_current(&mut self) -> Activation {
        let (r, c) = self.cursor;
        match self.rows[r][c].clone() {
            Cell::Char { lower, upper } => {
                let (ch, code, shift) = if self.shifted { upper } else { lower };
                self.display.push(ch);
                self.strokes.push(Stroke { code, shift });
                Activation::Continue
            }
            Cell::Space => {
                self.display.push(' ');
                self.strokes.push(Stroke {
                    code: KeyCode::KEY_SPACE,
                    shift: false,
                });
                Activation::Continue
            }
            Cell::Enter => {
                self.display.push('\n');
                self.strokes.push(Stroke {
                    code: KeyCode::KEY_ENTER,
                    shift: false,
                });
                Activation::Continue
            }
            Cell::Backspace => {
                self.backspace();
                Activation::Continue
            }
            Cell::ShiftKey => {
                self.shifted = !self.shifted;
                Activation::Continue
            }
            Cell::Done => Activation::Commit,
            Cell::Cancel => Activation::Cancel,
        }
    }

    fn backspace(&mut self) {
        self.display.pop();
        self.strokes.pop();
    }
}

fn default_layout() -> Vec<Vec<Cell>> {
    use KeyCode as K;
    let digits = "1234567890";
    let sym = "!@#$%^&*()";
    let mut row0: Vec<Cell> = digits
        .chars()
        .zip(sym.chars())
        .zip([
            K::KEY_1,
            K::KEY_2,
            K::KEY_3,
            K::KEY_4,
            K::KEY_5,
            K::KEY_6,
            K::KEY_7,
            K::KEY_8,
            K::KEY_9,
            K::KEY_0,
        ])
        .map(|((d, s), code)| Cell::Char {
            lower: (d, code, false),
            upper: (s, code, true),
        })
        .collect();
    row0.push(Cell::Backspace);

    let letters = |chars: &str, codes: &[KeyCode]| -> Vec<Cell> {
        chars
            .chars()
            .zip(codes.iter())
            .map(|(ch, &code)| Cell::simple(ch, code, ch.to_ascii_uppercase()))
            .collect()
    };

    let mut row1 = letters(
        "qwertyuiop",
        &[
            K::KEY_Q,
            K::KEY_W,
            K::KEY_E,
            K::KEY_R,
            K::KEY_T,
            K::KEY_Y,
            K::KEY_U,
            K::KEY_I,
            K::KEY_O,
            K::KEY_P,
        ],
    );
    row1.push(Cell::ShiftKey);

    let mut row2 = letters(
        "asdfghjkl",
        &[
            K::KEY_A,
            K::KEY_S,
            K::KEY_D,
            K::KEY_F,
            K::KEY_G,
            K::KEY_H,
            K::KEY_J,
            K::KEY_K,
            K::KEY_L,
        ],
    );
    row2.push(Cell::Enter);

    let mut row3 = letters(
        "zxcvbnm",
        &[
            K::KEY_Z,
            K::KEY_X,
            K::KEY_C,
            K::KEY_V,
            K::KEY_B,
            K::KEY_N,
            K::KEY_M,
        ],
    );
    // Punctuation common to server names, IPs and passwords.
    row3.push(Cell::ch('.', K::KEY_DOT, ':', K::KEY_SEMICOLON, true));
    row3.push(Cell::ch('-', K::KEY_MINUS, '_', K::KEY_MINUS, true));
    row3.push(Cell::ch('/', K::KEY_SLASH, '@', K::KEY_2, true));

    let row4 = vec![Cell::Space, Cell::Cancel, Cell::Done];

    vec![row0, row1, row2, row3, row4]
}

/// A uinput virtual keyboard used to replay a buffered string into whatever
/// window currently holds compositor focus.
pub struct VirtualKeyboard {
    device: VirtualDevice,
}

impl VirtualKeyboard {
    pub fn new() -> Result<Self> {
        let mut keys = AttributeSet::<KeyCode>::new();
        for code in all_keycodes() {
            keys.insert(code);
        }
        keys.insert(KeyCode::KEY_LEFTSHIFT);
        let device = VirtualDevice::builder()
            .context("opening /dev/uinput (is it writable? see `hyprcoop doctor`)")?
            .name("hyprcoop virtual keyboard")
            .with_keys(&keys)
            .context("declaring keyboard keys")?
            .build()
            .context("creating virtual keyboard")?;
        Ok(Self { device })
    }

    /// Type a sequence of strokes, pressing/releasing shift as needed.
    pub fn type_strokes(&mut self, strokes: &[Stroke]) -> Result<()> {
        for stroke in strokes {
            self.tap(stroke)?;
            // Small gap so games sample each keypress independently.
            std::thread::sleep(Duration::from_millis(8));
        }
        Ok(())
    }

    fn tap(&mut self, stroke: &Stroke) -> Result<()> {
        let key = EventType::KEY.0;
        let shift = KeyCode::KEY_LEFTSHIFT.code();
        if stroke.shift {
            self.device
                .emit(&[InputEvent::new(key, shift, 1)])
                .context("emit shift press")?;
        }
        self.device
            .emit(&[
                InputEvent::new(key, stroke.code.code(), 1),
                InputEvent::new(key, stroke.code.code(), 0),
            ])
            .context("emit key tap")?;
        if stroke.shift {
            self.device
                .emit(&[InputEvent::new(key, shift, 0)])
                .context("emit shift release")?;
        }
        Ok(())
    }
}

fn all_keycodes() -> Vec<KeyCode> {
    use KeyCode as K;
    let mut codes = vec![
        K::KEY_SPACE,
        K::KEY_BACKSPACE,
        K::KEY_ENTER,
        K::KEY_DOT,
        K::KEY_COMMA,
        K::KEY_MINUS,
        K::KEY_EQUAL,
        K::KEY_SLASH,
        K::KEY_SEMICOLON,
        K::KEY_APOSTROPHE,
    ];
    for n in 0..=9u8 {
        codes.push(digit_code(n));
    }
    for c in b'a'..=b'z' {
        codes.push(letter_code(c as char));
    }
    codes
}

fn digit_code(n: u8) -> KeyCode {
    use KeyCode as K;
    match n {
        1 => K::KEY_1,
        2 => K::KEY_2,
        3 => K::KEY_3,
        4 => K::KEY_4,
        5 => K::KEY_5,
        6 => K::KEY_6,
        7 => K::KEY_7,
        8 => K::KEY_8,
        9 => K::KEY_9,
        _ => K::KEY_0,
    }
}

fn letter_code(c: char) -> KeyCode {
    use KeyCode as K;
    match c.to_ascii_lowercase() {
        'a' => K::KEY_A,
        'b' => K::KEY_B,
        'c' => K::KEY_C,
        'd' => K::KEY_D,
        'e' => K::KEY_E,
        'f' => K::KEY_F,
        'g' => K::KEY_G,
        'h' => K::KEY_H,
        'i' => K::KEY_I,
        'j' => K::KEY_J,
        'k' => K::KEY_K,
        'l' => K::KEY_L,
        'm' => K::KEY_M,
        'n' => K::KEY_N,
        'o' => K::KEY_O,
        'p' => K::KEY_P,
        'q' => K::KEY_Q,
        'r' => K::KEY_R,
        's' => K::KEY_S,
        't' => K::KEY_T,
        'u' => K::KEY_U,
        'v' => K::KEY_V,
        'w' => K::KEY_W,
        'x' => K::KEY_X,
        'y' => K::KEY_Y,
        _ => K::KEY_Z,
    }
}
