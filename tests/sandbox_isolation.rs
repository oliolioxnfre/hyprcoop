//! Verifies bwrap input isolation: an instance sandbox must see only the
//! pad nodes assigned to it, and hidraw nodes must be neutralized.

use std::path::PathBuf;
use std::time::Duration;

use evdev::uinput::VirtualDevice;
use evdev::{AttributeSet, KeyCode};

use hyprcoop::launch::sandbox::{build_command, SandboxSpec};

fn make_pad(name: &str) -> (VirtualDevice, PathBuf) {
    let mut keys = AttributeSet::<KeyCode>::new();
    keys.insert(KeyCode::BTN_SOUTH);
    let mut dev = VirtualDevice::builder()
        .expect("open /dev/uinput")
        .name(name)
        .with_keys(&keys)
        .expect("set keys")
        .build()
        .expect("create virtual pad");
    let node = dev
        .enumerate_dev_nodes_blocking()
        .expect("enumerate nodes")
        .flatten()
        .find(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("event"))
        })
        .expect("event node");
    (dev, node)
}

fn run_in_sandbox(spec: &SandboxSpec) -> String {
    let out = build_command(spec)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .expect("run bwrap");
    assert!(
        out.status.success(),
        "bwrap failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn sandbox_sees_only_assigned_pad() {
    let (_pad_a, node_a) = make_pad("hyprcoop-iso-a");
    let (_pad_b, node_b) = make_pad("hyprcoop-iso-b");
    std::thread::sleep(Duration::from_millis(500));

    let spec = SandboxSpec {
        input_nodes: vec![node_a.clone()],
        binds: vec![],
        env: vec![],
        workdir: PathBuf::from("/"),
        command: vec!["ls".into(), "/dev/input".into()],
    };
    let listing = run_in_sandbox(&spec);

    let name_a = node_a.file_name().unwrap().to_str().unwrap();
    let name_b = node_b.file_name().unwrap().to_str().unwrap();
    assert!(
        listing.contains(name_a),
        "assigned pad {name_a} missing from sandbox: {listing}"
    );
    assert!(
        !listing.contains(name_b),
        "unassigned pad {name_b} leaked into sandbox: {listing}"
    );
    // No other event nodes leak either: only the assigned one.
    let event_nodes: Vec<&str> = listing
        .split_whitespace()
        .filter(|l| l.starts_with("event"))
        .collect();
    assert_eq!(event_nodes, vec![name_a], "unexpected nodes: {listing}");
}

#[test]
fn sandbox_masks_hidraw_and_sets_sdl_env() {
    let spec = SandboxSpec {
        input_nodes: vec![],
        binds: vec![],
        env: vec![],
        workdir: PathBuf::from("/"),
        command: vec![
            "sh".into(),
            "-c".into(),
            "echo hidapi=$SDL_JOYSTICK_HIDAPI; for f in /dev/hidraw*; do \
             [ -e \"$f\" ] && stat -c '%n %t:%T' \"$f\"; done"
                .into(),
        ],
    };
    let out = run_in_sandbox(&spec);
    assert!(out.contains("hidapi=0"), "SDL_JOYSTICK_HIDAPI not set: {out}");
    // Any hidraw node present must be /dev/null in disguise (major 1, minor 3).
    for line in out.lines().filter(|l| l.starts_with("/dev/hidraw")) {
        assert!(
            line.ends_with("1:3"),
            "hidraw node not masked with /dev/null: {line}"
        );
    }
}

#[test]
fn sandbox_bind_overlays_directory() {
    let src = tempdir("hyprcoop-bind-src");
    std::fs::write(src.join("marker.txt"), "from-profile").unwrap();
    let dest = tempdir("hyprcoop-bind-dest");

    let spec = SandboxSpec {
        input_nodes: vec![],
        binds: vec![(src.clone(), dest.clone())],
        env: vec![],
        workdir: PathBuf::from("/"),
        command: vec!["cat".into(), format!("{}/marker.txt", dest.display())],
    };
    let out = run_in_sandbox(&spec);
    assert_eq!(out.trim(), "from-profile");
    // Outside the sandbox the dest dir is untouched.
    assert!(!dest.join("marker.txt").exists());

    let _ = std::fs::remove_dir_all(&src);
    let _ = std::fs::remove_dir_all(&dest);
}

fn tempdir(prefix: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("{prefix}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}
