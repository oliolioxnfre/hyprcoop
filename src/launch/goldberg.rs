use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

use crate::profiles;

/// gbe_fork's LAN discovery port window (see its `dll/dll/network.h`):
/// instances bind sequential ports from `DEFAULT_PORT` and announce across
/// `[DEFAULT_PORT, DEFAULT_PORT + NUM_QUERY_PORTS)`.
const DEFAULT_PORT: u16 = 47584;
const NUM_QUERY_PORTS: u16 = 10;

/// Location of the Goldberg 64-bit libsteam_api.so managed by hyprcoop.
pub fn goldberg_so_path() -> PathBuf {
    profiles::data_dir().join("goldberg/libsteam_api.so")
}

pub fn ensure_goldberg() -> Result<PathBuf> {
    let path = goldberg_so_path();
    if path.is_file() {
        return Ok(path);
    }
    bail!(
        "Goldberg Steam Emu not found at {}.\n\
         Run `hyprcoop fetch-goldberg` to download it (gbe_fork release), or place\n\
         a 64-bit Goldberg libsteam_api.so there yourself.",
        path.display()
    )
}

/// Build a shadow copy of the game dir for one Goldberg instance:
/// every entry is symlinked from the real install, except the directories on
/// the path to the Steam API lib (and the exe), which are materialized so we
/// can swap in Goldberg's libsteam_api.so and drop steam_appid.txt plus a
/// per-instance steam_settings dir beside the .so (where gbe_fork looks).
#[allow(clippy::too_many_arguments)]
pub fn build_shadow(
    game_dir: &Path,
    exe_rel: &str,
    steam_api_rel: &str,
    appid: u32,
    instance: usize,
    goldberg_so: &Path,
    save_dir: &Path,
    copy_instead: &[String],
) -> Result<PathBuf> {
    let shadow = profiles::data_dir()
        .join("shadow")
        .join(format!("{}-{}", sanitize(game_dir), instance));
    if shadow.exists() {
        std::fs::remove_dir_all(&shadow)
            .with_context(|| format!("clearing old shadow dir {}", shadow.display()))?;
    }

    // Directories that must be real (not symlinks) in the shadow tree.
    let mut real_dirs: Vec<PathBuf> = Vec::new();
    for rel in [exe_rel, steam_api_rel] {
        let mut cur = PathBuf::new();
        for comp in Path::new(rel).parent().unwrap_or(Path::new("")).components() {
            cur.push(comp);
            if !real_dirs.contains(&cur) {
                real_dirs.push(cur.clone());
            }
        }
    }

    farm(game_dir, &shadow, Path::new(""), &real_dirs)?;

    // Swap in Goldberg.
    let api_dest = shadow.join(steam_api_rel);
    if api_dest.exists() {
        std::fs::remove_file(&api_dest)?;
    }
    std::fs::copy(goldberg_so, &api_dest)
        .with_context(|| format!("installing goldberg at {}", api_dest.display()))?;

    // Replace selected executables' symlinks with real copies so their
    // $ORIGIN-relative RPATH resolves to this shadow's Goldberg lib64.
    for rel in copy_instead {
        let src = game_dir.join(rel);
        if !src.is_file() {
            continue; // not present in this install; nothing to copy
        }
        let dst = shadow.join(rel);
        if dst.is_symlink() || dst.exists() {
            std::fs::remove_file(&dst).ok();
        }
        std::fs::copy(&src, &dst)
            .with_context(|| format!("copying {} into shadow", rel))?;
        let perms = std::fs::metadata(&src)?.permissions();
        std::fs::set_permissions(&dst, perms).ok();
    }

    // gbe_fork reads steam_settings from the directory containing the .so;
    // steam_appid.txt is also honored next to the exe (and cwd), so write both.
    let api_dir = api_dest
        .parent()
        .map(|p| p.to_path_buf())
        .context("steam api lib has no parent dir")?;
    let exe_dir = shadow
        .join(exe_rel)
        .parent()
        .map(|p| p.to_path_buf())
        .context("exe has no parent dir")?;
    std::fs::write(exe_dir.join("steam_appid.txt"), format!("{appid}"))?;

    let settings = api_dir.join("steam_settings");
    std::fs::create_dir_all(&settings)?;
    std::fs::write(settings.join("steam_appid.txt"), format!("{appid}"))?;
    // Unique identity per instance + per-instance emu save location so the
    // instances don't fight over the global "GSE Saves" dir.
    let gse_saves = save_dir.join("gse-saves");
    std::fs::create_dir_all(&gse_saves)?;
    std::fs::write(
        settings.join("configs.user.ini"),
        format!(
            "[user::general]\n\
             account_name=Player{}\n\
             account_steamid=7656119{:010}\n\
             [user::saves]\n\
             local_save_path={}\n",
            instance + 1,
            8000000000u64 + instance as u64,
            gse_saves.display(),
        ),
    )?;

    // Same-machine LAN discovery, firewall-proof.
    //
    // gbe_fork's peer announce broadcasts to 255.255.255.255 / the subnet
    // broadcast across a port range (DEFAULT_PORT 47584 .. +NUM_QUERY_PORTS 10),
    // which is how it normally finds same-machine instances (each binds a
    // distinct sequential port 47584, 47585, … because SO_REUSEADDR is disabled
    // in its Linux build). But a `deny incoming` firewall — Omarchy's default —
    // drops those broadcasts as they arrive back on the real NIC.
    //
    // The emu's `custom_broadcasts` are exempt because we point them at
    // loopback, but it sends each entry only to that entry's *own* port (not the
    // range). So a bare `127.0.0.1` (port 0) reaches nobody. We therefore list
    // loopback once per port in the query range, so every instance's announce
    // reaches every other instance's listen port over `lo`. Verified end to end.
    let broadcasts: String = (DEFAULT_PORT..DEFAULT_PORT + NUM_QUERY_PORTS)
        .map(|port| format!("127.0.0.1:{port}\n"))
        .collect();
    std::fs::write(settings.join("custom_broadcasts.txt"), broadcasts)?;

    Ok(shadow)
}

/// Recursively build the symlink farm. Dirs listed in `real_dirs` (relative
/// paths) are created as real directories and recursed into; everything else
/// is a symlink to the original.
fn farm(src_root: &Path, dst_root: &Path, rel: &Path, real_dirs: &[PathBuf]) -> Result<()> {
    let src = src_root.join(rel);
    let dst = dst_root.join(rel);
    std::fs::create_dir_all(&dst)?;
    for entry in std::fs::read_dir(&src)
        .with_context(|| format!("reading {}", src.display()))?
        .flatten()
    {
        let name = entry.file_name();
        let child_rel = rel.join(&name);
        if real_dirs.contains(&child_rel) {
            farm(src_root, dst_root, &child_rel, real_dirs)?;
        } else {
            symlink(entry.path(), dst.join(&name))
                .with_context(|| format!("symlinking {}", child_rel.display()))?;
        }
    }
    Ok(())
}

fn sanitize(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().replace(['/', ' ', '\''], "_"))
        .unwrap_or_else(|| "game".into())
}

/// Download the latest gbe_fork release and extract the regular 64-bit
/// libsteam_api.so to the managed location. Shells out to curl + bsdtar.
pub fn fetch_goldberg() -> Result<PathBuf> {
    let dest_dir = profiles::data_dir().join("goldberg");
    std::fs::create_dir_all(&dest_dir)?;
    let tmp = dest_dir.join("download");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp)?;

    let url =
        "https://github.com/Detanup01/gbe_fork/releases/latest/download/emu-linux-release.tar.bz2";
    let archive = tmp.join("emu-linux-release.tar.bz2");
    run(&["curl", "-fL", "-o", &archive.to_string_lossy(), url])
        .context("downloading gbe_fork release")?;
    run(&[
        "bsdtar",
        "-xf",
        &archive.to_string_lossy(),
        "-C",
        &tmp.to_string_lossy(),
    ])
    .context("extracting gbe_fork release")?;

    // Keep both 64-bit variants (regular is the default; experimental is what
    // Nucleus uses for some games and is one file-copy away as a fallback).
    let mut regular: Option<PathBuf> = None;
    let mut experimental: Option<PathBuf> = None;
    let mut stack = vec![tmp.clone()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir)?.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.file_name().and_then(|n| n.to_str()) == Some("libsteam_api.so") {
                let p = path.to_string_lossy().to_lowercase();
                if !(p.contains("64") || p.contains("x64")) {
                    continue;
                }
                if p.contains("experimental") {
                    experimental = Some(path);
                } else {
                    regular = Some(path);
                }
            }
        }
    }
    let regular = regular.context("no 64-bit libsteam_api.so found in gbe_fork release")?;
    std::fs::copy(&regular, dest_dir.join("libsteam_api.regular.so"))?;
    if let Some(experimental) = experimental {
        std::fs::copy(&experimental, dest_dir.join("libsteam_api.experimental.so"))?;
    }
    let dest = goldberg_so_path();
    std::fs::copy(&regular, &dest)?;
    let _ = std::fs::remove_dir_all(&tmp);
    Ok(dest)
}

fn run(argv: &[&str]) -> Result<()> {
    let status = std::process::Command::new(argv[0])
        .args(&argv[1..])
        .status()
        .with_context(|| format!("running {}", argv[0]))?;
    if !status.success() {
        bail!("{} failed with {status}", argv[0]);
    }
    Ok(())
}
