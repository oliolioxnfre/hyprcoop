//! End-to-end test of gamepad detection and press-to-claim events using a
//! virtual uinput gamepad. Requires write access to /dev/uinput (udev uaccess
//! ACL or the input group), which is present on typical desktop setups.

use std::time::{Duration, Instant};

use evdev::uinput::VirtualDevice;
use evdev::{AttributeSet, EventType, InputEvent, KeyCode};

use hyprcoop::input::{scan_pads, spawn_input_manager, PadButton, PadEvent};

const PAD_NAME: &str = "hyprcoop-test-pad";

fn make_virtual_pad() -> VirtualDevice {
    let mut keys = AttributeSet::<KeyCode>::new();
    keys.insert(KeyCode::BTN_SOUTH);
    keys.insert(KeyCode::BTN_EAST);
    keys.insert(KeyCode::BTN_START);
    VirtualDevice::builder()
        .expect("open /dev/uinput")
        .name(PAD_NAME)
        .with_keys(&keys)
        .expect("set keys")
        .build()
        .expect("create virtual pad")
}

#[test]
fn virtual_pad_is_detected_and_button_press_flows() {
    let mut pad = make_virtual_pad();
    // Let udev create the node and settle permissions.
    std::thread::sleep(Duration::from_millis(500));

    let pads = scan_pads();
    let found = pads
        .iter()
        .find(|p| p.name == PAD_NAME)
        .expect("virtual pad should be detected as a gamepad");
    assert!(found.event_path.to_string_lossy().contains("event"));

    let rx = spawn_input_manager();

    // Wait until the manager reports the pad.
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut added = false;
    while Instant::now() < deadline {
        match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(PadEvent::Added(info)) if info.name == PAD_NAME => {
                added = true;
                break;
            }
            Ok(_) => {}
            Err(_) => {}
        }
    }
    assert!(added, "input manager should report the virtual pad");

    // Press and release BTN_SOUTH; expect an Accept press then release.
    std::thread::sleep(Duration::from_millis(200));
    pad.emit(&[
        InputEvent::new(EventType::KEY.0, KeyCode::BTN_SOUTH.code(), 1),
        InputEvent::new(EventType::KEY.0, KeyCode::BTN_SOUTH.code(), 0),
    ])
    .expect("emit button press");

    let deadline = Instant::now() + Duration::from_secs(5);
    let mut got_press = false;
    let mut got_release = false;
    while Instant::now() < deadline && !(got_press && got_release) {
        match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(PadEvent::Button {
                path,
                button: PadButton::Accept,
                pressed,
            }) => {
                assert_eq!(path, found.event_path);
                if pressed {
                    got_press = true;
                } else {
                    got_release = true;
                }
            }
            Ok(_) => {}
            Err(_) => {}
        }
    }
    assert!(got_press, "button press should reach the input manager");
    assert!(got_release, "button release should reach the input manager");
}
