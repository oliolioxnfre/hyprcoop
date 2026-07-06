use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::Deserialize;

/// Patch an INI-style file inside the player's isolated config dir.
#[derive(Debug, Clone, Deserialize)]
pub struct ConfigPatch {
    /// Path relative to the isolated config dir. A single `*` path component
    /// matches any directory (e.g. `DoNotStarveTogether/*/client.ini`).
    pub file: String,
    pub section: String,
    pub set: HashMap<String, String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GameHandler {
    pub name: String,
    pub short: String,
    pub steam_appid: u32,
    /// Executable path relative to the game dir.
    pub exe: String,
    /// Working directory relative to the game dir (defaults to the game dir).
    #[serde(default)]
    pub workdir: Option<String>,
    /// Candidate install locations; the first that exists wins.
    pub game_dirs: Vec<String>,
    /// The game window's class (for diagnostics; matching is pid-based).
    #[serde(default)]
    pub window_class: Option<String>,
    /// Config dir under $HOME isolated per player (e.g. `~/.klei`).
    #[serde(default)]
    pub config_dir: Option<String>,
    /// Steam API lib relative to the game dir, swapped for Goldberg.
    #[serde(default)]
    pub steam_api_lib: Option<String>,
    /// Whether instances beyond the first need the Goldberg Steam emu.
    #[serde(default)]
    pub goldberg: bool,
    /// Extra env vars. `{game_dir}` and `{home}` are expanded.
    #[serde(default)]
    pub env: HashMap<String, String>,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default = "default_max_players")]
    pub max_players: u8,
    #[serde(default)]
    pub config_patch: Vec<ConfigPatch>,
    /// In-game instructions shown to players during the session.
    #[serde(default)]
    pub notes: Option<String>,
}

fn default_max_players() -> u8 {
    4
}

/// A handler whose game install has been located on disk.
#[derive(Debug, Clone)]
pub struct LoadedHandler {
    pub handler: GameHandler,
    /// Resolved absolute game dir, or None if not installed.
    pub game_dir: Option<PathBuf>,
}

impl LoadedHandler {
    pub fn installed(&self) -> bool {
        self.game_dir.is_some()
    }
}

pub fn expand_tilde(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/")
        && let Some(home) = dirs::home_dir() {
            return home.join(rest);
        }
    PathBuf::from(path)
}

pub fn expand_env_value(value: &str, game_dir: &Path) -> String {
    let home = dirs::home_dir().unwrap_or_default();
    value
        .replace("{game_dir}", &game_dir.to_string_lossy())
        .replace("{home}", &home.to_string_lossy())
}

fn handler_search_dirs() -> Vec<PathBuf> {
    let mut dirs_out = Vec::new();
    // Dev checkout: handlers/ next to the binary's cargo project or cwd.
    dirs_out.push(PathBuf::from("handlers"));
    if let Some(exe) = std::env::current_exe().ok().and_then(|p| {
        p.parent()
            .map(|d| d.join("../../handlers"))
            .filter(|d| d.is_dir())
    }) {
        dirs_out.push(exe);
    }
    if let Some(cfg) = dirs::config_dir() {
        dirs_out.push(cfg.join("hyprcoop/handlers"));
    }
    if let Some(data) = dirs::data_dir() {
        dirs_out.push(data.join("hyprcoop/handlers"));
    }
    dirs_out
}

pub fn load_handlers() -> Result<Vec<LoadedHandler>> {
    let mut seen: HashMap<String, LoadedHandler> = HashMap::new();
    for dir in handler_search_dirs() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("toml") {
                continue;
            }
            let text = std::fs::read_to_string(&path)
                .with_context(|| format!("reading handler {}", path.display()))?;
            let handler: GameHandler = toml::from_str(&text)
                .with_context(|| format!("parsing handler {}", path.display()))?;
            let game_dir = handler
                .game_dirs
                .iter()
                .map(|d| expand_tilde(d))
                .find(|d| d.is_dir());
            // First occurrence wins (earlier dirs have priority).
            seen.entry(handler.short.clone())
                .or_insert(LoadedHandler { handler, game_dir });
        }
    }
    if seen.is_empty() {
        bail!(
            "no game handlers found (searched: {})",
            handler_search_dirs()
                .iter()
                .map(|d| d.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    let mut list: Vec<_> = seen.into_values().collect();
    list.sort_by(|a, b| a.handler.name.cmp(&b.handler.name));
    Ok(list)
}
