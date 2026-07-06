# hyprcoop

A Hyprland-native, Nucleus Co-op-style **split-screen game launcher** with a TUI.
Runs multiple instances of the same game (first target: **Don't Starve Together**),
gives each player their own controller, and lets Hyprland do what it does best:
tile the game windows like any other windows — resize them live with your normal
Hyprland binds while you play.

## How it works

Gamepads on Linux bypass the compositor: SDL games read them straight from
`/dev/input`. That means compositor focus doesn't matter for controllers — but it
also means every instance normally sees *every* pad. hyprcoop fixes that per
instance:

| Piece | Job |
|---|---|
| **bubblewrap** | Each instance runs in a sandbox where `/dev/input` contains only that player's pad; `/dev/hidraw*` is masked and `SDL_JOYSTICK_HIDAPI=0` is set so PS/Switch pads can't leak in via HIDAPI. `SDL_JOYSTICK_ALLOW_BACKGROUND_EVENTS=1` keeps unfocused instances reading their pad (the job gamescope does in PartyDeck). |
| **Goldberg (gbe_fork)** | Steam only allows one running copy of a game, so every instance runs a shadow copy (symlink farm) with Goldberg's `libsteam_api.so` and a unique per-player identity. Steam doesn't need to be running. |
| **Hyprland IPC** | A session guard zeroes gaps/borders/rounding, hides waybar, adds window rules (`suppress_event fullscreen`, `stay_focused` for the keyboard player), and adopts each game window onto a dedicated `coop` workspace as it opens. Everything is restored when the session ends — even on crash. |
| **Profiles** | Each player gets a persistent profile dir (`~/.local/share/hyprcoop/profiles/playerN`) bind-mounted over the game's config dir (e.g. `~/.klei`), so saves and settings never collide. |

## Requirements

- Hyprland (tested on 0.55 / Omarchy) and `hyprctl`
- `bubblewrap` (`pacman -S bubblewrap`)
- The game installed via Steam (you must own it — Goldberg only replaces the
  *runtime* Steam API so extra local instances can start and LAN-connect)

## Setup

```sh
cargo build --release
./target/release/hyprcoop fetch-goldberg   # one-time: downloads gbe_fork
./target/release/hyprcoop doctor           # checks everything is in place
./target/release/hyprcoop                  # launch the TUI
```

## Playing Don't Starve Together

1. Start `hyprcoop`, pick *Don't Starve Together*, press Enter.
2. Each player presses a button on their controller to claim a slot
   (press `m` to claim a slot for the keyboard+mouse player — max one).
3. Press Enter. Instances open tiled on the `coop` workspace.
4. In **every** instance: `Play` → **Play Offline**.
5. One player: `Host Game` → create a **local only** world.
6. Everyone else: `Browse Games` → **LAN** tab → join.
7. Resize the splits any time with your normal Hyprland binds.
8. Quit the games (or switch back to the hyprcoop terminal and press `e`)
   — gaps, borders, and waybar come right back.

## Controller keyboard

Hold your controller's two center buttons for 2 seconds to pop up an on-screen
keyboard for typing server names, passwords, chat, etc:

- **DualShock 4:** Options + Share
- **Xbox:** Menu + View (Start + Select)
- **Switch Pro:** (+) and (−)

(All three report the same evdev codes, so it's one unified combo.) Navigate
with the **d-pad**, press a key with **✕ / A / B (south)**, toggle case with
**△ / Y / X (north)**, erase with **□ / X / Y (west)**, cancel with **○ / B / A
(east)**. Pick **Done** to type the text into your game instance.

Under the hood, hyprcoop creates a **uinput virtual keyboard** (no extra
dependency — built on the `evdev` crate) and replays your text into the
triggering player's window. It needs write access to `/dev/uinput`
(`hyprcoop doctor` checks this); on most desktops the `input` group / udev
`uaccess` rule already grants it.

Notes:
- Text is buffered while you edit and injected on **Done**, so keystrokes land
  in the game, not the launcher.
- Your controller still drives your game character while the keyboard is open
  (we don't grab the pad in v1) — open it from a menu/text field. Exclusive pad
  grabbing is a planned improvement.
- If you have a keyboard+mouse player, their `stay_focused` window can fight the
  focus hand-off; the keyboard is most reliable in all-controller sessions for
  now.

## Notes & limitations

- **One keyboard/mouse player max.** Wayland routes all keyboards to the focused
  window; hyprcoop pins focus to the KB/M player's instance (`stay_focused`).
  The KB/M player should keep the cursor over their own split. Multiple
  keyboards would require per-instance gamescope — possible future opt-in.
- **First run of a new profile** may ask the game to go fullscreen; the
  `suppress_event fullscreen` rule keeps it tiled anyway, and hyprcoop patches
  `client.ini` to windowed mode for later runs.
- **LAN discovery is same-machine and firewall-safe by design.** Two pieces make
  it work with no `ufw` changes:
  - Games that spawn their own helper servers (DST's dedicated server) get those
    binaries **copied** into the shadow (`copy_instead` in the handler), so their
    `$ORIGIN/lib64` RPATH loads the Goldberg `libsteam_api.so`. Otherwise they'd
    run on real Steam and never announce on the emu's LAN network — the servers
    simply wouldn't appear, no matter the firewall.
  - Each instance sends Goldberg discovery to `127.0.0.1` across the emu's whole
    query port range (`custom_broadcasts.txt`, `47584–47593`), so announces reach
    peers over loopback — which a `deny incoming` firewall (Omarchy's default)
    always allows, unlike the `255.255.255.255` broadcast it drops.
- **If instances still can't see each other** in the LAN tab, try the
  experimental Goldberg build (the variant Nucleus uses for DST on Windows):
  `cp ~/.local/share/hyprcoop/goldberg/libsteam_api.experimental.so \
      ~/.local/share/hyprcoop/goldberg/libsteam_api.so`
- Progress/skins live in the per-player hyprcoop profiles, separate from your
  normal (online) DST profile.

## Adding more games

Drop a TOML handler in `~/.config/hyprcoop/handlers/` (see `handlers/dst.toml`).
The essentials: where the game lives, the exe, the `libsteam_api.so` path for
the Goldberg swap, the `$HOME` config dir to isolate per player, and optional
INI patches / in-game instructions. Native Linux SDL games are the sweet spot;
Proton titles are future work.

## Development

```sh
cargo test                                        # hermetic tests (uses /dev/uinput + bwrap)
cargo test --test live_session -- --ignored       # live test against your running Hyprland
```

The live test opens two sandboxed mpv windows as fake game instances, checks
adoption/tagging/workspace placement, then restores the desktop.
