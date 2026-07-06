pub mod goldberg;
pub mod sandbox;

use std::path::PathBuf;
use std::process::{Child, ExitStatus};

use anyhow::{bail, Context, Result};

use crate::handler::{expand_env_value, expand_tilde, LoadedHandler};
use crate::hypr::{self, SessionGuard};
use crate::input::PadInfo;
use crate::profiles;

#[derive(Debug, Clone)]
pub enum Assignment {
    Pad(PadInfo),
    Kbm,
}

impl Assignment {
    pub fn label(&self) -> String {
        match self {
            Assignment::Pad(pad) => pad.name.clone(),
            Assignment::Kbm => "Keyboard + Mouse".into(),
        }
    }
}

pub struct Instance {
    pub player: usize,
    pub assignment: Assignment,
    pub child: Child,
    pub window: Option<String>,
    pub exit: Option<ExitStatus>,
}

impl Instance {
    pub fn running(&self) -> bool {
        self.exit.is_none()
    }
}

pub struct Session {
    pub instances: Vec<Instance>,
    pub guard: SessionGuard,
    pub events: std::sync::mpsc::Receiver<hypr::HyprEvent>,
}

/// Launch one sandboxed instance per player and apply the Hyprland session.
pub fn launch(loaded: &LoadedHandler, players: &[Assignment]) -> Result<Session> {
    let handler = &loaded.handler;
    let game_dir = loaded
        .game_dir
        .clone()
        .with_context(|| format!("{} is not installed", handler.name))?;

    if players.is_empty() {
        bail!("no players assigned");
    }
    // All instances run Goldberg symmetrically (the Nucleus-proven recipe for
    // e.g. DST: every instance plays offline and meets over LAN discovery).
    // Steam itself doesn't need to be running.
    let goldberg_so = if handler.goldberg {
        Some(goldberg::ensure_goldberg()?)
    } else {
        None
    };

    // Hyprland event stream + desktop state first, so rules exist before
    // any game window opens.
    let events = hypr::spawn_event_listener()?;
    let guard = SessionGuard::apply()?;

    let mut instances = Vec::new();
    for (index, assignment) in players.iter().enumerate() {
        let instance = spawn_instance(
            loaded,
            &game_dir,
            index,
            assignment.clone(),
            goldberg_so.as_deref(),
        )
        .with_context(|| format!("launching instance for player {}", index + 1))?;
        instances.push(instance);
    }

    Ok(Session {
        instances,
        guard,
        events,
    })
}

fn spawn_instance(
    loaded: &LoadedHandler,
    game_dir: &std::path::Path,
    index: usize,
    assignment: Assignment,
    goldberg_so: Option<&std::path::Path>,
) -> Result<Instance> {
    let handler = &loaded.handler;

    // Every player gets an isolated persistent profile dir, bound over the
    // game's config dir so saves/settings never collide.
    let profile = profiles::profile_dir(&format!("player{}", index + 1))?;

    // Every instance runs a Goldberg shadow copy with its own identity.
    let instance_root: PathBuf = match goldberg_so {
        Some(goldberg_so) => {
            let api_rel = handler
                .steam_api_lib
                .as_deref()
                .context("handler has goldberg=true but no steam_api_lib")?;
            goldberg::build_shadow(
                game_dir,
                &handler.exe,
                api_rel,
                handler.steam_appid,
                index,
                goldberg_so,
                &profile,
            )?
        }
        None => game_dir.to_path_buf(),
    };

    let mut binds = Vec::new();
    if let Some(config_dir) = &handler.config_dir {
        let config_abs = expand_tilde(config_dir);
        std::fs::create_dir_all(&config_abs).ok();
        let bind_src = profiles::config_bind_dir(&profile, &config_abs)?;
        profiles::apply_patches(&bind_src, &handler.config_patch)?;
        binds.push((bind_src, config_abs));
    }

    let input_nodes = match &assignment {
        Assignment::Pad(pad) => {
            let mut nodes = vec![pad.event_path.clone()];
            nodes.extend(pad.js_path.clone());
            nodes
        }
        Assignment::Kbm => Vec::new(),
    };

    let exe = instance_root.join(&handler.exe);
    let workdir = handler
        .workdir
        .as_ref()
        .map(|w| instance_root.join(w))
        .unwrap_or_else(|| instance_root.clone());

    let env = handler
        .env
        .iter()
        .map(|(k, v)| (k.clone(), expand_env_value(v, &instance_root)))
        .collect();

    let mut command = vec![exe.to_string_lossy().into_owned()];
    command.extend(handler.args.iter().cloned());

    let spec = sandbox::SandboxSpec {
        input_nodes,
        binds,
        env,
        workdir,
        command,
    };
    let child = sandbox::build_command(&spec)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .context("spawning bwrap")?;

    Ok(Instance {
        player: index,
        assignment,
        child,
        window: None,
        exit: None,
    })
}

impl Session {
    /// Poll child processes and Hyprland events; adopt new game windows.
    pub fn tick(&mut self) {
        for instance in &mut self.instances {
            if instance.exit.is_none()
                && let Ok(Some(status)) = instance.child.try_wait() {
                    instance.exit = Some(status);
                }
        }

        while let Ok(event) = self.events.try_recv() {
            if let hypr::HyprEvent::OpenWindow(address, _class) = event {
                self.match_window(&address);
            }
        }
    }

    /// Map a newly opened window to one of our instances by walking the
    /// window pid's ancestors up to our bwrap child pids.
    fn match_window(&mut self, address: &str) {
        let Ok(clients) = hypr::clients() else { return };
        let Some(client) = clients
            .iter()
            .find(|c| c.address.trim_start_matches("0x") == address.trim_start_matches("0x"))
        else {
            return;
        };
        let ancestors = hypr::pid_ancestors(client.pid as u32);
        let Some(instance) = self.instances.iter_mut().find(|i| {
            i.window.is_none() && ancestors.contains(&i.child.id())
        }) else {
            return;
        };
        instance.window = Some(address.to_string());
        let kbm = matches!(instance.assignment, Assignment::Kbm);
        let _ = self.guard.adopt_window(address, kbm);
    }

    pub fn all_exited(&self) -> bool {
        self.instances.iter().all(|i| !i.running())
    }

    pub fn shutdown(&mut self) {
        for instance in &mut self.instances {
            if instance.running() {
                let _ = instance.child.kill();
                let _ = instance.child.wait();
            }
        }
        self.guard.restore();
    }
}
