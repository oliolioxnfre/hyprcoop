use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Receiver;
use std::time::Instant;

use crate::handler::LoadedHandler;
use crate::hypr;
use crate::input::{PadButton, PadEvent, PadInfo};
use crate::launch::{self, Assignment, Session};
use crate::osk::{Activation, ComboTracker, Osk, VirtualKeyboard};

/// Desktop state to restore when the on-screen keyboard closes.
struct OskReturn {
    owner_pad: PathBuf,
    prev_focus: Option<String>,
    terminal: Option<String>,
    terminal_home_ws: String,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Screen {
    GameSelect,
    PlayerSetup,
    Session,
}

pub struct App {
    pub handlers: Vec<LoadedHandler>,
    pub selected: usize,
    pub screen: Screen,
    pub slots: Vec<Assignment>,
    pub pads: Vec<PadInfo>,
    pub pad_events: Receiver<PadEvent>,
    pub session: Option<Session>,
    pub status: String,
    pub should_quit: bool,
    pub osk: Option<Osk>,
    combos: HashMap<PathBuf, ComboTracker>,
    keyboard: Option<VirtualKeyboard>,
    osk_return: Option<OskReturn>,
}

impl App {
    pub fn new(handlers: Vec<LoadedHandler>, pad_events: Receiver<PadEvent>) -> Self {
        Self {
            handlers,
            selected: 0,
            screen: Screen::GameSelect,
            slots: Vec::new(),
            pads: Vec::new(),
            pad_events,
            session: None,
            status: String::new(),
            should_quit: false,
            osk: None,
            combos: HashMap::new(),
            keyboard: None,
            osk_return: None,
        }
    }

    pub fn current_handler(&self) -> &LoadedHandler {
        &self.handlers[self.selected]
    }

    fn max_players(&self) -> usize {
        self.current_handler().handler.max_players as usize
    }

    fn kbm_taken(&self) -> bool {
        self.slots.iter().any(|s| matches!(s, Assignment::Kbm))
    }

    pub fn pad_claimed(&self, pad: &PadInfo) -> Option<usize> {
        self.slots.iter().position(
            |s| matches!(s, Assignment::Pad(p) if p.event_path == pad.event_path),
        )
    }

    pub fn tick(&mut self) {
        let now = Instant::now();
        while let Ok(event) = self.pad_events.try_recv() {
            match event {
                PadEvent::Added(pad) => {
                    if !self.pads.iter().any(|p| p.event_path == pad.event_path) {
                        self.pads.push(pad);
                    }
                }
                PadEvent::Removed(path) => {
                    self.pads.retain(|p| p.event_path != path);
                    self.combos.remove(&path);
                    let before = self.slots.len();
                    self.slots.retain(
                        |s| !matches!(s, Assignment::Pad(p) if p.event_path == path),
                    );
                    if self.slots.len() != before {
                        self.status = "a claimed controller was unplugged".into();
                    }
                }
                PadEvent::Button {
                    path,
                    button,
                    pressed,
                } => self.on_pad_button(&path, button, pressed, now),
            }
        }

        // The Start+Select hold fires purely on elapsed time, so poll the
        // trackers every tick rather than only on button events.
        if self.osk.is_none() && self.screen == Screen::Session {
            let fired = self
                .combos
                .iter_mut()
                .find_map(|(path, t)| t.fired(now).then(|| path.clone()));
            if let Some(path) = fired {
                self.open_osk(&path);
            }
        }

        let session_ended = match &mut self.session {
            Some(session) => {
                session.tick();
                session.all_exited()
            }
            None => false,
        };
        if session_ended {
            if self.osk.is_some() {
                self.close_osk(false);
            }
            if let Some(mut session) = self.session.take() {
                session.shutdown();
            }
            self.screen = Screen::PlayerSetup;
            self.status = "all game instances exited — session ended".into();
        }
    }

    fn on_pad_button(&mut self, path: &Path, button: PadButton, pressed: bool, now: Instant) {
        // Claiming players: any button press assigns the pad to a slot.
        if self.screen == Screen::PlayerSetup && pressed {
            self.claim_pad(path);
            return;
        }

        // On-screen keyboard open: only the owner's pad drives it, and only on
        // key-down edges.
        if self.osk.is_some() {
            let owns = self
                .osk_return
                .as_ref()
                .is_some_and(|r| r.owner_pad == path);
            if owns && pressed {
                self.osk_button(button);
            }
            return;
        }

        // Otherwise track the Start+Select hold that summons the keyboard.
        self.combos
            .entry(path.to_path_buf())
            .or_default()
            .set(button, pressed, now);
    }

    fn osk_button(&mut self, button: PadButton) {
        let Some(osk) = &mut self.osk else { return };
        match osk.on_button(button) {
            Activation::Continue => {}
            Activation::Commit => self.close_osk(true),
            Activation::Cancel => self.close_osk(false),
        }
    }

    /// Bring up the on-screen keyboard for the player owning `pad`.
    fn open_osk(&mut self, pad: &Path) {
        let Some(session) = &self.session else { return };
        let Some(instance) = session
            .instances
            .iter()
            .find(|i| matches!(&i.assignment, Assignment::Pad(p) if p.event_path == pad))
        else {
            return;
        };
        let player = instance.player;
        let target_window = instance.window.clone();

        let prev_focus = hypr::active_window();
        let terminal = hypr::own_window();
        let terminal_home_ws = hypr::active_workspace_name().unwrap_or_else(|| "1".into());
        if let Some(term) = &terminal {
            let _ = hypr::present_osk_terminal(term);
        }

        self.osk = Some(Osk::new(player, target_window));
        self.osk_return = Some(OskReturn {
            owner_pad: pad.to_path_buf(),
            prev_focus,
            terminal,
            terminal_home_ws,
        });
        self.status = format!(
            "controller keyboard open for player {} — d-pad to move, ✕/A to type, △/Y shift, □/X erase",
            player + 1
        );
    }

    /// Close the keyboard, optionally injecting the buffered text first.
    fn close_osk(&mut self, commit: bool) {
        let Some(osk) = self.osk.take() else { return };
        let ret = self.osk_return.take();

        // Send our terminal back where it came from.
        if let Some(ret) = &ret
            && let Some(term) = &ret.terminal
        {
            let _ = hypr::dismiss_osk_terminal(term, &ret.terminal_home_ws);
        }

        if commit && !osk.strokes().is_empty() {
            // Focus the player's game window so the replay lands there.
            if let Some(window) = &osk.target_window {
                let _ = hypr::focus_window(window);
                std::thread::sleep(std::time::Duration::from_millis(120));
            }
            match self.type_text(osk.strokes()) {
                Ok(()) => {
                    self.status = format!("typed into player {}'s game", osk.player + 1);
                }
                Err(err) => {
                    self.status = format!("keyboard injection failed: {err:#}");
                }
            }
        } else {
            // Cancelled: hand focus back to whatever had it.
            if let Some(prev) = ret.as_ref().and_then(|r| r.prev_focus.clone()) {
                let _ = hypr::focus_window(&prev);
            }
            self.status = "controller keyboard closed".into();
        }
    }

    fn type_text(&mut self, strokes: &[crate::osk::Stroke]) -> anyhow::Result<()> {
        if self.keyboard.is_none() {
            self.keyboard = Some(VirtualKeyboard::new()?);
        }
        self.keyboard.as_mut().unwrap().type_strokes(strokes)
    }

    fn claim_pad(&mut self, path: &std::path::Path) {
        if self.slots.len() >= self.max_players() {
            return;
        }
        let Some(pad) = self
            .pads
            .iter()
            .find(|p| p.event_path == path)
            .cloned()
        else {
            return;
        };
        if self.pad_claimed(&pad).is_some() {
            return;
        }
        self.status = format!("player {} ← {}", self.slots.len() + 1, pad.name);
        self.slots.push(Assignment::Pad(pad));
    }

    pub fn on_key(&mut self, code: KeyCode) {
        // The keyboard is normally controller-driven; Esc still cancels it so
        // it's testable and never a trap.
        if self.osk.is_some() {
            if let KeyCode::Esc = code {
                self.close_osk(false);
            }
            return;
        }
        match self.screen {
            Screen::GameSelect => self.on_key_game_select(code),
            Screen::PlayerSetup => self.on_key_player_setup(code),
            Screen::Session => self.on_key_session(code),
        }
    }

    fn on_key_game_select(&mut self, code: KeyCode) {
        match code {
            KeyCode::Char('q') | KeyCode::Esc => self.should_quit = true,
            KeyCode::Up | KeyCode::Char('k') => {
                self.selected = self.selected.saturating_sub(1);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.selected = (self.selected + 1).min(self.handlers.len() - 1);
            }
            KeyCode::Enter => {
                if self.current_handler().installed() {
                    self.slots.clear();
                    self.status =
                        "press a button on each controller to claim a player slot".into();
                    self.screen = Screen::PlayerSetup;
                } else {
                    self.status = format!(
                        "{} is not installed",
                        self.current_handler().handler.name
                    );
                }
            }
            _ => {}
        }
    }

    fn on_key_player_setup(&mut self, code: KeyCode) {
        match code {
            KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Esc => {
                self.slots.clear();
                self.screen = Screen::GameSelect;
            }
            KeyCode::Char('m') => {
                if self.kbm_taken() {
                    self.status = "keyboard + mouse is already assigned".into();
                } else if self.slots.len() >= self.max_players() {
                    self.status = "all player slots are full".into();
                } else {
                    self.status = format!("player {} ← keyboard + mouse", self.slots.len() + 1);
                    self.slots.push(Assignment::Kbm);
                }
            }
            KeyCode::Backspace | KeyCode::Char('x') => {
                if self.slots.pop().is_some() {
                    self.status = "removed last player".into();
                }
            }
            KeyCode::Enter => self.start_session(),
            _ => {}
        }
    }

    fn start_session(&mut self) {
        if self.slots.is_empty() {
            self.status = "assign at least one player first".into();
            return;
        }
        let loaded = self.current_handler().clone();
        match launch::launch(&loaded, &self.slots) {
            Ok(session) => {
                self.session = Some(session);
                self.screen = Screen::Session;
                self.status = format!(
                    "launched {} instance(s) — switch back to this workspace to manage the session",
                    self.slots.len()
                );
            }
            Err(err) => {
                self.status = format!("launch failed: {err:#}");
            }
        }
    }

    fn on_key_session(&mut self, code: KeyCode) {
        match code {
            KeyCode::Char('e') | KeyCode::Esc => {
                self.end_session();
                self.screen = Screen::PlayerSetup;
                self.status = "session ended — desktop restored".into();
            }
            KeyCode::Char('q') => {
                self.end_session();
                self.should_quit = true;
            }
            _ => {}
        }
    }

    fn end_session(&mut self) {
        if self.osk.is_some() {
            self.close_osk(false);
        }
        if let Some(mut session) = self.session.take() {
            session.shutdown();
        }
    }

    pub fn quit_cleanup(&mut self) {
        self.end_session();
    }
}

pub use ratatui::crossterm::event::KeyCode;
