//! On-screen keyboard: combo timing, navigation/typing state machine, and a
//! real uinput injection round-trip (read back through a second evdev handle).

use std::time::{Duration, Instant};

use evdev::{Device, EventSummary, KeyCode};

use hyprcoop::input::PadButton;
use hyprcoop::osk::{Activation, ComboTracker, Osk, VirtualKeyboard, HOLD};

#[test]
fn combo_fires_only_after_hold() {
    let t0 = Instant::now();
    let mut combo = ComboTracker::default();

    combo.set(PadButton::Start, true, t0);
    assert!(!combo.fired(t0), "one button down should not fire");

    combo.set(PadButton::Select, true, t0);
    assert!(!combo.fired(t0), "not held long enough");
    assert!(!combo.fired(t0 + HOLD - Duration::from_millis(1)), "just short");

    assert!(combo.fired(t0 + HOLD), "held long enough fires");
    assert!(!combo.fired(t0 + HOLD * 2), "fires only once per hold");

    // Releasing and re-holding arms it again.
    combo.set(PadButton::Select, false, t0 + HOLD * 2);
    combo.set(PadButton::Select, true, t0 + HOLD * 2);
    assert!(combo.fired(t0 + HOLD * 3));
}

#[test]
fn releasing_a_button_cancels_the_hold() {
    let t0 = Instant::now();
    let mut combo = ComboTracker::default();
    combo.set(PadButton::Start, true, t0);
    combo.set(PadButton::Select, true, t0);
    combo.set(PadButton::Start, false, t0 + Duration::from_millis(500));
    assert!(!combo.fired(t0 + HOLD * 2), "released before hold completed");
}

#[test]
fn typing_navigates_shifts_and_erases() {
    let mut osk = Osk::new(1, Some("0xabc".into()));
    assert_eq!(osk.cursor(), (0, 0));

    // Type "12" from the digit row.
    matches!(osk.on_button(PadButton::Accept), Activation::Continue);
    osk.on_button(PadButton::Right);
    osk.on_button(PadButton::Accept);
    assert_eq!(osk.display(), "12");

    // Erase the last digit.
    osk.on_button(PadButton::Erase);
    assert_eq!(osk.display(), "1");

    // Shift then type a letter -> uppercase display. Cursor is at digit col 1,
    // so step Left to col 0 before dropping into the qwerty row ('q').
    osk.on_button(PadButton::Left);
    osk.on_button(PadButton::Down);
    assert_eq!(osk.cursor(), (1, 0));
    osk.on_button(PadButton::Shift);
    assert!(osk.shifted());
    osk.on_button(PadButton::Accept); // 'q' -> 'Q'
    assert_eq!(osk.display(), "1Q");
    assert!(osk.strokes().last().unwrap().shift, "uppercase uses shift");

    // Cancel with Back.
    assert!(matches!(osk.on_button(PadButton::Back), Activation::Cancel));
}

#[test]
fn done_commits_and_wraps_navigation() {
    let mut osk = Osk::new(0, None);
    // Up from the top row wraps to the last row, which holds Done.
    osk.on_button(PadButton::Up);
    let last_row = osk.grid().len() - 1;
    assert_eq!(osk.cursor().0, last_row);
    // Walk to the Done cell and activate it.
    let done_col = osk.grid()[last_row]
        .iter()
        .position(|(label, _)| label == "Done")
        .expect("Done key exists");
    while osk.cursor().1 != done_col {
        osk.on_button(PadButton::Right);
    }
    assert!(matches!(osk.on_button(PadButton::Accept), Activation::Commit));
}

/// Create a virtual keyboard, type a string, and read the injected key events
/// back from the resulting evdev node.
#[test]
fn virtual_keyboard_injects_readable_key_events() {
    let mut kb = VirtualKeyboard::new().expect("create virtual keyboard");
    std::thread::sleep(Duration::from_millis(400));

    // Find the keyboard node we just created.
    let node = evdev::enumerate()
        .find(|(_, d)| d.name() == Some("hyprcoop virtual keyboard"))
        .map(|(p, _)| p)
        .expect("virtual keyboard node should exist");
    let mut reader = Device::open(&node).expect("open keyboard node");

    // Build strokes for "aB": 'a' (no shift), 'B' (shift + b).
    let mut osk = Osk::new(0, None);
    // Row 1 col 0 is 'a'? No — 'a' is row 2. Navigate: Down, Down -> asdf row.
    osk.on_button(PadButton::Down);
    osk.on_button(PadButton::Down);
    osk.on_button(PadButton::Accept); // 'a'
    // Shift and type 'b' (row 3 col 4 is 'b').
    osk.on_button(PadButton::Shift);
    osk.on_button(PadButton::Down); // row 3 (zxcv...)
    // 'b' is at index 4 in "zxcvbnm".
    for _ in 0..4 {
        osk.on_button(PadButton::Right);
    }
    osk.on_button(PadButton::Accept); // 'B'
    assert_eq!(osk.display(), "aB");

    let strokes = osk.strokes().to_vec();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(100));
        kb.type_strokes(&strokes).expect("inject");
        // keep kb alive until injection completes
        std::thread::sleep(Duration::from_millis(100));
    });

    let mut saw_a = false;
    let mut saw_shift = false;
    let mut saw_b = false;
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline && !(saw_a && saw_shift && saw_b) {
        let Ok(events) = reader.fetch_events() else {
            break;
        };
        for ev in events {
            if let EventSummary::Key(_, key, 1) = ev.destructure() {
                match key {
                    KeyCode::KEY_A => saw_a = true,
                    KeyCode::KEY_LEFTSHIFT => saw_shift = true,
                    KeyCode::KEY_B => saw_b = true,
                    _ => {}
                }
            }
        }
    }
    assert!(saw_a, "KEY_A should be injected");
    assert!(saw_shift, "shift should be held for uppercase");
    assert!(saw_b, "KEY_B should be injected");
}
