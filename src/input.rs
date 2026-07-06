use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::time::Duration;

use evdev::{Device, KeyCode};

/// A gamepad-capable evdev device.
#[derive(Debug, Clone, PartialEq)]
pub struct PadInfo {
    pub event_path: PathBuf,
    pub js_path: Option<PathBuf>,
    pub name: String,
}

/// Semantic controller inputs, unified across DualShock 4, Xbox and Switch
/// Pro pads (which all report these via the same evdev codes).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PadButton {
    /// Options (DS4) / Menu (Xbox) / + (Switch) — `BTN_START`.
    Start,
    /// Share (DS4) / View (Xbox) / − (Switch) — `BTN_SELECT`.
    Select,
    /// South face button (✕ / A / B) — accept.
    Accept,
    /// East face button (○ / B / A) — back / cancel.
    Back,
    /// North face button (△ / Y / X) — shift.
    Shift,
    /// West face button (□ / X / Y) — erase.
    Erase,
    Up,
    Down,
    Left,
    Right,
    /// Any other gamepad button (still counts as activity for claiming).
    Other,
}

#[derive(Debug)]
pub enum PadEvent {
    Added(PadInfo),
    Removed(PathBuf),
    /// A button on the pad changed state (edge-triggered).
    Button {
        path: PathBuf,
        button: PadButton,
        pressed: bool,
    },
}

/// Spawns a manager thread that scans for gamepads (rescanning for hotplug)
/// plus one reader thread per pad that reports button presses.
pub fn spawn_input_manager() -> Receiver<PadEvent> {
    let (tx, rx) = channel();
    std::thread::spawn(move || manager_loop(tx));
    rx
}

fn manager_loop(tx: Sender<PadEvent>) {
    let mut known: HashSet<PathBuf> = HashSet::new();
    loop {
        let current = scan_pads();
        let current_paths: HashSet<PathBuf> =
            current.iter().map(|p| p.event_path.clone()).collect();

        for pad in &current {
            if known.insert(pad.event_path.clone()) {
                if tx.send(PadEvent::Added(pad.clone())).is_err() {
                    return;
                }
                spawn_reader(pad.event_path.clone(), tx.clone());
            }
        }
        known.retain(|path| {
            let still_here = current_paths.contains(path);
            if !still_here {
                let _ = tx.send(PadEvent::Removed(path.clone()));
            }
            still_here
        });

        std::thread::sleep(Duration::from_millis(1000));
    }
}

/// Enumerate evdev devices that look like gamepads (report BTN_SOUTH).
pub fn scan_pads() -> Vec<PadInfo> {
    let mut pads = Vec::new();
    for (path, device) in evdev::enumerate() {
        if !is_gamepad(&device) {
            continue;
        }
        let name = device.name().unwrap_or("unknown gamepad").to_string();
        let js_path = find_js_sibling(&path);
        pads.push(PadInfo {
            event_path: path,
            js_path,
            name,
        });
    }
    pads.sort_by(|a, b| a.event_path.cmp(&b.event_path));
    pads
}

fn is_gamepad(device: &Device) -> bool {
    device
        .supported_keys()
        .map(|keys| keys.contains(KeyCode::BTN_SOUTH))
        .unwrap_or(false)
}

/// Find the legacy joystick node (/dev/input/jsN) belonging to the same
/// underlying input device as the given event node, via sysfs siblings.
fn find_js_sibling(event_path: &Path) -> Option<PathBuf> {
    let event_name = event_path.file_name()?.to_str()?;
    let sys_dir = PathBuf::from("/sys/class/input").join(event_name).join("device");
    for entry in std::fs::read_dir(sys_dir).ok()?.flatten() {
        let name = entry.file_name();
        let name = name.to_str()?;
        if name.starts_with("js") {
            let dev = PathBuf::from("/dev/input").join(name);
            if dev.exists() {
                return Some(dev);
            }
        }
    }
    None
}

fn spawn_reader(path: PathBuf, tx: Sender<PadEvent>) {
    std::thread::spawn(move || {
        let Ok(mut device) = Device::open(&path) else {
            return;
        };
        // Last non-zero direction reported by each d-pad hat axis, so we can
        // synthesize press/release edges for the directional buttons.
        let mut hat_x = 0i32;
        let mut hat_y = 0i32;
        loop {
            let events = match device.fetch_events() {
                Ok(events) => events.collect::<Vec<_>>(),
                Err(_) => return, // unplugged; manager reports Removed
            };
            for event in events {
                let sends = match event.destructure() {
                    evdev::EventSummary::Key(_, key, value) if is_gamepad_button(key) => {
                        // value 1 = press, 0 = release (2 = autorepeat, ignored).
                        match value {
                            1 => vec![(map_button(key), true)],
                            0 => vec![(map_button(key), false)],
                            _ => vec![],
                        }
                    }
                    evdev::EventSummary::AbsoluteAxis(_, axis, value) => {
                        hat_edges(axis, value, &mut hat_x, &mut hat_y)
                    }
                    _ => vec![],
                };
                for (button, pressed) in sends {
                    let event = PadEvent::Button {
                        path: path.clone(),
                        button,
                        pressed,
                    };
                    if tx.send(event).is_err() {
                        return;
                    }
                }
            }
        }
    });
}

fn is_gamepad_button(key: KeyCode) -> bool {
    // BTN_SOUTH..BTN_THUMBR covers the standard gamepad button range.
    (KeyCode::BTN_SOUTH.code()..=KeyCode::BTN_THUMBR.code()).contains(&key.code())
}

fn map_button(key: KeyCode) -> PadButton {
    match key {
        KeyCode::BTN_START => PadButton::Start,
        KeyCode::BTN_SELECT => PadButton::Select,
        KeyCode::BTN_SOUTH => PadButton::Accept,
        KeyCode::BTN_EAST => PadButton::Back,
        KeyCode::BTN_NORTH => PadButton::Shift,
        KeyCode::BTN_WEST => PadButton::Erase,
        // D-pad reported as buttons on some drivers (e.g. Switch Pro).
        KeyCode::BTN_DPAD_UP => PadButton::Up,
        KeyCode::BTN_DPAD_DOWN => PadButton::Down,
        KeyCode::BTN_DPAD_LEFT => PadButton::Left,
        KeyCode::BTN_DPAD_RIGHT => PadButton::Right,
        _ => PadButton::Other,
    }
}

/// Translate a hat-axis absolute value into press/release edges for the
/// directional buttons, updating the remembered per-axis state.
fn hat_edges(
    axis: evdev::AbsoluteAxisCode,
    value: i32,
    hat_x: &mut i32,
    hat_y: &mut i32,
) -> Vec<(PadButton, bool)> {
    let (state, neg, pos) = match axis {
        evdev::AbsoluteAxisCode::ABS_HAT0X => (hat_x, PadButton::Left, PadButton::Right),
        evdev::AbsoluteAxisCode::ABS_HAT0Y => (hat_y, PadButton::Up, PadButton::Down),
        _ => return vec![],
    };
    let value = value.signum();
    if value == *state {
        return vec![];
    }
    let mut edges = Vec::new();
    // Release the previously held direction on this axis.
    match *state {
        v if v < 0 => edges.push((neg, false)),
        v if v > 0 => edges.push((pos, false)),
        _ => {}
    }
    // Press the new direction.
    match value {
        v if v < 0 => edges.push((neg, true)),
        v if v > 0 => edges.push((pos, true)),
        _ => {}
    }
    *state = value;
    edges
}
