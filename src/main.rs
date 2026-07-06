use std::time::Duration;

use anyhow::Result;
use ratatui::crossterm::event::{self, Event, KeyEventKind};

use hyprcoop::app::App;
use hyprcoop::{handler, input, launch, ui};

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("fetch-goldberg") => {
            let path = launch::goldberg::fetch_goldberg()?;
            println!("Goldberg installed at {}", path.display());
            Ok(())
        }
        Some("doctor") => doctor(),
        Some(other) => {
            eprintln!("unknown command: {other}\nusage: hyprcoop [doctor|fetch-goldberg]");
            std::process::exit(2);
        }
        None => run_tui(),
    }
}

fn run_tui() -> Result<()> {
    let handlers = handler::load_handlers()?;
    let pad_events = input::spawn_input_manager();
    let mut app = App::new(handlers, pad_events);
    app.status = "welcome — select a game".into();

    let mut terminal = ratatui::init();
    let result = (|| -> Result<()> {
        loop {
            terminal.draw(|frame| ui::draw(frame, &app))?;
            if event::poll(Duration::from_millis(50))?
                && let Event::Key(key) = event::read()?
                    && key.kind == KeyEventKind::Press {
                        app.on_key(key.code);
                    }
            app.tick();
            if app.should_quit {
                return Ok(());
            }
        }
    })();
    app.quit_cleanup();
    ratatui::restore();
    result
}

fn doctor() -> Result<()> {
    let check = |name: &str, ok: bool, hint: &str| {
        println!(
            "{} {name}{}",
            if ok { "✔" } else { "✘" },
            if ok || hint.is_empty() {
                String::new()
            } else {
                format!("  — {hint}")
            }
        );
    };

    let has = |bin: &str| {
        std::process::Command::new("which")
            .arg(bin)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    };

    check("bwrap (bubblewrap)", has("bwrap"), "pacman -S bubblewrap");
    check("hyprctl", has("hyprctl"), "hyprcoop requires Hyprland");
    check(
        "running under Hyprland",
        std::env::var("HYPRLAND_INSTANCE_SIGNATURE").is_ok(),
        "HYPRLAND_INSTANCE_SIGNATURE not set",
    );
    check(
        "goldberg steam emu",
        launch::goldberg::goldberg_so_path().is_file(),
        "run `hyprcoop fetch-goldberg`",
    );
    check(
        "/dev/uinput writable (controller keyboard)",
        uinput_writable(),
        "add udev rule / join the `input` group so hyprcoop can create a virtual keyboard",
    );

    match handler::load_handlers() {
        Ok(handlers) => {
            for loaded in &handlers {
                check(
                    &format!("game: {}", loaded.handler.name),
                    loaded.installed(),
                    "install not found",
                );
            }
        }
        Err(err) => check("game handlers", false, &format!("{err:#}")),
    }

    fn uinput_writable() -> bool {
        std::fs::OpenOptions::new()
            .write(true)
            .open("/dev/uinput")
            .is_ok()
    }

    let pads = input::scan_pads();
    check(
        &format!("controllers detected: {}", pads.len()),
        !pads.is_empty(),
        "connect a controller (or check /dev/input permissions)",
    );
    for pad in pads {
        println!(
            "    {} ({}{})",
            pad.name,
            pad.event_path.display(),
            pad.js_path
                .map(|j| format!(", {}", j.display()))
                .unwrap_or_default()
        );
    }
    Ok(())
}
