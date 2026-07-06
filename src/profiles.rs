use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::handler::ConfigPatch;

pub fn data_dir() -> PathBuf {
    dirs::data_dir()
        .expect("XDG data dir")
        .join("hyprcoop")
}

/// Per-player persistent profile dir, e.g. ~/.local/share/hyprcoop/profiles/player2
pub fn profile_dir(name: &str) -> Result<PathBuf> {
    let dir = data_dir().join("profiles").join(name);
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("creating profile dir {}", dir.display()))?;
    Ok(dir)
}

/// Dir inside the profile that gets bind-mounted over the game's config dir
/// (e.g. profiles/player2/.klei over ~/.klei).
pub fn config_bind_dir(profile: &Path, config_dir: &Path) -> Result<PathBuf> {
    let base = config_dir
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "config".into());
    let dir = profile.join(base);
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("creating config bind dir {}", dir.display()))?;
    Ok(dir)
}

/// Apply handler config patches inside an isolated config dir.
pub fn apply_patches(config_root: &Path, patches: &[ConfigPatch]) -> Result<()> {
    for patch in patches {
        for file in expand_pattern(config_root, &patch.file) {
            patch_ini(&file, &patch.section, &patch.set)
                .with_context(|| format!("patching {}", file.display()))?;
        }
    }
    Ok(())
}

/// Expand a relative path where a `*` component matches any directory entry.
fn expand_pattern(root: &Path, pattern: &str) -> Vec<PathBuf> {
    let mut candidates = vec![root.to_path_buf()];
    for comp in pattern.split('/') {
        let mut next = Vec::new();
        for base in &candidates {
            if comp == "*" {
                if let Ok(entries) = std::fs::read_dir(base) {
                    for entry in entries.flatten() {
                        next.push(entry.path());
                    }
                }
            } else {
                next.push(base.join(comp));
            }
        }
        candidates = next;
    }
    candidates.into_iter().filter(|p| p.is_file()).collect()
}

/// Minimal INI patcher: sets keys in a section, preserving everything else.
/// Creates the section (and file) if missing.
fn patch_ini(
    path: &Path,
    section: &str,
    values: &std::collections::HashMap<String, String>,
) -> Result<()> {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let mut lines: Vec<String> = text.lines().map(|l| l.to_string()).collect();

    let header = format!("[{section}]");
    let section_start = lines
        .iter()
        .position(|l| l.trim().eq_ignore_ascii_case(&header));

    let section_start = match section_start {
        Some(i) => i,
        None => {
            if !lines.is_empty() {
                lines.push(String::new());
            }
            lines.push(header.clone());
            lines.len() - 1
        }
    };
    let section_end = lines[section_start + 1..]
        .iter()
        .position(|l| l.trim().starts_with('['))
        .map(|off| section_start + 1 + off)
        .unwrap_or(lines.len());

    let mut remaining: Vec<(&String, &String)> = values.iter().collect();
    for line in &mut lines[section_start + 1..section_end] {
        let Some((key, _)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim().to_string();
        remaining.retain(|(k, v)| {
            if key.eq_ignore_ascii_case(k) {
                *line = format!("{key} = {v}");
                false
            } else {
                true
            }
        });
    }
    // Insert keys that weren't present, right after the section header.
    for (i, (key, value)) in remaining.into_iter().enumerate() {
        lines.insert(section_start + 1 + i, format!("{key} = {value}"));
    }

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, lines.join("\n") + "\n")?;
    Ok(())
}
