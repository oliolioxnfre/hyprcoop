use std::io::{BufRead, BufReader};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::Command;
use std::sync::mpsc::{channel, Receiver, Sender};

use anyhow::{bail, Context, Result};
use serde::Deserialize;

pub const TAG_ALL: &str = "hyprcoop";
pub const TAG_KBM: &str = "hyprcoop_kbm";
pub const WORKSPACE: &str = "coop";

pub fn hyprctl(args: &[&str]) -> Result<String> {
    let out = Command::new("hyprctl")
        .args(args)
        .output()
        .context("running hyprctl")?;
    if !out.status.success() {
        bail!(
            "hyprctl {:?} failed: {}",
            args,
            String::from_utf8_lossy(&out.stderr)
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn hyprctl_batch(commands: &[String]) -> Result<()> {
    let out = hyprctl(&["--batch", &commands.join(" ; ")])?;
    // hyprctl exits 0 even for rejected keywords ("invalid field …",
    // "… is deprecated"); every accepted command echoes "ok".
    let lower = out.to_lowercase();
    if lower.contains("invalid") || lower.contains("deprecated") || lower.contains("error") {
        bail!("hyprctl rejected a batch command: {out}");
    }
    Ok(())
}

#[derive(Debug, Clone, Deserialize)]
pub struct Client {
    pub address: String,
    pub pid: i32,
    pub class: String,
    pub title: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub workspace: Option<WorkspaceRef>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct WorkspaceRef {
    pub id: i32,
    pub name: String,
}

pub fn clients() -> Result<Vec<Client>> {
    let json = hyprctl(&["-j", "clients"])?;
    serde_json::from_str(&json).context("parsing hyprctl clients")
}

#[derive(Debug, Deserialize)]
struct WorkspaceInfo {
    id: i32,
    name: String,
}

fn active_workspace() -> Result<WorkspaceInfo> {
    let json = hyprctl(&["-j", "activeworkspace"])?;
    serde_json::from_str(&json).context("parsing activeworkspace")
}

/// Name of the currently active workspace (for returning windows to it).
pub fn active_workspace_name() -> Option<String> {
    active_workspace().ok().map(|ws| ws.name)
}

#[derive(Debug)]
pub enum HyprEvent {
    /// address (without 0x prefix), class
    OpenWindow(String, String),
    CloseWindow(String),
}

/// Listen on Hyprland's socket2 for window events.
pub fn spawn_event_listener() -> Result<Receiver<HyprEvent>> {
    let runtime = std::env::var("XDG_RUNTIME_DIR").context("XDG_RUNTIME_DIR not set")?;
    let signature =
        std::env::var("HYPRLAND_INSTANCE_SIGNATURE").context("not running under Hyprland")?;
    let sock = PathBuf::from(runtime)
        .join("hypr")
        .join(signature)
        .join(".socket2.sock");
    let stream = UnixStream::connect(&sock)
        .with_context(|| format!("connecting to {}", sock.display()))?;
    let (tx, rx) = channel();
    std::thread::spawn(move || listener_loop(stream, tx));
    Ok(rx)
}

fn listener_loop(stream: UnixStream, tx: Sender<HyprEvent>) {
    let reader = BufReader::new(stream);
    for line in reader.lines() {
        let Ok(line) = line else { return };
        let Some((event, data)) = line.split_once(">>") else {
            continue;
        };
        let msg = match event {
            "openwindow" => {
                // openwindow>>ADDRESS,WORKSPACENAME,CLASS,TITLE
                let mut parts = data.splitn(4, ',');
                let address = parts.next().unwrap_or_default().to_string();
                let _ws = parts.next();
                let class = parts.next().unwrap_or_default().to_string();
                HyprEvent::OpenWindow(address, class)
            }
            "closewindow" => HyprEvent::CloseWindow(data.to_string()),
            _ => continue,
        };
        if tx.send(msg).is_err() {
            return;
        }
    }
}

/// Applies the co-op desktop state and restores it on drop:
/// gaps/borders/rounding to 0, waybar hidden, dynamic window rules for
/// tagged game windows, dedicated workspace. Restore = `hyprctl reload`
/// (resets all dynamic keywords/rules to the user's config) + waybar toggle
/// + jump back to the original workspace.
pub struct SessionGuard {
    original_workspace: Option<String>,
    waybar_hidden: bool,
    restored: bool,
}

impl SessionGuard {
    pub fn apply() -> Result<Self> {
        let original_workspace = active_workspace().ok().map(|ws| {
            if ws.id > 0 {
                ws.id.to_string()
            } else {
                format!("name:{}", ws.name)
            }
        });

        // Hyprland ≥0.53 windowrule syntax: `<field> <value>, match:<prop> <pat>`.
        hyprctl_batch(&[
            "keyword general:gaps_in 0".into(),
            "keyword general:gaps_out 0".into(),
            "keyword general:border_size 0".into(),
            "keyword decoration:rounding 0".into(),
            format!("keyword windowrule suppress_event fullscreen maximize, match:tag {TAG_ALL}"),
            format!("keyword windowrule idle_inhibit always, match:tag {TAG_ALL}"),
            format!("keyword windowrule stay_focused on, match:tag {TAG_KBM}"),
        ])?;

        let waybar_hidden = toggle_waybar().is_ok();

        Ok(Self {
            original_workspace,
            waybar_hidden,
            restored: false,
        })
    }

    /// Adopt a newly opened game window: tag it, tile it, move it to the
    /// co-op workspace (and focus that workspace).
    pub fn adopt_window(&self, address: &str, kbm: bool) -> Result<()> {
        let addr = format!("address:0x{}", address.trim_start_matches("0x"));
        let mut cmds = vec![
            format!("dispatch tagwindow +{TAG_ALL} {addr}"),
            format!("dispatch settiled {addr}"),
            format!("dispatch movetoworkspace name:{WORKSPACE},{addr}"),
        ];
        if kbm {
            cmds.push(format!("dispatch tagwindow +{TAG_KBM} {addr}"));
        }
        hyprctl_batch(&cmds)
    }

    pub fn restore(&mut self) {
        if self.restored {
            return;
        }
        self.restored = true;
        let _ = hyprctl(&["reload"]);
        if self.waybar_hidden {
            let _ = toggle_waybar();
        }
        if let Some(ws) = &self.original_workspace {
            let _ = hyprctl(&["dispatch", "workspace", ws]);
        }
    }
}

impl Drop for SessionGuard {
    fn drop(&mut self) {
        self.restore();
    }
}

fn toggle_waybar() -> Result<()> {
    let status = Command::new("pkill")
        .args(["-SIGUSR1", "-x", "waybar"])
        .status()
        .context("running pkill")?;
    if !status.success() {
        bail!("waybar not running");
    }
    Ok(())
}

/// Address (with `0x`) of the currently focused window, if any.
pub fn active_window() -> Option<String> {
    let json = hyprctl(&["-j", "activewindow"]).ok()?;
    let client: Client = serde_json::from_str(&json).ok()?;
    if client.address.is_empty() {
        None
    } else {
        Some(client.address)
    }
}

/// The window hosting this process (our terminal emulator), found by matching
/// our own pid's ancestor chain against the client list.
pub fn own_window() -> Option<String> {
    let ancestors = pid_ancestors(std::process::id());
    let clients = clients().ok()?;
    clients
        .into_iter()
        .find(|c| ancestors.contains(&(c.pid as u32)))
        .map(|c| c.address)
}

fn addr(address: &str) -> String {
    format!("address:0x{}", address.trim_start_matches("0x"))
}

pub fn focus_window(address: &str) -> Result<()> {
    hyprctl(&["dispatch", "focuswindow", &addr(address)]).map(|_| ())
}

/// Float the hyprcoop terminal, drop it on the coop workspace and focus it so
/// the on-screen keyboard is visible over the games.
pub fn present_osk_terminal(address: &str) -> Result<()> {
    let a = addr(address);
    hyprctl_batch(&[
        format!("dispatch movetoworkspace name:{WORKSPACE},{a}"),
        format!("dispatch setfloating {a}"),
        format!("dispatch resizewindowpixel exact 60% 55%,{a}"),
        format!("dispatch centerwindow {a}"),
        format!("dispatch focuswindow {a}"),
    ])
}

/// Undo [`present_osk_terminal`]: send the terminal back to a workspace and
/// retile it.
pub fn dismiss_osk_terminal(address: &str, workspace: &str) -> Result<()> {
    let a = addr(address);
    hyprctl_batch(&[
        format!("dispatch settiled {a}"),
        format!("dispatch movetoworkspacesilent {workspace},{a}"),
    ])
}

/// Walk /proc to find a pid's ancestor chain (pid itself first).
pub fn pid_ancestors(mut pid: u32) -> Vec<u32> {
    let mut chain = Vec::new();
    while pid > 1 && chain.len() < 32 {
        chain.push(pid);
        let Ok(status) = std::fs::read_to_string(format!("/proc/{pid}/status")) else {
            break;
        };
        let Some(ppid) = status
            .lines()
            .find_map(|l| l.strip_prefix("PPid:"))
            .and_then(|v| v.trim().parse::<u32>().ok())
        else {
            break;
        };
        pid = ppid;
    }
    chain
}
