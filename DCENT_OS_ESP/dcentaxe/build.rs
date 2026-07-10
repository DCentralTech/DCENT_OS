// DCENT_axe build script
// Propagates ESP-IDF linker arguments from esp-idf-sys to this binary crate

fn main() {
    embuild::espidf::sysenv::output();

    if std::env::var_os("DCENT_ENFORCE_SIGNED_OTA").is_some()
        && std::env::var_os("DCENT_OTA_PUBLIC_KEY_HEX").is_none()
    {
        panic!("DCENT_ENFORCE_SIGNED_OTA is set but DCENT_OTA_PUBLIC_KEY_HEX is missing");
    }

    // ── Git hash + build epoch stamps ──
    // Surfaced via /api/system/info so operators know exactly what commit is
    // on a miner — essential for OTA audit trails and field debugging.
    let git_hash = std::process::Command::new("git")
        .args(["rev-parse", "--short=10", "HEAD"])
        .output()
        .ok()
        .and_then(|o| {
            if o.status.success() {
                Some(String::from_utf8_lossy(&o.stdout).trim().to_string())
            } else {
                None
            }
        })
        .unwrap_or_else(|| "unknown".to_string());
    println!("cargo:rustc-env=DCENTAXE_GIT_HASH={git_hash}");

    let git_dirty = std::process::Command::new("git")
        .args(["status", "--porcelain"])
        .output()
        .map(|o| !o.stdout.is_empty())
        .unwrap_or(false);
    println!(
        "cargo:rustc-env=DCENTAXE_GIT_DIRTY={}",
        if git_dirty { "1" } else { "0" }
    );

    let epoch = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    println!("cargo:rustc-env=DCENTAXE_BUILD_EPOCH={epoch}");

    // Re-run if git state changes.
    println!("cargo:rerun-if-changed=../../.git/HEAD");
    println!("cargo:rerun-if-changed=../../.git/index");

    let bitaxe_touch = std::env::var_os("CARGO_FEATURE_BITAXE_TOUCH").is_some();
    let bitaxe_gt_touch = std::env::var_os("CARGO_FEATURE_BITAXE_GT_TOUCH").is_some();
    let mut selected_targets: Vec<&'static str> = Vec::new();

    if std::env::var_os("CARGO_FEATURE_BITAXE_MAX").is_some() {
        selected_targets.push("bitaxe-max");
    }
    if std::env::var_os("CARGO_FEATURE_BITAXE_ULTRA").is_some() {
        selected_targets.push("bitaxe-ultra");
    }
    if std::env::var_os("CARGO_FEATURE_BITAXE_SUPRA").is_some() {
        selected_targets.push("bitaxe-supra");
    }
    if std::env::var_os("CARGO_FEATURE_BITAXE_GAMMA").is_some() && !bitaxe_touch {
        selected_targets.push("bitaxe-gamma");
    }
    if std::env::var_os("CARGO_FEATURE_BITAXE_GAMMA_DUO").is_some() {
        selected_targets.push("bitaxe-gamma-duo");
    }
    if std::env::var_os("CARGO_FEATURE_BITAXE_GT").is_some() && !bitaxe_gt_touch {
        selected_targets.push("bitaxe-gt");
    }
    if bitaxe_touch {
        selected_targets.push("bitaxe-touch");
    }
    if bitaxe_gt_touch {
        selected_targets.push("bitaxe-gt-touch");
    }
    if std::env::var_os("CARGO_FEATURE_BITAXE_HEX_ULTRA").is_some() {
        selected_targets.push("bitaxe-hex-ultra");
    }
    if std::env::var_os("CARGO_FEATURE_BITAXE_HEX_SUPRA").is_some() {
        selected_targets.push("bitaxe-hex-supra");
    }
    if std::env::var_os("CARGO_FEATURE_NERDNOS").is_some() {
        selected_targets.push("nerdnos");
    }
    if std::env::var_os("CARGO_FEATURE_NERDAXE").is_some() {
        selected_targets.push("nerdaxe");
    }
    if std::env::var_os("CARGO_FEATURE_NERDQAXE_PLUS").is_some() {
        selected_targets.push("nerdqaxe-plus");
    }
    if std::env::var_os("CARGO_FEATURE_NERDQAXE_PP").is_some() {
        selected_targets.push("nerdqaxe-pp");
    }
    if std::env::var_os("CARGO_FEATURE_DCENT_AXE_BM1397").is_some() {
        selected_targets.push("dcent-axe-bm1397");
    }
    if std::env::var_os("CARGO_FEATURE_DCENT_AXE_QUAD_BM1397").is_some() {
        selected_targets.push("dcent-axe-quad-bm1397");
    }
    if std::env::var_os("CARGO_FEATURE_DCENT_AXE_HEX_BM1397").is_some() {
        selected_targets.push("dcent-axe-hex-bm1397");
    }

    if selected_targets.len() != 1 {
        panic!(
            "exactly one DCENT_axe board feature must be enabled; got {}: {}",
            selected_targets.len(),
            selected_targets.join(", ")
        );
    }
    let board_target = selected_targets[0];

    println!("cargo:rustc-env=DCENTAXE_BOARD_TARGET={board_target}");
}
