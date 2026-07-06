use std::path::PathBuf;
use std::process::Command;

/// Everything needed to launch one sandboxed game instance.
#[derive(Debug)]
pub struct SandboxSpec {
    /// Pad device nodes this instance is allowed to see. All other
    /// /dev/input nodes are masked by a tmpfs.
    pub input_nodes: Vec<PathBuf>,
    /// Read-write binds applied inside the sandbox (src, dest),
    /// e.g. the player's profile klei dir over ~/.klei.
    pub binds: Vec<(PathBuf, PathBuf)>,
    pub env: Vec<(String, String)>,
    pub workdir: PathBuf,
    /// Program + args (absolute exe path).
    pub command: Vec<String>,
}

/// Build the bwrap command implementing per-instance input isolation:
/// - the whole system is passed through (`--dev-bind / /`) so GPU, X11/Wayland
///   sockets, and networking all work normally;
/// - a tmpfs over /dev/input hides every input node, then only this player's
///   pad nodes are bound back in;
/// - all /dev/hidraw nodes are masked (SDL's HIDAPI backend would otherwise
///   see PS/Switch/Xbox-BT pads in every instance) and SDL_JOYSTICK_HIDAPI=0
///   forces the evdev path as belt-and-suspenders.
pub fn build_command(spec: &SandboxSpec) -> Command {
    let mut cmd = Command::new("bwrap");
    cmd.args(["--die-with-parent", "--dev-bind", "/", "/"]);
    cmd.args(["--tmpfs", "/dev/input"]);

    for node in &spec.input_nodes {
        let node = node.to_string_lossy();
        cmd.args(["--dev-bind", &node, &node]);
    }
    for hidraw in list_hidraw_nodes() {
        let hidraw = hidraw.to_string_lossy();
        cmd.args(["--dev-bind", "/dev/null", &hidraw]);
    }
    for (src, dest) in &spec.binds {
        cmd.args([
            "--bind",
            &src.to_string_lossy(),
            &dest.to_string_lossy(),
        ]);
    }

    cmd.arg("--");
    cmd.args(&spec.command);

    cmd.current_dir(&spec.workdir);
    cmd.env("SDL_JOYSTICK_HIDAPI", "0");
    // Only one window holds compositor focus, but every instance must keep
    // reading its pad — without this SDL drops joystick events when unfocused.
    cmd.env("SDL_JOYSTICK_ALLOW_BACKGROUND_EVENTS", "1");
    for (key, value) in &spec.env {
        cmd.env(key, value);
    }
    cmd
}

fn list_hidraw_nodes() -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir("/dev") else {
        return Vec::new();
    };
    entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("hidraw"))
        })
        .collect()
}
