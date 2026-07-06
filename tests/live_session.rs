//! Live end-to-end session test against the running Hyprland compositor,
//! using mpv idle windows as stand-ins for game instances.
//!
//! This intentionally mutates the live desktop (gaps, waybar, workspace) and
//! restores it afterwards, so it is #[ignore]d by default. Run explicitly:
//!
//!     cargo test --test live_session -- --ignored --nocapture

use std::collections::HashMap;
use std::time::{Duration, Instant};

use evdev::uinput::VirtualDevice;
use evdev::{AttributeSet, KeyCode};

use hyprcoop::handler::{GameHandler, LoadedHandler};
use hyprcoop::hypr;
use hyprcoop::input::PadInfo;
use hyprcoop::launch::{self, Assignment};

fn mpv_handler() -> LoadedHandler {
    LoadedHandler {
        handler: GameHandler {
            name: "mpv (live test)".into(),
            short: "mpv-test".into(),
            steam_appid: 0,
            exe: "bin/mpv".into(),
            workdir: None,
            game_dirs: vec!["/usr".into()],
            window_class: Some("mpv".into()),
            config_dir: None,
            steam_api_lib: None,
            goldberg: false,
            env: HashMap::new(),
            args: vec![
                "--idle=yes".into(),
                "--force-window=yes".into(),
                "--title=hyprcoop-live-test".into(),
            ],
            max_players: 4,
            config_patch: vec![],
            notes: None,
        },
        game_dir: Some("/usr".into()),
    }
}

fn make_virtual_pad() -> (VirtualDevice, PadInfo) {
    let mut keys = AttributeSet::<KeyCode>::new();
    keys.insert(KeyCode::BTN_SOUTH);
    let mut dev = VirtualDevice::builder()
        .expect("open /dev/uinput")
        .name("hyprcoop-live-pad")
        .with_keys(&keys)
        .expect("keys")
        .build()
        .expect("virtual pad");
    let node = dev
        .enumerate_dev_nodes_blocking()
        .expect("nodes")
        .flatten()
        .find(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("event"))
        })
        .expect("event node");
    std::thread::sleep(Duration::from_millis(400));
    (
        dev,
        PadInfo {
            event_path: node,
            js_path: None,
            name: "hyprcoop-live-pad".into(),
        },
    )
}

fn gaps_in() -> String {
    hypr::hyprctl(&["-j", "getoption", "general:gaps_in"]).expect("getoption")
}

#[test]
#[ignore = "mutates the live Hyprland desktop; run explicitly"]
fn full_session_with_dummy_windows() {
    let gaps_before = gaps_in();

    let (_pad_dev, pad) = make_virtual_pad();
    let players = vec![Assignment::Pad(pad), Assignment::Kbm];
    let mut session = launch::launch(&mpv_handler(), &players).expect("launch session");

    // Gaps must be zeroed while the session is active.
    assert!(
        gaps_in().contains("\"custom\": \"0 0 0 0\""),
        "gaps not zeroed: {}",
        gaps_in()
    );

    // Wait for both windows to be adopted.
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        session.tick();
        if session.instances.iter().all(|i| i.window.is_some()) {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let addresses: Vec<String> = session
        .instances
        .iter()
        .map(|i| {
            i.window
                .clone()
                .expect("window should be adopted within 20s")
        })
        .collect();

    // Both windows are tagged, tiled onto the coop workspace; the KB/M
    // window additionally has the stayfocused tag.
    let clients = hypr::clients().expect("clients");
    for (idx, addr) in addresses.iter().enumerate() {
        let client = clients
            .iter()
            .find(|c| c.address.trim_start_matches("0x") == addr.trim_start_matches("0x"))
            .expect("adopted window in client list");
        assert!(
            client.tags.iter().any(|t| t.starts_with(hypr::TAG_ALL)),
            "missing hyprcoop tag: {:?}",
            client.tags
        );
        let is_kbm = matches!(session.instances[idx].assignment, Assignment::Kbm);
        assert_eq!(
            client.tags.iter().any(|t| t.starts_with(hypr::TAG_KBM)),
            is_kbm,
            "kbm tag mismatch for instance {idx}: {:?}",
            client.tags
        );
        let ws = client.workspace.as_ref().expect("workspace");
        assert_eq!(ws.name, hypr::WORKSPACE, "window not on coop workspace");
    }

    session.shutdown();

    // Desktop restored: gaps back to the original value, mpv gone.
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(gaps_in(), gaps_before, "gaps not restored");
    let clients = hypr::clients().expect("clients");
    assert!(
        !clients.iter().any(|c| c.title == "hyprcoop-live-test"),
        "test windows still alive after shutdown"
    );
}
