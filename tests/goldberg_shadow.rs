//! Shadow game dir builder: symlink farm with a materialized path to the
//! Steam API lib, Goldberg swapped in, and per-instance gbe_fork settings.

use std::path::Path;

use hyprcoop::launch::goldberg::build_shadow;

fn fake_game_dir(root: &Path) {
    std::fs::create_dir_all(root.join("bin64/lib64")).unwrap();
    std::fs::create_dir_all(root.join("data")).unwrap();
    std::fs::write(root.join("bin64/game_x64"), "exe").unwrap();
    std::fs::write(root.join("bin64/lib64/libsteam_api.so"), "real-steam").unwrap();
    std::fs::write(root.join("bin64/lib64/libother.so"), "other").unwrap();
    std::fs::write(root.join("data/assets.bin"), "assets").unwrap();
    std::fs::write(root.join("version.txt"), "1.0").unwrap();
}

#[test]
fn shadow_swaps_goldberg_and_links_the_rest() {
    let tmp = std::env::temp_dir().join(format!("hyprcoop-shadow-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let game = tmp.join("game");
    fake_game_dir(&game);
    let fake_goldberg = tmp.join("libsteam_api.so");
    std::fs::write(&fake_goldberg, "goldberg").unwrap();
    let save_dir = tmp.join("profile");
    std::fs::create_dir_all(&save_dir).unwrap();

    let shadow = build_shadow(
        &game,
        "bin64/game_x64",
        "bin64/lib64/libsteam_api.so",
        322330,
        1,
        &fake_goldberg,
        &save_dir,
    )
    .expect("build shadow");

    // Goldberg is a real file (copied), not a symlink to the original.
    let api = shadow.join("bin64/lib64/libsteam_api.so");
    assert_eq!(std::fs::read_to_string(&api).unwrap(), "goldberg");
    assert!(!api.is_symlink());

    // Untouched entries are symlinks back to the real install.
    let exe = shadow.join("bin64/game_x64");
    assert!(exe.is_symlink());
    assert_eq!(std::fs::read_to_string(&exe).unwrap(), "exe");
    assert!(shadow.join("data").is_symlink());
    assert!(shadow.join("version.txt").is_symlink());
    assert_eq!(
        std::fs::read_to_string(shadow.join("bin64/lib64/libother.so")).unwrap(),
        "other"
    );

    // gbe_fork config: steam_settings beside the .so, appid in both spots.
    assert_eq!(
        std::fs::read_to_string(shadow.join("bin64/steam_appid.txt")).unwrap(),
        "322330"
    );
    let settings = shadow.join("bin64/lib64/steam_settings");
    assert_eq!(
        std::fs::read_to_string(settings.join("steam_appid.txt")).unwrap(),
        "322330"
    );
    let user_ini = std::fs::read_to_string(settings.join("configs.user.ini")).unwrap();
    assert!(user_ini.contains("account_name=Player2"), "{user_ini}");
    assert!(user_ini.contains("account_steamid=76561198000000001"), "{user_ini}");
    assert!(user_ini.contains("local_save_path="), "{user_ini}");
    assert!(save_dir.join("gse-saves").is_dir());

    // Same-machine LAN discovery over loopback, one entry per port in
    // gbe_fork's query range so a peer on any of those ports is reached.
    let broadcasts = std::fs::read_to_string(settings.join("custom_broadcasts.txt")).unwrap();
    assert!(broadcasts.contains("127.0.0.1:47584"), "{broadcasts}");
    assert!(broadcasts.contains("127.0.0.1:47593"), "{broadcasts}");
    assert_eq!(
        broadcasts.lines().filter(|l| l.starts_with("127.0.0.1:")).count(),
        10,
        "expected the full query port range: {broadcasts}"
    );

    // Rebuilding for the same instance replaces the shadow cleanly.
    let shadow2 = build_shadow(
        &game,
        "bin64/game_x64",
        "bin64/lib64/libsteam_api.so",
        322330,
        1,
        &fake_goldberg,
        &save_dir,
    )
    .expect("rebuild shadow");
    assert_eq!(shadow, shadow2);

    let _ = std::fs::remove_dir_all(&tmp);
}
