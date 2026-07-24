use super::*;

pub(super) const DEFAULT_DONATION_POOL_URL: &str = "stratum+tcp://pool.d-central.tech:3333";
pub(super) const DEFAULT_DONATION_WORKER: &str = "DungeonMaster";
pub(super) const DEFAULT_DONATION_PASSWORD: &str = "x";
pub(super) const DEFAULT_DONATION_FALLBACK_POOL_URL: &str =
    "stratum+tcp://stratum.braiins.com:3333";
pub(super) const DEFAULT_DONATION_FALLBACK_WORKER: &str = "DungeonMaster";
pub(super) const DEFAULT_DONATION_CYCLE_S: i64 = 3600;
pub(super) const DEFAULT_DONATION_PERCENT: f64 = 2.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct HomeNightModeView {
    pub enabled: bool,
    pub start_hour: u8,
    pub end_hour: u8,
    pub max_fan_pwm: u8,
    pub power_reduction_pct: u8,
    pub schema_source: &'static str,
}

fn home_night_mode_from_value(nm: &toml::Table, schema_source: &'static str) -> HomeNightModeView {
    HomeNightModeView {
        enabled: nm.get("enabled").and_then(|v| v.as_bool()).unwrap_or(false),
        start_hour: nm
            .get("start_hour")
            .and_then(|v| v.as_integer())
            .unwrap_or(22) as u8,
        end_hour: nm.get("end_hour").and_then(|v| v.as_integer()).unwrap_or(7) as u8,
        max_fan_pwm: nm
            .get("max_fan_pwm")
            .and_then(|v| v.as_integer())
            .unwrap_or(30)
            .min(dcentrald_hal::fan::PWM_SAFETY_MAX as i64) as u8,
        power_reduction_pct: nm
            .get("power_reduction_pct")
            .and_then(|v| v.as_integer())
            .unwrap_or(40) as u8,
        schema_source,
    }
}

pub(crate) fn read_home_night_mode_from_table(table: &toml::Table) -> HomeNightModeView {
    if let Some(nm) = table
        .get("mode")
        .and_then(|v| v.as_table())
        .and_then(|mode| mode.get("home"))
        .and_then(|v| v.as_table())
        .and_then(|home| home.get("night_mode"))
        .and_then(|v| v.as_table())
    {
        return home_night_mode_from_value(nm, "mode.home.night_mode");
    }
    if let Some(nm) = table
        .get("home")
        .and_then(|v| v.as_table())
        .and_then(|home| home.get("night_mode"))
        .and_then(|v| v.as_table())
    {
        return home_night_mode_from_value(nm, "legacy.home.night_mode");
    }
    HomeNightModeView {
        enabled: false,
        start_hour: 22,
        end_hour: 7,
        max_fan_pwm: 30,
        power_reduction_pct: 40,
        schema_source: "defaults",
    }
}

pub(super) fn donation_config_value_from_table(table: &toml::Table) -> serde_json::Value {
    let donation = table.get("donation").and_then(|value| value.as_table());
    serde_json::json!({
        "enabled": donation
            .and_then(|section| section.get("enabled"))
            .and_then(|value| value.as_bool())
            .unwrap_or(true),
        "percent": donation
            .and_then(|section| section.get("percent"))
            .and_then(toml_value_as_f64)
            .unwrap_or(DEFAULT_DONATION_PERCENT),
        "pool_url": donation
            .and_then(|section| section.get("pool_url"))
            .and_then(|value| value.as_str())
            .unwrap_or(DEFAULT_DONATION_POOL_URL),
        "worker": donation
            .and_then(|section| section.get("worker"))
            .and_then(|value| value.as_str())
            .unwrap_or(DEFAULT_DONATION_WORKER),
        "password": donation
            .and_then(|section| section.get("password"))
            .and_then(|value| value.as_str())
            .unwrap_or(DEFAULT_DONATION_PASSWORD),
        "fallback_enabled": donation
            .and_then(|section| section.get("fallback_enabled"))
            .and_then(|value| value.as_bool())
            .unwrap_or(true),
        "fallback_pool_url": donation
            .and_then(|section| section.get("fallback_pool_url"))
            .and_then(|value| value.as_str())
            .unwrap_or(DEFAULT_DONATION_FALLBACK_POOL_URL),
        "fallback_worker": donation
            .and_then(|section| section.get("fallback_worker"))
            .and_then(|value| value.as_str())
            .unwrap_or(DEFAULT_DONATION_FALLBACK_WORKER),
        "fallback_password": donation
            .and_then(|section| section.get("fallback_password"))
            .and_then(|value| value.as_str())
            .unwrap_or(DEFAULT_DONATION_PASSWORD),
        "cycle_duration_s": donation
            .and_then(|section| section.get("cycle_duration_s"))
            .and_then(toml_value_as_i64)
            .unwrap_or(DEFAULT_DONATION_CYCLE_S),
    })
}

pub(super) fn merge_config_update_table(
    mut table: toml::Table,
    body: &serde_json::Value,
) -> std::result::Result<toml::Table, String> {
    let update_value = json_to_toml(body)?;
    let update_table = match update_value {
        toml::Value::Table(table) => table,
        _ => return Err("Config update body must be a JSON object".to_string()),
    };

    let disallowed: Vec<String> = update_table
        .keys()
        .filter(|key| !CONFIG_ALLOWED_KEYS.contains(&key.as_str()))
        .cloned()
        .collect();
    if !disallowed.is_empty() {
        return Err(format!(
            "Disallowed config keys: {}. Allowed: {}",
            disallowed.join(", "),
            CONFIG_ALLOWED_KEYS.join(", "),
        ));
    }

    for (key, value) in update_table {
        if let Some(existing) = table.get_mut(&key) {
            merge_toml_value(existing, value);
        } else {
            table.insert(key, value);
        }
    }

    validate_mining_write(&table)?;
    validate_imported_config_table(&table)?;
    Ok(table)
}

pub(super) fn apply_config_update_body(
    body: &serde_json::Value,
) -> std::result::Result<serde_json::Value, ConfigPersistenceError> {
    let _cfg_write_guard = crate::atomic_io::config_write_lock();
    let table = load_config_table_for_write().map_err(ConfigPersistenceError::bad_request)?;
    let table =
        merge_config_update_table(table, body).map_err(ConfigPersistenceError::bad_request)?;
    let config_path = get_writable_config_path();

    if let Some(parent) = std::path::Path::new(config_path).parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| ConfigPersistenceError::from_io("Failed to create config directory", e))?;
    }

    let merged_toml = toml::to_string_pretty(&table).map_err(|e| {
        ConfigPersistenceError::bad_request(format!("Failed to serialize config: {}", e))
    })?;
    atomic_write(config_path, &merged_toml)
        .map_err(|e| ConfigPersistenceError::from_io("Failed to write config", e))?;

    let merged_json = serde_json::to_string(&table).map_err(|e| {
        ConfigPersistenceError::bad_request(format!("Failed to convert merged config: {}", e))
    })?;
    serde_json::from_str(&merged_json).map_err(|e| {
        ConfigPersistenceError::bad_request(format!("Failed to parse merged config: {}", e))
    })
}

pub(super) async fn get_config_donation() -> impl IntoResponse {
    let table = load_config_table_for_write().unwrap_or_else(|_| toml::Table::new());
    Json(serde_json::json!({
        "status": "ok",
        "config": donation_config_value_from_table(&table),
        "restart_required": false,
    }))
}

pub(super) async fn post_config_donation(
    State(state): State<Arc<AppState>>,
    Json(body): Json<serde_json::Value>,
) -> Response {
    if let Err(response) = require_antminer_runtime_capability(
        &state,
        RuntimeCapability::ConfigRw,
        "/api/config/donation",
    ) {
        return response;
    }

    if !body.is_object() {
        return api_error(
            StatusCode::BAD_REQUEST,
            dcentrald_api_types::api_error_codes::CONFIG_VALIDATION,
            "Donation config update body must be a JSON object",
            Some("Submit a JSON object containing only donation config fields."),
        );
    }

    let wrapped = serde_json::json!({ "donation": body });
    match apply_config_update_body(&wrapped) {
        Ok(merged) => (
            StatusCode::OK,
            Json(serde_json::json!({
                "status": "ok",
                "message": "Donation configuration updated. Restart required to apply.",
                "config_path": get_writable_config_path(),
                "config": merged
                    .get("donation")
                    .cloned()
                    .unwrap_or_else(|| donation_config_value_from_table(&toml::Table::new())),
                "restart_required": true,
            })),
        )
            .into_response(),
        Err(error) => error.into_response(),
    }
}

pub(crate) async fn post_config(
    State(state): State<Arc<AppState>>,
    Json(body): Json<serde_json::Value>,
) -> Response {
    if let Err(response) =
        require_antminer_runtime_capability(&state, RuntimeCapability::ConfigRw, "/api/config")
    {
        return response;
    }

    // SEC (mirrors post_pools' W1.4): never log the full config body at INFO —
    // it can carry a pool `password` and a `stratum+tcp://user:pass@host` URL,
    // which would land as a plaintext credential on disk (/tmp/dcentrald.log +
    // the persistent ring) and in support bundles. Log only the top-level key
    // names being set (never their values).
    let config_keys: Vec<&str> = body
        .as_object()
        .map(|m| m.keys().map(|k| k.as_str()).collect())
        .unwrap_or_default();
    tracing::info!(keys = ?config_keys, "Config update request received");

    //  W1a — capture current runtime mode before the write so we
    // can audit operator-issued mode-change requests. The watch channel
    // reflects the *running* daemon's mode, which is what operators see
    // in the dashboard. The TOML write completes immediately but the
    // running daemon's `mode_tx` channel will not flip until restart, so
    // this audit event records the *request*, not realized state.
    let old_mode_str = state.mode_rx.borrow().to_string();

    let config_path = get_writable_config_path();

    let write_result = apply_config_update_body(&body);
    match write_result {
        Ok(merged) => {
            tracing::info!(config_path, "Config updated — restart required to apply");

            //  W1a — audit a mode-change request when the body
            // sets `mode.active` to something different from the
            // running daemon's mode. Body shape is
            // `{"mode":{"active":"home"}}`. Skip on no-op writes so
            // worker-name / unrelated-section edits don't spam the ring.
            if let Some(new_mode_str) = body
                .get("mode")
                .and_then(|m| m.get("active"))
                .and_then(|s| s.as_str())
            {
                if new_mode_str != old_mode_str {
                    crate::push_audit_event(
                        &state,
                        "rest_dashboard",
                        dcentrald_api_types::audit_log::AuditEvent::ModeChange {
                            from: old_mode_str.clone(),
                            to: new_mode_str.to_string(),
                        },
                    );
                }
            }

            (
                StatusCode::OK,
                Json(serde_json::json!({
                    "status": "ok",
                    "message": "Configuration updated. Restart required to apply.",
                    "config_path": config_path,
                    "config": merged,
                })),
            )
                .into_response()
        }
        Err(error) => {
            tracing::error!(error = ?error, "Failed to save config update");
            error.into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[tokio::test]
    async fn config_persistence_storage_full_response_has_typed_code() {
        let response = ConfigPersistenceError::from_io(
            "Failed to write config",
            io::Error::new(io::ErrorKind::StorageFull, "full"),
        )
        .into_response();

        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let bytes = to_bytes(response.into_body(), ERROR_ENVELOPE_BODY_LIMIT)
            .await
            .unwrap();
        let body: dcentrald_api_types::ApiErrorBody = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body.code.as_deref(), Some("storage_full"));
        assert!(body.error.contains("Persistent storage is full"));
        assert!(body.detail.as_deref().unwrap_or_default().contains("full"));
    }

    #[tokio::test]
    async fn config_persistence_read_only_response_has_typed_code() {
        let response = ConfigPersistenceError::from_io(
            "Failed to write config",
            io::Error::new(io::ErrorKind::PermissionDenied, "read-only"),
        )
        .into_response();

        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let bytes = to_bytes(response.into_body(), ERROR_ENVELOPE_BODY_LIMIT)
            .await
            .unwrap();
        let body: dcentrald_api_types::ApiErrorBody = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body.code.as_deref(), Some("storage_read_only"));
        assert!(body.error.contains("Persistent storage is read-only"));
        assert!(body
            .suggestion
            .as_deref()
            .unwrap_or_default()
            .contains("/data"));
    }

    /// P0-4 (C-5/C-6) regression: the daily-sats estimate must be anchored to
    /// network difficulty + block subsidy, NOT inflated by treating every
    /// accepted share as a block reward. A ~1.9 TH/s S9 must read ~100 sats/day,
    /// not the tens-of-millions (~$228/day) the old path produced.
    #[test]
    fn test_sats_today_network_anchored_not_inflated() {
        // Representative live network difficulty (~110 T) + post-2024 subsidy.
        let network_difficulty = 110_000_000_000_000.0_f64;
        let block_subsidy_sats = 3.125_f64 * 100_000_000.0;

        let sats =
            estimate_daily_sats_network_anchored(1.9, network_difficulty, block_subsidy_sats)
                .expect("calibrated estimate when difficulty is positive");

        // Hand-computed canonical value:
        //   1.9e12 * 86_400 / (110e12 * 2^32) * 3.125e8 ≈ 108.6 sats/day
        assert!(
            (90u64..=130).contains(&sats),
            "expected ~108 sats/day for a 1.9 TH/s S9, got {sats}"
        );

        // The old inflated reading mapped to ~$228/day ≈ 22.8 MILLION sats at
        // ~$100k/BTC. The corrected value must be orders of magnitude smaller.
        assert!(
            sats < 22_800,
            "sats_today must not reach block-reward magnitude, got {sats}"
        );

        // Truth-contract: absent / non-positive inputs must yield None so the
        // caller emits an "uncalibrated estimate" instead of a fabricated value.
        assert_eq!(
            estimate_daily_sats_network_anchored(1.9, 0.0, block_subsidy_sats),
            None,
            "absent network difficulty must yield None, not a fabricated value"
        );
        assert_eq!(
            estimate_daily_sats_network_anchored(0.0, network_difficulty, block_subsidy_sats),
            None,
        );
        assert_eq!(
            estimate_daily_sats_network_anchored(1.9, network_difficulty, 0.0),
            None,
        );
    }

    /// P2-4 (§4.E) regression: the daemon `[home]` config is the SINGLE SOURCE
    /// OF TRUTH for the electricity rate + currency. The default MUST be the
    /// daemon's `0.12` (NOT the old client-localStorage `0.10` guess), and the
    /// calibration flag MUST start `false` so cost/earnings surfaces label
    /// themselves "uncalibrated" until the operator confirms a rate.
    #[test]
    fn test_home_economics_is_single_source_of_truth() {
        let close = |a: f64, b: f64| (a - b).abs() < 1e-9;

        // The pinned default matches the daemon config default, not 0.10.
        assert!(close(DEFAULT_ELECTRICITY_RATE_USD_PER_KWH, 0.12));
        assert!(!close(DEFAULT_ELECTRICITY_RATE_USD_PER_KWH, 0.10));

        // An empty config ⇒ daemon default rate + currency, uncalibrated.
        let empty = home_economics_from_table(&toml::Table::new());
        assert!(close(empty.rate_usd_per_kwh, 0.12));
        assert_eq!(empty.currency, "USD");
        assert!(
            !empty.rate_calibrated,
            "an unset rate must report uncalibrated, never a confident default"
        );

        // An operator-confirmed rate round-trips verbatim and flips calibrated.
        let confirmed: toml::Table = toml::from_str(
            "[home]\nelectricity_rate = 0.155\ncurrency = \"CAD\"\nelectricity_rate_calibrated = true\n",
        )
        .expect("valid toml");
        let eco = home_economics_from_table(&confirmed);
        assert!(close(eco.rate_usd_per_kwh, 0.155));
        assert_eq!(eco.currency, "CAD");
        assert!(eco.rate_calibrated);

        // An integer-valued rate is accepted (toml integers are not floats).
        let int_rate: toml::Table =
            toml::from_str("[home]\nelectricity_rate = 1\n").expect("valid toml");
        assert!(close(
            home_economics_from_table(&int_rate).rate_usd_per_kwh,
            1.0
        ));

        // A corrupt (negative) rate falls back to the default rather than
        // poisoning every downstream cost estimate.
        let corrupt: toml::Table =
            toml::from_str("[home]\nelectricity_rate = -3.0\n").expect("valid toml");
        assert!(close(
            home_economics_from_table(&corrupt).rate_usd_per_kwh,
            DEFAULT_ELECTRICITY_RATE_USD_PER_KWH,
        ));

        // A blank currency falls back to the default; calibration defaults off.
        let blank_currency: toml::Table =
            toml::from_str("[home]\ncurrency = \"  \"\n").expect("valid toml");
        let bc = home_economics_from_table(&blank_currency);
        assert_eq!(bc.currency, "USD");
        assert!(!bc.rate_calibrated);
    }

    /// Off-grid commissioning must never persist a simulated ADC as if it were
    /// a field protection sensor. The lab path remains `/api/offgrid/test`;
    /// saved config must point at measured INA226/Sysfs telemetry.
    #[test]
    fn test_offgrid_persistence_rejects_simulated_adc() {
        let mut payload = OffGridConfigPayload {
            adc: Some(dcentrald_hal::adc::AdcBackendConfig::Simulated {
                voltage_v: 52.0,
                current_a: 0.0,
            }),
            ..OffGridConfigPayload::default()
        };

        let err = validate_offgrid_payload(&payload)
            .expect_err("simulated ADC must not be persisted, even while disabled");
        assert!(
            err.contains("Simulated ADC is lab-only"),
            "unexpected validation error: {err}"
        );

        payload.enabled = true;
        let err = validate_offgrid_payload(&payload)
            .expect_err("simulated ADC must not arm off-grid protection");
        assert!(
            err.contains("cannot be saved as off-grid protection config"),
            "unexpected validation error: {err}"
        );

        let response = offgrid_config_response(payload);
        assert!(
            !response.ready,
            "simulated ADC must never report ready for live protection"
        );
        assert!(response.readiness_message.contains("cannot arm"));
    }

    #[test]
    fn test_offgrid_ready_requires_measured_adc_backend() {
        let payload = OffGridConfigPayload {
            enabled: true,
            adc: Some(dcentrald_hal::adc::AdcBackendConfig::Ina226 {
                i2c_bus: 0,
                i2c_addr: 0x40,
                shunt_mohm: 10,
                voltage_divider: 1.0,
            }),
            ..OffGridConfigPayload::default()
        };

        validate_offgrid_payload(&payload).expect("measured ADC backend should validate");
        let response = offgrid_config_response(payload);
        assert!(response.ready);
    }

    /// CFG-1/CFG-4 regression: the `[home]` power-target write must preserve
    /// EVERY other config section. The pre-fix handler did a bespoke
    /// read-modify-write against a `/data`-only path; on a fresh beta install
    /// (no `/data` file) it wrote a `[home]`-only file that shadowed the baked
    /// `[pool]`/`[power]`/`[thermal]`/`[auth]` sections on the next reboot —
    /// silent total config loss. The fix routes through the full effective table;
    /// this test pins the merge helper so a regression to a `[home]`-only write
    /// would FAIL here.
    #[test]
    fn test_apply_home_power_target_preserves_other_sections() {
        // Simulate the full effective config produced by load_config_table_for_write
        // (i.e. the baked /etc config merged in). It carries pool/thermal/auth.
        let mut table: toml::Table = toml::from_str(
            r#"
[pool]
url = "stratum+tcp://public-pool.io:21496"
worker = "bc1qexampleworker"

[thermal]
fan_max_pwm = 30
target_temp_c = 55

[auth]
require_token = true

[home]
target_watts = 800
"#,
        )
        .expect("valid toml");

        apply_home_power_target_to_table(&mut table, 1200, Some("high"));

        // The home keys updated.
        let home = table.get("home").and_then(|v| v.as_table()).unwrap();
        assert_eq!(home.get("target_watts").unwrap().as_integer(), Some(1200));
        assert_eq!(home.get("preset").unwrap().as_str(), Some("high"));

        // CRITICAL: every other section survives untouched (no config loss).
        let pool = table.get("pool").and_then(|v| v.as_table()).unwrap();
        assert_eq!(
            pool.get("url").unwrap().as_str(),
            Some("stratum+tcp://public-pool.io:21496")
        );
        assert_eq!(
            pool.get("worker").unwrap().as_str(),
            Some("bc1qexampleworker")
        );

        let thermal = table.get("thermal").and_then(|v| v.as_table()).unwrap();
        assert_eq!(thermal.get("fan_max_pwm").unwrap().as_integer(), Some(30));
        assert_eq!(thermal.get("target_temp_c").unwrap().as_integer(), Some(55));

        let auth = table.get("auth").and_then(|v| v.as_table()).unwrap();
        assert_eq!(auth.get("require_token").unwrap().as_bool(), Some(true));
    }

    /// CFG-1/CFG-4 regression for the night-mode handler — same config-loss
    /// guard as the power-target test. Pins that a `[home.night_mode]` write
    /// preserves `[pool]`/`[thermal]`/`[auth]` and mutates only the night-mode
    /// subsection.
    #[test]
    fn test_apply_home_night_mode_preserves_other_sections() {
        let mut table: toml::Table = toml::from_str(
            r#"
[pool]
url = "stratum+tcp://public-pool.io:21496"

[thermal]
fan_max_pwm = 30

[auth]
require_token = true
"#,
        )
        .expect("valid toml");

        // Note: no [home] section present yet — the helper must create it without
        // dropping the baked sections (the exact fresh-install danger case).
        apply_home_night_mode_to_table(&mut table, true, 22, 7, 30, 40);

        let nm = table
            .get("mode")
            .and_then(|v| v.as_table())
            .and_then(|m| m.get("home"))
            .and_then(|v| v.as_table())
            .and_then(|h| h.get("night_mode"))
            .and_then(|v| v.as_table())
            .expect("night_mode subsection created");
        assert_eq!(nm.get("enabled").unwrap().as_bool(), Some(true));
        assert_eq!(nm.get("start_hour").unwrap().as_integer(), Some(22));
        assert_eq!(nm.get("end_hour").unwrap().as_integer(), Some(7));
        assert_eq!(nm.get("max_fan_pwm").unwrap().as_integer(), Some(30));
        assert_eq!(
            nm.get("power_reduction_pct").unwrap().as_integer(),
            Some(40)
        );
        assert!(
            table.get("home").is_none(),
            "night-mode writes must not create strict-reload-breaking legacy [home]"
        );
        let view = read_home_night_mode_from_table(&table);
        assert_eq!(view.schema_source, "mode.home.night_mode");
        assert!(view.enabled);

        // CRITICAL: the baked sections survive (no config loss).
        assert_eq!(
            table
                .get("pool")
                .and_then(|v| v.as_table())
                .and_then(|p| p.get("url"))
                .and_then(|v| v.as_str()),
            Some("stratum+tcp://public-pool.io:21496")
        );
        assert_eq!(
            table
                .get("thermal")
                .and_then(|v| v.as_table())
                .and_then(|t| t.get("fan_max_pwm"))
                .and_then(|v| v.as_integer()),
            Some(30)
        );
        assert_eq!(
            table
                .get("auth")
                .and_then(|v| v.as_table())
                .and_then(|a| a.get("require_token"))
                .and_then(|v| v.as_bool()),
            Some(true)
        );
    }

    #[test]
    fn test_read_home_night_mode_prefers_mode_home_over_legacy() {
        let table: toml::Table = toml::from_str(
            r#"
[home.night_mode]
enabled = true
start_hour = 20
end_hour = 6

[mode.home.night_mode]
enabled = false
start_hour = 22
end_hour = 7
max_fan_pwm = 24
power_reduction_pct = 35
"#,
        )
        .expect("valid toml");

        let view = read_home_night_mode_from_table(&table);
        assert_eq!(view.schema_source, "mode.home.night_mode");
        assert!(!view.enabled);
        assert_eq!(view.start_hour, 22);
        assert_eq!(view.end_hour, 7);
        assert_eq!(view.max_fan_pwm, 24);
        assert_eq!(view.power_reduction_pct, 35);
    }

    #[test]
    fn test_read_home_night_mode_legacy_fallback_is_labeled() {
        let table: toml::Table = toml::from_str(
            r#"
[home.night_mode]
enabled = true
start_hour = 21
end_hour = 5
"#,
        )
        .expect("valid toml");

        let view = read_home_night_mode_from_table(&table);
        assert_eq!(view.schema_source, "legacy.home.night_mode");
        assert!(view.enabled);
        assert_eq!(view.start_hour, 21);
        assert_eq!(view.end_hour, 5);
    }

    #[test]
    fn test_thermal_supervisor_snapshot_response_includes_runtime_truth_fields() {
        let snap = dcentrald_thermal::supervisor::SupervisorSnapshot {
            enabled: true,
            uptime_secs: 12,
            secs_since_last_step: 3,
            board_states: Vec::new(),
            fan_max_pwm: 30,
            chip_imbalance_threshold_c: 8.0,
            worst_chip_imbalance_c: None,
            hydro_configured: false,
        };
        let status = crate::ThermalSupervisorRuntimeStatus {
            configured_enabled: true,
            runtime_present: true,
            snapshot_available: true,
            commissioning_state: "running",
        };

        let body = thermal_supervisor_snapshot_response_value(snap, status);

        assert_eq!(body["enabled"].as_bool(), Some(true));
        assert_eq!(body["configured_enabled"].as_bool(), Some(true));
        assert_eq!(body["runtime_present"].as_bool(), Some(true));
        assert_eq!(body["snapshot_available"].as_bool(), Some(true));
        assert_eq!(body["commissioning_state"].as_str(), Some("running"));
        assert_eq!(body["fan_max_pwm"].as_u64(), Some(30));
    }

    const SIGNED_SYSUPGRADE_ALIAS_ROUTES: &[&str] =
        &["/api/v1/system/upgrade", "/api/v1/firmware/update"];
    const API_COMPATIBILITY_ROUTE_PARITY_OMISSIONS: &[&str] = &[
        "/api/action/reboot",
        "/api/action/restart",
        "/api/action/sleep",
        "/api/action/wake",
        "/api/auth/session",
        "/api/auth/session/current",
        "/api/auth/setup",
        "/api/auth/status",
        "/api/auth/ws-ticket",
        "/api/autotuner/chip-health",
        "/api/autotuner/decrement_hashrate_target",
        "/api/autotuner/decrement_power_target",
        "/api/autotuner/efficiency",
        "/api/autotuner/fleet-profile/export",
        "/api/autotuner/increment_hashrate_target",
        "/api/autotuner/increment_power_target",
        "/api/autotuner/noise-profile",
        "/api/autotuner/profitability",
        "/api/autotuner/room-temp-factor",
        "/api/autotuner/saved-status",
        "/api/autotuner/set_default_hashrate_target",
        "/api/autotuner/silicon-report",
        "/api/autotuner/state",
        "/api/autotuner/status",
        "/api/autotuner/target",
        "/api/autotuner/telemetry",
        "/api/autotuner/telemetry/csv",
        "/api/autotuner/tuned_profiles",
        "/api/autotuner/visibility",
        "/api/config",
        "/api/config/backup/manifest",
        "/api/config/export",
        "/api/config/import",
        "/api/config/mqtt",
        "/api/config/mqtt/test",
        // Read-only MQTT/HA status (P2-10) — observability route, intentionally
        // omitted from the formal API-compat manifest.
        "/api/mqtt/status",
        "/api/config/power-calibration",
        "/api/config/psu-override",
        "/api/config/shared",
        "/api/config/webhook",
        "/api/config/webhook/test",
        "/api/debug/asic-command",
        "/api/debug/chip/frequency",
        "/api/debug/chip/voltage",
        "/api/debug/i2c",
        "/api/debug/log",
        "/api/debug/pid-params",
        "/api/debug/pid-state",
        "/api/debug/psu/control",
        "/api/debug/registers",
        "/api/diagnostics/board-health/report",
        "/api/diagnostics/board-health/result",
        "/api/diagnostics/board-health/start",
        "/api/diagnostics/board-health/status",
        "/api/diagnostics/chip-health/report",
        "/api/diagnostics/chip-health/result",
        "/api/diagnostics/chip-health/start",
        "/api/diagnostics/chip-health/status",
        "/api/diagnostics/hashreport/cancel",
        "/api/diagnostics/hashreport/report",
        "/api/diagnostics/hashreport/result",
        "/api/diagnostics/hashreport/start",
        "/api/diagnostics/hashreport/status",
        "/api/diagnostics/reports/recent",
        "/api/diagnostics/troubleshoot/asic-comm",
        "/api/diagnostics/troubleshoot/fpga",
        "/api/diagnostics/troubleshoot/i2c-scan",
        "/api/diagnostics/troubleshoot/network",
        "/api/diagnostics/troubleshoot/psu",
        "/api/fan",
        "/api/fleet/discover",
        "/api/fleet/miners",
        "/api/history",
        "/api/home/history",
        "/api/home/night-mode",
        "/api/home/presets",
        "/api/home/room-temp",
        "/api/home/status",
        "/api/home/target",
        // P3-1: the index route is the catalog itself (derived FROM the
        // manifest); it is intentionally not a manifest surface entry.
        "/api/index",
        "/api/jd/config",
        "/api/jd/status",
        "/api/jd/test-connection",
        "/api/led/config",
        "/api/led/locate",
        "/api/led/locate/stop",
        "/api/led/pattern",
        "/api/led/patterns",
        "/api/led/status",
        "/api/offgrid/config",
        "/api/offgrid/presets",
        "/api/offgrid/status",
        "/api/offgrid/test",
        "/api/pool/sv2/handshake",
        "/api/pool/sv2/messages",
        "/api/pool/sv2/status",
        "/api/pools",
        "/api/pools/test",
        "/api/profiles",
        "/api/profiles/silicon-table",
        "/api/profiles/silicon",
        "/api/profiles/silicon/:id",
        "/api/profiles/silicon/active",
        "/api/profiles/silicon/import",
        "/api/profiles/silicon/import-json",
        "/api/profiles/silicon/reload",
        "/api/re/catalog/apw-psu",
        "/api/re/catalog/asic-commands",
        "/api/re/catalog/asic-registers",
        "/api/re/catalog/asic-registers/bm1387",
        "/api/re/catalog/asic-registers/bm1397",
        "/api/re/catalog/bb-uart-trans",
        "/api/re/catalog/bm1362-baud-init",
        "/api/re/catalog/boot-flow",
        "/api/re/catalog/boot-orchestration",
        "/api/re/catalog/diode-voltage",
        "/api/re/catalog/dspic-frames",
        "/api/re/catalog/eeprom/bhb-skus",
        "/api/re/catalog/firmware-stratum-matrix",
        "/api/re/catalog/fpga-registers",
        "/api/re/catalog/index",
        "/api/re/catalog/luxos-network-exposure",
        "/api/re/catalog/luxos-rest-commands",
        "/api/re/catalog/s21-fixture-production-warnings",
        "/api/re/catalog/thermal-model",
        "/api/re/catalog/vnish-rest-endpoints",
        "/api/safety/acknowledge",
        "/api/safety/warnings",
        "/api/setup/complete",
        "/api/setup/quiet-hours",
        "/api/setup/skip-password",
        "/api/setup/skip-safety",
        "/api/setup/status",
        "/api/setup/step-economics",
        "/api/setup/step1-safety",
        "/api/setup/step2-circuit",
        "/api/setup/step3-password",
        "/api/setup/step4-mode",
        "/api/setup/step5-pool",
        "/api/setup/test-pool",
        "/api/solar/config",
        "/api/solar/status",
        "/api/solar/test",
        "/api/solar/verification-history",
        "/api/swarm",
        "/api/swarm/room-temp",
        "/api/system/health",
        "/api/system/restart",
        "/api/system/restore-to-stock",
        "/api/system/restore-to-stock/preflight",
        "/api/system/restore-to-stock/preflight-checks",
        "/api/system/restore-to-stock/status",
        "/api/system/stats",
        "/api/system/update/metadata",
        "/api/thermal/posture",
        "/api/tou/schedule",
        "/mcp",
        "/metrics",
    ];

    // ─── SW-02 gRPC WRITE-control bridge tests ──────────────────────────
    //
    // These pin the SHARED validation + safety-cap logic the gRPC write
    // bridges (`grpc_bridge_set_pools` / `grpc_bridge_set_fan` /
    // `grpc_bridge_reboot`) reuse from the REST handlers. They exercise the
    // pure (no-HAL, no-filesystem) pieces — the load-bearing PWM-30 fan clamp
    // and the pool-validation pipeline — so the bridges can never diverge from
    // the REST handlers' caps. The physical fan command is deliberately
    // unavailable until the mining runtime owns a serialized broker; the
    // `/data/dcentrald.toml` atomic write remains path-bound and is covered
    // separately.)

    /// A process exit cannot authorize a new hardware session. Pin the
    /// non-destructive refusal until the restart path has a durable, typed
    /// disposition receipt for every owned rail.
    #[test]
    fn api_restart_is_refused_without_typed_hardware_disposition() {
        assert!(
            DAEMON_RESTART_REFUSAL.contains("typed hardware disposition"),
            "refusal must identify the missing proof: {DAEMON_RESTART_REFUSAL}"
        );
        assert!(
            DAEMON_RESTART_REFUSAL.contains("operator verification"),
            "refusal must name the guarded resolution path: {DAEMON_RESTART_REFUSAL}"
        );
    }

    /// Task 4 — disk telemetry alert threshold: a mount at/above 90% usage is
    /// flagged `alert: true`; below 90% is not. Pin the threshold and the
    /// used/available math (used = total - free; available = avail blocks).
    #[test]
    fn disk_mount_alert_fires_at_or_above_90_percent() {
        const BS: u64 = 4096;
        const TOTAL: u64 = 1_000_000; // blocks
        const RW: bool = false; // writable mount
        const RO: bool = true; // read-only mount

        // 100% full WRITABLE (the live .100 rootfs case): free=0, avail=0 → alert.
        let full = build_disk_mount("/", BS, TOTAL, 0, 0, RW);
        assert_eq!(full.used_bytes, BS * TOTAL);
        assert_eq!(full.available_bytes, 0);
        assert_eq!(full.used_percent, Some(100.0));
        assert!(!full.read_only);
        assert!(full.alert, "a 100%-full writable mount must alert");

        // 100% full READ-ONLY (a normal packed squashfs rootfs) → NO alert.
        // Fullness is by design, not disk pressure — alerting would be a
        // permanent false positive.
        let full_ro = build_disk_mount("/", BS, TOTAL, 0, 0, RO);
        assert_eq!(full_ro.used_percent, Some(100.0));
        assert!(full_ro.read_only);
        assert!(
            !full_ro.alert,
            "a full READ-ONLY squashfs mount must NOT alert (by-design fullness)"
        );

        // Exactly 90% used, writable → alert (boundary is inclusive).
        let at_90 = build_disk_mount("/data", BS, TOTAL, TOTAL / 10, TOTAL / 10, RW);
        assert_eq!(at_90.used_percent, Some(90.0));
        assert!(
            at_90.alert,
            "exactly 90% writable must alert (>= threshold)"
        );

        // 50% used, writable → no alert.
        let half = build_disk_mount("/tmp", BS, TOTAL, TOTAL / 2, TOTAL / 2, RW);
        assert_eq!(half.used_percent, Some(50.0));
        assert!(!half.alert, "50% must NOT alert");
        assert_eq!(half.used_bytes, BS * (TOTAL / 2));
        assert_eq!(half.available_bytes, BS * (TOTAL / 2));

        // Just below threshold (89.9%), writable → no alert.
        let below = build_disk_mount("/overlay", BS, 1000, 101, 101, RW);
        assert_eq!(below.used_percent, Some(89.9));
        assert!(!below.alert, "89.9% must NOT alert");

        // Zero-total pseudo fs → no percent, no alert (and read_disk_mounts
        // skips it entirely).
        let pseudo = build_disk_mount("/proc", BS, 0, 0, 0, RW);
        assert_eq!(pseudo.used_percent, None);
        assert!(!pseudo.alert);

        // The threshold constant is the documented 90%.
        assert_eq!(DISK_USAGE_ALERT_PERCENT, 90.0);
    }

    /// Task 5 — OTA honesty: the REST update-metadata builder derives its
    /// signature posture from `ota_signature::ota_signature_state()`. With no
    /// trust anchor present (the host test build pins no key and has no
    /// `/etc/dcentos/release_ed25519.pub`), that state is `InertNoKey` →
    /// `signature_required`/`signature_capable` resolve to FALSE and the honest
    /// key id is None. (The full `update_metadata_payload` requires a live
    /// `AppState` + HAL and is exercised on-target; here we pin the exact
    /// derivation the builder consumes so it can never silently re-claim a
    /// `signatureRequired: true` gate.)
    #[test]
    fn update_metadata_ota_fields_derive_from_honest_state_without_a_key() {
        assert!(
            crate::ota_signature::compiled_public_key_hex().is_none(),
            "test build must not pin an OTA key"
        );
        let state = crate::ota_signature::ota_signature_state();
        assert_eq!(
            state,
            crate::ota_signature::OtaSignatureState::InertNoKey,
            "no key anywhere on the host build must yield the inert state"
        );
        // These are exactly the values update_metadata_payload assigns to
        // signature_capable / signature_required / key_id.
        assert!(
            !state.is_enforced(),
            "signature_required/signature_capable must be false when inert"
        );
        assert!(
            crate::ota_signature::honest_key_id().is_none(),
            "key_id must be None when no OTA key is pinned"
        );
    }

    // SEC-1 (2026-06-20): /api/system/update/metadata is intentionally
    // unauthenticated (monitoring parity with /api/system/info). Pin that its
    // payload carries NO secret/credential/wallet — only non-sensitive
    // version/model/board/identity data. If a future field leaks a secret, this
    // test fails and the endpoint must be moved behind auth.
    #[test]
    fn update_metadata_payload_carries_no_secrets_for_unauth_surface() {
        // InstallIntent / ToolboxPackageInfo / UpdateMetadata / UPDATE_SCHEMA_VERSION
        // are already in scope via `use super::*` (re-exported from dcent_schema::update).

        // Worst case: every optional identity field populated.
        let metadata = UpdateMetadata {
            schema: UPDATE_SCHEMA_VERSION,
            product: "DCENT_OS".to_string(),
            family: "antminer".to_string(),
            device_model: "Antminer S9".to_string(),
            board_target: "am1-s9".to_string(),
            current_version: "0.5.0".to_string(),
            package_type: "sysupgrade".to_string(),
            upload_endpoint: Some("/api/system/upgrade".to_string()),
            board_target_header: None,
            device_model_header: None,
            inactive_slot_supported: true,
            signature_capable: false,
            signature_required: false,
            allow_unsigned: false,
            // key_id is an OTA *public* key id, never a secret.
            key_id: Some("dcent-2026-q1".to_string()),
            install_intent: Some(InstallIntent {
                schema_version: UPDATE_SCHEMA_VERSION,
                installer: "dcent-toolbox".to_string(),
                install_origin: "operator".to_string(),
                bootstrap_transport: "ssh".to_string(),
                install_method: "sysupgrade".to_string(),
                hardening_profile: "standard".to_string(),
                target_ip: Some("203.0.113.39".to_string()),
                model: Some("S9".to_string()),
                hostname: Some("miner-39".to_string()),
                mac: Some("aa:bb:cc:dd:ee:ff".to_string()),
                hwid: Some("hw-39".to_string()),
                package_version: Some("0.5.0".to_string()),
                package_model: Some("S9".to_string()),
                board_target: Some("am1-s9".to_string()),
                package_type: Some("sysupgrade".to_string()),
                created_at: "2026-04-30T00:00:00Z".to_string(),
            }),
            toolbox: ToolboxPackageInfo {
                install_command: "dcent install <ip> -f dcentos-sysupgrade.tar".to_string(),
                update_command: "dcent install <ip> -f dcentos-sysupgrade.tar".to_string(),
                upload_endpoint: Some("/api/system/upgrade".to_string()),
                board_target_header: None,
                device_model_header: None,
                requires_inactive_slot: true,
            },
        };

        let json = serde_json::to_string(&metadata).expect("serializes");
        let lower = json.to_ascii_lowercase();
        for forbidden in [
            "password",
            "passwd",
            "secret",
            "token",
            "private_key",
            "privatekey",
            "apikey",
            "api_key",
            "credential",
            "bc1q",
            "bc1p",
        ] {
            assert!(
                !lower.contains(forbidden),
                "SEC-1: unauth update/metadata must not expose `{forbidden}`: {json}"
            );
        }
    }

    #[test]
    fn fan_bridge_clamps_home_to_pwm_30_hard_cap() {
        // The load-bearing HOME PWM-30 hard cap: a home unit asking for full
        // (100) or any value above 30 must be clamped to 30 — even though the
        // gRPC bridge passes allow_loud=false, allow_loud could not lift it
        // anyway because the Home per-mode max is itself 30.
        for requested in [31u8, 50, 100, 200, 255] {
            let pwm = compute_commanded_fan_pwm(
                crate::OperatingMode::Home,
                "custom",
                Some(requested),
                false,
            )
            .expect("home custom PWM >= min is accepted");
            assert_eq!(
                pwm,
                dcentrald_hal::fan::PWM_SAFETY_MAX,
                "home requested {requested} must clamp to PWM-30"
            );
            assert_eq!(pwm, 30);
        }
        // allow_loud=true must STILL not lift the Home ceiling above 30.
        let pwm = compute_commanded_fan_pwm(crate::OperatingMode::Home, "custom", Some(100), true)
            .expect("accepted");
        assert_eq!(pwm, 30, "allow_loud must not widen the Home PWM-30 cap");
    }

    #[test]
    fn fan_bridge_home_rejects_below_minimum_custom_pwm() {
        // Home custom min is 10; a request below it is an honest reject (the
        // bridge maps this to a gRPC failed_precondition, not a silent ack).
        let err = compute_commanded_fan_pwm(crate::OperatingMode::Home, "custom", Some(5), false)
            .expect_err("below-min custom PWM must be rejected");
        assert!(err.contains("below safety minimum"), "got: {err}");
    }

    #[test]
    fn mqtt_fan_pwm_discovery_max_equals_home_safety_cap() {
        // P2-7: the HA `number` entity must advertise the SAME ceiling the
        // load-bearing home fan cap enforces, so HA's UI is safe-by-default. The
        // MQTT sink also re-clamps every command, but the advertised max must
        // never drift above the universal PWM-30 safety cap.
        assert_eq!(
            crate::mqtt::CMD_FAN_PWM_MAX,
            dcentrald_hal::fan::PWM_SAFETY_MAX
        );
        assert_eq!(crate::mqtt::CMD_FAN_PWM_MAX, 30);
    }

    #[test]
    fn fan_bridge_standard_and_hacker_envelopes() {
        // Standard without allow_loud is capped at the universal PWM-30 safety
        // max even though its per-mode ceiling is 60.
        let pwm =
            compute_commanded_fan_pwm(crate::OperatingMode::Standard, "custom", Some(55), false)
                .expect("accepted");
        assert_eq!(pwm, 30, "standard without allow_loud caps at PWM-30");
        // Standard WITH allow_loud may reach its per-mode ceiling (60).
        let pwm =
            compute_commanded_fan_pwm(crate::OperatingMode::Standard, "custom", Some(55), true)
                .expect("accepted");
        assert_eq!(pwm, 55, "standard allow_loud honours up to per-mode max");
        // Hacker takes responsibility — full duty allowed (allow_loud lifts to
        // the per-mode max of 100).
        let pwm =
            compute_commanded_fan_pwm(crate::OperatingMode::Hacker, "custom", Some(100), true)
                .expect("accepted");
        assert_eq!(pwm, 100, "hacker allow_loud reaches PWM-100");
    }

    #[test]
    fn fan_bridge_never_inverts_the_clamp_across_modes() {
        // Regression pin for the Mujina-#49 clamp-inversion class: the internal
        // `requested_pwm.clamp(safety_min, effective_max)` must NEVER be handed
        // min > max (u8::clamp panics, and panic=abort would abort the daemon
        // from a REST fan-set). We can't read the internal bounds directly, so
        // exercise every (mode × allow_loud × request) combination and assert
        // the call NEVER panics and always returns a PWM within the mode's
        // absolute safety envelope. A future edit that lowers PWM_SAFETY_MAX
        // below a per-mode safety_min (or raises a safety_min above its ceiling)
        // would panic here instead of shipping a crash.
        let modes = [
            (crate::OperatingMode::Home, 10u8, 30u8),
            (crate::OperatingMode::Standard, 20u8, 60u8),
            (crate::OperatingMode::Hacker, 0u8, 100u8),
        ];
        for (mode, min_floor, mode_ceiling) in modes {
            for &allow_loud in &[false, true] {
                for req in 0u8..=100 {
                    // Hacker has no per-mode minimum, so a sub-floor custom PWM
                    // is accepted; other modes reject it with an Err (never a
                    // panic) — both outcomes are safe. Use the label mode so the
                    // sub-floor rejection path is exercised too.
                    let out = compute_commanded_fan_pwm(mode, "custom", Some(req), allow_loud);
                    if let Ok(pwm) = out {
                        assert!(
                            pwm >= min_floor.min(mode_ceiling),
                            "{mode:?} loud={allow_loud} req={req} -> {pwm} below floor"
                        );
                        assert!(
                            pwm <= mode_ceiling,
                            "{mode:?} loud={allow_loud} req={req} -> {pwm} above per-mode ceiling"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn pool_bridge_tuple_conversion_preserves_fields() {
        let reqs = grpc_pools_to_requests(vec![(
            "stratum+tcp://public-pool.io:21496".to_string(),
            "bc1qexampleworker".to_string(),
            "x".to_string(),
            0,
        )]);
        assert_eq!(reqs.len(), 1);
        assert_eq!(reqs[0].url, "stratum+tcp://public-pool.io:21496");
        assert_eq!(reqs[0].worker, "bc1qexampleworker");
        assert_eq!(reqs[0].password, "x");
        assert_eq!(reqs[0].priority, Some(0));
        // gRPC carries no protocol/sv2/split.
        assert!(reqs[0].protocol.is_none());
        assert!(reqs[0].sv2_url.is_none());
        assert!(reqs[0].split_bps.is_none());
    }

    #[test]
    fn pool_bridge_validation_rejects_empty_and_overflow() {
        // Empty (no usable URL) is rejected by the same normalize the REST
        // handler uses — the bridge surfaces this as an honest reject.
        let empty = grpc_pools_to_requests(vec![(
            "   ".to_string(),
            "w".to_string(),
            "x".to_string(),
            0,
        )]);
        let err = normalize_pool_requests(empty).expect_err("empty primary URL must be rejected");
        assert!(err.contains("At least one pool URL"), "got: {err}");

        // More than 3 dashboard pools is rejected (matches POST /api/pools).
        let four = grpc_pools_to_requests(
            (0..4)
                .map(|i| {
                    (
                        format!("stratum+tcp://pool{i}.example:3333"),
                        "w".to_string(),
                        "x".to_string(),
                        i as u32,
                    )
                })
                .collect(),
        );
        let err = normalize_pool_requests(four).expect_err("more than 3 pools must be rejected");
        assert!(err.contains("up to 3"), "got: {err}");
    }

    #[test]
    fn pool_bridge_validation_rejects_bad_url() {
        // A syntactically invalid pool URL is rejected by the same
        // `validate_pool_url_support` the REST handler runs.
        let reqs = grpc_pools_to_requests(vec![(
            "not-a-stratum-url".to_string(),
            "w".to_string(),
            "x".to_string(),
            0,
        )]);
        let normalized = normalize_pool_requests(reqs).expect("non-empty URL passes normalize");
        let bad = normalized
            .iter()
            .any(|p| validate_pool_url_support(&p.url).is_err());
        assert!(
            bad,
            "an invalid stratum URL must fail validate_pool_url_support"
        );
    }

    #[test]
    fn home_noise_does_not_infer_db_from_pwm_without_rpm() {
        let fans = crate::FanState {
            pwm: 10,
            rpm: 0,
            per_fan: Vec::new(),
        };

        let (noise_db, source, note, max_rpm, feedback) = home_noise_from_fans(&fans);

        assert_eq!(noise_db, None);
        assert_eq!(source, "unavailable_no_rpm_feedback");
        assert_eq!(max_rpm, 0);
        assert!(!feedback);
        assert!(note.contains("PWM command alone"));
    }

    #[test]
    fn home_noise_does_not_infer_db_from_am2_zero_tach_channels() {
        let fans = crate::FanState {
            pwm: 10,
            rpm: 0,
            per_fan: (0..4)
                .map(|id| crate::PerFanReading {
                    id,
                    rpm: 0,
                    pwm_percent: 10,
                })
                .collect(),
        };

        let (noise_db, source, _note, max_rpm, feedback) = home_noise_from_fans(&fans);

        assert_eq!(noise_db, None);
        assert_eq!(source, "unavailable_no_rpm_feedback");
        assert_eq!(max_rpm, 0);
        assert!(!feedback);
    }

    #[test]
    fn home_noise_uses_positive_tach_feedback() {
        let fans = crate::FanState {
            pwm: 10,
            rpm: 0,
            per_fan: vec![
                crate::PerFanReading {
                    id: 0,
                    rpm: 0,
                    pwm_percent: 10,
                },
                crate::PerFanReading {
                    id: 1,
                    rpm: 2400,
                    pwm_percent: 10,
                },
            ],
        };

        let (noise_db, source, _note, max_rpm, feedback) = home_noise_from_fans(&fans);

        assert_eq!(noise_db, Some(50));
        assert_eq!(source, "tach_estimate");
        assert_eq!(max_rpm, 2400);
        assert!(feedback);
    }

    #[test]
    fn home_noise_emits_capped_db_and_tach_source_for_high_rpm() {
        // D-17: a present fan RPM must ALWAYS yield Some(noise_db) tagged
        // `tach_estimate` (never the "RPM" UI placeholder) — and the dB estimate
        // is bounded by the min(75, 30 + rpm/120) cap so a runaway tach reading
        // cannot emit an absurd acoustic figure. 9000 RPM -> 30 + 75 = 105 -> 75.
        let fans = crate::FanState {
            pwm: 30,
            rpm: 9000,
            per_fan: Vec::new(),
        };

        let (noise_db, source, _note, max_rpm, feedback) = home_noise_from_fans(&fans);

        assert_eq!(noise_db, Some(75));
        assert_eq!(source, "tach_estimate");
        assert_eq!(max_rpm, 9000);
        assert!(feedback);
    }

    #[test]
    fn home_presets_keep_live_noise_unverified_and_s9_estimate_separate() {
        let presets = home_preset_values();
        let low_power = presets
            .iter()
            .find(|preset| preset["name"].as_str() == Some("whisper"))
            .expect("low-power preset missing");

        assert!(low_power["noise_db"].is_null());
        assert!(low_power["estimated_noise_db_s9"].is_number());
        assert!(low_power["noise_note"]
            .as_str()
            .unwrap_or_default()
            .contains("S9 estimate only"));
        assert!(!low_power["display_name"]
            .as_str()
            .unwrap_or_default()
            .contains("dB"));
    }

    fn route_path_is_manifest_relevant(path: &str) -> bool {
        path.starts_with("/api/") || path == "/metrics" || path == MCP_HTTP_PATH
    }

    fn collect_route_literals(source: &str, routes: &mut std::collections::BTreeSet<String>) {
        let mut rest = source;
        while let Some(index) = rest.find(".route(") {
            rest = &rest[index + ".route(".len()..];
            let trimmed = rest.trim_start();
            let Some(after_quote) = trimmed.strip_prefix('"') else {
                continue;
            };
            let Some(end_quote) = after_quote.find('"') else {
                continue;
            };
            let path = &after_quote[..end_quote];
            if route_path_is_manifest_relevant(path) {
                routes.insert(path.to_string());
            }
            rest = &after_quote[end_quote..];
        }
    }

    fn mounted_route_paths_from_source() -> std::collections::BTreeSet<String> {
        let mut routes = std::collections::BTreeSet::new();
        for source in [
            include_str!("../rest.rs"),
            include_str!("../routes/re_catalog.rs"),
            include_str!("../routes/profiles.rs"),
            include_str!("../routes/restore_to_stock.rs"),
            include_str!("../routes/stock_parity.rs"),
        ] {
            collect_route_literals(source, &mut routes);
        }
        routes.insert("/api/re/catalog".to_string());
        routes
    }

    fn expected_mounted_route_paths_snapshot() -> std::collections::BTreeSet<String> {
        include_str!("route_paths_snapshot.txt")
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
            .map(str::to_string)
            .collect()
    }

    fn api_compatibility_manifest_route_paths() -> std::collections::BTreeSet<String> {
        build_api_compatibility_manifest_response()
            .surfaces
            .iter()
            .flat_map(|surface| surface.routes.iter())
            .map(|route| route.path)
            .filter(|path| route_path_is_manifest_relevant(path))
            .map(str::to_string)
            .collect()
    }

    #[test]
    fn mounted_router_path_snapshot_is_explicit() {
        let mounted = mounted_route_paths_from_source();
        let expected = expected_mounted_route_paths_snapshot();
        assert_eq!(
            mounted, expected,
            "mounted router path set changed; update route_paths_snapshot.txt only after reviewing the route-table diff"
        );
    }

    #[test]
    fn mcp_read_only_mount_advertises_exactly_read_tools() {
        assert_eq!(
            mcp_read_tool_names(),
            vec!["get_status", "get_device_info", "get_swarm_status"]
        );

        let tools = mcp_read_tool_descriptors();
        assert_eq!(tools.len(), 3);
        for tool in tools {
            assert_eq!(tool["annotations"]["readOnlyHint"].as_bool(), Some(true));
            assert_eq!(
                tool["inputSchema"]["additionalProperties"].as_bool(),
                Some(false)
            );
        }
    }

    #[test]
    fn mcp_read_only_mount_excludes_shared_write_tools() {
        for name in ["identify_device", "restart_mining", "set_pool"] {
            assert!(
                mcp_profile_write_tool_name(name),
                "{name} must remain classified as a shared write-profile tool"
            );
            assert!(
                !mcp_read_tool_names().iter().any(|tool| tool == name),
                "{name} must not be advertised by the DCENT_OS read-only MCP mount"
            );
        }
    }

    #[test]
    fn mcp_initialize_payload_is_read_only_streamable_http() {
        let payload = mcp_initialize_payload();
        assert_eq!(
            payload["protocolVersion"].as_str(),
            Some(MCP_PROTOCOL_VERSION)
        );
        assert_eq!(payload["transport"].as_str(), Some(MCP_TRANSPORT));
        assert_eq!(payload["profile"].as_str(), Some(MINIMAL_PROFILE_ID));
        assert_eq!(payload["readOnly"].as_bool(), Some(true));
        assert_eq!(
            payload["capabilities"]["tools"]["listChanged"].as_bool(),
            Some(false)
        );
    }

    #[test]
    fn mcp_status_power_hides_static_fallback_watts() {
        let projected = PowerTelemetryProjection {
            board_watts: 1_000,
            wall_watts: 1_100,
            efficiency_jth: 80.0,
            btu_h: 3_753.0,
            source: "static_model_fallback".to_string(),
            source_detail: "static_power_fallback_from_miner_state",
            live_power_available: false,
            modeled: true,
            calibrated: false,
            calibration_multiplier: None,
            note: "Live power has not published a positive reading; values are modeled from miner state and chip-profile defaults.",
        };

        let power = build_mcp_status_power_section(&projected);

        assert!(power["wall_watts"].is_null());
        assert!(power["board_watts"].is_null());
        assert!(power["efficiency_jth"].is_null());
        assert!(power["btu_h"].is_null());
        assert_eq!(power["source"].as_str(), Some("static_model_fallback"));
        assert_eq!(
            power["source_detail"].as_str(),
            Some("static_power_fallback_from_miner_state")
        );
        assert_eq!(power["live_power_available"].as_bool(), Some(false));
        assert_eq!(power["modeled"].as_bool(), Some(true));
    }

    #[test]
    fn mcp_status_power_surfaces_measured_watts_with_provenance() {
        let projected = PowerTelemetryProjection {
            board_watts: 1_000,
            wall_watts: 1_080,
            efficiency_jth: 27.0,
            btu_h: 3_684.96,
            source: "pmbus".to_string(),
            source_detail: "pmbus_measured",
            live_power_available: true,
            modeled: false,
            calibrated: false,
            calibration_multiplier: None,
            note: "Power is sourced from live measured telemetry.",
        };

        let power = build_mcp_status_power_section(&projected);

        assert_eq!(power["wall_watts"].as_u64(), Some(1_080));
        assert_eq!(power["board_watts"].as_u64(), Some(1_000));
        assert_eq!(power["source"].as_str(), Some("pmbus"));
        assert_eq!(power["source_detail"].as_str(), Some("pmbus_measured"));
        assert_eq!(power["live_power_available"].as_bool(), Some(true));
        assert_eq!(power["modeled"].as_bool(), Some(false));
    }

    #[test]
    fn unprovenanced_swarm_surfaces_accept_only_measured_power() {
        let measured = PowerTelemetryProjection {
            board_watts: 1_000,
            wall_watts: 1_080,
            efficiency_jth: 27.0,
            btu_h: 3_684.96,
            source: "pmbus".to_string(),
            source_detail: "pmbus_measured",
            live_power_available: true,
            modeled: false,
            calibrated: false,
            calibration_multiplier: None,
            note: "Power is sourced from live measured telemetry.",
        };
        let modeled = PowerTelemetryProjection {
            source: "static_model_fallback".to_string(),
            source_detail: "static_power_fallback_from_miner_state",
            live_power_available: false,
            modeled: true,
            ..measured.clone()
        };
        let runtime_model = PowerTelemetryProjection {
            source: "live".to_string(),
            source_detail: "live_runtime_model",
            live_power_available: true,
            modeled: true,
            ..measured.clone()
        };

        assert_eq!(
            measured_wall_watts_for_unprovenanced_surface(&measured),
            1_080.0
        );
        assert_eq!(measured_wall_watts_for_unprovenanced_surface(&modeled), 0.0);
        assert_eq!(
            measured_wall_watts_for_unprovenanced_surface(&runtime_model),
            0.0
        );
    }

    #[test]
    fn power_targeting_delta_requires_measured_wall_power() {
        let configured = ConfiguredPowerTarget {
            source: "autotuner".to_string(),
            mode: Some("power".to_string()),
            preset: None,
            schedule_label: None,
            target_watts: Some(1_200),
        };
        let measured = PowerTelemetryProjection {
            board_watts: 1_100,
            wall_watts: 1_260,
            efficiency_jth: 30.0,
            btu_h: 4_299.12,
            source: "pmbus".to_string(),
            source_detail: "pmbus_measured",
            live_power_available: true,
            modeled: false,
            calibrated: false,
            calibration_multiplier: None,
            note: "Power is sourced from live measured telemetry.",
        };
        let runtime_model = PowerTelemetryProjection {
            source: "live".to_string(),
            source_detail: "live_runtime_model",
            modeled: true,
            ..measured.clone()
        };

        let measured_targeting =
            build_power_targeting_state_from_configured(Some(configured.clone()), &measured);
        assert_eq!(measured_targeting.current_wall_watts, 1_260);
        assert!(measured_targeting.current_wall_watts_measured);
        assert_eq!(
            measured_targeting.current_wall_watts_source_detail,
            Some("pmbus_measured")
        );
        assert_eq!(measured_targeting.delta_watts, Some(60));
        assert_eq!(measured_targeting.comparison.as_deref(), Some("over"));

        let modeled_targeting =
            build_power_targeting_state_from_configured(Some(configured), &runtime_model);
        assert!(modeled_targeting.active);
        assert_eq!(modeled_targeting.target_watts, Some(1_200));
        assert_eq!(modeled_targeting.current_wall_watts, 0);
        assert!(!modeled_targeting.current_wall_watts_measured);
        assert_eq!(modeled_targeting.current_wall_watts_source_detail, None);
        assert_eq!(modeled_targeting.delta_watts, None);
        assert_eq!(modeled_targeting.comparison, None);
    }

    #[test]
    fn power_cost_projection_suppresses_static_fallback_watts() {
        let fallback = PowerTelemetryProjection {
            board_watts: 1_000,
            wall_watts: 1_100,
            efficiency_jth: 80.0,
            btu_h: 3_753.0,
            source: "static_model_fallback".to_string(),
            source_detail: "static_power_fallback_from_miner_state",
            live_power_available: false,
            modeled: true,
            calibrated: false,
            calibration_multiplier: None,
            note: "Live power has not published a positive reading; values are modeled from miner state and chip-profile defaults.",
        };
        let live_modeled = PowerTelemetryProjection {
            source: "live".to_string(),
            source_detail: "live_runtime_model",
            live_power_available: true,
            modeled: true,
            ..fallback.clone()
        };

        let fallback_cost = project_power_costs(&fallback, 0.10, 1.0);
        assert_eq!(fallback_cost.wall_watts, 0);
        assert!(!fallback_cost.live_power_available);
        assert!(fallback_cost.note.contains("suppressed"));
        assert_eq!(format!("{:.2}", fallback_cost.daily_cost_usd), "0.00");
        assert_eq!(
            format!("{:.2}", fallback_cost.heat_reuse_credit_usd_per_day),
            "0.00"
        );

        let live_modeled_cost = project_power_costs(&live_modeled, 0.10, 1.0);
        assert_eq!(live_modeled_cost.wall_watts, 1_100);
        assert!(live_modeled_cost.live_power_available);
        assert!(live_modeled_cost.modeled);
        assert_eq!(live_modeled_cost.source_detail, "live_runtime_model");
        assert_eq!(format!("{:.2}", live_modeled_cost.daily_cost_usd), "2.64");
        assert_eq!(
            format!("{:.2}", live_modeled_cost.heat_reuse_credit_usd_per_day),
            "2.64"
        );
    }

    /// CALIBRATION-THREADING PIN: the saved-profile efficiency snapshot and the
    /// profitability estimate both run through `saved_profile_efficiency_snapshot`,
    /// which threads the ACTIVE operator wall-meter calibration multiplier
    /// instead of the old hardcoded `1.0`. An active calibration must therefore
    /// raise BOTH the fallback snapshot watts AND the profitability daily cost,
    /// and stamp the estimate's provenance as `calibrated_model`.
    #[test]
    fn operator_calibration_scales_saved_profile_efficiency_and_profitability_cost() {
        use std::collections::HashMap;

        let chips: Vec<dcentrald_autotuner::ChipProfile> = (0..3)
            .map(|i| dcentrald_autotuner::ChipProfile {
                chip_index: i as u8,
                max_stable_mhz: 700,
                operating_mhz: 650,
                grade: dcentrald_autotuner::profile::ChipGrade::B,
                error_rate: 0.001,
                nonces_counted: 100,
                vf_curve: None,
                thermal_max_stable_mhz: None,
            })
            .collect();
        let stats = dcentrald_autotuner::TuningProfile::compute_stats(&chips, 15.0);
        let mut profiles: HashMap<u8, dcentrald_autotuner::TuningProfile> = HashMap::new();
        profiles.insert(
            6u8,
            dcentrald_autotuner::TuningProfile {
                version: 2,
                chip_type: "BM1387".to_string(),
                chain_id: 6,
                chip_count: 3,
                voltage_mv: 9100,
                tuned_at: "0".to_string(),
                ambient_temp_c: None,
                optimal_voltage_mv: Some(9000),
                estimated_power_w: 0.0,
                estimated_efficiency_jth: 0.0,
                equilibrium_temp_c: Some(55.0),
                thermal_refinement_duration_s: None,
                calibrated_c_eff: None,
                chips,
                stats,
                hashboard_sku: None,
                hashboard_sku_flags: None,
            },
        );

        // Uncalibrated (multiplier 1.0) vs an active operator wall-meter
        // calibration (1.25x).
        let uncalibrated = dcentrald_autotuner::PowerCalibration::default();
        let calibrated = dcentrald_autotuner::PowerCalibration {
            enabled: true,
            multiplier: 1.25,
            ..Default::default()
        };
        assert!((uncalibrated.effective_multiplier() - 1.0).abs() < 1e-9);
        assert!((calibrated.effective_multiplier() - 1.25).abs() < 1e-9);

        let base = saved_profile_efficiency_snapshot(&profiles, &uncalibrated);
        let scaled = saved_profile_efficiency_snapshot(&profiles, &calibrated);

        assert!(base.total_power_w > 0.0);
        assert!(
            scaled.total_power_w > base.total_power_w * 1.1,
            "an active 1.25x calibration must raise the fallback snapshot watts: \
             base={} scaled={}",
            base.total_power_w,
            scaled.total_power_w
        );

        // Profitability daily electricity cost, computed from those watts, must
        // move with the calibration — the whole point of threading it in.
        let base_est = dcentrald_autotuner::estimate_profitability(
            base.total_hashrate_ghs / 1000.0,
            base.total_power_w,
            0.12,
            100_000.0,
            1e14,
        );
        let scaled_est = dcentrald_autotuner::estimate_profitability(
            scaled.total_hashrate_ghs / 1000.0,
            scaled.total_power_w,
            0.12,
            100_000.0,
            1e14,
        );
        assert!(
            scaled_est.daily_electricity_usd > base_est.daily_electricity_usd * 1.1,
            "calibrated daily cost must exceed the uncalibrated modeled cost: \
             base={} scaled={}",
            base_est.daily_electricity_usd,
            scaled_est.daily_electricity_usd
        );

        // Provenance stamp mirrors `post_autotuner_profitability`: the default
        // estimate is `model`; an active calibration stamps `calibrated_model`.
        assert_eq!(base_est.power_basis, "model");
        let stamped_basis = if calibrated.is_active() {
            "calibrated_model"
        } else {
            "model"
        };
        assert_eq!(stamped_basis, "calibrated_model");
    }

    #[test]
    fn power_calibration_contract_suppresses_static_fallback_watts() {
        let fallback = PowerTelemetryProjection {
            board_watts: 1_000,
            wall_watts: 1_100,
            efficiency_jth: 80.0,
            btu_h: 3_753.0,
            source: "static_model_fallback".to_string(),
            source_detail: "static_power_fallback_from_miner_state",
            live_power_available: false,
            modeled: true,
            calibrated: false,
            calibration_multiplier: None,
            note: "Live power has not published a positive reading; values are modeled from miner state and chip-profile defaults.",
        };

        let contract = project_power_calibration_contract(&fallback, Some(1.2));
        assert_eq!(contract.current_reported_wall_watts, 0.0);
        assert_eq!(contract.current_reported_unit_watts, 0.0);
        assert_eq!(contract.projected_wall_watts, Some(0.0));
        assert_eq!(contract.projected_unit_watts, Some(0.0));
        assert_eq!(contract.power_source, "static_model_fallback");
        assert_eq!(
            contract.power_source_detail,
            "static_power_fallback_from_miner_state"
        );
        assert!(!contract.live_power_available);
        assert!(contract.power_modeled);
        assert!(!contract.calibrated);
        assert!(contract.calibration_multiplier.is_none());
    }

    #[test]
    fn power_calibration_contract_labels_live_modeled_power() {
        let live_modeled = PowerTelemetryProjection {
            board_watts: 1_000,
            wall_watts: 1_100,
            efficiency_jth: 80.0,
            btu_h: 3_753.0,
            source: "live".to_string(),
            source_detail: "live_runtime_model",
            live_power_available: true,
            modeled: true,
            calibrated: false,
            calibration_multiplier: None,
            note: "Power is modeled from the live dispatcher estimate; it is not a direct wall-meter measurement.",
        };

        let contract = project_power_calibration_contract(&live_modeled, Some(1.1));
        assert_eq!(contract.current_reported_wall_watts, 1_100.0);
        assert_eq!(contract.current_reported_unit_watts, 1_000.0);
        assert_eq!(
            format!("{:.2}", contract.projected_wall_watts.unwrap()),
            "1210.00"
        );
        assert_eq!(contract.power_source_detail, "live_runtime_model");
        assert!(contract.live_power_available);
        assert!(contract.power_modeled);
        assert!(contract.calibrated);
        assert_eq!(contract.calibration_multiplier, Some(1.1));
        assert!(contract.power_note.contains("modeled"));
    }

    #[test]
    fn system_info_source_keeps_power_provenance_contract() {
        // get_system_info / get_system_asic moved into this module (late.rs) by
        // the rest.rs decomposition; grep the current file, not the old rest.rs.
        let source = include_str!("late.rs");
        let start = source
            .rfind("async fn get_system_info")
            .expect("get_system_info present");
        let end = start
            + source[start..]
                .find("async fn get_system_asic")
                .expect("get_system_asic present after get_system_info");
        let system_info = &source[start..end];

        assert!(system_info.contains("\"power_source_detail\""));
        assert!(system_info.contains("power_projection.source_detail"));
        assert!(
            system_info.contains("\"live_power_available\": power_projection.live_power_available")
        );
        assert!(system_info.contains("\"power_modeled\": power_projection.modeled"));
        assert!(system_info.contains("\"power_note\": power_projection.note"));
        assert!(system_info.contains("\"power\": measured_wall_watts"));
        assert!(system_info.contains("\"identification_confidence\""));
        assert!(system_info.contains("\"identification\": &hw.identification"));
        assert!(!system_info.contains("\"power\": wall_watts"));
    }

    #[test]
    fn reboot_api_leaves_all_escalation_to_pid1() {
        let source = include_str!("late.rs");
        let handler_start = source
            .rfind("pub(super) async fn post_action_reboot")
            .expect("reboot handler present");
        let start = source
            .rfind("pub(super) async fn trigger_system_reboot()")
            .expect("reboot helper present");
        let handler = &source[handler_start..start];
        let end = start
            + source[start..]
                .find("/// POST /api/action/sleep")
                .expect("sleep handler follows reboot helper");
        let reboot = &source[start..end];

        let request = handler
            .find("trigger_system_reboot().await")
            .expect("handler awaits init request acceptance");
        let accepted = handler
            .find("StatusCode::ACCEPTED")
            .expect("accepted response exists");
        assert!(request < accepted);
        let before_transfer = &handler[..request];
        assert!(!before_transfer.contains("push_rest_audit_free"));
        assert!(!before_transfer.contains("tracing::"));
        assert!(!handler.contains("tokio::spawn"));
        assert!(!handler.contains("tokio::time::sleep"));
        assert!(!handler.contains("Command::new(\"dd\")"));
        assert!(!handler.contains("Command::new(\"sync\")"));

        assert!(reboot.contains("trigger_system_reboot_with(\"reboot\")"));
        assert!(reboot.contains("tokio::process::Command::new(program)"));
        assert!(!reboot.contains("REBOOT_FALLBACK_GRACE_SECS"));
        assert!(!reboot.contains("/proc/sysrq-trigger"));
        assert!(!reboot.contains(".arg(\"-f\")"));
        assert!(!reboot.contains("tokio::time::sleep"));
        assert!(!reboot.contains("Command::new(\"sync\")"));
    }

    #[tokio::test]
    async fn reboot_request_reports_acceptance_rejection_and_spawn_failure() {
        assert!(trigger_system_reboot_with("/bin/true").await.is_ok());
        assert!(matches!(
            trigger_system_reboot_with("/bin/false").await,
            Err(RebootRequestError::Rejected(_))
        ));
        assert!(matches!(
            trigger_system_reboot_with("/no/such/dcentos-reboot-command").await,
            Err(RebootRequestError::Spawn(_))
        ));
    }

    #[test]
    fn mcp_device_info_surfaces_hardware_identification_confidence() {
        let miner = crate::MinerState::empty(crate::OperatingMode::Standard);
        let hw = crate::HardwareInfo {
            control_board: "Zynq am2-s17".to_string(),
            chip_type: "BM1362".to_string(),
            identification: crate::HardwareIdentification::from_evidence(
                vec![
                    crate::HardwareIdentityEvidence::declared_asic_config("s19jpro", "BM1362"),
                    crate::HardwareIdentityEvidence::declared_asic_board_target(
                        "am2-s19j", "BM1362",
                    ),
                ],
                Some("configured model and baked board target agree on ASIC family".to_string()),
            ),
            ..crate::HardwareInfo::default()
        };

        let body = mcp_device_info_payload(
            &miner,
            &hw,
            "dcent-test".to_string(),
            "192.0.2.44".to_string(),
        );

        assert_eq!(body["identification_confidence"], "low");
        assert_eq!(body["identification"]["confidence"], "low");
        assert_eq!(body["identification"]["evidence"][0]["level"], "declared");
        assert_eq!(
            body["identification"]["sources"][1],
            "board_target:am2-s19j->BM1362"
        );
    }

    #[test]
    fn swarm_discovery_advertises_mcp_only_when_url_is_known() {
        assert!(
            mounted_route_paths_from_source().contains(MCP_HTTP_PATH),
            "swarm discovery must not advertise MCP unless the /mcp route is mounted"
        );

        let known = swarm_discovery("192.0.2.10");
        assert_eq!(known.mcp_url.as_deref(), Some("http://192.0.2.10/mcp"));
        assert_eq!(known.mcp_transport.as_deref(), Some(MCP_TRANSPORT));
        assert_eq!(known.mcp_profile.as_deref(), Some(MINIMAL_PROFILE_ID));

        let unknown = swarm_discovery("unknown");
        assert_eq!(unknown.mcp_url, None);
        assert_eq!(unknown.mcp_transport, None);
        assert_eq!(unknown.mcp_profile, None);
    }

    #[test]
    fn pool_state_semantics_do_not_treat_connecting_as_connected_or_active() {
        for status in ["connecting", "Connecting", " CONNECTING "] {
            assert!(is_pool_connecting(status), "{status} should be connecting");
            assert!(!is_pool_connected(status), "{status} must not be connected");
            assert!(
                !is_pool_mining_capable(status),
                "{status} must not be mining-capable"
            );
        }
    }

    #[test]
    fn pool_state_semantics_split_connected_from_mining_capable() {
        for status in ["Connected", "Authorized"] {
            assert!(is_pool_connected(status), "{status} should be connected");
            assert!(
                !is_pool_mining_capable(status),
                "{status} is not job-active mining telemetry"
            );
        }

        for status in ["Alive", "Donating", "Active", "Mining", "mining"] {
            assert!(is_pool_connected(status), "{status} should be connected");
            assert!(
                is_pool_mining_capable(status),
                "{status} should be mining-capable"
            );
        }
    }

    #[test]
    fn rest_pool_url_validation_uses_strict_v1_validator() {
        assert!(validate_pool_url_support("stratum+tcp://pool.example.com:3333").is_ok());

        let credentials =
            validate_pool_url_support("stratum+tcp://user:secret@pool.example.com:3333")
                .unwrap_err();
        assert!(credentials.contains("credentials"));

        let path =
            validate_pool_url_support("stratum+tcp://pool.example.com:3333/path").unwrap_err();
        assert!(path.contains("path"));

        assert!(validate_pool_url_support("stratum+tls://pool.example.com:443").is_ok());
        assert!(validate_pool_url_support("stratum+ssl://pool.example.com:443").is_ok());
    }

    #[test]
    fn diagnostic_failure_modes_response_uses_canonical_catalog() {
        let body = build_diag_failure_modes_response();
        let modes = body["modes"].as_array().expect("modes array");

        assert_eq!(
            body["count"].as_u64(),
            Some(dcentrald_api_types::failure_mode::ALL_FAILURE_MODES.len() as u64)
        );
        assert_eq!(
            modes.len(),
            dcentrald_api_types::failure_mode::ALL_FAILURE_MODES.len()
        );
        let degraded = serde_json::to_value(
            dcentrald_api_types::failure_mode::FailureMode::Am2DegradedDspicFirmware,
        )
        .unwrap();
        assert!(
            modes.iter().any(|entry| entry["mode"] == degraded),
            "failure mode catalog must include Am2DegradedDspicFirmware"
        );
    }

    #[test]
    fn pool_config_change_metadata_covers_worker_failover_and_redacts_values() {
        let before = PoolAuditSnapshot {
            pools: vec![
                PoolAuditSlot {
                    url: "stratum+tcp://primary.example:3333".to_string(),
                    worker: "old-worker".to_string(),
                    password: "old-password".to_string(),
                    protocol: None,
                    sv2_url: None,
                    split_bps: None,
                },
                PoolAuditSlot {
                    url: "stratum+tcp://failover-old.example:3333".to_string(),
                    worker: "failover-worker".to_string(),
                    password: "failover-password".to_string(),
                    protocol: None,
                    sv2_url: None,
                    split_bps: None,
                },
            ],
            routing_mode: None,
            split_cycle_duration_s: None,
        };
        let after = PoolAuditSnapshot {
            pools: vec![
                PoolAuditSlot {
                    url: "stratum+tcp://primary.example:3333".to_string(),
                    worker: "new-worker-secret".to_string(),
                    password: "old-password".to_string(),
                    protocol: None,
                    sv2_url: None,
                    split_bps: None,
                },
                PoolAuditSlot {
                    url: "stratum+tcp://failover-new.example:3333".to_string(),
                    worker: "failover-worker".to_string(),
                    password: "new-failover-password".to_string(),
                    protocol: None,
                    sv2_url: None,
                    split_bps: None,
                },
            ],
            routing_mode: None,
            split_cycle_duration_s: None,
        };

        let changed = pool_config_changed_fields(&before, &after);
        assert!(changed.contains(&"pool.primary.worker".to_string()));
        assert!(changed.contains(&"pool.failover1.url".to_string()));
        assert!(changed.contains(&"pool.failover1.password".to_string()));

        let event = pool_config_write_audit_event(2, changed);
        let line = dcentrald_api_types::audit_log::AuditRecord::new(1, "rest_dashboard", event)
            .to_ndjson_line()
            .unwrap();
        assert!(line.contains("\"event\":\"pool_config_write\""));
        assert!(line.contains("pool.failover1.url"));
        assert!(!line.contains("new-worker-secret"));
        assert!(!line.contains("new-failover-password"));
    }

    /// P0-2 (C-2 / D-1 / D-2): pins the `/api/status` per-chain projection so a
    /// live topline never surfaces bare per-chain `hashrate_ghs: 0.0`, and
    /// per-chain `voltage_mv` is always tagged as commanded (never measured)
    /// because the S9 has no per-chain voltage ADC.
    #[test]
    fn chain_telemetry_projection_is_honest_under_live_topline() {
        let mk = |id: u8, chips: u8, hr: f64, mv: u16| crate::ChainState {
            id,
            chips,
            frequency_mhz: 650,
            voltage_mv: mv,
            temp_c: 50.0,
            temp_source: None,
            hashrate_ghs: hr,
            errors: 0,
            status: "Alive".to_string(),
        };

        // Audit case: live 1131 GH/s topline, every per-chain hashrate 0.0, and
        // no commanded per-chain voltage. Representative S9 commanded DAC default
        // is 8600 mV (BM1387 profile).
        let chains = vec![mk(6, 63, 0.0, 0), mk(7, 63, 0.0, 0), mk(8, 62, 0.0, 0)];
        let proj = project_chain_telemetry(&chains, 1131.0, 8600);
        // No bare zero under a live topline — split proportional to chip count,
        // and the split conserves the topline.
        let total: f64 = proj.iter().map(|p| p.hashrate_ghs).sum();
        assert!(
            (total - 1131.0).abs() < 1e-6,
            "topline must be conserved across the per-chain split"
        );
        assert!(proj.iter().all(|p| p.hashrate_ghs > 0.0));
        assert!(proj
            .iter()
            .all(|p| p.hashrate_source == "derived_topline_split"));
        assert!(proj.iter().all(|p| p.frequency_mhz == 650));
        assert!(proj.iter().all(|p| p.frequency_source == "chain_state"));
        assert!((proj[0].hashrate_ghs - 1131.0 * 63.0 / 188.0).abs() < 1e-6);
        // Voltage falls back to the commanded DAC default, tagged not-measured.
        assert!(proj.iter().all(|p| p.voltage_mv == 8600));
        assert!(proj.iter().all(|p| p.voltage_source == "commanded_default"));

        // Per-chain case: real per-chain hashrate + commanded voltage present.
        let chains2 = vec![
            mk(6, 63, 377.0, 8900),
            mk(7, 63, 377.0, 8900),
            mk(8, 62, 377.0, 8900),
        ];
        let proj2 = project_chain_telemetry(&chains2, 1131.0, 8600);
        assert!(proj2.iter().all(|p| (p.hashrate_ghs - 377.0).abs() < 1e-6));
        assert!(proj2.iter().all(|p| p.hashrate_source == "per_chain"));
        assert!(proj2.iter().all(|p| p.voltage_mv == 8900));
        assert!(proj2
            .iter()
            .all(|p| p.voltage_source == "commanded_not_measured"));

        // Mixed: one dead chain (0 chips, 0 hashrate) while a sibling is live —
        // the dead chain stays a genuine measured 0 (NOT a fabricated split),
        // and the live sibling keeps its real per-chain value.
        let chains3 = vec![mk(6, 63, 800.0, 8900), mk(7, 0, 0.0, 0)];
        let proj3 = project_chain_telemetry(&chains3, 800.0, 8600);
        assert_eq!(proj3[0].hashrate_source, "per_chain");
        assert!((proj3[0].hashrate_ghs - 800.0).abs() < 1e-6);
        assert_eq!(proj3[1].hashrate_ghs, 0.0);
        assert_eq!(proj3[1].hashrate_source, "per_chain");
        // Dead chain (0 chips) with no commanded voltage → honest unknown, not a
        // fabricated default.
        assert_eq!(proj3[1].voltage_mv, 0);
        assert_eq!(proj3[1].voltage_source, "unknown");
    }

    #[test]
    fn chain_frequency_source_distinguishes_runtime_missing_and_unavailable() {
        let mk = |id: u8, chips: u8, frequency_mhz: u16| crate::ChainState {
            id,
            chips,
            frequency_mhz,
            voltage_mv: 0,
            temp_c: 0.0,
            temp_source: None,
            hashrate_ghs: 0.0,
            errors: 0,
            status: "Idle".to_string(),
        };

        let live = mk(0, 126, 525);
        let active_but_missing = mk(1, 126, 0);
        let unavailable = mk(2, 0, 0);

        assert_eq!(chain_frequency_source(&live), "chain_state");
        assert_eq!(
            chain_frequency_source(&active_but_missing),
            "unreported_active_chain"
        );
        assert_eq!(chain_frequency_source(&unavailable), "unavailable");
        assert_eq!(primary_frequency_source(Some(&live)), "chain_state");
        assert_eq!(primary_frequency_source(None), "unavailable");

        let chains = vec![live, active_but_missing, unavailable];
        let projected = project_chain_telemetry(&chains, 0.0, 0);
        assert_eq!(projected[0].frequency_mhz, 525);
        assert_eq!(projected[0].frequency_source, "chain_state");
        assert_eq!(projected[1].frequency_mhz, 0);
        assert_eq!(projected[1].frequency_source, "unreported_active_chain");
        assert_eq!(projected[2].frequency_mhz, 0);
        assert_eq!(projected[2].frequency_source, "unavailable");
    }

    /// AT-1 (chip-rail voltage read-back): a plausible per-chain *measured* 0x3A
    /// reading overrides the commanded DAC value and is tagged `"measured"`,
    /// while a chain with no measured entry still falls back to commanded — same
    /// projection, just provenance-upgraded where a real rail reading exists.
    #[test]
    fn chain_telemetry_measured_rail_takes_priority_over_commanded() {
        let mk = |id: u8, chips: u8, hr: f64, mv: u16| crate::ChainState {
            id,
            chips,
            frequency_mhz: 525,
            voltage_mv: mv,
            temp_c: 55.0,
            temp_source: None,
            hashrate_ghs: hr,
            errors: 0,
            status: "Alive".to_string(),
        };

        // am2-class: two chains, both commanded 13700 mV. Chain 0 has a fresh
        // measured 0x3A reading of 13702 mV; chain 1 has none.
        let chains = vec![mk(0, 126, 377.0, 13_700), mk(1, 126, 377.0, 13_700)];
        let mut measured = std::collections::HashMap::new();
        measured.insert(0u8, 13_702u16);

        let proj = project_chain_telemetry_with_measured(&chains, 754.0, 13_700, &measured);

        // Chain 0: measured wins, tagged "measured".
        assert_eq!(proj[0].voltage_mv, 13_702);
        assert_eq!(proj[0].voltage_source, "measured");
        // Chain 1: no measured reading → commanded, tagged not-measured.
        assert_eq!(proj[1].voltage_mv, 13_700);
        assert_eq!(proj[1].voltage_source, "commanded_not_measured");

        // An implausible measured reading (over the dsPIC DAC ceiling) is rejected
        // by the shared resolver and must NOT masquerade as "measured".
        let mut bad = std::collections::HashMap::new();
        bad.insert(0u8, 0xFFFFu16);
        let proj_bad = project_chain_telemetry_with_measured(&chains, 754.0, 13_700, &bad);
        assert_eq!(proj_bad[0].voltage_mv, 13_700);
        assert_eq!(proj_bad[0].voltage_source, "commanded_not_measured");

        // The empty-map path is byte-identical to the legacy 3-arg projection.
        let legacy = project_chain_telemetry(&chains, 754.0, 13_700);
        let empty = project_chain_telemetry_with_measured(
            &chains,
            754.0,
            13_700,
            &std::collections::HashMap::new(),
        );
        for (a, b) in legacy.iter().zip(empty.iter()) {
            assert_eq!(a.voltage_mv, b.voltage_mv);
            assert_eq!(a.voltage_source, b.voltage_source);
            assert_eq!(a.hashrate_source, b.hashrate_source);
        }
    }

    /// AT-3 end-to-end (slot → live projection): a reading published into the
    /// process-global AT-3 slot by the gated quiet-window 0x3A read flows
    /// through  and tags that chain `"measured"`;
    /// a chain with no published reading stays commanded-tagged. Uses a unique
    /// chain id so it never collides with the process-global slot's other
    /// (parallel-test) keys.
    #[test]
    fn at3_live_projection_picks_up_published_measured_rail() {
        const CID: u8 = 91; // unique, far from any real am2 chain id (0..=3)
        let mk = |id: u8, mv: u16| crate::ChainState {
            id,
            chips: 126,
            frequency_mhz: 525,
            voltage_mv: mv,
            temp_c: 55.0,
            temp_source: None,
            hashrate_ghs: 377.0,
            errors: 0,
            status: "Alive".to_string(),
        };
        let chains = vec![mk(CID, 13_700), mk(CID + 1, 13_700)];

        // Before any AT-3 publish: both chains are commanded-tagged (the
        // default, byte-identical to the pre-AT-3 projection).
        let before = project_chain_telemetry_live(&chains, 754.0, 13_700);
        assert_eq!(before[0].voltage_source, "commanded_not_measured");
        assert_eq!(before[1].voltage_source, "commanded_not_measured");

        // AT-3 publishes a fresh, plausible 0x3A reading for CID only.
        dcentrald_common::at3_rail::publish(CID, 13_702, false);
        let after = project_chain_telemetry_live(&chains, 754.0, 13_700);
        assert_eq!(after[0].voltage_mv, 13_702);
        assert_eq!(after[0].voltage_source, "measured");
        // The chain with no published reading is unaffected.
        assert_eq!(after[1].voltage_mv, 13_700);
        assert_eq!(after[1].voltage_source, "commanded_not_measured");
    }

    #[test]
    fn snapshot_context_sanitizes_credentialed_pool_url() {
        let profile_path = std::env::temp_dir()
            .join(format!("dcentrald-snapshot-context-{}", std::process::id()))
            .to_string_lossy()
            .into_owned();
        let state = crate::build_minimal_app_state(crate::MinimalAppStateInputs {
            api_config: crate::ApiConfig::default(),
            pool_url: "stratum+tcp://user:secret@pool.example.com:3333".to_string(),
            pool_protocol: "sv1".to_string(),
            mode: crate::OperatingMode::Standard,
            firmware_version: "test".to_string(),
            fan_pwm: 30,
            network_block: crate::NetworkBlockConfig::default(),
            profile_path,
            control_board_label: "test-control".to_string(),
            chip_type_label: "BM1387".to_string(),
            external_state_rx: None,
        });

        let context = snapshot_context(&state);
        assert_eq!(
            context.pool_url, "stratum+tcp://pool.example.com:3333",
            "diagnostic snapshots must not persist inline pool credentials"
        );
        assert!(
            !context.pool_url.contains("user:secret@"),
            "raw credentialed pool URL leaked into snapshot context"
        );
    }

    #[test]
    fn diagnostic_pool_dns_host_strips_inline_credentials() {
        let host =
            diagnostic_pool_dns_host("stratum+tcp://api09_user:api09_secret@pool.example.com:3333");

        assert_eq!(host.as_deref(), Some("pool.example.com"));
        assert!(
            !host.as_deref().unwrap_or_default().contains("api09_user")
                && !host.as_deref().unwrap_or_default().contains("api09_secret"),
            "diagnostic DNS host must not expose inline pool credentials: {host:?}"
        );
    }

    #[test]
    fn diagnostic_pool_dns_host_has_no_unsolicited_external_fallback() {
        assert_eq!(diagnostic_pool_dns_host(""), None);
        assert_eq!(diagnostic_pool_dns_host("not-a-pool-url"), None);
        assert_eq!(diagnostic_pool_dns_host("stratum+tcp://:3333"), None);
    }

    #[test]
    fn redaction_surface_harness_covers_diagnostic_snapshot_exports() {
        let raw_pool_url =
            "stratum+tcp://api09_user:api09_secret@pool.example.com:3333".to_string();
        let profile_path = std::env::temp_dir()
            .join(format!(
                "dcentrald-redaction-surface-{}",
                std::process::id()
            ))
            .to_string_lossy()
            .into_owned();
        let state = crate::build_minimal_app_state(crate::MinimalAppStateInputs {
            api_config: crate::ApiConfig::default(),
            pool_url: raw_pool_url.clone(),
            pool_protocol: "sv1".to_string(),
            mode: crate::OperatingMode::Standard,
            firmware_version: "test".to_string(),
            fan_pwm: 30,
            network_block: crate::NetworkBlockConfig::default(),
            profile_path,
            control_board_label: "test-control".to_string(),
            chip_type_label: "BM1387".to_string(),
            external_state_rx: None,
        });

        let context = snapshot_context(&state);
        let chip_health = build_chip_health_snapshot(&context, None);
        let board_health = build_board_health_snapshot(&context, None);
        let hashreport = build_hashreport_snapshot(&context, None);
        let network_fields = serde_json::json!({
            "dns_test_host": diagnostic_pool_dns_host(&raw_pool_url),
            "pool_url": dcentrald_stratum::pool_api::sanitize_pool_url(&raw_pool_url),
        });

        let surfaces = [
            (
                "snapshot_context",
                serde_json::to_string(&context).expect("snapshot context serializes"),
            ),
            (
                "chip_health_snapshot",
                serde_json::to_string(&chip_health).expect("chip health serializes"),
            ),
            (
                "board_health_snapshot",
                serde_json::to_string(&board_health).expect("board health serializes"),
            ),
            (
                "hashreport_snapshot",
                serde_json::to_string(&hashreport).expect("hashreport serializes"),
            ),
            ("network_diagnostic_fields", network_fields.to_string()),
        ];

        for (name, body) in surfaces {
            for forbidden in ["api09_user", "api09_secret", "api09_user:api09_secret@"] {
                assert!(
                    !body.contains(forbidden),
                    "{name} leaked raw pool credential marker {forbidden}: {body}"
                );
            }
        }

        assert_eq!(
            context.pool_url, "stratum+tcp://pool.example.com:3333",
            "diagnostic context should preserve the usable pool endpoint without credentials"
        );
        assert_eq!(
            diagnostic_pool_dns_host(&raw_pool_url).as_deref(),
            Some("pool.example.com"),
            "network diagnostics should resolve the sanitized pool host, not the URL userinfo"
        );
    }

    /// P1-2 (D-4/D-5): pins that `/api/mining/chain/presence` derives its
    /// `mv_target` + `chips_expected` per-platform from the detected chip's
    /// `MinerProfile`, instead of leaking the am2/BM1362  constants
    /// (13700 mV / 63 chips) onto every platform. An S9/BM1387 presence MUST NOT
    /// report the 13700 mV am2 chip-rail target.
    #[test]
    fn chain_presence_targets_are_per_platform_not_am2_constants() {
        // S9 / BM1387: must come from the BM1387 profile, NOT the am2 fallback.
        let (chips_expected, mv_target, source) = chain_presence_targets(Some(0x1387));
        assert_eq!(source, "chip_profile");
        assert_ne!(
            mv_target, W55A_DEFAULT_CHIP_RAIL_TARGET_MV,
            "S9/BM1387 must not report the am2 13700 mV chip-rail target"
        );
        let bm1387 = MinerProfile::for_chip(0x1387).expect("BM1387 profile present");
        assert_eq!(mv_target, bm1387.default_voltage_mv);
        assert_eq!(chips_expected, bm1387.chips_per_chain as u16);

        // am3-aml S21 / BM1368: a different platform resolves to its own target,
        // proving the value tracks the chip and isn't a single hardcoded const.
        let (s21_chips, s21_mv, s21_source) = chain_presence_targets(Some(0x1368));
        assert_eq!(s21_source, "chip_profile");
        let bm1368 = MinerProfile::for_chip(0x1368).expect("BM1368 profile present");
        assert_eq!(s21_mv, bm1368.default_voltage_mv);
        assert_eq!(s21_chips, bm1368.chips_per_chain as u16);

        // No resolvable chip (undetected/unknown) → clearly-labeled am2 fallback,
        // never a silent un-tagged value.
        let (fb_chips, fb_mv, fb_source) = chain_presence_targets(None);
        assert_eq!(fb_source, "fallback_am2_default");
        assert_eq!(fb_mv, W55A_DEFAULT_CHIP_RAIL_TARGET_MV);
        assert_eq!(fb_chips, W55A_DEFAULT_CHIPS_PER_CHAIN);
    }

    #[test]
    fn local_fleet_miners_response_matches_dashboard_shape() {
        let miner = crate::MinerState {
            hashrate_ghs: 12_345.6,
            hashrate_5s_ghs: 12_000.0,
            accepted: 7,
            rejected: 1,
            chains: vec![
                crate::ChainState {
                    id: 6,
                    chips: 63,
                    frequency_mhz: 550,
                    voltage_mv: 8_900,
                    temp_c: 52.0,
                    temp_source: Some(crate::ChainTempSource::BOARD_SENSOR.to_string()),
                    hashrate_ghs: 6_000.0,
                    errors: 0,
                    status: "Alive".to_string(),
                },
                crate::ChainState {
                    id: 7,
                    chips: 63,
                    frequency_mhz: 550,
                    voltage_mv: 8_900,
                    temp_c: 58.5,
                    temp_source: Some(crate::ChainTempSource::BOARD_SENSOR.to_string()),
                    hashrate_ghs: 6_345.6,
                    errors: 0,
                    status: "Alive".to_string(),
                },
            ],
            fans: crate::FanState {
                pwm: 30,
                rpm: 2_400,
                per_fan: Vec::new(),
            },
            pool: crate::PoolState {
                url: "stratum+tcp://example.invalid:3333".to_string(),
                worker: String::new(),
                status: "Alive".to_string(),
                difficulty: 1.0,
                last_share_at: 0,
                protocol: "sv1".to_string(),
                encrypted: false,
                encrypted_source: crate::pool_quality_honest_default_source(),
                sv2_session: None,
                sv2_session_source: crate::pool_quality_honest_default_source(),
                sv2_custom_job: None,
                donating: false,
                donating_source: crate::pool_quality_honest_default_source(),
                donation_active_url: String::new(),
                donation_active_worker: String::new(),
                donation_pool_index: 0,
                share_efficiency: None,
                auto_fallback_active: false,
                auto_fallback_source: crate::pool_quality_honest_default_source(),
                auto_retry_sv2_after_s: None,
                auto_fallback_reason: None,
                failover: dcentrald_stratum::types::PoolFailoverStatus::default(),
                failover_source: crate::pool_quality_honest_default_source(),
                hashrate_split: dcentrald_stratum::types::HashrateSplitStatus::default(),
                hashrate_split_source: crate::pool_quality_honest_default_source(),
                latency_ms: 0,
                latency_ms_source: crate::pool_quality_honest_default_source(),
                reject_reason_counts: [0; 6],
                reject_reason_counts_source: crate::pool_quality_honest_default_source(),
                rolling_acceptance_pct_30min: 100.0,
                rolling_acceptance_count_30min: (0, 0),
                rolling_acceptance_source: crate::pool_quality_honest_default_source(),
                worst_chip_hw_err_rate: None,
            },
            uptime_s: 600,
            firmware_version: "test".to_string(),
            mode: crate::OperatingMode::Standard,
        };
        let hardware = crate::HardwareInfo {
            chip_type: "BM1387".to_string(),
            ..crate::HardwareInfo::default()
        };

        let body = serde_json::to_value(build_local_fleet_miners_response(
            &miner,
            &hardware,
            "s9-lab".to_string(),
            "203.0.113.97".to_string(),
            1_777_000,
            // PR-048b: a snapshot with a proven  split.
            Some(8192.0),
            Some(40_000.0),
        ))
        .unwrap();

        assert_eq!(body["generated_at_ms"].as_u64(), Some(1_777_000));
        let row = &body["miners"][0];
        assert_eq!(row["id"].as_str(), Some("s9-lab"));
        assert_eq!(row["hostname"].as_str(), Some("s9-lab"));
        assert_eq!(row["ip"].as_str(), Some("203.0.113.97"));
        assert_eq!(row["model"].as_str(), Some("Antminer S9"));
        assert_eq!(row["hashrate_ghs"].as_f64(), Some(12_345.6));
        assert_eq!(row["temp_c"].as_f64(), Some(58.5));
        assert_eq!(row["fan_pwm"].as_u64(), Some(30));
        assert_eq!(row["status"].as_str(), Some("alive"));
        assert_eq!(row["last_seen_ms"].as_u64(), Some(1_777_000));
        // PR-048b:  split present and NOT conflated.
        assert_eq!(row["pool_target_difficulty"].as_f64(), Some(8192.0));
        assert_eq!(row["achieved_difficulty"].as_f64(), Some(40_000.0));
        assert_ne!(
            row["pool_target_difficulty"], row["achieved_difficulty"],
            "achieved must never equal pool target by construction"
        );
    }

    /// PR-048b: when no mining pipeline snapshot publisher is wired the fleet
    /// row must omit both difficulty fields entirely (additive/optional — zero
    /// wire-contract regression for existing consumers), and must NEVER
    /// fabricate `achieved_difficulty` from the pool target.
    #[test]
    fn fleet_miner_row_difficulty_fields_are_additive_optional_and_unconflated() {
        // Fixture is a freshly-started, not-yet-hashing miner (hashrate 0,
        // uptime 30 s, pool "Starting") so `fleet_status_for_miner` classifies
        // it as Starting -> "starting" below. The earlier 1_000.0 GH/s fixture
        // was a stale-test bug: `fleet_status_for_miner` (stable since
        // 2026-05-04, predating this PR-048b test) returns Alive for ANY
        // positive hashrate (`hashrate_ghs > 0.0`), so a 1_000 GH/s miner is
        // correctly "alive", never "starting". The CODE is correct (a miner
        // hashing at 1 TH/s IS alive — cf. the sibling
        // `local_fleet_miners_response_matches_dashboard_shape` test); only the
        // fixture's hashrate contradicted its own "starting" expectation. The
        // load-bearing point of THIS test is the difficulty-field
        // additivity/non-conflation contract below, which is unaffected by
        // hashrate (the pair is sourced from the (None, None) args, never from
        // MinerState).
        let miner = crate::MinerState {
            hashrate_ghs: 0.0,
            hashrate_5s_ghs: 0.0,
            accepted: 0,
            rejected: 0,
            chains: Vec::new(),
            fans: crate::FanState {
                pwm: 10,
                rpm: 0,
                per_fan: Vec::new(),
            },
            pool: crate::PoolState {
                url: "stratum+tcp://example.invalid:3333".to_string(),
                worker: String::new(),
                status: "Starting".to_string(),
                // Wave-9D9/9F: pool.difficulty is pool target only; it must
                // never leak into achieved_difficulty.
                difficulty: 2048.0,
                last_share_at: 0,
                protocol: "sv1".to_string(),
                encrypted: false,
                encrypted_source: crate::pool_quality_honest_default_source(),
                sv2_session: None,
                sv2_session_source: crate::pool_quality_honest_default_source(),
                sv2_custom_job: None,
                donating: false,
                donating_source: crate::pool_quality_honest_default_source(),
                donation_active_url: String::new(),
                donation_active_worker: String::new(),
                donation_pool_index: 0,
                share_efficiency: None,
                auto_fallback_active: false,
                auto_fallback_source: crate::pool_quality_honest_default_source(),
                auto_retry_sv2_after_s: None,
                auto_fallback_reason: None,
                failover: dcentrald_stratum::types::PoolFailoverStatus::default(),
                failover_source: crate::pool_quality_honest_default_source(),
                hashrate_split: dcentrald_stratum::types::HashrateSplitStatus::default(),
                hashrate_split_source: crate::pool_quality_honest_default_source(),
                latency_ms: 0,
                latency_ms_source: crate::pool_quality_honest_default_source(),
                reject_reason_counts: [0; 6],
                reject_reason_counts_source: crate::pool_quality_honest_default_source(),
                rolling_acceptance_pct_30min: 0.0,
                rolling_acceptance_count_30min: (0, 0),
                rolling_acceptance_source: crate::pool_quality_honest_default_source(),
                worst_chip_hw_err_rate: None,
            },
            uptime_s: 30,
            firmware_version: "test".to_string(),
            mode: crate::OperatingMode::Standard,
        };
        let hardware = crate::HardwareInfo::default();

        // No snapshot publisher wired -> the  pair is (None, None),
        // exactly what `build_mining_pipeline_snapshot_response(None, ..)`
        // (the single-miner source) yields via `unavailable()`.
        let body = serde_json::to_value(build_local_fleet_miners_response(
            &miner,
            &hardware,
            "no-pub".to_string(),
            "203.0.113.5".to_string(),
            42,
            None,
            None,
        ))
        .unwrap();

        let row = &body["miners"][0];
        // Additive/optional: absent from the wire (skip_serializing_if), so
        // existing consumers see byte-identical shape to before PR-048b.
        assert!(
            row.get("pool_target_difficulty").is_none(),
            "pool_target_difficulty must be omitted when unknown"
        );
        assert!(
            row.get("achieved_difficulty").is_none(),
            "achieved_difficulty must be omitted when not locally proven"
        );
        // Truth contract: the 2048.0 pool target on miner.pool.difficulty
        // must NOT have leaked into the row as achieved difficulty.
        assert_ne!(row["achieved_difficulty"].as_f64(), Some(2048.0));
        // The pre-PR-048b fields are untouched (no regression).
        assert_eq!(row["id"].as_str(), Some("no-pub"));
        assert_eq!(row["status"].as_str(), Some("starting"));
    }

    #[test]
    fn local_fleet_discover_response_matches_dashboard_contract() {
        let miner = crate::MinerState {
            hashrate_ghs: 12_345.6,
            hashrate_5s_ghs: 12_000.0,
            accepted: 7,
            rejected: 1,
            chains: Vec::new(),
            fans: crate::FanState {
                pwm: 30,
                rpm: 2_400,
                per_fan: Vec::new(),
            },
            pool: crate::PoolState {
                url: "stratum+tcp://example.invalid:3333".to_string(),
                worker: String::new(),
                status: "Alive".to_string(),
                difficulty: 1.0,
                last_share_at: 0,
                protocol: "sv1".to_string(),
                encrypted: false,
                encrypted_source: crate::pool_quality_honest_default_source(),
                sv2_session: None,
                sv2_session_source: crate::pool_quality_honest_default_source(),
                sv2_custom_job: None,
                donating: false,
                donating_source: crate::pool_quality_honest_default_source(),
                donation_active_url: String::new(),
                donation_active_worker: String::new(),
                donation_pool_index: 0,
                share_efficiency: None,
                auto_fallback_active: false,
                auto_fallback_source: crate::pool_quality_honest_default_source(),
                auto_retry_sv2_after_s: None,
                auto_fallback_reason: None,
                failover: dcentrald_stratum::types::PoolFailoverStatus::default(),
                failover_source: crate::pool_quality_honest_default_source(),
                hashrate_split: dcentrald_stratum::types::HashrateSplitStatus::default(),
                hashrate_split_source: crate::pool_quality_honest_default_source(),
                latency_ms: 0,
                latency_ms_source: crate::pool_quality_honest_default_source(),
                reject_reason_counts: [0; 6],
                reject_reason_counts_source: crate::pool_quality_honest_default_source(),
                rolling_acceptance_pct_30min: 100.0,
                rolling_acceptance_count_30min: (0, 0),
                rolling_acceptance_source: crate::pool_quality_honest_default_source(),
                worst_chip_hw_err_rate: None,
            },
            uptime_s: 600,
            firmware_version: "test".to_string(),
            mode: crate::OperatingMode::Standard,
        };
        let hardware = crate::HardwareInfo {
            chip_type: "BM1387".to_string(),
            ..crate::HardwareInfo::default()
        };
        let request = FleetDiscoverRequest {
            include_configured: true,
            manual_ips: vec!["203.0.113.39".to_string()],
            hint_ips: vec!["203.0.113.1".to_string()],
        };

        let body = build_local_fleet_discover_response(
            &miner,
            &hardware,
            "s9-lab".to_string(),
            "203.0.113.97".to_string(),
            "00:11:22:33:44:55".to_string(),
            Some(1_234.5),
            &request,
            1_777_000,
        );

        assert_eq!(body["status"].as_str(), Some("ok"));
        assert_eq!(body["source"].as_str(), Some("local_state"));
        let row = &body["miners"][0];
        assert_eq!(row["ip"].as_str(), Some("203.0.113.97"));
        assert_eq!(row["hostname"].as_str(), Some("s9-lab"));
        assert_eq!(row["model"].as_str(), Some("Antminer S9"));
        assert_eq!(row["firmware"].as_str(), Some("DCENTos vtest"));
        let hashrate_ths = row["hashrateThs"].as_f64().unwrap_or_default();
        assert!((hashrate_ths - 12.3456).abs() < 1e-9);
        assert_eq!(row["powerWatts"].as_f64(), Some(1_234.5));
        assert_eq!(row["status"].as_str(), Some("online"));
        assert_eq!(row["uptimeS"].as_u64(), Some(600));
        assert_eq!(row["mac"].as_str(), Some("00:11:22:33:44:55"));
        assert_eq!(
            body["request"]["manualIps"][0].as_str(),
            Some("203.0.113.39")
        );
        assert!(body["limitations"][0]
            .as_str()
            .unwrap_or_default()
            .contains("does not scan subnets"));
    }

    #[test]
    fn local_fleet_discover_response_keeps_unreported_power_null() {
        let mut miner = crate::MinerState::empty(crate::OperatingMode::Standard);
        miner.hashrate_ghs = 12_345.6;
        miner.firmware_version = "test".to_string();
        let request = FleetDiscoverRequest {
            include_configured: true,
            manual_ips: vec![],
            hint_ips: vec![],
        };

        let body = build_local_fleet_discover_response(
            &miner,
            &crate::HardwareInfo::default(),
            "s9-lab".to_string(),
            "203.0.113.97".to_string(),
            "00:11:22:33:44:55".to_string(),
            None,
            &request,
            1_777_000,
        );
        assert!(body["miners"][0]["powerWatts"].is_null());

        let negative_body = build_local_fleet_discover_response(
            &miner,
            &crate::HardwareInfo::default(),
            "s9-lab".to_string(),
            "203.0.113.97".to_string(),
            "00:11:22:33:44:55".to_string(),
            Some(-1.0),
            &request,
            1_777_000,
        );
        assert!(negative_body["miners"][0]["powerWatts"].is_null());
    }

    #[test]
    fn local_fleet_discover_reports_only_measured_power() {
        let miner = crate::MinerState::empty(crate::OperatingMode::Standard);
        let hardware = crate::HardwareInfo::default();

        let board_only = dcentrald_autotuner::LivePowerEstimate {
            board_watts: 1_234.0,
            wall_watts: 0.0,
            source: "pmbus".to_string(),
            ..dcentrald_autotuner::LivePowerEstimate::default()
        };
        assert_eq!(
            fleet_discovery_reported_power_watts(&board_only, &miner, &hardware),
            None
        );

        let modeled = dcentrald_autotuner::LivePowerEstimate {
            board_watts: 1_100.0,
            wall_watts: 1_300.0,
            source: "estimated".to_string(),
            ..dcentrald_autotuner::LivePowerEstimate::default()
        };
        assert_eq!(
            fleet_discovery_reported_power_watts(&modeled, &miner, &hardware),
            None
        );

        let measured = dcentrald_autotuner::LivePowerEstimate {
            board_watts: 1_100.0,
            wall_watts: 1_250.0,
            source: "pmbus".to_string(),
            ..dcentrald_autotuner::LivePowerEstimate::default()
        };
        assert_eq!(
            fleet_discovery_reported_power_watts(&measured, &miner, &hardware),
            Some(1_250.0)
        );
    }

    #[test]
    fn local_fleet_pool_stats_response_uses_sanitized_local_rollup() {
        let miner = crate::MinerState {
            hashrate_ghs: 12_345.6,
            hashrate_5s_ghs: 12_000.0,
            accepted: 9,
            rejected: 1,
            chains: Vec::new(),
            fans: crate::FanState {
                pwm: 30,
                rpm: 2_400,
                per_fan: Vec::new(),
            },
            pool: crate::PoolState {
                url: "stratum+tcp://user:secret@pool.example.com:3333".to_string(),
                worker: String::new(),
                status: "Alive".to_string(),
                difficulty: 4096.0,
                last_share_at: 0,
                protocol: "sv1".to_string(),
                encrypted: false,
                encrypted_source: crate::pool_quality_honest_default_source(),
                sv2_session: None,
                sv2_session_source: crate::pool_quality_honest_default_source(),
                sv2_custom_job: None,
                donating: false,
                donating_source: crate::pool_quality_honest_default_source(),
                donation_active_url: String::new(),
                donation_active_worker: String::new(),
                donation_pool_index: 0,
                share_efficiency: None,
                auto_fallback_active: false,
                auto_fallback_source: crate::pool_quality_honest_default_source(),
                auto_retry_sv2_after_s: None,
                auto_fallback_reason: None,
                failover: dcentrald_stratum::types::PoolFailoverStatus {
                    shares_unresolved: 2,
                    pending_submit_dropped: 3,
                    switch_count: 4,
                    ..dcentrald_stratum::types::PoolFailoverStatus::default()
                },
                failover_source: crate::pool_quality_honest_default_source(),
                hashrate_split: dcentrald_stratum::types::HashrateSplitStatus::default(),
                hashrate_split_source: crate::pool_quality_honest_default_source(),
                latency_ms: 0,
                latency_ms_source: crate::pool_quality_honest_default_source(),
                reject_reason_counts: [0; 6],
                reject_reason_counts_source: crate::pool_quality_honest_default_source(),
                rolling_acceptance_pct_30min: 100.0,
                rolling_acceptance_count_30min: (0, 0),
                rolling_acceptance_source: crate::pool_quality_honest_default_source(),
                worst_chip_hw_err_rate: None,
            },
            uptime_s: 600,
            firmware_version: "test".to_string(),
            mode: crate::OperatingMode::Standard,
        };
        let hardware = crate::HardwareInfo {
            chip_type: "BM1387".to_string(),
            ..crate::HardwareInfo::default()
        };

        let body = build_local_fleet_pool_stats_response(
            &miner,
            &hardware,
            "s9-lab".to_string(),
            "203.0.113.97".to_string(),
            1_777,
        );

        assert_eq!(body["status"].as_str(), Some("ok"));
        assert_eq!(body["source"].as_str(), Some("local_state"));
        assert_eq!(body["stats"]["miner_count"].as_u64(), Some(1));
        assert_eq!(body["stats"]["connected_miners"].as_u64(), Some(1));
        assert_eq!(body["stats"]["shares_submitted"].as_u64(), Some(10));
        assert_eq!(body["stats"]["shares_unresolved"].as_u64(), Some(2));
        assert_eq!(body["stats"]["pending_submit_dropped"].as_u64(), Some(3));
        assert_eq!(body["stats"]["failover_switches"].as_u64(), Some(4));
        assert_eq!(
            body["stats"]["miners"][0]["active_pool_url"].as_str(),
            Some("stratum+tcp://pool.example.com:3333")
        );
        assert_eq!(
            body["stats"]["pools"][0]["pool_url"].as_str(),
            Some("stratum+tcp://pool.example.com:3333")
        );
        assert!(body["limitations"][0]
            .as_str()
            .unwrap_or_default()
            .contains("does not scan LAN peers"));
    }

    #[test]
    fn local_fleet_status_reports_starting_before_dead() {
        let mut miner = crate::MinerState {
            hashrate_ghs: 0.0,
            hashrate_5s_ghs: 0.0,
            accepted: 0,
            rejected: 0,
            chains: Vec::new(),
            fans: crate::FanState {
                pwm: 10,
                rpm: 0,
                per_fan: Vec::new(),
            },
            pool: crate::PoolState {
                url: String::new(),
                worker: String::new(),
                status: "Connecting".to_string(),
                difficulty: 0.0,
                last_share_at: 0,
                protocol: "sv1".to_string(),
                encrypted: false,
                encrypted_source: crate::pool_quality_honest_default_source(),
                sv2_session: None,
                sv2_session_source: crate::pool_quality_honest_default_source(),
                sv2_custom_job: None,
                donating: false,
                donating_source: crate::pool_quality_honest_default_source(),
                donation_active_url: String::new(),
                donation_active_worker: String::new(),
                donation_pool_index: 0,
                share_efficiency: None,
                auto_fallback_active: false,
                auto_fallback_source: crate::pool_quality_honest_default_source(),
                auto_retry_sv2_after_s: None,
                auto_fallback_reason: None,
                failover: dcentrald_stratum::types::PoolFailoverStatus::default(),
                failover_source: crate::pool_quality_honest_default_source(),
                hashrate_split: dcentrald_stratum::types::HashrateSplitStatus::default(),
                hashrate_split_source: crate::pool_quality_honest_default_source(),
                latency_ms: 0,
                latency_ms_source: crate::pool_quality_honest_default_source(),
                reject_reason_counts: [0; 6],
                reject_reason_counts_source: crate::pool_quality_honest_default_source(),
                rolling_acceptance_pct_30min: 100.0,
                rolling_acceptance_count_30min: (0, 0),
                rolling_acceptance_source: crate::pool_quality_honest_default_source(),
                worst_chip_hw_err_rate: None,
            },
            uptime_s: 12,
            firmware_version: "test".to_string(),
            mode: crate::OperatingMode::Standard,
        };

        assert_eq!(fleet_status_for_miner(&miner), FleetMinerStatus::Starting);

        miner.uptime_s = 600;
        miner.pool.status = "Dead".to_string();
        assert_eq!(fleet_status_for_miner(&miner), FleetMinerStatus::Dead);
    }

    #[test]
    fn recent_share_event_response_uses_snake_case_difficulty_contract() {
        let body = recent_share_event_response_json(&crate::RecentShareEvent {
            timestamp_ms: 123_456,
            result: "accepted".to_string(),
            job_id: "job-7".to_string(),
            difficulty: Some(4096.0),
            target_difficulty: Some(2048.0),
            error_code: None,
            error_msg: None,
            worker_name: Some("worker.1".to_string()),
            nonce: Some("0000002a".to_string()),
            ntime: Some("66112233".to_string()),
            extranonce2: Some("00000001".to_string()),
            version_bits: Some("20000000".to_string()),
            version: Some(0x2000_0000),
            protocol_meta_present: true,
        });

        assert_eq!(body["timestamp_ms"].as_u64(), Some(123_456));
        assert_eq!(body["job_id"].as_str(), Some("job-7"));
        assert_eq!(body["difficulty"].as_f64(), Some(4096.0));
        assert_eq!(body["target_difficulty"].as_f64(), Some(2048.0));
        assert_eq!(body["protocol_meta_present"].as_bool(), Some(true));
        assert!(body.get("timestampMs").is_none());
        assert!(body.get("jobId").is_none());
        assert!(body.get("targetDifficulty").is_none());
    }

    #[test]
    fn firmware_update_alias_paths_are_covered() {
        for path in ["/api/v1/system/upgrade", "/api/v1/firmware/update"] {
            assert!(
                SIGNED_SYSUPGRADE_ALIAS_ROUTES.contains(&path),
                "{path} must stay wired to post_system_upgrade"
            );
        }
    }

    #[test]
    fn autotuner_resume_chain_summary_reports_fingerprint_and_last_known_good() {
        let state = dcentrald_autotuner::AutotunerResumeState {
            version: 1,
            saved_at_unix_s: 1234,
            fingerprint: dcentrald_autotuner::AutotunerHardwareFingerprint {
                platform: Some("am3-aml-s19k".to_string()),
                chains: vec![
                    dcentrald_autotuner::state_persistence::ChainHardwareFingerprint {
                        chain_id: 6,
                        chip_id: 0x1366,
                        chip_count: 77,
                        eeprom_serial: Some("BHB56902-TEST".to_string()),
                        eeprom_fingerprint: Some("i2c1-0x50:sha256:test".to_string()),
                        dspic_fw_byte: None,
                    },
                ],
            },
            chains: vec![dcentrald_autotuner::LastKnownGoodChainState {
                chain_id: 6,
                chip_count: 77,
                voltage_mv: 13_900,
                avg_freq_mhz: 670.0,
                estimated_power_w: 754.0,
                estimated_efficiency_jth: 18.0,
                chips: Vec::new(),
            }],
        };

        let body = autotuner_resume_chain_summary(Some(&state), 6);

        assert_eq!(body["available"].as_bool(), Some(true));
        assert_eq!(body["version"].as_u64(), Some(1));
        assert_eq!(
            body["fingerprint"]["eeprom_serial"].as_str(),
            Some("BHB56902-TEST")
        );
        assert_eq!(
            body["fingerprint"]["eeprom_fingerprint"].as_str(),
            Some("i2c1-0x50:sha256:test")
        );
        assert_eq!(
            body["last_known_good"]["avg_freq_mhz"].as_f64(),
            Some(670.0)
        );
        assert_eq!(
            body["last_known_good"]["chip_state_count"].as_u64(),
            Some(0)
        );
        assert!(body["last_known_good"].get("chips").is_none());
    }

    #[test]
    fn read_autotuner_resume_state_reports_missing_state_file() {
        let dir =
            std::env::temp_dir().join(format!("dcent_missing_resume_state_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);

        let (state, manifest) = read_autotuner_resume_state(dir.to_str().unwrap());

        assert!(state.is_none());
        assert_eq!(manifest["available"].as_bool(), Some(false));
        assert_eq!(manifest["present"].as_bool(), Some(false));
        assert_eq!(manifest["file"].as_str(), Some("state.toml"));
    }

    #[test]
    fn build_config_backup_manifest_response_reports_metadata_only_contract() {
        let body = build_config_backup_manifest_response();

        assert_eq!(body.status, "ok");
        assert!(body.read_only);
        assert!(!body.content_collected);
        // COMP-1: export/import now exist on dedicated endpoints, so the manifest
        // truthfully advertises the capability.
        assert!(body.restore_supported);
        assert!(body.daemon_config_export_supported);
        assert!(body.dashboard_preferences_export_supported);
        assert!(!body.redaction_policy.content_included);
        assert!(body
            .redaction_policy
            .secret_key_patterns
            .contains(&"password"));
        assert!(body
            .sources
            .iter()
            .any(|source| source.id == "persistent-config"
                && source.path == "/data/dcentrald.toml"
                && source.writable_target));
        assert!(body
            .sources
            .iter()
            .any(|source| source.id == "factory-default-config"
                && source.path == "/etc/dcentrald.toml"));
        assert!(body
            .limitations
            .iter()
            .any(|item| item.contains("does not return")));
    }

    // SW-13 (config-loss hardening): an older / extra-key config must survive a
    // load (parse → migrate) → save (re-serialize) round-trip with ZERO field
    // loss. This is the regression that proves a firmware upgrade can't silently
    // drop operator config (renamed/unknown sections + a stale schema_version).
    #[test]
    fn config_schema_migration_preserves_unknown_and_old_keys_round_trip() {
        // A config written by a HYPOTHETICAL older/other build: an old
        // schema_version, a key this build doesn't know in a known section, AND
        // an entire section this build never reads.
        let toml_src = r#"
            [general]
            schema_version = 0
            hostname = "operator-rig"
            legacy_only_key = "keep-me"

            [pool]
            url = "stratum+tcp://public-pool.io:21496"
            worker = "bc1qexamplewallet.worker1"

            [unknown_future_section]
            some_new_knob = 42
            nested = { deep = "also-keep-me" }
        "#;
        let mut table: toml::Table = toml::from_str(toml_src).expect("valid toml");

        let preserved = migrate_config_schema(&mut table);
        // general + pool + unknown_future_section.
        assert_eq!(preserved, 3, "no top-level section may be dropped");

        // The version got stamped up to current (older 0 -> CONFIG_SCHEMA_VERSION).
        let general = table.get("general").and_then(|v| v.as_table()).unwrap();
        assert_eq!(
            general.get("schema_version").and_then(|v| v.as_integer()),
            Some(i64::from(CONFIG_SCHEMA_VERSION))
        );
        // An unknown key inside a KNOWN section is preserved.
        assert_eq!(
            general.get("legacy_only_key").and_then(|v| v.as_str()),
            Some("keep-me")
        );
        // A known key is untouched.
        assert_eq!(
            general.get("hostname").and_then(|v| v.as_str()),
            Some("operator-rig")
        );

        // An entirely-unknown section is preserved, including nested values.
        let unknown = table
            .get("unknown_future_section")
            .and_then(|v| v.as_table())
            .expect("unknown section preserved");
        assert_eq!(
            unknown.get("some_new_knob").and_then(|v| v.as_integer()),
            Some(42)
        );
        assert_eq!(
            unknown
                .get("nested")
                .and_then(|v| v.as_table())
                .and_then(|n| n.get("deep"))
                .and_then(|v| v.as_str()),
            Some("also-keep-me")
        );

        // Re-serialize (the save step) — every original value must still be present.
        let serialized = toml::to_string(&table).expect("serialize");
        assert!(serialized.contains("legacy_only_key"));
        assert!(serialized.contains("keep-me"));
        assert!(serialized.contains("unknown_future_section"));
        assert!(serialized.contains("some_new_knob"));
        assert!(serialized.contains("also-keep-me"));
        assert!(serialized.contains("operator-rig"));
        assert!(serialized.contains("stratum+tcp://public-pool.io:21496"));

        // Round-trip back through the migrator: now self-describing, idempotent,
        // still no field loss.
        let mut reparsed: toml::Table = toml::from_str(&serialized).expect("re-parse");
        let preserved_again = migrate_config_schema(&mut reparsed);
        assert_eq!(preserved_again, 3);
        assert_eq!(
            reparsed
                .get("general")
                .and_then(|v| v.as_table())
                .and_then(|g| g.get("schema_version"))
                .and_then(|v| v.as_integer()),
            Some(i64::from(CONFIG_SCHEMA_VERSION))
        );
    }

    // SW-13: an UNVERSIONED config (no [general].schema_version at all — every
    // pre-SW-13 dcentrald.toml) must be stamped without losing its keys, and a
    // config with NO [general] section at all must gain one only for the stamp.
    #[test]
    fn config_schema_migration_stamps_unversioned_and_missing_general() {
        // Case 1: has [general] but no schema_version.
        let mut t1: toml::Table = toml::from_str(
            r#"
                [general]
                hostname = "rig-a"

                [mining]
                enabled = true
            "#,
        )
        .unwrap();
        migrate_config_schema(&mut t1);
        let g1 = t1.get("general").and_then(|v| v.as_table()).unwrap();
        assert_eq!(g1.get("hostname").and_then(|v| v.as_str()), Some("rig-a"));
        assert_eq!(
            g1.get("schema_version").and_then(|v| v.as_integer()),
            Some(i64::from(CONFIG_SCHEMA_VERSION))
        );
        // The unrelated [mining] section is untouched.
        assert_eq!(
            t1.get("mining")
                .and_then(|v| v.as_table())
                .and_then(|m| m.get("enabled"))
                .and_then(|v| v.as_bool()),
            Some(true)
        );

        // Case 2: no [general] section at all -> one is created solely for the stamp.
        let mut t2: toml::Table = toml::from_str(
            r#"
                [pool]
                url = "stratum+tcp://example:3333"
            "#,
        )
        .unwrap();
        migrate_config_schema(&mut t2);
        assert_eq!(
            t2.get("general")
                .and_then(|v| v.as_table())
                .and_then(|g| g.get("schema_version"))
                .and_then(|v| v.as_integer()),
            Some(i64::from(CONFIG_SCHEMA_VERSION))
        );
        // pool survived.
        assert!(t2.get("pool").is_some());
    }

    // SW-13: a config written by a NEWER firmware build (higher schema_version)
    // must NOT be down-stamped to ours, or a later re-upgrade would treat the
    // file as stale. All keys still preserved.
    #[test]
    fn config_schema_migration_does_not_downgrade_newer_schema() {
        let future = i64::from(CONFIG_SCHEMA_VERSION) + 5;
        let mut table: toml::Table = toml::from_str(&format!(
            r#"
                [general]
                schema_version = {future}
                hostname = "future-rig"
                brand_new_knob = "keep"
            "#
        ))
        .unwrap();

        migrate_config_schema(&mut table);

        let general = table.get("general").and_then(|v| v.as_table()).unwrap();
        // Higher version left intact.
        assert_eq!(
            general.get("schema_version").and_then(|v| v.as_integer()),
            Some(future)
        );
        // The newer key is preserved across the round-trip.
        assert_eq!(
            general.get("brand_new_knob").and_then(|v| v.as_str()),
            Some("keep")
        );
        assert_eq!(
            general.get("hostname").and_then(|v| v.as_str()),
            Some("future-rig")
        );
    }

    #[test]
    fn ensure_toml_value_table_section_creates_missing_section() {
        let mut doc = toml::Value::Table(toml::Table::new());

        {
            let section = ensure_toml_value_table_section(&mut doc, "power").unwrap();
            section.insert("target_watts".into(), toml::Value::Integer(1_200));
        }

        assert_eq!(
            doc.get("power")
                .and_then(|value| value.as_table())
                .and_then(|section| section.get("target_watts"))
                .and_then(|value| value.as_integer()),
            Some(1_200)
        );
    }

    #[test]
    fn ensure_toml_value_table_section_rejects_non_table_shapes() {
        let mut scalar_root = toml::Value::String("bad-root".to_string());
        assert_eq!(
            ensure_toml_value_table_section(&mut scalar_root, "power").unwrap_err(),
            "Config document root is not a TOML table"
        );

        let mut scalar_section: toml::Value = toml::from_str("power = \"bad-section\"").unwrap();
        assert_eq!(
            ensure_toml_value_table_section(&mut scalar_section, "power").unwrap_err(),
            "[power] is not a TOML table"
        );
    }

    // DEVOPS-009: the structured TOML secret redactor masks pool.password +
    // every other secret-bearing key recursively before any export/bundle.
    #[test]
    fn redact_password_masks_nonempty_keeps_empty() {
        assert_eq!(redact_password(""), "");
        assert_eq!(redact_password("hunter2"), SECRET_REDACTION_PLACEHOLDER);
        assert_eq!(redact_password("x"), SECRET_REDACTION_PLACEHOLDER);
    }

    #[test]
    fn debug_log_scrub_masks_wallets_and_secret_key_values() {
        let wallet = "bc1q04lzwddzgmtjex6jlsv2fwhe4se4jxje6rhzp6";

        let wallet_line = scrub_debug_log_line(&format!("authorized worker={wallet}"));
        assert!(
            !wallet_line.contains(wallet),
            "debug log wallet leaked: {wallet_line}"
        );

        let password_line = scrub_debug_log_line("pool.password = supersecret");
        assert!(password_line.contains(SECRET_REDACTION_PLACEHOLDER));
        assert!(
            !password_line.contains("supersecret"),
            "debug log password leaked: {password_line}"
        );

        let webhook_line =
            scrub_debug_log_line(r#"{"webhook.url":"https://hooks.slack.com/services/TOKEN"}"#);
        assert!(webhook_line.contains(SECRET_REDACTION_PLACEHOLDER));
        assert!(
            !webhook_line.contains("TOKEN"),
            "debug log webhook token leaked: {webhook_line}"
        );
    }

    // W20 SEC C-1: the MQTT config response must never echo the stored broker
    // password in cleartext (set => placeholder, unset => empty).
    #[test]
    fn mqtt_config_response_masks_password() {
        let mut cfg = MqttConfigPayload::default();
        cfg.broker = "mqtt://broker.local:1883".into();
        cfg.password = "supersecret".into();
        let resp = mqtt_config_response(cfg);
        assert_eq!(resp.password, SECRET_REDACTION_PLACEHOLDER);
        // broker (non-secret) is preserved verbatim.
        assert_eq!(resp.broker, "mqtt://broker.local:1883");

        let mut unset = MqttConfigPayload::default();
        unset.password = String::new();
        assert_eq!(mqtt_config_response(unset).password, "");
    }

    // MQTT-1 (2026-06-20): a dashboard "Test connection" round-trip re-POSTs the
    // masked password placeholder when the operator didn't change it. The broker
    // test must resolve that placeholder back to the stored secret, not test the
    // literal "<redacted>" string.
    #[test]
    fn resolve_mqtt_test_password_restores_kept_secret() {
        let table: toml::Table = toml::from_str(
            "[mqtt]\nbroker = \"mqtt://h:1883\"\nusername = \"u\"\npassword = \"realsecret\"\n",
        )
        .expect("valid toml");

        // Placeholder round-trip resolves to the real stored secret.
        assert_eq!(
            resolve_mqtt_test_password(SECRET_REDACTION_PLACEHOLDER, &table),
            "realsecret"
        );
        // A genuinely changed password is used verbatim (not resolved away).
        assert_eq!(resolve_mqtt_test_password("newpass", &table), "newpass");
        // An explicitly empty password (anonymous broker) is used verbatim.
        assert_eq!(resolve_mqtt_test_password("", &table), "");
        // No stored secret -> placeholder resolves to empty (no credentials),
        // never the literal mask.
        let empty = toml::Table::new();
        assert_eq!(
            resolve_mqtt_test_password(SECRET_REDACTION_PLACEHOLDER, &empty),
            ""
        );
    }

    // TEL-3 (2026-06-20): worst_chip_hw_err_rate is a RESERVED / always-null
    // field on the /api/status + per-pool surfaces. The live state-publisher
    // path (apply_quality_snapshot) must NOT populate it, so the JSON stays
    // honestly `null` rather than advertising live worst-chip telemetry that
    // flows only on the separate autotuner chip-health channel.
    #[test]
    fn worst_chip_hw_err_rate_is_reserved_null_not_wired_by_publisher() {
        // Minimal PoolState via serde defaults (only the 4 non-default fields).
        let mut pool: crate::PoolState = serde_json::from_value(serde_json::json!({
            "url": "stratum+tcp://example.invalid:3333",
            "status": "Alive",
            "difficulty": 1.0,
            "last_share_at": 0,
        }))
        .expect("minimal PoolState deserializes via serde defaults");
        assert_eq!(pool.worst_chip_hw_err_rate, None);

        // Applying the live pool-quality snapshot must leave the field untouched.
        let snap = dcentrald_stratum::pool_quality::PoolQualitySnapshot::default();
        pool.apply_quality_snapshot(&snap);
        assert_eq!(
            pool.worst_chip_hw_err_rate, None,
            "TEL-3: apply_quality_snapshot must not populate the reserved field"
        );

        // It serializes as JSON null — the honest /api/status contract.
        let v = serde_json::to_value(&pool).expect("PoolState serializes");
        assert!(
            v["worst_chip_hw_err_rate"].is_null(),
            "worst_chip_hw_err_rate must serialize as null, got {:?}",
            v["worst_chip_hw_err_rate"]
        );
    }

    // P2-10: /api/mqtt/status reports CONFIGURED posture and never claims a live
    // broker connection (the daemon tracks no in-process socket).
    #[test]
    fn mqtt_status_payload_reports_posture_not_a_fake_connection() {
        let enabled: toml::Table = toml::from_str(
            "[mqtt]\nenabled = true\nbroker = \"mqtt://h:1883\"\ndiscovery = true\n",
        )
        .expect("valid toml");
        let v = mqtt_status_payload(&enabled);
        assert_eq!(v["enabled"], serde_json::json!(true));
        assert_eq!(v["command_subscriber_enabled"], serde_json::json!(true));
        assert_eq!(v["integration_up"], serde_json::json!(true));
        // Truth contract: NEVER advertise a connection we don't track.
        assert_eq!(v["live_connection_tracked"], serde_json::json!(false));

        // Not configured -> disabled, integration_up null (omitted gauge).
        let v2 = mqtt_status_payload(&toml::Table::new());
        assert_eq!(v2["enabled"], serde_json::json!(false));
        assert_eq!(v2["integration_up"], serde_json::Value::Null);
        assert_eq!(v2["live_connection_tracked"], serde_json::json!(false));
    }

    // W20 SEC C-2: the webhook config response must never echo the stored
    // URL (it carries the delivery secret) in cleartext.
    #[test]
    fn webhook_config_response_masks_url() {
        let resp = webhook_config_response(WebhookConfigPayload {
            enabled: true,
            url: "https://hooks.slack.com/services/T000/B000/XXXXSECRET".into(),
            events: vec![],
            ..Default::default()
        });
        assert_eq!(resp.url, SECRET_REDACTION_PLACEHOLDER);

        let unset = webhook_config_response(WebhookConfigPayload {
            enabled: false,
            url: String::new(),
            events: vec![],
            ..Default::default()
        });
        assert_eq!(unset.url, "");
    }

    // W-notify: the Telegram bot token is a delivery secret and must be masked
    // in the GET response exactly like the webhook URL; the chat id is not a
    // secret and is echoed verbatim.
    #[test]
    fn webhook_config_response_masks_telegram_bot_token_not_chat_id() {
        let resp = webhook_config_response(WebhookConfigPayload {
            enabled: true,
            format: crate::webhook::WebhookFormat::Telegram,
            telegram_bot_token: "123456:ABCDEF-secret".into(),
            telegram_chat_id: "987654321".into(),
            ..Default::default()
        });
        assert_eq!(resp.telegram_bot_token, SECRET_REDACTION_PLACEHOLDER);
        assert_eq!(resp.telegram_chat_id, "987654321");
        assert_eq!(resp.format, crate::webhook::WebhookFormat::Telegram);

        // Unset token stays empty (must not masquerade as set).
        let unset = webhook_config_response(WebhookConfigPayload {
            enabled: false,
            ..Default::default()
        });
        assert_eq!(unset.telegram_bot_token, "");
    }

    // W-notify: `webhook.telegram_bot_token` must be classified secret so the
    // config-backup / support-bundle redactor masks it everywhere.
    #[test]
    fn telegram_bot_token_key_is_secret() {
        assert!(key_is_secret("webhook.telegram_bot_token"));
        assert!(key_is_secret("telegram_bot_token"));
        // The chat id is addressing data, not a secret.
        assert!(!key_is_secret("telegram_chat_id"));
    }

    // W-notify: format-aware validation — Telegram requires token + chat id
    // (not a URL); Generic/Discord/Slack require a URL.
    #[test]
    fn validate_webhook_config_is_format_aware() {
        // Telegram enabled but missing token/chat id => rejected.
        let mut tg = WebhookConfigPayload {
            enabled: true,
            format: crate::webhook::WebhookFormat::Telegram,
            ..Default::default()
        };
        assert!(validate_webhook_config(&tg).is_err());
        tg.telegram_bot_token = "TOK".into();
        tg.telegram_chat_id = "123".into();
        assert!(validate_webhook_config(&tg).is_ok());

        // Discord enabled but no URL => rejected (URL-based delivery).
        let discord = WebhookConfigPayload {
            enabled: true,
            format: crate::webhook::WebhookFormat::Discord,
            ..Default::default()
        };
        assert!(validate_webhook_config(&discord).is_err());

        // Disabled is always fine regardless of format.
        let off = WebhookConfigPayload {
            enabled: false,
            format: crate::webhook::WebhookFormat::Telegram,
            ..Default::default()
        };
        assert!(validate_webhook_config(&off).is_ok());
    }

    // W-notify: the synthetic test payload reshapes per channel — Generic keeps
    // the historical envelope; Telegram targets the Bot API endpoint.
    #[test]
    fn webhook_test_payload_reshapes_per_format() {
        let (gurl, gbody) = webhook_test_payload(
            crate::webhook::WebhookFormat::Generic,
            "rig-01",
            "https://example.com/hook",
            "",
            "",
        );
        assert_eq!(gurl, "https://example.com/hook");
        assert_eq!(gbody["alert"]["event"], "mining_stopped");
        assert_eq!(gbody["alert"]["data"]["reason"], "dashboard_test");

        let (turl, tbody) = webhook_test_payload(
            crate::webhook::WebhookFormat::Telegram,
            "rig-01",
            "",
            "BOTTOKEN",
            "987654",
        );
        assert_eq!(turl, "https://api.telegram.org/botBOTTOKEN/sendMessage");
        assert_eq!(tbody["chat_id"], "987654");
        assert!(tbody["text"].as_str().unwrap().contains("dashboard_test"));
    }

    // W-notify: format + telegram_chat_id survive a config table round-trip;
    // a missing [webhook] section defaults to generic with empty telegram fields.
    #[test]
    fn read_webhook_config_round_trips_format_and_chat_id() {
        let mut table = toml::Table::new();
        let mut webhook = toml::Table::new();
        webhook.insert("enabled".into(), toml::Value::Boolean(true));
        webhook.insert("format".into(), toml::Value::String("telegram".into()));
        webhook.insert(
            "telegram_bot_token".into(),
            toml::Value::String("TOK".into()),
        );
        webhook.insert("telegram_chat_id".into(), toml::Value::String("42".into()));
        table.insert("webhook".into(), toml::Value::Table(webhook));

        let cfg = read_webhook_config(&table);
        assert_eq!(cfg.format, crate::webhook::WebhookFormat::Telegram);
        assert_eq!(cfg.telegram_bot_token, "TOK");
        assert_eq!(cfg.telegram_chat_id, "42");

        // Missing section => default-OFF generic.
        let empty = read_webhook_config(&toml::Table::new());
        assert_eq!(empty.format, crate::webhook::WebhookFormat::Generic);
        assert_eq!(empty.telegram_bot_token, "");
    }

    // W20 SEC C-3: inline stratum credentials must be stripped from the
    // status/display surfaces (the helper the display paths now call).
    #[test]
    fn pool_url_sanitizer_strips_inline_credentials() {
        let dirty = "stratum+tcp://worker.1:poolpass@pool.example.com:3333";
        let clean = dcentrald_stratum::pool_api::sanitize_pool_url(dirty);
        assert!(!clean.contains("poolpass"), "creds leaked: {clean}");
        assert!(
            clean.contains("pool.example.com:3333"),
            "host lost: {clean}"
        );
    }

    #[test]
    fn key_is_secret_matches_canonical_patterns() {
        // Bare + dotted + substring patterns.
        assert!(key_is_secret("password"));
        assert!(key_is_secret("pool.password"));
        assert!(key_is_secret("fallback_password"));
        assert!(key_is_secret("api_token"));
        assert!(key_is_secret("private_key"));
        assert!(key_is_secret("client_secret"));
        // Dotted-only pattern (webhook.url carries embedded secrets).
        assert!(key_is_secret("webhook.url"));
        // Non-secrets stay visible — bare "url" is NOT secret (only the dotted
        // webhook.url is), so pool.url / stratum URLs are preserved.
        assert!(!key_is_secret("url"));
        assert!(!key_is_secret("worker")); // worker is redacted via redact_worker, not key_is_secret
        assert!(!key_is_secret("frequency_mhz"));
        assert!(!key_is_secret("enabled"));
    }

    #[test]
    fn redact_secrets_in_toml_table_redacts_dotted_webhook_url_but_not_pool_url() {
        let toml_src = r#"
            [pool]
            url = "stratum+tcp://public-pool.io:21496"

            [webhook]
            url = "https://hooks.example.com/services/T00/B00/secrettoken"
        "#;
        let mut table: toml::Table = toml::from_str(toml_src).expect("valid toml");
        redact_secrets_in_toml_table(&mut table);

        // pool.url is preserved (a stratum URL is not a secret).
        let pool = table.get("pool").and_then(|v| v.as_table()).unwrap();
        assert_eq!(
            pool.get("url").and_then(|v| v.as_str()),
            Some("stratum+tcp://public-pool.io:21496")
        );
        // webhook.url is redacted (the path can carry an embedded token).
        let webhook = table.get("webhook").and_then(|v| v.as_table()).unwrap();
        assert_eq!(
            webhook.get("url").and_then(|v| v.as_str()),
            Some(SECRET_REDACTION_PLACEHOLDER)
        );
    }

    #[test]
    fn redact_secrets_in_toml_table_masks_pool_password_recursively() {
        let toml_src = r#"
            [pool]
            url = "stratum+tcp://public-pool.io:21496"
            worker = "bc1qexamplewallet.worker1"
            password = "supersecret"

            [donation]
            enabled = true
            password = "x"
            fallback_password = "y"

            [mqtt]
            password = ""

            [[pools]]
            url = "stratum+tcp://backup:3333"
            password = "backuppass"
        "#;
        let mut table: toml::Table = toml::from_str(toml_src).expect("valid toml");
        redact_secrets_in_toml_table(&mut table);

        let pool = table.get("pool").and_then(|v| v.as_table()).unwrap();
        // pool.password masked; non-secret keys untouched.
        assert_eq!(
            pool.get("password").and_then(|v| v.as_str()),
            Some(SECRET_REDACTION_PLACEHOLDER)
        );
        assert_eq!(
            pool.get("url").and_then(|v| v.as_str()),
            Some("stratum+tcp://public-pool.io:21496")
        );
        assert_eq!(
            pool.get("worker").and_then(|v| v.as_str()),
            Some("bc1qexamplewallet.worker1")
        );

        // Nested donation secrets masked.
        let donation = table.get("donation").and_then(|v| v.as_table()).unwrap();
        assert_eq!(
            donation.get("password").and_then(|v| v.as_str()),
            Some(SECRET_REDACTION_PLACEHOLDER)
        );
        assert_eq!(
            donation.get("fallback_password").and_then(|v| v.as_str()),
            Some(SECRET_REDACTION_PLACEHOLDER)
        );
        assert_eq!(
            donation.get("enabled").and_then(|v| v.as_bool()),
            Some(true)
        );

        // Empty secret stays empty (an unset password must not look set).
        let mqtt = table.get("mqtt").and_then(|v| v.as_table()).unwrap();
        assert_eq!(mqtt.get("password").and_then(|v| v.as_str()), Some(""));

        // Array-of-tables ([[pools]]) entries are walked.
        let pools = table.get("pools").and_then(|v| v.as_array()).unwrap();
        let first = pools[0].as_table().unwrap();
        assert_eq!(
            first.get("password").and_then(|v| v.as_str()),
            Some(SECRET_REDACTION_PLACEHOLDER)
        );

        // Re-serializing must not contain any plaintext secret value.
        let serialized = toml::to_string(&table).expect("serialize");
        assert!(!serialized.contains("supersecret"));
        assert!(!serialized.contains("backuppass"));
        assert!(!serialized.contains("\"x\""));
        assert!(!serialized.contains("\"y\""));
    }

    // ─── COMP-1: config export / import (LuxOS/Braiins parity) ──────────────

    // The export redactor must mask EVERY secret-bearing value plus wallet
    // workers and credential-bearing pool URLs — and leave clean URLs verbatim
    // so the export round-trips. No raw password/token/bech32/inline-creds may
    // survive serialization.
    // FIX 1 negative: inject credentials into sv2_url / donation.pool_url /
    // donation.fallback_pool_url / bitcoind_rpc_url / mqtt.broker /
    // cgminer_scrape_url, and wallets into pool.worker / donation.fallback_worker /
    // job_declaration.coinbase_output_address. NONE may survive serialization (no
    // raw bech32, no inline `:pass@`); a clean pool url is kept verbatim.
    #[test]
    fn config_export_redacts_every_secret_wallet_and_credential_url() {
        let toml_src = r#"
            [general]
            hostname = "rig-1"

            [pool]
            url = "stratum+tcp://public-pool.io:21496"
            sv2_url = "sv2://sv2user:sv2pass@sv2.example.com:3334"
            worker = "bc1qexamplewalletaddress0000000000000000000.rig1"
            password = "poolsupersecret"

            [api]
            token = "api-supersecret-token"

            [mqtt]
            broker = "mqtt://mqttuser:mqttsecretpw@broker.local:1883"
            password = "mqttsupersecret"

            [webhook]
            url = "https://hooks.example.com/services/T00/B00/secrettoken"

            [donation]
            enabled = true
            pool_url = "stratum+tcp://donuser:donsecretpw@don.example.com:3333"
            fallback_pool_url = "stratum+tcp://fbuser:fbsecretpw@fb.example.com:3333"
            worker = "bc1qdonationwalletaddr1111111111111111111.don"
            fallback_worker = "bc1qfallbackwalletaddr2222222222222222.fb"

            [job_declaration]
            bitcoind_rpc_url = "http://rpcuser:rpcsecretpw@127.0.0.1:8332"
            coinbase_output_address = "bc1qcoinbaseoutputaddr3333333333333333.cb"

            [stratum_proxy]
            cgminer_scrape_url = "http://scrapeuser:scrapesecretpw@127.0.0.1:4028"
        "#;
        let mut table: toml::Table = toml::from_str(toml_src).expect("valid toml");

        redact_config_table_for_export(&mut table);
        let serialized = toml::to_string(&table).expect("serialize");

        // No raw secret value survives serialization.
        for leaked in [
            "poolsupersecret",
            "api-supersecret-token",
            "mqttsupersecret",
            "secrettoken",
        ] {
            assert!(
                !serialized.contains(leaked),
                "secret leaked ({leaked}): {serialized}"
            );
        }
        // No inline URL credential survives — neither the password halves nor the
        // tell-tale `pass@` joiner of a `user:pass@host` authority.
        for cred in [
            "sv2pass",
            "mqttsecretpw",
            "donsecretpw",
            "fbsecretpw",
            "rpcsecretpw",
            "scrapesecretpw",
        ] {
            assert!(
                !serialized.contains(cred),
                "url credential leaked ({cred}): {serialized}"
            );
        }
        assert!(
            !serialized.contains("pass@"),
            "inline creds leaked: {serialized}"
        );
        // No raw wallet / payout address (bech32) survives.
        for wallet in [
            "bc1qexamplewalletaddress",
            "bc1qdonationwalletaddr",
            "bc1qfallbackwalletaddr",
            "bc1qcoinbaseoutputaddr",
        ] {
            assert!(
                !serialized.contains(wallet),
                "wallet leaked ({wallet}): {serialized}"
            );
        }

        // Wallet/payout keys become the placeholder (re-importable keep-existing).
        let pool = table.get("pool").and_then(|v| v.as_table()).unwrap();
        assert_eq!(
            pool.get("worker").and_then(|v| v.as_str()),
            Some(SECRET_REDACTION_PLACEHOLDER)
        );
        let donation = table.get("donation").and_then(|v| v.as_table()).unwrap();
        assert_eq!(
            donation.get("fallback_worker").and_then(|v| v.as_str()),
            Some(SECRET_REDACTION_PLACEHOLDER)
        );
        let jd = table
            .get("job_declaration")
            .and_then(|v| v.as_table())
            .unwrap();
        assert_eq!(
            jd.get("coinbase_output_address").and_then(|v| v.as_str()),
            Some(SECRET_REDACTION_PLACEHOLDER)
        );

        // Credential URLs keep ONLY their clean host (creds stripped, NOT masked).
        assert_eq!(
            pool.get("sv2_url").and_then(|v| v.as_str()),
            Some("sv2://sv2.example.com:3334")
        );
        assert_eq!(
            donation.get("pool_url").and_then(|v| v.as_str()),
            Some("stratum+tcp://don.example.com:3333")
        );
        assert_eq!(
            donation.get("fallback_pool_url").and_then(|v| v.as_str()),
            Some("stratum+tcp://fb.example.com:3333")
        );
        assert_eq!(
            jd.get("bitcoind_rpc_url").and_then(|v| v.as_str()),
            Some("http://127.0.0.1:8332")
        );
        let proxy = table
            .get("stratum_proxy")
            .and_then(|v| v.as_table())
            .unwrap();
        assert_eq!(
            proxy.get("cgminer_scrape_url").and_then(|v| v.as_str()),
            Some("http://127.0.0.1:4028")
        );
        let mqtt = table.get("mqtt").and_then(|v| v.as_table()).unwrap();
        assert_eq!(
            mqtt.get("broker").and_then(|v| v.as_str()),
            Some("mqtt://broker.local:1883")
        );

        // A clean (no-credential) pool url round-trips VERBATIM.
        assert_eq!(
            pool.get("url").and_then(|v| v.as_str()),
            Some("stratum+tcp://public-pool.io:21496")
        );
        // Non-secret field untouched.
        let general = table.get("general").and_then(|v| v.as_table()).unwrap();
        assert_eq!(
            general.get("hostname").and_then(|v| v.as_str()),
            Some("rig-1")
        );
    }

    // A clean (no-credential) pool URL must NOT be redacted, so the export
    // round-trips losslessly for non-secret fields.
    #[test]
    fn config_export_keeps_clean_pool_url_verbatim() {
        let mut table: toml::Table = toml::from_str(
            "[pool]\nurl = \"stratum+tcp://public-pool.io:21496\"\nworker = \"bc1qcleanwalletaddress2222222222222222222.w\"\n",
        )
        .expect("valid toml");
        redact_config_table_for_export(&mut table);
        let pool = table.get("pool").and_then(|v| v.as_table()).unwrap();
        assert_eq!(
            pool.get("url").and_then(|v| v.as_str()),
            Some("stratum+tcp://public-pool.io:21496")
        );
        // worker still redacted.
        assert_eq!(
            pool.get("worker").and_then(|v| v.as_str()),
            Some(SECRET_REDACTION_PLACEHOLDER)
        );
    }

    // Import validation must REJECT invalid configs (fail-closed) and ACCEPT a
    // valid one. Mirrors DcentraldConfig::validate()'s platform-independent
    // checks.
    #[test]
    fn config_import_validation_rejects_invalid_and_accepts_valid() {
        // thermal ordering inverted.
        let bad_thermal: toml::Table = toml::from_str(
            "[thermal]\ntarget_temp_c = 70\nhot_temp_c = 65\ndangerous_temp_c = 75\n",
        )
        .unwrap();
        assert!(validate_imported_config_table(&bad_thermal).is_err());

        // residential ceiling exceeded.
        let bad_ceiling: toml::Table =
            toml::from_str("[thermal]\ndangerous_temp_c = 95\n").unwrap();
        assert!(validate_imported_config_table(&bad_ceiling).is_err());

        // donation percent out of range.
        let bad_donation: toml::Table = toml::from_str("[donation]\npercent = 9.0\n").unwrap();
        assert!(validate_imported_config_table(&bad_donation).is_err());

        // power target exceeds max.
        let bad_power: toml::Table =
            toml::from_str("[power]\ntarget_watts = 5000\nmax_watts = 3000\n").unwrap();
        assert!(validate_imported_config_table(&bad_power).is_err());

        // mining voltage out of envelope.
        let bad_voltage: toml::Table = toml::from_str("[mining]\nvoltage_mv = 30000\n").unwrap();
        assert!(validate_imported_config_table(&bad_voltage).is_err());

        // serial_chip_count = 0 (divide-by-zero panic guard).
        let bad_chips: toml::Table = toml::from_str("[mining]\nserial_chip_count = 0\n").unwrap();
        assert!(validate_imported_config_table(&bad_chips).is_err());

        // FIX 3: voltage inside the generic 5000-20000 envelope but above the
        // 14500 mV am2 chip-rail ceiling is rejected (import can't know platform).
        let bad_am2_voltage: toml::Table =
            toml::from_str("[mining]\nvoltage_mv = 15000\n").unwrap();
        assert!(validate_imported_config_table(&bad_am2_voltage).is_err());

        // FIX 3: thermal.pid_interval_s must be finite and in (0, 60].
        let bad_pid_zero: toml::Table = toml::from_str("[thermal]\npid_interval_s = 0\n").unwrap();
        assert!(validate_imported_config_table(&bad_pid_zero).is_err());
        let bad_pid_big: toml::Table = toml::from_str("[thermal]\npid_interval_s = 120\n").unwrap();
        assert!(validate_imported_config_table(&bad_pid_big).is_err());

        // FIX 3: watchdog kick must be > 0 and < timeout when enabled.
        let bad_kick_zero: toml::Table =
            toml::from_str("[watchdog]\nenabled = true\nkick_interval_s = 0\ntimeout_s = 30\n")
                .unwrap();
        assert!(validate_imported_config_table(&bad_kick_zero).is_err());
        let bad_kick_ge: toml::Table =
            toml::from_str("[watchdog]\nenabled = true\nkick_interval_s = 30\ntimeout_s = 30\n")
                .unwrap();
        assert!(validate_imported_config_table(&bad_kick_ge).is_err());

        // FIX 3: unknown mode.active is rejected (silently coerced to Standard).
        let bad_mode: toml::Table = toml::from_str("[mode]\nactive = \"bogus\"\n").unwrap();
        assert!(validate_imported_config_table(&bad_mode).is_err());

        // A sane full-ish config — exercising the new valid paths — passes.
        let good: toml::Table = toml::from_str(
            "[general]\nhostname = \"rig\"\n\
             [thermal]\ntarget_temp_c = 55\nhot_temp_c = 65\ndangerous_temp_c = 75\npid_interval_s = 2.0\n\
             [donation]\npercent = 2.0\ncycle_duration_s = 3600\n\
             [power]\ntarget_watts = 1200\nmax_watts = 3500\n\
             [mining]\nvoltage_mv = 9100\nfrequency_mhz = 650\nserial_chip_count = 63\n\
             [watchdog]\nenabled = true\nkick_interval_s = 5\ntimeout_s = 30\n\
             [mode]\nactive = \"home\"\n",
        )
        .unwrap();
        assert!(validate_imported_config_table(&good).is_ok());
    }

    // R-02 regression: post_config validates the MERGED config, so a partial update
    // that is individually parseable but makes the merged config invalid must be
    // rejected — otherwise it silently demotes the miner to management-only at the
    // next restart. Exercises the real merge + validate path.
    #[test]
    fn merged_partial_update_that_inverts_thermal_ordering_is_rejected() {
        let mut base: toml::Table = toml::from_str(
            "[thermal]\ntarget_temp_c = 55\nhot_temp_c = 65\ndangerous_temp_c = 70\n",
        )
        .unwrap();
        // A partial POST that only touches target_temp_c — individually fine, but
        // makes the merged config target(99) >= hot(65).
        let update: toml::Table = toml::from_str("[thermal]\ntarget_temp_c = 99\n").unwrap();
        for (key, value) in update {
            if let Some(existing) = base.get_mut(&key) {
                merge_toml_value(existing, value);
            } else {
                base.insert(key, value);
            }
        }
        assert!(
            validate_imported_config_table(&base).is_err(),
            "merged config with target >= hot must be rejected"
        );
    }

    #[test]
    fn shared_config_rejects_out_of_range_frequency() {
        let patch: SharedConfigPatch = serde_json::from_value(serde_json::json!({
            "mining": {
                "frequencyMhz": 5000.0
            }
        }))
        .expect("valid shared config patch shape");

        let err = merge_shared_config_patch_table(toml::Table::new(), patch)
            .expect_err("shared config must reject mining frequency outside boot envelope");
        assert!(
            err.contains("mining.frequency_mhz"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn shared_config_rejects_inverted_thermal() {
        let base: toml::Table = toml::from_str(
            "[thermal]\ntarget_temp_c = 55\nhot_temp_c = 65\ndangerous_temp_c = 75\n",
        )
        .unwrap();
        let patch: SharedConfigPatch = serde_json::from_value(serde_json::json!({
            "thermal": {
                "targetTempC": 70
            }
        }))
        .expect("valid shared config patch shape");

        let err = merge_shared_config_patch_table(base, patch)
            .expect_err("shared config must reject a merged thermal ordering inversion");
        assert!(
            err.contains("thermal.target_temp_c") && err.contains("thermal.hot_temp_c"),
            "unexpected error: {err}"
        );
    }

    // Unknown top-level sections are rejected (deny-unknown-fields daemon would
    // crash-loop on restart otherwise).
    #[test]
    fn config_import_rejects_unknown_sections() {
        let table: toml::Table = toml::from_str("[pool]\nurl = \"x\"\n[bogus]\nfoo = 1\n").unwrap();
        let unknown = disallowed_import_sections(&table);
        assert_eq!(unknown, vec!["bogus".to_string()]);

        // Known sections only -> no complaint.
        let ok: toml::Table = toml::from_str("[pool]\nurl = \"x\"\n[mining]\n").unwrap();
        assert!(disallowed_import_sections(&ok).is_empty());
    }

    // Keep-existing: a re-imported export carries the redaction placeholder for
    // secrets/wallets; import must restore them from the current stored config
    // and must NEVER persist the literal placeholder.
    #[test]
    fn config_import_placeholder_keeps_existing_secret() {
        let current: toml::Table = toml::from_str(
            "[pool]\nurl = \"stratum+tcp://public-pool.io:21496\"\nworker = \"bc1qrealwalletaddress3333333333333333333.w\"\npassword = \"realpoolpass\"\n[mqtt]\npassword = \"realmqttpass\"\n",
        )
        .unwrap();

        // Imported = a redacted export of the same config.
        let mut imported = current.clone();
        redact_config_table_for_export(&mut imported);
        // Sanity: the export masked the secrets.
        let p = imported.get("pool").and_then(|v| v.as_table()).unwrap();
        assert_eq!(
            p.get("password").and_then(|v| v.as_str()),
            Some(SECRET_REDACTION_PLACEHOLDER)
        );

        resolve_import_redaction_placeholders(&mut imported, &current);

        // Real secrets + wallet restored from current; placeholder never persisted.
        let p = imported.get("pool").and_then(|v| v.as_table()).unwrap();
        assert_eq!(
            p.get("password").and_then(|v| v.as_str()),
            Some("realpoolpass")
        );
        assert_eq!(
            p.get("worker").and_then(|v| v.as_str()),
            Some("bc1qrealwalletaddress3333333333333333333.w")
        );
        assert_eq!(
            p.get("url").and_then(|v| v.as_str()),
            Some("stratum+tcp://public-pool.io:21496")
        );
        let m = imported.get("mqtt").and_then(|v| v.as_table()).unwrap();
        assert_eq!(
            m.get("password").and_then(|v| v.as_str()),
            Some("realmqttpass")
        );

        let serialized = toml::to_string(&imported).expect("serialize");
        assert!(
            !serialized.contains(SECRET_REDACTION_PLACEHOLDER),
            "{serialized}"
        );
    }

    // A placeholder with no counterpart in the current config is DROPPED, never
    // persisted as the literal mask.
    #[test]
    fn config_import_placeholder_without_existing_is_dropped() {
        let current = toml::Table::new();
        let mut imported: toml::Table = toml::from_str(
            "[pool]\nurl = \"stratum+tcp://public-pool.io:21496\"\npassword = \"<redacted>\"\n",
        )
        .unwrap();
        resolve_import_redaction_placeholders(&mut imported, &current);
        let p = imported.get("pool").and_then(|v| v.as_table()).unwrap();
        assert!(
            p.get("password").is_none(),
            "placeholder must be dropped, not persisted"
        );
        // non-secret field preserved.
        assert_eq!(
            p.get("url").and_then(|v| v.as_str()),
            Some("stratum+tcp://public-pool.io:21496")
        );
    }

    // FIX 2: keep-existing restores wallet (worker), url, AND the FIX-1
    // generalized wallet key coinbase_output_address from the current config; an
    // orphan placeholder (no current counterpart) is DROPPED, never persisted.
    #[test]
    fn config_import_keep_existing_restores_wallet_url_coinbase_and_drops_orphan() {
        let current: toml::Table = toml::from_str(
            "[pool]\nurl = \"stratum+tcp://public-pool.io:21496\"\nworker = \"bc1qrealworker4444444444444444444444.w\"\n\
             [job_declaration]\ncoinbase_output_address = \"bc1qrealcoinbase5555555555555555555.cb\"\n",
        )
        .unwrap();

        // Imported: wallet/url placeholders that HAVE a counterpart in current,
        // plus an orphan placeholder ([mqtt].password — absent from current).
        let mut imported: toml::Table = toml::from_str(&format!(
            "[pool]\nurl = \"{ph}\"\nworker = \"{ph}\"\n\
             [job_declaration]\ncoinbase_output_address = \"{ph}\"\n\
             [mqtt]\npassword = \"{ph}\"\n",
            ph = SECRET_REDACTION_PLACEHOLDER
        ))
        .unwrap();

        resolve_import_redaction_placeholders(&mut imported, &current);

        // Placeholders with a current counterpart are restored to the real value.
        let pool = imported.get("pool").and_then(|v| v.as_table()).unwrap();
        assert_eq!(
            pool.get("worker").and_then(|v| v.as_str()),
            Some("bc1qrealworker4444444444444444444444.w")
        );
        assert_eq!(
            pool.get("url").and_then(|v| v.as_str()),
            Some("stratum+tcp://public-pool.io:21496")
        );
        let jd = imported
            .get("job_declaration")
            .and_then(|v| v.as_table())
            .unwrap();
        assert_eq!(
            jd.get("coinbase_output_address").and_then(|v| v.as_str()),
            Some("bc1qrealcoinbase5555555555555555555.cb")
        );

        // The orphan placeholder is DROPPED (no current value to keep).
        let mqtt = imported.get("mqtt").and_then(|v| v.as_table()).unwrap();
        assert!(
            mqtt.get("password").is_none(),
            "orphan placeholder must be dropped, not persisted as the literal mask"
        );

        // Nothing serializes as the literal mask.
        let serialized = toml::to_string(&imported).expect("serialize");
        assert!(
            !serialized.contains(SECRET_REDACTION_PLACEHOLDER),
            "{serialized}"
        );
    }

    // FIX 4: importing a config that contains ONLY [home] must MERGE onto the
    // current config — the existing [pool]/[thermal] sections are preserved (no
    // wholesale-persist data loss).
    #[test]
    fn config_import_merge_preserves_unmentioned_sections() {
        let current: toml::Table = toml::from_str(
            "[pool]\nurl = \"stratum+tcp://public-pool.io:21496\"\nworker = \"w\"\n\
             [thermal]\ntarget_temp_c = 55\nhot_temp_c = 65\ndangerous_temp_c = 75\n",
        )
        .unwrap();
        // Upload contains ONLY [home].
        let imported: toml::Table = toml::from_str("[home]\npreset = \"balanced\"\n").unwrap();

        let (merged, applied, preserved) = merge_imported_config_sections(&current, &imported);

        // The uploaded section is applied.
        assert_eq!(applied, vec!["home".to_string()]);
        assert!(merged.get("home").and_then(|v| v.as_table()).is_some());

        // The omitted current sections are PRESERVED (no data loss).
        assert_eq!(preserved, vec!["pool".to_string(), "thermal".to_string()]);
        let pool = merged.get("pool").and_then(|v| v.as_table()).unwrap();
        assert_eq!(
            pool.get("url").and_then(|v| v.as_str()),
            Some("stratum+tcp://public-pool.io:21496")
        );
        let thermal = merged.get("thermal").and_then(|v| v.as_table()).unwrap();
        assert_eq!(
            thermal.get("target_temp_c").and_then(toml_value_as_i64),
            Some(55)
        );
    }

    fn config_merge_leaf_strategy() -> impl Strategy<Value = serde_json::Value> {
        prop_oneof![
            any::<bool>().prop_map(serde_json::Value::Bool),
            (-10_000i64..=10_000).prop_map(|n| serde_json::json!(n)),
            "[A-Za-z0-9_./:@-]{0,32}".prop_map(serde_json::Value::String),
        ]
    }

    fn config_merge_value_strategy() -> impl Strategy<Value = serde_json::Value> {
        config_merge_leaf_strategy().prop_recursive(3, 24, 4, |inner| {
            prop::collection::btree_map("[a-z][a-z0-9_]{0,12}", inner, 0..4).prop_map(|map| {
                let object: serde_json::Map<String, serde_json::Value> = map.into_iter().collect();
                serde_json::Value::Object(object)
            })
        })
    }

    fn config_merge_object_strategy() -> impl Strategy<Value = serde_json::Value> {
        let section = prop::sample::select(vec![
            "general".to_string(),
            "pool".to_string(),
            "api".to_string(),
            "led".to_string(),
            "autotuner".to_string(),
            "webhook".to_string(),
        ]);
        prop::collection::btree_map(section, config_merge_value_strategy(), 0..6).prop_map(|map| {
            let object: serde_json::Map<String, serde_json::Value> = map.into_iter().collect();
            serde_json::Value::Object(object)
        })
    }

    fn json_object_to_toml_table(value: &serde_json::Value) -> toml::Table {
        match json_to_toml(value).expect("generated config JSON converts to TOML") {
            toml::Value::Table(table) => table,
            _ => unreachable!("config_merge_object_strategy always emits an object"),
        }
    }

    fn model_recursive_merge(dst: &mut toml::Value, src: toml::Value) {
        match (dst, src) {
            (toml::Value::Table(dst_table), toml::Value::Table(src_table)) => {
                for (key, src_value) in src_table {
                    match dst_table.get_mut(&key) {
                        Some(dst_value) => model_recursive_merge(dst_value, src_value),
                        None => {
                            dst_table.insert(key, src_value);
                        }
                    }
                }
            }
            (dst_value, src_value) => {
                *dst_value = src_value;
            }
        }
    }

    fn model_config_update_merge(mut current: toml::Table, update: toml::Table) -> toml::Table {
        for (key, value) in update {
            match current.get_mut(&key) {
                Some(existing) => model_recursive_merge(existing, value),
                None => {
                    current.insert(key, value);
                }
            }
        }
        current
    }

    proptest! {
        #![proptest_config(ProptestConfig {
            cases: 96,
            .. ProptestConfig::default()
        })]

        #[test]
        fn config_update_merge_matches_recursive_model_and_preserves_omitted_sections(
            current_json in config_merge_object_strategy(),
            update_json in config_merge_object_strategy(),
        ) {
            let current_table = json_object_to_toml_table(&current_json);
            let update_table = json_object_to_toml_table(&update_json);
            let expected = model_config_update_merge(current_table.clone(), update_table.clone());

            let merged = merge_config_update_table(current_table.clone(), &update_json)
                .expect("generated validation-safe config update should merge");

            prop_assert_eq!(&merged, &expected);

            for key in current_table.keys() {
                if !update_table.contains_key(key) {
                    prop_assert_eq!(
                        merged.get(key),
                        current_table.get(key),
                        "omitted top-level section {} must be preserved",
                        key
                    );
                }
            }
        }
    }

    // /api/pools no-leak: the pool worker (operator BTC payout address) must be
    // masked, never emitted raw.
    #[test]
    fn pools_worker_display_masks_wallet_no_raw_leak() {
        let raw = "bc1qexamplewalletaddress0000000000000000000";
        let shown = pool_worker_display(raw);
        assert_ne!(shown, raw, "worker emitted raw: {shown}");
        assert!(
            !shown.contains("walletaddress"),
            "raw middle leaked: {shown}"
        );
        // Masked form keeps a recognizable prefix/suffix for operator UX.
        assert!(shown.starts_with("bc1qex"), "{shown}");
        // Empty worker stays empty (not a fake mask).
        assert_eq!(pool_worker_display(""), "");
    }

    // RE-011: heat-reuse credit is a pure function of wall watts × electricity
    // rate × offset fraction (capped 0..1). Default offset 0 ⇒ zero credit (we
    // never overstate ROI).
    #[test]
    fn heat_reuse_credit_is_displaced_heating_value() {
        // 3000 W, $0.12/kWh, fully offsetting heat ⇒ 3 kW × 24 h × 0.12 = $8.64/day.
        let credit = compute_heat_reuse_credit_usd_per_day(3000.0, 0.12, 1.0);
        assert!((credit - 8.64).abs() < 1e-9, "got {credit}");
        // Half the heat offsets heating ⇒ half the credit.
        let half = compute_heat_reuse_credit_usd_per_day(3000.0, 0.12, 0.5);
        assert!((half - 4.32).abs() < 1e-9, "got {half}");
        // Default (no offset configured) ⇒ no credit, never overstates ROI.
        assert_eq!(
            compute_heat_reuse_credit_usd_per_day(3000.0, 0.12, 0.0),
            0.0
        );
    }

    #[test]
    fn heat_reuse_credit_clamps_and_floors_inputs() {
        // Offset fraction > 1 is clamped to 1 (can't displace more than 100%).
        let over = compute_heat_reuse_credit_usd_per_day(1000.0, 0.20, 5.0);
        let exact = compute_heat_reuse_credit_usd_per_day(1000.0, 0.20, 1.0);
        assert_eq!(over, exact);
        // Negative inputs floor to 0 (no negative credit / no garbage).
        assert_eq!(
            compute_heat_reuse_credit_usd_per_day(-100.0, 0.12, 1.0),
            0.0
        );
        assert_eq!(
            compute_heat_reuse_credit_usd_per_day(1000.0, -0.12, 1.0),
            0.0
        );
        assert_eq!(
            compute_heat_reuse_credit_usd_per_day(1000.0, 0.12, -0.5),
            0.0
        );
    }

    // RE-011: the mempool.space difficulty-adjustment parse is failure-tolerant
    // and never fabricates a difficulty it didn't receive.
    #[test]
    fn parse_difficulty_adjustment_reads_change_and_companion_fields() {
        let body = serde_json::json!({
            "difficultyChange": 2.34,
            "remainingBlocks": 1000
        });
        let sample = parse_difficulty_adjustment(&body, Some(7.3e13), Some(650.0), 12345)
            .expect("sample built");
        assert!((sample.difficulty_change_percent - 2.34).abs() < 1e-9);
        assert!((sample.difficulty - 7.3e13).abs() < 1.0);
        assert_eq!(sample.network_hashrate_ehs, Some(650.0));
        assert_eq!(sample.fetched_at_ms, 12345);
        assert_eq!(sample.source, "mempool.space");
    }

    #[test]
    fn parse_difficulty_adjustment_handles_missing_companion_difficulty() {
        // No companion difficulty ⇒ difficulty 0.0 (signals "unknown"; readers
        // map it to null). No panic, change still recorded.
        let body = serde_json::json!({ "difficultyChange": -1.0 });
        let sample = parse_difficulty_adjustment(&body, None, None, 1).expect("sample built");
        assert_eq!(sample.difficulty, 0.0);
        assert!((sample.difficulty_change_percent + 1.0).abs() < 1e-9);
        assert_eq!(sample.network_hashrate_ehs, None);
    }

    #[test]
    fn parse_difficulty_adjustment_tolerates_missing_change_field() {
        // Empty/garbage body ⇒ change defaults to 0.0, still a valid sample
        // (failure-tolerant — an offline unit's cache never panics).
        let body = serde_json::json!({});
        let sample = parse_difficulty_adjustment(&body, None, None, 0).expect("sample built");
        assert_eq!(sample.difficulty_change_percent, 0.0);
    }

    // RE-013: the donation transparency block surfaces BOTH the primary and the
    // visible Braiins-worker fallback, with workers redacted.
    #[test]
    fn donation_transparency_surfaces_visible_braiins_fallback() {
        let toml_src = r#"
            enabled = true
            percent = 2.0
            pool_url = "stratum+tcp://pool.d-central.tech:3333"
            worker = "DungeonMaster"
            fallback_enabled = true
            fallback_pool_url = "stratum+tcp://stratum.braiins.com:3333"
            fallback_worker = "DungeonMaster"
        "#;
        let donation: toml::Table = toml::from_str(toml_src).expect("valid toml");
        let block = donation_transparency_from_table(Some(&donation), true);

        assert_eq!(block["enabled"], serde_json::json!(true));
        assert_eq!(block["active"], serde_json::json!(true));
        assert_eq!(block["is_devfee"], serde_json::json!(false));
        assert_eq!(
            block["pool_url"],
            serde_json::json!("stratum+tcp://pool.d-central.tech:3333")
        );
        // Fallback is surfaced for transparency.
        assert_eq!(block["fallback_enabled"], serde_json::json!(true));
        assert_eq!(
            block["fallback_pool_url"],
            serde_json::json!("stratum+tcp://stratum.braiins.com:3333")
        );
        // Worker passes through redact_worker unchanged — "DungeonMaster" is
        // 13 chars (<= 18) so it is not truncated.
        assert_eq!(
            block["fallback_worker_redacted"],
            serde_json::json!("DungeonMaster")
        );
        // Primary worker "DungeonMaster" (13 chars) passes through unchanged.
        assert_eq!(block["worker_redacted"], serde_json::json!("DungeonMaster"));
    }

    #[test]
    fn donation_transparency_defaults_when_unconfigured() {
        // No [donation] table ⇒ defaults that match dcentrald.toml, including
        // the visible Braiins fallback worker.
        let block = donation_transparency_from_table(None, false);
        assert_eq!(block["enabled"], serde_json::json!(true));
        assert_eq!(block["percent"], serde_json::json!(2.0));
        // Default fallback worker "DungeonMaster" (13 chars) passes through
        // redact_worker unchanged.
        assert_eq!(
            block["fallback_worker_redacted"],
            serde_json::json!("DungeonMaster")
        );
        assert_eq!(
            block["fallback_pool_url"],
            serde_json::json!("stratum+tcp://stratum.braiins.com:3333")
        );
        assert_eq!(block["active"], serde_json::json!(false));
    }

    #[test]
    fn donation_config_value_from_table_reflects_config() {
        let table: toml::Table = toml::from_str(
            r#"
            [donation]
            enabled = false
            percent = 3.5
            pool_url = "stratum+tcp://donation.example.com:3333"
            worker = "donation-worker"
            password = "donation-pass"
            fallback_enabled = false
            fallback_pool_url = "stratum+tcp://fallback.example.com:443"
            fallback_worker = "fallback-worker"
            fallback_password = "fallback-pass"
            cycle_duration_s = 7200
        "#,
        )
        .expect("valid toml");

        let config = donation_config_value_from_table(&table);
        assert_eq!(config["enabled"], serde_json::json!(false));
        assert_eq!(config["percent"], serde_json::json!(3.5));
        assert_eq!(
            config["pool_url"],
            serde_json::json!("stratum+tcp://donation.example.com:3333")
        );
        assert_eq!(config["worker"], serde_json::json!("donation-worker"));
        assert_eq!(config["password"], serde_json::json!("donation-pass"));
        assert_eq!(config["fallback_enabled"], serde_json::json!(false));
        assert_eq!(
            config["fallback_pool_url"],
            serde_json::json!("stratum+tcp://fallback.example.com:443")
        );
        assert_eq!(
            config["fallback_worker"],
            serde_json::json!("fallback-worker")
        );
        assert_eq!(
            config["fallback_password"],
            serde_json::json!("fallback-pass")
        );
        assert_eq!(config["cycle_duration_s"], serde_json::json!(7200));
    }

    #[test]
    fn merge_config_update_table_rejects_bad_donation_percent() {
        let err = merge_config_update_table(
            toml::Table::new(),
            &serde_json::json!({ "donation": { "percent": 5.5 } }),
        )
        .expect_err("donation.percent above 5 must be rejected");

        assert!(
            err.contains("donation.percent"),
            "unexpected validation error: {err}"
        );
    }

    #[test]
    fn config_update_validation_matrix_rejects_known_bad_inputs() {
        let cases = [
            (
                "unknown_top_level_section",
                serde_json::json!({ "not_a_config_section": true }),
                "Disallowed config keys",
            ),
            (
                "donation_percent_above_range",
                serde_json::json!({ "donation": { "percent": 5.5 } }),
                "donation.percent",
            ),
            (
                "mining_voltage_above_am2_ceiling",
                serde_json::json!({ "mining": { "voltage_mv": 15000 } }),
                "voltage_mv",
            ),
            (
                "zero_serial_chip_count",
                serde_json::json!({ "mining": { "serial_chip_count": 0 } }),
                "serial_chip_count",
            ),
            (
                "non_ip_http_bind",
                serde_json::json!({ "api": { "http_bind": "miner.local" } }),
                "api.http_bind",
            ),
        ];

        for (name, body, needle) in cases {
            let err = match merge_config_update_table(toml::Table::new(), &body) {
                Ok(_) => panic!("{name} must be rejected"),
                Err(err) => err,
            };
            assert!(
                err.contains(needle),
                "{name} should mention {needle}, got: {err}"
            );
        }
    }

    proptest! {
        #[test]
        fn config_update_merge_never_panics_on_bounded_json_patch(
            section in prop_oneof![
                Just("api"),
                Just("donation"),
                Just("home"),
                Just("mining"),
                Just("pool"),
                Just("power"),
                Just("thermal"),
            ],
            key in "[a-z_]{1,24}",
            value in prop_oneof![
                any::<bool>().prop_map(serde_json::Value::Bool),
                (-10_000i64..30_000i64)
                    .prop_map(|value| serde_json::Value::Number(value.into())),
                "[ -~]{0,64}".prop_map(serde_json::Value::String),
            ],
        ) {
            let body = serde_json::json!({ section: { key: value } });
            let _ = merge_config_update_table(toml::Table::new(), &body);
        }
    }

    #[tokio::test]
    async fn donation_config_route_rejects_bad_percent() {
        let state = crate::build_minimal_app_state(crate::MinimalAppStateInputs {
            api_config: crate::ApiConfig::default(),
            pool_url: "stratum+tcp://pool.example.com:3333".to_string(),
            pool_protocol: "sv1".to_string(),
            mode: crate::OperatingMode::Standard,
            firmware_version: "test".to_string(),
            fan_pwm: 30,
            network_block: crate::NetworkBlockConfig::default(),
            profile_path: std::env::temp_dir()
                .join(format!("dcentrald-donation-route-{}", std::process::id()))
                .to_string_lossy()
                .into_owned(),
            control_board_label: "am1-s9".to_string(),
            chip_type_label: "BM1387".to_string(),
            external_state_rx: None,
        });
        {
            let mut hw = state.hardware_info.lock().expect("hardware lock");
            hw.identification = crate::HardwareIdentification::from_evidence(
                vec![
                    crate::HardwareIdentityEvidence::declared_asic_board_target("am1-s9", "BM1387"),
                    crate::HardwareIdentityEvidence::measured_asic_enumeration(
                        0x1387,
                        "BM1387",
                        crate::HardwareCompositionToken::new(1, "test:am1-s9"),
                    ),
                ],
                Some("test S9 identity".to_string()),
            );
        }

        let response =
            post_config_donation(State(state), Json(serde_json::json!({ "percent": 5.5 }))).await;

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let bytes = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .expect("body bytes");
        let body: serde_json::Value = serde_json::from_slice(&bytes).expect("json body");
        assert_eq!(
            body["code"],
            serde_json::json!(dcentrald_api_types::api_error_codes::CONFIG_VALIDATION)
        );
        assert!(
            body["error"]
                .as_str()
                .expect("error string")
                .contains("donation.percent"),
            "unexpected response body: {body}"
        );
    }

    #[tokio::test]
    async fn pool_validation_error_uses_canonical_envelope() {
        let response = pool_validation_error("pool url rejected");
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .expect("body bytes");
        let body: serde_json::Value = serde_json::from_slice(&body).expect("json body");
        assert_eq!(body["error"], serde_json::json!("pool url rejected"));
        assert_eq!(body["code"], serde_json::json!("pool_validation"));
        assert_eq!(
            body["suggestion"],
            serde_json::json!(
                "Check the pool URL format, worker name, and failover split settings."
            )
        );
    }

    #[tokio::test]
    async fn api_error_mapper_wraps_bare_text_and_json_string_only() {
        use axum::{
            body::Body,
            http::{header, Request},
            routing::get,
            Router,
        };
        use tower::ServiceExt;

        let app = Router::new()
            .route(
                "/bare-text",
                get(|| async { (StatusCode::BAD_REQUEST, "plain failure") }),
            )
            .route(
                "/json-string",
                get(|| async {
                    (
                        StatusCode::BAD_REQUEST,
                        Json(serde_json::json!("json failure")),
                    )
                }),
            )
            .route("/ok", get(|| async { (StatusCode::OK, "ok") }))
            .route(
                "/json-object",
                get(|| async {
                    (
                        StatusCode::BAD_REQUEST,
                        Json(serde_json::json!({ "error": "already structured" })),
                    )
                }),
            )
            .route(
                "/legacy-error",
                get(|| async {
                    (
                        StatusCode::BAD_REQUEST,
                        Json(serde_json::json!({
                            "status": "error",
                            "message": "legacy failure",
                            "detail": "legacy detail",
                            "code": "legacy_code",
                            "suggestion": "legacy suggestion",
                        })),
                    )
                }),
            )
            .route(
                "/binary",
                get(|| async {
                    (
                        StatusCode::BAD_REQUEST,
                        [(header::CONTENT_TYPE, "application/octet-stream")],
                        "raw",
                    )
                }),
            )
            .layer(axum::middleware::map_response(normalize_api_error_response));

        let bare = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/bare-text")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("bare response");
        assert_eq!(bare.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            bare.headers()
                .get(header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok()),
            Some("application/json")
        );
        let body = axum::body::to_bytes(bare.into_body(), 4096)
            .await
            .expect("body bytes");
        let body: serde_json::Value = serde_json::from_slice(&body).expect("json body");
        assert_eq!(body["error"], serde_json::json!("plain failure"));
        assert_eq!(
            body["code"],
            serde_json::json!(dcentrald_api_types::api_error_codes::UNCLASSIFIED_ERROR)
        );

        let json_string = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/json-string")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("json string response");
        let body = axum::body::to_bytes(json_string.into_body(), 4096)
            .await
            .expect("body bytes");
        let body: serde_json::Value = serde_json::from_slice(&body).expect("json body");
        assert_eq!(body["error"], serde_json::json!("json failure"));
        assert_eq!(
            body["code"],
            serde_json::json!(dcentrald_api_types::api_error_codes::UNCLASSIFIED_ERROR)
        );

        let ok = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/ok")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("ok response");
        assert_eq!(ok.status(), StatusCode::OK);
        let ok_body = axum::body::to_bytes(ok.into_body(), 4096)
            .await
            .expect("body bytes");
        assert_eq!(ok_body.as_ref(), b"ok");

        let json_object = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/json-object")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("json object response");
        let body = axum::body::to_bytes(json_object.into_body(), 4096)
            .await
            .expect("body bytes");
        let body: serde_json::Value = serde_json::from_slice(&body).expect("json body");
        assert_eq!(body, serde_json::json!({ "error": "already structured" }));

        let legacy_error = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/legacy-error")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("legacy error response");
        let body = axum::body::to_bytes(legacy_error.into_body(), 4096)
            .await
            .expect("body bytes");
        let body: serde_json::Value = serde_json::from_slice(&body).expect("json body");
        assert_eq!(body["error"], serde_json::json!("legacy failure"));
        assert_eq!(body["detail"], serde_json::json!("legacy detail"));
        assert_eq!(body["code"], serde_json::json!("legacy_code"));
        assert_eq!(body["suggestion"], serde_json::json!("legacy suggestion"));
        assert!(body.get("status").is_none(), "legacy status must not leak");
        assert!(
            body.get("message").is_none(),
            "legacy message must be merged"
        );

        let binary = app
            .oneshot(
                Request::builder()
                    .uri("/binary")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("binary response");
        assert_eq!(
            binary
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok()),
            Some("application/octet-stream")
        );
        let body = axum::body::to_bytes(binary.into_body(), 4096)
            .await
            .expect("body bytes");
        assert_eq!(body.as_ref(), b"raw");
    }

    #[test]
    fn autotuner_mode_response_exposes_runtime_alias() {
        let mode = dcentrald_autotuner::config::TunerMode::Efficiency;
        let body = autotuner_mode_response(
            "set_active",
            &mode,
            serde_json::json!({ "source": "test" }),
            serde_json::json!({
                "channel_available": true,
                "accepted": true,
                "applied_runtime": true,
                "status": "applied",
                "message": "applied",
            }),
        );

        assert_eq!(body["runtime"]["status"], serde_json::json!("applied"));
        assert_eq!(body["runtime"], body["runtime_command"]);
    }

    #[test]
    fn build_api_compatibility_manifest_response_reports_declared_no_probe_contract() {
        let body = build_api_compatibility_manifest_response();

        assert_eq!(body.status, "ok");
        assert_eq!(body.schema_version, 1);
        assert!(body.read_only);
        assert!(!body.content_collected);
        assert!(!body.probe_performed);
        assert!(!body.handlers_executed);

        let all_routes: Vec<&ApiCompatibilityRouteEntry> = body
            .surfaces
            .iter()
            .flat_map(|surface| surface.routes.iter())
            .collect();
        let all_commands: Vec<&ApiCompatibilityCommandEntry> = body
            .surfaces
            .iter()
            .flat_map(|surface| surface.commands.iter())
            .collect();

        assert!(all_routes.iter().any(|route| {
            route.method == "GET"
                && route.path == "/api/system/info"
                && route.support == "implemented"
                && route.unsupported_fields.contains(&"bestDiff")
        }));
        assert!(all_routes.iter().any(|route| {
            route.method == "GET"
                && route.path == "/api/system/asic"
                && route.support == "implemented"
        }));
        assert!(all_routes.iter().any(|route| {
            route.method == "POST"
                && route.path == "/api/v1/firmware/update"
                && route.support == "implemented_alias"
                && route.mutates
        }));
        assert!(all_routes.iter().any(|route| {
            route.method == "GET"
                && route.path == "/api/system/api-compatibility/manifest"
                && !route.mutates
        }));
        assert!(all_routes.iter().any(|route| {
            route.method == "GET" && route.path == "/api/competitive/readiness" && !route.mutates
        }));
        for expected in [
            "/api/fleet/pool-stats",
            "/api/mining/pipeline/snapshot",
            "/api/diagnostics/failure_modes",
            "/api/diagnostics/chain",
            "/api/diagnostics/shares/local_rejects",
            "/api/hardware/pic_info",
            "/api/diagnostics/recovery_actions",
            "/api/system/boot_timeline",
            "/api/history/audit",
            "/api/hardware/psu_catalog",
            "/api/cgminer/catalog",
            "/api/profiles/presets",
            "/api/hardware/thermal/sensors",
            "/api/re/catalog",
        ] {
            assert!(
                all_routes.iter().any(|route| {
                    route.method == "GET" && route.path == expected && !route.mutates
                }),
                "compatibility manifest missing {expected}"
            );
        }
        assert!(all_routes.iter().any(|route| {
            route.method == "GET" && route.path == "/api/config/donation" && !route.mutates
        }));
        assert!(all_routes.iter().any(|route| {
            route.method == "POST" && route.path == "/api/config/donation" && route.mutates
        }));
        assert!(all_routes.iter().any(|route| {
            route.method == "PUT" && route.path == "/api/autotuner/active" && route.mutates
        }));

        assert!(all_commands.iter().any(|command| {
            command.name == "summary" && command.support == "implemented" && !command.mutates
        }));
        assert!(all_commands.iter().any(|command| {
            command.name == "switchpool"
                && command.support == "recognized_unsupported"
                && !command.mutates
        }));
        assert!(!body
            .omissions
            .iter()
            .any(|omission| omission.path == Some("/api/config/donation")));
        assert!(body
            .limitations
            .iter()
            .any(|item| item.contains("does not call")));
    }

    #[test]
    fn build_api_index_response_lists_routes_and_pyasic_aliases() {
        let body = build_api_index_response();

        assert_eq!(body["status"], serde_json::json!("ok"));
        assert_eq!(body["schema"], serde_json::json!("dcentos.api.index.v1"));
        assert_eq!(
            body["api_contract_version"],
            serde_json::json!(dcentrald_api_types::API_CONTRACT_VERSION)
        );
        assert_eq!(body["read_only"], serde_json::json!(true));
        assert_eq!(
            body["generated_from"],
            serde_json::json!("/api/system/api-compatibility/manifest")
        );

        let routes = body["routes"].as_array().expect("routes array");
        assert!(!routes.is_empty(), "index must list mounted routes");
        assert_eq!(
            body["route_count"].as_u64(),
            Some(routes.len() as u64),
            "route_count must match the routes array length"
        );
        // Every cataloged route is derived from the compatibility manifest, so
        // it carries method + path + surface (no hand-maintained second list).
        assert!(routes.iter().all(|route| {
            route["method"].is_string() && route["path"].is_string() && route["surface"].is_string()
        }));
        // A well-known DCENT route shows up in the catalog.
        assert!(routes
            .iter()
            .any(|route| route["path"] == serde_json::json!("/api/status")));
        // Surfaces summary is present and counted.
        assert_eq!(
            body["surface_count"].as_u64(),
            Some(body["surfaces"].as_array().expect("surfaces array").len() as u64)
        );

        // pyasic-friendly aliases resolve friendly names onto canonical routes.
        let aliases = body["aliases"].as_object().expect("aliases object");
        assert_eq!(aliases["fans"], serde_json::json!("/api/status"));
        assert_eq!(
            aliases["autotune_status"],
            serde_json::json!("/api/autotuner/status")
        );
        assert_eq!(
            aliases["thermal"],
            serde_json::json!("/api/thermal/supervisor")
        );
        assert_eq!(
            aliases["pools.failover"],
            serde_json::json!("/api/pools/failover_policy")
        );
        assert_eq!(
            aliases["tuning_profiles"],
            serde_json::json!("/api/profiles")
        );

        // Every alias target MUST be a real mounted route (no advertised 404s).
        let mounted = mounted_route_paths_from_source();
        for target in aliases.values() {
            let path = target.as_str().expect("alias target is a string");
            assert!(
                mounted.contains(path),
                "alias target {path} must be a mounted route"
            );
        }
    }

    #[test]
    fn api_compatibility_manifest_route_parity_is_explicit() {
        let mounted = mounted_route_paths_from_source();
        let manifest = api_compatibility_manifest_route_paths();
        let omitted: std::collections::BTreeSet<&str> = API_COMPATIBILITY_ROUTE_PARITY_OMISSIONS
            .iter()
            .copied()
            .collect();

        let unmanifested: Vec<&str> = mounted
            .iter()
            .map(String::as_str)
            .filter(|path| !manifest.contains(*path) && !omitted.contains(*path))
            .collect();
        assert!(
            unmanifested.is_empty(),
            "mounted API routes must be declared in the API compatibility manifest or explicitly omitted: {unmanifested:?}"
        );

        let unmounted: Vec<&str> = manifest
            .iter()
            .map(String::as_str)
            .filter(|path| !mounted.contains(*path))
            .collect();
        assert!(
            unmounted.is_empty(),
            "API compatibility manifest routes must stay mounted: {unmounted:?}"
        );
    }

    /// P0-6 (Omega Plan, C-7): the diagnostic banner reads `dcentrald_status`
    /// (falling back to `alive`) from `/api/dashboard/health`. Pin the exact
    /// keys/values the banner depends on so the daemon-served route can never
    /// silently drift away from server.py's always-local schema and re-break
    /// the "is the daemon alive?" probe.
    #[test]
    fn dashboard_health_payload_reports_alive_contract_for_the_banner() {
        let body = dashboard_health_payload(4242, 1337);

        // The banner only treats `dcentrald_status === "alive"` as healthy; a
        // reachable daemon must report exactly that (anything else would make
        // the banner show a false DEAD bar on the :8080-direct path).
        assert_eq!(body["dcentrald_status"].as_str(), Some("alive"));
        assert_eq!(body["alive"].as_bool(), Some(true));
        assert_eq!(body["api_bound"].as_bool(), Some(true));
        assert_eq!(body["pid"].as_u64(), Some(4242));
        assert_eq!(body["uptime_s"].as_u64(), Some(1337));
        assert_eq!(body["source"].as_str(), Some("dcentrald"));
        assert!(
            body["version"].as_str().is_some_and(|v| !v.is_empty()),
            "version must be a non-empty string"
        );
        // The route must stay in the compatibility manifest (the banner is a
        // real consumer); guard against accidental removal of the declaration.
        assert!(api_compatibility_manifest_route_paths().contains("/api/dashboard/health"));
    }

    #[test]
    fn build_competitive_readiness_response_reports_read_only_gate_contract() {
        let body = build_competitive_readiness_response(123_000);

        assert_eq!(
            body["schema"].as_str(),
            Some("dcentos.competitive.readiness.v1")
        );
        assert_eq!(body["read_only"].as_bool(), Some(true));
        assert_eq!(body["control_actions"].as_bool(), Some(false));
        assert_eq!(body["hardware_writes"].as_bool(), Some(false));
        assert_eq!(body["filesystem_mutation"].as_bool(), Some(false));
        assert_eq!(body["content_collected"].as_bool(), Some(false));
        assert_eq!(body["probe_performed"].as_bool(), Some(false));
        assert_eq!(body["handlers_executed"].as_bool(), Some(false));
        assert_eq!(
            body["decentralization_gate"]["license_required"].as_bool(),
            Some(false)
        );
        assert_eq!(
            body["decentralization_gate"]["license_server_required"].as_bool(),
            Some(false)
        );
        assert_eq!(
            body["decentralization_gate"]["mandatory_fee"].as_bool(),
            Some(false)
        );
        assert_eq!(
            body["decentralization_gate"]["fee_route"].as_str(),
            Some("transparent_donation")
        );
        assert!(body["decentralization_gate"]["donation"]
            .as_object()
            .expect("donation gate object")
            .contains_key("donation_off_test_status"));
        assert_eq!(
            body["decentralization_gate"]["offline_behavior"].as_str(),
            Some("local_first")
        );
        assert!(body["decentralization_gate"]["external_dependencies"]
            .as_array()
            .expect("external dependencies array")
            .iter()
            .any(|dependency| dependency["id"].as_str() == Some("user_pool")));
        assert!(body["decentralization_gate"]["source_basis"]
            .as_array()
            .expect("source basis array")
            .iter()
            .any(|source| source.as_str() == Some("clean_room")));
        assert_eq!(
            body["decentralization_gate"]["repair_diagnostic"].as_str(),
            Some("read_only_default")
        );
        assert!(body["decentralization_gate"]["write_surfaces"]
            .as_array()
            .expect("write surfaces array")
            .iter()
            .any(|surface| surface["surface"].as_str() == Some("MCP raw hardware tools")));
        assert_eq!(
            body["decentralization_gate"]["home_miner_safe"].as_bool(),
            Some(false)
        );
        assert_eq!(
            body["decentralization_gate"]["home_miner_safe_status"].as_str(),
            Some("partial")
        );

        let features = body["features"].as_array().expect("features array");
        assert!(features.len() >= 8);
        assert!(features.iter().any(|feature| {
            feature["id"].as_str() == Some("s19jpro_139_native_mining")
                && feature["status"].as_str() == Some("blocked")
        }));
        assert!(features.iter().any(|feature| {
            feature["id"].as_str() == Some("home_safe_profiles")
                && feature["status"].as_str() == Some("saved_only")
        }));
        assert!(features.iter().all(|feature| {
            feature["status"].as_str().is_some()
                && feature["source_basis"].as_str().is_some()
                && feature["telemetry_source"].as_str().is_some()
                && feature["confidence"].as_str().is_some()
                && feature["blockers"].as_array().is_some()
                && feature["docs_link"].as_str().is_some()
                && feature["recovery_link"].as_str().is_some()
                && feature["license_required"].as_bool() == Some(false)
                && feature["mandatory_fee"].as_bool() == Some(false)
                && feature["promotion_allowed"].as_bool().is_some()
                && feature["decentralization"]["source_basis"]
                    .as_array()
                    .is_some()
        }));

        // W1.1 default-credential lockdown: response MUST advertise the
        // dropbear gate state so the dashboard can render an honest
        // SSH-on/off chip without probing port 22.
        let ssh_state = body["setup_ssh_state"]
            .as_str()
            .expect("setup_ssh_state field present");
        assert!(
            matches!(
                ssh_state,
                "disabled" | "enabled-by-wizard" | "enabled-by-keys"
            ),
            "setup_ssh_state must be one of the three documented values, got {:?}",
            ssh_state
        );
    }

    /// W1.1 default-credential lockdown -- exhaustive truth table for
    /// `compute_setup_ssh_state`. The on-device init script consults the
    /// same three files; this test pins their relationship.
    #[test]
    fn compute_setup_ssh_state_matches_dropbear_gate_contract() {
        use std::io::Write;
        let tmp = tempdir_for_test("setup_ssh_state");
        let gate = tmp.join(".ssh-enabled");
        let keys = tmp.join("authorized_keys");
        let auth = tmp.join("auth.json");

        // (1) No gate flag -> always disabled, regardless of evidence.
        std::fs::File::create(&keys)
            .and_then(|mut f| f.write_all(b"ssh-rsa AAAA"))
            .expect("write keys");
        std::fs::File::create(&auth)
            .and_then(|mut f| f.write_all(b"{}"))
            .expect("write auth");
        assert_eq!(
            super::compute_setup_ssh_state(&gate, &keys, &auth),
            "disabled",
            "missing .ssh-enabled flag must keep gate closed"
        );

        // (2) Gate present, keys present (non-empty) -> enabled-by-keys.
        std::fs::File::create(&gate).expect("create gate");
        assert_eq!(
            super::compute_setup_ssh_state(&gate, &keys, &auth),
            "enabled-by-keys"
        );

        // (3) Gate present, keys file empty, auth.json present -> enabled-by-wizard.
        std::fs::write(&keys, b"").expect("truncate keys");
        assert_eq!(
            super::compute_setup_ssh_state(&gate, &keys, &auth),
            "enabled-by-wizard"
        );

        // (4) Gate present, keys file missing, auth.json present -> enabled-by-wizard.
        std::fs::remove_file(&keys).expect("rm keys");
        assert_eq!(
            super::compute_setup_ssh_state(&gate, &keys, &auth),
            "enabled-by-wizard"
        );

        // (5) Gate present, but neither evidence -> disabled (defensive).
        std::fs::remove_file(&auth).expect("rm auth");
        assert_eq!(
            super::compute_setup_ssh_state(&gate, &keys, &auth),
            "disabled",
            "gate flag without evidence must not advertise SSH-on"
        );
    }

    /// Tiny per-test scratch directory under the runner's tempdir. Avoids
    /// dragging in the `tempfile` crate just for a couple of tests.
    fn tempdir_for_test(label: &str) -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!("dcent-w1-1-{}-{}", label, nanos));
        std::fs::create_dir_all(&dir).expect("create per-test scratch dir");
        dir
    }

    #[test]
    fn build_mining_pipeline_manifest_response_reports_metadata_only_contract() {
        let body = build_mining_pipeline_manifest_response(123_000, false);

        assert_eq!(
            body["schema"].as_str(),
            Some("dcentos.mining.pipeline.manifest.v1")
        );
        assert_eq!(body["status"].as_str(), Some("publisher_disabled"));
        assert_eq!(body["read_only"].as_bool(), Some(true));
        assert_eq!(body["control_actions"].as_bool(), Some(false));
        assert_eq!(body["hardware_writes"].as_bool(), Some(false));
        assert_eq!(body["filesystem_mutation"].as_bool(), Some(false));
        assert_eq!(body["content_collected"].as_bool(), Some(false));
        assert_eq!(body["probe_performed"].as_bool(), Some(false));
        assert_eq!(body["handlers_executed"].as_bool(), Some(false));
        assert_eq!(body["publisher_live"].as_bool(), Some(false));
        assert_eq!(body["snapshot_available"].as_bool(), Some(false));
        assert_eq!(
            body["publisher_gate"]["receiver_configured"].as_bool(),
            Some(false)
        );
        assert_eq!(
            body["publisher_gate"]["config_toml_path"].as_str(),
            Some("mining.pipeline_snapshot.enabled")
        );
        assert_eq!(
            body["publisher_gate"]["live_snapshot_endpoint"].as_str(),
            Some("/api/mining/pipeline/snapshot")
        );
        assert_eq!(
            body["freshness_contract"]["default_stale_after_ms"].as_u64(),
            Some(crate::MINING_PIPELINE_SNAPSHOT_DEFAULT_STALE_AFTER_MS)
        );
        assert_eq!(
            body["freshness_contract"]["snapshot_available_only_when"].as_str(),
            Some("status == live")
        );
        assert_eq!(
            body["freshness_classifier_schema"].as_str(),
            Some(crate::MINING_PIPELINE_FRESHNESS_CLASSIFIER_SCHEMA)
        );
        assert_eq!(
            body["freshness_classifier"]["schema"].as_str(),
            Some(crate::MINING_PIPELINE_FRESHNESS_CLASSIFIER_SCHEMA)
        );
        assert_eq!(
            body["freshness_classifier"]["runtime_wired"].as_bool(),
            Some(false)
        );
        assert_eq!(
            body["freshness_classifier"]["telemetry_source"].as_str(),
            Some("classifier_contract_only")
        );
        assert!(body["freshness_classifier"]["outputs"]
            .as_array()
            .map(|items| {
                items
                    .iter()
                    .any(|item| item.as_str() == Some("future_clock_skew"))
                    && items.iter().any(|item| item.as_str() == Some("invalid"))
            })
            .unwrap_or(false));
        assert_eq!(
            body["freshness_classifier"]["snapshot_status_mapping"]["future_clock_skew"].as_str(),
            Some("unavailable")
        );
        assert_eq!(
            body["freshness_classifier"]["snapshot_status_mapping"]["invalid"].as_str(),
            Some("unavailable")
        );
        assert_eq!(
            body["freshness_classifier"]["example_fixtures_schema"].as_str(),
            Some("dcentos.mining.pipeline.freshness.classifier.fixture.v1")
        );
        assert_eq!(
            body["freshness_classifier"]["example_fixture_count"].as_u64(),
            Some(5)
        );
        assert_eq!(
            body["freshness_classifier"]["example_fixtures_are_design_only"].as_bool(),
            Some(true)
        );
        assert_eq!(
            body["freshness_classifier"]["example_fixtures_live_telemetry"].as_bool(),
            Some(false)
        );
        assert!(body["freshness_classifier"]["example_fixtures"]
            .as_array()
            .map(|items| {
                items.len() == 5
                    && items.iter().all(|item| {
                        item["design_only"].as_bool() == Some(true)
                            && item["non_telemetry"].as_bool() == Some(true)
                            && item["telemetry_source"].as_str() == Some("none")
                            && item["dispatcher_reads"].as_bool() == Some(false)
                            && item["hardware_reads"].as_bool() == Some(false)
                            && item["pool_socket_reads"].as_bool() == Some(false)
                    })
                    && items
                        .iter()
                        .any(|item| item["id"].as_str() == Some("unavailable"))
                    && items.iter().any(|item| item["id"].as_str() == Some("live"))
                    && items
                        .iter()
                        .any(|item| item["id"].as_str() == Some("stale"))
                    && items
                        .iter()
                        .any(|item| item["id"].as_str() == Some("future_clock_skew"))
                    && items
                        .iter()
                        .any(|item| item["id"].as_str() == Some("invalid"))
            })
            .unwrap_or(false));
        assert_eq!(
            body["publisher_design"]["status"].as_str(),
            Some("implemented_default_off")
        );
        assert_eq!(
            body["publisher_design"]["implemented"].as_bool(),
            Some(true)
        );
        assert_eq!(
            body["publisher_design"]["bounded_publish_cadence"]["publish_per_nonce"].as_bool(),
            Some(false)
        );
        assert_eq!(
            body["publisher_design"]["bounded_publish_cadence"]["max_hz"].as_u64(),
            Some(1)
        );
        assert!(body["publisher_design"]["forbidden"]
            .as_array()
            .map(|items| {
                items.iter().any(|item| {
                    item.as_str()
                        .map(|text| text.contains("mining_sync"))
                        .unwrap_or(false)
                }) && items.iter().any(|item| {
                    item.as_str()
                        .map(|text| text.contains("per-nonce"))
                        .unwrap_or(false)
                })
            })
            .unwrap_or(false));
        assert_eq!(
            body["snapshot_design_schema"].as_str(),
            Some("dcentos.mining.pipeline.snapshot.design.v2")
        );
        assert_eq!(
            body["snapshot_design"]["schema"].as_str(),
            Some("dcentos.mining.pipeline.snapshot.design.v2")
        );
        assert_eq!(
            body["snapshot_design"]["status"].as_str(),
            Some("implemented_default_off")
        );
        assert_eq!(body["snapshot_design"]["implemented"].as_bool(), Some(true));
        assert_eq!(
            body["snapshot_design"]["publisher_enabled"].as_bool(),
            Some(false)
        );
        assert_eq!(
            body["snapshot_design"]["snapshot_available"].as_bool(),
            Some(false)
        );
        assert_eq!(
            body["snapshot_design"]["live_route_mounted"].as_bool(),
            Some(true)
        );
        for block_id in [
            "job_freshness",
            "work_freshness",
            "nonce_freshness",
            "share_freshness",
        ] {
            let block = &body["snapshot_design"]["blocks"][block_id];
            assert_eq!(block["status"].as_str(), Some("unavailable"));
            assert!(block["last_update_ms"].is_null());
            assert!(block["age_ms"].is_null());
            assert!(block["stale_after_ms"].is_null());
            assert!(block["source"].is_null());
            assert!(block["null_reason"].as_str().is_some());
            assert_eq!(block["control_authority"].as_bool(), Some(false));
        }
        assert_eq!(
            body["promotion_checklist_schema"].as_str(),
            Some("dcentos.mining.pipeline.publisher.promotion.checklist.v1")
        );
        assert_eq!(
            body["publisher_promotion_checklist"]["schema"].as_str(),
            Some("dcentos.mining.pipeline.publisher.promotion.checklist.v1")
        );
        assert_eq!(
            body["publisher_promotion_checklist"]["status"].as_str(),
            Some("implemented_default_off")
        );
        assert_eq!(
            body["publisher_promotion_checklist"]["promotion_state"].as_str(),
            Some("blocked")
        );
        assert_eq!(
            body["publisher_promotion_checklist"]["implemented"].as_bool(),
            Some(true)
        );
        assert_eq!(
            body["publisher_promotion_checklist"]["route_required"].as_bool(),
            Some(true)
        );
        assert_eq!(
            body["publisher_promotion_checklist"]["dispatcher_reads"].as_bool(),
            Some(false)
        );
        assert_eq!(
            body["publisher_promotion_checklist"]["hardware_reads"].as_bool(),
            Some(false)
        );
        assert_eq!(
            body["publisher_promotion_checklist"]["blockers_schema"].as_str(),
            Some("dcentos.mining.pipeline.publisher.promotion.blocker.v1")
        );
        assert_eq!(
            body["publisher_promotion_checklist"]["blocker_count"].as_u64(),
            Some(7)
        );
        assert_eq!(
            body["publisher_promotion_checklist"]["active_blocker_count"].as_u64(),
            Some(4)
        );
        assert_eq!(
            body["publisher_promotion_checklist"]["all_blockers_active"].as_bool(),
            Some(false)
        );
        let active_blocker_ids = body["publisher_promotion_checklist"]["active_blocker_ids"]
            .as_array()
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| item.as_str())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        assert_eq!(
            active_blocker_ids,
            vec![
                "hardware_smoke_s9_not_run",
                "hardware_smoke_s19pro_not_run",
                "hardware_smoke_s21_not_run",
                "rollback_not_tested",
            ]
        );
        assert_eq!(
            active_blocker_ids.len() as u64,
            body["publisher_promotion_checklist"]["active_blocker_count"]
                .as_u64()
                .unwrap()
        );
        let active_blockers = body["publisher_promotion_checklist"]["blockers"]
            .as_array()
            .map(|items| {
                items
                    .iter()
                    .filter(|item| item["active"].as_bool() == Some(true))
                    .filter_map(|item| item["id"].as_str())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        assert_eq!(active_blocker_ids, active_blockers);
        assert_eq!(
            body["fleet_parser_notes_schema"].as_str(),
            Some("dcentos.mining.pipeline.fleet_parser_notes.v1")
        );
        assert_eq!(
            body["fleet_parser_notes"]["schema"].as_str(),
            Some("dcentos.mining.pipeline.fleet_parser_notes.v1")
        );
        assert_eq!(
            body["fleet_parser_notes"]["read_only"].as_bool(),
            Some(true)
        );
        assert_eq!(
            body["fleet_parser_notes"]["live_telemetry"].as_bool(),
            Some(false)
        );
        assert_eq!(
            body["fleet_parser_notes"]["readiness_evidence"].as_bool(),
            Some(false)
        );
        assert_eq!(
            body["fleet_parser_notes"]["static_aliases"]["active_blocker_ids"]["source_path"]
                .as_str(),
            Some("publisher_promotion_checklist.active_blocker_ids")
        );
        assert_eq!(
            body["fleet_parser_notes"]["static_aliases"]["active_blocker_ids"]["source"].as_str(),
            Some("static_manifest")
        );
        assert_eq!(
            body["fleet_parser_notes"]["static_aliases"]["active_blocker_ids"]
                ["readiness_evidence"]
                .as_bool(),
            Some(false)
        );
        assert!(
            body["fleet_parser_notes"]["static_aliases"]["active_blocker_ids"]
                ["not_authoritative_for"]
                .as_array()
                .map(|items| items
                    .iter()
                    .any(|item| item.as_str() == Some("blocker reason")))
                .unwrap_or(false)
        );
        assert_eq!(
            body["fleet_parser_notes"]["static_aliases"]["freshness_classifier_example_fixtures"]
                ["source_path"]
                .as_str(),
            Some("freshness_classifier.example_fixtures")
        );
        assert_eq!(
            body["fleet_parser_notes"]["static_aliases"]["freshness_classifier_example_fixtures"]
                ["must_not_display_as_miner_state"]
                .as_bool(),
            Some(true)
        );
        assert!(body["fleet_parser_notes"]["live_promotion_requires"]
            .as_array()
            .map(|items| {
                items
                    .iter()
                    .any(|item| item.as_str() == Some("S9 hardware smoke"))
                    && items
                        .iter()
                        .any(|item| item.as_str() == Some("S19 Pro hardware smoke"))
                    && items
                        .iter()
                        .any(|item| item.as_str() == Some("S21 hardware smoke"))
            })
            .unwrap_or(false));
        assert!(body["fleet_parser_notes"]["does_not_read"]
            .as_array()
            .map(|items| {
                items
                    .iter()
                    .any(|item| item.as_str() == Some("mining_sync"))
                    && items
                        .iter()
                        .any(|item| item.as_str() == Some("hardware registers"))
            })
            .unwrap_or(false));
        assert!(body["publisher_promotion_checklist"]["requirements"]
            .as_array()
            .map(|items| {
                items
                    .iter()
                    .any(|item| item["id"].as_str() == Some("design_v2_fields"))
                    && items
                        .iter()
                        .any(|item| item["id"].as_str() == Some("publisher_source_owner"))
                    && items
                        .iter()
                        .any(|item| item["id"].as_str() == Some("rollback_disable_path"))
            })
            .unwrap_or(false));
        assert_eq!(body["live_publisher"]["enabled"].as_bool(), Some(false));
        assert_eq!(
            body["live_publisher"]["source"].as_str(),
            Some("disabled_by_config")
        );
        assert_eq!(
            body["publisher_contract"]["transport"].as_str(),
            Some("watch_channel")
        );
        assert!(body["existing_surfaces"]
            .as_array()
            .map(|surfaces| {
                surfaces.iter().any(|surface| {
                    surface["id"].as_str() == Some("websocket_mining_sync")
                        && surface["rest_queryable"].as_bool() == Some(false)
                })
            })
            .unwrap_or(false));
        assert!(body["candidate_snapshot_fields"]
            .as_array()
            .map(|fields| {
                fields.iter().any(|field| {
                    field["id"].as_str() == Some("current_job_id")
                        && field["status"].as_str() == Some("unavailable")
                })
            })
            .unwrap_or(false));
        assert!(body["limitations"]
            .as_array()
            .map(|items| {
                items.iter().any(|item| {
                    item.as_str()
                        .map(|text| text.contains("Manifest-only"))
                        .unwrap_or(false)
                })
            })
            .unwrap_or(false));
    }

    #[test]
    fn build_mining_pipeline_manifest_response_receiver_present_reports_enabled_publisher() {
        let body = build_mining_pipeline_manifest_response(123_000, true);

        assert_eq!(body["status"].as_str(), Some("publisher_enabled"));
        assert_eq!(body["publisher_live"].as_bool(), Some(true));
        assert_eq!(body["snapshot_available"].as_bool(), Some(false));
        assert_eq!(
            body["publisher_gate"]["receiver_configured"].as_bool(),
            Some(true)
        );
        assert_eq!(
            body["publisher_gate"]["enabled_configs_rejected"].as_bool(),
            Some(false)
        );
        assert_eq!(
            body["publisher_gate"]["live_snapshot_endpoint"].as_str(),
            Some("/api/mining/pipeline/snapshot")
        );
        assert_eq!(body["live_publisher"]["enabled"].as_bool(), Some(true));
        assert_eq!(
            body["live_publisher"]["source"].as_str(),
            Some("receiver_present")
        );
        assert_eq!(
            body["snapshot_contract"]["snapshot_available"].as_bool(),
            Some(false)
        );
        assert_eq!(
            body["freshness_contract"]["snapshot_available_only_when"].as_str(),
            Some("status == live")
        );
        assert_eq!(
            body["publisher_design"]["implemented"].as_bool(),
            Some(true)
        );
        assert_eq!(
            body["publisher_design"]["live_route_mounted"].as_bool(),
            Some(true)
        );
        assert_eq!(body["snapshot_design"]["implemented"].as_bool(), Some(true));
        assert_eq!(
            body["snapshot_design"]["snapshot_available"].as_bool(),
            Some(false)
        );
        assert_eq!(
            body["snapshot_design"]["blocks"]["job_freshness"]["status"].as_str(),
            Some("unavailable")
        );
        assert_eq!(
            body["publisher_promotion_checklist"]["promotion_state"].as_str(),
            Some("blocked")
        );
        assert_eq!(
            body["publisher_promotion_checklist"]["snapshot_available"].as_bool(),
            Some(false)
        );
    }

    #[test]
    fn mining_pipeline_snapshot_response_without_receiver_is_unavailable() {
        let snapshot = build_mining_pipeline_snapshot_response(None, 456_000, 5_000);

        assert_eq!(snapshot.generated_at_ms, 456_000);
        assert_eq!(snapshot.publisher_enabled, false);
        assert_eq!(snapshot.snapshot_available, false);
        assert_eq!(snapshot.read_only, true);
        assert_eq!(snapshot.control_actions, false);
        assert_eq!(snapshot.hardware_writes, false);
        assert_eq!(snapshot.filesystem_mutation, false);
        assert_eq!(snapshot.publisher_last_update_ms, None);
        assert_eq!(snapshot.source, "disabled_pipeline_snapshot_gate");
    }

    #[test]
    fn mining_pipeline_snapshot_response_clones_receiver_and_normalizes_freshness() {
        let published = crate::MiningPipelineSnapshot {
            publisher_enabled: true,
            publisher_last_update_ms: Some(100_000),
            source: "test_publisher".to_string(),
            ..crate::MiningPipelineSnapshot::default()
        };
        let (_tx, rx) = tokio::sync::watch::channel(published);

        let snapshot = build_mining_pipeline_snapshot_response(Some(&rx), 104_000, 5_000);

        assert_eq!(snapshot.generated_at_ms, 104_000);
        assert_eq!(snapshot.publisher_enabled, true);
        assert_eq!(snapshot.snapshot_available, true);
        assert_eq!(snapshot.snapshot_age_ms, Some(4_000));
        assert_eq!(snapshot.read_only, true);
        assert_eq!(snapshot.control_actions, false);
        assert_eq!(snapshot.hardware_writes, false);
        assert_eq!(snapshot.filesystem_mutation, false);
        assert_eq!(snapshot.source, "test_publisher");
    }

    #[test]
    fn build_mining_pipeline_snapshot_schema_response_reports_disabled_schema_only_contract() {
        let body = build_mining_pipeline_snapshot_schema_response(456_000);

        assert_eq!(
            body["schema"].as_str(),
            Some("dcentos.mining.pipeline.snapshot.schema.v1")
        );
        assert_eq!(
            body["snapshot_schema"].as_str(),
            Some(crate::MINING_PIPELINE_SNAPSHOT_SCHEMA)
        );
        assert_eq!(body["status"].as_str(), Some("default_off"));
        assert_eq!(body["read_only"].as_bool(), Some(true));
        assert_eq!(body["control_actions"].as_bool(), Some(false));
        assert_eq!(body["hardware_writes"].as_bool(), Some(false));
        assert_eq!(body["filesystem_mutation"].as_bool(), Some(false));
        assert_eq!(body["content_collected"].as_bool(), Some(false));
        assert_eq!(body["probe_performed"].as_bool(), Some(false));
        assert_eq!(body["handlers_executed"].as_bool(), Some(false));
        assert_eq!(body["publisher_default_enabled"].as_bool(), Some(false));
        assert_eq!(
            body["live_snapshot_endpoint"].as_str(),
            Some("/api/mining/pipeline/snapshot")
        );
        assert_eq!(
            body["config_gate"]["toml_path"].as_str(),
            Some("mining.pipeline_snapshot.enabled")
        );
        assert_eq!(
            body["config_gate"]["enabled_configs_rejected"].as_bool(),
            Some(false)
        );
        assert_eq!(
            body["config_gate"]["live_snapshot_endpoint"].as_str(),
            Some("/api/mining/pipeline/snapshot")
        );
        assert_eq!(
            body["default_snapshot"]["snapshot_available"].as_bool(),
            Some(false)
        );
        assert_eq!(
            body["freshness_contract"]["default_stale_after_ms"].as_u64(),
            Some(crate::MINING_PIPELINE_SNAPSHOT_DEFAULT_STALE_AFTER_MS)
        );
        assert_eq!(
            body["freshness_classifier_schema"].as_str(),
            Some(crate::MINING_PIPELINE_FRESHNESS_CLASSIFIER_SCHEMA)
        );
        assert_eq!(
            body["freshness_classifier"]["status"].as_str(),
            Some("design_only")
        );
        assert_eq!(
            body["freshness_classifier"]["runtime_wired"].as_bool(),
            Some(false)
        );
        assert_eq!(
            body["freshness_classifier"]["publisher_enabled"].as_bool(),
            Some(false)
        );
        assert_eq!(
            body["freshness_classifier"]["snapshot_available"].as_bool(),
            Some(false)
        );
        assert!(body["freshness_classifier"]["fail_closed_when"]
            .as_array()
            .map(|items| {
                items.iter().any(|item| {
                    item.as_str()
                        .map(|text| text.contains("domain_last_update_ms is null"))
                        .unwrap_or(false)
                }) && items.iter().any(|item| {
                    item.as_str()
                        .map(|text| text.contains("age would be negative"))
                        .unwrap_or(false)
                })
            })
            .unwrap_or(false));
        assert_eq!(
            body["freshness_classifier"]["example_fixtures_schema"].as_str(),
            Some("dcentos.mining.pipeline.freshness.classifier.fixture.v1")
        );
        assert_eq!(
            body["freshness_classifier"]["example_fixture_count"].as_u64(),
            Some(5)
        );
        assert!(body["freshness_classifier"]["example_fixtures"]
            .as_array()
            .map(|items| {
                items.len() == 5
                    && items.iter().all(|item| {
                        item["design_only"].as_bool() == Some(true)
                            && item["non_telemetry"].as_bool() == Some(true)
                            && item["dispatcher_reads"].as_bool() == Some(false)
                            && item["hardware_reads"].as_bool() == Some(false)
                            && item["pool_socket_reads"].as_bool() == Some(false)
                            && item["content_collected"].as_bool() == Some(false)
                            && item["probe_performed"].as_bool() == Some(false)
                            && item["handlers_executed"].as_bool() == Some(false)
                    })
                    && items
                        .iter()
                        .any(|item| item["id"].as_str() == Some("future_clock_skew"))
                    && items
                        .iter()
                        .any(|item| item["id"].as_str() == Some("invalid"))
                    && items.iter().any(|item| {
                        item["id"].as_str() == Some("live")
                            && item["expected_classifier_status"].as_str() == Some("live")
                            && item["expected_snapshot_status"].as_str() == Some("live")
                            && item["snapshot_available"].as_bool() == Some(true)
                    })
            })
            .unwrap_or(false));
        assert!(body["freshness_contract"]["does_not_populate"]
            .as_array()
            .map(|items| {
                items
                    .iter()
                    .any(|item| item.as_str() == Some("current_job_id"))
                    && items
                        .iter()
                        .any(|item| item.as_str() == Some("nonce_bursts_total"))
            })
            .unwrap_or(false));
        assert_eq!(
            body["publisher_design"]["config_gate"].as_str(),
            Some("mining.pipeline_snapshot.enabled")
        );
        assert_eq!(
            body["publisher_design"]["enabled_configs_rejected"].as_bool(),
            Some(false)
        );
        assert_eq!(
            body["publisher_design"]["bounded_publish_cadence"]["min_interval_ms"].as_u64(),
            Some(1000)
        );
        assert!(body["publisher_design"]["hardware_smoke_required"]
            .as_array()
            .map(|items| {
                items
                    .iter()
                    .any(|item| item["model"].as_str() == Some("Antminer S9"))
                    && items
                        .iter()
                        .any(|item| item["model"].as_str() == Some("Antminer S19 Pro"))
                    && items
                        .iter()
                        .any(|item| item["model"].as_str() == Some("Antminer S21"))
            })
            .unwrap_or(false));
        assert_eq!(
            body["snapshot_design_schema"].as_str(),
            Some("dcentos.mining.pipeline.snapshot.design.v2")
        );
        assert_eq!(
            body["snapshot_design"]["target_snapshot_schema"].as_str(),
            Some(crate::MINING_PIPELINE_SNAPSHOT_SCHEMA)
        );
        assert_eq!(
            body["snapshot_design"]["domain_freshness_status"].as_str(),
            Some("available_when_enabled_and_events_received")
        );
        assert_eq!(body["snapshot_design"]["read_only"].as_bool(), Some(true));
        assert_eq!(
            body["snapshot_design"]["control_actions"].as_bool(),
            Some(false)
        );
        assert_eq!(
            body["snapshot_design"]["hardware_writes"].as_bool(),
            Some(false)
        );
        assert!(body["snapshot_design"]["forbidden"]
            .as_array()
            .map(|items| {
                items.iter().any(|item| {
                    item.as_str()
                        .map(|text| text.contains("mining_sync"))
                        .unwrap_or(false)
                }) && items.iter().any(|item| {
                    item.as_str()
                        .map(|text| text.contains("per-nonce"))
                        .unwrap_or(false)
                }) && items.iter().any(|item| {
                    item.as_str()
                        .map(|text| text.contains("hardware"))
                        .unwrap_or(false)
                })
            })
            .unwrap_or(false));
        assert!(body["snapshot_design"]["hardware_smoke_required"]
            .as_array()
            .map(|items| {
                items
                    .iter()
                    .any(|item| item["model"].as_str() == Some("Antminer S9"))
                    && items
                        .iter()
                        .any(|item| item["model"].as_str() == Some("Antminer S19 Pro"))
                    && items
                        .iter()
                        .any(|item| item["model"].as_str() == Some("Antminer S21"))
            })
            .unwrap_or(false));
        for block_id in [
            "job_freshness",
            "work_freshness",
            "nonce_freshness",
            "share_freshness",
        ] {
            let block = &body["snapshot_design"]["blocks"][block_id];
            assert_eq!(block["status"].as_str(), Some("unavailable"));
            assert!(block["last_update_ms"].is_null());
            assert!(block["age_ms"].is_null());
            assert!(block["stale_after_ms"].is_null());
            assert!(block["source"].is_null());
            assert!(block["null_reason"].as_str().is_some());
            assert!(block["future_fields"]
                .as_array()
                .map(|items| !items.is_empty())
                .unwrap_or(false));
        }
        assert_eq!(
            body["promotion_checklist_schema"].as_str(),
            Some("dcentos.mining.pipeline.publisher.promotion.checklist.v1")
        );
        assert_eq!(
            body["publisher_promotion_checklist"]["status"].as_str(),
            Some("implemented_default_off")
        );
        assert_eq!(
            body["publisher_promotion_checklist"]["promotion_state"].as_str(),
            Some("blocked")
        );
        assert_eq!(
            body["publisher_promotion_checklist"]["control_actions"].as_bool(),
            Some(false)
        );
        assert_eq!(
            body["publisher_promotion_checklist"]["hardware_writes"].as_bool(),
            Some(false)
        );
        assert_eq!(
            body["publisher_promotion_checklist"]["filesystem_mutation"].as_bool(),
            Some(false)
        );
        assert_eq!(
            body["publisher_promotion_checklist"]["active_blocker_count"].as_u64(),
            Some(4)
        );
        let active_blocker_ids = body["publisher_promotion_checklist"]["active_blocker_ids"]
            .as_array()
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| item.as_str())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        assert_eq!(
            active_blocker_ids,
            vec![
                "hardware_smoke_s9_not_run",
                "hardware_smoke_s19pro_not_run",
                "hardware_smoke_s21_not_run",
                "rollback_not_tested",
            ]
        );
        assert_eq!(
            active_blocker_ids.len() as u64,
            body["publisher_promotion_checklist"]["active_blocker_count"]
                .as_u64()
                .unwrap()
        );
        assert_eq!(
            body["fleet_parser_notes_schema"].as_str(),
            Some("dcentos.mining.pipeline.fleet_parser_notes.v1")
        );
        assert_eq!(
            body["fleet_parser_notes"]["schema"].as_str(),
            Some("dcentos.mining.pipeline.fleet_parser_notes.v1")
        );
        assert_eq!(
            body["fleet_parser_notes"]["read_only"].as_bool(),
            Some(true)
        );
        assert_eq!(
            body["fleet_parser_notes"]["live_telemetry"].as_bool(),
            Some(false)
        );
        assert_eq!(
            body["fleet_parser_notes"]["readiness_evidence"].as_bool(),
            Some(false)
        );
        assert_eq!(
            body["fleet_parser_notes"]["static_aliases"]["active_blocker_ids"]["mirrors"].as_str(),
            Some("publisher_promotion_checklist.blockers where active == true")
        );
        assert_eq!(
            body["fleet_parser_notes"]["static_aliases"]["active_blocker_ids"]["missing_means"]
                .as_str(),
            Some("treat promotion_state as blocked")
        );
        assert_eq!(
            body["fleet_parser_notes"]["static_aliases"]["freshness_classifier_example_fixtures"]
                ["source"]
                .as_str(),
            Some("static_design_fixture")
        );
        assert_eq!(
            body["fleet_parser_notes"]["static_aliases"]["freshness_classifier_example_fixtures"]
                ["live_telemetry"]
                .as_bool(),
            Some(false)
        );
        assert!(body["fleet_parser_notes"]["does_not_clear"]
            .as_array()
            .map(|items| {
                items
                    .iter()
                    .any(|item| item.as_str() == Some("hardware_smoke_s9_not_run"))
                    && items
                        .iter()
                        .any(|item| item.as_str() == Some("rollback_not_tested"))
            })
            .unwrap_or(false));
        assert!(body["publisher_promotion_checklist"]["blockers"]
            .as_array()
            .map(|items| {
                items
                    .iter()
                    .filter(|item| item["active"].as_bool() == Some(true))
                    .all(|item| {
                        item["id"].as_str().is_some_and(|id| {
                            id.starts_with("hardware_smoke_") || id == "rollback_not_tested"
                        })
                    })
                    && items
                        .iter()
                        .any(|item| item["id"].as_str() == Some("hardware_smoke_s9_not_run"))
                    && items
                        .iter()
                        .any(|item| item["id"].as_str() == Some("hardware_smoke_s19pro_not_run"))
                    && items
                        .iter()
                        .any(|item| item["id"].as_str() == Some("hardware_smoke_s21_not_run"))
            })
            .unwrap_or(false));
        assert!(body["publisher_promotion_checklist"]["forbidden"]
            .as_array()
            .map(|items| {
                items.iter().any(|item| {
                    item.as_str()
                        .map(|text| text.contains("mining_sync"))
                        .unwrap_or(false)
                }) && items.iter().any(|item| {
                    item.as_str()
                        .map(|text| text.contains("hardware"))
                        .unwrap_or(false)
                }) && items.iter().any(|item| {
                    item.as_str()
                        .map(|text| text.contains("per nonce"))
                        .unwrap_or(false)
                })
            })
            .unwrap_or(false));
        assert!(body["publisher_promotion_checklist"]["requirements"]
            .as_array()
            .map(|items| {
                items
                    .iter()
                    .any(|item| item["id"].as_str() == Some("hardware_smoke_s9"))
                    && items
                        .iter()
                        .any(|item| item["id"].as_str() == Some("hardware_smoke_s19pro"))
                    && items
                        .iter()
                        .any(|item| item["id"].as_str() == Some("hardware_smoke_s21"))
            })
            .unwrap_or(false));
        assert!(body["fields"]
            .as_array()
            .map(|fields| {
                fields
                    .iter()
                    .any(|field| field["name"].as_str() == Some("current_job_id"))
                    && fields
                        .iter()
                        .any(|field| field["name"].as_str() == Some("dispatch_queue_depth"))
                    && fields
                        .iter()
                        .any(|field| field["name"].as_str() == Some("nonce_bursts_total"))
            })
            .unwrap_or(false));
        assert!(body["forbidden"]
            .as_array()
            .map(|items| {
                items.iter().any(|item| {
                    item.as_str()
                        .map(|text| text.contains("dispatcher internals"))
                        .unwrap_or(false)
                }) && items.iter().any(|item| {
                    item.as_str()
                        .map(|text| text.contains("mining_sync"))
                        .unwrap_or(false)
                }) && items.iter().any(|item| {
                    item.as_str()
                        .map(|text| text.contains("FPGA"))
                        .unwrap_or(false)
                })
            })
            .unwrap_or(false));
    }

    #[test]
    fn read_upgrade_stage_entries_returns_empty_for_missing_root() {
        let root = std::env::temp_dir().join(format!(
            "dcentos-missing-stage-{}-{}",
            std::process::id(),
            unique_test_suffix()
        ));

        assert!(read_upgrade_stage_entries(&root).is_empty());
    }

    #[test]
    fn read_upgrade_stage_entries_marks_tar_case_insensitively_and_preserves_metadata() {
        let root = make_test_stage_dir("metadata");
        let tar_path = root.join("DCENTOS-SYSUPGRADE.TAR");
        let text_path = root.join("notes.txt");

        std::fs::write(&tar_path, b"tar payload").expect("write tar fixture");
        std::fs::write(&text_path, b"notes").expect("write text fixture");

        let entries = read_upgrade_stage_entries(&root);
        let tar_entry = entries
            .iter()
            .find(|entry| entry.filename == "DCENTOS-SYSUPGRADE.TAR")
            .expect("tar fixture should be reported");
        let text_entry = entries
            .iter()
            .find(|entry| entry.filename == "notes.txt")
            .expect("text fixture should be reported");

        assert!(tar_entry.is_tar);
        assert_eq!(tar_entry.size_bytes, 11);
        assert!(tar_entry.modified_ms.is_some());
        assert!(!text_entry.is_tar);
        assert_eq!(text_entry.size_bytes, 5);

        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn read_upgrade_stage_entries_limits_depth() {
        let root = make_test_stage_dir("depth");
        let depth_two = root.join("one").join("two");
        let depth_three = depth_two.join("three");
        std::fs::create_dir_all(&depth_three).expect("create nested fixture dirs");
        std::fs::write(depth_two.join("visible.tar"), b"ok").expect("write visible fixture");
        std::fs::write(depth_three.join("hidden.tar"), b"too deep").expect("write hidden fixture");

        let entries = read_upgrade_stage_entries(&root);

        assert!(entries.iter().any(|entry| entry.filename == "visible.tar"));
        assert!(!entries.iter().any(|entry| entry.filename == "hidden.tar"));

        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn read_upgrade_stage_entries_limits_entry_count() {
        let root = make_test_stage_dir("count");
        for index in 0..40 {
            std::fs::write(root.join(format!("bulk-{index:02}.tar")), b"x")
                .expect("write bulk fixture");
        }

        let entries = read_upgrade_stage_entries(&root);

        assert!(
            entries.len() <= 32,
            "stage scanner should cap reported entries"
        );

        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn build_system_upgrade_status_reports_idle() {
        let fwenv = UpgradeFwEnvSnapshot::default();
        let body = build_system_upgrade_status_payload("/tmp/dcentos-upgrade", false, &[], &fwenv);

        assert_eq!(body["status"].as_str(), Some("ok"));
        assert_eq!(body["read_only"].as_bool(), Some(true));
        assert_eq!(body["state"].as_str(), Some("idle"));
        assert_eq!(body["stage_root_present"].as_bool(), Some(false));
        assert_eq!(body["staged_package_count"].as_u64(), Some(0));
        assert!(body["upgrade_stage"].is_null());
        assert!(body["bootcount"].is_null());
        assert!(body["bootlimit"].is_null());
        assert!(body["boot_slot"].is_null());
    }

    #[test]
    fn build_system_upgrade_status_reports_staged_tar_only() {
        let entries = vec![
            stage_entry("dcentos-sysupgrade.tar", true),
            stage_entry("notes.txt", false),
        ];
        let fwenv = UpgradeFwEnvSnapshot::default();

        let body =
            build_system_upgrade_status_payload("/tmp/dcentos-upgrade", true, &entries, &fwenv);

        assert_eq!(body["state"].as_str(), Some("validated_or_staged"));
        assert_eq!(body["staged_package_count"].as_u64(), Some(1));
        assert_eq!(
            body["staged_packages"][0]["filename"].as_str(),
            Some("dcentos-sysupgrade.tar")
        );
        assert_eq!(
            body["staged_packages"][0]["source"].as_str(),
            Some("browser_staging_dir")
        );
        assert!(body["upgrade_stage"].is_null());
    }

    #[test]
    fn build_system_upgrade_status_reports_null_fwenv_when_unavailable() {
        let fwenv = UpgradeFwEnvSnapshot::default();
        let body = build_system_upgrade_status_payload("/tmp/dcentos-upgrade", true, &[], &fwenv);

        assert!(body["upgrade_stage"].is_null());
        assert!(body["bootcount"].is_null());
        assert!(body["bootlimit"].is_null());
        assert!(body["boot_slot"].is_null());
    }

    #[test]
    fn build_system_upgrade_status_reports_pending_boot_commit_for_upgrade_stage_zero() {
        let entries = vec![stage_entry("dcentos-sysupgrade.tar", true)];
        let fwenv = UpgradeFwEnvSnapshot {
            upgrade_stage: Some("0".to_string()),
            bootcount: Some("1".to_string()),
            bootlimit: Some("3".to_string()),
            ..Default::default()
        };

        let body =
            build_system_upgrade_status_payload("/tmp/dcentos-upgrade", true, &entries, &fwenv);

        assert_eq!(body["state"].as_str(), Some("pending_boot_commit"));
        assert_eq!(body["upgrade_stage"].as_str(), Some("0"));
        assert_eq!(body["bootcount"].as_str(), Some("1"));
        assert_eq!(body["bootlimit"].as_str(), Some("3"));
        assert_eq!(body["staged_package_count"].as_u64(), Some(1));
    }

    #[test]
    fn build_system_upgrade_status_reports_pending_boot_commit_for_upgrade_stage_one() {
        let fwenv = UpgradeFwEnvSnapshot {
            upgrade_stage: Some("1".to_string()),
            ..Default::default()
        };

        let body = build_system_upgrade_status_payload("/tmp/dcentos-upgrade", true, &[], &fwenv);

        assert_eq!(body["state"].as_str(), Some("pending_boot_commit"));
        assert_eq!(body["upgrade_stage"].as_str(), Some("1"));
        assert_eq!(body["staged_package_count"].as_u64(), Some(0));
    }

    #[test]
    fn build_system_upgrade_status_uses_boot_slot_fallback_order() {
        let cases = [
            (
                UpgradeFwEnvSnapshot {
                    boot_slot: Some("boot".to_string()),
                    dcent_boot_slot: Some("dcent".to_string()),
                    active_slot: Some("active".to_string()),
                    ..Default::default()
                },
                "boot",
            ),
            (
                UpgradeFwEnvSnapshot {
                    dcent_boot_slot: Some("dcent".to_string()),
                    active_slot: Some("active".to_string()),
                    ..Default::default()
                },
                "dcent",
            ),
            (
                UpgradeFwEnvSnapshot {
                    active_slot: Some("active".to_string()),
                    ..Default::default()
                },
                "active",
            ),
        ];

        for (fwenv, expected) in cases {
            let body =
                build_system_upgrade_status_payload("/tmp/dcentos-upgrade", true, &[], &fwenv);
            assert_eq!(body["boot_slot"].as_str(), Some(expected));
        }
    }

    #[test]
    fn build_log_manifest_response_reports_metadata_only_sources() {
        let body = build_log_manifest_response();

        assert_eq!(body.status, "ok");
        assert!(body.read_only);
        assert!(!body.content_collected);
        assert!(body
            .sources
            .iter()
            .any(|source| source.id == "dcentrald-runtime"
                && source.path == "/tmp/dcentrald.log"
                && source.content_endpoint == Some("/api/debug/log?lines=200")
                && source.content_access == "mode_gated_content_endpoint"));
        assert!(body
            .sources
            .iter()
            .any(|source| source.id == "dashboard-server"
                && source.path == "/tmp/dashboard.log"
                && source.content_access == "not_exposed_metadata_only"));
        assert!(body
            .limitations
            .iter()
            .any(|item| item.contains("never returns log lines")));
    }

    #[test]
    fn read_bounded_log_tail_returns_last_lines() {
        let root = make_test_stage_dir("log-tail-basic");
        let path = root.join("dcentrald.log");
        std::fs::write(&path, "line 1\nline 2\nline 3\nline 4\n").expect("write log");

        let tail = read_bounded_log_tail(&path, 2, None, 1024).expect("tail log");

        assert_eq!(tail.lines, vec!["line 3".to_string(), "line 4".to_string()]);
        assert_eq!(tail.file_size_bytes, 28);
        assert_eq!(tail.read_bytes, 28);
        assert!(!tail.truncated);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn read_bounded_log_tail_filters_after_tail_selection() {
        let root = make_test_stage_dir("log-tail-grep");
        let path = root.join("dcentrald.log");
        std::fs::write(&path, "error old\ninfo current\nerror current\n").expect("write log");

        let tail = read_bounded_log_tail(&path, 2, Some("error"), 1024).expect("tail log");

        assert_eq!(tail.lines, vec!["error current".to_string()]);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn read_bounded_log_tail_limits_read_window() {
        let root = make_test_stage_dir("log-tail-window");
        let path = root.join("dcentrald.log");
        let content = (0..200)
            .map(|idx| format!("line {idx:03} filler filler filler"))
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(&path, content).expect("write log");

        let tail = read_bounded_log_tail(&path, 10, None, 128).expect("tail log");

        assert!(tail.truncated);
        assert!(tail.read_bytes <= 128);
        assert!(tail.lines.len() <= 10);
        assert!(tail
            .lines
            .last()
            .is_some_and(|line| line.contains("line 199")));
        let _ = std::fs::remove_dir_all(root);
    }

    fn stage_entry(filename: &str, is_tar: bool) -> UpgradeStageEntry {
        UpgradeStageEntry {
            path: format!("/tmp/dcentos-upgrade/{filename}"),
            filename: filename.to_string(),
            size_bytes: 123,
            modified_ms: Some(456),
            is_tar,
        }
    }

    fn make_test_stage_dir(label: &str) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!(
            "dcentos-stage-{label}-{}-{}",
            std::process::id(),
            unique_test_suffix()
        ));
        std::fs::create_dir_all(&root).expect("create temp stage dir");
        root
    }

    fn unique_test_suffix() -> u128 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system time before UNIX_EPOCH")
            .as_nanos()
    }

    // ─── Freedom-first onboarding: password-opt-out tests ────────────
    //
    // Pure / host-safe — no filesystem, no axum runtime. These pin the
    // load-bearing decision logic the handlers run.

    /// A state that satisfies everything EXCEPT the password decision.
    fn onboarding_state_modulo_password() -> OnboardingState {
        let mut s = OnboardingState::default();
        s.steps.complete = true;
        s.steps.mode_configured = true;
        s.steps.safety_ack = true;
        s.auth.password_set = false;
        s.auth.password_opt_out = false;
        s
    }

    #[test]
    fn onboarding_incomplete_without_any_password_decision() {
        let s = onboarding_state_modulo_password();
        // No password, no opt-out → setup is NOT complete (the only nag).
        assert!(!onboarding_is_complete(&s));
        assert!(!onboarding_password_decision_made(&s.auth));
    }

    #[test]
    fn onboarding_complete_with_password_opt_out() {
        let mut s = onboarding_state_modulo_password();
        s.auth.password_opt_out = true;
        // Freedom-first: explicit opt-out completes onboarding without a
        // password. mode + safety_ack are still required.
        assert!(onboarding_is_complete(&s));
        assert!(onboarding_password_decision_made(&s.auth));
    }

    #[test]
    fn onboarding_complete_with_password_set() {
        let mut s = onboarding_state_modulo_password();
        s.auth.password_set = true;
        assert!(onboarding_is_complete(&s));
        assert!(onboarding_password_decision_made(&s.auth));
    }

    #[test]
    fn onboarding_opt_out_still_requires_mode_and_safety() {
        let mut s = onboarding_state_modulo_password();
        s.auth.password_opt_out = true;
        s.steps.mode_configured = false;
        assert!(
            !onboarding_is_complete(&s),
            "opt-out must NOT bypass the mode/safety gates"
        );
        s.steps.mode_configured = true;
        s.steps.safety_ack = false;
        assert!(!onboarding_is_complete(&s));
    }

    #[test]
    fn post_setup_complete_gate_allows_opt_out() {
        // Mirrors the exact `post_setup_complete` 400 gate: it returns 400
        // ONLY when no password decision has been made.
        let mut auth = OnboardingAuthState::default();
        // Neither set nor opted out → would 400.
        assert!(!onboarding_password_decision_made(&auth));
        // Opted out → 200 path (gate passes).
        auth.password_opt_out = true;
        assert!(onboarding_password_decision_made(&auth));
        // Password set → 200 path (gate passes).
        let auth2 = OnboardingAuthState {
            password_set: true,
            ..OnboardingAuthState::default()
        };
        assert!(onboarding_password_decision_made(&auth2));
    }

    #[test]
    fn password_opt_out_cleared_when_password_set() {
        // Reconciliation: a real password supersedes the opt-out.
        let mut auth = OnboardingAuthState {
            password_set: true,
            password_opt_out: true,
            ..OnboardingAuthState::default()
        };
        let mutated = reconcile_password_opt_out(&mut auth);
        assert!(mutated, "reconciliation must report it changed the state");
        assert!(!auth.password_opt_out, "password supersedes the opt-out");
        assert!(auth.password_set);
        // Idempotent: a second pass is a no-op.
        assert!(!reconcile_password_opt_out(&mut auth));
    }

    #[test]
    fn reconcile_no_op_when_only_opt_out() {
        // Opt-out WITHOUT a password must be preserved (the freedom choice).
        let mut auth = OnboardingAuthState {
            password_set: false,
            password_opt_out: true,
            ..OnboardingAuthState::default()
        };
        assert!(!reconcile_password_opt_out(&mut auth));
        assert!(auth.password_opt_out, "opt-out without a password stays");
    }

    #[test]
    fn onboarding_auth_state_deserializes_legacy_without_opt_out() {
        // Old onboarding.json files have no `password_opt_out` field —
        // serde(default) must keep them deserializing cleanly (defaults
        // to false, i.e. no accidental opt-out on upgrade).
        let legacy = r#"{ "password_set": true, "token_issued": false }"#;
        let auth: OnboardingAuthState =
            serde_json::from_str(legacy).expect("legacy auth deserializes");
        assert!(auth.password_set);
        assert!(!auth.password_opt_out, "missing field defaults to false");
    }

    // ─── Freedom-first onboarding: safety/circuit-opt-out tests ──────
    //
    // The exact parallel of the password-opt-out tests above. These pin
    // the load-bearing decision logic for the full-wizard-skip contract:
    // the circuit/breaker/safety-ack step must be a first-class opt-out,
    // never a wall in front of the dashboard + logs.

    /// A state that satisfies everything EXCEPT the safety decision.
    fn onboarding_state_modulo_safety() -> OnboardingState {
        let mut s = OnboardingState::default();
        s.steps.complete = true;
        s.steps.mode_configured = true;
        s.steps.safety_ack = false;
        s.steps.safety_opt_out = false;
        s.auth.password_set = true;
        s.auth.password_opt_out = false;
        s
    }

    #[test]
    fn onboarding_incomplete_without_any_safety_decision() {
        let s = onboarding_state_modulo_safety();
        // No safety_ack, no opt-out → setup is NOT complete (the only nag).
        assert!(!onboarding_is_complete(&s));
        assert!(!onboarding_safety_decision_made(&s.steps));
    }

    #[test]
    fn onboarding_complete_with_safety_opt_out() {
        let mut s = onboarding_state_modulo_safety();
        s.steps.safety_opt_out = true;
        // Freedom-first: explicit opt-out completes onboarding without the
        // circuit/safety check. mode + a password decision are still
        // required.
        assert!(onboarding_is_complete(&s));
        assert!(onboarding_safety_decision_made(&s.steps));
    }

    #[test]
    fn onboarding_complete_with_safety_ack() {
        let mut s = onboarding_state_modulo_safety();
        s.steps.safety_ack = true;
        assert!(onboarding_is_complete(&s));
        assert!(onboarding_safety_decision_made(&s.steps));
    }

    #[test]
    fn onboarding_safety_opt_out_still_requires_mode_and_password() {
        let mut s = onboarding_state_modulo_safety();
        s.steps.safety_opt_out = true;
        s.steps.mode_configured = false;
        assert!(
            !onboarding_is_complete(&s),
            "safety opt-out must NOT bypass the mode/password gates"
        );
        s.steps.mode_configured = true;
        s.auth.password_set = false;
        s.auth.password_opt_out = false;
        assert!(!onboarding_is_complete(&s));
    }

    #[test]
    fn full_skip_both_opt_outs_completes_onboarding() {
        // The headline of this work: a fresh unit where the operator
        // skipped the ENTIRE wizard (no password AND no circuit check)
        // must still report onboarding complete so the dashboard + logs
        // are reachable. mode is still required (safe-default Standard is
        // applied by the terminal-skip path).
        let mut s = OnboardingState::default();
        s.steps.complete = true;
        s.steps.mode_configured = true;
        s.steps.safety_ack = false;
        s.steps.safety_opt_out = true;
        s.auth.password_set = false;
        s.auth.password_opt_out = true;
        assert!(
            onboarding_is_complete(&s),
            "full wizard skip (both opt-outs) must complete onboarding"
        );
        assert!(onboarding_safety_decision_made(&s.steps));
        assert!(onboarding_password_decision_made(&s.auth));
    }

    #[test]
    fn safety_opt_out_cleared_when_safety_acked() {
        // Reconciliation: a real safety acknowledgement supersedes the
        // opt-out (the operator later completed the circuit check).
        let mut steps = OnboardingStepState {
            safety_ack: true,
            safety_opt_out: true,
            ..OnboardingStepState::default()
        };
        let mutated = reconcile_safety_opt_out(&mut steps);
        assert!(mutated, "reconciliation must report it changed the state");
        assert!(
            !steps.safety_opt_out,
            "a real safety ack supersedes the opt-out"
        );
        assert!(steps.safety_ack);
        // Idempotent: a second pass is a no-op.
        assert!(!reconcile_safety_opt_out(&mut steps));
    }

    #[test]
    fn reconcile_safety_no_op_when_only_opt_out() {
        // Opt-out WITHOUT an ack must be preserved (the freedom choice).
        let mut steps = OnboardingStepState {
            safety_ack: false,
            safety_opt_out: true,
            ..OnboardingStepState::default()
        };
        assert!(!reconcile_safety_opt_out(&mut steps));
        assert!(steps.safety_opt_out, "opt-out without a safety ack stays");
    }

    #[test]
    fn onboarding_step_state_deserializes_legacy_without_safety_opt_out() {
        // Old onboarding.json files have no `safety_opt_out` field —
        // serde(default) must keep them deserializing cleanly (defaults
        // to false, i.e. no accidental opt-out on upgrade).
        let legacy =
            r#"{ "safety_ack": true, "circuit_configured": false, "mode_configured": true }"#;
        let steps: OnboardingStepState =
            serde_json::from_str(legacy).expect("legacy steps deserialize");
        assert!(steps.safety_ack);
        assert!(!steps.safety_opt_out, "missing field defaults to false");
    }

    // ─── OMEGA P2-9 / D-20: mining_ready vs the skippable circuit step ──
    //
    // `mining_ready` must never report `false` for a unit that is actually
    // mining, and must not hinge on the optional AC-values circuit step
    // (which the operator can opt out of, and which DC/solar installs never
    // set). The daemon's real gate is `mining.enabled && pool configured` —
    // no circuit dependency.

    /// A fully-onboarded, pool-configured, non-solar state with the optional
    /// circuit step SKIPPED (operator opted out → `circuit_configured=false`).
    fn onboarding_state_mining_ready_modulo_circuit() -> OnboardingState {
        let mut s = OnboardingState::default();
        s.steps.complete = true;
        s.steps.mode_configured = true;
        s.steps.safety_opt_out = true; // circuit/safety DECISION made (opted out)
        s.steps.circuit_configured = false; // AC volts+amps NOT entered (skipped)
        s.steps.pool_configured = true;
        s.auth.password_opt_out = true; // password DECISION made
        s
    }

    #[test]
    fn mining_ready_true_when_actively_mining_even_if_circuit_skipped() {
        // THE bug (D-20): `is_mining:true` while `mining_ready:false`. Ground
        // truth wins — an actively-mining unit reports mining_ready=true even
        // with the circuit step skipped AND even if onboarding is otherwise
        // incomplete (the live override short-circuits the wizard predicate).
        let mut s = OnboardingState::default();
        s.steps.circuit_configured = false; // circuit step skipped
        assert!(
            !onboarding_is_complete(&s),
            "deliberately incomplete onboarding"
        );
        assert!(
            onboarding_is_mining_ready(&s, false, /* actively_mining */ true),
            "an actively-mining unit must report mining_ready=true"
        );
    }

    #[test]
    fn mining_ready_when_onboarded_pool_set_circuit_skipped_not_mining() {
        // Decoupled from the skippable AC-values circuit step: a fully
        // onboarded, pool-configured unit is mining-ready even when
        // `circuit_configured` is false (opted out / DC-solar), matching the
        // daemon's real gate, which has no circuit dependency.
        let s = onboarding_state_mining_ready_modulo_circuit();
        assert!(onboarding_is_complete(&s));
        assert!(!s.steps.circuit_configured);
        assert!(
            onboarding_is_mining_ready(&s, false, /* actively_mining */ false),
            "circuit step is optional; onboarded + pool configured => mining-ready"
        );
    }

    #[test]
    fn mining_ready_false_when_not_mining_and_onboarding_incomplete() {
        let mut s = OnboardingState::default();
        s.steps.pool_configured = true; // pool set but the wizard is not finished
        assert!(!onboarding_is_complete(&s));
        assert!(
            !onboarding_is_mining_ready(&s, false, false),
            "not mining + incomplete onboarding => not mining-ready"
        );
    }

    #[test]
    fn mining_ready_pool_still_required_when_not_mining() {
        // Decoupling drops the *circuit* gate, NOT the pool gate.
        let mut s = onboarding_state_mining_ready_modulo_circuit();
        s.steps.pool_configured = false;
        assert!(
            !onboarding_is_mining_ready(&s, false, false),
            "pool config is still required when not actively mining"
        );
        // ...but ground truth still wins if it is in fact mining.
        assert!(onboarding_is_mining_ready(&s, false, true));
    }

    #[test]
    fn mining_ready_solar_gate_enforced_only_when_not_mining() {
        // Empty on-disk config in tests => power_source falls back to the
        // in-memory state, so the solar gate is exercised deterministically.
        let mut s = onboarding_state_mining_ready_modulo_circuit();
        s.power_source = "solar_battery".to_string();
        assert!(
            !onboarding_is_mining_ready(&s, false, false),
            "solar provider required but not ready => not mining-ready when idle"
        );
        assert!(
            onboarding_is_mining_ready(&s, true, false),
            "solar provider ready => mining-ready"
        );
        assert!(
            onboarding_is_mining_ready(&s, false, true),
            "actively mining overrides the solar gate (ground truth)"
        );
    }
}

/// GET /api/config/webhook -- Current webhook notification config.
pub(super) async fn get_webhook_config(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    // P3-2: post-write-fresh in-memory config cache (no per-request disk read).
    // A POST to /api/config/webhook persists via atomic_write, which bumps the
    // config-write generation, so this GET observes the new value.
    let table = state.config_cache.snapshot();
    Json(webhook_config_response(read_webhook_config(&table)))
}

/// GET /api/config/mqtt -- Current MQTT / Home Assistant config.
pub(super) async fn get_mqtt_config(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    // P3-2: post-write-fresh in-memory config cache (no per-request disk read).
    let table = state.config_cache.snapshot();
    Json(mqtt_config_response(read_mqtt_config(&table)))
}

/// Build the observable MQTT/HA integration posture from a config table (P2-10).
///
/// Reports the CONFIGURED state (enabled / broker / discovery / command
/// subscriber) plus the config-derived `integration_up` (the same value the
/// Prometheus `dcentrald_integration_up{kind="mqtt"}` sample reflects). It does
/// NOT claim a live "connected" state: the daemon does not track an in-process
/// broker socket, so `live_connection_tracked` is always `false` and a real
/// connected/last-publish proof requires a broker (operator-gated). Pure over
/// its input so it is host-testable without disk/AppState.
pub(super) fn mqtt_status_payload(table: &toml::Table) -> serde_json::Value {
    let cfg = read_mqtt_config(table);
    let (mqtt_up, _webhook_up) = integration_up_from_config_table(table);
    serde_json::json!({
        "enabled": cfg.enabled,
        "broker": cfg.broker,
        "topic_prefix": cfg.topic_prefix,
        "discovery": cfg.discovery,
        // The HA command subscriber (P2-7) is active whenever the publisher is
        // enabled; every command it applies is re-clamped to the safety caps.
        "command_subscriber_enabled": cfg.enabled,
        // Config-derived: Some(true) when [mqtt].enabled, else null when the
        // integration is not configured (matches the Prometheus gauge).
        "integration_up": mqtt_up,
        // Honest: this is configuration state, NOT a live connection proof.
        "live_connection_tracked": false,
        "note": "Configured posture only - live broker connection (connected / last-publish) is not tracked in-process; verify with mosquitto (operator-gated).",
    })
}

/// GET /api/mqtt/status -- observable MQTT / Home Assistant integration posture
/// (P2-10). See [`mqtt_status_payload`] for the honesty contract.
pub(super) async fn get_mqtt_status(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    // P3-2: post-write-fresh in-memory config cache (no per-request disk read).
    let table = state.config_cache.snapshot();
    Json(mqtt_status_payload(&table))
}

/// POST /api/config/mqtt -- Persist MQTT / Home Assistant config.
pub(super) async fn post_mqtt_config(
    State(state): State<Arc<AppState>>,
    Json(mut body): Json<MqttConfigPayload>,
) -> impl IntoResponse {
    // CE-111: dedicated config writer — gate on runtime ConfigRw (fail-closed
    // for Unknown/Experimental/Unsupported identities, same as the generic
    // /api/config writer).
    if let Err(response) =
        require_antminer_runtime_capability(&state, RuntimeCapability::ConfigRw, "/api/config/mqtt")
    {
        return response;
    }

    body.broker = body.broker.trim().to_string();
    body.topic_prefix = body.topic_prefix.trim().trim_matches('/').to_string();
    body.username = body.username.trim().to_string();

    if let Err(message) = validate_mqtt_config(&body) {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "status": "error",
                "message": message,
            })),
        )
            .into_response();
    }

    match persist_mqtt_config(&body) {
        Ok(()) => {
            push_rest_audit_free(
                &state,
                "mqtt_config",
                format!(
                    "MQTT config saved: enabled={}, discovery={}",
                    body.enabled, body.discovery
                ),
            );
            Json(serde_json::json!({
                "status": "ok",
                "message": mqtt_runtime_message(),
                "config": mqtt_config_response(body),
            }))
            .into_response()
        }
        Err(message) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({
                "status": "error",
                "message": message,
            })),
        )
            .into_response(),
    }
}

/// POST /api/config/mqtt/test -- Validate broker reachability and credentials.
pub(super) async fn post_mqtt_test(Json(mut body): Json<MqttConfigPayload>) -> impl IntoResponse {
    body.broker = body.broker.trim().to_string();
    body.topic_prefix = body.topic_prefix.trim().trim_matches('/').to_string();
    body.username = body.username.trim().to_string();

    // MQTT-1: a dashboard "Test connection" round-trip re-POSTs the masked
    // password placeholder when the operator didn't change it. Resolve it back
    // to the stored secret so the connection test uses the real credential,
    // not the literal "<redacted>" mask.
    if body.password == SECRET_REDACTION_PLACEHOLDER {
        let stored = load_config_table_for_write().unwrap_or_default();
        body.password = resolve_mqtt_test_password(&body.password, &stored);
    }

    if body.broker.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "ok": false,
                "connected": false,
                "message": "MQTT broker is required to run a connection test",
            })),
        )
            .into_response();
    }

    if let Err(message) = validate_mqtt_config(&body) {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "ok": false,
                "connected": false,
                "message": message,
            })),
        )
            .into_response();
    }

    let publisher_config = crate::mqtt::MqttPublisherConfig {
        broker: body.broker.clone(),
        topic_prefix: body.topic_prefix.clone(),
        discovery: body.discovery,
        username: (!body.username.is_empty()).then(|| body.username.clone()),
        password: (!body.password.is_empty()).then(|| body.password.clone()),
        publish_interval_s: body.publish_interval_s,
        // Connection test only — no discovery is published, so the generic
        // identity fallbacks are fine here.
        device: crate::mqtt::MqttDeviceIdentity::default(),
    };

    match crate::mqtt::test_connection(&publisher_config).await {
        Ok(client_id) => Json(serde_json::json!({
            "ok": true,
            "connected": true,
            "client_id": client_id,
            "broker": body.broker,
            "state_topic": format!("{}/state", body.topic_prefix),
            "availability_topic": format!("{}/availability", body.topic_prefix),
            "restart_required": true,
            "message": "MQTT broker connection succeeded. Saved settings will be used by the daemon after restart.",
        }))
        .into_response(),
        Err(error) => (
            StatusCode::BAD_GATEWAY,
            Json(serde_json::json!({
                "ok": false,
                "connected": false,
                "broker": body.broker,
                "restart_required": true,
                "message": format!("MQTT connection test failed: {}", error),
            })),
        )
            .into_response(),
    }
}

/// POST /api/config/webhook -- Persist webhook notification config.
pub(super) async fn post_webhook_config(
    State(state): State<Arc<AppState>>,
    Json(mut body): Json<WebhookConfigPayload>,
) -> impl IntoResponse {
    // CE-111: dedicated config writer — gate on runtime ConfigRw.
    if let Err(response) = require_antminer_runtime_capability(
        &state,
        RuntimeCapability::ConfigRw,
        "/api/config/webhook",
    ) {
        return response;
    }

    body.url = body.url.trim().to_string();
    body.telegram_bot_token = body.telegram_bot_token.trim().to_string();
    body.telegram_chat_id = body.telegram_chat_id.trim().to_string();

    // Keep-existing-on-redaction (W20 SEC): the GET response masks the stored
    // webhook URL AND the Telegram bot token (both carry a delivery secret) to
    // SECRET_REDACTION_PLACEHOLDER. A dashboard round-trip that didn't change
    // them re-POSTs the placeholder — resolve each back to the stored value so
    // we never persist the mask as the real secret.
    if body.url == SECRET_REDACTION_PLACEHOLDER
        || body.telegram_bot_token == SECRET_REDACTION_PLACEHOLDER
    {
        let stored = load_config_table_for_write()
            .ok()
            .map(|t| read_webhook_config(&t));
        if body.url == SECRET_REDACTION_PLACEHOLDER {
            body.url = stored.as_ref().map(|c| c.url.clone()).unwrap_or_default();
        }
        if body.telegram_bot_token == SECRET_REDACTION_PLACEHOLDER {
            body.telegram_bot_token = stored
                .as_ref()
                .map(|c| c.telegram_bot_token.clone())
                .unwrap_or_default();
        }
    }

    if let Err(message) = validate_webhook_config(&body) {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "status": "error",
                "message": message,
            })),
        )
            .into_response();
    }

    match write_toml_section(
        "webhook",
        &[
            ("enabled", toml::Value::Boolean(body.enabled)),
            ("url", toml::Value::String(body.url.clone())),
            (
                "events",
                toml::Value::Array(
                    body.events
                        .iter()
                        .cloned()
                        .map(toml::Value::String)
                        .collect(),
                ),
            ),
            (
                "format",
                toml::Value::String(webhook_format_str(body.format).to_string()),
            ),
            (
                "telegram_bot_token",
                toml::Value::String(body.telegram_bot_token.clone()),
            ),
            (
                "telegram_chat_id",
                toml::Value::String(body.telegram_chat_id.clone()),
            ),
        ],
    ) {
        Ok(()) => {
            push_rest_audit_free(
                &state,
                "webhook_config",
                format!(
                    "Webhook config saved: enabled={}, event_count={}",
                    body.enabled,
                    body.events.len()
                ),
            );
            Json(serde_json::json!({
                "status": "ok",
                "message": "Webhook configuration saved. Runtime delivery uses the new values after daemon restart.",
                "config": webhook_config_response(body),
            }))
            .into_response()
        }
        Err(message) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({
                "status": "error",
                "message": message,
            })),
        )
            .into_response(),
    }
}

/// POST /api/config/webhook/test -- Send a synthetic test webhook immediately.
pub(super) async fn post_webhook_test(
    State(_state): State<Arc<AppState>>,
    Json(mut body): Json<WebhookConfigPayload>,
) -> impl IntoResponse {
    body.url = body.url.trim().to_string();
    body.telegram_bot_token = body.telegram_bot_token.trim().to_string();
    body.telegram_chat_id = body.telegram_chat_id.trim().to_string();

    let stored = load_config_table_for_write().unwrap_or_else(|_| toml::Table::new());

    // Keep-existing-on-redaction: a "Test" round-trip that didn't change the
    // secret re-POSTs the mask. Resolve it back so the test exercises the REAL
    // stored credential (URL or Telegram token), not the literal "<redacted>".
    if body.url == SECRET_REDACTION_PLACEHOLDER {
        body.url = read_webhook_config(&stored).url;
    }
    if body.telegram_bot_token == SECRET_REDACTION_PLACEHOLDER {
        body.telegram_bot_token = read_webhook_config(&stored).telegram_bot_token;
    }

    // The target a test needs depends on the format: Generic/Discord/Slack POST
    // to the URL; Telegram needs a bot token + chat id (its URL field is unused).
    let missing_target = match body.format {
        crate::webhook::WebhookFormat::Telegram => {
            body.telegram_bot_token.is_empty() || body.telegram_chat_id.is_empty()
        }
        _ => body.url.is_empty(),
    };
    if missing_target {
        let message = match body.format {
            crate::webhook::WebhookFormat::Telegram => {
                "Telegram bot token and chat id are required to send a test notification"
            }
            _ => "Webhook URL is required to send a test notification",
        };
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "status": "error", "message": message })),
        )
            .into_response();
    }

    if let Err(message) = validate_webhook_config(&body) {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "status": "error",
                "message": message,
            })),
        )
            .into_response();
    }

    // Reshape exactly as the live dispatcher would for this channel.
    let (target_url, payload) = webhook_test_payload(
        body.format,
        &webhook_miner_name(&stored),
        &body.url,
        &body.telegram_bot_token,
        &body.telegram_chat_id,
    );
    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
    {
        Ok(client) => client,
        Err(error) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({
                    "status": "error",
                    "message": format!("Failed to create webhook client: {}", error),
                })),
            )
                .into_response();
        }
    };

    match client.post(&target_url).json(&payload).send().await {
        Ok(response) if response.status().is_success() => Json(serde_json::json!({
            "status": "ok",
            "message": format!("Test webhook delivered successfully ({})", response.status()),
            "http_status": response.status().as_u16(),
        }))
        .into_response(),
        Ok(response) => (
            StatusCode::BAD_GATEWAY,
            Json(serde_json::json!({
                "status": "error",
                "message": format!("Webhook endpoint returned {}", response.status()),
                "http_status": response.status().as_u16(),
            })),
        )
            .into_response(),
        Err(error) => (
            StatusCode::BAD_GATEWAY,
            Json(serde_json::json!({
                "status": "error",
                "message": format!("Webhook test failed: {}", error),
            })),
        )
            .into_response(),
    }
}

/// GET /api/system/update/metadata -- OTA update capability / package metadata.
///
/// SEC-1 (2026-06-20): deliberately UNAUTHENTICATED, matching the sibling
/// read-only monitoring endpoints registered next to it (`/api/system/info`,
/// `/api/system/health`, `/api/system/asic`) under the dev-firmware no-auth
/// posture. The payload ([`UpdateMetadata`]) carries ONLY non-sensitive
/// version/model/board/identity data — firmware version, device model, board
/// target, OTA signature posture, the OTA *public* key id (not a secret), and
/// the install intent (installer/origin/ip/mac/hostname). The MAC + IP it can
/// surface are already exposed in cleartext by the unauth `/api/system/info`
/// (pyasic-compatible), so this is monitoring parity, not a new leak. It carries
/// NO password, pool worker/wallet, private key, or other credential. If a
/// secret-bearing field is ever added to `UpdateMetadata`, this endpoint must be
/// moved behind auth — pinned by
/// `update_metadata_payload_carries_no_secrets_for_unauth_surface`.
pub(super) async fn get_update_metadata(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let miner = state.state_rx.borrow().clone();
    let hw = state
        .hardware_info
        .lock()
        .map(|g| g.clone())
        .unwrap_or_default();
    Json(update_metadata_payload(&miner, &hw))
}

/// GET `/api/dashboard/version` — dashboard SPA build metadata.
///
/// W5.1 (2026-05-07): the dashboard SPA is no longer compiled into
/// dcentrald (see `dashboard.rs`). It ships as a static asset at
/// `/usr/share/dcentos-dashboard/index.html` served by `server.py`. The
/// React client polls this endpoint to confirm the dashboard build it is
/// rendering matches what is on disk and prompts a hard reload when the
/// SHA-256 differs (e.g. after a `dev_deploy.sh --dashboard-only` push).
///
/// The endpoint is intentionally cheap and does not lock any HAL state.
/// On platforms where the dashboard file is absent (e.g. am3-bb today)
/// the response carries `version="missing"` rather than 404 so dashboards
/// served from older daemon binaries can still parse the JSON.
pub(super) async fn get_dashboard_version(
    State(_state): State<Arc<AppState>>,
) -> impl IntoResponse {
    use sha2::{Digest, Sha256};

    // Probe order mirrors the rootfs-overlay layout. The canonical install
    // path is /usr/share/dcentos-dashboard/index.html (W5.1 buildroot
    // post-build copies dashboard/dist/index.html here). The legacy
    // /root/web/static/index.html path is kept as a fallback so partial
    // upgrades (server.py refreshed but post-build hook not yet rerun)
    // still report a build hash.
    const PROBE_PATHS: &[&str] = &[
        "/usr/share/dcentos-dashboard/index.html",
        "/root/web/static/index.html",
    ];

    let mut path_used: Option<&'static str> = None;
    let mut bytes: Option<Vec<u8>> = None;
    let mut built_at_unix: Option<i64> = None;

    for candidate in PROBE_PATHS {
        match std::fs::read(candidate) {
            Ok(buf) => {
                path_used = Some(candidate);
                if let Ok(meta) = std::fs::metadata(candidate) {
                    if let Ok(modified) = meta.modified() {
                        if let Ok(dur) = modified.duration_since(std::time::UNIX_EPOCH) {
                            built_at_unix = Some(dur.as_secs() as i64);
                        }
                    }
                }
                bytes = Some(buf);
                break;
            }
            Err(_) => continue,
        }
    }

    let payload = match bytes {
        Some(buf) => {
            let mut h = Sha256::new();
            h.update(&buf);
            let digest = h.finalize();
            let sha = digest
                .iter()
                .map(|b| format!("{:02x}", b))
                .collect::<String>();
            // W5.1: short version label is the daemon's package version
            // (single source of truth from workspace Cargo.toml). Pair
            // with the artifact sha256 to detect drift.
            serde_json::json!({
                "version": env!("CARGO_PKG_VERSION"),
                "sha256": sha,
                "built_at": built_at_unix,
                "size_bytes": buf.len(),
                "path": path_used,
            })
        }
        None => serde_json::json!({
            "version": "missing",
            "sha256": null,
            "built_at": null,
            "size_bytes": 0,
            "path": null,
        }),
    };

    Json(payload)
}

/// GET `/api/dashboard/health` — lightweight daemon-liveness probe for the
/// always-injected diagnostic banner (`static/diagnostic-banner.js`).
///
/// P0-6 (Omega Plan, C-7). The banner polls this path on every dashboard load.
/// `server.py` (the always-local web server on :80) intercepts it with a rich
/// handler that works even when the daemon is dead (it tails the daemon log and
/// surfaces the last error). But when the SPA is served DIRECTLY by the daemon
/// on :8080, the route did not exist — so the poll 404'd and the banner's
/// `if (!r.ok)` guard silently swallowed it, meaning the "dcentrald is DEAD"
/// bar could never appear from the daemon-served context. With the banner now
/// treating any non-200 as unhealthy, this route is what keeps a *reachable*
/// daemon from triggering a false DEAD bar: if we can answer this request, the
/// daemon is by definition alive.
///
/// The payload is a strict subset of `server.py`'s `get_dashboard_health()` so
/// the banner JS parses both responses identically (it reads `dcentrald_status`
/// then falls back to `alive`). The dead / starting states are surfaced by
/// `server.py`'s always-local handler or by the banner's fetch-failure path —
/// the daemon cannot report its own death, so it only ever answers `"alive"`.
/// Intentionally cheap: no HAL lock, no hardware probe.
pub(super) async fn get_dashboard_health(State(_state): State<Arc<AppState>>) -> impl IntoResponse {
    Json(dashboard_health_payload(
        std::process::id(),
        crate::daemon_uptime_s().unwrap_or(0),
    ))
}

/// Pure builder for the `/api/dashboard/health` payload (P0-6 / C-7).
///
/// Kept separate from the handler so the banner's exact JSON contract is
/// unit-testable without constructing an `AppState`. A daemon that can answer
/// at all is alive by definition, so `alive`/`dcentrald_status` are constant
/// `true`/`"alive"` — the dead/starting states are owned by server.py's
/// always-local handler and the banner's fetch-failure path.
pub(super) fn dashboard_health_payload(pid: u32, uptime_s: u64) -> serde_json::Value {
    serde_json::json!({
        "alive": true,
        "dcentrald_status": "alive",
        "pid": pid,
        "uptime_s": uptime_s,
        "api_bound": true,
        "version": env!("CARGO_PKG_VERSION"),
        "source": "dcentrald",
    })
}

/// Map an `OperatingMode` to its user-facing display label.
/// DCENT_OS rule: "Space Heater / Mining / Hacker" —
/// NEVER "Advanced" or other generic names. Backend enum values stay unchanged
/// for API / config compatibility; any surface that renders a mode name to a
/// human must go through this helper.
pub(super) fn mode_display_label(mode: &crate::OperatingMode) -> &'static str {
    match mode {
        crate::OperatingMode::Home => "Space Heater",
        crate::OperatingMode::Standard => "Mining",
        crate::OperatingMode::Hacker => "Hacker",
    }
}

/// GET /api/system/health -- Proxy-aware health truth layer.
///
/// Native mode returns harmless defaults. Proxy mode is fed by the daemon's
/// ProxiedStats publisher so the dashboard can distinguish live, stale, and
/// blocked bosminer-owned hardware without claiming native mining ownership.
pub(super) async fn get_system_health(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let miner = state.state_rx.borrow().clone();
    let runtime = crate::runtime_health_snapshot();
    let mode = runtime.mode;
    let version = if miner.firmware_version.trim().is_empty() {
        env!("CARGO_PKG_VERSION").to_string()
    } else {
        miner.firmware_version.clone()
    };
    let uptime_s = if miner.uptime_s > 0 {
        miner.uptime_s
    } else {
        crate::daemon_uptime_s().unwrap_or(0)
    };

    //  HIGH-1: surface `is_mining` and platform fingerprint so the
    // dashboard's `getHonestModeState()` can detect the .25-class XIL
    // handoff-mining path. `is_mining` is true when chains report hashrate
    // or chip enumeration (the daemon is dispatching work, regardless of
    // whether bosminer is also alive). Fingerprint mirrors what the
    // wave55a_recipe_guard would use for the same detection.
    let is_mining = miner_is_actively_mining(&miner);
    let (w55a_platform, w55a_board_target, w55a_psu_hw) = w55a_read_platform_files();
    let w55a_is_xil_25 =
        w55a_is_xil_25_class(&w55a_platform, &w55a_board_target, w55a_psu_hw.as_deref());

    Json(serde_json::json!({
        "api_contract_version": dcentrald_api_types::API_CONTRACT_VERSION,
        "mode": mode.as_str(),
        "daemon": {
            "version": version,
            "uptime_s": uptime_s,
            "pid": std::process::id(),
            "is_mining": is_mining,
        },
        "bosminer": {
            "alive": runtime.bosminer.alive,
            "pid": runtime.bosminer.pid,
            "pid_history": &runtime.bosminer.pid_history,
            "last_seen_ms": runtime.bosminer.last_seen_ms,
            "blockers": &runtime.bosminer.blockers,
            "last_summary": {
                "accepted": runtime.bosminer.last_summary.accepted,
                "rejected": runtime.bosminer.last_summary.rejected,
                "mhs_5s": runtime.bosminer.last_summary.mhs_5s,
            },
        },
        "rail": system_health_rail(mode),
        "recovery": {
            "next_action": system_health_next_action(mode, &runtime),
        },
        "scrape": {
            "cgminer_url": &runtime.scrape.cgminer_url,
            "cgminer_reachable": &runtime.scrape.cgminer_reachable,
            "last_poll_ms": &runtime.scrape.last_poll_ms,
            "consecutive_failures": runtime.scrape.consecutive_failures,
        },
        "watchdog": read_kernel_watchdog_state(),
        "fingerprint": {
            "platform": if w55a_platform.is_empty() { serde_json::Value::Null } else { serde_json::Value::String(w55a_platform) },
            "board_target": if w55a_board_target.is_empty() { serde_json::Value::Null } else { serde_json::Value::String(w55a_board_target) },
            "psu_hardware_variant": w55a_psu_hw,
            "is_xil_25_class": w55a_is_xil_25,
        },
    }))
}

// ──────────────────────────────────────────────────────────────────────
//  HIGH-1/2/3 (2026-05-24) — `a lab unit`-class XIL bosminer-handoff
// REST surfaces. Read-only endpoints, no auth (per Gate-1 Q3 — match
// existing dev-firmware no-auth posture per
// ). Dashboard
// components self-gate on the `is_xil_25_class` flag — every other unit
// (S9 / .109 / .79 / .135 / .129) renders the new components empty.
// ──────────────────────────────────────────────────────────────────────

/// The 13 env vars that MUST be set in the  PROVEN MINING RECIPE
/// launcher for `a lab unit`-class XIL hardware. Any subset has been
/// LIVE-FALSIFIED.
///
/// MIRRORED from
/// `DCENT_OS_Antminer/dcentrald/dcentrald/tests/wave54_proven_mining_recipe.rs::REQUIRED_ENV_VARS`
/// (the test crate cannot be imported here — dcentrald-api lives one
/// crate up the graph). Keep in sync with:
/// - `scripts/run_wave54_25_PROVEN_MINING.sh`
///
pub(super) const WAVE54_REQUIRED_ENV_VARS: &[&str] = &[
    "DCENT_AM2_TRUST_RAIL_FALLBACK",
    "DCENT_AM2_MCR_OUT2",
    "DCENT_AM2_IER_BOSMINER_PARITY",
    "DCENT_AM2_GET_VERSION_FRAMED_4B",
    "DCENT_AM2_PSU_BITBANG_HALF_PERIOD_US",
    "DCENT_AM2_PSU_BITBANG_USE_MMAP",
    "DCENT_AM2_DSPIC_BOSMINER_FAITHFUL",
    "DCENT_AM2_EEPROM_BUS_WARMUP",
    "DCENT_AM2_I2C_SLAVE_SAFE",
    "DCENT_AM2_SKIP_FAST_UART",
    "DCENT_AM2_SKIP_115200_PER_CHIP",
    "DCENT_AM2_SERIAL_WORK_DISPATCH",
    "DCENT_BM1362_ENABLE_UART_RELAY_LAB",
];

/// The 4 env vars that MUST NOT be set (each LIVE-FALSIFIED on .25).
/// MIRRORED from `dcentrald::wave55a_recipe_guard::WAVE54_FORBIDDEN_ENV_VARS`
/// and `wave54_proven_mining_recipe.rs::FORBIDDEN_ENV_VARS`.
pub(super) const WAVE54_FORBIDDEN_ENV_VARS: &[&str] = &[
    "DCENT_AM2_PIC_RESET_AND_START_APP",
    "DCENT_AM2_PIC_RESET_STRACE_DERIVED",
    "DCENT_AM2_PSU_LOKI_REGISTER_POINTER",
    "DCENT_AM2_PSU_CALIBRATION_PROBE_WAKE",
];

/// `a lab unit`-class XIL fingerprint files. Mirrors
/// `dcentrald::wave55a_recipe_guard::{PLATFORM_FILE, BOARD_TARGET_FILE,
/// PSU_HARDWARE_VARIANT_FILE, ZYNQ_BM3_AM2_PLATFORM,
/// XIL_25_BOARD_TARGET_SUFFIX, LOKI_PSU_HARDWARE_VARIANT}`.
pub(super) const W55A_PLATFORM_FILE: &str = "/etc/dcentos/platform";
pub(super) const W55A_BOARD_TARGET_FILE: &str = "/etc/dcentos/board_target";
pub(super) const W55A_PSU_HARDWARE_VARIANT_FILE: &str = "/etc/dcentos/psu_hardware_variant";
pub(super) const W55A_ZYNQ_BM3_AM2_PLATFORM: &str = "zynq-bm3-am2";
pub(super) const W55A_XIL_25_BOARD_TARGET_SUFFIX: &str = "xil";
pub(super) const W55A_LOKI_PSU_HARDWARE_VARIANT: &str = "loki";

/// `a lab unit` chip rail target — the dsPIC SetVoltage value DCENT_OS commands
/// on AM2 (13.7 V, big-endian-mV). Source:
/// `dcentrald-asic` cold-boot init constant; also the value the
///  dsPIC fw=0x82 BARE ACK'd live on 2026-05-24.
///
/// P1-2 (D-4/D-5): this is the am2/BM1362-only chip-rail target and is NO LONGER
/// the unconditional `mv_target` in `get_chain_presence` — it leaked 13700 mV
/// onto every platform (the S9 real rail is ~8.6-9.1 V via the PIC DAC). The
/// per-platform target is now derived from `MinerProfile::default_voltage_mv`;
/// this const is the clearly-labeled (`fallback_am2_default`) fallback used only
/// when no chip profile resolves.
pub(super) const W55A_DEFAULT_CHIP_RAIL_TARGET_MV: u16 = 13700;

/// `a lab unit` has 2 hashboards × 63 chips = 126 expected. Per-chain expected
/// chips when the snapshot doesn't carry an explicit `chips_expected`.
/// Source:  +
/// .
///
/// P1-2 (D-4/D-5): like the rail target above, this am2 const is now only the
/// `fallback_am2_default` value for `chips_expected`; the per-platform expected
/// count is derived from `MinerProfile::chips_per_chain` (it was right on the S9
/// only by luck — BM1387 also happens to be 63/chain).
pub(super) const W55A_DEFAULT_CHIPS_PER_CHAIN: u16 = 63;

pub(super) fn w55a_env_value_is_truthy(value: &str) -> bool {
    matches!(value, "1" | "true" | "TRUE" | "yes" | "YES" | "on" | "ON")
}

pub(super) fn w55a_read_platform_files() -> (String, String, Option<String>) {
    let platform = std::fs::read_to_string(W55A_PLATFORM_FILE)
        .or_else(|_| std::fs::read_to_string("/etc/bos_platform"))
        .map(|v| v.trim().to_string())
        .unwrap_or_default();
    let board_target = std::fs::read_to_string(W55A_BOARD_TARGET_FILE)
        .map(|v| v.trim().to_string())
        .unwrap_or_default();
    let psu_hw = std::fs::read_to_string(W55A_PSU_HARDWARE_VARIANT_FILE)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty());
    (platform, board_target, psu_hw)
}

pub(super) fn w55a_is_xil_25_class(
    platform: &str,
    board_target: &str,
    psu_hw: Option<&str>,
) -> bool {
    if platform != W55A_ZYNQ_BM3_AM2_PLATFORM {
        return false;
    }
    if !board_target.ends_with(W55A_XIL_25_BOARD_TARGET_SUFFIX) {
        return false;
    }
    match psu_hw {
        None => true,
        Some(v) if v.eq_ignore_ascii_case(W55A_LOKI_PSU_HARDWARE_VARIANT) => true,
        Some("") => true,
        Some(_) => false,
    }
}

/// GET /api/env/recipe —  HIGH-3.
///
/// Read-only view of the  PROVEN MINING RECIPE env state for the
/// live `dcentrald` process. Operators can see if the recipe is applied
/// without SSH'ing in to grep the env. Dashboard banner consumes this.
pub(super) async fn get_env_recipe() -> impl IntoResponse {
    let (platform, board_target, psu_hw) = w55a_read_platform_files();
    let is_xil_25 = w55a_is_xil_25_class(&platform, &board_target, psu_hw.as_deref());

    let mut applied = serde_json::Map::new();
    let mut missing: Vec<String> = Vec::new();
    for name in WAVE54_REQUIRED_ENV_VARS {
        match std::env::var(name) {
            Ok(value) if w55a_env_value_is_truthy(&value) => {
                applied.insert((*name).to_string(), serde_json::Value::String(value));
            }
            _ => missing.push((*name).to_string()),
        }
    }

    let mut forbidden_detected: Vec<String> = Vec::new();
    for name in WAVE54_FORBIDDEN_ENV_VARS {
        if let Ok(value) = std::env::var(name) {
            if w55a_env_value_is_truthy(&value) {
                forbidden_detected.push((*name).to_string());
            }
        }
    }

    let recipe_intact = missing.is_empty() && forbidden_detected.is_empty();

    Json(serde_json::json!({
        "applied": applied,
        "missing": missing,
        "forbidden_detected": forbidden_detected,
        "fingerprint": {
            "platform": if platform.is_empty() { serde_json::Value::Null } else { serde_json::Value::String(platform) },
            "board_target": if board_target.is_empty() { serde_json::Value::Null } else { serde_json::Value::String(board_target) },
            "psu_hardware_variant": psu_hw,
        },
        "is_xil_25_class": is_xil_25,
        "wave54_recipe_intact": recipe_intact,
    }))
}

/// P1-2 (D-4/D-5): resolve the per-chain expected chip count, the commanded
/// chip-rail target voltage, and a provenance tag from the detected chip's
/// [`MinerProfile`]. This replaces the hardcoded am2/BM1362  constants
/// (13700 mV / 63 chips) that leaked onto every platform — the S9/BM1387 rail is
/// commanded to ~8.6 V via the PIC DAC, not 13.7 V, and the 63-chip count was
/// right on the S9 only by coincidence.
///
/// Falls back to the legacy am2/.25 constants — tagged `fallback_am2_default` —
/// only when no profile resolves (chip undetected or unrecognized), so an
/// unknown chip can never silently report a BM1362 target as if it were the
/// platform-resolved value. Same `MinerProfile::for_chip` pattern P0-2 used in
/// `get_status`.
pub(super) fn chain_presence_targets(chip_id: Option<u16>) -> (u16, u16, &'static str) {
    match chip_id.and_then(MinerProfile::for_chip) {
        Some(profile) => (
            profile.chips_per_chain as u16,
            profile.default_voltage_mv,
            "chip_profile",
        ),
        None => (
            W55A_DEFAULT_CHIPS_PER_CHAIN,
            W55A_DEFAULT_CHIP_RAIL_TARGET_MV,
            "fallback_am2_default",
        ),
    }
}

/// GET /api/mining/chain/presence —  HIGH-2.
///
/// Per-chain `chips_responding / chips_expected` + chip-rail mV
/// actual-vs-target. Source: the existing `MinerState::chains` snapshot
/// the daemon already maintains. P1-2 (D-4/D-5): `chips_expected` and
/// `mv_target` are derived per-platform from the detected chip's
/// `MinerProfile` (see [`chain_presence_targets`]) instead of the am2/BM1362
/// constants; `presence_source` tags whether the targets came from a resolved
/// chip profile or the labeled am2 fallback.
pub(super) async fn get_chain_presence(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let miner = state.state_rx.borrow().clone();

    // Resolve the detected chip the same way P0-2's `get_status` does, then
    // derive the per-platform expected chip count + commanded rail target.
    let chip_id = {
        let chip_type = state
            .hardware_info
            .lock()
            .map(|guard| guard.chip_type.clone())
            .unwrap_or_default();
        chip_type_to_chip_id(&chip_type)
    };
    let (chips_expected, mv_target, presence_source) = chain_presence_targets(chip_id);

    let chains: Vec<serde_json::Value> = miner
        .chains
        .iter()
        .enumerate()
        .map(|(idx, chain)| {
            let mv_actual = if chain.voltage_mv > 0 {
                Some(chain.voltage_mv)
            } else {
                None
            };
            serde_json::json!({
                "idx": idx,
                "chips_responding": chain.chips,
                "chips_expected": chips_expected,
                "mv_actual": mv_actual,
                "mv_target": mv_target,
            })
        })
        .collect();
    Json(serde_json::json!({
        "chains": chains,
        "presence_source": presence_source,
    }))
}

/// GET /api/mining/handoff/state —  HIGH-1.
///
/// Canonical handoff-mining mode classifier. Dashboard can read this
/// directly instead of re-deriving the state from `bosminer.alive` +
/// `daemon.is_mining` + fingerprint heuristics on its own.
pub(super) async fn get_mining_handoff_state(
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    let miner = state.state_rx.borrow().clone();
    let runtime = crate::runtime_health_snapshot();
    let bosminer_alive = runtime.bosminer.alive;
    let is_mining = miner_is_actively_mining(&miner);
    let (platform, board_target, psu_hw) = w55a_read_platform_files();
    let is_xil_25 = w55a_is_xil_25_class(&platform, &board_target, psu_hw.as_deref());

    let mode = if bosminer_alive && is_mining && is_xil_25 {
        "handoff_mining"
    } else if bosminer_alive && !is_mining {
        "bosminer_only"
    } else if !bosminer_alive && is_mining {
        "standalone"
    } else {
        "idle"
    };

    // ac_cycle_recommended is true when the recipe is broken (forbidden
    // env detected) — the next mining attempt will require an AC cycle
    // because the launcher's env layer needs to be re-applied from a clean
    // start. Best-effort heuristic; the runtime guard makes the final call.
    let mut ac_cycle_recommended = false;
    if is_xil_25 {
        for name in WAVE54_FORBIDDEN_ENV_VARS {
            if let Ok(value) = std::env::var(name) {
                if w55a_env_value_is_truthy(&value) {
                    ac_cycle_recommended = true;
                    break;
                }
            }
        }
    }

    Json(serde_json::json!({
        "mode": mode,
        "last_handoff_ms": serde_json::Value::Null,
        "bosminer_was_engaged": bosminer_alive,
        "ac_cycle_recommended": ac_cycle_recommended,
    }))
}

pub(super) fn read_kernel_watchdog_state() -> serde_json::Value {
    let sysfs = Path::new(KERNEL_WATCHDOG0_SYSFS);
    if !sysfs.exists() {
        return serde_json::json!({
            "available": false,
            "source": KERNEL_WATCHDOG0_SYSFS,
            "state": "unavailable",
            "reason": "watchdog0 sysfs entry is not present on this target",
        });
    }

    serde_json::json!({
        "available": true,
        "source": KERNEL_WATCHDOG0_SYSFS,
        "state": "reported",
        "identity": read_trimmed_sysfs(sysfs.join("identity")),
        "status": read_trimmed_sysfs(sysfs.join("status")),
        "state_text": read_trimmed_sysfs(sysfs.join("state")),
        "bootstatus": read_u64_sysfs(sysfs.join("bootstatus")),
        "timeout_s": read_u64_sysfs(sysfs.join("timeout")),
        "timeleft_s": read_u64_sysfs(sysfs.join("timeleft")),
        "nowayout": read_u64_sysfs(sysfs.join("nowayout")).map(|value| value != 0),
        "read_only": true,
    })
}

pub(super) fn read_trimmed_sysfs(path: impl AsRef<Path>) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

pub(super) fn read_u64_sysfs(path: impl AsRef<Path>) -> Option<u64> {
    read_trimmed_sysfs(path).and_then(|value| value.parse::<u64>().ok())
}

pub(super) fn system_health_rail(mode: crate::RuntimeHealthMode) -> serde_json::Value {
    if matches!(mode, crate::RuntimeHealthMode::Native) {
        return serde_json::json!({
            "verdict": "NOT_APPLICABLE",
            "applicable": false,
            "last_multimeter_reading_v": null,
            "last_reading_at_ms": null,
            "uart_rx_bytes_post_enable": 0,
            "test_steps": [],
            "steps_url": null,
        });
    }

    serde_json::json!({
        "verdict": "PENDING",
        "applicable": true,
        "last_multimeter_reading_v": null,
        "last_reading_at_ms": null,
        "uart_rx_bytes_post_enable": 0,
        "test_steps": [
            { "id": "fw_86_ack", "label": "PIC 0x21 fw=0x86 ACK", "status": "pending" },
            { "id": "set_voltage_ack", "label": "SET_VOLTAGE bare ACK", "status": "pending" },
            { "id": "uart_rx_post_enable", "label": "Chain UART RX post-ENABLE", "status": "pending" },
            { "id": "multimeter_chain_vdd", "label": "Multimeter chain VDD ~13.7 V", "status": "pending" }
        ],
        "steps_url": "/docs/hardware-probe-playbook",
    })
}

pub(super) fn system_health_next_action(
    mode: crate::RuntimeHealthMode,
    runtime: &crate::RuntimeHealthSnapshot,
) -> serde_json::Value {
    match mode {
        crate::RuntimeHealthMode::Native => serde_json::json!({
            "kind": "none",
            "rationale": "Native DCENT_OS mode has no proxy recovery action pending.",
            "doc_url": null,
        }),
        crate::RuntimeHealthMode::Proxy if runtime.bosminer.alive => serde_json::json!({
            "kind": "monitor_proxy",
            "rationale": "Bosminer scrape is reachable; dashboard values must be shown as proxied.",
            "doc_url": null,
        }),
        crate::RuntimeHealthMode::Proxy | crate::RuntimeHealthMode::Hybrid => serde_json::json!({
            "kind": "bench_multimeter",
            "rationale": "Rail and chain-UART truth are pending; take the safe bench measurement before any recovery action.",
            "doc_url": "/docs/hardware-probe-playbook#step-1",
        }),
    }
}

/// WAVE 0 STABILIZE (2026-06-05) — per-mount disk usage with a >90% alert.
///
/// The live `.100` audit found **rootfs 100% full** (and `/tmp/dcentrald.log`
/// growing inside a 64 MiB tmpfs) with NO telemetry surfacing it — the
/// dashboard rendered a healthy-looking unit while the filesystem was wedged.
/// Surface every writable mount the daemon cares about so the dashboard can
/// raise disk pressure before a full disk hard-blocks logging / config writes /
/// OTA staging.
#[derive(Debug, Clone, Serialize)]
pub(super) struct DiskMount {
    /// Mount point path (e.g. `/`, `/data`, `/tmp`).
    mount_point: String,
    /// Total bytes on the filesystem backing this mount.
    total_bytes: u64,
    /// Bytes available to an unprivileged writer (statvfs `f_bavail`).
    available_bytes: u64,
    /// Bytes in use (`total - free`, computed from `f_blocks - f_bfree`).
    used_bytes: u64,
    /// Used percentage (0.0–100.0). `None` only when total is 0 (pseudo-fs).
    used_percent: Option<f64>,
    /// True when this mount is read-only (statvfs `ST_RDONLY`). A read-only
    /// squashfs rootfs is ALWAYS ~100% used by design — surfacing the number is
    /// honest, but it is not disk pressure, so it must NOT raise an alert.
    read_only: bool,
    /// True when `used_percent >= DISK_USAGE_ALERT_PERCENT` AND the mount is
    /// WRITABLE. The dashboard hangs a disk-pressure warning on this. A full
    /// read-only mount never alerts (its fullness is by design).
    alert: bool,
}

/// Usage percentage at or above which a mount is flagged `alert: true`.
/// 90% matches the OMEGA-plan  "alert rootfs >90%" requirement.
pub(super) const DISK_USAGE_ALERT_PERCENT: f64 = 90.0;

/// Mount points the daemon surfaces (in this order). Other lines in
/// `/proc/mounts` (cgroup, proc, sysfs, devtmpfs, …) are pseudo-filesystems
/// with no meaningful usage and are skipped. A mount that isn't present on a
/// given platform is simply omitted from the response (no error).
pub(super) const DISK_TELEMETRY_MOUNTS: &[&str] = &["/", "/data", "/tmp", "/overlay", "/mnt/data"];

#[derive(Debug, Clone, Serialize)]
pub(super) struct SystemStatsResponse {
    uptime_s: u64,
    load_avg_1m: f64,
    load_avg_5m: f64,
    load_avg_15m: f64,
    load_percent_1m: Option<f64>,
    cpu_count: u32,
    mem_total_kb: u64,
    mem_available_kb: u64,
    mem_used_kb: u64,
    mem_used_percent: Option<f64>,
    soc_temp_c: Option<f64>,
    soc_temp_source: Option<String>,
    /// Per-mount disk usage. Empty on platforms where none of the tracked
    /// mounts resolve (e.g. a Windows host test).
    disks: Vec<DiskMount>,
    /// True when ANY surfaced mount is at/above the alert threshold — a single
    /// flag the dashboard can hang a top-level disk-pressure badge on.
    disk_alert: bool,
}

/// GET /api/system/stats -- live host telemetry for dashboard/system debug.
pub(super) async fn get_system_stats() -> impl IntoResponse {
    Json(read_system_stats())
}

pub(super) fn read_system_stats() -> SystemStatsResponse {
    let uptime_s = read_proc_uptime_s();
    let (load_avg_1m, load_avg_5m, load_avg_15m) = read_proc_loadavg();
    let cpu_count = read_cpu_count().max(1);
    let load_percent_1m = Some(((load_avg_1m / cpu_count as f64) * 100.0).max(0.0));
    let (mem_total_kb, mem_available_kb) = read_proc_meminfo();
    let mem_used_kb = mem_total_kb.saturating_sub(mem_available_kb);
    let mem_used_percent = if mem_total_kb > 0 {
        Some((mem_used_kb as f64 / mem_total_kb as f64) * 100.0)
    } else {
        None
    };
    let (soc_temp_c, soc_temp_source) = read_soc_temp();
    let disks = read_disk_mounts();
    let disk_alert = disks.iter().any(|d| d.alert);

    SystemStatsResponse {
        uptime_s,
        load_avg_1m,
        load_avg_5m,
        load_avg_15m,
        load_percent_1m,
        cpu_count,
        mem_total_kb,
        mem_available_kb,
        mem_used_kb,
        mem_used_percent,
        soc_temp_c,
        soc_temp_source,
        disks,
        disk_alert,
    }
}

/// Compute the per-mount usage view from raw statvfs block counts. Pure +
/// host-testable so the >90% alert threshold is regression-pinned without a
/// real filesystem. `total_blocks`/`free_blocks`/`avail_blocks` are statvfs
/// `f_blocks`/`f_bfree`/`f_bavail`; `block_size` is `f_frsize`. `read_only`
/// is `f_flag & ST_RDONLY != 0`.
pub(super) fn build_disk_mount(
    mount_point: &str,
    block_size: u64,
    total_blocks: u64,
    free_blocks: u64,
    avail_blocks: u64,
    read_only: bool,
) -> DiskMount {
    let total_bytes = block_size.saturating_mul(total_blocks);
    let available_bytes = block_size.saturating_mul(avail_blocks);
    // Used = total - free (free includes root-reserved blocks; avail does not,
    // so used+available != total — matches `df`'s Used column).
    let used_blocks = total_blocks.saturating_sub(free_blocks);
    let used_bytes = block_size.saturating_mul(used_blocks);
    let used_percent = if total_blocks > 0 {
        Some((used_blocks as f64 / total_blocks as f64) * 100.0)
    } else {
        None
    };
    // Alert only on a WRITABLE mount over the threshold. A full read-only
    // squashfs `/` is normal (the image is packed) and is not actionable disk
    // pressure — alerting on it would be a permanent false-green-killing false
    // positive.
    let alert = !read_only
        && used_percent
            .map(|p| p >= DISK_USAGE_ALERT_PERCENT)
            .unwrap_or(false);
    DiskMount {
        mount_point: mount_point.to_string(),
        total_bytes,
        available_bytes,
        used_bytes,
        used_percent,
        read_only,
        alert,
    }
}

/// statvfs(3) one mount point into a [`DiskMount`]. Returns `None` when the
/// path doesn't resolve (mount absent on this platform) or the syscall fails.
#[cfg(unix)]
#[allow(clippy::unnecessary_cast)]
pub(super) fn statvfs_disk_mount(mount_point: &str) -> Option<DiskMount> {
    use std::ffi::CString;
    use std::mem::MaybeUninit;

    let cpath = CString::new(mount_point).ok()?;
    let mut stat = MaybeUninit::<libc::statvfs>::uninit();
    // Safety: valid path + out-pointer to an uninit statvfs, per statvfs(3).
    let rc = unsafe { libc::statvfs(cpath.as_ptr(), stat.as_mut_ptr()) };
    if rc != 0 {
        return None;
    }
    // Safety: rc == 0 means stat was initialized.
    let stat = unsafe { stat.assume_init() };
    // ST_RDONLY (1) in f_flag marks a read-only mount (squashfs rootfs, etc.).
    let read_only = (stat.f_flag as u64 & libc::ST_RDONLY as u64) != 0;
    Some(build_disk_mount(
        mount_point,
        stat.f_frsize as u64,
        stat.f_blocks as u64,
        stat.f_bfree as u64,
        stat.f_bavail as u64,
        read_only,
    ))
}

#[cfg(not(unix))]
pub(super) fn statvfs_disk_mount(_mount_point: &str) -> Option<DiskMount> {
    // No statvfs on the Windows host build; disk telemetry is empty there.
    None
}

/// Read per-mount disk usage for the tracked mount points. A mount that is
/// absent (statvfs fails) is skipped. The same backing filesystem can be
/// mounted at several paths; we dedup by (total,available) so `/` and an
/// `/overlay` bind of the same fs aren't double-listed.
pub(super) fn read_disk_mounts() -> Vec<DiskMount> {
    let mut out: Vec<DiskMount> = Vec::new();
    let mut seen: std::collections::HashSet<(u64, u64)> = std::collections::HashSet::new();
    for mount in DISK_TELEMETRY_MOUNTS {
        if let Some(disk) = statvfs_disk_mount(mount) {
            // Skip zero-total pseudo mounts and duplicate backing filesystems.
            if disk.total_bytes == 0 {
                continue;
            }
            if seen.insert((disk.total_bytes, disk.available_bytes)) {
                out.push(disk);
            }
        }
    }
    out
}

pub(super) fn read_proc_uptime_s() -> u64 {
    std::fs::read_to_string("/proc/uptime")
        .ok()
        .and_then(|raw| raw.split_whitespace().next()?.parse::<f64>().ok())
        .map(|value| value.max(0.0).round() as u64)
        .unwrap_or(0)
}

pub(super) fn read_proc_loadavg() -> (f64, f64, f64) {
    let Some(raw) = std::fs::read_to_string("/proc/loadavg").ok() else {
        return (0.0, 0.0, 0.0);
    };
    let mut parts = raw.split_whitespace();
    let one = parts
        .next()
        .and_then(|value| value.parse::<f64>().ok())
        .unwrap_or(0.0);
    let five = parts
        .next()
        .and_then(|value| value.parse::<f64>().ok())
        .unwrap_or(0.0);
    let fifteen = parts
        .next()
        .and_then(|value| value.parse::<f64>().ok())
        .unwrap_or(0.0);
    (one, five, fifteen)
}

pub(super) fn read_cpu_count() -> u32 {
    std::fs::read_to_string("/proc/cpuinfo")
        .ok()
        .map(|raw| {
            raw.lines()
                .filter(|line| line.trim_start().starts_with("processor"))
                .count() as u32
        })
        .unwrap_or(1)
        .max(1)
}

pub(super) fn read_proc_meminfo() -> (u64, u64) {
    let Some(raw) = std::fs::read_to_string("/proc/meminfo").ok() else {
        return (0, 0);
    };
    let mut total = 0_u64;
    let mut available = 0_u64;
    for line in raw.lines() {
        if let Some(value) = line.strip_prefix("MemTotal:") {
            total = value
                .split_whitespace()
                .next()
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or(0);
        } else if let Some(value) = line.strip_prefix("MemAvailable:") {
            available = value
                .split_whitespace()
                .next()
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or(0);
        }
    }
    (total, available)
}

pub(super) fn read_soc_temp() -> (Option<f64>, Option<String>) {
    let Ok(entries) = std::fs::read_dir("/sys/class/thermal") else {
        return (None, None);
    };

    let preferred = ["soc", "cpu", "zynq", "thermal"];
    let mut fallback: Option<(f64, String)> = None;

    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !name.starts_with("thermal_zone") {
            continue;
        }
        let Some(temp_c) = read_thermal_temp_c(&path.join("temp")) else {
            continue;
        };
        let zone_type = std::fs::read_to_string(path.join("type"))
            .ok()
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| name.to_string());
        let source = format!("/sys/class/thermal/{name}");
        if preferred
            .iter()
            .any(|needle| zone_type.to_ascii_lowercase().contains(needle))
        {
            return (Some(temp_c), Some(source));
        }
        fallback.get_or_insert((temp_c, source));
    }

    fallback
        .map(|(temp, source)| (Some(temp), Some(source)))
        .unwrap_or((None, None))
}

pub(super) fn read_thermal_temp_c(path: &std::path::Path) -> Option<f64> {
    let raw = std::fs::read_to_string(path).ok()?;
    let value = raw.trim().parse::<f64>().ok()?;
    Some(if value.abs() > 1000.0 {
        value / 1000.0
    } else {
        value
    })
}

/// GET /api/system/info -- System identification (pyasic compatible).
///
/// Returns firmware version, model, MAC, IP, uptime, chip type, chip count.
/// Format compatible with ESP-Miner/AxeOS for pyasic autodetection.
pub(super) async fn get_system_info(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let miner = state.state_rx.borrow().clone();
    let hw = state
        .hardware_info
        .lock()
        .map(|g| g.clone())
        .unwrap_or_default();

    // Read system info from /sys where available
    let hostname = local_hostname();

    let mac = std::fs::read_to_string("/sys/class/net/eth0/address")
        .unwrap_or_else(|_| "00:00:00:00:00:00".to_string())
        .trim()
        .to_string();
    let ipv4 = eth0_ipv4();

    let total_chips: u16 = miner.chains.iter().map(|c| c.chips as u16).sum();
    let total_chains = miner.chains.len();
    let first_active_chain = miner
        .chains
        .iter()
        .find(|c| c.chips > 0)
        .or_else(|| miner.chains.first());
    let max_temp = miner
        .chains
        .iter()
        .map(|c| c.temp_c)
        .fold(0.0_f32, f32::max);
    let primary_temp = first_active_chain.map(|c| c.temp_c).unwrap_or(0.0);
    let primary_freq = first_active_chain.map(|c| c.frequency_mhz).unwrap_or(0);
    let primary_freq_source = primary_frequency_source(first_active_chain);
    let primary_voltage = first_active_chain.map(|c| c.voltage_mv).unwrap_or(0);
    let live_power = state.power_rx.borrow().clone();
    let power_projection = project_power_telemetry(&live_power, &miner, &hw);
    let measured_wall_watts = measured_wall_watts_for_unprovenanced_surface(&power_projection);
    let profile = chip_type_to_chip_id(&hw.chip_type).and_then(MinerProfile::for_chip);
    let device_model = profile.map(|p| p.name).unwrap_or("Antminer");
    let model_label = if hw.chip_type.trim().is_empty() {
        "Antminer (unknown ASIC)".to_string()
    } else {
        format!("Antminer ({})", hw.chip_type)
    };
    let dcent_swarm = dcent_swarm_info(
        &state,
        &miner,
        &mac,
        measured_wall_watts,
        measured_wall_watts,
    );

    // Derive SoC and model from control board detection
    let soc = if hw.control_board.starts_with("AML") {
        "Amlogic A113D".to_string()
    } else if hw.control_board.contains("am2") {
        "Zynq XC7Z020".to_string()
    } else {
        "Zynq XC7Z010".to_string()
    };
    let board = antminer_board_version(&hw);

    // APIC-2: canonical platform tier key for the dashboard's fail-closed
    // per-platform capability gating (utils/platformCapabilities.ts ->
    // tierFromPlatformKey/platformCapabilities). Derived from the SAME
    // control_board signal used for `soc` above so the two stay consistent;
    // am3-bb is distinguished by its BeagleBone/AM335x control board. Fail-closed
    // to "unknown" when the control board is not yet identified so the dashboard
    // hides hardware-control cards rather than rendering an irrelevant inspector.
    let cb_lower = hw.control_board.to_ascii_lowercase();
    let platform_key = if hw.control_board.trim().is_empty()
        || cb_lower == "unknown"
        || cb_lower == "idle-first boot"
    {
        // Fail closed: detect_control_board() returns the literal "Unknown"
        // (never empty) for an unidentified board, and the pre-detection boot
        // placeholder is "Idle-first boot" — both must map to "unknown" so the
        // dashboard hides hardware-control cards rather than asserting S9.
        "unknown"
    } else if hw.control_board.starts_with("AML") {
        "am3-aml"
    } else if cb_lower.contains("beaglebone")
        || cb_lower.contains("am3-bb")
        || cb_lower.contains("am335")
    {
        "am3-bb"
    } else if hw.control_board.contains("am2") {
        "am2-zynq"
    } else {
        "am1-zynq"
    };

    // P1-3 (D-8): real windowed hashrate averages instead of fabricated ones.
    // Previously `hashRate_10m` and `hashRate_1h` were BOTH set to the same
    // non-windowed value (the lifetime-average `hashrate_ghs`), so two distinctly
    // labeled windows always showed the identical number. Compute genuine
    // 10-minute and 1-hour averages from the 5-minute `/api/history` ring (the
    // same source the Prometheus 15m/24h averages use). When the ring has no
    // sample inside a window (early boot / < window of uptime) fall back to the
    // lifetime average — a real average, just a coarser window — so the field
    // stays numeric for AxeOS/pyasic consumers and the two windows converge
    // HONESTLY (insufficient history) rather than by fabrication. Once the ring
    // spans > 10 min the windows diverge into genuine rolling averages.
    let history = read_history_samples(&state);
    let now_s = unix_time_ms() / 1000;
    let hashrate_10m_ghs =
        hashrate_avg_over_window(&history, now_s, 10 * 60).unwrap_or(miner.hashrate_ghs);
    let hashrate_1h_ghs =
        hashrate_avg_over_window(&history, now_s, 60 * 60).unwrap_or(miner.hashrate_ghs);

    Json(serde_json::json!({
        "firmware": "DCENTos",
        "version": &miner.firmware_version,
        "model": model_label,
        "hostname": hostname,
        "mac": &mac,
        "macAddr": &mac,
        "ipv4": ipv4,
        "uptime_s": miner.uptime_s,
        "uptime": miner.uptime_s,
        "uptimeSeconds": miner.uptime_s,
        "chip_type": &hw.chip_type,
        "identification_confidence": &hw.identification.confidence,
        "identification": &hw.identification,
        "chip_count": total_chips,
        "chain_count": total_chains,
        "mode": miner.mode,
        "mode_display": mode_display_label(&miner.mode),
        "hashrate_ghs": miner.hashrate_ghs,
        "hashRate": miner.hashrate_ghs,
        // 5-minute ring cadence is too coarse for a true 1-minute window; the
        // 5 s rolling average is the freshest real short-window value available.
        "hashRate_1m": miner.hashrate_5s_ghs,
        "hashRate_10m": hashrate_10m_ghs,
        "hashRate_1h": hashrate_1h_ghs,
        "sharesAccepted": miner.accepted,
        "sharesRejected": miner.rejected,
        "bestDiff": 0,
        "bestSessionDiff": 0,
        "power": measured_wall_watts,
        "power_source": power_projection.source,
        "power_source_detail": power_projection.source_detail,
        "live_power_available": power_projection.live_power_available,
        "power_modeled": power_projection.modeled,
        "power_note": power_projection.note,
        "power_calibrated": power_projection.calibrated,
        "power_calibration_multiplier": power_projection.calibration_multiplier,
        "temp": primary_temp,
        "temp2": max_temp,
        "vrTemp": 0,
        "fanrpm": miner.fans.rpm,
        "fan2rpm": miner.fans.per_fan.get(1).map(|f| f.rpm).unwrap_or(0),
        "frequency": primary_freq,
        "frequency_source": primary_freq_source,
        "coreVoltage": primary_voltage,
        "coreVoltageActual": primary_voltage,
        "ASICModel": &hw.chip_type,
        "deviceModel": device_model,
        "swarmColor": "gray",
        "boardVersion": &board,
        "poolDifficulty": miner.pool.difficulty,
        "stratumDiff": miner.pool.difficulty,
        "isUsingFallbackStratum": 0,
        "power_fault": false,
        "overheat_mode": if max_temp >= 75.0 { 1 } else { 0 },
        "showNewBlock": false,
        "axeOSVersion": &miner.firmware_version,
        "api_version": "1.0.0",
        "board": &board,
        "soc": &soc,
        // APIC-2: canonical tier id ("am1-zynq" / "am2-zynq" / "am3-aml" /
        // "am3-bb" / "unknown") for deterministic dashboard capability gating.
        "platform_key": platform_key,
        "field_sources": {
            "model": if hw.chip_type.trim().is_empty() { "unknown" } else { "hardware_info.chip_type" },
            "chip_type": if hw.chip_type.trim().is_empty() { "unknown" } else { "hardware_info.chip_type" },
            "power": if measured_wall_watts > 0.0 { "measured_power.wall_watts" } else { "unavailable_or_modeled_power_suppressed" },
            "temp": "first_active_chain.temp_c",
            "temp2": "max_chain_temp_c",
            "frequency": "first_active_chain.frequency_mhz",
            "coreVoltage": "first_active_chain.voltage_mv",
            "fanrpm": "miner_state.fans.rpm",
            "bestDiff": "unsupported_zero_for_compatibility",
            "bestSessionDiff": "unsupported_zero_for_compatibility",
            "vrTemp": "unsupported_zero_for_compatibility"
        },
        "unsupported_metrics": [
            "bestDiff",
            "bestSessionDiff",
            "vrTemp",
            "isUsingFallbackStratum",
            "showNewBlock"
        ],
        "dcentSwarm": dcent_swarm,
        "hardware": {
            "miner_serial": hw.miner_serial,
            "control_board": hw.control_board,
            "hb_type": hw.hb_type,
            "chip_type": &hw.chip_type,
            "identification": &hw.identification,
            "psu_model": hw.psu_model,
            "psu_fw_version": hw.psu_fw_version,
            "psu_serial": hw.psu_serial,
            "psu_voltage_range": hw.psu_voltage_range,
            "psu_override_active": hw.psu_override_active,
            "capabilities": {
                "voltage_control": hw.capabilities.voltage_control,
                "fan_rpm_feedback": hw.capabilities.fan_rpm_feedback,
                "sleep_wake_supported": hw.capabilities.sleep_wake_supported,
            },
            "autotuner": hw.autotuner,
        },
    }))
}

/// GET /api/system/asic -- Per-ASIC data (AxeOS compatible for pyasic).
///
/// Returns per-chain ASIC chip data — counts, frequencies, voltages, temps.
pub(super) async fn get_system_asic(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let miner = state.state_rx.borrow().clone();
    let hw = state
        .hardware_info
        .lock()
        .map(|g| g.clone())
        .unwrap_or_default();
    let mac = std::fs::read_to_string("/sys/class/net/eth0/address")
        .unwrap_or_else(|_| "00:00:00:00:00:00".to_string())
        .trim()
        .to_string();
    let live_power = state.power_rx.borrow().clone();
    let power_projection = project_power_telemetry(&live_power, &miner, &hw);
    let measured_wall_watts = measured_wall_watts_for_unprovenanced_surface(&power_projection);
    let total_chips: u16 = miner.chains.iter().map(|c| c.chips as u16).sum();
    let chip_id = chip_type_to_chip_id(&hw.chip_type);
    let profile = chip_id.and_then(MinerProfile::for_chip);
    let frequency_options: Vec<u16> = chip_id
        .map(MinerProfile::pll_frequencies_for_chip)
        .map(|freqs| freqs.to_vec())
        .unwrap_or_default();
    let dcent_swarm = dcent_swarm_info(
        &state,
        &miner,
        &mac,
        measured_wall_watts,
        measured_wall_watts,
    );

    let asics: Vec<serde_json::Value> = miner
        .chains
        .iter()
        .map(|c| {
            serde_json::json!({
                "chain_id": c.id,
                "chips": c.chips,
                "frequency": c.frequency_mhz,
                "frequency_source": chain_frequency_source(c),
                "voltage": c.voltage_mv as f64 / 1000.0,
                "temp": c.temp_c,
                "hashrate": c.hashrate_ghs,
                "status": c.status,
                "errors": c.errors,
            })
        })
        .collect();

    Json(serde_json::json!({
        "ASICModel": hw.chip_type,
        "deviceModel": profile.map(|p| p.name).unwrap_or("Antminer"),
        "swarmColor": "gray",
        "asicCount": total_chips,
        "hashDomains": match hw.chip_type.as_str() {
            "BM1366" | "BM1368" | "BM1370" => 4,
            _ => 1,
        },
        "defaultFrequency": profile.map(|p| p.default_freq_mhz).unwrap_or(0),
        "frequencyOptions": frequency_options,
        "defaultVoltage": profile.map(|p| p.default_voltage_mv).unwrap_or(0),
        "voltageOptions": profile
            .map(|p| vec![p.default_voltage_mv])
            .unwrap_or_default(),
        "asics": asics,
        "dcentSwarm": dcent_swarm,
    }))
}

/// GET /api/history -- Historical data (24h hashrate, temp, power).
///
/// Returns 5-minute samples from the shared history buffer.
/// The daemon's history task populates this every 5 minutes.
/// Up to 288 samples (24 hours) are retained in a ring buffer.
pub(super) async fn get_history(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let mode = *state.mode_rx.borrow();
    if let Err(resp) = crate::mode_middleware::check_mode_access("/api/history", mode) {
        return resp.into_response();
    }

    history_response(read_history_data(&state))
}

/// GET /api/history/shares -- Recent correlated share accept/reject events.
pub(super) async fn get_share_history(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let mode = *state.mode_rx.borrow();
    if let Err(resp) = crate::mode_middleware::check_mode_access("/api/history/shares", mode) {
        return resp.into_response();
    }

    let events = state
        .recent_share_history
        .lock()
        .map(|events| events.clone())
        .unwrap_or_default();
    let events: Vec<serde_json::Value> = events
        .iter()
        .map(recent_share_event_response_json)
        .collect();
    Json(serde_json::json!({ "events": events })).into_response()
}

pub(super) fn recent_share_event_response_json(
    event: &crate::RecentShareEvent,
) -> serde_json::Value {
    serde_json::to_value(recent_share_event_response_row(event))
        .expect("RecentShareRow serialization should not fail")
}

pub(super) fn recent_share_event_response_row(
    event: &crate::RecentShareEvent,
) -> dcentrald_api_types::RecentShareRow {
    dcentrald_api_types::RecentShareRow {
        timestamp_ms: event.timestamp_ms,
        result: event.result.clone(),
        job_id: event.job_id.clone(),
        difficulty: event.difficulty,
        target_difficulty: event.target_difficulty,
        error_code: event.error_code,
        error_msg: event.error_msg.clone(),
        worker_name: event.worker_name.clone(),
        nonce: event.nonce.clone(),
        ntime: event.ntime.clone(),
        extranonce2: event.extranonce2.clone(),
        version_bits: event.version_bits.clone(),
        version: event.version,
        protocol_meta_present: event.protocol_meta_present,
    }
}

pub(super) fn mining_pipeline_snapshot_freshness_contract() -> serde_json::Value {
    serde_json::json!({
        "default_stale_after_ms": crate::MINING_PIPELINE_SNAPSHOT_DEFAULT_STALE_AFTER_MS,
        "status_unavailable_when": [
            "publisher_enabled is false",
            "publisher_last_update_ms is null",
            "publisher_last_update_ms is greater than generated_at_ms"
        ],
        "status_live_when": [
            "publisher_enabled is true",
            "publisher_last_update_ms is present",
            "generated_at_ms minus publisher_last_update_ms is less than or equal to default_stale_after_ms"
        ],
        "status_stale_when": [
            "publisher_enabled is true",
            "publisher_last_update_ms is present",
            "generated_at_ms minus publisher_last_update_ms is greater than default_stale_after_ms"
        ],
        "snapshot_available_only_when": "status == live",
        "does_not_populate": [
            "current_job_id",
            "last_notify_timestamp_ms",
            "dispatch_queue_depth",
            "work_ring_occupancy",
            "nonce_bursts_total",
            "stale_nonce_drops_total",
            "unsupported_version_drops_total",
            "local_validation_drops_total",
            "shares_accepted_total",
            "shares_rejected_total",
            "lucky_shares_total",
            "last_share_timestamp_ms",
            "last_share_achieved_difficulty",
            "last_share_target_difficulty"
        ]
    })
}

pub(super) fn mining_pipeline_freshness_classifier_contract() -> serde_json::Value {
    serde_json::json!({
        "schema": crate::MINING_PIPELINE_FRESHNESS_CLASSIFIER_SCHEMA,
        "status": "design_only",
        "implemented": true,
        "runtime_wired": false,
        "publisher_enabled": false,
        "snapshot_available": false,
        "live_route_mounted": false,
        "read_only": true,
        "control_actions": false,
        "hardware_writes": false,
        "filesystem_mutation": false,
        "content_collected": false,
        "probe_performed": false,
        "handlers_executed": false,
        "telemetry_source": "classifier_contract_only",
        "default_stale_after_ms": crate::MINING_PIPELINE_SNAPSHOT_DEFAULT_STALE_AFTER_MS,
        "max_future_skew_ms": crate::MINING_PIPELINE_FRESHNESS_DEFAULT_MAX_FUTURE_SKEW_MS,
        "inputs": [
            "domain_last_update_ms:number|null",
            "generated_at_ms:number",
            "stale_after_ms:number",
            "max_future_skew_ms:number"
        ],
        "outputs": [
            "unavailable",
            "live",
            "stale",
            "future_clock_skew",
            "invalid"
        ],
        "fail_closed_when": [
            "domain_last_update_ms is null",
            "stale_after_ms is zero",
            "domain_last_update_ms is greater than generated_at_ms",
            "domain_last_update_ms minus generated_at_ms is greater than max_future_skew_ms",
            "age would be negative"
        ],
        "snapshot_status_mapping": {
            "unavailable": "unavailable",
            "live": "live",
            "stale": "stale",
            "future_clock_skew": "unavailable",
            "invalid": "unavailable"
        },
        "example_fixtures_schema": "dcentos.mining.pipeline.freshness.classifier.fixture.v1",
        "example_fixture_count": 5,
        "example_fixtures_are_design_only": true,
        "example_fixtures_live_telemetry": false,
        "example_fixtures": mining_pipeline_freshness_classifier_example_fixtures(),
        "does_not_read": [
            "AppState",
            "mining_sync",
            "dispatcher internals",
            "hardware registers",
            "pool sockets",
            "logs",
            "filesystem state"
        ],
        "does_not_populate": [
            "current_job_id",
            "last_notify_timestamp_ms",
            "dispatch_queue_depth",
            "work_ring_occupancy",
            "nonce_bursts_total",
            "stale_nonce_drops_total",
            "unsupported_version_drops_total",
            "local_validation_drops_total",
            "shares_accepted_total",
            "shares_rejected_total",
            "lucky_shares_total",
            "last_share_timestamp_ms",
            "last_share_achieved_difficulty",
            "last_share_target_difficulty"
        ],
        "promotion_note": "Pure classifier only; live publisher promotion still requires S9/S19 Pro/S21 hardware smoke."
    })
}

pub(super) fn mining_pipeline_freshness_classifier_example_fixtures() -> serde_json::Value {
    let generated_at_ms = 10_000_u64;
    let stale_after_ms = crate::MINING_PIPELINE_SNAPSHOT_DEFAULT_STALE_AFTER_MS;
    let max_future_skew_ms = crate::MINING_PIPELINE_FRESHNESS_DEFAULT_MAX_FUTURE_SKEW_MS;
    let fixtures: [(&str, &str, Option<u64>, u64, &str); 5] = [
        (
            "unavailable",
            "Unavailable timestamp source",
            None,
            stale_after_ms,
            "Null domain timestamp fails closed until a mining-owned publisher provides evidence.",
        ),
        (
            "live",
            "Fresh publisher timestamp",
            Some(9_500_u64),
            stale_after_ms,
            "Example timestamp is inside the stale window; it is not evidence that the runtime publisher is wired.",
        ),
        (
            "stale",
            "Expired publisher timestamp",
            Some(4_999_u64),
            stale_after_ms,
            "Example timestamp is older than the stale window; it is a parser fixture only.",
        ),
        (
            "future_clock_skew",
            "Future timestamp beyond skew allowance",
            Some(11_001_u64),
            stale_after_ms,
            "Future timestamp exceeds the allowed clock skew and maps to public unavailable.",
        ),
        (
            "invalid",
            "Invalid stale window",
            Some(generated_at_ms),
            0_u64,
            "Zero stale window cannot safely classify freshness and maps to public unavailable.",
        ),
    ];

    serde_json::Value::Array(
        fixtures
            .into_iter()
            .map(
                |(id, label, domain_last_update_ms, fixture_stale_after_ms, reason)| {
                    let expected_classifier_status =
                        crate::MiningPipelineFreshnessClassifierStatus::classify_domain_timestamp(
                            domain_last_update_ms,
                            generated_at_ms,
                            fixture_stale_after_ms,
                            max_future_skew_ms,
                        );
                    let expected_snapshot_status = expected_classifier_status.as_snapshot_status();

                    serde_json::json!({
                        "id": id,
                        "label": label,
                        "design_only": true,
                        "non_telemetry": true,
                        "telemetry_source": "none",
                        "content_collected": false,
                        "probe_performed": false,
                        "handlers_executed": false,
                        "dispatcher_reads": false,
                        "hardware_reads": false,
                        "pool_socket_reads": false,
                        "runtime_wired": false,
                        "live_route_mounted": false,
                        "inputs": {
                            "domain_last_update_ms": domain_last_update_ms,
                            "generated_at_ms": generated_at_ms,
                            "stale_after_ms": fixture_stale_after_ms,
                            "max_future_skew_ms": max_future_skew_ms
                        },
                        "expected_classifier_status": expected_classifier_status,
                        "expected_snapshot_status": expected_snapshot_status,
                        "snapshot_available": matches!(
                            expected_snapshot_status,
                            crate::MiningPipelineSnapshotStatus::Live
                        ),
                        "reason": reason
                    })
                },
            )
            .collect(),
    )
}

pub(super) fn mining_pipeline_snapshot_publisher_design_contract() -> serde_json::Value {
    serde_json::json!({
        "schema": "dcentos.mining.pipeline.publisher.design.v1",
        "status": "implemented_default_off",
        "implemented": true,
        "publisher_enabled": false,
        "live_route_mounted": true,
        "config_gate": "mining.pipeline_snapshot.enabled",
        "enabled_configs_rejected": false,
        "owner": "daemon_mining_pipeline_snapshot_task",
        "transport": "tokio_watch_channel",
        "rest_consumer": "read_only_snapshot_clone",
        "runtime_source": "existing mining_sync broadcast stream",
        "bounded_publish_cadence": {
            "required": true,
            "max_hz": 1,
            "min_interval_ms": 1000,
            "publish_per_nonce": false,
            "reason": "Publisher must summarize bounded state at low frequency; nonce bursts must update counters in the mining loop, not trigger REST publications."
        },
        "promotion_blockers": [
            "No S9/S19 Pro/S21 hardware smoke has promoted live snapshot reads.",
            "The publisher remains disabled by default.",
            "Rollback disable path is not target-smoked yet."
        ],
        "forbidden": [
            "REST handlers subscribing to mining_sync",
            "dispatcher internals read by REST",
            "per-nonce watch publications",
            "hardware register polling from REST",
            "pool socket reads from REST",
            "filesystem mutation"
        ],
        "hardware_smoke_required": [
            {
                "model": "Antminer S9",
                "required": true,
                "status": "not_run",
                "checks": [
                    "publisher overhead does not perturb job dispatch",
                    "clean-job flush counters remain truthful",
                    "stale snapshot transitions fail closed"
                ]
            },
            {
                "model": "Antminer S19 Pro",
                "required": true,
                "status": "not_run",
                "checks": [
                    "publisher overhead does not perturb job dispatch",
                    "nonce/drop counters match mining-loop evidence",
                    "thermal/fan/watchdog behavior unchanged"
                ]
            },
            {
                "model": "Antminer S21",
                "required": true,
                "status": "not_run",
                "checks": [
                    "publisher overhead does not perturb job dispatch",
                    "snapshot freshness stays within configured stale window",
                    "pool failover and watchdog behavior unchanged"
                ]
            }
        ],
        "promotion_requires": [
            "mining-pipeline-owned nonblocking publisher",
            "bounded cadence at or below max_hz",
            "no per-nonce publication",
            "no REST reconstruction from mining_sync",
            "no hardware or pool socket reads from REST",
            "model-gated S9/S19 Pro/S21 hardware smoke",
            "rollback plan that disables the publisher and leaves mining unaffected"
        ]
    })
}

pub(super) fn mining_pipeline_snapshot_domain_freshness_contract(
    null_reason: &'static str,
    future_fields: &'static [&'static str],
) -> serde_json::Value {
    serde_json::json!({
        "status": "unavailable",
        "last_update_ms": serde_json::Value::Null,
        "age_ms": serde_json::Value::Null,
        "stale_after_ms": serde_json::Value::Null,
        "source": serde_json::Value::Null,
        "null_reason": null_reason,
        "future_fields": future_fields,
        "control_authority": false
    })
}

pub(super) fn mining_pipeline_snapshot_design_v2_contract() -> serde_json::Value {
    serde_json::json!({
        "schema": "dcentos.mining.pipeline.snapshot.design.v2",
        "status": "implemented_default_off",
        "implemented": true,
        "publisher_enabled": false,
        "snapshot_available": false,
        "live_route_mounted": true,
        "read_only": true,
        "control_actions": false,
        "hardware_writes": false,
        "filesystem_mutation": false,
        "content_collected": false,
        "probe_performed": false,
        "handlers_executed": false,
        "source": "bounded_mining_sync_publisher_contract",
        "target_snapshot_schema": crate::MINING_PIPELINE_SNAPSHOT_SCHEMA,
        "config_gate": "mining.pipeline_snapshot.enabled",
        "enabled_configs_rejected": false,
        "publisher_required": true,
        "domain_freshness_status": "available_when_enabled_and_events_received",
        "blocks": {
            "job_freshness": mining_pipeline_snapshot_domain_freshness_contract(
                "no mining-pipeline-owned job publisher is wired",
                &[
                    "current_job_id",
                    "last_notify_timestamp_ms",
                    "clean_jobs_total"
                ]
            ),
            "work_freshness": mining_pipeline_snapshot_domain_freshness_contract(
                "dispatcher queue and work-ring state are not published",
                &[
                    "dispatch_queue_depth",
                    "work_ring_occupancy",
                    "dispatch_bursts_total"
                ]
            ),
            "nonce_freshness": mining_pipeline_snapshot_domain_freshness_contract(
                "nonce-flow counters are not published",
                &[
                    "nonce_bursts_total",
                    "stale_nonce_drops_total",
                    "unsupported_version_drops_total"
                ]
            ),
            "share_freshness": mining_pipeline_snapshot_domain_freshness_contract(
                "share freshness is null until a share accepted/rejected/lucky event is published",
                &[
                    "last_share_timestamp_ms",
                    "shares_accepted_total",
                    "shares_rejected_total",
                    "lucky_shares_total",
                    "last_share_achieved_difficulty",
                    "last_share_target_difficulty",
                    "local_validation_drops_total"
                ]
            )
        },
        "forbidden": [
            "REST handlers subscribing to mining_sync",
            "dispatcher internals read by REST",
            "per-nonce watch publications",
            "hardware register polling from REST",
            "pool socket reads from REST",
            "tuning or DPS control authority",
            "filesystem mutation"
        ],
        "hardware_smoke_required": [
            {
                "model": "Antminer S9",
                "required": true,
                "status": "not_run"
            },
            {
                "model": "Antminer S19 Pro",
                "required": true,
                "status": "not_run"
            },
            {
                "model": "Antminer S21",
                "required": true,
                "status": "not_run"
            }
        ],
        "promotion_requires": [
            "mining-pipeline-owned nonblocking publisher",
            "bounded cadence at or below one hertz",
            "job/work/nonce/share domain freshness copied by the mining loop",
            "all domain freshness null fields replaced only by publisher evidence",
            "no per-nonce publication",
            "no REST reconstruction from mining_sync",
            "no hardware or pool socket reads from REST",
            "S9/S19 Pro/S21 hardware smoke before live route promotion"
        ],
        "limitations": [
            "Design-only contract; no live publisher is active.",
            "Domain freshness blocks are unavailable and null until a mining-pipeline-owned publisher exists.",
            "REST must not reconstruct freshness from mining_sync, dispatcher internals, hardware reads, logs, or pool sockets."
        ]
    })
}

pub(super) fn mining_pipeline_publisher_promotion_checklist_contract() -> serde_json::Value {
    serde_json::json!({
        "schema": "dcentos.mining.pipeline.publisher.promotion.checklist.v1",
        "status": "implemented_default_off",
        "promotion_state": "blocked",
        "implemented": true,
        "source": "bounded_mining_sync_publisher_contract",
        "read_only": true,
        "route_required": true,
        "dispatcher_reads": false,
        "hardware_reads": false,
        "pool_socket_reads": false,
        "control_actions": false,
        "hardware_writes": false,
        "filesystem_mutation": false,
        "content_collected": false,
        "probe_performed": false,
        "handlers_executed": false,
        "publisher_enabled": false,
        "snapshot_available": false,
        "live_route_mounted": true,
        "target_snapshot_design_schema": "dcentos.mining.pipeline.snapshot.design.v2",
        "target_snapshot_schema": crate::MINING_PIPELINE_SNAPSHOT_SCHEMA,
        "config_gate": "mining.pipeline_snapshot.enabled",
        "enabled_configs_rejected": false,
        "required_publisher_owner": "daemon_mining_pipeline_snapshot_task",
        "required_transport": "tokio_watch_channel",
        "required_rest_consumer": "read_only_snapshot_clone",
        "required_rollback_path": "disable mining.pipeline_snapshot.enabled and leave /api/mining/pipeline/snapshot returning unavailable",
        "blockers_schema": "dcentos.mining.pipeline.publisher.promotion.blocker.v1",
        "blocker_count": 7,
        "active_blocker_count": 4,
        "all_blockers_active": false,
        "active_blocker_ids": [
            "hardware_smoke_s9_not_run",
            "hardware_smoke_s19pro_not_run",
            "hardware_smoke_s21_not_run",
            "rollback_not_tested"
        ],
        "requirements": [
            {
                "id": "design_v2_fields",
                "label": "Snapshot design v2 fields",
                "status": "pass",
                "required": true,
                "current_state": "implemented_default_off",
                "evidence_source": "snapshot_design.blocks",
                "reason": "job/work/nonce/share fields are declared and populated only by the bounded publisher when enabled."
            },
            {
                "id": "publisher_source_owner",
                "label": "Publisher source owner",
                "status": "pass",
                "required": true,
                "current_state": "wired_default_off",
                "evidence_source": "daemon_mining_pipeline_snapshot_task",
                "reason": "a nonblocking daemon task owns the watch publisher when mining.pipeline_snapshot.enabled is true."
            },
            {
                "id": "bounded_cadence",
                "label": "Bounded publish cadence",
                "status": "pass",
                "required": true,
                "current_state": "bounded_event_copy",
                "evidence_source": "publisher_design.bounded_publish_cadence",
                "reason": "publisher sends latest-value snapshots from session events and does not publish hardware reads or control actions."
            },
            {
                "id": "forbidden_rest_reconstruction",
                "label": "Forbidden REST reconstruction",
                "status": "pass",
                "required": true,
                "current_state": "guarded_by_contract",
                "evidence_source": "publisher_design.forbidden",
                "reason": "REST clones an AppState watch receiver and does not subscribe to mining_sync, dispatcher internals, hardware reads, logs, or pool sockets."
            },
            {
                "id": "hardware_smoke_s9",
                "label": "Antminer S9 hardware smoke",
                "status": "not_run",
                "required": true,
                "current_state": "hardware_required",
                "evidence_source": "hardware_smoke_required",
                "reason": "S9 smoke must prove publisher overhead does not perturb mining."
            },
            {
                "id": "hardware_smoke_s19pro",
                "label": "Antminer S19 Pro hardware smoke",
                "status": "not_run",
                "required": true,
                "current_state": "hardware_required",
                "evidence_source": "hardware_smoke_required",
                "reason": "S19 Pro smoke must prove nonce/drop counters match mining-loop evidence and fan/thermal/watchdog behavior is unchanged."
            },
            {
                "id": "hardware_smoke_s21",
                "label": "Antminer S21 hardware smoke",
                "status": "not_run",
                "required": true,
                "current_state": "hardware_required",
                "evidence_source": "hardware_smoke_required",
                "reason": "S21 smoke must prove freshness cadence, pool failover, and watchdog behavior are unchanged."
            },
            {
                "id": "rollback_disable_path",
                "label": "Rollback disable path",
                "status": "blocked",
                "required": true,
                "current_state": "documented_only",
                "evidence_source": "config_gate",
                "reason": "promotion requires a tested path that disables the publisher and leaves mining unaffected."
            }
        ],
        "blockers": [
            {
                "id": "publisher_not_wired",
                "label": "Publisher not wired",
                "active": false,
                "severity": "cleared",
                "evidence_source": "AppState.mining_pipeline_snapshot_rx",
                "reason": "publisher receiver is installed when mining.pipeline_snapshot.enabled is true",
                "clears_when": "cleared by default-off bounded publisher wiring"
            },
            {
                "id": "live_route_absent",
                "label": "Live route absent",
                "active": false,
                "severity": "cleared",
                "evidence_source": "rest_router",
                "reason": "the read-only /api/mining/pipeline/snapshot route is mounted",
                "clears_when": "cleared by read-only route mount"
            },
            {
                "id": "domain_freshness_unavailable",
                "label": "Domain freshness unavailable",
                "active": false,
                "severity": "cleared",
                "evidence_source": "snapshot_design.blocks",
                "reason": "job/work/nonce/share fields are populated only by publisher-owned event evidence when enabled",
                "clears_when": "cleared by bounded publisher field mapping"
            },
            {
                "id": "hardware_smoke_s9_not_run",
                "label": "Antminer S9 smoke not run",
                "active": true,
                "severity": "hardware_required",
                "evidence_source": "hardware_smoke_required",
                "reason": "S9 smoke has not proven publisher overhead is mining-safe",
                "clears_when": "S9 hardware smoke records no dispatch, fan, thermal, watchdog, or pool regression"
            },
            {
                "id": "hardware_smoke_s19pro_not_run",
                "label": "Antminer S19 Pro smoke not run",
                "active": true,
                "severity": "hardware_required",
                "evidence_source": "hardware_smoke_required",
                "reason": "S19 Pro smoke has not proven nonce/drop counters and safety loops are unchanged",
                "clears_when": "S19 Pro hardware smoke records matching mining-loop evidence and no safety regression"
            },
            {
                "id": "hardware_smoke_s21_not_run",
                "label": "Antminer S21 smoke not run",
                "active": true,
                "severity": "hardware_required",
                "evidence_source": "hardware_smoke_required",
                "reason": "S21 smoke has not proven freshness cadence, pool failover, and watchdog behavior are unchanged",
                "clears_when": "S21 hardware smoke records stable cadence, failover, watchdog, fan, and thermal behavior"
            },
            {
                "id": "rollback_not_tested",
                "label": "Rollback not tested",
                "active": true,
                "severity": "promotion_blocking",
                "evidence_source": "config_gate",
                "reason": "rollback disable path is documented but not hardware-tested",
                "clears_when": "disabling mining.pipeline_snapshot.enabled is tested to leave mining unaffected"
            }
        ],
        "forbidden": [
            "enabling mining.pipeline_snapshot.enabled by default",
            "reading dispatcher internals from REST",
            "subscribing REST handlers to mining_sync",
            "publishing per nonce",
            "reading hardware registers from REST",
            "reading pool sockets from REST",
            "mutating filesystem or runtime control state"
        ],
        "promotion_allowed_only_when": [
            "all required checklist items are pass",
            "publisher remains mining-pipeline-owned and nonblocking",
            "bounded cadence stays at or below one hertz",
            "S9/S19 Pro/S21 hardware smoke is recorded",
            "rollback disable path is recorded"
        ]
    })
}

pub(super) fn mining_pipeline_fleet_parser_notes_contract() -> serde_json::Value {
    serde_json::json!({
        "schema": "dcentos.mining.pipeline.fleet_parser_notes.v1",
        "status": "schema_only",
        "read_only": true,
        "live_telemetry": false,
        "telemetry_source": "none",
        "readiness_evidence": false,
        "control_actions": false,
        "hardware_writes": false,
        "filesystem_mutation": false,
        "content_collected": false,
        "probe_performed": false,
        "handlers_executed": false,
        "static_aliases": {
            "active_blocker_ids": {
                "source_path": "publisher_promotion_checklist.active_blocker_ids",
                "kind": "static_metadata_alias",
                "source": "static_manifest",
                "mirrors": "publisher_promotion_checklist.blockers where active == true",
                "ordering": "stable_manifest_order",
                "missing_means": "treat promotion_state as blocked",
                "parser_use": "fleet filters and blocked-reason summaries",
                "readiness_evidence": false,
                "live_telemetry": false,
                "telemetry_source": "none",
                "not_authoritative_for": [
                    "blocker label",
                    "blocker reason",
                    "blocker severity",
                    "blocker evidence source",
                    "blocker clear condition"
                ]
            },
            "freshness_classifier_example_fixtures": {
                "source_path": "freshness_classifier.example_fixtures",
                "kind": "static_metadata_examples",
                "source": "static_design_fixture",
                "parser_use": "enum/schema/parser validation only",
                "readiness_evidence": false,
                "live_telemetry": false,
                "telemetry_source": "none",
                "export_default": false,
                "must_not_display_as_miner_state": true,
                "not_authoritative_for": [
                    "publisher last update",
                    "snapshot age",
                    "job freshness",
                    "work freshness",
                    "nonce freshness",
                    "share freshness"
                ]
            }
        },
        "authoritative_sources": [
            {
                "field": "blocker details",
                "source_path": "publisher_promotion_checklist.blockers",
                "reason": "Detailed blockers carry labels, reasons, severity, evidence source, and clear conditions."
            },
            {
                "field": "active blocker ids",
                "source_path": "publisher_promotion_checklist.active_blocker_ids",
                "reason": "Fleet tools can use the alias for machine grouping only."
            },
            {
                "field": "classifier fixture states",
                "source_path": "freshness_classifier.example_fixtures",
                "reason": "Fixtures exercise parser/classifier states without proving live runtime state."
            }
        ],
        "live_promotion_requires": [
            "mining-pipeline-owned publisher wiring",
            "live /api/mining/pipeline/snapshot route mounted after validation",
            "domain freshness populated only by publisher evidence",
            "S9 hardware smoke",
            "S19 Pro hardware smoke",
            "S21 hardware smoke",
            "rollback disable path tested"
        ],
        "does_not_read": [
            "AppState runtime channels",
            "mining_sync",
            "dispatcher internals",
            "hardware registers",
            "pool sockets",
            "logs",
            "filesystem state"
        ],
        "does_not_clear": [
            "publisher_not_wired",
            "live_route_absent",
            "domain_freshness_unavailable",
            "hardware_smoke_s9_not_run",
            "hardware_smoke_s19pro_not_run",
            "hardware_smoke_s21_not_run",
            "rollback_not_tested"
        ],
        "operator_note": "Fleet parser notes are schema-only metadata. They do not prove the default-off publisher is enabled or fresh on this unit."
    })
}

pub(super) fn build_mining_pipeline_snapshot_schema_response(now_ms: u64) -> serde_json::Value {
    serde_json::json!({
        "schema": "dcentos.mining.pipeline.snapshot.schema.v1",
        "snapshot_schema": crate::MINING_PIPELINE_SNAPSHOT_SCHEMA,
        "status": "default_off",
        "read_only": true,
        "control_actions": false,
        "hardware_writes": false,
        "filesystem_mutation": false,
        "content_collected": false,
        "probe_performed": false,
        "handlers_executed": false,
        "publisher_default_enabled": false,
        "live_snapshot_endpoint": "/api/mining/pipeline/snapshot",
        "config_gate": {
            "toml_path": "mining.pipeline_snapshot.enabled",
            "default_enabled": false,
            "current_config_read": false,
            "enabled_configs_rejected": false,
            "live_snapshot_endpoint": "/api/mining/pipeline/snapshot",
            "reason": "Default-off gate; when enabled, a bounded publisher copies existing mining_sync session events into a read-only watch snapshot."
        },
        "generated_at_s": now_ms / 1000,
        "fetched_at_ms": now_ms,
        "default_snapshot": crate::MiningPipelineSnapshot::unavailable(now_ms),
        "freshness_contract": mining_pipeline_snapshot_freshness_contract(),
        "freshness_classifier_schema": crate::MINING_PIPELINE_FRESHNESS_CLASSIFIER_SCHEMA,
        "freshness_classifier": mining_pipeline_freshness_classifier_contract(),
        "publisher_design": mining_pipeline_snapshot_publisher_design_contract(),
        "snapshot_design_schema": "dcentos.mining.pipeline.snapshot.design.v2",
        "snapshot_design": mining_pipeline_snapshot_design_v2_contract(),
        "promotion_checklist_schema": "dcentos.mining.pipeline.publisher.promotion.checklist.v1",
        "publisher_promotion_checklist": mining_pipeline_publisher_promotion_checklist_contract(),
        "fleet_parser_notes_schema": "dcentos.mining.pipeline.fleet_parser_notes.v1",
        "fleet_parser_notes": mining_pipeline_fleet_parser_notes_contract(),
        "fields": [
            {
                "name": "schema",
                "type": "string",
                "default": crate::MINING_PIPELINE_SNAPSHOT_SCHEMA,
                "source": "api_contract"
            },
            {
                "name": "publisher_enabled",
                "type": "boolean",
                "default": false,
                "source": "default_off_gate"
            },
            {
                "name": "snapshot_available",
                "type": "boolean",
                "default": false,
                "source": "default_off_gate"
            },
            {
                "name": "current_job_id",
                "type": "string|null",
                "default": serde_json::Value::Null,
                "source": "future_mining_pipeline_publisher"
            },
            {
                "name": "last_notify_timestamp_ms",
                "type": "number|null",
                "default": serde_json::Value::Null,
                "source": "future_mining_pipeline_publisher"
            },
            {
                "name": "dispatch_queue_depth",
                "type": "number|null",
                "default": serde_json::Value::Null,
                "source": "future_mining_pipeline_publisher"
            },
            {
                "name": "work_ring_occupancy",
                "type": "number|null",
                "default": serde_json::Value::Null,
                "source": "future_mining_pipeline_publisher"
            },
            {
                "name": "nonce_bursts_total",
                "type": "number|null",
                "default": serde_json::Value::Null,
                "source": "future_mining_pipeline_publisher"
            },
            {
                "name": "stale_nonce_drops_total",
                "type": "number|null",
                "default": serde_json::Value::Null,
                "source": "future_mining_pipeline_publisher"
            },
            {
                "name": "unsupported_version_drops_total",
                "type": "number|null",
                "default": serde_json::Value::Null,
                "source": "future_mining_pipeline_publisher"
            },
            {
                "name": "local_validation_drops_total",
                "type": "number|null",
                "default": serde_json::Value::Null,
                "source": "future_mining_pipeline_publisher"
            },
            {
                "name": "shares_accepted_total",
                "type": "number|null",
                "default": serde_json::Value::Null,
                "source": "bounded_mining_sync_publisher"
            },
            {
                "name": "shares_rejected_total",
                "type": "number|null",
                "default": serde_json::Value::Null,
                "source": "bounded_mining_sync_publisher"
            },
            {
                "name": "lucky_shares_total",
                "type": "number|null",
                "default": serde_json::Value::Null,
                "source": "bounded_mining_sync_publisher"
            },
            {
                "name": "last_share_timestamp_ms",
                "type": "number|null",
                "default": serde_json::Value::Null,
                "source": "bounded_mining_sync_publisher"
            },
            {
                "name": "last_share_achieved_difficulty",
                "type": "number|null",
                "default": serde_json::Value::Null,
                "source": "bounded_mining_sync_publisher"
            },
            {
                "name": "last_share_target_difficulty",
                "type": "number|null",
                "default": serde_json::Value::Null,
                "source": "bounded_mining_sync_publisher"
            }
        ],
        "forbidden": [
            "REST must not read dispatcher internals.",
            "REST must not subscribe to mining_sync or replay session-only events.",
            "REST must not poll FPGA, ASIC, UART, I2C, PSU, fan, watchdog, or pool control paths.",
            "REST must not fabricate current jobs, nonce flow, queue depth, or drop counters."
        ],
        "validation_required": [
            "Unit/default serialization tests for the unavailable snapshot.",
            "Freshness fixture tests for unavailable, live, and stale classifications.",
            "Production validator checks for default-off booleans and nullable counters.",
            "Any future live /api/mining/pipeline/snapshot route must remain gated by mining.pipeline_snapshot.enabled and hardware-validation docs.",
            "S9, S19 Pro, and S21 hardware smoke before any live snapshot publisher is promoted."
        ],
        "limitations": [
            "Schema response only; live snapshot state is read from /api/mining/pipeline/snapshot.",
            "The default snapshot is unavailable and default-off.",
            "REST does not subscribe to mining_sync, inspect dispatcher internals, read logs, touch hardware registers, or mutate filesystem contents.",
            "Live fields remain null until the default-off bounded publisher is enabled and receives session events."
        ]
    })
}

/// GET /api/mining/pipeline/snapshot/schema -- Default-off snapshot contract.
///
/// Declares the future mining pipeline snapshot schema without reading runtime
/// state, subscribing to event streams, touching hardware, or exposing a live
/// snapshot endpoint.
pub(super) async fn get_mining_pipeline_snapshot_schema(
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    let mode = *state.mode_rx.borrow();
    if let Err(resp) =
        crate::mode_middleware::check_mode_access("/api/mining/pipeline/snapshot/schema", mode)
    {
        return resp.into_response();
    }

    Json(build_mining_pipeline_snapshot_schema_response(
        unix_time_ms(),
    ))
    .into_response()
}

pub(super) fn build_mining_pipeline_snapshot_response(
    snapshot_rx: Option<&tokio::sync::watch::Receiver<crate::MiningPipelineSnapshot>>,
    now_ms: u64,
    stale_after_ms: u64,
) -> crate::MiningPipelineSnapshot {
    match snapshot_rx {
        Some(rx) => rx
            .borrow()
            .clone()
            .normalize_freshness(now_ms, stale_after_ms.max(1)),
        None => crate::MiningPipelineSnapshot::unavailable(now_ms),
    }
}

/// GET /api/mining/pipeline/snapshot -- Read-only latest snapshot clone.
///
/// This endpoint does not enable a publisher, subscribe to mining_sync,
/// inspect dispatcher internals, poll pool sockets, or touch hardware. When a
/// default-off watch receiver is installed by the daemon, REST only clones the
/// latest value and normalizes freshness.
pub(super) async fn get_mining_pipeline_snapshot(
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    let mode = *state.mode_rx.borrow();
    if let Err(resp) =
        crate::mode_middleware::check_mode_access("/api/mining/pipeline/snapshot", mode)
    {
        return resp.into_response();
    }

    Json(build_mining_pipeline_snapshot_response(
        state.mining_pipeline_snapshot_rx.as_ref(),
        unix_time_ms(),
        state.mining_pipeline_snapshot_stale_after_ms,
    ))
    .into_response()
}

pub(super) fn build_mining_pipeline_manifest_response(
    now_ms: u64,
    snapshot_receiver_configured: bool,
) -> serde_json::Value {
    let live_publisher_source = if snapshot_receiver_configured {
        "receiver_present"
    } else {
        "disabled_by_config"
    };
    let live_publisher_reason = if snapshot_receiver_configured {
        "A bounded watch receiver is configured. Current snapshot availability is reported by /api/mining/pipeline/snapshot."
    } else {
        "No mining pipeline snapshot receiver is installed because mining.pipeline_snapshot.enabled is false."
    };

    serde_json::json!({
        "schema": "dcentos.mining.pipeline.manifest.v1",
        "status": if snapshot_receiver_configured { "publisher_enabled" } else { "publisher_disabled" },
        "read_only": true,
        "control_actions": false,
        "hardware_writes": false,
        "filesystem_mutation": false,
        "content_collected": false,
        "probe_performed": false,
        "handlers_executed": false,
        "telemetry_source": "declared_manifest_existing_surfaces",
        "source": "firmware_source_manifest_only",
        "generated_at_s": now_ms / 1000,
        "fetched_at_ms": now_ms,
        "publisher_live": snapshot_receiver_configured,
        "snapshot_available": false,
        "snapshot_schema": crate::MINING_PIPELINE_SNAPSHOT_SCHEMA,
        "snapshot_contract": crate::MiningPipelineSnapshot::unavailable(now_ms),
        "publisher_gate": {
            "app_state_field": "mining_pipeline_snapshot_rx",
            "receiver_configured": snapshot_receiver_configured,
            "receiver_default": "None",
            "config_toml_path": "mining.pipeline_snapshot.enabled",
            "config_default_enabled": false,
            "enabled_configs_rejected": false,
            "publisher_default_enabled": false,
            "live_snapshot_endpoint": "/api/mining/pipeline/snapshot",
            "promotion_requires": [
                "default-off nonblocking publisher",
                "bounded publish cadence that is not per nonce",
                "S9/S19 Pro/S21 hardware smoke",
                "freshness and stale-state validation"
            ]
        },
        "freshness_contract": mining_pipeline_snapshot_freshness_contract(),
        "freshness_classifier_schema": crate::MINING_PIPELINE_FRESHNESS_CLASSIFIER_SCHEMA,
        "freshness_classifier": mining_pipeline_freshness_classifier_contract(),
        "publisher_design": mining_pipeline_snapshot_publisher_design_contract(),
        "snapshot_design_schema": "dcentos.mining.pipeline.snapshot.design.v2",
        "snapshot_design": mining_pipeline_snapshot_design_v2_contract(),
        "promotion_checklist_schema": "dcentos.mining.pipeline.publisher.promotion.checklist.v1",
        "publisher_promotion_checklist": mining_pipeline_publisher_promotion_checklist_contract(),
        "fleet_parser_notes_schema": "dcentos.mining.pipeline.fleet_parser_notes.v1",
        "fleet_parser_notes": mining_pipeline_fleet_parser_notes_contract(),
        "live_publisher": {
            "available": snapshot_receiver_configured,
            "enabled": snapshot_receiver_configured,
            "snapshot_available": false,
            "source": live_publisher_source,
            "reason": live_publisher_reason
        },
        "existing_surfaces": [
            {
                "id": "websocket_mining_sync",
                "label": "mining_sync WebSocket events",
                "available": true,
                "persistent": false,
                "rest_queryable": false,
                "source": "broadcast_channel_session_events",
                "fields": [
                    "job_received",
                    "clean_job",
                    "dispatch_burst",
                    "nonce_burst",
                    "share_accepted",
                    "share_rejected",
                    "lucky_share"
                ],
                "limitations": [
                    "Session-only event stream.",
                    "Slow consumers may miss broadcast events.",
                    "REST handlers must not subscribe to this stream to reconstruct counters."
                ]
            },
            {
                "id": "mining_work_posture",
                "label": "/api/mining/work/posture",
                "available": true,
                "persistent": true,
                "rest_queryable": true,
                "source": "state_rx_recent_share_history",
                "fields": [
                    "pool_status",
                    "protocol",
                    "donation_window",
                    "sv2_session",
                    "recent_share_job_ids",
                    "aggregate_share_counts"
                ],
                "limitations": [
                    "Current mining.notify age is unavailable.",
                    "Current job identity is not persisted.",
                    "Dispatcher queue depth and work ring occupancy are unavailable."
                ]
            },
            {
                "id": "share_history",
                "label": "/api/history/shares",
                "available": true,
                "persistent": true,
                "rest_queryable": true,
                "source": "recent_share_history",
                "fields": [
                    "timestamp_ms",
                    "result",
                    "job_id",
                    "difficulty",
                    "worker_name",
                    "nonce",
                    "ntime",
                    "version_bits"
                ],
                "limitations": [
                    "Bounded recent history only.",
                    "Rows exist only when real share events were recorded."
                ]
            },
            {
                "id": "pool_status",
                "label": "/api/pools and MinerState.pool",
                "available": true,
                "persistent": false,
                "rest_queryable": true,
                "source": "miner_state_pool",
                "fields": [
                    "url",
                    "status",
                    "protocol",
                    "encrypted",
                    "difficulty",
                    "last_accepted_share_age"
                ],
                "limitations": [
                    "last_accepted_share_age is not mining.notify age.",
                    "No no-notify watchdog counter is exposed yet."
                ]
            },
            {
                "id": "dispatcher_internal_counters",
                "label": "dispatcher-local runtime counters",
                "available": false,
                "persistent": false,
                "rest_queryable": false,
                "source": "not_published",
                "fields": [
                    "stale_nonces",
                    "stale_overwrite_nonces",
                    "stale_empty_slot_nonces",
                    "unsupported_version_drops",
                    "clean_job_flushes",
                    "total_work_dispatched"
                ],
                "limitations": [
                    "Internal counters are not read by this endpoint.",
                    "A future publisher must copy them into a bounded nonblocking snapshot owned by the mining pipeline."
                ]
            }
        ],
        "candidate_snapshot_fields": [
            {
                "id": "last_notify_timestamp_ms",
                "label": "Last mining.notify timestamp",
                "status": "unavailable",
                "source_hint": "stratum_status_handler",
                "publisher_required": true,
                "hardware_required": false,
                "regression_risk": "low",
                "validation": "unit plus pool-session hardware smoke",
                "reason": "No persisted notify timestamp exists in REST state."
            },
            {
                "id": "current_job_id",
                "label": "Current pool job ID",
                "status": "unavailable",
                "source_hint": "job_builder_snapshot",
                "publisher_required": true,
                "hardware_required": false,
                "regression_risk": "medium",
                "validation": "SV1/SV2 pool-session hardware smoke",
                "reason": "Recent share rows can show observed job IDs, but they are not current-job proof."
            },
            {
                "id": "clean_jobs_total",
                "label": "Clean-job flush count",
                "status": "unavailable",
                "source_hint": "dispatcher_snapshot",
                "publisher_required": true,
                "hardware_required": false,
                "regression_risk": "medium",
                "validation": "pool reconnect and clean-job hardware smoke",
                "reason": "Clean-job flushes are not published through REST state."
            },
            {
                "id": "dispatch_bursts_total",
                "label": "Dispatch burst count",
                "status": "unavailable",
                "source_hint": "dispatcher_snapshot",
                "publisher_required": true,
                "hardware_required": false,
                "regression_risk": "medium",
                "validation": "hashboard smoke with no hashrate regression",
                "reason": "WebSocket dispatch bursts are event-only and not durable."
            },
            {
                "id": "nonce_bursts_total",
                "label": "Nonce burst count",
                "status": "unavailable",
                "source_hint": "dispatcher_snapshot",
                "publisher_required": true,
                "hardware_required": true,
                "regression_risk": "medium",
                "validation": "S9/S19 Pro/S21 nonce-flow hardware smoke",
                "reason": "Nonce flow must be published by the mining loop without adding REST-side hardware reads."
            },
            {
                "id": "stale_nonce_drops_total",
                "label": "Stale nonce drop count",
                "status": "unavailable",
                "source_hint": "dispatcher_snapshot",
                "publisher_required": true,
                "hardware_required": true,
                "regression_risk": "medium",
                "validation": "clean-job and stale-work hardware smoke",
                "reason": "Drop buckets are dispatcher-local and must not be inferred from rejected share counts."
            },
            {
                "id": "unsupported_version_drops_total",
                "label": "Unsupported version drop count",
                "status": "unavailable",
                "source_hint": "dispatcher_snapshot",
                "publisher_required": true,
                "hardware_required": true,
                "regression_risk": "medium",
                "validation": "version-bits compatibility hardware smoke",
                "reason": "Version filtering counters are not published to REST state."
            },
            {
                "id": "local_validation_drops_total",
                "label": "Local share validation drop count",
                "status": "unavailable",
                "source_hint": "dispatcher_snapshot",
                "publisher_required": true,
                "hardware_required": true,
                "regression_risk": "medium",
                "validation": "share validation hardware smoke",
                "reason": "Local validation buckets are not published to REST state."
            },
            {
                "id": "publisher_last_update_ms",
                "label": "Snapshot publisher timestamp",
                "status": "unavailable",
                "source_hint": "mining_pipeline_watch_snapshot",
                "publisher_required": true,
                "hardware_required": false,
                "regression_risk": "low",
                "validation": "unit plus stale snapshot API test",
                "reason": "The watch snapshot does not exist yet."
            }
        ],
        "publisher_contract": {
            "owner": "mining_pipeline",
            "transport": "watch_channel",
            "update_budget": "nonblocking_drop_ok",
            "rest_consumer": "read_latest_only",
            "control_scope": "observability_only",
            "forbidden": [
                "REST must not subscribe to mining_sync to rebuild durable counters.",
                "REST must not poll FPGA, ASIC, UART, I2C, PSU, fan, watchdog, or pool control paths.",
                "REST must not lock dispatcher internals or change mining timing.",
                "REST must not infer current jobs, nonce flow, or drop buckets from hashrate."
            ]
        },
        "validation_plan": {
            "automated": [
                "cargo fmt --check",
                "dcentrald-api compile check where the host toolchain supports target dependencies",
                "dashboard npm build",
                "production-readiness validator route, TS contract, UI, and mutation-free checks"
            ],
            "hardware_required": [
                "S9 mining smoke after publisher is added",
                "S19 Pro mining smoke after publisher is added",
                "S21 mining smoke after publisher is added",
                "pool reconnect and clean-job smoke before any live readiness claim"
            ]
        },
        "related_endpoints": [
            "/api/mining/work/posture",
            "/api/history/shares",
            "/api/pools",
            "/api/stats",
            "/api/system/health"
        ],
        "limitations": [
            "Manifest-only response: current default-off publisher state is reported by /api/mining/pipeline/snapshot.",
            "No logs, pool sockets, dispatcher internals, hardware registers, or filesystem contents are collected.",
            "The endpoint declares where real pipeline evidence can come from and leaves unavailable fields unavailable unless a watch receiver is configured.",
            "Hardware promotion still requires S9, S19 Pro, and S21 smoke evidence."
        ]
    })
}

/// GET /api/mining/pipeline/manifest -- Read-only mining pipeline manifest.
///
/// This endpoint declares available and missing mining-pipeline evidence surfaces
/// without subscribing to event streams, collecting logs, probing hardware, or
/// reading dispatcher internals.
pub(super) async fn get_mining_pipeline_manifest(
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    let mode = *state.mode_rx.borrow();
    if let Err(resp) =
        crate::mode_middleware::check_mode_access("/api/mining/pipeline/manifest", mode)
    {
        return resp.into_response();
    }

    Json(build_mining_pipeline_manifest_response(
        unix_time_ms(),
        state.mining_pipeline_snapshot_rx.is_some(),
    ))
    .into_response()
}

/// GET /api/mining/work/posture -- Read-only pool/job/share provenance posture.
///
/// This endpoint intentionally composes only existing watch-channel state and
/// bounded recent share history. It does not inspect dispatcher internals, poll
/// hardware, reconnect pools, or infer current work from hashrate.
pub(super) async fn get_mining_work_posture(
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    let mode = *state.mode_rx.borrow();
    if let Err(resp) = crate::mode_middleware::check_mode_access("/api/mining/work/posture", mode) {
        return resp.into_response();
    }

    let miner = state.state_rx.borrow().clone();
    let now_ms = unix_time_ms();
    let pipeline_snapshot = build_mining_pipeline_snapshot_response(
        state.mining_pipeline_snapshot_rx.as_ref(),
        now_ms,
        state.mining_pipeline_snapshot_stale_after_ms,
    );
    let pipeline_snapshot_live = pipeline_snapshot.snapshot_available;
    let pipeline_current_job_id = pipeline_snapshot
        .current_job_id
        .clone()
        .filter(|_| pipeline_snapshot_live);
    let pipeline_notify_age_s = pipeline_snapshot
        .last_notify_age_ms
        .filter(|_| pipeline_snapshot_live)
        .map(|age_ms| age_ms / 1000);
    let pipeline_value_u64 = |value: Option<u64>| {
        value
            .filter(|_| pipeline_snapshot_live)
            .map(serde_json::Value::from)
            .unwrap_or(serde_json::Value::Null)
    };
    let pipeline_value_u32 = |value: Option<u32>| {
        value
            .filter(|_| pipeline_snapshot_live)
            .map(serde_json::Value::from)
            .unwrap_or(serde_json::Value::Null)
    };
    let pipeline_value_bool = |value: Option<bool>| {
        value
            .filter(|_| pipeline_snapshot_live)
            .map(serde_json::Value::from)
            .unwrap_or(serde_json::Value::Null)
    };
    let pipeline_value_string = |value: &Option<String>| {
        value
            .clone()
            .filter(|_| pipeline_snapshot_live)
            .map(serde_json::Value::from)
            .unwrap_or(serde_json::Value::Null)
    };
    let pool_connecting = is_pool_connecting(&miner.pool.status);
    let pool_connected = is_pool_connected(&miner.pool.status);
    let pool_mining_capable = is_pool_mining_capable(&miner.pool.status);
    let hasrate_active = miner.hashrate_ghs > 0.0 || miner.hashrate_5s_ghs > 0.0;
    let pool_url = miner.pool.url.clone();
    let pool_status = miner.pool.status.clone();
    let pool_protocol = miner.pool.protocol.clone();
    let pool_encrypted = miner.pool.encrypted;
    let pool_difficulty = miner.pool.difficulty;
    let pool_last_share_at = miner.pool.last_share_at;
    let pool_donating = miner.pool.donating;
    let pool_auto_fallback_active = miner.pool.auto_fallback_active;
    let pool_auto_retry_sv2_after_s = miner.pool.auto_retry_sv2_after_s;
    let pool_auto_fallback_reason = miner.pool.auto_fallback_reason.clone();
    let sv2_session = miner.pool.sv2_session.clone();
    let sv2_custom_job = miner.pool.sv2_custom_job.clone();
    let sv2_available = sv2_session.is_some();
    let posture_status = if pool_mining_capable && hasrate_active {
        "active"
    } else if pool_mining_capable {
        "mining_capable"
    } else if pool_connected {
        "connected"
    } else if pool_connecting {
        "connecting"
    } else if !pool_url.trim().is_empty() {
        "waiting"
    } else {
        "unavailable"
    };

    let recent_events = state
        .recent_share_history
        .lock()
        .map(|events| events.clone())
        .unwrap_or_default();
    let recent_count = recent_events.len();
    let accepted_recent = recent_events
        .iter()
        .filter(|event| {
            matches!(
                event.result.to_ascii_lowercase().as_str(),
                "accepted" | "lucky"
            )
        })
        .count();
    let rejected_recent = recent_events
        .iter()
        .filter(|event| event.result.eq_ignore_ascii_case("rejected"))
        .count();
    let unknown_recent = recent_count.saturating_sub(accepted_recent + rejected_recent);
    let latest_event = recent_events
        .iter()
        .rev()
        .find(|event| event.timestamp_ms > 0);
    let latest_job = recent_events
        .iter()
        .rev()
        .find(|event| !event.job_id.trim().is_empty());
    let latest_event_age_s = latest_event.and_then(|event| {
        if now_ms >= event.timestamp_ms {
            Some((now_ms - event.timestamp_ms) / 1000)
        } else {
            None
        }
    });
    let latest_job_age_s = latest_job.and_then(|event| {
        if event.timestamp_ms > 0 && now_ms >= event.timestamp_ms {
            Some((now_ms - event.timestamp_ms) / 1000)
        } else {
            None
        }
    });

    let mut recent_job_ids: Vec<String> = Vec::new();
    for event in recent_events.iter().rev() {
        let job_id = event.job_id.trim();
        if job_id.is_empty() || recent_job_ids.iter().any(|known| known == job_id) {
            continue;
        }
        recent_job_ids.push(job_id.to_string());
        if recent_job_ids.len() >= 8 {
            break;
        }
    }

    let total_shares = miner.accepted.saturating_add(miner.rejected);
    let accept_rate_pct = if total_shares > 0 {
        Some((miner.accepted as f64 / total_shares as f64) * 100.0)
    } else {
        None
    };
    let reject_rate_pct = if total_shares > 0 {
        Some((miner.rejected as f64 / total_shares as f64) * 100.0)
    } else {
        None
    };
    let recent_share_events: Vec<serde_json::Value> = recent_events
        .iter()
        .rev()
        .take(10)
        .map(recent_share_event_response_json)
        .collect();
    let jd_config = crate::routes::jd::read_jd_config_request();
    let jd_live = state.jd_status_rx.borrow().clone();
    let jd_configured = jd_live.configured;
    let custom_job_injection_active = sv2_custom_job
        .as_ref()
        .map(|job| job.status == "accepted")
        .unwrap_or(false);
    let custom_job_injection_ready = jd_live.custom_job_candidate_ready && pool_encrypted;
    let jd_reason = if custom_job_injection_active {
        "SV2 Job Declaration custom work has been accepted by the upstream mining connection and dispatched to the local mining pipeline."
    } else if sv2_custom_job
        .as_ref()
        .map(|job| job.status == "declared")
        .unwrap_or(false)
    {
        "SV2 Job Declaration custom work has been sent to the upstream mining connection and is awaiting pool commitment."
    } else if jd_live.custom_job_candidate_ready {
        "SV2 Job Declaration has a live Template Provider candidate and JDS mining-job token. The mining bridge will inject it after SV2 work-selection channel setup."
    } else if jd_live.connected && jd_live.mining_job_token_available {
        "SV2 Job Declaration completed both setup handshakes and allocated a mining-job token, but a complete Template Provider candidate is not ready."
    } else if jd_live.connected {
        "SV2 Job Declaration completed both setup handshakes. Mining-job token allocation or template candidate readiness is still pending."
    } else if jd_config.enabled && jd_configured {
        "SV2 Job Declaration is configured, but the live supervisor has not completed both protocol handshakes."
    } else if jd_config.enabled {
        "SV2 Job Declaration is enabled but missing a Template Provider or Job Declarator endpoint."
    } else {
        "SV2 Job Declaration is disabled."
    };

    Json(serde_json::json!({
        "schema": "dcentos.mining.work.posture.v1",
        "status": posture_status,
        "read_only": true,
        "control_actions": false,
        "hardware_writes": false,
        "filesystem_mutation": false,
        "telemetry_source": if pipeline_snapshot_live {
            "state_rx_recent_share_history_mining_pipeline_snapshot"
        } else {
            "state_rx_recent_share_history"
        },
        "source": "existing_daemon_state_only",
        "mode": mode,
        "generated_at_s": now_ms / 1000,
        "fetched_at_ms": now_ms,
        "pool": {
            "available": !pool_url.trim().is_empty(),
            // SEC (W20 / parity #66): strip inline stratum credentials.
            "url": dcentrald_stratum::pool_api::sanitize_pool_url(&pool_url),
            "status": pool_status,
            "active": pool_mining_capable,
            "connected": pool_connected,
            "connecting": pool_connecting,
            "mining_capable": pool_mining_capable,
            "published_authorized": pipeline_value_bool(pipeline_snapshot.pool_authorized),
            "published_authorize_state": pipeline_value_string(&pipeline_snapshot.pool_authorize_state),
            "protocol": pool_protocol.clone(),
            "encrypted": pool_encrypted,
            "difficulty": pool_difficulty,
            "pool_target_difficulty": pool_difficulty,
            "last_accepted_share_s": if pool_last_share_at > 0 {
                serde_json::json!(secs_since(pool_last_share_at))
            } else {
                serde_json::Value::Null
            },
            "telemetry_source": "miner_state.pool",
            "health_limitations": [
                "last_accepted_share_s is accepted-share age, not mining.notify age.",
                "recent_share_events.difficulty is achieved difficulty only when locally proven; target_difficulty is the pool target.",
                "Current mining.notify age is copied only from the default-off mining pipeline snapshot publisher when that publisher is live.",
                "This endpoint is read-only and does not switch pools or trigger failover."
            ],
            "no_notify_age_s": pipeline_notify_age_s.map(serde_json::Value::from).unwrap_or(serde_json::Value::Null),
            "failover_policy": "observability_only",
            "auto_fallback_active": pool_auto_fallback_active,
            "auto_retry_sv2_after_s": pool_auto_retry_sv2_after_s,
            "auto_fallback_reason": pool_auto_fallback_reason,
        },
        "protocol": {
            "name": pool_protocol,
            "encrypted": pool_encrypted,
            "source": "miner_state.pool",
            "reason": "Protocol labels come from the existing pool runtime state."
        },
        "asic_version_rolling": {
            "bm1362_status": "not_claimed_pending_accepted_share_proof",
            "claim_default_enabled": false,
            "source": "static_serial_bm1362_contract",
            "operator_label": "BM1362 version rolling is not claimed until hardware accepted-share proof is captured.",
            "reason": "The BM1362 serial path sends full-header work frames and keeps version bits explicit, but DCENT_OS does not advertise BM1362 version-rolling support as proven without an accepted-share fixture from target hardware."
        },
        "donation": {
            "active": pool_donating,
            "source": "miner_state.pool.donating",
            "reason": if pool_donating {
                "The daemon reports that the transparent donation window is active."
            } else {
                "The daemon does not report an active donation window."
            }
        },
        "sv2": {
            "available": sv2_available,
            "encrypted": pool_encrypted,
            "session": sv2_session,
            "source": "miner_state.pool.sv2_session",
            "reason": if sv2_available {
                "SV2 session metadata is present in miner state."
            } else {
                "No SV2 session metadata is present in miner state."
            }
        },
        "job_declaration": {
            "available": jd_live.custom_job_candidate_ready,
            "enabled": jd_config.enabled,
            "configured": jd_configured,
            "connected": jd_live.connected,
            "runtime_state": jd_live.runtime_state,
            "mining_job_token_available": jd_live.mining_job_token_available,
            "template_prev_hash_ready": jd_live.template_prev_hash_ready,
            "custom_job_candidate_ready": jd_live.custom_job_candidate_ready,
            "custom_job_injection_ready": custom_job_injection_ready,
            "custom_job_injection_active": custom_job_injection_active,
            "custom_job_bridge": sv2_custom_job,
            "mode": &jd_config.mode,
            "endpoint": "/api/jd/status",
            "template_provider_url": &jd_live.template_provider_url,
            "job_declarator_url": &jd_live.job_declarator_url,
            "source": "jd_status_rx",
            "reason": jd_reason
        },
        "jobs": {
            "available": pipeline_current_job_id.is_some() || !recent_job_ids.is_empty(),
            "current_job_available": pipeline_current_job_id.is_some(),
            "current_job_id": pipeline_current_job_id,
            "current_job_source": if pipeline_snapshot_live {
                "mining_pipeline_snapshot"
            } else {
                "not_persisted"
            },
            "latest_observed_job_id": latest_job.map(|event| event.job_id.clone()),
            "latest_observed_job_age_s": latest_job_age_s,
            "latest_observed_job_source": if latest_job.is_some() {
                "recent_share_history"
            } else {
                "not_persisted"
            },
            "recent_job_ids": recent_job_ids,
            "reason": if pipeline_snapshot_live {
                "Current job ID is copied from the mining pipeline snapshot publisher. Recent share history remains available as a stale-safe fallback."
            } else if latest_job.is_some() {
                "Recent share history contains real pool job IDs, but REST state does not persist the current mining.notify job yet."
            } else {
                "No persisted job IDs are available. DCENT_OS will not infer job IDs from hashrate or aggregate share counters."
            }
        },
        "work": {
            "available": hasrate_active || pipeline_snapshot_live,
            "active_hashrate": hasrate_active,
            "hashrate_ghs": miner.hashrate_ghs,
            "hashrate_5s_ghs": miner.hashrate_5s_ghs,
            "current_notify_age_s": pipeline_notify_age_s.map(serde_json::Value::from).unwrap_or(serde_json::Value::Null),
            "work_ring_occupancy": pipeline_value_u32(pipeline_snapshot.work_ring_occupancy),
            "dispatch_queue_depth": pipeline_value_u32(pipeline_snapshot.dispatch_queue_depth),
            "stale_nonce_drops_total": pipeline_value_u64(pipeline_snapshot.stale_nonce_drops_total),
            "unsupported_version_drops_total": pipeline_value_u64(pipeline_snapshot.unsupported_version_drops_total),
            "local_validation_drops_total": pipeline_value_u64(pipeline_snapshot.local_validation_drops_total),
            "source": if pipeline_snapshot_live {
                "mining_pipeline_snapshot"
            } else {
                "miner_state.hashrate_only"
            },
            "reason": if pipeline_snapshot_live {
                "Queue depth, work-ring occupancy, notify age, and drop counters are copied from the mining pipeline snapshot publisher without polling dispatcher internals from REST."
            } else if hasrate_active {
                "Hashrate is active in miner state, but current work-ring and dispatcher internals are intentionally not read by this endpoint."
            } else {
                "No active hashrate is reported in miner state."
            }
        },
        "shares": {
            "available": total_shares > 0 || recent_count > 0,
            "accepted_total": miner.accepted,
            "rejected_total": miner.rejected,
            "total": total_shares,
            "accept_rate_pct": accept_rate_pct,
            "reject_rate_pct": reject_rate_pct,
            "recent_count": recent_count,
            "accepted_recent": accepted_recent,
            "rejected_recent": rejected_recent,
            "unknown_recent": unknown_recent,
            "latest_event_timestamp_ms": latest_event.map(|event| event.timestamp_ms),
            "latest_event_age_s": latest_event_age_s,
            "latest_result": latest_event.map(|event| event.result.clone()),
            "latest_job_id": latest_job.map(|event| event.job_id.clone()),
            "source": "miner_state_counters_and_recent_share_history",
            "recent_events": recent_share_events,
            "reason": if recent_count > 0 {
                "Recent rows are real accepted/rejected share events recorded by the daemon."
            } else {
                "No recent share rows are recorded; aggregate accepted/rejected counters are reported without fabricating rows."
            }
        },
        "sources": [
            "state_rx",
            "recent_share_history",
            "/api/mining/pipeline/snapshot",
            "/api/pools",
            "/api/history/shares",
            "/api/pool/sv2/status",
            "/api/jd/status"
        ],
        "limitations": [
            "Read-only posture only: no pool reconfiguration, pool reconnect, failover, sleep/wake, fan, voltage, frequency, PSU, watchdog, upgrade, rollback, or filesystem write is invoked.",
            "Current mining.notify age, work-ring occupancy, dispatcher queue depth, and local nonce drop buckets are populated only when the mining pipeline publishes a live nonblocking snapshot.",
            "Latest job IDs come only from real recent share history and may be stale.",
            "Hashrate proves mining activity, not current job identity; this endpoint does not infer missing job or share rows.",
            "Detailed share rows remain available through /api/history/shares."
        ]
    }))
    .into_response()
}

/// GET /api/network/block -- Honest read-only Bitcoin network/block status.
///
/// This endpoint intentionally does not infer a block height/hash from pool jobs.
/// Stratum mining.notify metadata can identify a pool job, but it is not a local
/// node tip and does not provide block height. Public lookups remain disabled by
/// default so dashboard rendering never depends on internet availability.
pub(super) async fn get_network_block(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let mode = *state.mode_rx.borrow();
    if let Err(resp) = crate::mode_middleware::check_mode_access("/api/network/block", mode) {
        return resp.into_response();
    }

    let network_block = state.network_block.clone();
    let local_node_configured =
        network_block.enabled && !network_block.local_node_rpc_url.trim().is_empty();
    let local_node_reason = if network_block.enabled {
        "Local Bitcoin node source is configured, but this Loop 14 endpoint is manifest-only and performs no live RPC probe yet."
    } else {
        "Local Bitcoin node source is disabled by default."
    };
    let source = if network_block.enabled {
        "local_node"
    } else {
        "unavailable"
    };
    let source_label = if network_block.enabled {
        "Local node manifest"
    } else {
        "Unavailable"
    };

    let latest_share_job = state.recent_share_history.lock().ok().and_then(|events| {
        events
            .iter()
            .rev()
            .find(|event| !event.job_id.trim().is_empty())
            .cloned()
    });

    let pool_job = latest_share_job
        .as_ref()
        .map(|event| {
            serde_json::json!({
                "available": true,
                "source": "recent_share_history",
                "job_id": event.job_id.clone(),
                "last_share_timestamp_ms": event.timestamp_ms,
                "difficulty": event.difficulty,
                "target_difficulty": event.target_difficulty,
                "protocol_meta_present": event.protocol_meta_present,
                "reason": "Recent share history links this miner to a pool job, but REST state does not persist current mining.notify block metadata yet."
            })
        })
        .unwrap_or_else(|| {
            serde_json::json!({
                "available": false,
                "source": "not_persisted",
                "job_id": null,
                "last_share_timestamp_ms": null,
                "difficulty": null,
                "target_difficulty": null,
                "protocol_meta_present": false,
                "reason": "No pool job metadata has been recorded in recent share history."
            })
        });

    Json(serde_json::json!({
        "status": "unavailable",
        "read_only": true,
        "internet_dependency": false,
        "available": false,
        "source": source,
        "source_label": source_label,
        "fetched_at_ms": unix_time_ms(),
        "cache_ttl_ms": network_block.cache_ttl_ms,
        "block_height": null,
        "height": null,
        "block_hash": null,
        "hash": null,
        "timestamp_ms": null,
        "age_s": null,
        "difficulty": null,
        "previous_hash": null,
        "tx_count": null,
        "transaction_count": null,
        "subsidy_btc": null,
        "fees_btc": null,
        "reward_btc": null,
        "reward_source": null,
        "mempool": {
            "available": false,
            "source": "unavailable",
            "fee_rate_sat_vb": null,
            "fastest_fee_sat_vb": null,
            "half_hour_fee_sat_vb": null,
            "hour_fee_sat_vb": null,
            "reason": "No local node mempool source or explicitly enabled public fallback is configured."
        },
        "pool_job": pool_job,
        "source_manifest": {
            "local_node": {
                "enabled": network_block.enabled,
                "configured": local_node_configured,
                "available": false,
                "live_rpc": false,
                "endpoint_label": network_block.redacted_rpc_url(),
                "credential_mode": network_block.credential_source(),
                "request_timeout_ms": network_block.request_timeout_ms,
                "reason": local_node_reason
            },
            "public_fallback": {
                "enabled": false,
                "available": false,
                "reason": "Public blockchain API fallback is disabled by default and no public provider is called by this endpoint."
            },
            "cache": {
                "enabled": false,
                "ttl_ms": network_block.cache_ttl_ms,
                "age_ms": null,
                "reason": "Cache policy is visible for future live local-node lookups; no cached block data exists because live RPC probing is not active."
            }
        },
        "reasons": [
            local_node_reason,
            "Stratum jobs do not provide block height.",
            "Public blockchain API fallback is disabled by default.",
            "No live RPC probe is performed in this manifest-only build.",
            "No fabricated block or mempool data is returned."
        ],
        "limitations": [
            "Dashboard rendering does not depend on internet availability.",
            "This endpoint is read-only and does not collect logs or mutate miner state.",
            "Local node settings are reported as source capability only; credentials are never exposed and no RPC request is made by this loop.",
            "Pool job linkage, when present, comes from real recent share history and may be stale.",
            "Block height, hash, previous hash, difficulty, timestamp, transaction count, reward, and mempool fees stay null until backed by a real configured source."
        ]
    }))
    .into_response()
}

/// GROUP-B: build the read-only 21-step BM1362 silicon-table JSON body.
///
/// LuxOS exposes its CGMiner silicon characterization ladder via the
/// `profiles` API (code 323). The DCENT_OS equivalent is the canonical
/// `dcentrald-silicon-profiles::bm1362::BM1362_TABLE` — 21 discrete steps from
/// `-16` (145 MHz / 11.880 V) to `+4` (645 MHz / 14.400 V), with the nameplate
/// `default` at step 0 and the efficiency sweet spot at step `-9`
/// (320 MHz / 12.45 V ≈ 27.6 J/TH). Each row carries its provenance
/// (`source`: live-confirmed / operator-confirmed / reconstructed) so consumers
/// never mistake an extrapolated row for a measured one.
///
/// Pure (no `AppState`, no HAL) so it's host-testable. Read-only — this surface
/// applies nothing; it documents the silicon envelope the autotuner steps along.
pub(super) fn build_silicon_table_response() -> serde_json::Value {
    use dcentrald_silicon_profiles::bm1362::BM1362_TABLE;

    let levels: Vec<serde_json::Value> = BM1362_TABLE
        .profiles
        .iter()
        .map(|p| {
            serde_json::json!({
                "step": p.step,
                "name": p.profile_name(),
                "freq_mhz": p.freq_mhz,
                "voltage_v": p.voltage_v,
                "wall_watts": p.wall_watts,
                "hashrate_ths": p.hashrate_ths,
                // None for rows lacking watts/hashrate; never a fake number.
                "efficiency_jth": p.watts_per_ths(),
                "heat_btu_per_hour": p.heat_btu_per_hour(),
                "source": format!("{:?}", p.source),
            })
        })
        .collect();

    serde_json::json!({
        "schema": "dcentrald-silicon-profiles::bm1362::BM1362_TABLE v1",
        "chip_family": BM1362_TABLE.chip_family,
        "step_count": BM1362_TABLE.profiles.len(),
        "min_step": BM1362_TABLE.min_step(),
        "max_step": BM1362_TABLE.max_step(),
        "default_step": BM1362_TABLE.default_step,
        "sweet_spot_step": BM1362_TABLE.sweet_spot_step,
        "live_status": format!("{:?}", BM1362_TABLE.live_status),
        "levels": levels,
        "limitations": [
            "Read-only silicon characterization ladder (LuxOS CGMiner profiles code 323 equivalent); this endpoint applies no tuning.",
            "voltage_v is a ~0.03 V-granularity target, not an exact achievable rail value.",
            "Rows with source=Reconstructed are linear-extrapolated from the live-confirmed cadence and must be re-verified on the next live API session.",
            "wall_watts/hashrate_ths/efficiency_jth are null for any row that does not carry measured power/hashrate.",
        ],
    })
}

/// GET /api/profiles/silicon-table -- canonical read-only 21-step BM1362
/// silicon characterization table (LuxOS CGMiner `profiles` code 323
/// equivalent).
pub(super) async fn get_silicon_table() -> impl IntoResponse {
    Json(build_silicon_table_response())
}

/// GET /api/profiles -- Saved tuning profiles.
pub(super) async fn get_profiles(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let mode = *state.mode_rx.borrow();
    if let Err(resp) = crate::mode_middleware::check_mode_access("/api/profiles", mode) {
        return resp.into_response();
    }

    Json(serde_json::json!({
        "profiles": [
            {
                "id": "eco",
                "name": "Eco",
                "description": "Minimal power profile; acoustic result requires live RPM and room-noise verification",
                "frequency_mhz": 300,
                "voltage_pic": 95,
                "voltage_v": 8.88,
                "expected_hashrate_ths": 4.5,
                "expected_watts": 450,
                "expected_efficiency_jth": 100.0,
                "fan_max_pwm": 30,
                "noise_level": "Unverified until live RPM/acoustic measurement",
            },
            {
                "id": "balanced",
                "name": "Balanced",
                "description": "Default hashrate/power profile; acoustic result depends on fan tach and platform floor",
                "frequency_mhz": 500,
                "voltage_pic": 75,
                "voltage_v": 9.0,
                "expected_hashrate_ths": 10.0,
                "expected_watts": 900,
                "expected_efficiency_jth": 90.0,
                "fan_max_pwm": 60,
                "noise_level": "RPM-dependent; not inferred from PWM alone",
            },
            {
                "id": "performance",
                "name": "Performance",
                "description": "Maximum hashrate — stock-level performance with full cooling",
                "frequency_mhz": 650,
                "voltage_pic": 58,
                "voltage_v": 9.1,
                "expected_hashrate_ths": 13.5,
                "expected_watts": 1350,
                "expected_efficiency_jth": 100.0,
                "fan_max_pwm": 100,
                "noise_level": "RPM-dependent; verify live",
            },
            {
                "id": "overclock",
                "name": "Overclock",
                "description": "Beyond stock — higher voltage, higher frequency, more heat. Use at your own risk.",
                "frequency_mhz": 700,
                "voltage_pic": 50,
                "voltage_v": 9.14,
                "expected_hashrate_ths": 15.0,
                "expected_watts": 1500,
                "expected_efficiency_jth": 100.0,
                "fan_max_pwm": 100,
                "noise_level": "RPM-dependent; verify live",
            },
        ],
        "active_profile": "balanced",
    })).into_response()
}

/// POST /api/profiles -- Save current tuning as a named profile.
///
/// Saves a named profile to `/data/dcent/profiles/{name}.json`.
/// Body must include `"name"` field. All other fields are stored as-is.
pub(crate) async fn post_profiles(
    State(state): State<Arc<AppState>>,
    Json(body): Json<serde_json::Value>,
) -> impl IntoResponse {
    if let Err(response) =
        require_antminer_runtime_capability(&state, RuntimeCapability::ConfigRw, "/api/profiles")
    {
        return response;
    }

    let mode = *state.mode_rx.borrow();
    if let Err(resp) = crate::mode_middleware::check_mode_access("/api/profiles", mode) {
        return resp.into_response();
    }

    let name = match body.get("name").and_then(|v| v.as_str()) {
        Some(n) => n.to_string(),
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "status": "error",
                    "message": "Missing required field: name",
                })),
            )
                .into_response();
        }
    };

    // Sanitize name to prevent path traversal
    let safe_name: String = name
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    if safe_name.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "status": "error",
                "message": "Profile name must contain at least one alphanumeric character",
            })),
        )
            .into_response();
    }

    let profile_dir = "/data/dcent/profiles";
    let profile_path = format!("{}/{}.json", profile_dir, safe_name);

    // Ensure directory exists
    if let Err(e) = std::fs::create_dir_all(profile_dir) {
        tracing::error!(error = %e, "Failed to create profile directory");
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({
                "status": "error",
                "message": format!("Failed to create profile directory: {}", e),
            })),
        )
            .into_response();
    }

    // Write profile data
    let json_str = match serde_json::to_string_pretty(&body) {
        Ok(s) => s,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({
                    "status": "error",
                    "message": format!("Failed to serialize profile: {}", e),
                })),
            )
                .into_response();
        }
    };

    match atomic_write(&profile_path, json_str) {
        Ok(()) => {
            tracing::info!(name = %safe_name, path = %profile_path, "Profile saved");
            (
                StatusCode::OK,
                Json(serde_json::json!({
                    "status": "ok",
                    "message": format!("Profile '{}' saved", safe_name),
                    "path": profile_path,
                    "profile": body,
                })),
            )
                .into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, path = %profile_path, "Failed to save profile");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({
                    "status": "error",
                    "message": format!("Failed to write profile: {}", e),
                })),
            )
                .into_response()
        }
    }
}

// ─── Action Handlers ───────────────────────────────────────────────────

/// POST /api/action/restart -- Refuse an unsafe in-process daemon restart.
///
/// A clean process exit is not a physical SafeOff receipt. Preserve the live
/// hardware owner and return a conflict until a durable typed-disposition
/// protocol can authorize supervisor re-admission.
pub(super) async fn post_action_restart(State(state): State<Arc<AppState>>) -> Response {
    if let Err(response) = require_antminer_runtime_capability(
        &state,
        RuntimeCapability::Reboot,
        "/api/action/restart",
    ) {
        return response;
    }

    push_rest_audit_free(
        &state,
        "system",
        "Daemon restart refused: typed hardware disposition unavailable",
    );
    tracing::warn!("{DAEMON_RESTART_REFUSAL}");

    (
        StatusCode::CONFLICT,
        Json(serde_json::json!({
            "status": "restart_refused",
            "message": DAEMON_RESTART_REFUSAL,
        })),
    )
        .into_response()
}

/// POST /api/action/reboot -- Reboot the miner.
pub(super) async fn post_action_reboot(State(state): State<Arc<AppState>>) -> Response {
    if let Err(response) =
        require_antminer_runtime_capability(&state, RuntimeCapability::Reboot, "/api/action/reboot")
    {
        return response;
    }

    // Transfer authority before claiming acceptance. No delay, entropy write,
    // sync, audit/log write, or detached task may stand between capability
    // admission and the init request: any of those can block or disappear with
    // this daemon while PID 1 remains unaware. Critical writes must establish
    // their own durability at transaction time; PID 1 owns final shutdown sync
    // and escalation.
    match trigger_system_reboot().await {
        Ok(()) => {
            tracing::info!("System reboot requested via API and accepted by init");
            // W21 audit coverage is best-effort after authority transfer. A
            // persistent audit lock or storage stall must never prevent PID 1
            // from observing the reboot request and arming its deadline.
            push_rest_audit_free(
                &state,
                "system",
                "System reboot requested via API and accepted by init",
            );
            (
                StatusCode::ACCEPTED,
                Json(serde_json::json!({
                    "status": "reboot_accepted",
                    "message": "System init accepted the orderly reboot request",
                })),
            )
                .into_response()
        }
        Err(error) => {
            tracing::error!(%error, "system init did not accept the reboot request");
            push_rest_audit_free(
                &state,
                "system",
                format!("System reboot request failed before init acceptance: {error}"),
            );
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({
                    "status": "reboot_not_accepted",
                    "message": "System init did not accept the reboot request; the miner remains online",
                })),
            )
                .into_response()
        }
    }
}

/// Submit one real, full **system reboot** request (not a daemon restart).
///
/// The primary path runs `reboot`, which under DCENT_OS's custom PID 1
/// (`dcentos-init`) sends SIGTERM to init -> orderly shutdown -> `reboot(2)`.
/// The live S9's former lost-signal bug is fixed in `dcentos-init`, which owns
/// the one absolute shutdown watchdog. The API must never add an earlier sysrq
/// or `reboot -f` deadline: a valid platform stop path may still be producing
/// its terminal hardware disposition after this daemon submits the request.
#[derive(Debug, thiserror::Error)]
pub(super) enum RebootRequestError {
    #[error("failed to execute the system reboot command: {0}")]
    Spawn(#[source] std::io::Error),
    #[error("system init rejected the reboot request with status {0}")]
    Rejected(std::process::ExitStatus),
}

pub(super) async fn trigger_system_reboot() -> Result<(), RebootRequestError> {
    trigger_system_reboot_with("reboot").await
}

async fn trigger_system_reboot_with(program: &str) -> Result<(), RebootRequestError> {
    // Standard reboot command: SIGTERM -> PID 1 under dcentos-init, or the host
    // init on a passthrough/BraiinsOS rootfs. The command must return success
    // before the API reports acceptance; the exact host-init deadline remains
    // unqualified unless dcentos-init is PID 1.
    let status = tokio::process::Command::new(program)
        .status()
        .await
        .map_err(RebootRequestError::Spawn)?;
    if status.success() {
        tracing::info!("orderly reboot request accepted by system init");
        Ok(())
    } else {
        Err(RebootRequestError::Rejected(status))
    }
}

/// POST /api/action/sleep -- Enter curtailment sleep mode.
pub(crate) async fn post_action_sleep(State(state): State<Arc<AppState>>) -> Response {
    if let Err(response) = require_antminer_runtime_capability(
        &state,
        RuntimeCapability::PowerControl,
        "/api/action/sleep",
    ) {
        return response;
    }

    let supported = state
        .hardware_info
        .lock()
        .ok()
        .map(|info| info.capabilities.sleep_wake_supported)
        .unwrap_or(false);
    if !supported {
        let current = state.curtailment.lock().await.state();
        return (
            StatusCode::NOT_IMPLEMENTED,
            Json(serde_json::json!({
                "status": "error",
                "message": "Sleep mode is not supported on this hardware/runtime path.",
                "state": format!("{:?}", current),
                "supported": false,
            })),
        )
            .into_response();
    }

    let mut curtailment = state.curtailment.lock().await;
    let response = match curtailment.state() {
        dcentrald_thermal::curtailment::CurtailmentState::Active => {
            let _ = curtailment.enter_sleep();
            // W21 audit-coverage: record the operator-initiated curtailment sleep.
            push_rest_audit_free(&state, "curtailment", "Curtailment sleep requested via API");
            tracing::info!("Curtailment sleep requested — thermal loop will power down hash boards on the next control tick");
            (
                StatusCode::ACCEPTED,
                serde_json::json!({
                    "status": "ok",
                    "message": "Sleep transition started. Hash boards will power down and fans will move to the standby envelope.",
                    "state": "EnteringSleep",
                    "supported": true,
                }),
            )
        }
        dcentrald_thermal::curtailment::CurtailmentState::EnteringSleep
        | dcentrald_thermal::curtailment::CurtailmentState::Sleeping => (
            StatusCode::OK,
            serde_json::json!({
                "status": "ok",
                "message": "Miner is already entering sleep or sleeping.",
                "state": format!("{:?}", curtailment.state()),
                "supported": true,
            }),
        ),
        dcentrald_thermal::curtailment::CurtailmentState::Waking => (
            StatusCode::CONFLICT,
            serde_json::json!({
                "status": "error",
                "message": "Wake transition already in progress. Wait for wake to finish before requesting sleep again.",
                "state": "Waking",
                "supported": true,
            }),
        ),
    };
    (response.0, Json(response.1)).into_response()
}

/// POST /api/action/wake -- Wake from curtailment sleep mode.
pub(crate) async fn post_action_wake(State(state): State<Arc<AppState>>) -> Response {
    if let Err(response) = require_antminer_runtime_capability(
        &state,
        RuntimeCapability::PowerControl,
        "/api/action/wake",
    ) {
        return response;
    }

    let supported = state
        .hardware_info
        .lock()
        .ok()
        .map(|info| info.capabilities.sleep_wake_supported)
        .unwrap_or(false);
    if !supported {
        let current = state.curtailment.lock().await.state();
        return (
            StatusCode::NOT_IMPLEMENTED,
            Json(serde_json::json!({
                "status": "error",
                "message": "Wake mode is not supported on this hardware/runtime path.",
                "state": format!("{:?}", current),
                "supported": false,
            })),
        )
            .into_response();
    }

    let mut curtailment = state.curtailment.lock().await;
    let response = match curtailment.state() {
        dcentrald_thermal::curtailment::CurtailmentState::Sleeping => {
            let _ = curtailment.wake();
            // W21 audit-coverage: record the operator-initiated curtailment wake.
            push_rest_audit_free(&state, "curtailment", "Curtailment wake requested via API");
            tracing::info!("Curtailment wake requested — thermal loop will restore controller outputs on the next control tick");
            (
                StatusCode::ACCEPTED,
                serde_json::json!({
                    "status": "ok",
                    "message": "Wake transition started. Controller outputs will be restored on the next thermal tick.",
                    "state": "Waking",
                    "supported": true,
                }),
            )
        }
        dcentrald_thermal::curtailment::CurtailmentState::Waking
        | dcentrald_thermal::curtailment::CurtailmentState::Active => (
            StatusCode::OK,
            serde_json::json!({
                "status": "ok",
                "message": "Miner is already waking or active.",
                "state": format!("{:?}", curtailment.state()),
                "supported": true,
            }),
        ),
        dcentrald_thermal::curtailment::CurtailmentState::EnteringSleep => (
            StatusCode::CONFLICT,
            serde_json::json!({
                "status": "error",
                "message": "Sleep transition still in progress. Wait for sleep to complete before waking.",
                "state": "EnteringSleep",
                "supported": true,
            }),
        ),
    };
    (response.0, Json(response.1)).into_response()
}

// ─── Fan Control Handler ─────────────────────────────────────────────

/// Pure, host-testable computation of the actually-commanded fan PWM for a
/// requested fan mode. This is the SINGLE source of truth for the per-mode
/// safety envelope (incl. the load-bearing HOME PWM-30 hard cap) shared by both
/// `POST /api/fan` and the gRPC `grpc_bridge_set_fan` bridge, so the cap can
/// never diverge between the two control planes.
///
/// `mode_str` is the preset name (`quiet|balanced|performance|custom`).
/// `custom_pwm` is the operator's requested duty when `mode_str == "custom"`.
/// `allow_loud` opts above [`dcentrald_hal::fan::PWM_SAFETY_MAX`] (PWM 30) up to
/// the per-mode maximum — but only for non-Home contexts; Home is hard-capped
/// regardless. Returns `Err` only when a custom PWM is below the per-mode safety
/// minimum on a non-Hacker mode (mirrors the REST 400 response).
pub(crate) fn compute_commanded_fan_pwm(
    current_mode: crate::OperatingMode,
    mode_str: &str,
    custom_pwm: Option<u8>,
    allow_loud: bool,
) -> std::result::Result<u8, String> {
    // Determine the safety envelope for the operating mode. Home mode commands
    // PWM 10 by default and is HARD-capped at PWM 30 (load-bearing). Physical
    // noise must be checked via RPM/tach, especially on AM2/XIL boards with a
    // low-PWM fan floor.
    let safety_min_fan_pwm: u8 = match current_mode {
        crate::OperatingMode::Home => 10,
        crate::OperatingMode::Standard => 20,
        crate::OperatingMode::Hacker => 0,
    };
    let safety_max_fan_pwm: u8 = match current_mode {
        crate::OperatingMode::Home => 30,
        crate::OperatingMode::Standard => 60,
        crate::OperatingMode::Hacker => 100,
    };
    // `allow_loud` only lifts the ceiling to the per-mode max. With it unset,
    // the universal PWM-30 safety cap applies. NOTE: for Home the per-mode max
    // is itself 30, so a home unit can NEVER exceed PWM 30 here — allow_loud
    // does not widen the Home ceiling.
    let ceiling = if allow_loud {
        safety_max_fan_pwm
    } else {
        safety_max_fan_pwm.min(dcentrald_hal::fan::PWM_SAFETY_MAX)
    };
    // Defensive floor on the ceiling: the return below is
    // `requested_pwm.clamp(safety_min_fan_pwm, effective_max_fan_pwm)`, and
    // `u8::clamp` PANICS when min > max. Today PWM_SAFETY_MAX (30) >= every
    // per-mode safety_min, so it never inverts — but that is a coincidence of
    // constants across two independent tables, and the workspace is
    // `panic = "abort"`, so a panic here would abort the daemon from a REST
    // fan-set call. A future lowering of PWM_SAFETY_MAX or a raised safety_min
    // would invert it. Cross-firmware evidence this class is real: Mujina issue
    // #49 ("Assertion failed: min <= max" scheduler panic after runtime bound
    // drift). Guarded the same way as solar.rs / power_budget.rs (687bd2ed) —
    // when the noise ceiling collides with the thermal-airflow floor, the floor
    // MUST win (airflow is the fail-safe), never a clamp panic.
    let effective_max_fan_pwm = ceiling.max(safety_min_fan_pwm);

    let requested_pwm: u8 = match mode_str {
        "quiet" => 10,        // home idle command; physical RPM is hardware-dependent
        "balanced" => 50,     // mid-range command; physical RPM is platform-dependent
        "performance" => 100, // full cooling command
        "custom" => custom_pwm.unwrap_or(10).min(100),
        _ => 10,
    };

    // SAFETY (2026-04-11): reject a custom PWM below the per-mode minimum on
    // non-Hacker modes (Hacker takes responsibility). Mirrors the REST 400.
    if mode_str == "custom" && current_mode != crate::OperatingMode::Hacker {
        if let Some(raw) = custom_pwm {
            if raw < safety_min_fan_pwm {
                return Err(format!(
                    "Custom PWM {} is below safety minimum {} for {} mode. Switch to Hacker mode to override.",
                    raw, safety_min_fan_pwm, current_mode
                ));
            }
        }
    }

    Ok(requested_pwm.clamp(safety_min_fan_pwm, effective_max_fan_pwm))
}

pub(super) fn set_fan_pwm_via_hal(
    _hardware_mutation_lease: &dcentrald_hal::platform::HardwareMutationLease,
    _pwm: u8,
) -> std::result::Result<(u8, dcentrald_hal::fan::FanVariant, u8, u8, u8, u32), String> {
    Err(
        "fan mutation unavailable until the mining runtime exposes a serialized command broker"
            .to_string(),
    )
}

/// Acquire shared teardown admission for a future brokered hardware write.
///
/// The returned RAII lease must stay live through the final broker response.
/// Requiring a reference to it in [`set_fan_pwm_via_hal`] prevents future fan
/// call sites from silently bypassing the mutation barrier.
pub(super) fn acquire_hardware_mutation_lease(
    state: &AppState,
    operation: &str,
) -> std::result::Result<dcentrald_hal::platform::HardwareMutationLease, String> {
    state
        .hardware_mutation_gate
        .try_acquire()
        .map_err(|error| format!("{operation}: {error}"))
}

pub(super) fn read_fan_via_hal(
) -> std::result::Result<(u8, dcentrald_hal::fan::FanVariant, u8, u8, u8, u32), String> {
    Err("fan reads must consume the runtime telemetry snapshot".to_string())
}

/// POST /api/fan -- Set fan speed mode.
///
/// Accepts JSON: { "mode": "quiet|balanced|performance|custom", "target_pwm": 50 }
/// Maps mode presets to PWM values, or uses target_pwm for custom mode.
/// Physical application is unavailable until the mining runtime exposes a
/// serialized command broker. This request never opens UIO or devmem.
/// Normal dashboard/API requests are home-capped at PWM 30. Higher requests
/// require explicit `allow_loud: true`.
pub(crate) async fn post_fan(
    State(state): State<Arc<AppState>>,
    Json(body): Json<serde_json::Value>,
) -> Response {
    if let Err(response) =
        require_antminer_runtime_capability(&state, RuntimeCapability::PowerControl, "/api/fan")
    {
        return response;
    }

    let mode_str = body.get("mode").and_then(|v| v.as_str()).unwrap_or("quiet");
    let allow_loud = body
        .get("allow_loud")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let custom_pwm = body.get("target_pwm").and_then(|v| {
        // Clamp to the 0..=100 PWM domain BEFORE the u8 cast. A bare `as u8` wraps
        // (300 -> 44, and 512 -> 0 = fan OFF when maximum airflow was requested),
        // silently turning an out-of-range request into a wrong/dangerous value.
        // Accept both integer and float JSON numbers. The per-mode envelope below
        // still caps this further.
        v.as_u64()
            .map(|n| n.min(100) as u8)
            .or_else(|| v.as_f64().map(|f| f.clamp(0.0, 100.0) as u8))
    });

    // Determine safety minimum based on operating mode. Home mode commands PWM
    // 10 by default. Physical noise must be checked via RPM/tach, especially
    // on AM2/XIL boards with a low-PWM fan floor. Normal requests are capped at
    // the home maximum; explicit allow_loud is required above PWM 30. The whole
    // envelope (incl. the load-bearing HOME PWM-30 hard cap) is computed by the
    // shared `compute_commanded_fan_pwm` helper so the REST and gRPC-bridge
    // control planes can never diverge.
    let current_mode = *state.mode_rx.borrow();
    // Mirror the helper's reported envelope for the response body (the helper
    // is the source of truth for the actual clamp).
    let safety_min_fan_pwm: u8 = match current_mode {
        crate::OperatingMode::Home => 10,
        crate::OperatingMode::Standard => 20,
        crate::OperatingMode::Hacker => 0,
    };
    let effective_max_fan_pwm = {
        let per_mode_max: u8 = match current_mode {
            crate::OperatingMode::Home => 30,
            crate::OperatingMode::Standard => 60,
            crate::OperatingMode::Hacker => 100,
        };
        if allow_loud {
            per_mode_max
        } else {
            per_mode_max.min(dcentrald_hal::fan::PWM_SAFETY_MAX)
        }
    };

    let requested_pwm: u8 = match mode_str {
        "quiet" => 10,
        "balanced" => 50,
        "performance" => 100,
        "custom" => custom_pwm.unwrap_or(10).min(100),
        _ => 10,
    };

    let pwm = match compute_commanded_fan_pwm(current_mode, mode_str, custom_pwm, allow_loud) {
        Ok(pwm) => pwm,
        Err(message) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "status": "error",
                    "message": message,
                })),
            )
                .into_response();
        }
    };

    let hardware_mutation_lease = match acquire_hardware_mutation_lease(&state, "POST /api/fan") {
        Ok(lease) => lease,
        Err(detail) => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({
                    "status": "error",
                    "message": "hardware mutation admission rejected",
                    "detail": detail,
                })),
            )
                .into_response();
        }
    };

    match set_fan_pwm_via_hal(&hardware_mutation_lease, pwm) {
        Ok((uio, variant, commanded_pwm, commanded_pwm0, commanded_pwm1, max_rpm)) => {
            let physical_floor_warning = pwm <= 10 && max_rpm >= 2000;
            tracing::info!(
                mode = mode_str,
                pwm,
                requested_pwm,
                allow_loud,
                safety_min = safety_min_fan_pwm,
                safety_max = effective_max_fan_pwm,
                uio,
                ?variant,
                commanded_pwm,
                max_rpm,
                physical_floor_warning,
                "Fan PWM command accepted via API"
            );
            // W21 audit-coverage: record the operator-committed fan PWM
            // (incl. allow_loud above the home cap).
            push_rest_audit_free(
                &state,
                "fan",
                format!("Fan PWM set via API: mode={mode_str} pwm={pwm} allow_loud={allow_loud}"),
            );
            (
                StatusCode::OK,
                Json(serde_json::json!({
                    "status": "ok",
                    "mode": mode_str,
                    "pwm": pwm,
                    "requested_pwm": requested_pwm,
                    "allow_loud": allow_loud,
                    "clamped": pwm != requested_pwm,
                    "uio": uio,
                    "variant": format!("{:?}", variant),
                    "commanded_pwm": commanded_pwm,
                    "commanded_pwm0": commanded_pwm0,
                    "commanded_pwm1": commanded_pwm1,
                    "max_rpm": max_rpm,
                    "physical_floor_warning": physical_floor_warning,
                    "safety_min_pwm": safety_min_fan_pwm,
                    "safety_max_pwm": effective_max_fan_pwm,
                    "message": format!("Fan PWM command accepted for {} (PWM {})", mode_str, pwm),
                })),
            )
                .into_response()
        }
        Err(e) => (
            StatusCode::NOT_IMPLEMENTED,
            Json(serde_json::json!({
                "status": "unavailable",
                "hardware_access_attempted": false,
                "message": format!("Fan command broker unavailable: {}", e),
            })),
        )
            .into_response(),
    }
}

// ─── PSU Override Handlers ──────────────────────────────────────────

#[cfg(test)]
mod fan_mutation_admission_tests {
    use super::*;

    fn granted_state_with_gate(
        gate: dcentrald_hal::platform::HardwareMutationGate,
    ) -> Arc<AppState> {
        let state = crate::build_minimal_app_state_with_hardware_mutation_gate(
            crate::MinimalAppStateInputs {
                api_config: crate::ApiConfig::default(),
                pool_url: String::new(),
                pool_protocol: "sv1".to_string(),
                mode: crate::OperatingMode::Standard,
                firmware_version: "fan-mutation-admission-test".to_string(),
                fan_pwm: 20,
                network_block: crate::NetworkBlockConfig::default(),
                profile_path: "/tmp/fan-mutation-admission-test".to_string(),
                control_board_label: "test".to_string(),
                chip_type_label: "BM1387".to_string(),
                external_state_rx: None,
            },
            gate,
        );
        {
            let mut hardware = state.hardware_info.lock().unwrap();
            hardware.identification = crate::HardwareIdentification::from_evidence(
                vec![
                    crate::HardwareIdentityEvidence::declared_asic_board_target("am1-s9", "BM1387"),
                    crate::HardwareIdentityEvidence::measured_asic_enumeration(
                        0x1387,
                        "BM1387",
                        crate::HardwareCompositionToken::new(1, "test:am1-s9"),
                    ),
                ],
                Some("test S9 enumeration evidence".to_string()),
            );
        }
        state
    }

    async fn assert_rest_fan_admission_denied(state: Arc<AppState>, expected_detail: &str) {
        let response = post_fan(
            State(state),
            Json(serde_json::json!({
                "mode": "custom",
                "target_pwm": 30,
            })),
        )
        .await;

        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let bytes = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(
            body["message"].as_str(),
            Some("hardware mutation admission rejected")
        );
        let detail = body["detail"].as_str().unwrap_or_default();
        assert!(
            detail.contains(expected_detail),
            "expected `{expected_detail}` in admission detail, got: {detail}"
        );
    }

    #[tokio::test]
    async fn rest_fan_rejects_pending_mutation_admission_before_hal() {
        let gate_owner = dcentrald_hal::platform::HardwareMutationGateOwner::new_pending();
        let state = granted_state_with_gate(gate_owner.gate());

        assert_rest_fan_admission_denied(state, "pending mining readiness").await;
    }

    #[tokio::test]
    async fn rest_fan_rejects_closed_mutation_admission_before_hal() {
        let state =
            granted_state_with_gate(dcentrald_hal::platform::HardwareMutationGate::new_closed());

        assert_rest_fan_admission_denied(state, "closed for teardown").await;
    }
}

/// GET /api/config/power-calibration -- Current wall-meter calibration state.
pub(super) async fn get_power_calibration(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let calibration = state
        .power_calibration
        .read()
        .map(|cal| cal.clone())
        .unwrap_or_default();
    let miner = state.state_rx.borrow().clone();
    let live_power = state.power_rx.borrow().clone();
    let hardware = state
        .hardware_info
        .lock()
        .map(|guard| guard.clone())
        .unwrap_or_default();
    let power_projection = project_power_telemetry(&live_power, &miner, &hardware);
    let power_contract = project_power_calibration_contract(&power_projection, None);

    Json(serde_json::json!({
        "enabled": calibration.enabled,
        "multiplier": calibration.effective_multiplier(),
        "reference_wall_watts": calibration.reference_wall_watts,
        "estimated_wall_watts": calibration.estimated_wall_watts,
        "estimated_unit_watts": calibration.estimated_board_watts,
        "updated_at_ms": calibration.updated_at_ms,
        "current_reported_wall_watts": power_contract.current_reported_wall_watts,
        "current_reported_unit_watts": power_contract.current_reported_unit_watts,
        "power_source": power_contract.power_source,
        "power_source_detail": power_contract.power_source_detail,
        "live_power_available": power_contract.live_power_available,
        "power_modeled": power_contract.power_modeled,
        "power_note": power_contract.power_note,
        "calibrated": power_contract.calibrated,
        "calibration_multiplier": power_contract.calibration_multiplier,
    }))
}

/// POST /api/config/power-calibration -- Anchor the estimate to an external wall meter.
///
/// Body: { "measured_wall_watts": 1310 }
/// Or:   { "enabled": false } to clear the saved correction.
pub(super) async fn post_power_calibration(
    State(state): State<Arc<AppState>>,
    Json(body): Json<serde_json::Value>,
) -> Response {
    if let Err(response) = require_antminer_runtime_capability(
        &state,
        RuntimeCapability::ConfigRw,
        "/api/config/power-calibration",
    ) {
        return response;
    }

    let disable = body
        .get("enabled")
        .and_then(|v| v.as_bool())
        .map(|enabled| !enabled)
        .unwrap_or(false);

    if disable {
        let cleared = dcentrald_autotuner::PowerCalibration::default();
        if let Err(e) = persist_power_calibration(None) {
            return Json(serde_json::json!({
                "status": "error",
                "message": e,
            }))
            .into_response();
        }
        if let Ok(mut guard) = state.power_calibration.write() {
            *guard = cleared;
        }

        let miner = state.state_rx.borrow().clone();
        let live_power = state.power_rx.borrow().clone();
        let hardware = state
            .hardware_info
            .lock()
            .map(|guard| guard.clone())
            .unwrap_or_default();
        let power_projection = project_power_telemetry(&live_power, &miner, &hardware);
        let power_contract = project_power_calibration_contract(&power_projection, None);
        push_rest_audit_free(&state, "power_calibration", "Power calibration cleared");
        return Json(serde_json::json!({
            "status": "ok",
            "message": "Power calibration cleared. The uncalibrated estimate will return on the next power refresh.",
            "enabled": false,
            "multiplier": 1.0,
            "current_reported_wall_watts": power_contract.current_reported_wall_watts,
            "current_reported_unit_watts": power_contract.current_reported_unit_watts,
            "power_source": power_contract.power_source,
            "power_source_detail": power_contract.power_source_detail,
            "live_power_available": power_contract.live_power_available,
            "power_modeled": power_contract.power_modeled,
            "power_note": power_contract.power_note,
            "calibrated": power_contract.calibrated,
            "calibration_multiplier": power_contract.calibration_multiplier,
        }))
        .into_response();
    }

    let measured_wall_watts = body
        .get("measured_wall_watts")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);

    if !(50.0..=5000.0).contains(&measured_wall_watts) {
        return Json(serde_json::json!({
            "status": "error",
            "message": "Measured wall watts must be between 50W and 5000W.",
        }))
        .into_response();
    }

    let miner = state.state_rx.borrow().clone();
    let live_power = state.power_rx.borrow().clone();
    let hardware = state
        .hardware_info
        .lock()
        .map(|guard| guard.clone())
        .unwrap_or_default();
    let power_projection = project_power_telemetry(&live_power, &miner, &hardware);
    let power_contract = project_power_calibration_contract(&power_projection, None);
    if power_contract.current_reported_wall_watts <= 0.0 {
        return Json(serde_json::json!({
            "status": "error",
            "message": "Live power estimate is not available yet. Let the miner run for a few seconds and try again.",
            "current_reported_wall_watts": power_contract.current_reported_wall_watts,
            "current_reported_unit_watts": power_contract.current_reported_unit_watts,
            "power_source": power_contract.power_source,
            "power_source_detail": power_contract.power_source_detail,
            "live_power_available": power_contract.live_power_available,
            "power_modeled": power_contract.power_modeled,
            "power_note": power_contract.power_note,
        }))
        .into_response();
    }

    let multiplier = measured_wall_watts / power_contract.current_reported_wall_watts;
    if !(0.5..=1.5).contains(&multiplier) {
        return Json(serde_json::json!({
            "status": "error",
            "message": "Calibration multiplier is out of range. Re-check the wall-meter reading and only calibrate while the miner is stable.",
            "estimated_wall_watts": power_contract.current_reported_wall_watts,
            "estimated_unit_watts": power_contract.current_reported_unit_watts,
            "measured_wall_watts": measured_wall_watts,
            "multiplier": multiplier,
            "power_source": power_contract.power_source,
            "power_source_detail": power_contract.power_source_detail,
            "live_power_available": power_contract.live_power_available,
            "power_modeled": power_contract.power_modeled,
            "power_note": power_contract.power_note,
        }))
        .into_response();
    }

    let calibration = dcentrald_autotuner::PowerCalibration {
        enabled: true,
        multiplier,
        reference_wall_watts: Some(measured_wall_watts),
        estimated_wall_watts: Some(power_contract.current_reported_wall_watts),
        estimated_board_watts: Some(power_contract.current_reported_unit_watts),
        updated_at_ms: Some(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64,
        ),
        // /api/config/power-calibration is the generic wall-meter anchor
        // and does NOT mean the operator confirmed a J/TH source-of-truth.
        // The dedicated /api/perf/calibrate endpoint (W9.4) sets these.
        operator_confirmed: false,
        confirmed_hashrate_ths: None,
    };

    if let Err(e) = persist_power_calibration(Some(&calibration)) {
        return Json(serde_json::json!({
            "status": "error",
            "message": e,
        }))
        .into_response();
    }

    if let Ok(mut guard) = state.power_calibration.write() {
        *guard = calibration.clone();
    }
    let power_contract = project_power_calibration_contract(
        &power_projection,
        Some(calibration.effective_multiplier()),
    );

    push_rest_audit_free(
        &state,
        "power_calibration",
        format!(
            "Power calibration saved: multiplier={:.4}, measured_wall_watts={:.1}",
            multiplier, measured_wall_watts
        ),
    );

    Json(serde_json::json!({
        "status": "ok",
        "message": "Power calibration saved. Updated wall and unit estimates will appear on the next power refresh.",
        "enabled": true,
        "multiplier": calibration.effective_multiplier(),
        "reference_wall_watts": calibration.reference_wall_watts,
        "estimated_wall_watts": calibration.estimated_wall_watts,
        "estimated_unit_watts": calibration.estimated_board_watts,
        "projected_wall_watts": power_contract.projected_wall_watts,
        "projected_unit_watts": power_contract.projected_unit_watts,
        "power_source": power_contract.power_source,
        "power_source_detail": power_contract.power_source_detail,
        "live_power_available": power_contract.live_power_available,
        "power_modeled": power_contract.power_modeled,
        "power_note": power_contract.power_note,
        "calibrated": power_contract.calibrated,
        "calibration_multiplier": power_contract.calibration_multiplier,
        "projected_power_source_detail": "wall_calibrated_estimate",
        "projected_power_live_available": power_contract.live_power_available,
        "projected_power_modeled": true,
        "projected_power_note": "Projected watts apply the wall-meter multiplier to the current reported estimate; they are not direct live wall-meter telemetry.",
    }))
    .into_response()
}

/// GET /api/config/psu-override -- Current PSU override state.
pub(super) async fn get_psu_override(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let hw = state
        .hardware_info
        .lock()
        .map(|g| g.clone())
        .unwrap_or_default();

    // Read stored override config from TOML (single source of truth for override settings)
    let stored_config: Option<toml::Value> = ["/data/dcentrald.toml", "/etc/dcentrald.toml"]
        .iter()
        .find_map(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| s.parse::<toml::Value>().ok())
        .and_then(|doc| doc.get("power")?.get("psu_override").cloned());

    let stored_voltage = stored_config
        .as_ref()
        .and_then(|c| c.get("voltage_v")?.as_float())
        .unwrap_or(12.0);
    let stored_model = stored_config
        .as_ref()
        .and_then(|c| c.get("model")?.as_str().map(|s| s.to_string()))
        .unwrap_or_default();
    //  (2026-05-22) — surface operator-declared hardware variant for
    // dashboard PsuOverrideStep. Defaults to None when the field is absent
    // (backward-compatible with units that have never been touched by the
    //  wizard). See `config.rs::PsuOverride::psu_hardware_variant`.
    let stored_psu_hardware_variant = stored_config.as_ref().and_then(|c| {
        c.get("psu_hardware_variant")?
            .as_str()
            .map(|s| s.to_string())
    });

    Json(serde_json::json!({
        "active": hw.psu_override_active,
        "model": if hw.psu_override_active { stored_model } else { hw.psu_model.unwrap_or_default() },
        "voltage_v": stored_voltage,
        "voltage_range": hw.psu_voltage_range.unwrap_or_default(),
        "psu_hardware_variant": stored_psu_hardware_variant,
        "available_models": [
            { "id": "APW3", "name": "APW3 / APW3++", "voltage_range": "11.60 - 13.00 V" },
            { "id": "APW7", "name": "APW7", "voltage_range": "11.60 - 14.50 V" },
            { "id": "APW9", "name": "APW9 / APW9+", "voltage_range": "14.10 - 21.00 V" },
            { "id": "APW12", "name": "APW12 / APW121215", "voltage_range": "11.96 - 15.20 V" },
            { "id": "custom", "name": "Custom / Other", "voltage_range": "10.00 - 20.00 V" },
        ],
    }))
}

/// POST /api/config/psu-override -- Set PSU override.
///
/// Body: { "enabled": true, "model": "APW7", "voltage_v": 12.0 }
/// When enabled, skips PSU I2C probing and uses the declared fixed voltage.
/// Eliminates need for Pivotal Pleb Tech Loki device on S19-S21 with APW3/APW7.
pub(super) async fn post_psu_override(
    State(state): State<Arc<AppState>>,
    Json(body): Json<serde_json::Value>,
) -> Response {
    // CE-111: dedicated power-policy writer — gate on ConfigRw AND PowerControl
    // (this persists [power] override, live power authority).
    if let Err(response) = require_antminer_runtime_capability(
        &state,
        RuntimeCapability::ConfigRw,
        "/api/config/psu-override",
    ) {
        return response;
    }
    if let Err(response) = require_antminer_runtime_capability(
        &state,
        RuntimeCapability::PowerControl,
        "/api/config/psu-override",
    ) {
        return response;
    }

    let enabled = body
        .get("enabled")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let model = body
        .get("model")
        .and_then(|v| v.as_str())
        .unwrap_or("APW7")
        .to_string();
    let voltage_v = body
        .get("voltage_v")
        .and_then(|v| v.as_f64())
        .unwrap_or(12.0);
    //  (2026-05-22) — optional operator-declared hardware variant
    // (e.g. "loki" / "bare-apw3" / "stock-apw12"). Missing field => None
    // for backward compatibility with pre- dashboard clients.
    let psu_hardware_variant = body
        .get("psu_hardware_variant")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    // Validate model allowlist (matches available_models in GET /api/config/psu-override).
    // Accepting unknown strings would pollute config and confuse downstream efficiency tables.
    const ALLOWED_MODELS: &[&str] = &["APW3", "APW7", "APW9", "APW12", "custom"];
    if enabled && !ALLOWED_MODELS.contains(&model.as_str()) {
        return Json(serde_json::json!({
            "status": "error",
            "message": format!("Unknown PSU model '{}'. Allowed: {}.", model, ALLOWED_MODELS.join(", ")),
        }))
        .into_response();
    }

    // Validate voltage range (safety)
    if !voltage_v.is_finite() || !(10.0..=20.0).contains(&voltage_v) {
        return Json(serde_json::json!({
            "status": "error",
            "message": "Voltage must be between 10.0V and 20.0V",
        }))
        .into_response();
    }

    // Persist to config file FIRST — only update in-memory state on success
    let mut persisted = false;
    let _cfg_write_guard = crate::atomic_io::config_write_lock();
    let config_paths = ["/data/dcentrald.toml", "/etc/dcentrald.toml"];
    for config_path in &config_paths {
        if std::path::Path::new(config_path).exists() {
            match std::fs::read_to_string(config_path) {
                Ok(contents) => {
                    match contents.parse::<toml::Value>() {
                        Ok(mut doc) => {
                            match ensure_toml_value_table_section(&mut doc, "power") {
                                Ok(power) => {
                                    if enabled {
                                        let mut ovr = toml::value::Table::new();
                                        ovr.insert("enabled".into(), toml::Value::Boolean(true));
                                        ovr.insert(
                                            "model".into(),
                                            toml::Value::String(model.clone()),
                                        );
                                        ovr.insert(
                                            "voltage_v".into(),
                                            toml::Value::Float(voltage_v),
                                        );
                                        //  (2026-05-22) — operator-declared hardware
                                        // variant metadata. Only persist when the dashboard
                                        // sent a value; absent => keep TOML byte-identical
                                        // to pre- saved configs.
                                        if let Some(variant) = psu_hardware_variant.as_ref() {
                                            ovr.insert(
                                                "psu_hardware_variant".into(),
                                                toml::Value::String(variant.clone()),
                                            );
                                        }
                                        power
                                            .insert("psu_override".into(), toml::Value::Table(ovr));
                                    } else {
                                        // Clean removal — don't leave stale override in config
                                        power.remove("psu_override");
                                    }

                                    match toml::to_string_pretty(&doc) {
                                        Ok(serialized) => {
                                            match atomic_write(config_path, &serialized) {
                                                Ok(_) => {
                                                    persisted = true;
                                                    tracing::info!(
                                                        path = config_path,
                                                        "PSU override saved to config"
                                                    );
                                                }
                                                Err(e) => {
                                                    tracing::warn!(error = %e, path = config_path, "Failed to write PSU override config")
                                                }
                                            }
                                        }
                                        Err(e) => {
                                            tracing::warn!(error = %e, "Failed to serialize PSU override config")
                                        }
                                    }
                                }
                                Err(e) => {
                                    tracing::warn!(error = %e, path = config_path, "Failed to update PSU override config")
                                }
                            }
                        }
                        Err(e) => {
                            tracing::warn!(error = %e, path = config_path, "Failed to parse config TOML")
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, path = config_path, "Failed to read config file")
                }
            }
            break;
        }
    }

    if !persisted {
        tracing::warn!("PSU override applied in memory only — no config file was writable");
    }

    // Update hardware info in memory (after persistence attempt)
    if let Ok(mut hw) = state.hardware_info.lock() {
        hw.psu_override_active = enabled;
        if enabled {
            hw.psu_model = Some(model.clone());
            hw.psu_voltage_range = Some(format!("{:.2} V (fixed)", voltage_v));
            hw.psu_fw_version = None;
            hw.psu_serial = None;
        } else {
            // Clear override data so stale values don't persist in the API
            hw.psu_model = None;
            hw.psu_fw_version = None;
            hw.psu_serial = None;
            hw.psu_voltage_range = None;
        }
    }

    tracing::info!(
        enabled,
        model = %model,
        voltage_v,
        persisted,
        "PSU override updated"
    );
    push_rest_audit_free(
        &state,
        "psu_override",
        format!(
            "PSU override updated: enabled={}, model={}, voltage_v={:.2}, persisted={}",
            enabled, model, voltage_v, persisted
        ),
    );

    Json(serde_json::json!({
        "status": if persisted { "ok" } else { "warning" },
        "enabled": enabled,
        "model": model,
        "voltage_v": voltage_v,
        "psu_hardware_variant": psu_hardware_variant,
        "persisted": persisted,
        "restart_required": true,
        "message": if enabled {
            format!("PSU override active: {} at {:.2}V — no Loki device needed. Restart dcentrald to apply efficiency changes.{}", model, voltage_v,
                if !persisted { " (NOT saved to disk — config file read-only)" } else { "" })
        } else {
            "PSU override disabled — auto-detect via I2C".to_string()
        },
    }))
    .into_response()
}

// ─── Off-Grid / Direct DC Handlers ──────────────────────────────────

pub(super) fn default_offgrid_source_profile() -> String {
    "direct_dc".to_string()
}

pub(super) fn default_offgrid_battery_preset() -> String {
    "lifepo4_48v".to_string()
}

pub(super) fn default_offgrid_freq_step() -> u16 {
    25
}

pub(super) fn default_offgrid_min_freq() -> u16 {
    200
}

pub(super) fn default_offgrid_interval_ms() -> u64 {
    2000
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub(super) struct OffGridConfigPayload {
    source_profile: String,
    enabled: bool,
    battery_preset: String,
    adc: Option<dcentrald_hal::adc::AdcBackendConfig>,
    freq_step_mhz: u16,
    min_frequency_mhz: u16,
    loop_interval_ms: u64,
    custom_critical_v: Option<f32>,
    custom_low_v: Option<f32>,
    custom_high_v: Option<f32>,
    custom_full_v: Option<f32>,
    custom_recovery_v: Option<f32>,
}

impl Default for OffGridConfigPayload {
    fn default() -> Self {
        Self {
            source_profile: default_offgrid_source_profile(),
            enabled: false,
            battery_preset: default_offgrid_battery_preset(),
            adc: None,
            freq_step_mhz: default_offgrid_freq_step(),
            min_frequency_mhz: default_offgrid_min_freq(),
            loop_interval_ms: default_offgrid_interval_ms(),
            custom_critical_v: None,
            custom_low_v: None,
            custom_high_v: None,
            custom_full_v: None,
            custom_recovery_v: None,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub(super) struct OffGridConfigResponse {
    #[serde(flatten)]
    config: OffGridConfigPayload,
    ready: bool,
    restart_required: bool,
    readiness_message: String,
}

pub(super) fn battery_thresholds_for_preset(
    preset: &str,
) -> dcentrald_thermal::battery::VoltageThresholds {
    match preset {
        "lifepo4_48v" => dcentrald_thermal::battery::BatteryPreset::LiFePO4_48V.thresholds(),
        "lifepo4_24v" => dcentrald_thermal::battery::BatteryPreset::LiFePO4_24V.thresholds(),
        "lifepo4_12v" => dcentrald_thermal::battery::BatteryPreset::LiFePO4_12V.thresholds(),
        "lead_acid_48v" => dcentrald_thermal::battery::BatteryPreset::LeadAcid_48V.thresholds(),
        "lead_acid_24v" => dcentrald_thermal::battery::BatteryPreset::LeadAcid_24V.thresholds(),
        "lead_acid_12v" => dcentrald_thermal::battery::BatteryPreset::LeadAcid_12V.thresholds(),
        "custom" => dcentrald_thermal::battery::BatteryPreset::Custom.thresholds(),
        _ => dcentrald_thermal::battery::BatteryPreset::LiFePO4_48V.thresholds(),
    }
}

pub(super) fn offgrid_adc_is_simulated(adc: &dcentrald_hal::adc::AdcBackendConfig) -> bool {
    matches!(adc, dcentrald_hal::adc::AdcBackendConfig::Simulated { .. })
}

pub(super) fn validate_offgrid_payload(
    payload: &OffGridConfigPayload,
) -> std::result::Result<(), String> {
    if payload.adc.as_ref().is_some_and(offgrid_adc_is_simulated) {
        return Err(
            "Simulated ADC is lab-only and cannot be saved as off-grid protection config. Use Test ADC Path for simulated lab probes; choose INA226 or Sysfs ADC before saving commissioning config."
                .to_string(),
        );
    }

    if payload.enabled && payload.adc.is_none() {
        return Err(
            "Off-grid mode requires an explicit INA226 or Sysfs ADC backend before protection can be enabled."
                .to_string(),
        );
    }

    if !matches!(
        payload.source_profile.as_str(),
        "direct_dc" | "solar_battery" | ""
    ) {
        return Err(
            "Off-grid config currently supports source_profile 'direct_dc' or 'solar_battery'."
                .to_string(),
        );
    }

    if payload.freq_step_mhz < 5 {
        return Err("freq_step_mhz must be at least 5 MHz".to_string());
    }

    if payload.min_frequency_mhz < 100 {
        return Err("min_frequency_mhz must be at least 100 MHz".to_string());
    }

    if payload.loop_interval_ms < 500 {
        return Err("loop_interval_ms must be at least 500 ms".to_string());
    }

    let mut thresholds = battery_thresholds_for_preset(&payload.battery_preset);
    if let Some(v) = payload.custom_critical_v {
        thresholds.critical_v = v;
    }
    if let Some(v) = payload.custom_low_v {
        thresholds.low_v = v;
    }
    if let Some(v) = payload.custom_high_v {
        thresholds.high_v = v;
    }
    if let Some(v) = payload.custom_full_v {
        thresholds.full_v = v;
    }
    if let Some(v) = payload.custom_recovery_v {
        thresholds.recovery_v = v;
    }

    if thresholds.critical_v >= thresholds.low_v {
        return Err(format!(
            "critical_v ({:.1}) must be less than low_v ({:.1})",
            thresholds.critical_v, thresholds.low_v
        ));
    }
    if thresholds.low_v >= thresholds.high_v {
        return Err(format!(
            "low_v ({:.1}) must be less than high_v ({:.1})",
            thresholds.low_v, thresholds.high_v
        ));
    }
    if thresholds.high_v >= thresholds.full_v {
        return Err(format!(
            "high_v ({:.1}) must be less than full_v ({:.1})",
            thresholds.high_v, thresholds.full_v
        ));
    }
    if thresholds.recovery_v <= thresholds.critical_v {
        return Err(format!(
            "recovery_v ({:.1}) must be greater than critical_v ({:.1})",
            thresholds.recovery_v, thresholds.critical_v
        ));
    }
    if thresholds.critical_v < 5.0 {
        return Err(format!(
            "critical_v ({:.1}) is dangerously low",
            thresholds.critical_v
        ));
    }

    Ok(())
}

pub(super) fn load_offgrid_config_payload() -> OffGridConfigPayload {
    let table = load_config_table_for_write().unwrap_or_else(|_| toml::Table::new());
    let mut payload = OffGridConfigPayload {
        source_profile: {
            let source = config_string(&table, "power", "source_profile");
            if source.is_empty() {
                default_offgrid_source_profile()
            } else {
                source
            }
        },
        ..OffGridConfigPayload::default()
    };

    if let Some(offgrid) = table
        .get("power")
        .and_then(|value| value.as_table())
        .and_then(|power| power.get("offgrid"))
        .and_then(|value| value.as_table())
    {
        payload.enabled = offgrid
            .get("enabled")
            .and_then(|value| value.as_bool())
            .unwrap_or(false);
        payload.battery_preset = offgrid
            .get("battery_preset")
            .and_then(|value| value.as_str())
            .map(str::to_string)
            .unwrap_or_else(default_offgrid_battery_preset);
        payload.freq_step_mhz = offgrid
            .get("freq_step_mhz")
            .and_then(|value| value.as_integer())
            .and_then(|value| u16::try_from(value).ok())
            .unwrap_or_else(default_offgrid_freq_step);
        payload.min_frequency_mhz = offgrid
            .get("min_frequency_mhz")
            .and_then(|value| value.as_integer())
            .and_then(|value| u16::try_from(value).ok())
            .unwrap_or_else(default_offgrid_min_freq);
        payload.loop_interval_ms = offgrid
            .get("loop_interval_ms")
            .and_then(|value| value.as_integer())
            .and_then(|value| u64::try_from(value).ok())
            .unwrap_or_else(default_offgrid_interval_ms);
        payload.custom_critical_v = offgrid.get("custom_critical_v").and_then(|value| {
            value
                .as_float()
                .map(|v| v as f32)
                .or_else(|| value.as_integer().map(|v| v as f32))
        });
        payload.custom_low_v = offgrid.get("custom_low_v").and_then(|value| {
            value
                .as_float()
                .map(|v| v as f32)
                .or_else(|| value.as_integer().map(|v| v as f32))
        });
        payload.custom_high_v = offgrid.get("custom_high_v").and_then(|value| {
            value
                .as_float()
                .map(|v| v as f32)
                .or_else(|| value.as_integer().map(|v| v as f32))
        });
        payload.custom_full_v = offgrid.get("custom_full_v").and_then(|value| {
            value
                .as_float()
                .map(|v| v as f32)
                .or_else(|| value.as_integer().map(|v| v as f32))
        });
        payload.custom_recovery_v = offgrid.get("custom_recovery_v").and_then(|value| {
            value
                .as_float()
                .map(|v| v as f32)
                .or_else(|| value.as_integer().map(|v| v as f32))
        });

        payload.adc = offgrid
            .get("adc")
            .cloned()
            .and_then(|value| value.try_into().ok());
    }

    payload
}

pub(super) fn offgrid_readiness_message(payload: &OffGridConfigPayload) -> String {
    if payload.adc.as_ref().is_some_and(offgrid_adc_is_simulated) {
        return "Simulated ADC is lab-only and cannot arm off-grid protection. Use Test ADC Path for lab probes, then select INA226 or Sysfs ADC before saving commissioning config.".to_string();
    }

    if !payload.enabled {
        return "Off-grid controller is disabled. Save a configuration, then restart dcentrald when you are ready to commission direct-DC or solar-battery mining.".to_string();
    }

    match payload.adc.as_ref() {
        Some(dcentrald_hal::adc::AdcBackendConfig::Ina226 { .. }) => {
            "Configured for measured DC-bus telemetry via INA226. Restart dcentrald to arm live voltage protection and battery-aware curtailment.".to_string()
        }
        Some(dcentrald_hal::adc::AdcBackendConfig::Sysfs { .. }) => {
            "Configured for Sysfs ADC voltage monitoring. Restart dcentrald to arm direct-DC protection on the selected ADC path.".to_string()
        }
        Some(dcentrald_hal::adc::AdcBackendConfig::Simulated { .. }) => {
            "Configured with a simulated ADC source. This is suitable for lab testing only and should not be used for real battery protection.".to_string()
        }
        None => {
            "Select an ADC backend before enabling off-grid protection. DCENT_OS now refuses to silently fake a healthy battery voltage.".to_string()
        }
    }
}

pub(super) fn offgrid_adc_backend_key(adc: &dcentrald_hal::adc::AdcBackendConfig) -> &'static str {
    match adc {
        dcentrald_hal::adc::AdcBackendConfig::Ina226 { .. } => "ina226",
        dcentrald_hal::adc::AdcBackendConfig::Sysfs { .. } => "sysfs",
        dcentrald_hal::adc::AdcBackendConfig::Simulated { .. } => "simulated",
    }
}

pub(super) fn offgrid_sensor_source_name(
    adc: &dcentrald_hal::adc::AdcBackendConfig,
) -> &'static str {
    match adc {
        dcentrald_hal::adc::AdcBackendConfig::Ina226 { .. } => "INA226",
        dcentrald_hal::adc::AdcBackendConfig::Sysfs { .. } => "Sysfs ADC",
        dcentrald_hal::adc::AdcBackendConfig::Simulated { .. } => "Simulated",
    }
}

pub(super) fn offgrid_config_response(payload: OffGridConfigPayload) -> OffGridConfigResponse {
    let ready = payload.enabled
        && payload
            .adc
            .as_ref()
            .is_some_and(|adc| !offgrid_adc_is_simulated(adc));

    OffGridConfigResponse {
        ready,
        restart_required: true,
        readiness_message: offgrid_readiness_message(&payload),
        config: payload,
    }
}

pub(super) fn persist_offgrid_config(
    payload: &OffGridConfigPayload,
) -> std::result::Result<(), String> {
    // RELIAB-2b: serialize the whole load→modify→write so a concurrent config
    // writer can't interleave and drop this section's change (lost update).
    let _cfg_write_guard = crate::atomic_io::config_write_lock();
    let config_path = get_writable_config_path();
    let mut table = load_config_table_for_write()?;

    if let Some(parent) = std::path::Path::new(config_path).parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create config directory: {}", e))?;
    }

    let power = table
        .entry("power".to_string())
        .or_insert_with(|| toml::Value::Table(toml::Table::new()));
    let power_table = power
        .as_table_mut()
        .ok_or_else(|| "[power] is not a TOML table".to_string())?;

    power_table.insert(
        "source_profile".to_string(),
        toml::Value::String(payload.source_profile.clone()),
    );
    let persisted = toml::Value::try_from(serde_json::json!({
        "enabled": payload.enabled,
        "battery_preset": payload.battery_preset,
        "adc": payload.adc,
        "freq_step_mhz": payload.freq_step_mhz,
        "min_frequency_mhz": payload.min_frequency_mhz,
        "loop_interval_ms": payload.loop_interval_ms,
        "custom_critical_v": payload.custom_critical_v,
        "custom_low_v": payload.custom_low_v,
        "custom_high_v": payload.custom_high_v,
        "custom_full_v": payload.custom_full_v,
        "custom_recovery_v": payload.custom_recovery_v,
    }))
    .map_err(|e| format!("Failed to serialize off-grid config: {}", e))?;
    power_table.insert("offgrid".to_string(), persisted);

    let output =
        toml::to_string_pretty(&table).map_err(|e| format!("Failed to serialize config: {}", e))?;
    atomic_write(config_path, output).map_err(|e| format!("Failed to write config: {}", e))?;
    Ok(())
}

/// GET /api/offgrid/config -- Current off-grid/direct-DC commissioning config.
pub(super) async fn get_offgrid_config() -> impl IntoResponse {
    Json(serde_json::json!(offgrid_config_response(
        load_offgrid_config_payload()
    )))
}

/// POST /api/offgrid/config -- Persist off-grid/direct-DC commissioning config.
pub(super) async fn post_offgrid_config(
    State(state): State<Arc<AppState>>,
    Json(body): Json<OffGridConfigPayload>,
) -> impl IntoResponse {
    // CE-111: dedicated power-policy writer — gate on ConfigRw AND PowerControl
    // (off-grid config feeds the runtime power workflow).
    if let Err(response) = require_antminer_runtime_capability(
        &state,
        RuntimeCapability::ConfigRw,
        "/api/offgrid/config",
    ) {
        return response;
    }
    if let Err(response) = require_antminer_runtime_capability(
        &state,
        RuntimeCapability::PowerControl,
        "/api/offgrid/config",
    ) {
        return response;
    }

    if let Err(message) = validate_offgrid_payload(&body) {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "status": "error",
                "message": message,
            })),
        )
            .into_response();
    }

    if let Err(message) = persist_offgrid_config(&body) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({
                "status": "error",
                "message": message,
            })),
        )
            .into_response();
    }

    if let Err(e) = update_onboarding_state(|state| {
        if matches!(body.source_profile.as_str(), "direct_dc" | "solar_battery") {
            state.power_source = body.source_profile.clone();
            state.steps.circuit_configured = body.enabled && body.adc.is_some();
        }
    }) {
        tracing::warn!(error = %e, "Failed to update onboarding state after off-grid save");
    }

    Json(serde_json::json!({
        "status": "ok",
        "message": "Off-grid configuration saved. Restart dcentrald to activate the new power workflow.",
        "config": offgrid_config_response(body),
    }))
    .into_response()
}

/// POST /api/offgrid/test -- Probe the configured ADC backend without restarting the daemon.
pub(super) async fn post_offgrid_test(Json(body): Json<OffGridConfigPayload>) -> impl IntoResponse {
    match body.adc.as_ref() {
        Some(dcentrald_hal::adc::AdcBackendConfig::Simulated {
            voltage_v,
            current_a,
        }) => {
            let power_w = *voltage_v * *current_a;
            let plausible = voltage_v.is_finite()
                && *voltage_v > 0.0
                && *voltage_v < 500.0
                && current_a.is_finite()
                && power_w.is_finite();
            Json(serde_json::json!({
                "ok": true,
                "backend": "simulated",
                "sensorSource": "Simulated",
                "hasCurrent": *current_a > 0.0,
                "plausible": plausible,
                "voltageV": voltage_v,
                "currentA": current_a,
                "powerW": power_w,
                "hardware_access_attempted": false,
                "message": "Simulated ADC path responded. This confirms lab wiring and UI flow only; do not treat it as field protection.",
            }))
            .into_response()
        }
        Some(_) => (
            StatusCode::NOT_IMPLEMENTED,
            Json(serde_json::json!({
                "ok": false,
                "status": "unavailable",
                "hardware_access_attempted": false,
                "message": "Live ADC tests are unavailable until the runtime owner exposes a serialized telemetry snapshot or command broker.",
            })),
        )
            .into_response(),
        None => Json(serde_json::json!({
            "ok": false,
            "backend": "unconfigured",
            "sensorSource": "Unconfigured",
            "hasCurrent": false,
            "plausible": false,
            "hardware_access_attempted": false,
            "message": "Select an ADC backend before testing the Off-Grid sensor path.",
        }))
        .into_response(),
    }
}

#[cfg(feature = "recovery-tool")]
async fn post_offgrid_test_recovery(Json(body): Json<OffGridConfigPayload>) -> impl IntoResponse {
    let Some(adc) = body.adc.as_ref() else {
        return Json(serde_json::json!({
            "ok": false,
            "backend": "unconfigured",
            "sensorSource": "Unconfigured",
            "hasCurrent": false,
            "plausible": false,
            "message": "Select an ADC backend before testing the Off-Grid sensor path.",
        }))
        .into_response();
    };

    let backend = offgrid_adc_backend_key(adc);
    let fallback_source = offgrid_sensor_source_name(adc);

    let mut source = match dcentrald_hal::adc::create_voltage_source(adc) {
        Ok(source) => source,
        Err(error) => {
            return Json(serde_json::json!({
                "ok": false,
                "backend": backend,
                "sensorSource": fallback_source,
                "hasCurrent": false,
                "plausible": false,
                "message": format!("ADC probe setup failed: {}", error),
            }))
            .into_response();
        }
    };

    let sensor_source = source.source_name().to_string();
    let has_current = source.has_current();
    match source.read() {
        Ok(reading) => {
            let plausible = reading.voltage_v.is_finite()
                && reading.voltage_v > 0.0
                && reading.voltage_v < 500.0
                && reading.current_a.is_finite()
                && reading.power_w.is_finite();
            let message = match adc {
                dcentrald_hal::adc::AdcBackendConfig::Simulated { .. } => {
                    "Simulated ADC path responded. This confirms lab wiring and UI flow only; do not treat it as field protection.".to_string()
                }
                _ if plausible => {
                    format!(
                        "ADC probe succeeded: {:.2} V{}.",
                        reading.voltage_v,
                        if has_current {
                            format!(", {:.2} A / {:.0} W", reading.current_a, reading.power_w)
                        } else {
                            String::from(", voltage-only backend")
                        }
                    )
                }
                _ => {
                    format!(
                        "ADC probe returned implausible values ({:.2} V, {:.2} A, {:.0} W). Verify divider, path, and live wiring before trusting this backend.",
                        reading.voltage_v, reading.current_a, reading.power_w
                    )
                }
            };

            Json(serde_json::json!({
                "ok": true,
                "backend": backend,
                "sensorSource": sensor_source,
                "hasCurrent": has_current,
                "plausible": plausible,
                "voltageV": reading.voltage_v,
                "currentA": reading.current_a,
                "powerW": reading.power_w,
                "message": message,
            }))
            .into_response()
        }
        Err(error) => Json(serde_json::json!({
            "ok": false,
            "backend": backend,
            "sensorSource": sensor_source,
            "hasCurrent": has_current,
            "plausible": false,
            "message": format!("ADC probe read failed: {}", error),
        }))
        .into_response(),
    }
}

/// GET /api/offgrid/status -- Off-grid telemetry (voltage, SoC, zone, power).
pub(super) async fn get_offgrid_status(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    match &state.offgrid_rx {
        Some(rx) => {
            let telemetry = rx.borrow().clone();
            Json(serde_json::json!(telemetry))
        }
        None => {
            let config = load_offgrid_config_payload();
            if config.enabled {
                Json(serde_json::json!({
                    "enabled": true,
                    "zone": "pending_restart",
                    "state": "PendingRestart",
                    "bus_voltage_v": 0.0,
                    "current_a": 0.0,
                    "power_w": 0.0,
                    "battery_soc_pct": 0.0,
                    "target_freq_mhz": config.min_frequency_mhz,
                    "freq_pct": 0.0,
                    "voltage_rate_vps": 0.0,
                    "uptime_battery_s": 0,
                    "energy_consumed_wh": 0.0,
                    "critical_v": config.custom_critical_v.unwrap_or_else(|| battery_thresholds_for_preset(&config.battery_preset).critical_v),
                    "low_v": config.custom_low_v.unwrap_or_else(|| battery_thresholds_for_preset(&config.battery_preset).low_v),
                    "high_v": config.custom_high_v.unwrap_or_else(|| battery_thresholds_for_preset(&config.battery_preset).high_v),
                    "full_v": config.custom_full_v.unwrap_or_else(|| battery_thresholds_for_preset(&config.battery_preset).full_v),
                    "sensor_source": config.adc.as_ref().map(offgrid_sensor_source_name).unwrap_or("Unconfigured"),
                    "has_current": matches!(config.adc, Some(dcentrald_hal::adc::AdcBackendConfig::Ina226 { .. }) | Some(dcentrald_hal::adc::AdcBackendConfig::Simulated { .. })),
                    "sensor_ok": false,
                    "message": "Off-grid config is saved but not active in the running daemon yet. Restart dcentrald to arm the controller.",
                }))
            } else {
                Json(serde_json::json!({
                    "enabled": false,
                    "message": "Off-grid mode is not enabled. Configure it in Energy Tools or via /api/offgrid/config.",
                }))
            }
        }
    }
}

/// GET /api/offgrid/presets -- List available battery chemistry presets.
pub(super) async fn get_offgrid_presets() -> impl IntoResponse {
    let presets: Vec<serde_json::Value> = dcentrald_thermal::battery::BatteryPreset::all_presets()
        .into_iter()
        .map(|(preset, label, thresholds)| {
            serde_json::json!({
                "id": format!("{:?}", preset).to_lowercase(),
                "label": label,
                "critical_v": thresholds.critical_v,
                "low_v": thresholds.low_v,
                "normal_v": thresholds.normal_v,
                "high_v": thresholds.high_v,
                "full_v": thresholds.full_v,
                "recovery_v": thresholds.recovery_v,
            })
        })
        .collect();

    Json(serde_json::json!({ "presets": presets }))
}

// ─── Solar / Hybrid Status Handlers ──────────────────────────────────

pub(super) fn default_solar_inverter_brand() -> String {
    "manual".to_string()
}

pub(super) fn solar_provider_saved(config: &SolarConfigPayload) -> bool {
    if !config.enabled {
        return false;
    }

    if config.inverter_brand.trim().is_empty() {
        return false;
    }

    if config.inverter_brand == "manual" {
        return true;
    }

    !config.api_endpoint.trim().is_empty()
}

pub(super) fn solar_status_pending_restart_payload(
    config: &SolarConfigPayload,
    mining_power: &crate::SolarMiningPowerStatus,
) -> serde_json::Value {
    let mining_watts = mining_power.watts;
    let source_profile = load_config_table_for_write()
        .ok()
        .and_then(|table| {
            table
                .get("power")
                .and_then(|value| value.as_table())
                .and_then(|power| power.get("source_profile"))
                .and_then(|value| value.as_str())
                .map(str::to_string)
        })
        .unwrap_or_else(|| "grid".to_string());
    let support = solar_provider_support(&config.inverter_brand);
    let provider_telemetry_backed = crate::solar_provider_telemetry_backed(&config.inverter_brand);
    serde_json::json!({
        "enabled": config.enabled,
        "provider": config.inverter_brand,
        "providerLiveBackend": support.live_backend,
        "providerTelemetryBacked": provider_telemetry_backed,
        "providerConfigured": solar_provider_saved(config),
        "providerStage": support.stage,
        "providerStageReason": support.stage_reason,
        "recommendedProvider": support.recommended_provider,
        "providerBackendScope": support.backend_scope,
        "acceptedPayloadShapes": support.accepted_payload_shapes,
        "runtimeAdopted": false,
        "commissioningState": "pending_restart",
        "sourceProfile": source_profile,
        "productionWatts": 0,
        "consumptionWatts": mining_watts,
        "miningWatts": mining_watts,
        "miningWattsSource": mining_power.source.as_str(),
        "miningWattsLive": mining_power.live,
        "miningWattsModeled": mining_power.modeled,
        "miningWattsNote": mining_power.note,
        "netGridWatts": mining_watts,
        "solarSurplusWatts": 0,
        "batterySocPct": serde_json::Value::Null,
        "connected": false,
        "transport": shared_solar_transport(&config.inverter_brand, &config.api_endpoint),
        "matchedFields": [],
        "solarOnlyMode": config.solar_only_mode,
        "controlActive": false,
        "sleeping": false,
        "batteryFloorActive": false,
        "targetFreqMhz": serde_json::Value::Null,
        "action": "pending_restart",
        "sampleAgeMs": serde_json::Value::Null,
        "stale": true,
        "consecutiveFailures": 0,
        "lastSuccessMs": serde_json::Value::Null,
        "lastUpdateMs": 0,
        "message": if provider_telemetry_backed {
            "Solar provider config is saved, but the running daemon has not adopted it yet. Restart dcentrald before treating solar policy as active."
        } else {
            "Manual solar provider values are saved, but the running daemon has not adopted them yet. Restart dcentrald before treating manual solar policy as active."
        },
    })
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub(super) struct SolarConfigPayload {
    enabled: bool,
    inverter_brand: String,
    api_endpoint: String,
    api_key: String,
    solar_only_mode: bool,
    base_load_watts: u32,
    battery_threshold_pct: u8,
    battery_wake_hysteresis_pct: u8,
    provider_max_sample_age_ms: u64,
    provider_failure_hysteresis_samples: u8,
    hybrid_import_deadband_watts: u32,
    manual_production_watts: u32,
    manual_site_load_watts: u32,
    manual_battery_soc_pct: Option<f32>,
}

impl Default for SolarConfigPayload {
    fn default() -> Self {
        Self {
            enabled: false,
            inverter_brand: default_solar_inverter_brand(),
            api_endpoint: String::new(),
            api_key: String::new(),
            solar_only_mode: false,
            base_load_watts: 500,
            battery_threshold_pct: 20,
            battery_wake_hysteresis_pct: 3,
            provider_max_sample_age_ms: 60_000,
            provider_failure_hysteresis_samples: 1,
            hybrid_import_deadband_watts: 75,
            manual_production_watts: 0,
            manual_site_load_watts: 0,
            manual_battery_soc_pct: None,
        }
    }
}

pub(super) fn load_solar_config_payload() -> SolarConfigPayload {
    let table = load_config_table_for_write().unwrap_or_else(|_| toml::Table::new());
    let mut payload = SolarConfigPayload::default();

    if let Some(solar) = table
        .get("power")
        .and_then(|value| value.as_table())
        .and_then(|power| power.get("solar"))
        .and_then(|value| value.as_table())
    {
        payload.enabled = solar_table_bool(solar, "enabled", "enabled").unwrap_or(false);
        payload.inverter_brand = solar_table_string(solar, "inverter_brand", "inverterBrand")
            .unwrap_or_else(default_solar_inverter_brand);
        payload.api_endpoint =
            solar_table_string(solar, "api_endpoint", "apiEndpoint").unwrap_or_default();
        payload.api_key = solar_table_string(solar, "api_key", "apiKey").unwrap_or_default();
        payload.solar_only_mode =
            solar_table_bool(solar, "solar_only_mode", "solarOnlyMode").unwrap_or(false);
        payload.base_load_watts =
            solar_table_u32(solar, "base_load_watts", "baseLoadWatts").unwrap_or(500);
        payload.battery_threshold_pct =
            solar_table_u8(solar, "battery_threshold_pct", "batteryThresholdPct").unwrap_or(20);
        payload.battery_wake_hysteresis_pct = solar_table_u8(
            solar,
            "battery_wake_hysteresis_pct",
            "batteryWakeHysteresisPct",
        )
        .unwrap_or(3);
        payload.provider_max_sample_age_ms = solar_table_u64(
            solar,
            "provider_max_sample_age_ms",
            "providerMaxSampleAgeMs",
        )
        .unwrap_or(60_000);
        payload.provider_failure_hysteresis_samples = solar_table_u8(
            solar,
            "provider_failure_hysteresis_samples",
            "providerFailureHysteresisSamples",
        )
        .unwrap_or(1);
        payload.hybrid_import_deadband_watts = solar_table_u32(
            solar,
            "hybrid_import_deadband_watts",
            "hybridImportDeadbandWatts",
        )
        .unwrap_or(75);
        payload.manual_production_watts =
            solar_table_u32(solar, "manual_production_watts", "manualProductionWatts").unwrap_or(0);
        payload.manual_site_load_watts =
            solar_table_u32(solar, "manual_site_load_watts", "manualSiteLoadWatts").unwrap_or(0);
        payload.manual_battery_soc_pct =
            solar_table_f32(solar, "manual_battery_soc_pct", "manualBatterySocPct");
    }

    payload
}

pub(super) fn persist_solar_config(
    payload: &SolarConfigPayload,
) -> std::result::Result<(), String> {
    // RELIAB-2b: serialize the whole load→modify→write so a concurrent config
    // writer can't interleave and drop this section's change (lost update).
    let _cfg_write_guard = crate::atomic_io::config_write_lock();
    let config_path = get_writable_config_path();
    let mut table = load_config_table_for_write()?;

    if let Some(parent) = std::path::Path::new(config_path).parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create config directory: {}", e))?;
    }

    let power = table
        .entry("power".to_string())
        .or_insert_with(|| toml::Value::Table(toml::Table::new()));
    let power_table = power
        .as_table_mut()
        .ok_or_else(|| "[power] is not a TOML table".to_string())?;
    let persisted = toml::Value::try_from(serde_json::json!(payload))
        .map_err(|e| format!("Failed to serialize solar config: {}", e))?;
    power_table.insert("solar".to_string(), persisted);

    let output =
        toml::to_string_pretty(&table).map_err(|e| format!("Failed to serialize config: {}", e))?;
    atomic_write(config_path, output).map_err(|e| format!("Failed to write config: {}", e))?;
    Ok(())
}

pub(super) fn validate_solar_config(
    payload: &SolarConfigPayload,
) -> std::result::Result<(), String> {
    if payload.battery_threshold_pct > 100 {
        return Err("batteryThresholdPct must be between 0 and 100".to_string());
    }

    if payload.battery_wake_hysteresis_pct > 50 {
        return Err("batteryWakeHysteresisPct must be between 0 and 50".to_string());
    }

    if payload.battery_threshold_pct as u16 + payload.battery_wake_hysteresis_pct as u16 > 100 {
        return Err(
            "batteryThresholdPct plus batteryWakeHysteresisPct must stay at or below 100"
                .to_string(),
        );
    }

    if payload.provider_max_sample_age_ms > 300_000 {
        return Err("providerMaxSampleAgeMs must be between 0 and 300000".to_string());
    }

    if payload.provider_failure_hysteresis_samples == 0
        || payload.provider_failure_hysteresis_samples > 10
    {
        return Err("providerFailureHysteresisSamples must be between 1 and 10".to_string());
    }

    if payload.hybrid_import_deadband_watts > 5_000 {
        return Err("hybridImportDeadbandWatts must be between 0 and 5000".to_string());
    }

    let support = solar_provider_support(&payload.inverter_brand);
    if support.stage == "unsupported" {
        return Err(support
            .stage_reason
            .unwrap_or_else(|| "Unsupported inverter brand".to_string()));
    }

    if payload.enabled && !support.live_backend {
        return Err(format!(
            "{} is not a live backend yet. {} Save it disabled for staging, or normalize EcoFlow telemetry through the bridge provider for active enforcement.",
            payload.inverter_brand,
            support
                .stage_reason
                .as_deref()
                .unwrap_or("This provider is staged only.")
        ));
    }

    if payload.inverter_brand != "manual" && payload.api_endpoint.trim().is_empty() {
        return Err("apiEndpoint is required for non-manual solar providers".to_string());
    }

    if payload.inverter_brand == "ecoflow"
        && !(payload.api_endpoint.starts_with("http://")
            || payload.api_endpoint.starts_with("https://")
            || payload.api_endpoint.starts_with("mqtt://")
            || payload.api_endpoint.starts_with("mqtts://")
            || payload.api_endpoint.starts_with("ws://")
            || payload.api_endpoint.starts_with("wss://"))
    {
        return Err(
            "ecoflow requires an HTTP(S) or MQTT/WS endpoint serving one of the supported normalized EcoFlow bridge payload shapes; direct EcoFlow auth/protocol coverage is intentionally out of scope"
                .to_string(),
        );
    }

    Ok(())
}

pub(super) fn solar_table_string(table: &toml::Table, snake: &str, camel: &str) -> Option<String> {
    table
        .get(snake)
        .and_then(|value| value.as_str())
        .or_else(|| table.get(camel).and_then(|value| value.as_str()))
        .map(str::to_string)
}

pub(super) fn solar_table_bool(table: &toml::Table, snake: &str, camel: &str) -> Option<bool> {
    table
        .get(snake)
        .and_then(|value| value.as_bool())
        .or_else(|| table.get(camel).and_then(|value| value.as_bool()))
}

pub(super) fn solar_table_u32(table: &toml::Table, snake: &str, camel: &str) -> Option<u32> {
    table
        .get(snake)
        .and_then(|value| value.as_integer())
        .or_else(|| table.get(camel).and_then(|value| value.as_integer()))
        .and_then(|value| u32::try_from(value).ok())
}

pub(super) fn solar_table_u64(table: &toml::Table, snake: &str, camel: &str) -> Option<u64> {
    table
        .get(snake)
        .and_then(|value| value.as_integer())
        .or_else(|| table.get(camel).and_then(|value| value.as_integer()))
        .and_then(|value| u64::try_from(value).ok())
}

pub(super) fn solar_table_u8(table: &toml::Table, snake: &str, camel: &str) -> Option<u8> {
    table
        .get(snake)
        .and_then(|value| value.as_integer())
        .or_else(|| table.get(camel).and_then(|value| value.as_integer()))
        .and_then(|value| u8::try_from(value).ok())
}

pub(super) fn solar_table_f32(table: &toml::Table, snake: &str, camel: &str) -> Option<f32> {
    table
        .get(snake)
        .or_else(|| table.get(camel))
        .and_then(|value| {
            value
                .as_float()
                .map(|v| v as f32)
                .or_else(|| value.as_integer().map(|v| v as f32))
        })
}

#[derive(Debug, Clone)]
pub(super) struct SolarProviderSnapshot {
    production_watts: u32,
    consumption_watts: u32,
    net_grid_watts: i64,
    battery_soc_pct: Option<f32>,
    connected: bool,
    message: String,
    matched_fields: Vec<String>,
    transport: String,
    sample_age_ms: Option<u64>,
    stale: bool,
}

pub(super) struct EcoFlowPayloadShape {
    label: &'static str,
    production_patterns: &'static [&'static str],
    consumption_patterns: &'static [&'static str],
    grid_patterns: &'static [&'static str],
    battery_soc_patterns: &'static [&'static str],
    age_patterns: &'static [&'static str],
}

pub(super) const BRIDGE_STALE_MS: u64 = 60_000;

pub(super) const ECOFLOW_PAYLOAD_SHAPES: &[EcoFlowPayloadShape] = &[
    EcoFlowPayloadShape {
        label: "bridge-contract",
        production_patterns: &[
            "productionWatts",
            "solarProductionWatts",
            "pvProductionWatts",
            "solar/power",
            "production/power",
        ],
        consumption_patterns: &[
            "consumptionWatts",
            "siteLoadWatts",
            "loadWatts",
            "site/load/power",
            "load/power",
        ],
        grid_patterns: &[
            "netGridWatts",
            "gridPowerWatts",
            "siteImportWatts",
            "grid/power",
            "site/grid/power",
        ],
        battery_soc_patterns: &["batterySocPct", "battery/soc", "soc"],
        age_patterns: &[
            "timestampMs",
            "timestamp_ms",
            "sampleAgeMs",
            "sample_age_ms",
        ],
    },
    EcoFlowPayloadShape {
        label: "site-summary",
        production_patterns: &["pvWatts", "solarWatts", "pvPowerWatts"],
        consumption_patterns: &[
            "homeLoadWatts",
            "loadPowerWatts",
            "consumptionWatts",
            "outputWatts",
        ],
        grid_patterns: &["gridExchangeWatts", "netGridWatts", "gridPowerWatts"],
        battery_soc_patterns: &["batteryPercent", "batterySocPct", "batterySoc", "soc"],
        age_patterns: &["updatedAtMs", "lastUpdatedMs", "sampleAgeMs", "timestampMs"],
    },
    EcoFlowPayloadShape {
        label: "power-summary",
        production_patterns: &["solarInputWatts", "inputPowerWatts", "solarWatts"],
        consumption_patterns: &["outputWatts", "homeLoadWatts", "consumptionWatts"],
        grid_patterns: &["netGridWatts", "gridExchangeWatts", "gridPowerWatts"],
        battery_soc_patterns: &["batteryLevelPct", "batteryPercent", "batterySocPct", "soc"],
        age_patterns: &[
            "telemetryTimestampMs",
            "updatedAtMs",
            "lastUpdatedMs",
            "sampleAgeMs",
        ],
    },
];

pub(super) fn normalize_metric_key(key: &str) -> String {
    key.to_ascii_lowercase()
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric() || *ch == '/')
        .collect()
}

pub(super) fn extract_numeric_value(value: &serde_json::Value) -> Option<f64> {
    match value {
        serde_json::Value::Number(number) => number.as_f64(),
        serde_json::Value::Object(map) => map.get("value").and_then(extract_numeric_value),
        _ => None,
    }
}

pub(super) fn extract_string_value(value: &serde_json::Value) -> Option<&str> {
    match value {
        serde_json::Value::String(text) => Some(text.as_str()),
        serde_json::Value::Object(map) => map.get("value").and_then(extract_string_value),
        _ => None,
    }
}

pub(super) fn object_number_field(value: &serde_json::Value, key: &str) -> Option<f64> {
    value.get(key).and_then(extract_numeric_value)
}

pub(super) fn object_string_field<'a>(value: &'a serde_json::Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(extract_string_value)
}

pub(super) fn append_query_param(endpoint: &str, key: &str, value: &str) -> String {
    if value.trim().is_empty() || endpoint.contains(&format!("{}=", key)) {
        return endpoint.to_string();
    }

    let separator = if endpoint.contains('?') { '&' } else { '?' };
    format!("{}{}{}={}", endpoint, separator, key, value)
}

pub(super) fn array_item_by_measurement<'a>(
    value: &'a serde_json::Value,
    field: &str,
    measurement_type: &str,
) -> Option<&'a serde_json::Value> {
    value.get(field)?.as_array()?.iter().find(|entry| {
        object_string_field(entry, "measurementType") == Some(measurement_type)
            && object_number_field(entry, "activeCount").unwrap_or(1.0) > 0.0
    })
}

pub(super) fn kw_to_watts(value: f64, unit: &str) -> f64 {
    if unit.eq_ignore_ascii_case("kw") {
        value * 1000.0
    } else {
        value
    }
}

pub(super) fn collect_numeric_metrics(
    prefix: &str,
    value: &serde_json::Value,
    metrics: &mut std::collections::BTreeMap<String, f64>,
) {
    if !prefix.is_empty() {
        if let Some(number) = extract_numeric_value(value) {
            metrics.insert(prefix.to_string(), number);
        }
    }

    match value {
        serde_json::Value::Object(map) => {
            for (key, child) in map {
                let next = if prefix.is_empty() {
                    key.clone()
                } else {
                    format!("{}/{}", prefix, key)
                };
                collect_numeric_metrics(&next, child, metrics);
            }
        }
        serde_json::Value::Array(items) => {
            for (idx, child) in items.iter().enumerate() {
                let next = if prefix.is_empty() {
                    idx.to_string()
                } else {
                    format!("{}/{}", prefix, idx)
                };
                collect_numeric_metrics(&next, child, metrics);
            }
        }
        _ => {}
    }
}

pub(super) fn lookup_metric(
    metrics: &std::collections::BTreeMap<String, f64>,
    patterns: &[&str],
) -> Option<(f64, String)> {
    let normalized_metrics: Vec<(String, String, f64)> = metrics
        .iter()
        .map(|(key, value)| (key.clone(), normalize_metric_key(key), *value))
        .collect();

    for pattern in patterns {
        let normalized_pattern = normalize_metric_key(pattern);
        if let Some((original_key, _, value)) =
            normalized_metrics.iter().find(|(_, normalized_key, _)| {
                normalized_key == &normalized_pattern
                    || normalized_key.ends_with(&normalized_pattern)
                    || normalized_key.contains(&normalized_pattern)
            })
        {
            return Some((*value, original_key.clone()));
        }
    }

    None
}

pub(super) fn sum_metrics(
    metrics: &std::collections::BTreeMap<String, f64>,
    patterns: &[&str],
) -> Option<(f64, Vec<String>)> {
    let mut total = 0.0;
    let mut matched = Vec::new();

    for pattern in patterns {
        if let Some((value, key)) = lookup_metric(metrics, &[*pattern]) {
            if !matched.iter().any(|existing| existing == &key) {
                total += value;
                matched.push(key);
            }
        }
    }

    if matched.is_empty() {
        None
    } else {
        Some((total, matched))
    }
}

pub(super) fn parse_mqtt_endpoint(endpoint: &str) -> std::result::Result<(String, u16), String> {
    let trimmed = endpoint.trim().trim_start_matches("mqtt://");
    let host_port = trimmed.split('/').next().unwrap_or(trimmed);
    let mut parts = host_port.split(':');
    let host = parts.next().unwrap_or("").trim();
    if host.is_empty() {
        return Err("Victron MQTT endpoint must include a hostname".to_string());
    }
    let port = parts
        .next()
        .map(|part| {
            part.parse::<u16>()
                .map_err(|_| format!("Invalid MQTT port: {}", part))
        })
        .transpose()?
        .unwrap_or(1883);
    Ok((host.to_string(), port))
}

pub(super) fn parse_mqtt_endpoint_with_topic(
    endpoint: &str,
) -> std::result::Result<(String, u16, String), String> {
    let trimmed = endpoint.trim().trim_start_matches("mqtt://");
    let mut parts = trimmed.splitn(2, '/');
    let host_port = parts.next().unwrap_or("").trim();
    let topic = parts.next().unwrap_or("").trim().trim_matches('/');
    let (host, port) = parse_mqtt_endpoint(host_port)?;
    if topic.is_empty() {
        return Err(
            "Bridge MQTT endpoint must include a topic path, for example mqtt://broker:1883/dcentos/solar"
                .to_string(),
        );
    }
    Ok((host, port, topic.to_string()))
}

pub(super) fn solar_transport(provider: &str, endpoint: &str) -> String {
    shared_solar_transport(provider, endpoint)
}

pub(super) fn apply_http_auth(
    request: reqwest::RequestBuilder,
    api_key: &str,
    prefer_cookie: bool,
) -> reqwest::RequestBuilder {
    let trimmed = api_key.trim();
    if trimmed.is_empty() {
        return request;
    }

    let mut request = request;
    if trimmed.contains('=') {
        if let Ok(cookie) = HeaderValue::from_str(trimmed) {
            request = request.header(COOKIE, cookie);
        }
    } else {
        request = request
            .header("Authorization", format!("Bearer {}", trimmed))
            .header("X-Api-Key", trimmed);

        if prefer_cookie {
            let auth_cookie = format!("AuthCookie={}", trimmed);
            if let Ok(cookie) = HeaderValue::from_str(&auth_cookie) {
                request = request.header(COOKIE, cookie);
            }
        }
    }

    request
}

pub(super) async fn fetch_http_json(
    endpoint: &str,
    api_key: &str,
    accept_invalid_certs: bool,
    provider_label: &str,
    prefer_cookie: bool,
) -> std::result::Result<serde_json::Value, String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(4))
        .danger_accept_invalid_certs(accept_invalid_certs)
        .build()
        .map_err(|e| format!("Failed to create HTTP client: {}", e))?;
    let response = apply_http_auth(client.get(endpoint), api_key, prefer_cookie)
        .send()
        .await
        .map_err(|e| format!("{} HTTP request failed: {}", provider_label, e))?;
    if !response.status().is_success() {
        if response.status().as_u16() == 401 || response.status().as_u16() == 403 {
            return Err(format!(
                "{} HTTP endpoint rejected the request with status {}. Authentication is missing or invalid.",
                provider_label,
                response.status()
            ));
        }

        return Err(format!(
            "{} HTTP endpoint returned status {}",
            provider_label,
            response.status()
        ));
    }

    response
        .json::<serde_json::Value>()
        .await
        .map_err(|e| format!("{} HTTP response was not valid JSON: {}", provider_label, e))
}

pub(super) fn normalize_bridge_metrics(
    metrics: &std::collections::BTreeMap<String, f64>,
    mining_watts: u32,
    base_load_watts: u32,
    transport: &str,
) -> std::result::Result<SolarProviderSnapshot, String> {
    let mut matched_fields = Vec::new();

    let production = lookup_metric(
        metrics,
        &[
            "productionWatts",
            "solarProductionWatts",
            "pvProductionWatts",
            "solar/power",
            "production/power",
        ],
    );
    let consumption = lookup_metric(
        metrics,
        &[
            "consumptionWatts",
            "siteLoadWatts",
            "loadWatts",
            "site/load/power",
            "load/power",
        ],
    );
    let grid = lookup_metric(
        metrics,
        &[
            "netGridWatts",
            "gridPowerWatts",
            "siteImportWatts",
            "grid/power",
            "site/grid/power",
        ],
    );
    let battery_soc = lookup_metric(metrics, &["batterySocPct", "battery/soc", "soc"]);

    if let Some((_, key)) = &production {
        matched_fields.push(key.clone());
    }
    if let Some((_, key)) = &consumption {
        matched_fields.push(key.clone());
    }
    if let Some((_, key)) = &grid {
        matched_fields.push(key.clone());
    }
    if let Some((_, key)) = &battery_soc {
        matched_fields.push(key.clone());
    }

    let Some((production_value, production_key)) = production else {
        return Err(
            "Bridge provider did not expose a normalized productionWatts field".to_string(),
        );
    };

    let consumption_value = if let Some((value, _)) = consumption {
        value
    } else if let Some((grid_value, _)) = grid {
        production_value + grid_value
    } else {
        return Err(
            "Bridge provider must expose consumptionWatts or netGridWatts alongside productionWatts"
                .to_string(),
        );
    };

    let net_grid_value = if let Some((value, _)) = grid {
        value
    } else {
        consumption_value - production_value
    };

    Ok(SolarProviderSnapshot {
        production_watts: production_value.max(0.0).round() as u32,
        consumption_watts: consumption_value
            .max(base_load_watts.saturating_add(mining_watts) as f64)
            .round() as u32,
        net_grid_watts: net_grid_value.round() as i64,
        battery_soc_pct: battery_soc.map(|(value, _)| value as f32),
        connected: true,
        message: format!(
            "Bridge {} provider connected. Fields matched: {}, using {} as normalized production.",
            transport,
            matched_fields.join(", "),
            production_key
        ),
        matched_fields,
        transport: transport.to_string(),
        sample_age_ms: sample_age_from_metrics(metrics),
        stale: sample_age_from_metrics(metrics)
            .map(|age| age > BRIDGE_STALE_MS)
            .unwrap_or(false),
    })
}

pub(super) fn sample_age_from_metrics(
    metrics: &std::collections::BTreeMap<String, f64>,
) -> Option<u64> {
    sample_age_from_patterns(
        metrics,
        &[
            "timestampMs",
            "timestamp_ms",
            "sampleAgeMs",
            "sample_age_ms",
        ],
    )
}

pub(super) fn sample_age_from_patterns(
    metrics: &std::collections::BTreeMap<String, f64>,
    patterns: &[&str],
) -> Option<u64> {
    lookup_metric(metrics, patterns).and_then(|(value, _)| {
        let now = unix_time_ms();
        let ts = value.round() as i128;
        if ts > 1_000_000_000_000_i128 && ts <= now as i128 {
            Some(now.saturating_sub(ts as u64))
        } else if ts >= 0 {
            Some(ts as u64)
        } else {
            None
        }
    })
}

pub(super) fn normalize_ecoflow_shape(
    metrics: &std::collections::BTreeMap<String, f64>,
    mining_watts: u32,
    base_load_watts: u32,
    shape: &EcoFlowPayloadShape,
) -> Option<SolarProviderSnapshot> {
    let production = lookup_metric(metrics, shape.production_patterns)?;
    let consumption = lookup_metric(metrics, shape.consumption_patterns);
    let grid = lookup_metric(metrics, shape.grid_patterns);
    let battery_soc = lookup_metric(metrics, shape.battery_soc_patterns);

    let consumption_value = if let Some((value, _)) = consumption {
        value
    } else if let Some((grid_value, _)) = grid {
        production.0 + grid_value
    } else {
        return None;
    };
    let net_grid_value = grid
        .as_ref()
        .map(|(value, _)| *value)
        .unwrap_or_else(|| consumption_value - production.0);

    let mut matched_fields = vec![production.1.clone()];
    if let Some((_, key)) = &consumption {
        matched_fields.push(key.clone());
    }
    if let Some((_, key)) = &grid {
        if !matched_fields.iter().any(|existing| existing == key) {
            matched_fields.push(key.clone());
        }
    }
    if let Some((_, key)) = &battery_soc {
        if !matched_fields.iter().any(|existing| existing == key) {
            matched_fields.push(key.clone());
        }
    }

    let sample_age_ms = sample_age_from_patterns(metrics, shape.age_patterns)
        .or_else(|| sample_age_from_metrics(metrics));

    Some(SolarProviderSnapshot {
        production_watts: production.0.max(0.0).round() as u32,
        consumption_watts: consumption_value
            .max(base_load_watts.saturating_add(mining_watts) as f64)
            .round() as u32,
        net_grid_watts: net_grid_value.round() as i64,
        battery_soc_pct: battery_soc.map(|(value, _)| value as f32),
        connected: true,
        message: format!(
            "EcoFlow provider connected via normalized HTTP payload shape '{}'. DCENT_OS is using normalized telemetry, not direct EcoFlow auth/protocol. Fields matched: {}.",
            shape.label,
            matched_fields.join(", ")
        ),
        matched_fields,
        transport: "ecoflow-http-bridge".to_string(),
        sample_age_ms,
        stale: sample_age_ms
            .map(|age| age > BRIDGE_STALE_MS)
            .unwrap_or(false),
    })
}

pub(super) fn read_tesla_metric(value: &serde_json::Value, path: &[&str]) -> Option<f64> {
    let mut current = value;
    for key in path {
        current = current.get(*key)?;
    }
    current.as_f64()
}

pub(super) fn normalize_tesla_snapshot(
    aggregates: &serde_json::Value,
    soe: Option<&serde_json::Value>,
) -> std::result::Result<SolarProviderSnapshot, String> {
    let production = read_tesla_metric(aggregates, &["solar", "instant_power"]);
    let consumption = read_tesla_metric(aggregates, &["load", "instant_power"]);
    let net_grid = read_tesla_metric(aggregates, &["site", "instant_power"]);
    let battery_soc = soe
        .and_then(|value| value.get("percentage"))
        .and_then(|value| value.as_f64())
        .map(|value| value as f32);

    let (Some(production), Some(consumption), Some(net_grid)) = (production, consumption, net_grid)
    else {
        return Err(
            "Tesla local API response is missing one or more required fields: solar.instant_power, load.instant_power, site.instant_power"
                .to_string(),
        );
    };

    let mut matched_fields = vec![
        "solar.instant_power".to_string(),
        "load.instant_power".to_string(),
        "site.instant_power".to_string(),
    ];
    if battery_soc.is_some() {
        matched_fields.push("percentage".to_string());
    }

    Ok(SolarProviderSnapshot {
        production_watts: production.max(0.0).round() as u32,
        consumption_watts: consumption.max(0.0).round() as u32,
        net_grid_watts: net_grid.round() as i64,
        battery_soc_pct: battery_soc,
        connected: true,
        message: if battery_soc.is_some() {
            "Tesla local provider connected via Powerwall/Gateway API.".to_string()
        } else {
            "Tesla local provider connected, but battery SoC was unavailable from /api/system_status/soe.".to_string()
        },
        matched_fields,
        transport: "http-json".to_string(),
        sample_age_ms: Some(0),
        stale: false,
    })
}

pub(super) fn normalize_enphase_snapshot(
    production_details: &serde_json::Value,
    secctrl: Option<&serde_json::Value>,
) -> std::result::Result<SolarProviderSnapshot, String> {
    let production_meter =
        array_item_by_measurement(production_details, "production", "production");
    let inverter_meter = production_details
        .get("production")
        .and_then(|value| value.as_array())
        .and_then(|items| {
            items.iter().find(|entry| {
                object_string_field(entry, "type") == Some("inverters")
                    && object_number_field(entry, "activeCount").unwrap_or(1.0) > 0.0
            })
        });
    let total_consumption =
        array_item_by_measurement(production_details, "consumption", "total-consumption");
    let net_consumption =
        array_item_by_measurement(production_details, "consumption", "net-consumption");

    let production = production_meter
        .and_then(|entry| object_number_field(entry, "wNow"))
        .or_else(|| inverter_meter.and_then(|entry| object_number_field(entry, "wNow")));
    let consumption = total_consumption.and_then(|entry| object_number_field(entry, "wNow"));
    let net_grid = net_consumption.and_then(|entry| object_number_field(entry, "wNow"));
    let battery_soc = secctrl
        .and_then(|value| object_number_field(value, "agg_soc"))
        .or_else(|| secctrl.and_then(|value| object_number_field(value, "ENC_agg_soc")))
        .or_else(|| {
            production_details
                .get("storage")
                .and_then(|value| value.as_array())
                .and_then(|items| items.first())
                .and_then(|entry| object_number_field(entry, "percentFull"))
        })
        .map(|value| value as f32);

    let Some(production) = production else {
        return Err(
            "Enphase local response is missing solar production telemetry (production[].wNow or inverters[].wNow)"
                .to_string(),
        );
    };
    let consumption = if let Some(value) = consumption {
        value
    } else if let Some(value) = net_grid {
        production + value
    } else {
        return Err(
            "Enphase local response is missing both total-consumption and net-consumption telemetry"
                .to_string(),
        );
    };
    let net_grid = net_grid.unwrap_or(consumption - production);

    let mut matched_fields = vec!["production.wNow".to_string()];
    if total_consumption.is_some() {
        matched_fields.push("consumption.total-consumption.wNow".to_string());
    }
    if net_consumption.is_some() {
        matched_fields.push("consumption.net-consumption.wNow".to_string());
    }
    if battery_soc.is_some() {
        matched_fields.push("ensemble.agg_soc".to_string());
    }

    Ok(SolarProviderSnapshot {
        production_watts: production.max(0.0).round() as u32,
        consumption_watts: consumption.max(0.0).round() as u32,
        net_grid_watts: net_grid.round() as i64,
        battery_soc_pct: battery_soc,
        connected: true,
        message: if battery_soc.is_some() {
            "Enphase local provider connected via Envoy / IQ Gateway.".to_string()
        } else {
            "Enphase local provider connected, but battery SoC was unavailable from the local gateway.".to_string()
        },
        matched_fields,
        transport: "http-json".to_string(),
        sample_age_ms: Some(0),
        stale: false,
    })
}

pub(super) fn normalize_solaredge_snapshot(
    current_power_flow: &serde_json::Value,
    overview: Option<&serde_json::Value>,
) -> std::result::Result<SolarProviderSnapshot, String> {
    let flow = current_power_flow
        .get("siteCurrentPowerFlow")
        .ok_or_else(|| "SolarEdge response is missing siteCurrentPowerFlow".to_string())?;
    let unit = object_string_field(flow, "unit").unwrap_or("W");
    let production = flow
        .get("PV")
        .and_then(|value| object_number_field(value, "currentPower"))
        .or_else(|| {
            overview
                .and_then(|value| value.get("overview"))
                .and_then(|value| value.get("currentPower"))
                .and_then(|value| object_number_field(value, "power"))
        });
    let consumption = flow
        .get("LOAD")
        .and_then(|value| object_number_field(value, "currentPower"));
    let grid_mag = flow
        .get("GRID")
        .and_then(|value| object_number_field(value, "currentPower"));
    let battery_soc = flow
        .get("STORAGE")
        .and_then(|value| object_number_field(value, "chargeLevel"))
        .map(|value| value as f32);

    let Some(production) = production else {
        return Err(
            "SolarEdge response is missing PV currentPower and overview fallback".to_string(),
        );
    };
    let Some(consumption) = consumption else {
        return Err("SolarEdge response is missing LOAD currentPower".to_string());
    };

    let importing = flow
        .get("connections")
        .and_then(|value| value.as_array())
        .map(|connections| {
            connections
                .iter()
                .any(|entry| object_string_field(entry, "from") == Some("GRID"))
        })
        .unwrap_or(false);
    let exporting = flow
        .get("connections")
        .and_then(|value| value.as_array())
        .map(|connections| {
            connections
                .iter()
                .any(|entry| object_string_field(entry, "to") == Some("GRID"))
        })
        .unwrap_or(false);
    let grid_mag = grid_mag.unwrap_or_else(|| (consumption - production).abs());
    let signed_grid = if importing && !exporting {
        grid_mag
    } else if exporting && !importing {
        -grid_mag
    } else {
        consumption - production
    };

    let mut matched_fields = vec![
        "siteCurrentPowerFlow.PV.currentPower".to_string(),
        "siteCurrentPowerFlow.LOAD.currentPower".to_string(),
        "siteCurrentPowerFlow.GRID.currentPower".to_string(),
    ];
    if battery_soc.is_some() {
        matched_fields.push("siteCurrentPowerFlow.STORAGE.chargeLevel".to_string());
    }

    Ok(SolarProviderSnapshot {
        production_watts: kw_to_watts(production, unit).max(0.0).round() as u32,
        consumption_watts: kw_to_watts(consumption, unit).max(0.0).round() as u32,
        net_grid_watts: kw_to_watts(signed_grid, unit).round() as i64,
        battery_soc_pct: battery_soc,
        connected: true,
        message: if battery_soc.is_some() {
            "SolarEdge cloud provider connected via currentPowerFlow.".to_string()
        } else {
            "SolarEdge cloud provider connected. Battery SoC is unavailable from currentPowerFlow for this site.".to_string()
        },
        matched_fields,
        transport: "cloud-http".to_string(),
        sample_age_ms: Some(0),
        stale: false,
    })
}

pub(super) fn normalize_victron_metrics(
    metrics: &std::collections::BTreeMap<String, f64>,
    mining_watts: u32,
    base_load_watts: u32,
    transport: &str,
) -> std::result::Result<SolarProviderSnapshot, String> {
    let mut matched_fields = Vec::new();

    let production_patterns = [
        "/Ac/PvOnGrid/L1/Power",
        "/Ac/PvOnGrid/L2/Power",
        "/Ac/PvOnGrid/L3/Power",
        "/Ac/PvOnOutput/L1/Power",
        "/Ac/PvOnOutput/L2/Power",
        "/Ac/PvOnOutput/L3/Power",
        "/Dc/Pv/Power",
    ];
    let fallback_production_patterns = [
        "productionWatts",
        "solarProductionWatts",
        "pv/power",
        "solarpower",
    ];
    let consumption_patterns = [
        "/Ac/ConsumptionOnInput/L1/Power",
        "/Ac/ConsumptionOnInput/L2/Power",
        "/Ac/ConsumptionOnInput/L3/Power",
        "/Ac/ConsumptionOnOutput/L1/Power",
        "/Ac/ConsumptionOnOutput/L2/Power",
        "/Ac/ConsumptionOnOutput/L3/Power",
        "/Ac/Consumption/L1/Power",
        "/Ac/Consumption/L2/Power",
        "/Ac/Consumption/L3/Power",
    ];
    let fallback_consumption_patterns = ["consumptionWatts", "siteLoadWatts", "load/power"];
    let grid_patterns = [
        "/Ac/Grid/L1/Power",
        "/Ac/Grid/L2/Power",
        "/Ac/Grid/L3/Power",
    ];
    let battery_soc_patterns = ["/Dc/Battery/Soc", "/Soc", "batterySocPct", "batterysoc"];

    let production = sum_metrics(metrics, &production_patterns).or_else(|| {
        lookup_metric(metrics, &fallback_production_patterns).map(|(value, key)| (value, vec![key]))
    });
    let consumption = sum_metrics(metrics, &consumption_patterns).or_else(|| {
        lookup_metric(metrics, &fallback_consumption_patterns)
            .map(|(value, key)| (value, vec![key]))
    });
    let grid = sum_metrics(metrics, &grid_patterns).or_else(|| {
        lookup_metric(metrics, &["netGridWatts", "gridPowerWatts", "grid/power"])
            .map(|(value, key)| (value, vec![key]))
    });
    let battery_soc = lookup_metric(metrics, &battery_soc_patterns);

    if let Some((_, keys)) = &production {
        matched_fields.extend(keys.clone());
    }
    if let Some((_, keys)) = &consumption {
        matched_fields.extend(keys.clone());
    }
    if let Some((_, keys)) = &grid {
        matched_fields.extend(keys.clone());
    }
    if let Some((_, key)) = &battery_soc {
        matched_fields.push(key.clone());
    }

    if production.is_none() && consumption.is_none() && battery_soc.is_none() {
        return Err(
            "Victron provider connected but no recognizable production/load/battery metrics were found. Use a GX JSON bridge exposing Victron D-Bus keys or enable LAN MQTT on the GX device."
                .to_string(),
        );
    }

    let production_watts = production
        .map(|(value, _)| value.max(0.0).round() as u32)
        .unwrap_or(0);
    let consumption_watts = consumption
        .map(|(value, _)| value.max(0.0).round() as u32)
        .unwrap_or_else(|| base_load_watts.saturating_add(mining_watts));
    let net_grid_watts = grid
        .map(|(value, _)| value.round() as i64)
        .unwrap_or_else(|| consumption_watts as i64 - production_watts as i64);

    Ok(SolarProviderSnapshot {
        production_watts,
        consumption_watts,
        net_grid_watts,
        battery_soc_pct: battery_soc.map(|(value, _)| value as f32),
        connected: true,
        message: format!(
            "Victron {} provider connected. Fields matched: {}.",
            transport,
            if matched_fields.is_empty() {
                "none".to_string()
            } else {
                matched_fields.join(", ")
            }
        ),
        matched_fields,
        transport: transport.to_string(),
        sample_age_ms: sample_age_from_metrics(metrics),
        stale: sample_age_from_metrics(metrics)
            .map(|age| age > BRIDGE_STALE_MS)
            .unwrap_or(false),
    })
}

pub(super) async fn fetch_victron_http_metrics(
    endpoint: &str,
    api_key: &str,
) -> std::result::Result<std::collections::BTreeMap<String, f64>, String> {
    let json = fetch_http_json(endpoint, api_key, false, "Victron", false).await?;
    let mut metrics = std::collections::BTreeMap::new();
    collect_numeric_metrics("", &json, &mut metrics);
    Ok(metrics)
}

pub(super) async fn fetch_bridge_http_metrics(
    endpoint: &str,
    api_key: &str,
) -> std::result::Result<std::collections::BTreeMap<String, f64>, String> {
    let json = fetch_http_json(endpoint, api_key, false, "Bridge", false).await?;
    let mut metrics = std::collections::BTreeMap::new();
    collect_numeric_metrics("", &json, &mut metrics);
    Ok(metrics)
}

pub(super) async fn fetch_victron_mqtt_metrics(
    endpoint: &str,
    api_key: &str,
) -> std::result::Result<std::collections::BTreeMap<String, f64>, String> {
    let (host, port) = parse_mqtt_endpoint(endpoint)?;
    let client_id = format!("dcentos-solar-{}", unix_time_ms());
    let mut mqtt_options = rumqttc::MqttOptions::new(client_id, host, port);
    mqtt_options.set_keep_alive(std::time::Duration::from_secs(5));
    if !api_key.trim().is_empty() {
        mqtt_options.set_credentials("dcentos", api_key.trim());
    }

    let (client, mut eventloop) = rumqttc::AsyncClient::new(mqtt_options, 32);
    client
        .subscribe("N/+/system/0/#", rumqttc::QoS::AtMostOnce)
        .await
        .map_err(|e| format!("Victron MQTT subscribe failed: {}", e))?;

    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
    let mut metrics = std::collections::BTreeMap::new();

    while tokio::time::Instant::now() < deadline {
        let timeout = deadline
            .saturating_duration_since(tokio::time::Instant::now())
            .min(std::time::Duration::from_millis(500));
        match tokio::time::timeout(timeout, eventloop.poll()).await {
            Ok(Ok(rumqttc::Event::Incoming(rumqttc::Packet::Publish(publish)))) => {
                let topic = publish.topic;
                let Some(system_index) = topic.find("/system/0/") else {
                    continue;
                };
                let key = &topic[system_index + "/system/0/".len()..];
                if key.is_empty() {
                    continue;
                }
                if let Ok(json) = serde_json::from_slice::<serde_json::Value>(&publish.payload) {
                    collect_numeric_metrics(key, &json, &mut metrics);
                }
            }
            Ok(Ok(_)) => {}
            Ok(Err(e)) => return Err(format!("Victron MQTT polling failed: {}", e)),
            Err(_) => break,
        }
    }

    if metrics.is_empty() {
        Err(
            "No Victron MQTT metrics were received. Enable GX LAN MQTT and verify retained system/0 topics are available."
                .to_string(),
        )
    } else {
        Ok(metrics)
    }
}

pub(super) async fn fetch_bridge_mqtt_metrics(
    endpoint: &str,
    api_key: &str,
) -> std::result::Result<std::collections::BTreeMap<String, f64>, String> {
    let (host, port, topic) = parse_mqtt_endpoint_with_topic(endpoint)?;
    let client_id = format!("dcentos-solar-{}", unix_time_ms());
    let mut mqtt_options = rumqttc::MqttOptions::new(client_id, host, port);
    mqtt_options.set_keep_alive(std::time::Duration::from_secs(5));
    if !api_key.trim().is_empty() {
        mqtt_options.set_credentials("dcentos", api_key.trim());
    }

    let (client, mut eventloop) = rumqttc::AsyncClient::new(mqtt_options, 32);
    client
        .subscribe(topic.clone(), rumqttc::QoS::AtMostOnce)
        .await
        .map_err(|e| format!("Bridge MQTT subscribe failed: {}", e))?;

    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
    let mut metrics = std::collections::BTreeMap::new();

    while tokio::time::Instant::now() < deadline {
        let timeout = deadline
            .saturating_duration_since(tokio::time::Instant::now())
            .min(std::time::Duration::from_millis(500));
        match tokio::time::timeout(timeout, eventloop.poll()).await {
            Ok(Ok(rumqttc::Event::Incoming(rumqttc::Packet::Publish(publish)))) => {
                if publish.topic != topic {
                    continue;
                }
                let json =
                    serde_json::from_slice::<serde_json::Value>(&publish.payload).map_err(|e| {
                        format!(
                            "Bridge MQTT payload on '{}' was not valid JSON: {}",
                            topic, e
                        )
                    })?;
                collect_numeric_metrics("", &json, &mut metrics);
                break;
            }
            Ok(Ok(_)) => {}
            Ok(Err(e)) => return Err(format!("Bridge MQTT polling failed: {}", e)),
            Err(_) => break,
        }
    }

    if metrics.is_empty() {
        Err(format!(
            "No bridge MQTT metrics were received on topic '{}'",
            topic
        ))
    } else {
        Ok(metrics)
    }
}

pub(super) async fn fetch_enphase_snapshot(
    config: &SolarConfigPayload,
) -> std::result::Result<SolarProviderSnapshot, String> {
    let base = config.api_endpoint.trim().trim_end_matches('/');
    let production_url = if base.contains("production.json") {
        if base.contains("details=") {
            base.to_string()
        } else {
            format!("{}?details=1", base)
        }
    } else {
        format!("{}/production.json?details=1", base)
    };
    let secctrl_base = if base.contains("production.json") {
        base.split("/production.json").next().unwrap_or(base)
    } else {
        base
    };
    let secctrl_url = format!("{}/ivp/ensemble/secctrl", secctrl_base);

    let production = fetch_http_json(
        &production_url,
        &config.api_key,
        true,
        "Enphase local",
        false,
    )
    .await?;
    let secctrl =
        match fetch_http_json(&secctrl_url, &config.api_key, true, "Enphase local", false).await {
            Ok(value) => Some(value),
            Err(message) => {
                if message.contains("Authentication is missing or invalid")
                    || message.contains("returned status 404")
                {
                    None
                } else {
                    return Err(message);
                }
            }
        };

    normalize_enphase_snapshot(&production, secctrl.as_ref())
}

pub(super) async fn fetch_solaredge_snapshot(
    config: &SolarConfigPayload,
) -> std::result::Result<SolarProviderSnapshot, String> {
    let current_power_flow_url = if config.api_endpoint.contains("currentPowerFlow") {
        config.api_endpoint.clone()
    } else {
        format!(
            "{}/currentPowerFlow",
            config.api_endpoint.trim_end_matches('/')
        )
    };
    let overview_url = current_power_flow_url.replace("currentPowerFlow", "overview");
    let current_power_flow_url =
        append_query_param(&current_power_flow_url, "api_key", &config.api_key);
    let overview_url = append_query_param(&overview_url, "api_key", &config.api_key);

    let current_power_flow =
        fetch_http_json(&current_power_flow_url, "", false, "SolarEdge cloud", false).await?;
    let overview = fetch_http_json(&overview_url, "", false, "SolarEdge cloud", false)
        .await
        .ok();

    normalize_solaredge_snapshot(&current_power_flow, overview.as_ref())
}

pub(super) async fn fetch_bridge_snapshot(
    config: &SolarConfigPayload,
    mining_watts: u32,
) -> std::result::Result<SolarProviderSnapshot, String> {
    let endpoint = config.api_endpoint.trim();
    if endpoint.is_empty() {
        return Err("Bridge endpoint is empty.".to_string());
    }

    let metrics = if endpoint.starts_with("http://") || endpoint.starts_with("https://") {
        fetch_bridge_http_metrics(endpoint, &config.api_key).await?
    } else {
        fetch_bridge_mqtt_metrics(endpoint, &config.api_key).await?
    };

    normalize_bridge_metrics(
        &metrics,
        mining_watts,
        config.base_load_watts,
        &solar_transport("bridge", endpoint),
    )
}

pub(super) async fn fetch_ecoflow_snapshot(
    config: &SolarConfigPayload,
    mining_watts: u32,
) -> std::result::Result<SolarProviderSnapshot, String> {
    let endpoint = config.api_endpoint.trim();
    let metrics = if endpoint.starts_with("http://") || endpoint.starts_with("https://") {
        fetch_bridge_http_metrics(endpoint, &config.api_key).await?
    } else {
        fetch_bridge_mqtt_metrics(endpoint, &config.api_key).await?
    };
    for shape in ECOFLOW_PAYLOAD_SHAPES {
        if let Some(mut snapshot) =
            normalize_ecoflow_shape(&metrics, mining_watts, config.base_load_watts, shape)
        {
            snapshot.transport = solar_transport("ecoflow", endpoint);
            return Ok(snapshot);
        }
    }

    Err("EcoFlow provider did not match any supported normalized payload shape. Expected one of: bridge-contract (productionWatts + consumptionWatts|netGridWatts), site-summary (pvWatts|solarWatts + homeLoadWatts|loadPowerWatts|consumptionWatts), or power-summary (solarInputWatts|inputPowerWatts + outputWatts|homeLoadWatts|consumptionWatts).".to_string())
}

pub(super) async fn fetch_tesla_snapshot(
    config: &SolarConfigPayload,
) -> std::result::Result<SolarProviderSnapshot, String> {
    let base = config.api_endpoint.trim().trim_end_matches('/');
    if !(base.starts_with("http://") || base.starts_with("https://")) {
        return Err(
            "Tesla local backend requires an http:// or https:// gateway endpoint".to_string(),
        );
    }

    let aggregates_url = format!("{}/api/meters/aggregates", base);
    let soe_url = format!("{}/api/system_status/soe", base);
    let aggregates =
        fetch_http_json(&aggregates_url, &config.api_key, true, "Tesla local", true).await?;
    let soe = match fetch_http_json(&soe_url, &config.api_key, true, "Tesla local", true).await {
        Ok(value) => Some(value),
        Err(message) => {
            if message.contains("Authentication is missing or invalid") {
                None
            } else {
                return Err(message);
            }
        }
    };

    normalize_tesla_snapshot(&aggregates, soe.as_ref())
}

pub(super) async fn fetch_victron_snapshot(
    config: &SolarConfigPayload,
    mining_watts: u32,
) -> std::result::Result<SolarProviderSnapshot, String> {
    let endpoint = config.api_endpoint.trim();
    if endpoint.is_empty() {
        return Err("Victron endpoint is empty.".to_string());
    }

    let metrics = if endpoint.starts_with("http://") || endpoint.starts_with("https://") {
        fetch_victron_http_metrics(endpoint, &config.api_key).await?
    } else {
        fetch_victron_mqtt_metrics(endpoint, &config.api_key).await?
    };

    normalize_victron_metrics(
        &metrics,
        mining_watts,
        config.base_load_watts,
        if endpoint.starts_with("http://") || endpoint.starts_with("https://") {
            "http-json"
        } else {
            "mqtt"
        },
    )
}

/// GET /api/solar/config -- Persisted solar/hybrid integration config.
pub(super) async fn get_solar_config() -> impl IntoResponse {
    let payload = load_solar_config_payload();
    let support = solar_provider_support(&payload.inverter_brand);
    Json(serde_json::json!({
        "enabled": payload.enabled,
        "inverterBrand": payload.inverter_brand,
        "apiEndpoint": payload.api_endpoint,
        "apiKey": payload.api_key,
        "solarOnlyMode": payload.solar_only_mode,
        "baseLoadWatts": payload.base_load_watts,
        "batteryThresholdPct": payload.battery_threshold_pct,
        "manualProductionWatts": payload.manual_production_watts,
        "manualSiteLoadWatts": payload.manual_site_load_watts,
        "manualBatterySocPct": payload.manual_battery_soc_pct,
        "providerLiveBackend": support.live_backend,
        "providerTelemetryBacked": crate::solar_provider_telemetry_backed(&payload.inverter_brand),
        "providerStage": support.stage,
        "providerStageReason": support.stage_reason,
        "recommendedProvider": support.recommended_provider,
        "providerBackendScope": support.backend_scope,
        "acceptedPayloadShapes": support.accepted_payload_shapes,
    }))
}

/// POST /api/solar/config -- Persisted solar/hybrid integration config.
pub(super) async fn post_solar_config(
    State(state): State<Arc<AppState>>,
    Json(body): Json<SolarConfigPayload>,
) -> impl IntoResponse {
    // CE-111: dedicated power-policy writer — gate on ConfigRw AND PowerControl.
    if let Err(response) = require_antminer_runtime_capability(
        &state,
        RuntimeCapability::ConfigRw,
        "/api/solar/config",
    ) {
        return response;
    }
    if let Err(response) = require_antminer_runtime_capability(
        &state,
        RuntimeCapability::PowerControl,
        "/api/solar/config",
    ) {
        return response;
    }

    if let Err(message) = validate_solar_config(&body) {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "status": "error", "message": message })),
        )
            .into_response();
    }

    match persist_solar_config(&body) {
        Ok(()) => {
            let support = solar_provider_support(&body.inverter_brand);
            Json(serde_json::json!({
                "status": "ok",
                "message": "Solar integration config saved.",
                "config": body,
                "providerLiveBackend": support.live_backend,
                "providerTelemetryBacked": crate::solar_provider_telemetry_backed(&body.inverter_brand),
                "providerStage": support.stage,
                "providerStageReason": support.stage_reason,
                "recommendedProvider": support.recommended_provider,
                "providerBackendScope": support.backend_scope,
                "acceptedPayloadShapes": support.accepted_payload_shapes,
            }))
            .into_response()
        }
        Err(message) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "status": "error", "message": message })),
        )
            .into_response(),
    }
}

/// POST /api/solar/test -- Validate a solar provider path before trusting it.
pub(super) async fn post_solar_test(
    State(state): State<Arc<AppState>>,
    Json(body): Json<SolarConfigPayload>,
) -> impl IntoResponse {
    if let Err(message) = validate_solar_config(&body) {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "ok": false, "message": message })),
        )
            .into_response();
    }

    let power = state.power_rx.borrow().clone();
    let mining_watts = crate::solar_mining_power_status(&power).watts;
    let support = solar_provider_support(&body.inverter_brand);
    let provider_live_backend = support.live_backend;
    let provider_telemetry_backed = crate::solar_provider_telemetry_backed(&body.inverter_brand);
    let provider_stage = support.stage.clone();
    let provider_stage_reason = support.stage_reason.clone();
    let recommended_provider = support.recommended_provider.clone();
    let provider_backend_scope = support.backend_scope.clone();
    let accepted_payload_shapes = support.accepted_payload_shapes.clone();
    let response = match body.inverter_brand.as_str() {
        "manual" => serde_json::json!({
            "ok": true,
            "provider": "manual",
            "connected": true,
            "transport": "manual",
            "providerLiveBackend": provider_live_backend,
            "providerTelemetryBacked": provider_telemetry_backed,
            "providerStage": provider_stage.clone(),
            "providerStageReason": provider_stage_reason.clone(),
            "recommendedProvider": recommended_provider.clone(),
            "providerBackendScope": provider_backend_scope.clone(),
            "acceptedPayloadShapes": accepted_payload_shapes.clone(),
            "message": "Manual provider does not require a live connection test.",
        }),
        "victron" => match fetch_victron_snapshot(&body, mining_watts).await {
            Ok(snapshot) => serde_json::json!({
                "ok": true,
                "provider": "victron",
                "connected": snapshot.connected,
                "transport": snapshot.transport,
                "providerTelemetryBacked": provider_telemetry_backed,
                "matchedFields": snapshot.matched_fields,
                "productionWatts": snapshot.production_watts,
                "consumptionWatts": snapshot.consumption_watts,
                "netGridWatts": snapshot.net_grid_watts,
                "batterySocPct": snapshot.battery_soc_pct,
                "message": snapshot.message,
            }),
            Err(message) => serde_json::json!({
                "ok": false,
                "provider": "victron",
                "connected": false,
                "message": message,
            }),
        },
        "bridge" => match fetch_bridge_snapshot(&body, mining_watts).await {
            Ok(snapshot) => serde_json::json!({
                "ok": true,
                "provider": "bridge",
                "connected": snapshot.connected,
                "transport": snapshot.transport,
                "matchedFields": snapshot.matched_fields,
                "productionWatts": snapshot.production_watts,
                "consumptionWatts": snapshot.consumption_watts,
                "netGridWatts": snapshot.net_grid_watts,
                "batterySocPct": snapshot.battery_soc_pct,
                "message": snapshot.message,
            }),
            Err(message) => serde_json::json!({
                "ok": false,
                "provider": "bridge",
                "connected": false,
                "transport": solar_transport("bridge", &body.api_endpoint),
                "providerTelemetryBacked": provider_telemetry_backed,
                "message": message,
            }),
        },
        "ecoflow" => match fetch_ecoflow_snapshot(&body, mining_watts).await {
            Ok(snapshot) => serde_json::json!({
                "ok": true,
                "provider": "ecoflow",
                "connected": snapshot.connected,
                "transport": snapshot.transport,
                "providerLiveBackend": provider_live_backend,
                "providerTelemetryBacked": provider_telemetry_backed,
                "providerStage": provider_stage.clone(),
                "providerStageReason": provider_stage_reason.clone(),
                "recommendedProvider": recommended_provider.clone(),
                "providerBackendScope": provider_backend_scope.clone(),
                "acceptedPayloadShapes": accepted_payload_shapes.clone(),
                "matchedFields": snapshot.matched_fields,
                "productionWatts": snapshot.production_watts,
                "consumptionWatts": snapshot.consumption_watts,
                "netGridWatts": snapshot.net_grid_watts,
                "batterySocPct": snapshot.battery_soc_pct,
                "sampleAgeMs": snapshot.sample_age_ms,
                "stale": snapshot.stale,
                "message": snapshot.message,
            }),
            Err(message) => serde_json::json!({
                "ok": false,
                "provider": "ecoflow",
                "connected": false,
                "transport": solar_transport("ecoflow", &body.api_endpoint),
                "providerLiveBackend": provider_live_backend,
                "providerTelemetryBacked": provider_telemetry_backed,
                "providerStage": provider_stage.clone(),
                "providerStageReason": provider_stage_reason.clone(),
                "recommendedProvider": recommended_provider.clone(),
                "providerBackendScope": provider_backend_scope.clone(),
                "acceptedPayloadShapes": accepted_payload_shapes.clone(),
                "message": message,
            }),
        },
        "enphase" => match fetch_enphase_snapshot(&body).await {
            Ok(snapshot) => serde_json::json!({
                "ok": true,
                "provider": "enphase",
                "connected": snapshot.connected,
                "transport": snapshot.transport,
                "matchedFields": snapshot.matched_fields,
                "productionWatts": snapshot.production_watts,
                "consumptionWatts": snapshot.consumption_watts,
                "netGridWatts": snapshot.net_grid_watts,
                "batterySocPct": snapshot.battery_soc_pct,
                "message": snapshot.message,
            }),
            Err(message) => serde_json::json!({
                "ok": false,
                "provider": "enphase",
                "connected": false,
                "transport": "http-json",
                "message": message,
            }),
        },
        "solaredge" => match fetch_solaredge_snapshot(&body).await {
            Ok(snapshot) => serde_json::json!({
                "ok": true,
                "provider": "solaredge",
                "connected": snapshot.connected,
                "transport": snapshot.transport,
                "matchedFields": snapshot.matched_fields,
                "productionWatts": snapshot.production_watts,
                "consumptionWatts": snapshot.consumption_watts,
                "netGridWatts": snapshot.net_grid_watts,
                "batterySocPct": snapshot.battery_soc_pct,
                "message": snapshot.message,
            }),
            Err(message) => serde_json::json!({
                "ok": false,
                "provider": "solaredge",
                "connected": false,
                "transport": "cloud-http",
                "message": message,
            }),
        },
        "tesla" => match fetch_tesla_snapshot(&body).await {
            Ok(snapshot) => serde_json::json!({
                "ok": true,
                "provider": "tesla",
                "connected": snapshot.connected,
                "transport": snapshot.transport,
                "matchedFields": snapshot.matched_fields,
                "productionWatts": snapshot.production_watts,
                "consumptionWatts": snapshot.consumption_watts,
                "netGridWatts": snapshot.net_grid_watts,
                "batterySocPct": snapshot.battery_soc_pct,
                "message": snapshot.message,
            }),
            Err(message) => serde_json::json!({
                "ok": false,
                "provider": "tesla",
                "connected": false,
                "transport": "http-json",
                "message": message,
            }),
        },
        provider => serde_json::json!({
            "ok": false,
            "provider": provider,
            "connected": false,
            "transport": solar_transport(provider, &body.api_endpoint),
            "providerLiveBackend": provider_live_backend,
            "providerTelemetryBacked": provider_telemetry_backed,
            "providerStage": provider_stage,
            "providerStageReason": provider_stage_reason.clone(),
            "recommendedProvider": recommended_provider,
            "providerBackendScope": provider_backend_scope,
            "acceptedPayloadShapes": accepted_payload_shapes,
            "message": provider_stage_reason.unwrap_or_else(|| format!("{} backend is not implemented yet.", provider)),
        }),
    };

    Json(response).into_response()
}

/// GET /api/solar/status -- Normalized solar/hybrid energy status.
pub(super) async fn get_solar_status(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    if let Some(rx) = &state.solar_rx {
        let telemetry = rx.borrow().clone();
        if telemetry.enabled {
            return Json(serde_json::json!(telemetry));
        }
    }

    let config = load_solar_config_payload();
    let power = state.power_rx.borrow().clone();
    let mining_power = crate::solar_mining_power_status(&power);
    let support = solar_provider_support(&config.inverter_brand);
    let provider_live_backend = support.live_backend;
    let provider_telemetry_backed = crate::solar_provider_telemetry_backed(&config.inverter_brand);
    let provider_stage = support.stage.clone();
    let provider_stage_reason = support.stage_reason.clone();
    let recommended_provider = support.recommended_provider.clone();
    let provider_backend_scope = support.backend_scope.clone();
    let accepted_payload_shapes = support.accepted_payload_shapes.clone();

    if !config.enabled {
        return Json(serde_json::json!({
            "enabled": false,
            "productionWatts": 0,
            "consumptionWatts": 0,
            "miningWatts": mining_power.watts,
            "miningWattsSource": mining_power.source.as_str(),
            "miningWattsLive": mining_power.live,
            "miningWattsModeled": mining_power.modeled,
            "miningWattsNote": mining_power.note,
            "netGridWatts": mining_power.watts,
            "solarSurplusWatts": 0,
            "batterySocPct": null,
            "connected": false,
            "provider": config.inverter_brand,
            "providerLiveBackend": provider_live_backend,
            "providerTelemetryBacked": provider_telemetry_backed,
            "providerConfigured": false,
            "providerStage": provider_stage.clone(),
            "providerStageReason": provider_stage_reason.clone(),
            "recommendedProvider": recommended_provider.clone(),
            "providerBackendScope": provider_backend_scope.clone(),
            "acceptedPayloadShapes": accepted_payload_shapes.clone(),
            "runtimeAdopted": false,
            "commissioningState": "disabled",
            "message": "Solar integration is disabled.",
        }));
    }

    Json(solar_status_pending_restart_payload(&config, &mining_power))
}

/// GET /api/solar/verification-history -- Rolling provider verification samples.
pub(super) async fn get_solar_verification_history(
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    let entries = state
        .solar_history
        .lock()
        .map(|guard| guard.clone())
        .unwrap_or_default();
    Json(serde_json::json!({
        "generatedAtMs": std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64,
        "entries": entries,
    }))
}

// ─── TOU / Power Schedule Handlers ──────────────────────────────────

/// GET /api/tou/schedule -- Current TOU/power schedule.
pub(super) async fn get_tou_schedule() -> impl IntoResponse {
    let config_path = if std::path::Path::new("/data/dcentrald.toml").exists() {
        "/data/dcentrald.toml"
    } else {
        "/etc/dcentrald.toml"
    };

    let schedule = (|| {
        let contents = std::fs::read_to_string(config_path).ok()?;
        let table: toml::Table = toml::from_str(&contents).ok()?;
        let sched = table
            .get("autotuner")?
            .as_table()?
            .get("schedule")?
            .as_table()?;

        let enabled = sched
            .get("enabled")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let tz_offset = sched
            .get("timezone_offset_hours")
            .and_then(|v| v.as_integer())
            .unwrap_or(0) as i8;
        let ramp = sched
            .get("ramp_duration_s")
            .and_then(|v| v.as_integer())
            .unwrap_or(60) as u32;

        let slots: Vec<serde_json::Value> = sched
            .get("slots")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter().map(|s| {
                serde_json::json!({
                    "start_hour": s.get("start_hour").and_then(|v| v.as_integer()).unwrap_or(0),
                    "end_hour": s.get("end_hour").and_then(|v| v.as_integer()).unwrap_or(0),
                    "target_watts": s.get("target_watts").and_then(|v| v.as_integer()).unwrap_or(0),
                    "label": s.get("label").and_then(|v| v.as_str()).unwrap_or(""),
                })
            }).collect()
            })
            .unwrap_or_default();

        // Determine active slot based on current hour
        let now_hour = {
            let secs = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            let total_hours = (secs / 3600) as i64;
            (((total_hours + tz_offset as i64) % 24 + 24) % 24) as u8
        };

        Some(serde_json::json!({
            "enabled": enabled,
            "slots": slots,
            "timezone_offset_hours": tz_offset,
            "ramp_duration_s": ramp,
            "current_hour": now_hour,
        }))
    })();

    Json(schedule.unwrap_or_else(|| {
        serde_json::json!({
            "enabled": false,
            "slots": [],
            "timezone_offset_hours": 0,
            "ramp_duration_s": 60,
            "current_hour": 0,
        })
    }))
}

/// POST /api/tou/schedule -- Update TOU/power schedule.
pub(super) async fn post_tou_schedule(
    State(state): State<Arc<AppState>>,
    Json(body): Json<serde_json::Value>,
) -> Response {
    // CE-111: dedicated power-policy writer — gate on ConfigRw AND PowerControl
    // (the TOU schedule drives runtime mining power over time).
    if let Err(response) = require_antminer_runtime_capability(
        &state,
        RuntimeCapability::ConfigRw,
        "/api/tou/schedule",
    ) {
        return response;
    }
    if let Err(response) = require_antminer_runtime_capability(
        &state,
        RuntimeCapability::PowerControl,
        "/api/tou/schedule",
    ) {
        return response;
    }

    let enabled = body
        .get("enabled")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let tz_offset = body
        .get("timezone_offset_hours")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let ramp = body
        .get("ramp_duration_s")
        .and_then(|v| v.as_u64())
        .unwrap_or(60);

    // Validate
    if !(-12..=14).contains(&tz_offset) {
        return Json(serde_json::json!({
            "status": "error",
            "message": "timezone_offset_hours must be between -12 and 14",
        }))
        .into_response();
    }

    // Persist to config
    let config_path = if std::path::Path::new("/data/dcentrald.toml").exists() {
        "/data/dcentrald.toml"
    } else {
        "/etc/dcentrald.toml"
    };

    let mut persisted = false;
    let _cfg_write_guard = crate::atomic_io::config_write_lock();
    if let Ok(contents) = std::fs::read_to_string(config_path) {
        if let Ok(mut doc) = contents.parse::<toml::Value>() {
            match ensure_toml_value_table_section(&mut doc, "autotuner") {
                Ok(autotuner) => {
                    let mut sched = toml::value::Table::new();
                    sched.insert("enabled".into(), toml::Value::Boolean(enabled));
                    sched.insert(
                        "timezone_offset_hours".into(),
                        toml::Value::Integer(tz_offset),
                    );
                    sched.insert("ramp_duration_s".into(), toml::Value::Integer(ramp as i64));

                    // Convert slots from JSON to TOML
                    if let Some(slots) = body.get("slots").and_then(|v| v.as_array()) {
                        let toml_slots: Vec<toml::Value> = slots
                            .iter()
                            .map(|s| {
                                let mut slot = toml::value::Table::new();
                                slot.insert(
                                    "start_hour".into(),
                                    toml::Value::Integer(
                                        s.get("start_hour").and_then(|v| v.as_i64()).unwrap_or(0),
                                    ),
                                );
                                slot.insert(
                                    "end_hour".into(),
                                    toml::Value::Integer(
                                        s.get("end_hour").and_then(|v| v.as_i64()).unwrap_or(0),
                                    ),
                                );
                                slot.insert(
                                    "target_watts".into(),
                                    toml::Value::Integer(
                                        s.get("target_watts").and_then(|v| v.as_i64()).unwrap_or(0),
                                    ),
                                );
                                if let Some(label) = s.get("label").and_then(|v| v.as_str()) {
                                    slot.insert(
                                        "label".into(),
                                        toml::Value::String(label.to_string()),
                                    );
                                }
                                toml::Value::Table(slot)
                            })
                            .collect();
                        sched.insert("slots".into(), toml::Value::Array(toml_slots));
                    }

                    autotuner.insert("schedule".into(), toml::Value::Table(sched));

                    if let Ok(serialized) = toml::to_string_pretty(&doc) {
                        if atomic_write(config_path, &serialized).is_ok() {
                            persisted = true;
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, path = config_path, "Failed to update TOU schedule config")
                }
            }
        }
    }

    tracing::info!(enabled, tz_offset, ramp, persisted, "TOU schedule updated");

    Json(serde_json::json!({
        "status": if persisted { "ok" } else { "warning" },
        "persisted": persisted,
        "restart_required": true,
        "message": if enabled {
            "TOU schedule active — mining power will follow the configured time slots. Restart to apply."
        } else {
            "TOU schedule disabled — mining at full power. Restart to apply."
        },
    }))
    .into_response()
}

// ─── Home Mode Handlers ──────────────────────────────────────────────

pub(super) fn home_noise_from_fans(
    fans: &crate::FanState,
) -> (Option<u32>, &'static str, &'static str, u32, bool) {
    let max_fan_rpm = fans
        .per_fan
        .iter()
        .map(|fan| fan.rpm)
        .max()
        .unwrap_or(fans.rpm)
        .max(fans.rpm);
    let fan_rpm_feedback_available = max_fan_rpm > 0;
    let noise_db = if fan_rpm_feedback_available {
        Some(std::cmp::min(75, 30 + (max_fan_rpm / 120)))
    } else {
        None
    };
    let noise_source = if fan_rpm_feedback_available {
        "tach_estimate"
    } else {
        "unavailable_no_rpm_feedback"
    };
    let noise_note = if fan_rpm_feedback_available {
        "Estimated from live fan RPM; verify room dB for AM2 low-PWM floor diagnosis"
    } else {
        "No noise estimate: PWM command alone is not acoustic proof"
    };
    (
        noise_db,
        noise_source,
        noise_note,
        max_fan_rpm,
        fan_rpm_feedback_available,
    )
}

/// GET /api/home/status -- Home mode status.
///
/// Returns heat output (watts AND BTU/h), hashrate, room temperature,
/// noise level, electricity cost offset, and circuit headroom.
/// Shows both mining AND heating data — it's a bitcoin miner that heats your home.
pub(super) async fn get_home_status(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let miner = state.state_rx.borrow().clone();
    let mode = *state.mode_rx.borrow();

    // Use live power from the work dispatcher (updated every 5s from actual
    // per-chip frequencies and voltages). Falls back to static model if the
    // dispatcher hasn't published yet.
    let live = state.power_rx.borrow().clone();
    let hardware = state
        .hardware_info
        .lock()
        .map(|guard| guard.clone())
        .unwrap_or_default();
    let home_power = project_power_telemetry(&live, &miner, &hardware);
    let targeting = build_power_targeting_state(mode, &home_power);

    let (noise_db, noise_source, noise_note, max_fan_rpm, fan_rpm_feedback_available) =
        home_noise_from_fans(&miner.fans);

    // Estimate airflow from live RPM only (rough: 1 CFM per 30 RPM for S9-class fans).
    let airflow_cfm = (max_fan_rpm as f64 / 30.0) as u32;

    // Read electricity economics + circuit capacity from config. P2-4 (§4.E):
    // the daemon `[home]` section is the SINGLE SOURCE OF TRUTH for the
    // electricity rate + currency + calibration flag — the dashboard surfaces
    // these values (below) instead of guessing from its own localStorage.
    // P3-2: post-write-fresh in-memory config cache (no per-request disk read).
    let home_config_table = state.config_cache.snapshot();
    let economics = home_economics_from_table(&home_config_table);
    let electricity_rate = economics.rate_usd_per_kwh;
    let circuit_capacity = home_config_table
        .get("power")
        .and_then(|v| v.as_table())
        .and_then(|p| p.get("circuit_capacity_watts"))
        .and_then(|v| v.as_integer())
        .map(|v| v as u32)
        .unwrap_or(1800);
    let home_cost = project_power_costs(&home_power, electricity_rate, 0.0);
    let circuit_headroom_pct = if circuit_capacity > 0 {
        ((circuit_capacity as f64 - home_cost.wall_watts as f64) / circuit_capacity as f64 * 100.0)
            .max(0.0)
    } else {
        100.0
    };
    let circuit_usage_pct = 100.0 - circuit_headroom_pct;

    // Read room temp from shared atomic (set via POST /api/home/room-temp)
    let room_temp_raw = state
        .room_temp_c10
        .load(std::sync::atomic::Ordering::Relaxed);
    let room_temp_val: serde_json::Value = if room_temp_raw == 0 {
        serde_json::Value::Null
    } else {
        serde_json::json!(room_temp_raw as f32 / 10.0)
    };

    // P0-4 (C-5/C-6): estimate sats earned today from the CANONICAL,
    // network-difficulty-anchored model. The previous code multiplied the
    // accepted-share count by the pool *share* difficulty with NO network term
    // (accepted * share_diff * 1e8 / 2^32), which treats every accepted share
    // as if it were a block reward. That inflated the figure ~22,000x — a
    // ~1.9 TH/s S9 read tens of millions of sats (~$228/day) instead of the
    // real ~100 sats/day. We now anchor to the live network difficulty cache
    // (RE-011) + the active block subsidy:
    //   sats/day = hashrate_ths * 1e12 * 86_400 / (network_difficulty * 2^32)
    //              * block_subsidy_sats
    // When the live difficulty is absent (offline / not yet fetched) we DO NOT
    // fabricate a number: sats_today is 0 and `sats_today_calibrated=false`
    // flags the surface as an uncalibrated estimate (truth-contract: modeled
    // != measured, and an unknown input must not emit a confident value).
    let net_diff = cached_network_difficulty();
    let net_diff_value = net_diff.as_ref().map(|d| d.difficulty).filter(|d| *d > 0.0);
    let block_subsidy_sats =
        dcentrald_autotuner::profitability::block_reward_at(std::time::SystemTime::now())
            * 100_000_000.0;
    // `calibrated` tracks whether the live network difficulty is available (the
    // load-bearing input), NOT whether the miner is currently hashing — an idle
    // miner with difficulty known is a *calibrated* 0 sats, not "uncalibrated".
    let sats_today_calibrated = net_diff_value.is_some();
    let sats_today: u64 = net_diff_value
        .and_then(|diff| {
            estimate_daily_sats_network_anchored(
                miner.hashrate_ghs / 1000.0,
                diff,
                block_subsidy_sats,
            )
        })
        .unwrap_or(0);
    let sats_today_note = if sats_today_calibrated {
        "Modeled from live network difficulty and the current block subsidy at this hashrate — a statistical projection, not measured payout."
    } else {
        "Uncalibrated estimate: live network difficulty is unavailable (offline or not yet fetched), so a daily-sats figure cannot be computed. Shown as 0 rather than a fabricated number."
    };

    // FIX (2026-04-11): Emit daily_cost_usd as a JSON number, not a string.
    let daily_cost_usd_rounded = (home_cost.daily_cost_usd * 100.0).round() / 100.0;

    Json(serde_json::json!({
        "power_watts": home_power.board_watts,
        "wall_watts": home_power.wall_watts,
        "btu_h": home_power.btu_h as u32,
        "source": home_power.source,
        "power_source_detail": home_power.source_detail,
        "live_power_available": home_power.live_power_available,
        "power_modeled": home_power.modeled,
        "power_note": home_power.note,
        "calibrated": home_power.calibrated,
        "calibration_multiplier": home_power.calibration_multiplier,
        "targeting": targeting,
        "noise_db": noise_db,
        "noise_source": noise_source,
        "noise_note": noise_note,
        "airflow_cfm": airflow_cfm,
        "preset": "medium",
        "room_temp_c": room_temp_val,
        "cost_today_usd": daily_cost_usd_rounded,
        "daily_cost_usd": daily_cost_usd_rounded,
        "daily_cost_power_watts": home_cost.wall_watts,
        "daily_cost_power_live_available": home_cost.live_power_available,
        "daily_cost_power_modeled": home_cost.modeled,
        "daily_cost_power_source_detail": home_cost.source_detail,
        "daily_cost_note": home_cost.note,
        // P2-4 (§4.E): surface the daemon-persisted economics so the dashboard
        // uses THIS (the single source of truth), not its own localStorage
        // guess. `electricity_rate_calibrated=false` ⇒ the rate is the daemon
        // default (not operator-confirmed) and the UI must flag cost/earnings
        // as an uncalibrated estimate until the operator confirms a rate.
        "electricity_rate": electricity_rate,
        "currency": economics.currency,
        "electricity_rate_calibrated": economics.rate_calibrated,
        "sats_today": sats_today,
        // P0-4: honest calibration metadata for the daily-sats estimate.
        // `sats_today_calibrated=false` means the live network difficulty was
        // unavailable, so `sats_today` is 0 (NOT a fabricated number) and the
        // UI must label it an uncalibrated estimate. `network_difficulty` is
        // the difficulty the estimate was anchored to (null when uncalibrated)
        // so the dashboard can run the same canonical model client-side.
        "sats_today_calibrated": sats_today_calibrated,
        "sats_today_note": sats_today_note,
        "network_difficulty": net_diff_value,
        "night_mode_active": false,
        "night_mode_starts_in_s": null,
        // Still a bitcoin miner — always show hashrate!
        "hashrate_ghs": miner.hashrate_ghs,
        "hashrate_ths": miner.hashrate_ghs / 1000.0,
        // Circuit safety indicator
        "circuit_usage_pct": format!("{:.0}", circuit_usage_pct),
        "circuit_power_watts": home_cost.wall_watts,
        "circuit_power_live_available": home_cost.live_power_available,
        "circuit_power_modeled": home_cost.modeled,
        "circuit_power_source_detail": home_cost.source_detail,
        "circuit_note": home_cost.note,
        "circuit_status": if !home_cost.live_power_available { "unavailable" }
            else if circuit_usage_pct > 85.0 { "danger" }
            else if circuit_usage_pct > 70.0 { "warning" }
            else { "ok" },
        // Fans
        "fans": {
            "pwm": miner.fans.pwm,
            "rpm": miner.fans.rpm,
            "max_rpm": max_fan_rpm,
            "rpm_feedback_available": fan_rpm_feedback_available,
        },
    }))
}

/// POST /api/home/target -- Set home power target.
///
/// Body: `{ "preset": "medium" }` or `{ "watts": 800 }`
/// Writes the target to the [home] section of dcentrald.toml.
/// The thermal loop reads the config on each tick, so changes take effect
/// within one thermal cycle (~5s).
pub(super) async fn post_home_target(
    State(state): State<Arc<AppState>>,
    Json(body): Json<HomeTargetRequest>,
) -> Response {
    // CE-111: dedicated power-policy writer — gate on ConfigRw AND PowerControl
    // (writes [home].target_watts, live power ceiling for the heater path).
    if let Err(response) =
        require_antminer_runtime_capability(&state, RuntimeCapability::ConfigRw, "/api/home/target")
    {
        return response;
    }
    if let Err(response) = require_antminer_runtime_capability(
        &state,
        RuntimeCapability::PowerControl,
        "/api/home/target",
    ) {
        return response;
    }

    tracing::info!(preset = ?body.preset, watts = ?body.watts, "Home target change requested");

    // Resolve preset to watts if needed
    let target_watts = match (&body.preset, body.watts) {
        (_, Some(w)) => {
            // Boundary validation: clamp the caller-supplied watts to the
            // documented residential power envelope before it is persisted to
            // [home].target_watts. 1800 W is the absolute Hacker SafetyEnvelope
            // ceiling (120 V x 15 A circuit max); 50 W is a sane floor. Rejecting
            // out-of-range here means an absurd value is never written to /data
            // and can never feed the night-mode multiply in
            // dcentrald-thermal::heater::HeaterController::effective_target_watts.
            // (Defense-in-depth only — the heater per-tick ratio clamp + autotuner
            // freq/voltage bounds remain the load-bearing hardware guards.)
            const MIN_HOME_WATTS: u32 = 50;
            const MAX_HOME_WATTS: u32 = 1800;
            if !(MIN_HOME_WATTS..=MAX_HOME_WATTS).contains(&w) {
                return Json(serde_json::json!({
                    "status": "error",
                    "message": format!(
                        "watts {} out of range. Valid: {}-{} W.",
                        w, MIN_HOME_WATTS, MAX_HOME_WATTS
                    ),
                }))
                .into_response();
            }
            w
        }
        (Some(preset), None) => match preset.as_str() {
            "whisper" => 300,
            "low" => 500,
            "medium" => 800,
            "high" => 1200,
            "max" => 1400,
            _ => {
                return Json(serde_json::json!({
                    "status": "error",
                    "message": format!("Unknown preset '{}'. Valid: whisper, low, medium, high, max", preset),
                }))
                .into_response();
            }
        },
        (None, None) => {
            return Json(serde_json::json!({
                "status": "error",
                "message": "Either 'preset' or 'watts' must be provided",
            }))
            .into_response();
        }
    };

    // CFG-1/CFG-2: route through the canonical hardened read-modify-write so the
    // FULL effective config (merging the baked `/etc` config when `/data` is
    // absent) is loaded, only the `[home]` keys are mutated, and the whole table
    // is written back atomically. The previous bespoke path read a `/data`-only
    // file and, on a fresh beta install where `/data` is absent, wrote a
    // `[home]`-only file that shadowed the baked `[pool]`/`[power]`/`[thermal]`/
    // `[auth]` sections on the next reboot. The dead `if/else` (both arms were
    // `/data/dcentrald.toml`) is removed.
    let config_path = get_writable_config_path();
    let write_result = (|| -> std::result::Result<(), String> {
        // RELIAB-2b: serialize load→modify→write (lost-update guard). Scoped to
        // this synchronous closure so it drops before any `.await`.
        let _cfg_write_guard = crate::atomic_io::config_write_lock();
        let mut table = load_config_table_for_write()?;

        if let Some(parent) = std::path::Path::new(config_path).parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("Failed to create config directory: {}", e))?;
        }

        apply_home_power_target_to_table(&mut table, target_watts, body.preset.as_deref());

        let output =
            toml::to_string_pretty(&table).map_err(|e| format!("Serialize error: {}", e))?;
        atomic_write(config_path, output).map_err(|e| format!("Write error: {}", e))?;
        Ok(())
    })();

    match write_result {
        Ok(()) => {
            tracing::info!(
                target_watts,
                "Home power target saved — thermal loop will apply on next tick"
            );
            // W21 audit-coverage: record the operator-set home power target.
            push_rest_audit_free(
                &state,
                "home",
                format!("Home power target set: {target_watts} W"),
            );
            Json(serde_json::json!({
                "status": "ok",
                "message": "Home power target updated",
                "target_watts": target_watts,
                "preset": body.preset,
            }))
            .into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "Failed to save home target");
            Json(serde_json::json!({
                "status": "error",
                "message": format!("Failed to save: {}", e),
            }))
            .into_response()
        }
    }
}

pub(super) fn home_preset_values() -> Vec<serde_json::Value> {
    dcentrald_thermal::profiles::PowerPreset::s9_presets()
        .into_iter()
        .map(|preset| {
            let (display_name, description) = match preset.name.as_str() {
                "whisper" => (
                    "Low Power (300W)",
                    "Minimal heat; acoustic result requires live RPM/noise verification",
                ),
                "low" => (
                    "Low Heat (500W)",
                    "Gentle warmth; noise is RPM-dependent",
                ),
                "medium" => (
                    "Comfortable (800W)",
                    "Balanced heat target; verify fan RPM for acoustic result",
                ),
                "high" => (
                    "Toasty (1200W)",
                    "Strong heat output; noise depends on platform fan floor",
                ),
                "max" => (
                    "Max Heat (1400W)",
                    "Full heat output; verify live fan RPM and room noise",
                ),
                _ => (preset.name.as_str(), "S9 home-heating preset"),
            };

            serde_json::json!({
                "name": preset.name,
                "display_name": display_name,
                "watts": preset.watts,
                "wall_watts": preset.wall_watts,
                "btu_h": preset.btu_h,
                "noise_db": serde_json::Value::Null,
                "estimated_noise_db_s9": preset.noise_db,
                "noise_note": "S9 estimate only; AM2 acoustic result requires live RPM/noise verification",
                "hashrate_ths": preset.hashrate_ths,
                "description": description,
            })
        })
        .collect()
}

/// GET /api/home/presets -- Available home-heating presets.
///
/// These estimates are currently S9/BM1387-scoped, not universal across all miners.
pub(super) async fn get_home_presets(State(_state): State<Arc<AppState>>) -> impl IntoResponse {
    let presets = home_preset_values();

    Json(serde_json::json!({
        "scope": {
            "kind": "family",
            "family": "s9",
            "chip_type": "BM1387",
            "label": "S9 / BM1387",
            "universal": false,
        },
        "presets": presets,
    }))
}

/// POST /api/home/room-temp -- Set room temperature (manual/userinput source).
///
/// Stores the room temperature in shared atomic state so the thermal loop's
/// HeaterController PID can read it. Also used by Home Assistant integration.
pub(super) async fn post_home_room_temp(
    State(state): State<Arc<AppState>>,
    Json(body): Json<RoomTempRequest>,
) -> impl IntoResponse {
    // CE-111: dedicated config writer — gate on runtime ConfigRw.
    if let Err(response) = require_antminer_runtime_capability(
        &state,
        RuntimeCapability::ConfigRw,
        "/api/home/room-temp",
    ) {
        return response;
    }

    tracing::info!(temp_c = body.temp_c, "Room temperature updated via API");

    // Validate range
    if body.temp_c < 0.0 || body.temp_c > 80.0 || body.temp_c.is_nan() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "status": "error",
                "message": "temp_c must be in range [0.0, 80.0]",
            })),
        )
            .into_response();
    }

    // Store as fixed-point (temp * 10) in the atomic for lock-free access
    let temp_c10 = (body.temp_c * 10.0) as u32;
    state
        .room_temp_c10
        .store(temp_c10, std::sync::atomic::Ordering::Relaxed);

    tracing::info!(
        temp_c = body.temp_c,
        temp_c10,
        "Room temperature stored in shared state"
    );
    push_rest_audit_free(
        &state,
        "room_temperature",
        format!("Home room temperature updated: temp_c={:.1}", body.temp_c),
    );

    Json(serde_json::json!({
        "status": "ok",
        "room_temp_c": body.temp_c,
        "message": "Room temperature updated — thermal PID will use this on next cycle",
    }))
    .into_response()
}

/// GET /api/swarm — lightweight shared swarm status for future queen/worker logic.
pub(super) async fn get_swarm_status(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let miner = state.state_rx.borrow().clone();
    let hw = state
        .hardware_info
        .lock()
        .map(|g| g.clone())
        .unwrap_or_default();
    let hostname = std::fs::read_to_string("/etc/hostname")
        .unwrap_or_else(|_| "dcentos".to_string())
        .trim()
        .to_string();
    let mac = std::fs::read_to_string("/sys/class/net/eth0/address")
        .unwrap_or_else(|_| "00:00:00:00:00:00".to_string())
        .trim()
        .to_string();
    let ipv4 = eth0_ipv4();
    let live_power = state.power_rx.borrow().clone();
    let power_projection = project_power_telemetry(&live_power, &miner, &hw);
    let measured_wall_watts = measured_wall_watts_for_unprovenanced_surface(&power_projection);

    Json(swarm_status_payload(
        &state,
        &miner,
        &hw,
        &hostname,
        &mac,
        &ipv4,
        measured_wall_watts,
    ))
}

/// POST /api/swarm/room-temp — shared room temperature input for automation.
pub(super) async fn post_swarm_room_temp(
    State(state): State<Arc<AppState>>,
    Json(body): Json<SwarmRoomTempRequest>,
) -> impl IntoResponse {
    if body.temp_c < 0.0 || body.temp_c > 80.0 || body.temp_c.is_nan() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "status": "error",
                "message": "temp_c must be in range [0.0, 80.0]",
            })),
        )
            .into_response();
    }

    let temp_c10 = (body.temp_c * 10.0) as u32;
    state
        .room_temp_c10
        .store(temp_c10, std::sync::atomic::Ordering::Relaxed);
    push_rest_audit_free(
        &state,
        "room_temperature",
        format!(
            "Swarm room temperature updated: temp_c={:.1}, source={}",
            body.temp_c,
            body.source.as_deref().unwrap_or("unspecified")
        ),
    );

    Json(serde_json::json!({
        "status": "ok",
        "acceptedTempC": body.temp_c,
        "source": body.source,
        "ttlSupported": false,
        "message": "Swarm room temperature updated",
    }))
    .into_response()
}

/// GET /api/home/history -- 24h home mode history.
pub(super) async fn get_home_history(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    history_response(read_history_data(&state))
}

/// GET /api/home/night-mode -- Night mode configuration.
///
/// Reads the [mode.home.night_mode] section from the config file, with a
/// read-only fallback for legacy [home.night_mode].
/// Returns the current configuration including whether night mode
/// is currently active based on the system clock.
pub(super) async fn get_home_night_mode(State(_state): State<Arc<AppState>>) -> impl IntoResponse {
    let config_path = if std::path::Path::new("/data/dcentrald.toml").exists() {
        "/data/dcentrald.toml"
    } else {
        "/etc/dcentrald.toml"
    };

    // Try to read night mode config from TOML.
    let view = (|| {
        let contents = std::fs::read_to_string(config_path).ok()?;
        let table: toml::Table = toml::from_str(&contents).ok()?;
        Some(read_home_night_mode_from_table(&table))
    })()
    .unwrap_or(HomeNightModeView {
        enabled: false,
        start_hour: 22,
        end_hour: 7,
        max_fan_pwm: 30,
        power_reduction_pct: 40,
        schema_source: "defaults",
    });

    Json(serde_json::json!({
        "enabled": view.enabled,
        "start_hour": view.start_hour,
        "end_hour": view.end_hour,
        "max_fan_pwm": view.max_fan_pwm,
        "power_reduction_pct": view.power_reduction_pct,
        "active": false,
        "activeKnown": false,
        "activeSource": "unavailable_saved_only",
        "schemaSource": view.schema_source,
        "runtimeSource": "thermal.night_mode",
        "runtimeAdopted": false,
        "pendingRestart": false,
        "savedOnly": view.schema_source != "defaults",
    }))
}

/// POST /api/home/night-mode -- Update night mode configuration.
///
/// Writes night mode settings to the daemon-owned [mode.home.night_mode]
/// section of the config. The response deliberately reports saved-only truth:
/// the current live thermal loop enforces `thermal.night_mode`, not this saved
/// Home-mode schema.
pub(super) async fn post_home_night_mode(
    State(state): State<Arc<AppState>>,
    Json(body): Json<NightModeRequest>,
) -> Response {
    // CE-111: dedicated power-policy writer — gate on ConfigRw AND PowerControl
    // (night mode caps fan PWM + reduces mining power).
    if let Err(response) = require_antminer_runtime_capability(
        &state,
        RuntimeCapability::ConfigRw,
        "/api/home/night-mode",
    ) {
        return response;
    }
    if let Err(response) = require_antminer_runtime_capability(
        &state,
        RuntimeCapability::PowerControl,
        "/api/home/night-mode",
    ) {
        return response;
    }

    let start_hour = body.start_hour.unwrap_or(22);
    let end_hour = body.end_hour.unwrap_or(7);
    let max_fan_pwm = body
        .max_fan_pwm
        .unwrap_or(30)
        .min(dcentrald_hal::fan::PWM_SAFETY_MAX);
    let power_reduction_pct = body.power_reduction_pct.unwrap_or(40);

    tracing::info!(
        enabled = body.enabled,
        start = start_hour,
        end = end_hour,
        max_fan_pwm,
        power_reduction_pct,
        "Night mode configuration update"
    );

    // Validate hour ranges
    if start_hour > 23 || end_hour > 23 {
        return Json(serde_json::json!({
            "status": "error",
            "message": "Hours must be 0-23",
        }))
        .into_response();
    }

    // CFG-1/CFG-2: same hardened read-modify-write as the home power-target
    // handler — load the FULL effective config (baked `/etc` merged when `/data`
    // is absent), mutate ONLY `[mode.home.night_mode]`, write atomically to the
    // writable path. Prevents a fresh-install first touch from writing a
    // `[home]`-only file that shadows every other baked section. The dead
    // `if/else` (both arms identical) is removed.
    let config_path = get_writable_config_path();
    let write_result = (|| -> std::result::Result<(), String> {
        // RELIAB-2b: serialize load→modify→write (lost-update guard). Scoped to
        // this synchronous closure so it drops before any `.await`.
        let _cfg_write_guard = crate::atomic_io::config_write_lock();
        let mut table = load_config_table_for_write()?;

        if let Some(parent) = std::path::Path::new(config_path).parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("Failed to create config directory: {}", e))?;
        }

        apply_home_night_mode_to_table(
            &mut table,
            body.enabled,
            start_hour,
            end_hour,
            max_fan_pwm,
            power_reduction_pct,
        );

        let output =
            toml::to_string_pretty(&table).map_err(|e| format!("Serialize error: {}", e))?;
        atomic_write(config_path, output).map_err(|e| format!("Write error: {}", e))?;
        Ok(())
    })();

    match write_result {
        Ok(()) => {
            tracing::info!(
                "Night mode config saved to disk — thermal loop will apply on next tick"
            );
            Json(serde_json::json!({
                "status": "ok",
                "message": "Night mode configuration saved only; live thermal enforcement still uses thermal.night_mode",
                "enabled": body.enabled,
                "start_hour": start_hour,
                "end_hour": end_hour,
                "max_fan_pwm": max_fan_pwm,
                "power_reduction_pct": power_reduction_pct,
                "schemaSource": "mode.home.night_mode",
                "runtimeSource": "thermal.night_mode",
                "runtimeAdopted": false,
                "pendingRestart": false,
                "savedOnly": true,
            }))
            .into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "Failed to save night mode config");
            Json(serde_json::json!({
                "status": "error",
                "message": format!("Failed to save: {}", e),
            }))
            .into_response()
        }
    }
}

// ─── Debug Handlers (Hacker Mode Only) ─────────────────────────────────

/// GET /api/debug/registers -- Retired raw FPGA register compatibility route.
///
/// Hacker mode remains required, but the normal runtime never reads devmem.
pub(super) async fn get_debug_registers(
    State(state): State<Arc<AppState>>,
    Query(query): Query<RegisterQuery>,
) -> impl IntoResponse {
    let mode = *state.mode_rx.borrow();
    if let Err(resp) = crate::mode_middleware::check_mode_access("/api/debug/registers", mode) {
        return resp.into_response();
    }

    (
        StatusCode::NOT_IMPLEMENTED,
        Json(serde_json::json!({
            "status": "unavailable",
            "chain": query.chain,
            "offset": query.offset,
            "count": query.count,
            "hardware_access_attempted": false,
            "message": "Raw FPGA reads are unavailable until the runtime owner publishes a register snapshot.",
        })),
    )
        .into_response()
}

#[cfg(feature = "recovery-tool")]
async fn get_debug_registers_recovery(
    State(state): State<Arc<AppState>>,
    Query(query): Query<RegisterQuery>,
) -> impl IntoResponse {
    let mode = *state.mode_rx.borrow();
    if let Err(resp) = crate::mode_middleware::check_mode_access("/api/debug/registers", mode) {
        return resp.into_response();
    }

    // Read FPGA registers via devmem
    // Chain base addresses: 6=0x43C00000, 7=0x43C10000, 8=0x43C20000
    let base: u64 = match query.chain {
        6 => 0x43C00000,
        7 => 0x43C10000,
        8 => 0x43C20000,
        _ => {
            return Json(serde_json::json!({
                "error": "Invalid chain ID — must be 6, 7, or 8",
            }))
            .into_response();
        }
    };

    let offset = u64::from_str_radix(
        query
            .offset
            .trim_start_matches("0x")
            .trim_start_matches("0X"),
        16,
    )
    .unwrap_or(0);

    // Safety: only allow reads within the 4KB chain region
    if offset >= 0x1000 {
        return Json(serde_json::json!({
            "error": "Offset out of range — max 0xFFF within chain region",
        }))
        .into_response();
    }

    let count = query.count.unwrap_or(1).min(16); // max 16 registers at once

    // Run the blocking devmem shell-outs on a blocking thread under a timeout so a
    // stuck/contended bus can't park a tokio worker indefinitely (the AXI IIC
    // stuck-state is a documented hardware condition, and this is the very
    // diagnostic an operator uses to assess it — it must not hang the API).
    let read_fut = tokio::task::spawn_blocking(move || {
        let mut values = Vec::new();
        for i in 0..count as u64 {
            let addr = base + offset + (i * 4);
            let result = std::process::Command::new("devmem")
                .args([&format!("0x{:08X}", addr), "32"])
                .output()
                .ok()
                .and_then(|o| String::from_utf8(o.stdout).ok())
                .map(|s| s.trim().to_string())
                .unwrap_or_else(|| "read_error".to_string());
            values.push(serde_json::json!({
                "offset": format!("0x{:03X}", offset + (i * 4)),
                "addr": format!("0x{:08X}", addr),
                "value": result,
            }));
        }
        values
    });
    let values = match tokio::time::timeout(std::time::Duration::from_secs(3), read_fut).await {
        Ok(Ok(v)) => v,
        _ => {
            return Json(serde_json::json!({
                "chain": query.chain,
                "error": "bus did not respond within timeout",
                "status": "timeout",
            }))
            .into_response();
        }
    };

    Json(serde_json::json!({
        "chain": query.chain,
        "base": format!("0x{:08X}", base),
        "offset": format!("0x{:03X}", offset),
        "count": count,
        "values": values,
    }))
    .into_response()
}

/// POST /api/debug/registers -- Write FPGA register (requires confirm).
///
/// Hacker mode only. Requires `{ "confirm": true }`.
pub(super) async fn post_debug_registers(
    State(state): State<Arc<AppState>>,
    Json(body): Json<RegisterWriteRequest>,
) -> impl IntoResponse {
    if let Err(response) = require_antminer_runtime_capability(
        &state,
        RuntimeCapability::AsicOptions,
        "/api/debug/registers",
    ) {
        return response;
    }

    let mode = *state.mode_rx.borrow();
    if let Err(resp) = crate::mode_middleware::check_mode_access("/api/debug/registers", mode) {
        return resp.into_response();
    }

    let body_json = serde_json::to_value(&body).unwrap_or_default();
    if let Err(resp) = crate::mode_middleware::check_hacker_confirmation(&body_json) {
        return resp.into_response();
    }

    tracing::warn!(chain = body.chain, offset = %body.offset, value = %body.value, "Debug register write");

    (
        StatusCode::NOT_IMPLEMENTED,
        Json(serde_json::json!({
            "status": "not_implemented",
            "message": "Register write via HAL not yet wired",
        })),
    )
        .into_response()
}

/// GET /api/debug/i2c -- Retired raw I2C register read compatibility surface.
pub(super) async fn get_debug_i2c(
    State(state): State<Arc<AppState>>,
    Query(query): Query<I2cQuery>,
) -> impl IntoResponse {
    let mode = *state.mode_rx.borrow();
    if let Err(resp) = crate::mode_middleware::check_mode_access("/api/debug/i2c", mode) {
        return resp.into_response();
    }

    (
        StatusCode::NOT_IMPLEMENTED,
        Json(serde_json::json!({
            "status": "unavailable",
            "operation": "raw_i2c_read",
            "bus": query.bus,
            "addr": query.addr,
            "reg": query.reg,
            "hardware_access_attempted": false,
            "message": "Direct REST bus access is retired. dcentrald must publish a service-owned snapshot or provide a serialized typed command broker before this operation can be restored.",
        })),
    )
        .into_response()
}

/// POST /api/debug/i2c -- Raw I2C write.
pub(super) async fn post_debug_i2c(
    State(state): State<Arc<AppState>>,
    Json(body): Json<I2cWriteRequest>,
) -> impl IntoResponse {
    if let Err(response) = require_antminer_runtime_capability(
        &state,
        RuntimeCapability::AsicOptions,
        "/api/debug/i2c",
    ) {
        return response;
    }

    let mode = *state.mode_rx.borrow();
    if let Err(resp) = crate::mode_middleware::check_mode_access("/api/debug/i2c", mode) {
        return resp.into_response();
    }

    let body_json = serde_json::to_value(&body).unwrap_or_default();
    if let Err(resp) = crate::mode_middleware::check_hacker_confirmation(&body_json) {
        return resp.into_response();
    }

    tracing::warn!(bus = body.bus, addr = %body.addr, data_len = body.data.len(), "Debug I2C write");

    (
        StatusCode::NOT_IMPLEMENTED,
        Json(serde_json::json!({
            "status": "not_implemented",
            "message": "I2C write via HAL not yet wired",
        })),
    )
        .into_response()
}

/// POST /api/debug/asic-command -- Send raw ASIC command.
pub(super) async fn post_debug_asic_command(
    State(state): State<Arc<AppState>>,
    Json(body): Json<AsicCommandRequest>,
) -> impl IntoResponse {
    if let Err(response) = require_antminer_runtime_capability(
        &state,
        RuntimeCapability::AsicOptions,
        "/api/debug/asic-command",
    ) {
        return response;
    }

    let mode = *state.mode_rx.borrow();
    if let Err(resp) = crate::mode_middleware::check_mode_access("/api/debug/asic-command", mode) {
        return resp.into_response();
    }

    let body_json = serde_json::to_value(&body).unwrap_or_default();
    if let Err(resp) = crate::mode_middleware::check_hacker_confirmation(&body_json) {
        return resp.into_response();
    }

    tracing::warn!(chain = body.chain, command = %body.command, chip = ?body.chip, "Debug ASIC command");

    (
        StatusCode::NOT_IMPLEMENTED,
        Json(serde_json::json!({
            "status": "not_implemented",
            "chain": body.chain,
            "command": body.command,
            "response": [],
            "message": "ASIC command via protocol not yet wired",
        })),
    )
        .into_response()
}

/// GET /api/debug/pid-state -- Current PID controller state.
pub(super) async fn get_debug_pid_state(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let mode = *state.mode_rx.borrow();
    if let Err(resp) = crate::mode_middleware::check_mode_access("/api/debug/pid-state", mode) {
        return resp.into_response();
    }

    // Honest telemetry: report the REAL ThermalController PID state, or an
    // explicit "unavailable" — never fabricated kp/ki/kd/setpoint values
    // (the prior placeholder lied; this project's truth-contract forbids it).
    match state.pid_state_rx.as_ref().map(|rx| rx.borrow().clone()) {
        Some(Some(pid)) => Json(serde_json::json!({
            "available": true,
            "kp": pid.kp,
            "ki": pid.ki,
            "kd": pid.kd,
            "setpoint": pid.setpoint,
            "output": pid.output,
            "integral": pid.integral,
            "prev_error": pid.prev_error,
            "source": "dcentrald-thermal::ThermalController::pid_state (live)",
        }))
        .into_response(),
        Some(None) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({
                "available": false,
                "reason": "thermal controller has not produced a PID sample yet",
            })),
        )
            .into_response(),
        None => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({
                "available": false,
                "reason": "thermal PID telemetry not wired in this daemon path",
            })),
        )
            .into_response(),
    }
}

/// POST /api/debug/pid-params -- Update PID parameters.
pub(super) async fn post_debug_pid_params(
    State(state): State<Arc<AppState>>,
    Json(body): Json<PidParamsRequest>,
) -> impl IntoResponse {
    if let Err(response) = require_antminer_runtime_capability(
        &state,
        RuntimeCapability::ConfigRw,
        "/api/debug/pid-params",
    ) {
        return response;
    }

    let mode = *state.mode_rx.borrow();
    if let Err(resp) = crate::mode_middleware::check_mode_access("/api/debug/pid-params", mode) {
        return resp.into_response();
    }

    let body_json = serde_json::to_value(&body).unwrap_or_default();
    if let Err(resp) = crate::mode_middleware::check_hacker_confirmation(&body_json) {
        return resp.into_response();
    }

    tracing::warn!(kp = ?body.kp, ki = ?body.ki, kd = ?body.kd, "Debug PID params update");

    // P1 (expert-gated: ).
    // Real runtime thermal-PID tuning, safety-clamped. The thermal state
    // machine / fan caps / temp thresholds / EmergencyShutdown remain
    // independent of PID gains and fully enforced.
    let Some(tx) = state.pid_command_tx.as_ref() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({
                "applied": false,
                "reason": "this daemon path has no thermal loop (PID tuning unavailable)",
            })),
        )
            .into_response();
    };

    let current = state
        .pid_state_rx
        .as_ref()
        .and_then(|rx| rx.borrow().clone());

    fn resolve(req: Option<f64>, cur: Option<f32>) -> Option<f64> {
        req.or_else(|| cur.map(|v| v as f64))
    }
    let (kp, ki, kd) = (
        resolve(body.kp, current.as_ref().map(|c| c.kp)),
        resolve(body.ki, current.as_ref().map(|c| c.ki)),
        resolve(body.kd, current.as_ref().map(|c| c.kd)),
    );
    let (Some(kp), Some(ki), Some(kd)) = (kp, ki, kd) else {
        return (
            StatusCode::CONFLICT,
            Json(serde_json::json!({
                "applied": false,
                "reason": "partial update needs current PID state, which is not available yet — supply kp, ki, and kd explicitly",
            })),
        )
            .into_response();
    };

    for (name, v) in [("kp", kp), ("ki", ki), ("kd", kd)] {
        if !v.is_finite() || v < 0.0 {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "applied": false,
                    "reason": format!("{name} must be a finite, non-negative number"),
                })),
            )
                .into_response();
        }
    }
    // Binding safeguard: clamp each gain to [0.0, 50.0].
    let clamp = |v: f64| v.clamp(0.0, 50.0) as f32;
    let (kp, ki, kd) = (clamp(kp), clamp(ki), clamp(kd));

    match tx.try_send((kp, ki, kd)) {
        Ok(()) => Json(serde_json::json!({
            "applied": true,
            "kp": kp,
            "ki": ki,
            "kd": kd,
            "clamped_range": [0.0, 50.0],
            "note": "Applied on the next thermal tick. setpoint is governed by the thermal profile/target, not this endpoint. Thermal safety caps/thresholds are independent of PID gains and remain enforced.",
        }))
        .into_response(),
        Err(e) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({
                "applied": false,
                "reason": format!("thermal command channel unavailable: {e}"),
            })),
        )
            .into_response(),
    }
}

pub(super) const DEBUG_LOG_PATH: &str = "/tmp/dcentrald.log";
pub(super) const DEBUG_LOG_DEFAULT_LINES: usize = 100;
pub(super) const DEBUG_LOG_MAX_LINES: usize = 1000;
pub(super) const DEBUG_LOG_MAX_READ_BYTES: u64 = 256 * 1024;

#[derive(Debug)]
pub(super) struct BoundedLogTail {
    lines: Vec<String>,
    file_size_bytes: u64,
    read_bytes: u64,
    truncated: bool,
}

pub(super) fn read_bounded_log_tail(
    path: &Path,
    max_lines: usize,
    grep: Option<&str>,
    max_read_bytes: u64,
) -> std::io::Result<BoundedLogTail> {
    let mut file = std::fs::File::open(path)?;
    let file_size_bytes = file.metadata()?.len();
    let read_bytes = file_size_bytes.min(max_read_bytes);
    let start = file_size_bytes.saturating_sub(read_bytes);
    file.seek(SeekFrom::Start(start))?;

    let mut buf = Vec::with_capacity(read_bytes as usize);
    file.take(read_bytes).read_to_end(&mut buf)?;
    let text = String::from_utf8_lossy(&buf);
    let mut line_iter = text.lines();

    // If the read began in the middle of a line, drop that partial prefix.
    if start > 0 {
        let _ = line_iter.next();
    }

    let mut tail = VecDeque::with_capacity(max_lines.min(DEBUG_LOG_MAX_LINES));
    for line in line_iter {
        if max_lines == 0 {
            break;
        }
        if tail.len() == max_lines {
            let _ = tail.pop_front();
        }
        tail.push_back(line.to_string());
    }

    let lines = if let Some(pattern) = grep.filter(|pattern| !pattern.is_empty()) {
        tail.into_iter()
            .filter(|line| line.contains(pattern))
            .collect()
    } else {
        tail.into_iter().collect()
    };

    Ok(BoundedLogTail {
        lines,
        file_size_bytes,
        read_bytes,
        truncated: start > 0,
    })
}

/// GET /api/debug/log -- Read dcentrald log file.
///
/// Hacker mode only. Returns the last N lines from /tmp/dcentrald.log,
/// optionally filtered by a substring pattern. Reads only a bounded suffix of
/// the log file so large logs cannot be pulled fully into daemon memory.
///
/// Query parameters:
///   - lines: number of lines to return (default 100, max 1000)
///   - grep: plain-text substring filter (not regex)
pub(super) async fn get_debug_log(
    State(state): State<Arc<AppState>>,
    Query(query): Query<LogQuery>,
) -> impl IntoResponse {
    let mode = *state.mode_rx.borrow();
    if let Err(resp) = crate::mode_middleware::check_mode_access("/api/debug/log", mode) {
        return resp.into_response();
    }

    let requested_lines = query.lines.unwrap_or(DEBUG_LOG_DEFAULT_LINES);
    let max_lines = requested_lines.min(DEBUG_LOG_MAX_LINES);
    let grep = query.grep.clone();

    let tail_result = tokio::task::spawn_blocking(move || {
        read_bounded_log_tail(
            Path::new(DEBUG_LOG_PATH),
            max_lines,
            grep.as_deref(),
            DEBUG_LOG_MAX_READ_BYTES,
        )
    })
    .await;

    let tail = match tail_result {
        Ok(Ok(tail)) => tail,
        Ok(Err(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({
                    "error": "Log file not found",
                    "file": DEBUG_LOG_PATH,
                })),
            )
                .into_response();
        }
        Ok(Err(error)) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({
                    "error": "Failed to read log file",
                    "file": DEBUG_LOG_PATH,
                    "kind": error.kind().to_string(),
                })),
            )
                .into_response();
        }
        Err(error) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({
                    "error": "Log tail task failed",
                    "file": DEBUG_LOG_PATH,
                    "detail": error.to_string(),
                })),
            )
                .into_response();
        }
    };

    // W1.4/W3-2: pass tail lines through the log sanitizer when the
    // process-wide [logging] mask_logs gate is on (default true). This composes
    // wallet masking with key-name secret redaction so `/api/debug/log` matches
    // the stronger support-bundle scrub policy instead of wallet-only masking.
    //
    // `dcentrald_common::wallet_mask::mask_in_string` returns
    // `Cow::Borrowed` (zero allocation) when no match is found, which is
    // the case for the overwhelming majority of log lines. Hot path stays
    // cheap.
    let count = tail.lines.len();
    let mask_logs = dcentrald_common::mask_logs_enabled();
    let response_lines: Vec<String> = if mask_logs {
        tail.lines
            .iter()
            .map(|line| scrub_debug_log_line(line))
            .collect()
    } else {
        tail.lines.clone()
    };
    Json(serde_json::json!({
        "lines": response_lines,
        "count": count,
        "file": DEBUG_LOG_PATH,
        "requested_lines": requested_lines,
        "max_lines": DEBUG_LOG_MAX_LINES,
        "file_size_bytes": tail.file_size_bytes,
        "read_bytes": tail.read_bytes,
        "max_read_bytes": DEBUG_LOG_MAX_READ_BYTES,
        "truncated": tail.truncated,
        "wallet_mask_active": mask_logs,
    }))
    .into_response()
}

#[derive(Debug, Clone, Copy)]
pub(super) struct LogSourceSpec {
    id: &'static str,
    label: &'static str,
    path: &'static str,
    content_endpoint: Option<&'static str>,
    content_access: &'static str,
    limitations: &'static [&'static str],
}

#[derive(Debug, Serialize)]
pub(super) struct LogSourceManifestEntry {
    id: &'static str,
    label: &'static str,
    path: &'static str,
    content_endpoint: Option<&'static str>,
    content_access: &'static str,
    metadata_status: String,
    exists: bool,
    size_bytes: Option<u64>,
    modified_ms: Option<u128>,
    limitations: &'static [&'static str],
}

#[derive(Debug, Serialize)]
pub(super) struct LogManifestResponse {
    status: &'static str,
    read_only: bool,
    content_collected: bool,
    sources: Vec<LogSourceManifestEntry>,
    limitations: &'static [&'static str],
}

pub(super) const LOG_SOURCE_SPECS: &[LogSourceSpec] = &[
    LogSourceSpec {
        id: "dcentrald-runtime",
        label: "dcentrald runtime log",
        path: "/tmp/dcentrald.log",
        content_endpoint: Some("/api/debug/log?lines=200"),
        content_access: "mode_gated_content_endpoint",
        limitations: &[
            "Manifest checks metadata only.",
            "Log contents are exposed only through /api/debug/log in allowed modes.",
            "Content endpoint reads only a bounded suffix of the log file.",
        ],
    },
    LogSourceSpec {
        id: "dashboard-server",
        label: "dashboard server log",
        path: "/tmp/dashboard.log",
        content_endpoint: None,
        content_access: "not_exposed_metadata_only",
        limitations: &["Manifest checks metadata only; dashboard log contents are not exposed by this endpoint."],
    },
    LogSourceSpec {
        id: "system-messages",
        label: "system messages",
        path: "/var/log/messages",
        content_endpoint: None,
        content_access: "not_exposed_metadata_only",
        limitations: &["Manifest checks metadata only; syslog contents are not exposed by this endpoint."],
    },
    LogSourceSpec {
        id: "kernel-dmesg-file",
        label: "kernel message file",
        path: "/var/log/dmesg",
        content_endpoint: None,
        content_access: "not_exposed_metadata_only",
        limitations: &["Manifest checks metadata only; kernel log contents are not exposed by this endpoint."],
    },
];

pub(super) fn log_source_manifest_entry(spec: LogSourceSpec) -> LogSourceManifestEntry {
    match std::fs::metadata(spec.path) {
        Ok(metadata) => LogSourceManifestEntry {
            id: spec.id,
            label: spec.label,
            path: spec.path,
            content_endpoint: spec.content_endpoint,
            content_access: spec.content_access,
            metadata_status: "available".to_string(),
            exists: true,
            size_bytes: Some(metadata.len()),
            modified_ms: metadata.modified().ok().and_then(|modified| {
                modified
                    .duration_since(UNIX_EPOCH)
                    .ok()
                    .map(|duration| duration.as_millis())
            }),
            limitations: spec.limitations,
        },
        Err(error) => LogSourceManifestEntry {
            id: spec.id,
            label: spec.label,
            path: spec.path,
            content_endpoint: spec.content_endpoint,
            content_access: spec.content_access,
            metadata_status: if error.kind() == std::io::ErrorKind::NotFound {
                "missing".to_string()
            } else {
                format!("metadata_unavailable: {}", error.kind())
            },
            exists: false,
            size_bytes: None,
            modified_ms: None,
            limitations: spec.limitations,
        },
    }
}

pub(super) fn build_log_manifest_response() -> LogManifestResponse {
    LogManifestResponse {
        status: "ok",
        read_only: true,
        content_collected: false,
        sources: LOG_SOURCE_SPECS
            .iter()
            .copied()
            .map(log_source_manifest_entry)
            .collect(),
        limitations: &[
            "This endpoint never returns log lines.",
            "This endpoint never tails, greps, truncates, deletes, or rotates logs.",
            "Use the mode-gated bounded log content endpoint only when log contents are required.",
        ],
    }
}

/// GET /api/diagnostics/logs/manifest -- Metadata-only log source manifest.
///
/// Reports known real log sources and access status without reading log contents.
pub(super) async fn get_diagnostics_log_manifest() -> impl IntoResponse {
    Json(build_log_manifest_response())
}

/// POST /api/debug/chip/frequency -- Set per-chip frequency.
///
/// Hacker mode only. Allows per-chip frequency tuning for AutoTuner.
pub(super) async fn post_debug_chip_frequency(
    State(state): State<Arc<AppState>>,
    Json(body): Json<ChipFrequencyRequest>,
) -> impl IntoResponse {
    if let Err(response) = require_antminer_runtime_capability(
        &state,
        RuntimeCapability::AsicOptions,
        "/api/debug/chip/frequency",
    ) {
        return response;
    }

    let mode = *state.mode_rx.borrow();
    if let Err(resp) = crate::mode_middleware::check_mode_access("/api/debug/chip/frequency", mode)
    {
        return resp.into_response();
    }

    let body_json = serde_json::to_value(&body).unwrap_or_default();
    if let Err(resp) = crate::mode_middleware::check_hacker_confirmation(&body_json) {
        return resp.into_response();
    }

    tracing::warn!(
        chain = body.chain,
        chip = body.chip,
        freq_mhz = body.freq_mhz,
        "Debug per-chip frequency change"
    );

    (
        StatusCode::NOT_IMPLEMENTED,
        Json(serde_json::json!({
            "status": "not_implemented",
            "chain": body.chain,
            "chip": body.chip,
            "freq_mhz": body.freq_mhz,
            "message": "Per-chip frequency set via ChipDriver not yet wired",
        })),
    )
        .into_response()
}

/// POST /api/debug/chip/voltage -- Set per-chain voltage.
///
/// Hacker mode only. Directly controls PIC voltage via I2C.
/// WARNING: Incorrect voltage can damage hash boards permanently.
pub(super) async fn post_debug_chip_voltage(
    State(state): State<Arc<AppState>>,
    Json(body): Json<ChipVoltageRequest>,
) -> impl IntoResponse {
    if let Err(response) = require_antminer_runtime_capability(
        &state,
        RuntimeCapability::PowerControl,
        "/api/debug/chip/voltage",
    ) {
        return response;
    }

    let mode = *state.mode_rx.borrow();
    if let Err(resp) = crate::mode_middleware::check_mode_access("/api/debug/chip/voltage", mode) {
        return resp.into_response();
    }

    let body_json = serde_json::to_value(&body).unwrap_or_default();
    if let Err(resp) = crate::mode_middleware::check_hacker_confirmation(&body_json) {
        return resp.into_response();
    }

    let voltage_v = (1608.42 - body.pic_value as f64) / 170.42;

    tracing::warn!(
        chain = body.chain,
        pic_value = body.pic_value,
        voltage_v = format_args!("{:.3}", voltage_v),
        "Debug voltage change — HERE BE DRAGONS"
    );

    //  W1c — record the authenticated voltage-override request
    // even though no PIC write actually occurs (handler returns
    // NOT_IMPLEMENTED below). Both auth gates above already passed; the
    // operator-issued action is forensics-relevant.
    crate::push_audit_event(
        &state,
        "rest_attempt",
        dcentrald_api_types::audit_log::AuditEvent::VoltageOverrideAttempted {
            chain_id: body.chain,
            requested_voltage_v: voltage_v as f32,
        },
    );

    (
        StatusCode::NOT_IMPLEMENTED,
        Json(serde_json::json!({
            "status": "not_implemented",
            "chain": body.chain,
            "pic_value": body.pic_value,
            "estimated_voltage_v": voltage_v,
            "message": "Voltage set via PIC not yet wired",
            "warning": "Incorrect voltage values can permanently damage hash board ASICs",
        })),
    )
        .into_response()
}

/// POST /api/debug/psu/control -- Smart PSU runtime control.
///
/// Hacker mode only. Supports watchdog/voltage on Bitmain APW smart-PSU paths,
/// and output enable/disable on Amlogic GPIO-controlled platforms.
pub(super) async fn post_debug_psu_control(
    State(state): State<Arc<AppState>>,
    Json(body): Json<PsuControlRequest>,
) -> impl IntoResponse {
    if let Err(response) = require_antminer_runtime_capability(
        &state,
        RuntimeCapability::PowerControl,
        "/api/debug/psu/control",
    ) {
        return response;
    }

    let mode = *state.mode_rx.borrow();
    if let Err(resp) = crate::mode_middleware::check_mode_access("/api/debug/psu/control", mode) {
        return resp.into_response();
    }

    let body_json = serde_json::to_value(&body).unwrap_or_default();
    if let Err(resp) = crate::mode_middleware::check_hacker_confirmation(&body_json) {
        return resp.into_response();
    }

    push_rest_audit_free(
        &state,
        "psu",
        format!("Unavailable PSU control requested: action={}", body.action),
    );
    (
        StatusCode::NOT_IMPLEMENTED,
        Json(serde_json::json!({
            "status": "unavailable",
            "action": body.action,
            "hardware_access_attempted": false,
            "message": "Direct PSU control is unavailable until the runtime owner exposes a serialized command broker.",
        })),
    )
        .into_response()
}

#[cfg(feature = "recovery-tool")]
async fn post_debug_psu_control_recovery(
    State(state): State<Arc<AppState>>,
    Json(body): Json<PsuControlRequest>,
) -> impl IntoResponse {
    if let Err(response) = require_antminer_runtime_capability(
        &state,
        RuntimeCapability::PowerControl,
        "/api/debug/psu/control",
    ) {
        return response;
    }

    let mode = *state.mode_rx.borrow();
    if let Err(resp) = crate::mode_middleware::check_mode_access("/api/debug/psu/control", mode) {
        return resp.into_response();
    }

    let body_json = serde_json::to_value(&body).unwrap_or_default();
    if let Err(resp) = crate::mode_middleware::check_hacker_confirmation(&body_json) {
        return resp.into_response();
    }

    let hw = state
        .hardware_info
        .lock()
        .map(|g| g.clone())
        .unwrap_or_default();

    // W21 audit-coverage: PSU control is safety-critical (real enable/disable on
    // AML/Zynq am2 hardware). Record every accepted, gate-passed attempt.
    push_rest_audit_free(
        &state,
        "psu",
        format!("PSU control requested: action={}", body.action),
    );

    // Hold admission across the complete blocking hardware call. Teardown
    // closes this gate and drains every admitted lease before it observes
    // safe-off, so an in-flight Hacker request cannot re-enable a rail after
    // the engine has minted its GPIO-low evidence.
    let _hardware_mutation_lease = match state.hardware_mutation_gate.try_acquire() {
        Ok(lease) => lease,
        Err(error) => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({
                    "status": "error",
                    "action": body.action,
                    "message": "hardware mutation admission is closed for teardown",
                    "detail": error.to_string(),
                })),
            )
                .into_response();
        }
    };

    match body.action.as_str() {
        "enable_output" | "disable_output" if hw.control_board.starts_with("AML") => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({
                    "status": "error",
                    "action": body.action,
                    "hardware_access_attempted": false,
                    "message": "Amlogic PSU control requires the mining engine's retained power/thermal owner and terminal fence; this recovery route does not own either capability",
                })),
            )
                .into_response();
        }
        "enable_output" | "disable_output" if hw.control_board.starts_with("Zynq am2-s17") => {
            let result = if body.action == "enable_output" {
                dcentrald_hal::platform::zynq::enable_psu_output()
            } else {
                dcentrald_hal::platform::zynq::disable_psu_output()
            };

            return match result {
                Ok(()) => Json(serde_json::json!({
                    "status": "ok",
                    "action": body.action,
                    "control_mode": "gpio_enable",
                    "output_gate_enabled": dcentrald_hal::platform::zynq::is_psu_output_enabled(),
                    "output_enabled": null,
                    "voltage_out": null,
                    "telemetry_source": "unavailable_until_daemon_owned_psu_snapshot",
                    "hardware_bus_access_attempted": false,
                    "message": if body.action == "enable_output" {
                        "Zynq PSU output gate enabled"
                    } else {
                        "Zynq PSU output gate disabled"
                    },
                }))
                .into_response(),
                Err(e) => (
                    StatusCode::BAD_GATEWAY,
                    Json(serde_json::json!({
                        "status": "error",
                        "action": body.action,
                        "message": e.to_string(),
                    })),
                )
                    .into_response(),
            };
        }
        "enable_output" | "disable_output" => {
            return (
                StatusCode::NOT_IMPLEMENTED,
                Json(serde_json::json!({
                    "status": "not_implemented",
                    "action": body.action,
                    "message": "Direct PSU output gating is not wired on this platform. Smart APW control currently supports watchdog and voltage programming only.",
                })),
            )
                .into_response();
        }
        _ => {}
    }

    (
        StatusCode::NOT_IMPLEMENTED,
        Json(serde_json::json!({
            "status": "unavailable",
            "action": body.action,
            "hardware_bus_access_attempted": false,
            "message": "Smart PSU mutation is unavailable until the mining runtime exposes a typed, serialized control broker. REST must not open a second bus owner.",
        })),
    )
        .into_response()
}

// ─── Diagnostic Handlers ───────────────────────────────────────────────

/// POST /api/diagnostics/hashreport/start -- Start HashReport test.
///
/// Launches a 15-minute mining test that produces a per-chip health report.
/// Ideal for resellers verifying miner quality before sale.
/// Returns a job ID immediately; poll /status for progress.
pub(super) async fn post_diag_hashreport_start(
    State(state): State<Arc<AppState>>,
    Json(body): Json<DiagStartRequest>,
) -> impl IntoResponse {
    let requested_minutes = body.duration_minutes.unwrap_or(15).clamp(1, 60);
    let state_for_finalize = Arc::clone(&state);
    let chain = body.chain;
    let finalize = std::sync::Arc::new(move |test_id: Uuid, elapsed_s: u64| {
        let state_for_finalize = Arc::clone(&state_for_finalize);
        Box::pin(async move {
            build_timed_hashreport_result(&state_for_finalize, test_id, chain, elapsed_s).await
        }) as dcentrald_diagnostics::FinalizeTestFuture
    });

    let test_id = match state.diagnostic_service.lock().await.start_test(
        TestType::HashReport,
        DiagnosticJobConfig::HashReport(HashReportJobConfig {
            duration: std::time::Duration::from_secs(requested_minutes as u64 * 60),
            progress_interval: std::time::Duration::from_secs(5),
            finalize,
        }),
    ) {
        Ok(test_id) => test_id,
        Err(dcentrald_diagnostics::DiagnosticError::TestAlreadyRunning { test_type: _ }) => {
            return (
                StatusCode::CONFLICT,
                Json(serde_json::json!({
                    "status": "error",
                    "message": "A HashReport diagnostic job is already running.",
                })),
            )
                .into_response();
        }
        Err(error) => return report_storage_error_response(&error),
    };

    Json(serde_json::json!({
        "status": "started",
        "measurement_type": "timed_background_job",
        "test_type": "hashreport",
        "test_id": test_id,
        "requested_duration_minutes": requested_minutes,
        "phase": "queued",
        "progress_pct": 0,
        "report_available": false,
        "message": "HashReport timed background job started. Poll status or subscribe to diagnostic_progress WebSocket updates.",
    }))
    .into_response()
}

/// POST /api/diagnostics/hashreport/cancel -- Cancel a running timed HashReport job.
pub(super) async fn post_diag_hashreport_cancel(
    State(state): State<Arc<AppState>>,
    Json(body): Json<DiagCancelRequest>,
) -> impl IntoResponse {
    let test_id = match parse_test_id_or_response(&body.test_id) {
        Ok(test_id) => test_id,
        Err(response) => return response,
    };

    let mut service = state.diagnostic_service.lock().await;
    let job = match service.get_test_status(&test_id) {
        Some(job) => job,
        None => return report_not_found_response(&body.test_id),
    };

    if job.test_type != TestType::HashReport {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "status": "error",
                "message": "Only timed HashReport jobs can be cancelled through this endpoint.",
            })),
        )
            .into_response();
    }

    if job.status != TestStatus::Running {
        return (
            StatusCode::CONFLICT,
            Json(serde_json::json!({
                "status": "error",
                "test_id": body.test_id,
                "message": format!("HashReport job is already {}.", test_status_as_str(job.status)),
            })),
        )
            .into_response();
    }

    if service.cancel_test(&test_id) {
        Json(serde_json::json!({
            "status": "ok",
            "test_id": body.test_id,
            "message": "Timed HashReport cancellation requested. The running job will stop on its next control step.",
        }))
        .into_response()
    } else {
        report_not_found_response(&body.test_id)
    }
}

/// GET /api/diagnostics/hashreport/status -- HashReport test progress.
pub(super) async fn get_diag_hashreport_status(
    State(state): State<Arc<AppState>>,
    Query(query): Query<TestIdQuery>,
) -> impl IntoResponse {
    let test_id = match parse_test_id_or_response(&query.test_id) {
        Ok(test_id) => test_id,
        Err(response) => return response,
    };
    let job = match state
        .diagnostic_service
        .lock()
        .await
        .get_test_status(&test_id)
    {
        Some(job) => job,
        None => return report_not_found_response(&query.test_id),
    };
    let report_available = match snapshot_html_available(test_id).await {
        Ok(available) => available,
        Err(response) => return response,
    };
    Json(serde_json::json!({
        "test_id": query.test_id,
        "status": test_status_as_str(job.status),
        "progress_pct": job.progress_pct,
        "phase": job.phase_name,
        "detail": job.detail,
        "elapsed_s": job.elapsed_s,
        "measurement_type": "timed_background_job",
        "report_id": test_id,
        "started_at_epoch_s": job.started_at_epoch_s,
        "completed_at_epoch_s": job.completed_at_epoch_s,
        "duration_seconds": job.result.as_ref().map(|result| result.duration_s).unwrap_or(job.elapsed_s),
        "report_available": report_available,
        "message": match job.status {
            TestStatus::Running => "Timed HashReport job is still running.",
            TestStatus::Completed => "Timed HashReport job completed and report is available.",
            TestStatus::Failed => "Timed HashReport job failed.",
            TestStatus::Cancelled => "Timed HashReport job was cancelled.",
        },
    }))
    .into_response()
}

/// GET /api/diagnostics/hashreport/result -- HashReport test result.
pub(super) async fn get_diag_hashreport_result(
    State(state): State<Arc<AppState>>,
    Query(query): Query<TestIdQuery>,
) -> impl IntoResponse {
    let test_id = match parse_test_id_or_response(&query.test_id) {
        Ok(test_id) => test_id,
        Err(response) => return response,
    };
    let job = match state
        .diagnostic_service
        .lock()
        .await
        .get_test_status(&test_id)
    {
        Some(job) => job,
        None => return report_not_found_response(&query.test_id),
    };

    match job.status {
        TestStatus::Completed => match load_snapshot_artifact(test_id).await {
            Ok(report) => Json(report).into_response(),
            Err(response) => response,
        },
        TestStatus::Running => (
            StatusCode::ACCEPTED,
            Json(serde_json::json!({
                "status": "running",
                "test_id": query.test_id,
                "progress_pct": job.progress_pct,
                "phase": job.phase_name,
                "detail": job.detail,
                "elapsed_s": job.elapsed_s,
                "message": "Timed HashReport job is still running; no final result is available yet.",
            })),
        )
            .into_response(),
        TestStatus::Failed => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({
                "status": "failed",
                "test_id": query.test_id,
                "message": job.error.unwrap_or_else(|| "Timed HashReport job failed".to_string()),
            })),
        )
            .into_response(),
        TestStatus::Cancelled => (
            StatusCode::CONFLICT,
            Json(serde_json::json!({
                "status": "cancelled",
                "test_id": query.test_id,
                "message": "Timed HashReport job was cancelled before a final result was persisted.",
            })),
        )
            .into_response(),
    }
}

/// GET /api/diagnostics/hashreport/report -- Rendered HTML report.
pub(super) async fn get_diag_hashreport_report(
    Query(query): Query<ReportQuery>,
) -> impl IntoResponse {
    let test_id = match parse_test_id_or_response(&query.test_id) {
        Ok(test_id) => test_id,
        Err(response) => return response,
    };
    let format = query.format.unwrap_or_else(|| "html".to_string());
    if format.eq_ignore_ascii_case("json") {
        return match load_snapshot_artifact(test_id).await {
            Ok(report) => Json(report).into_response(),
            Err(response) => response,
        };
    }
    match load_snapshot_html(test_id).await {
        Ok(html) => Html(html).into_response(),
        Err(response) => response,
    }
}

/// POST /api/diagnostics/chip-health/start -- Start ChipHealth test.
pub(super) async fn post_diag_chiphealth_start(
    State(state): State<Arc<AppState>>,
    Json(body): Json<DiagStartRequest>,
) -> impl IntoResponse {
    let context = snapshot_context(&state);
    let report = build_chip_health_snapshot(&context, body.chain);
    let report_id = report.report_id;
    let (report, _) = match persist_snapshot_artifact(report_id, report, |generator, report| {
        generator.render_chip_health(report).map(Some)
    })
    .await
    {
        Ok(persisted) => persisted,
        Err(response) => return response,
    };
    Json(serde_json::json!({
        "status": "completed",
        "measurement_type": "snapshot",
        "test_type": "chip_health",
        "test_id": report.report_id,
        "message": "Chip-health snapshot generated from current runtime data.",
        "report": report,
        "report_available": true,
        "report_url": format!("/api/diagnostics/chip-health/report?test_id={}", report.report_id),
    }))
    .into_response()
}

/// GET /api/diagnostics/chip-health/status -- ChipHealth test progress.
pub(super) async fn get_diag_chiphealth_status(
    Query(query): Query<TestIdQuery>,
) -> impl IntoResponse {
    let test_id = match parse_test_id_or_response(&query.test_id) {
        Ok(test_id) => test_id,
        Err(response) => return response,
    };
    let (report, report_available) = match load_snapshot_artifact_with_html_status(test_id).await {
        Ok(report) => report,
        Err(response) => return response,
    };
    Json(serde_json::json!({
        "test_id": query.test_id,
        "status": "completed",
        "progress_pct": 100,
        "phase": "snapshot_ready",
        "measurement_type": "snapshot",
        "source": report.get("source").and_then(|value| value.as_str()).unwrap_or("stored_snapshot"),
        "generated_at": report.get("generated_at").and_then(|value| value.as_str()).unwrap_or_default(),
        "chains": report.get("chains").and_then(|value| value.as_array()).map(|chains| chains.len()).unwrap_or(0),
        "report_available": report_available,
        "report_url": format!("/api/diagnostics/chip-health/report?test_id={}", test_id),
        "message": "Stored chip-health snapshot is available for this completed test_id.",
    }))
    .into_response()
}

/// GET /api/diagnostics/chip-health/result -- ChipHealth test result.
pub(super) async fn get_diag_chiphealth_result(
    Query(query): Query<TestIdQuery>,
) -> impl IntoResponse {
    let test_id = match parse_test_id_or_response(&query.test_id) {
        Ok(test_id) => test_id,
        Err(response) => return response,
    };
    match load_snapshot_artifact_with_html_status(test_id).await {
        Ok((mut report, report_available)) => {
            if let Some(object) = report.as_object_mut() {
                object.insert("test_id".to_string(), serde_json::json!(query.test_id));
                object.insert(
                    "report_available".to_string(),
                    serde_json::json!(report_available),
                );
                object.insert(
                    "report_url".to_string(),
                    serde_json::json!(format!(
                        "/api/diagnostics/chip-health/report?test_id={}",
                        test_id
                    )),
                );
            }
            Json(report).into_response()
        }
        Err(response) => response,
    }
}

/// GET /api/diagnostics/chip-health/report -- Rendered HTML report.
pub(super) async fn get_diag_chiphealth_report(
    Query(query): Query<ReportQuery>,
) -> impl IntoResponse {
    let test_id = match parse_test_id_or_response(&query.test_id) {
        Ok(test_id) => test_id,
        Err(response) => return response,
    };
    let format = query.format.unwrap_or_else(|| "html".to_string());
    if format.eq_ignore_ascii_case("json") {
        return match load_snapshot_artifact(test_id).await {
            Ok(report) => Json(report).into_response(),
            Err(response) => response,
        };
    }
    match load_snapshot_html(test_id).await {
        Ok(html) => Html(html).into_response(),
        Err(response) => response,
    }
}

/// POST /api/diagnostics/board-health/start -- Start BoardHealth test.
pub(super) async fn post_diag_boardhealth_start(
    State(state): State<Arc<AppState>>,
    Json(body): Json<DiagStartRequest>,
) -> impl IntoResponse {
    let context = snapshot_context(&state);
    let report = build_board_health_snapshot(&context, body.chain);
    let report_id = context.report_id;
    let (report, _) = match persist_snapshot_artifact(report_id, report, |generator, report| {
        generator.render_board_health(report).map(Some)
    })
    .await
    {
        Ok(persisted) => persisted,
        Err(response) => return response,
    };
    Json(serde_json::json!({
        "status": "completed",
        "measurement_type": "snapshot",
        "test_type": "board_health",
        "test_id": context.report_id,
        "message": "Board-health snapshot generated from current runtime data.",
        "report": report,
        "report_available": true,
        "report_url": format!("/api/diagnostics/board-health/report?test_id={}", context.report_id),
    }))
    .into_response()
}

/// GET /api/diagnostics/board-health/status -- BoardHealth test progress.
pub(super) async fn get_diag_boardhealth_status(
    Query(query): Query<TestIdQuery>,
) -> impl IntoResponse {
    let test_id = match parse_test_id_or_response(&query.test_id) {
        Ok(test_id) => test_id,
        Err(response) => return response,
    };
    let (report, report_available) = match load_snapshot_artifact_with_html_status(test_id).await {
        Ok(report) => report,
        Err(response) => return response,
    };
    Json(serde_json::json!({
        "test_id": query.test_id,
        "status": "completed",
        "progress_pct": 100,
        "phase": "snapshot_ready",
        "measurement_type": "snapshot",
        "boards": report.as_array().map(|boards| boards.len()).unwrap_or(0),
        "report_available": report_available,
        "report_url": format!("/api/diagnostics/board-health/report?test_id={}", test_id),
        "message": "Stored board-health snapshot is available for this completed test_id.",
    }))
    .into_response()
}

/// GET /api/diagnostics/board-health/result -- BoardHealth test result.
pub(super) async fn get_diag_boardhealth_result(
    Query(query): Query<TestIdQuery>,
) -> impl IntoResponse {
    let test_id = match parse_test_id_or_response(&query.test_id) {
        Ok(test_id) => test_id,
        Err(response) => return response,
    };
    match load_snapshot_artifact_with_html_status(test_id).await {
        Ok((report, report_available)) => {
            let response = serde_json::json!({
                "test_id": query.test_id,
                "status": "completed",
                "message": "Stored board-health snapshot loaded.",
                "report_available": report_available,
                "report_url": format!("/api/diagnostics/board-health/report?test_id={}", test_id),
                "boards": report,
            });
            Json(response).into_response()
        }
        Err(response) => response,
    }
}

/// GET /api/diagnostics/board-health/report -- Rendered HTML report.
pub(super) async fn get_diag_boardhealth_report(
    Query(query): Query<ReportQuery>,
) -> impl IntoResponse {
    let test_id = match parse_test_id_or_response(&query.test_id) {
        Ok(test_id) => test_id,
        Err(response) => return response,
    };
    let format = query.format.unwrap_or_else(|| "html".to_string());
    if format.eq_ignore_ascii_case("json") {
        return match load_snapshot_artifact(test_id).await {
            Ok(report) => Json(report).into_response(),
            Err(response) => response,
        };
    }
    match load_snapshot_html(test_id).await {
        Ok(html) => Html(html).into_response(),
        Err(response) => response,
    }
}

/// GET /api/diagnostics/reports/recent -- Recent persisted diagnostic reports.
pub(super) async fn get_diag_recent_reports(
    Query(query): Query<RecentReportsQuery>,
) -> impl IntoResponse {
    let limit = query
        .limit
        .unwrap_or(10)
        .clamp(1, dcentrald_diagnostics::report::MAX_STORED_REPORTS);

    match list_snapshot_reports().await {
        Ok(mut reports) => {
            reports.truncate(limit);
            Json(serde_json::json!({
                "status": "ok",
                "reports": reports,
            }))
            .into_response()
        }
        Err(response) => response,
    }
}

/// GET /api/diagnostics/troubleshoot/network -- Network connectivity test.
///
/// Tests interface address discovery, gateway reachability, and DNS through a
/// zero-queue bounded subprocess owner. Pool connectivity remains an explicitly
/// labeled cached runtime observation; this endpoint does not claim an NTP or
/// live pool-connect probe.
pub(super) async fn get_diag_network(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let miner = state.state_rx.borrow().clone();
    let pool_host = diagnostic_pool_dns_host(&miner.pool.url);
    let probe = match run_network_diagnostic(NetworkProbeInput {
        dns_host: pool_host.clone(),
    })
    .await
    {
        Ok(probe) => probe,
        Err(NetworkProbeError::Busy) => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({
                    "status": ProbeStatus::Busy,
                    "message": "network diagnostic probe is busy; retry after the active bounded probe completes",
                })),
            )
                .into_response();
        }
        Err(NetworkProbeError::Cancelled) => {
            return (
                StatusCode::REQUEST_TIMEOUT,
                Json(serde_json::json!({
                    "status": ProbeStatus::Cancelled,
                    "message": "network diagnostic probe was cancelled after its active child was terminated",
                })),
            )
                .into_response();
        }
        Err(NetworkProbeError::Worker(error)) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({
                    "status": ProbeStatus::WorkerError,
                    "message": error,
                })),
            )
                .into_response();
        }
    };

    // Link metadata is intentionally after bounded-owner admission. A Busy
    // request therefore starts no Tokio filesystem work. The interface came
    // from the strictly validated default-route parser, not caller input.
    let (mac, carrier) = if let Some(interface) = probe.interface.as_deref() {
        const SYSFS_READ_DEADLINE: std::time::Duration = std::time::Duration::from_millis(250);
        let address_path = format!("/sys/class/net/{interface}/address");
        let carrier_path = format!("/sys/class/net/{interface}/carrier");
        let (mac, carrier) = tokio::join!(
            tokio::time::timeout(SYSFS_READ_DEADLINE, tokio::fs::read_to_string(address_path)),
            tokio::time::timeout(SYSFS_READ_DEADLINE, tokio::fs::read_to_string(carrier_path))
        );
        let mac = mac
            .ok()
            .and_then(|result| result.ok())
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| "unknown".to_string());
        let carrier = carrier
            .ok()
            .and_then(|result| result.ok())
            .map(|value| value.trim() == "1")
            .unwrap_or(false);
        (mac, carrier)
    } else {
        ("unknown".to_string(), false)
    };
    let cached_pool_connected = is_pool_connected(&miner.pool.status);

    Json(serde_json::json!({
        "status": "ok",
        "ethernet": {
            "mac": mac,
            "ip": probe.ip_cidr.clone().unwrap_or_else(|| "unknown".to_string()),
            "ip_address": probe.ip_address.clone().unwrap_or_else(|| "unknown".to_string()),
            "interface": probe.interface.clone().unwrap_or_else(|| "unknown".to_string()),
            "link_up": carrier,
            "gateway": probe.gateway.clone().unwrap_or_else(|| "unknown".to_string()),
        },
        "gateway_reachable": probe.gateway_reachable.unwrap_or(false),
        "dns_ok": probe.dns_ok.unwrap_or(false),
        "dns_test_host": pool_host,
        "probe": probe,
        // SEC (W20 / parity #66): strip inline stratum credentials.
        "pool_url": dcentrald_stratum::pool_api::sanitize_pool_url(&miner.pool.url),
        "pool_connected": cached_pool_connected,
        "pool_connectivity": {
            "connected": cached_pool_connected,
            "source": "cached_runtime_state",
            "live_probe_performed": false,
        },
        "ntp_probe": {
            "performed": false,
            "reason": "NTP probing is not implemented by this endpoint",
        },
    }))
    .into_response()
}

/// GET /api/diagnostics/troubleshoot/psu -- Daemon-owned PSU telemetry snapshot.
pub(super) async fn get_diag_psu(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let hw = state
        .hardware_info
        .lock()
        .map(|g| g.clone())
        .unwrap_or_default();
    let miner = state.state_rx.borrow().clone();
    let live_power = state.power_rx.borrow().clone();
    let projection = project_power_telemetry(&live_power, &miner, &hw);

    Json(serde_json::json!({
        "detected": hw
            .psu_model
            .as_deref()
            .is_some_and(|model| !model.trim().is_empty()),
        "model": hw.psu_model,
        "fw_version": hw.psu_fw_version,
        "transport": "daemon_snapshot",
        "control_mode": if hw.psu_override_active {
            "fixed_voltage_override"
        } else {
            "runtime_owned"
        },
        "voltage_range": hw.psu_voltage_range,
        "voltage_in": null,
        "voltage_out": null,
        "current_a": null,
        "power_w": projection.live_power_available.then_some(projection.wall_watts),
        "power_source": projection.source,
        "power_source_detail": projection.source_detail,
        "power_modeled": projection.modeled,
        "temp_c": null,
        "supports_output_gate": false,
        "supports_voltage_set": false,
        "supports_watchdog": false,
        "hardware_bus_access_attempted": false,
        "message": "This diagnostic consumes the mining runtime's published power snapshot and never opens a parallel PSU transport. Voltage/current/controller state remain unavailable until the runtime publishes them.",
    }))
}

/// GET /api/diagnostics/troubleshoot/fpga -- Daemon-owned chain snapshot.
///
/// Raw FPGA register fields remain unavailable in the normal runtime.
pub(super) async fn get_diag_fpga(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let miner = state.state_rx.borrow().clone();
    Json(serde_json::json!({
        "status": "snapshot_only",
        "source": "daemon_runtime_snapshot",
        "chains": miner.chains,
        "fans": miner.fans,
        "hardware_access_attempted": false,
        "unsupported_fields": [
            "raw_fpga_version",
            "raw_fpga_build_id",
            "raw_fpga_control_register",
            "raw_gpio_registers"
        ],
        "message": "Raw FPGA diagnostics are unavailable until the engine publishes a typed register snapshot.",
    }))
}

#[cfg(feature = "recovery-tool")]
async fn get_diag_fpga_recovery(State(_state): State<Arc<AppState>>) -> impl IntoResponse {
    // Read FPGA registers via devmem for each chain
    // Chain bases: 6=0x43C00000, 7=0x43C10000, 8=0x43C20000
    let chain_bases = [(6u8, 0x43C00000u64), (7, 0x43C10000), (8, 0x43C20000)];
    let mut chains = Vec::new();

    for (chain_id, base) in &chain_bases {
        // Read IP version register (offset 0x00)
        let version = std::process::Command::new("devmem")
            .args([&format!("0x{:08X}", base), "32"])
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .and_then(|s| {
                let s = s.trim().trim_start_matches("0x").trim_start_matches("0X");
                u32::from_str_radix(s, 16).ok()
            });

        // Read build ID register (offset 0x04)
        let build_id = std::process::Command::new("devmem")
            .args([&format!("0x{:08X}", base + 4), "32"])
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .and_then(|s| {
                let s = s.trim().trim_start_matches("0x").trim_start_matches("0X");
                u32::from_str_radix(s, 16).ok()
            });

        // Read CTRL_REG (offset 0x0C) for chain enable status
        let ctrl = std::process::Command::new("devmem")
            .args([&format!("0x{:08X}", base + 0x0C), "32"])
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .and_then(|s| {
                let s = s.trim().trim_start_matches("0x").trim_start_matches("0X");
                u32::from_str_radix(s, 16).ok()
            });

        let version_str = version.map(|v| {
            if v == 0x00901002 {
                "s9io v1.0.2 (OK)"
            } else {
                "Unknown"
            }
        });

        chains.push(serde_json::json!({
            "chain_id": chain_id,
            "base_addr": format!("0x{:08X}", base),
            "version": version.map(|v| format!("0x{:08X}", v)),
            "version_name": version_str,
            "build_id": build_id.map(|v| format!("0x{:08X}", v)),
            "ctrl_reg": ctrl.map(|v| format!("0x{:08X}", v)),
            "ok": version == Some(0x00901002),
        }));
    }

    // Read fan controller through the same HAL/UIO path as /api/fan. AM2
    // devmem reads can return a stale shadow and are not fan-control proof.
    let (fan_pwm, fan_status) = match read_fan_via_hal() {
        Ok((uio, variant, commanded_pwm, commanded_pwm0, commanded_pwm1, max_rpm)) => (
            Some(commanded_pwm.to_string()),
            serde_json::json!({
                "backend": "hal_uio",
                "uio": uio,
                "variant": format!("{:?}", variant),
                "commanded_pwm": commanded_pwm,
                "commanded_pwm0": commanded_pwm0,
                "commanded_pwm1": commanded_pwm1,
                "max_rpm": max_rpm,
            }),
        ),
        Err(error) => (
            None,
            serde_json::json!({
                "backend": "hal_uio",
                "error": error,
            }),
        ),
    };

    // Read GPIO registers
    let gpio_input = std::process::Command::new("devmem")
        .args(["0x41200000", "32"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string());

    let gpio_output = std::process::Command::new("devmem")
        .args(["0x41210000", "32"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string());

    Json(serde_json::json!({
        "chains": chains,
        "fan_commanded_pwm": fan_pwm,
        "fan_pwm_reg": fan_pwm, // Deprecated compatibility alias; value is HAL-commanded PWM.
        "fan": fan_status,
        "gpio_input_reg": gpio_input,
        "gpio_output_reg": gpio_output,
        "uio_count": 14,
    }))
}

/// GET /api/diagnostics/troubleshoot/asic-comm -- ASIC communication health.
///
/// Reports the live per-chain responding-chip count from the running
/// mining telemetry (the daemon continuously maintains chain
/// enumeration). Read-only by design: it does NOT issue a live
/// GetAddress broadcast — doing that from an HTTP request could disrupt
/// active mining; the continuously-tracked responding-chip count IS the
/// comm-health signal. (Replaces the prior "not yet wired" stub.)
pub(super) async fn get_diag_asic_comm(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let miner = state.state_rx.borrow().clone();
    let chains: Vec<serde_json::Value> = miner
        .chains
        .iter()
        .map(|c| {
            serde_json::json!({
                "chain_id": c.id,
                "responding_chips": c.chips,
                "comm_ok": c.chips > 0,
                "crc_errors": c.errors,
                "status": c.status,
            })
        })
        .collect();
    let total_responding: u32 = miner.chains.iter().map(|c| c.chips as u32).sum();
    let chains_with_comm = miner.chains.iter().filter(|c| c.chips > 0).count();

    Json(serde_json::json!({
        "schema": "diagnostics.asic_comm v1",
        "source": "live mining telemetry (state_rx); no live GetAddress broadcast issued",
        "chain_count": miner.chains.len(),
        "chains_with_comm": chains_with_comm,
        "total_responding_chips": total_responding,
        "chains": chains,
    }))
}

/// GET /api/diagnostics/troubleshoot/i2c-scan -- Retired scan compatibility surface.
pub(super) async fn get_diag_i2c_scan(State(_state): State<Arc<AppState>>) -> impl IntoResponse {
    (
        StatusCode::NOT_IMPLEMENTED,
        Json(serde_json::json!({
            "status": "unavailable",
            "operation": "i2c_topology_scan",
            "devices": [],
            "found_count": 0,
            "hardware_access_attempted": false,
            "message": "Live scans are retired because they bypass the daemon's serialized bus owner. Consume a daemon-owned topology snapshot when available.",
        })),
    )
}

// ───  W1: Failure-mode catalog (api-types) ──────────────────────

/// GET /api/diagnostics/failure_modes
///
/// Read-only catalog of every documented `dcentrald-api-types::FailureMode`
/// variant with its canonical severity + recovery action per `verdict()`.
/// Wired in  so dashboard / pyasic / fleet-management can consume
/// the canonical fault catalog from a single source of truth instead of
/// re-encoding it client-side.
pub(super) fn build_diag_failure_modes_response() -> serde_json::Value {
    use dcentrald_api_types::failure_mode::{verdict, ALL_FAILURE_MODES};

    let rows: Vec<serde_json::Value> = ALL_FAILURE_MODES
        .iter()
        .map(|m| {
            let v = verdict(*m);
            serde_json::json!({
                "mode": m,
                "severity": v.severity,
                "recovery": v.recovery,
            })
        })
        .collect();

    serde_json::json!({
        "schema": "dcentrald-api-types::failure_mode v1",
        "count": rows.len(),
        "modes": rows,
    })
}

pub(super) async fn get_diag_failure_modes() -> impl IntoResponse {
    Json(build_diag_failure_modes_response())
}

// ───  W4: Hashboard fault triage (api-types) ─────────────────────

/// Query parameters for `/api/diagnostics/chain`.
#[derive(Debug, Deserialize)]
pub struct ChainDiagQuery {
    pub id: u8,
}

/// GET /api/diagnostics/chain?id=N
///
/// Builds a `HashboardObservation` from the live `MinerState` snapshot for
/// chain `id` and returns the canonical `FaultVerdict` (fault class +
/// repair action) per `dcentrald-api-types::hashboard_diagnostics`. The
/// observation is read-only; this endpoint never resets, re-inits, or
/// otherwise touches the chain.
pub(super) async fn get_diag_chain(
    State(state): State<Arc<AppState>>,
    Query(q): Query<ChainDiagQuery>,
) -> impl IntoResponse {
    use dcentrald_api_types::hashboard_diagnostics::{FaultVerdict, HashboardObservation};

    let miner = state.state_rx.borrow().clone();
    let chain = match miner.chains.iter().find(|c| c.id == q.id) {
        Some(c) => c.clone(),
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({
                    "error": "chain_not_found",
                    "id": q.id,
                    "known_chain_ids": miner.chains.iter().map(|c| c.id).collect::<Vec<_>>(),
                })),
            )
                .into_response();
        }
    };

    // S9 = 63 BM1387 / chain, S19 Pro = 114 BM1398, S21 = 108 BM1368.
    // Use the firmware version string as a coarse hint; default to S9
    // (63) which is the .39 unit's chain length.
    let chips_expected: u32 =
        if miner.firmware_version.contains("s19") || miner.firmware_version.contains("S19") {
            114
        } else if miner.firmware_version.contains("s21") || miner.firmware_version.contains("S21") {
            108
        } else {
            63
        };

    // "nonces returning" = chain reports any hashrate at all on the
    // current 5-second average. A chain that enumerated chips but is
    // returning 0 GH/s is the canonical NoncesNotReturning case.
    let nonces_returning = chain.hashrate_ghs > 0.0;

    let observation = HashboardObservation {
        chips_detected: chain.chips as u32,
        chips_expected,
        nonces_returning,
    };
    let fault = observation.classify_chain();
    let v = FaultVerdict::for_fault(fault);

    Json(serde_json::json!({
        "schema": "dcentrald-api-types::hashboard_diagnostics v1",
        "id": q.id,
        "observation": {
            "chips_detected": observation.chips_detected,
            "chips_expected": observation.chips_expected,
            "nonces_returning": observation.nonces_returning,
        },
        "verdict": v.fault,
        "repair_action": v.repair,
        "break_point_chip_idx": observation.break_point_chip_idx(),
    }))
    .into_response()
}

// ───  W1: Share-validation local rejects (api-types) ─────────────

/// Query parameters for `/api/diagnostics/shares/local_rejects`.
#[derive(Debug, Deserialize)]
pub struct LocalRejectsQuery {
    /// How many of the most recent rejects to return. Capped at the
    /// ring's actual capacity (default 64). Defaults to ring capacity.
    pub limit: Option<usize>,
}

/// GET /api/diagnostics/shares/local_rejects?limit=N
///
/// Returns the most recent local share-validation rejects captured in
/// `AppState::local_reject_ring`. Each entry carries the per-chain /
/// per-chip nonce, the decoded work_id and midstate_idx, the FPGA-raw
/// `hw_work_id`, the work-table generation age at reject time, and the
/// big-endian first 8 bytes of the computed SHA-256d hash and pool
/// share_target. Together those let an operator distinguish:
///
/// - **HashAboveTarget close-margin** (`computed_hash_be_first8` near
///   `share_target_be_first8`): mining is healthy, just unlucky.
/// - **HashAboveTarget far-margin**: midstate corruption likely (8-bit
///   FPGA work_id ring wraparound aliasing).
/// - **StaleSlot / EmptyWorkSlot**: dispatcher work-table churn.
/// - **DuplicateDedupHit**: same nonce submitted twice.
/// - **MidstateIdxOutOfRange**: FPGA decode bug.
///
/// Wired in  to drive  share-validation root-cause
/// analysis on the live S9 at 203.0.113.39.
pub(super) async fn get_diag_share_local_rejects(
    State(state): State<Arc<AppState>>,
    Query(q): Query<LocalRejectsQuery>,
) -> impl IntoResponse {
    let ring_arc = state.local_reject_ring.clone();
    let (rejects, total_seen, capacity) = match ring_arc.lock() {
        Ok(ring) => {
            let cap = ring.capacity();
            let limit = q.limit.unwrap_or(cap).min(cap);
            (ring.snapshot(limit), ring.total_seen(), cap)
        }
        Err(_) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({
                    "error": "local_reject_ring_poisoned",
                })),
            )
                .into_response();
        }
    };

    Json(serde_json::json!({
        "schema": "dcentrald-api-types::share_validation::LocalRejectRing v1",
        "ring_capacity": capacity,
        "total_seen": total_seen,
        "returned": rejects.len(),
        "rejects": rejects,
    }))
    .into_response()
}

// ───  W3: PIC firmware catalog (api-types) ───────────────────────

/// GET /api/hardware/pic_info
///
/// Returns the canonical PIC firmware variant catalog from
/// `dcentrald-api-types::pic_firmware`: every documented PIC16F1704 +
/// dsPIC33EP16GS202 firmware byte we have RE evidence for, with the
/// per-variant wire form, RESET safety flag, voltage-trust flag, and
/// human-readable label.
///
///  ships the static catalog only. A future wave can extend this
/// to include the live per-slot firmware byte from
/// `dcentrald-asic::pic` once the PicService handle is reachable from
/// `AppState`.
pub(super) async fn get_hardware_pic_info() -> impl IntoResponse {
    Json(
        dcentrald_api_types::pic_firmware::PicFirmwareInfoResponse::catalog_only_without_live_service(),
    )
}

// ───  W4: LuxOS recovery actions catalog (api-types) ─────────────

/// GET /api/hardware/thermal/bm1368/chip_temps
///
/// Returns the read-only BM1368/S21 per-chip temperature response contract.
/// Until a daemon-owned target-read publisher is wired, BM1368 miners report
/// `not_proven`; other chip families report `unsupported`.
pub(super) async fn get_hardware_bm1368_chip_temps(
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    use dcentrald_api_types::bm1368_temperature::{
        is_bm1368_chip_type, Bm1368ChainTemperatureInput, Bm1368ChipTemperatureResponse,
        BM1368_S21_CHIPS_PER_CHAIN,
    };

    let hardware = state
        .hardware_info
        .lock()
        .map(|guard| guard.clone())
        .unwrap_or_default();
    let miner = state.state_rx.borrow().clone();
    let generated_at_ms = unix_now_ms();
    let chip_type = if hardware.chip_type.trim().is_empty() {
        "unknown".to_string()
    } else {
        hardware.chip_type.clone()
    };
    let model = hardware.hb_type.clone().unwrap_or_else(|| {
        if chip_type == "unknown" {
            "unknown".to_string()
        } else {
            format!("Antminer ({chip_type})")
        }
    });

    if !is_bm1368_chip_type(&chip_type) {
        return Json(Bm1368ChipTemperatureResponse::unsupported(
            chip_type,
            model,
            generated_at_ms,
        ))
        .into_response();
    }

    let mut chains: Vec<Bm1368ChainTemperatureInput> = miner
        .chains
        .iter()
        .map(|chain| Bm1368ChainTemperatureInput {
            chain_id: chain.id,
            chip_count: if chain.chips > 0 {
                chain.chips as u16
            } else {
                BM1368_S21_CHIPS_PER_CHAIN
            },
            board_temp_c: chain
                .temp_c
                .is_finite()
                .then_some(chain.temp_c)
                .filter(|temp_c| *temp_c > 0.0),
        })
        .collect();

    if chains.is_empty() {
        chains.push(Bm1368ChainTemperatureInput {
            chain_id: 0,
            chip_count: BM1368_S21_CHIPS_PER_CHAIN,
            board_temp_c: None,
        });
    }

    Json(Bm1368ChipTemperatureResponse::not_proven_for_bm1368(
        chip_type,
        model,
        generated_at_ms,
        chains,
    ))
    .into_response()
}

/// GET /api/diagnostics/recovery_actions
///
/// Read-only LuxOS recovery vocabulary: 5 recovery actions, the 4 CGI
/// recovery routes with their canonical paths, the 5-group log
/// whitelist, and the 6-step UNINSTALL_SH canonical ordering. Pure RE
/// translation — operator-facing reference. No runtime state read.
pub(super) async fn get_diag_recovery_actions() -> impl IntoResponse {
    use dcentrald_api_types::luxos_recovery::{
        LuxosLogGroup, LuxosRecoveryAction, LuxosRecoveryCgi, UNINSTALL_SH_STEPS,
    };

    let actions: Vec<serde_json::Value> = [
        LuxosRecoveryAction::Reboot,
        LuxosRecoveryAction::Uninstall,
        LuxosRecoveryAction::DownloadCgminerConf,
        LuxosRecoveryAction::DownloadLuxminerToml,
        LuxosRecoveryAction::BrowseLogs,
    ]
    .iter()
    .map(|a| {
        serde_json::json!({
            "action": a,
            "is_destructive": a.is_destructive(),
        })
    })
    .collect();

    let cgi_routes: Vec<serde_json::Value> = [
        LuxosRecoveryCgi::Reboot,
        LuxosRecoveryCgi::Uninstall,
        LuxosRecoveryCgi::GetLogs,
        LuxosRecoveryCgi::DownloadFile,
    ]
    .iter()
    .map(|c| {
        serde_json::json!({
            "cgi": c,
            "path": c.path(),
        })
    })
    .collect();

    let log_groups: Vec<LuxosLogGroup> = vec![
        LuxosLogGroup::RamdiskLogs,
        LuxosLogGroup::SdcardLogs,
        LuxosLogGroup::SdcardLogs2,
        LuxosLogGroup::NandLogs,
        LuxosLogGroup::Config,
    ];

    Json(serde_json::json!({
        "schema": "dcentrald-api-types::luxos_recovery v1",
        "actions": actions,
        "cgi_routes": cgi_routes,
        "log_groups_whitelist": log_groups,
        "uninstall_steps": UNINSTALL_SH_STEPS,
        "luxos_recovery_requires_auth": dcentrald_api_types::luxos_recovery::LUXOS_RECOVERY_REQUIRES_AUTH,
        "note": "DCENT_OS recovery surface is read-only by default — these routes are documented for parity reference, not exposed as live actions.",
    }))
}

// ───  W5: Boot timeline observability (api-types) ─────────────────

/// GET /api/system/boot_timeline
///
/// Returns the canonical `DCENT_OS_TIMELINE` (per-phase milestone
/// schedule from `firmware_boot_timeline.rs`) plus the runtime-observed
/// timestamps for each phase actually reached so far. Operators can see
/// where their unit got stuck if it never reaches `FirstShareAccepted`.
///
///  ships the static timeline only. The runtime-observed phase
/// timestamps will be populated by the W5 BootProgressTracker once the
/// daemon wires it into the existing `tracing::info!` boot phase sites.
pub(super) async fn get_system_boot_timeline(
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    use dcentrald_api_types::firmware_boot_timeline::{timeline_of, FirmwareBootFamily};

    let timeline = timeline_of(FirmwareBootFamily::DcentOs);
    let canonical: Vec<serde_json::Value> = timeline
        .iter()
        .map(|m| {
            serde_json::json!({
                "phase": m.phase,
                "at_seconds": m.at_seconds,
                "description": m.description,
            })
        })
        .collect();

    let observed = state.boot_progress.snapshot();
    Json(serde_json::json!({
        "schema": "dcentrald-api-types::firmware_boot_timeline::DCENT_OS_TIMELINE v1",
        "family": "dcent_os",
        "canonical": canonical,
        "observed": observed,
    }))
}

// ───  W2: Audit-log ring snapshot (api-types) ────────────────────

/// Query parameters for `/api/history/audit`.
#[derive(Debug, Deserialize)]
pub struct AuditHistoryQuery {
    /// How many of the most recent records to return. Capped at the
    /// ring's actual capacity (default 256). Defaults to 64.
    pub limit: Option<usize>,
}

/// GET /api/history/audit?limit=N
///
/// Returns the most recent audit records held in
/// `AppState::audit_ring`.  ships the ring infrastructure + this
/// read endpoint; the daemon-side push integration (mode change /
/// pool switch / voltage override / sysupgrade events) is queued for
/// . The persistent NDJSON log at `/data/audit.log` (handled
/// outside the ring) remains the long-term forensics artifact.
pub(super) async fn get_history_audit(
    State(state): State<Arc<AppState>>,
    Query(q): Query<AuditHistoryQuery>,
) -> impl IntoResponse {
    let ring_arc = state.audit_ring.clone();
    let (mut records, total_seen, capacity) = match ring_arc.lock() {
        Ok(ring) => {
            let cap = ring.capacity();
            let limit = q.limit.unwrap_or(64).min(cap);
            (ring.snapshot(limit), ring.total_seen(), cap)
        }
        Err(_) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({
                    "error": "audit_ring_poisoned",
                })),
            )
                .into_response();
        }
    };

    // SEC (W20 / C-4): defense-in-depth redaction — match the persistent
    // /api/audit-log reader so a PoolSwitch event carrying an inline-credential
    // URL (or a Free event) can't leak through the in-memory ring endpoint.
    for rec in records.iter_mut() {
        crate::routes::audit_log::redact_record(rec);
    }

    Json(serde_json::json!({
        "schema": "dcentrald-api-types::audit_log::AuditRing v1",
        "ring_capacity": capacity,
        "total_seen": total_seen,
        "returned": records.len(),
        "events": records,
    }))
    .into_response()
}

// ───  W3: PSU model catalog (api-types) ──────────────────────────

/// GET /api/hardware/psu_catalog
///
/// Read-only APW PSU family catalog from
/// `dcentrald-api-types::psu_model::ALL_MODELS`. Each entry includes the
/// voltage range, max wattage @ 110/220 V, AC input range, efficiency,
/// `has_voltage_feedback` flag (per
/// ), and the human-readable
/// label + compatible miner families.
///
/// Helps operators decide PSU replacements (e.g. "APW121215a can't be
/// swapped for APW121215d/e/f without firmware update").
pub(super) async fn get_hardware_psu_catalog() -> impl IntoResponse {
    use dcentrald_api_types::psu_model::ALL_MODELS;
    let rows: Vec<serde_json::Value> = ALL_MODELS
        .iter()
        .map(|m| {
            let s = m.spec();
            serde_json::json!({
                "model": m,
                "voltage_min_v": s.voltage_min_v,
                "voltage_max_v": s.voltage_max_v,
                "max_current_a": s.max_current_a,
                "max_wattage_220v_w": s.max_wattage_220v_w,
                "max_wattage_110v_w": s.max_wattage_110v_w,
                "ac_input_min_v": s.ac_input_min_v,
                "ac_input_max_v": s.ac_input_max_v,
                "efficiency_pct": s.efficiency_pct,
                "has_voltage_feedback": s.has_voltage_feedback,
                "label": s.label,
                "compatible_miners": s.compatible_miners,
            })
        })
        .collect();
    Json(serde_json::json!({
        "schema": "dcentrald-api-types::psu_model v1",
        "count": rows.len(),
        "models": rows,
    }))
}

// ───  W4: cgminer command catalog (api-types) ────────────────────

/// GET /api/cgminer/catalog
///
/// Read-only catalog of all 78 CGMiner-API commands from
/// `dcentrald-api-types::cgminer_catalog::CGMINER_CATALOG`. Each entry
/// includes the wire-protocol command name, kind (`Set`/`Get`), Luxor-
/// extension flag, destructive flag (commands that can brick/downgrade —
/// dashboard requires explicit confirmation), and a one-line doc
/// suitable for tooltip rendering.
///
/// Eliminates the dashboard's hardcoded command list (per
///  — the source of 14/24
/// stale ACH name drift in a prior wave).
pub(super) async fn get_cgminer_catalog() -> impl IntoResponse {
    use dcentrald_api_types::cgminer_catalog::{catalog_stats, CGMINER_CATALOG};
    let rows: Vec<serde_json::Value> = CGMINER_CATALOG
        .iter()
        .map(|c| {
            serde_json::json!({
                "name": c.name,
                "kind": c.kind,
                "luxor_extension": c.luxor_extension,
                "destructive": c.destructive,
                "doc": c.doc,
            })
        })
        .collect();
    let stats = catalog_stats();
    Json(serde_json::json!({
        "schema": "dcentrald-api-types::cgminer_catalog v1",
        "count": rows.len(),
        "total": stats.total,
        "set_count": stats.set_count,
        "get_count": stats.get_count,
        "luxor_extensions": stats.luxor_extensions,
        "destructive": stats.destructive,
        "commands": rows,
    }))
}

// ───  W2: power-profile preset catalog (api-types) ──────────────

/// GET /api/profiles/presets
///
/// Read-only catalog of per-model power presets from
/// `dcentrald-api-types::power_profile_preset::ALL_MODELS`. Each model
/// row carries its preset table (watt / hashrate / J-per-TH triples)
/// plus the most-efficient ("sweet spot") preset, computed by
/// `MinerModel::sweet_spot()`. The S19k Pro entry intentionally returns
/// `presets: []` and `sweet_spot: null` until a live capture lands
/// (BM1366 catalog deferred to + — see the api-types module
/// header).
///
/// Eliminates dashboard hardcoded preset rows per
/// .
pub(super) async fn get_profile_presets() -> impl IntoResponse {
    use dcentrald_api_types::power_profile_preset::{PowerProfile, ALL_MODELS};

    fn preset_row(p: &PowerProfile) -> serde_json::Value {
        serde_json::json!({
            "watts": p.watts,
            "hashrate_th": p.hashrate_th,
            "j_per_th": p.j_per_th,
        })
    }

    let mut total_presets: usize = 0;
    let models: Vec<serde_json::Value> = ALL_MODELS
        .iter()
        .map(|m| {
            let presets = m.presets();
            total_presets += presets.len();
            let preset_rows: Vec<serde_json::Value> = presets.iter().map(preset_row).collect();
            let sweet_spot = m.sweet_spot().map(preset_row);
            serde_json::json!({
                "model": m,
                "chip_family": m.chip_family(),
                "preset_count": presets.len(),
                "presets": preset_rows,
                "sweet_spot": sweet_spot,
            })
        })
        .collect();

    Json(serde_json::json!({
        "schema": "dcentrald-api-types::power_profile_preset v1",
        "model_count": models.len(),
        "total_presets": total_presets,
        "models": models,
    }))
}

// ───  W3: thermal-sensor topology catalog (api-types) ───────────

/// GET /api/hardware/thermal/sensors
///
/// Read-only catalog from
/// `dcentrald-api-types::luxos_sensor_topology`. Returns the canonical
/// 4-corner board sensor map (TopLeft / TopRight / BottomLeft /
/// BottomRight), the 3-tier threshold defaults
/// (`target_c` / `hot_c` / `panic_c`), and the 4-verdict mapping that
/// `classify_temperature()` produces (`Mine` / `Hold` / `ThrottlePower`
/// / `EmergencyShutdown`).
///
/// CATALOG ONLY — this endpoint does NOT change live thermal thresholds.
/// The running thermal controller has its own config.
pub(super) async fn get_hardware_thermal_sensors() -> impl IntoResponse {
    use dcentrald_api_types::luxos_sensor_topology::{
        classify_temperature, LuxosBoardSensorMap, LuxosThresholdConfig,
    };

    let positions = LuxosBoardSensorMap::S19J_PRO_4_CORNER.positions.to_vec();
    let thresholds = LuxosThresholdConfig::default();

    // Probe classify_temperature at four boundary points to surface
    // the 4 verdicts. The action labels are serde-snake-case stable.
    let sample = |c: f32| {
        let action = classify_temperature(&thresholds, c);
        serde_json::json!({
            "sample_c": c,
            "verdict": action,
        })
    };

    Json(serde_json::json!({
        "schema": "dcentrald-api-types::luxos_sensor_topology v1",
        "note": "catalog only — does NOT change live thermal thresholds",
        "sensor_positions": positions,
        "thresholds": {
            "target_c": thresholds.target_c,
            "hot_c": thresholds.hot_c,
            "panic_c": thresholds.panic_c,
        },
        "verdicts": [
            sample(thresholds.target_c - 5.0),
            sample(thresholds.target_c + 1.0),
            sample(thresholds.hot_c + 1.0),
            sample(thresholds.panic_c + 1.0),
        ],
    }))
}

// ─── : state-machine policy catalog (api-types, read-only) ───────

/// GET /api/diagnostics/state_machine
///
/// Read-only catalog of the canonical watchdog / power bring-up / mining
/// loop state-machine *policy* (thresholds + gates) DCENT_OS uses. This
/// is an operator-transparency surface — no competitor firmware
/// (BraiinsOS / VNish / LuxOS / Stock) exposes its watchdog, PSU
/// bring-up gate, or stall thresholds honestly. It reflects no live FSM
/// state and issues zero hardware I/O. Backed by
/// `dcentrald-api-types::{watchdog_policy, power_state,
/// mining_loop_state}`.
pub(super) async fn get_diagnostics_state_machine() -> impl IntoResponse {
    use dcentrald_api_types::mining_loop_state::MiningLoopConfig;
    use dcentrald_api_types::power_state::PowerStateConfig;
    use dcentrald_api_types::watchdog_policy::WatchdogPolicy;

    let wd = WatchdogPolicy::bosminer_canonical();
    let psc = PowerStateConfig::default();
    let mlc = MiningLoopConfig::default();

    Json(serde_json::json!({
        "schema": "dcentrald-api-types::state_machine v1",
        "read_only": true,
        "note": "Canonical policy thresholds only — not live FSM state; no hardware I/O.",
        "watchdog": serde_json::to_value(wd).unwrap_or(serde_json::Value::Null),
        "power_bringup": {
            "stable_tick_gate": psc.stable_tick_gate,
            "psu_disarm_triple_write_count": psc.psu_disarm_triple_write_count,
            "refuse_fw_86": psc.refuse_fw_86,
            "_doc": "stable_tick_gate is hard-mandated at 5 (no SET_VOLTAGE before 5 stable PIC ticks); refuse_fw_86 default-true (DCENT_AM2_TRUST_DEGRADED_FW=1 is a lab-only override).",
        },
        "mining_loop": serde_json::to_value(mlc).unwrap_or(serde_json::Value::Null),
        "watchdog_ops": ["psu_arm_disarm", "psu_heartbeat", "dspic_heartbeat"],
    }))
}

// ─── : OTA update-capability transparency (api-types + ota_signature) ──

/// GET /api/system/update_capability
///
/// Read-only transparency contract for DCENT_OS firmware-update +
/// rollback integrity, with the reverse-engineered LuxOS comparison.
/// DCENT_OS enforces an ed25519-signed manifest, fail-closed OTA
/// (`crate::ota_signature`); the RE'd LuxOS path is MD5-only with NO
/// cryptographic signature
/// (`dcentrald-api-types::luxos_update::LuxosUpdateIntegrity::Md5Only`).
/// Issues no update/flash/rollback action and no hardware I/O.
pub(super) async fn get_system_update_capability(
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    use dcentrald_api_types::luxos_update::LUXOS_DEFAULT_CHANNEL_URL;

    let miner = state.state_rx.borrow().clone();
    let hw = state
        .hardware_info
        .lock()
        .map(|guard| guard.clone())
        .unwrap_or_default();
    let board_target = antminer_board_target(&hw);
    let board_desc = antminer_board_desc(&hw);
    let descriptor = build_antminer_capability_descriptor(&miner, &hw);
    let update_authorized = descriptor
        .runtime_caps
        .contains(&RuntimeCapability::FlashOta);
    let ab_slot_rollback = update_authorized
        && board_desc
            .map(|desc| {
                desc.enablement.storage_topology
                    == dcentrald_common::StorageTopology::RedundantSlots
            })
            .unwrap_or(false);

    Json(serde_json::json!({
        "schema": "dcentrald-api-types::update_capability v1",
        "read_only": true,
        "note": "Transparency contract only — issues no update/flash/rollback action and no hardware I/O.",
        "dcentos": {
            // WAVE 0 STABILIZE (2026-06-05) — OTA honesty: report the real
            // signature posture from the trust anchors that exist at runtime.
            // "enforced" => a compile-time pin and/or the on-disk
            // /etc/dcentos/release_ed25519.pub is present (signed bundles
            // verify, production rejects unsigned/untrusted). "inert_no_key"
            // => NO trust anchor anywhere, so OTA cannot be signature-gated and
            // we say so instead of claiming a gate that would reject every
            // update.
            "ota_integrity": match crate::ota_signature::ota_signature_state() {
                crate::ota_signature::OtaSignatureState::Enforced =>
                    "ed25519-signed manifest (MANIFEST.sig verified against compile-time pin and/or /etc/dcentos/release_ed25519.pub), fail-closed",
                crate::ota_signature::OtaSignatureState::InertNoKey =>
                    "no OTA verification key is compiled in or present at /etc/dcentos/release_ed25519.pub — OTA signature verification is INERT (unsigned-only); the production upload path still rejects unsigned bundles (allow_unsigned=false)",
            },
            "signature_state": crate::ota_signature::ota_signature_state().as_str(),
            "signature_required": crate::ota_signature::ota_signature_state().is_enforced(),
            "compiled_key_id": crate::ota_signature::honest_key_id(),
            "board_target": board_target,
            "update_authorized": update_authorized,
            "upload_endpoint": update_authorized.then_some("/api/system/upgrade"),
            "storage_topology": board_desc.map(|desc| desc.enablement.storage_topology.as_str()).unwrap_or("unknown"),
            "update_mechanism": board_desc.map(|desc| desc.enablement.update_mechanism.as_str()).unwrap_or("none"),
            "install_authorization": board_desc.map(|desc| desc.enablement.install_authorization.as_str()).unwrap_or("denied"),
            "recovery_maturity": board_desc.map(|desc| desc.enablement.recovery_maturity.as_str()).unwrap_or("not_implemented"),
            "ab_slot_rollback": ab_slot_rollback,
            "rollback_policy": {
                "verdicts": ["allow_forward", "allow_reinstall", "allow_downgrade", "deny_older_version", "deny_malformed_version"],
                "default_denies_downgrade": true,
                "fail_closed_on_malformed_version": true,
                "_source": "dcentrald-api-types::ota_rollback_protection::RollbackVerdict",
            },
        },
        "competitor_re": {
            "luxos": {
                "update_integrity": "md5_only",
                "integrity_mechanism": "MD5 checksums over a TLS-fetched package; the reverse-engineered update path carries no ed25519/RSA/SHA-256 signature.",
                "default_channel_url": LUXOS_DEFAULT_CHANNEL_URL,
                "brick_survival_phase": "fallback_full_download",
                "_source": "dcentrald-api-types::luxos_update::LuxosUpdateIntegrity::Md5Only",
            },
        },
        "notes": "DCENT_OS enforces ed25519-signed, fail-closed OTA: the MANIFEST.sig is verified against a compile-time pin and/or /etc/dcentos/release_ed25519.pub before any sysupgrade. Read-only transparency surface.",
    }))
}

// ─── : cross-firmware status/error vocabulary (api-types) ────────

/// GET /api/diagnostics/error_vocab
///
/// Read-only cross-firmware status/error vocabulary catalog for
/// pyasic / hass-miner ecosystem parity + honest error surfacing: the
/// RE'd LuxOS error-class taxonomy (`ALL_ERROR_CLASSES` + prefixes), the
/// LuxOS runtime-error code map (`LuxosRuntimeError` code/msg, sourced
/// from the type's own methods), and the BraiinsOS DPS scale-down
/// canonical ordering. Issues no hardware I/O.
pub(super) async fn get_diagnostics_error_vocab() -> impl IntoResponse {
    use dcentrald_api_types::braiinsos_miner_status::DPS_SCALE_DOWN_CHAIN;
    use dcentrald_api_types::luxos_error_vocab::{LuxosRuntimeError, ALL_ERROR_CLASSES};

    let luxos_error_classes: Vec<serde_json::Value> = ALL_ERROR_CLASSES
        .iter()
        .map(|c| serde_json::json!({ "class": c, "prefix": c.prefix() }))
        .collect();

    let luxos_runtime_errors: Vec<serde_json::Value> = [
        LuxosRuntimeError::InvalidCommand,
        LuxosRuntimeError::InvalidParamValue,
        LuxosRuntimeError::InvalidSessionField,
    ]
    .iter()
    .map(|e| serde_json::json!({ "error": e, "code": e.code(), "msg_substr": e.msg_substr() }))
    .collect();

    Json(serde_json::json!({
        "schema": "dcentrald-api-types::error_vocab v1",
        "read_only": true,
        "note": "Cross-firmware status/error vocabulary for pyasic/hass-miner parity + honest error surfacing. Read-only; no hardware I/O.",
        "luxos_error_classes": luxos_error_classes,
        "luxos_runtime_errors": luxos_runtime_errors,
        "braiinsos_dps_scale_down_chain": serde_json::to_value(DPS_SCALE_DOWN_CHAIN).unwrap_or(serde_json::Value::Null),
    }))
}

// ─── : boot-to-mining ramp reference (api-types) ─────────────────

/// GET /api/mining/ramp
///
/// Read-only boot-to-mining ramp reference + autotune-phase config — the
/// "is it ramping normally?" answer BraiinsOS/LuxOS surface. Backed by
/// the RE'd canonical curve `ramp_curve::LUXOS_S19J_PRO_RAMP` (const) +
/// the `RampVerdict` taxonomy + `autotune_phase::AutotuneConfig`
/// defaults. The runtime verdict helper is
/// `ramp_curve::classify_ramp_progress(milestone, observed_seconds)`.
/// Issues no hardware I/O.
pub(super) async fn get_mining_ramp() -> impl IntoResponse {
    use dcentrald_api_types::autotune_phase::AutotuneConfig;
    use dcentrald_api_types::ramp_curve::LUXOS_S19J_PRO_RAMP;

    Json(serde_json::json!({
        "schema": "dcentrald-api-types::mining_ramp v1",
        "read_only": true,
        "note": "Canonical boot-to-mining ramp reference + autotune phase config. Read-only; classify_ramp_progress(milestone, observed_seconds) is the runtime verdict helper. No hardware I/O.",
        "luxos_reference_ramp": serde_json::to_value(LUXOS_S19J_PRO_RAMP).unwrap_or(serde_json::Value::Null),
        "ramp_reference_source": "LuxOS .79 trace — S19j Pro BM1362 3x126 (O-live-performance.md); other models scale ±20%",
        "ramp_verdicts": ["on_track", "ahead", "stuck"],
        "autotune_config_default": serde_json::to_value(AutotuneConfig::default()).unwrap_or(serde_json::Value::Null),
        "_source": "dcentrald-api-types::{ramp_curve,autotune_phase}",
    }))
}

// ─── : Stratum protocol-support transparency (api-types) ─────────

/// GET /api/stratum/protocol
///
/// Read-only Stratum protocol-support transparency: the SV1
/// share-rejection code catalog and the SV2 (Noise NX) protocol
/// constants + handshake order. DCENT_OS implements both V1 and V2 — see
/// `/api/competitive/readiness` for the cross-firmware capability matrix
/// (DCENT_OS ships an opt-in SV2 client with V1 as the default; BraiinsOS,
/// the firmware that authored Stratum V2, ships a native SV2 client too,
/// so DCENT_OS is not the only SV2 firmware in the matrix).
/// Read-only; issues no pool connection and no hardware I/O.
pub(super) async fn get_stratum_protocol() -> impl IntoResponse {
    use dcentrald_api_types::stratum_v1_messages::StratumV1RejectCode;
    use dcentrald_api_types::stratum_v2_messages::{
        NoiseHandshakeStep, DEFAULT_STRATUM_V2_PORT, MAX_PAYLOAD_LEN, NOISE_NX_CIPHER_SUITE,
        STANDARD_CHANNEL_NONCE_EXHAUSTION_THS,
    };

    let v1_reject_codes: Vec<serde_json::Value> = [
        StratumV1RejectCode::JobNotFound,
        StratumV1RejectCode::DuplicateShare,
        StratumV1RejectCode::LowDifficulty,
        StratumV1RejectCode::UnauthorizedWorker,
        StratumV1RejectCode::NotSubscribed,
        StratumV1RejectCode::Reserved,
        StratumV1RejectCode::InvalidVersionMask,
    ]
    .iter()
    .map(|c| serde_json::json!({ "code": c.code(), "name": c }))
    .collect();

    let noise_nx_handshake: Vec<serde_json::Value> = [
        NoiseHandshakeStep::ClientHello,
        NoiseHandshakeStep::ServerHello,
        NoiseHandshakeStep::ClientFinish,
    ]
    .iter()
    .map(|s| serde_json::json!({ "order": s.order(), "step": s, "direction": s.direction() }))
    .collect();

    Json(serde_json::json!({
        "schema": "dcentrald-api-types::stratum_protocol v1",
        "read_only": true,
        "note": "Stratum protocol-support transparency. DCENT_OS implements a Stratum V1 client (the default) and an opt-in Stratum V2 (Noise NX) client. Read-only; issues no pool connection.",
        "stratum_v1": { "reject_codes": v1_reject_codes },
        "stratum_v2": {
            "default_port": DEFAULT_STRATUM_V2_PORT,
            "noise_cipher_suite": NOISE_NX_CIPHER_SUITE,
            "max_payload_len": MAX_PAYLOAD_LEN,
            "standard_channel_nonce_exhaustion_ths": STANDARD_CHANNEL_NONCE_EXHAUSTION_THS,
            "noise_nx_handshake": noise_nx_handshake,
        },
        "notes": "DCENT_OS ships a Stratum V1 client (the default) and an opt-in Stratum V2 (Noise NX encrypted) client. SV2 is config-selected and its live accepted-share proof is still pending. See /api/competitive/readiness.",
        "_source": "dcentrald-api-types::{stratum_v1_messages,stratum_v2_messages}",
    }))
}

// ─── : PSU-bypass / Loki-requirement matrix (api-types) ──────────

/// GET /api/hardware/psu_bypass_matrix
///
/// Read-only Loki-board requirement matrix + DCENT_OS PSU-bypass modes.
/// Demonstrates a DCENT-unique capability: DCENT_OS needs NO Loki-PSS /
/// Loki-HBS spoof board (PSU model-string + hashboard-detection checks
/// designed out from day 1; `BypassMode::FullBypass` enables 120 V
/// single-board home mining). Backed by `dcentrald-api-types::psu_bypass`.
/// Issues no PSU/I2C/hardware I/O.
pub(super) async fn get_hardware_psu_bypass_matrix() -> impl IntoResponse {
    use dcentrald_api_types::psu_bypass::{
        allows_partial_hashboard_population, allows_psu_model_skip, loki_need, BypassMode,
        MinerFirmware,
    };

    let loki_requirement_matrix: Vec<serde_json::Value> = [
        MinerFirmware::BitmainStock,
        MinerFirmware::BraiinsOs,
        MinerFirmware::Vnish,
        MinerFirmware::LuxOs,
        MinerFirmware::DcentOs,
    ]
    .iter()
    .map(|fw| {
        let n = loki_need(*fw);
        serde_json::json!({
            "firmware": fw,
            "needs_psu_spoof": n.needs_psu_spoof,
            "needs_hashboard_spoof": n.needs_hashboard_spoof,
            "label": n.label,
        })
    })
    .collect();

    let dcentos_bypass_modes: Vec<serde_json::Value> =
        [BypassMode::Off, BypassMode::PsuOnly, BypassMode::FullBypass]
            .iter()
            .map(|m| {
                serde_json::json!({
                    "mode": m,
                    "allows_partial_hashboard_population": allows_partial_hashboard_population(*m),
                    "allows_psu_model_skip": allows_psu_model_skip(*m),
                })
            })
            .collect();

    Json(serde_json::json!({
        "schema": "dcentrald-api-types::psu_bypass v1",
        "read_only": true,
        "note": "Loki-board requirement matrix + DCENT_OS PSU-bypass modes. Read-only catalog; no PSU/I2C/hardware I/O.",
        "loki_requirement_matrix": loki_requirement_matrix,
        "dcentos_bypass_modes": dcentos_bypass_modes,
        "notes": "DCENT_OS requires NO Loki-PSS / Loki-HBS board — PSU model-string and hashboard-detection checks were designed out from day 1 (FullBypass enables 120V single-board home mining). The loki_requirement_matrix above is reverse-engineered interop data for cross-firmware tooling.",
        "_source": "dcentrald-api-types::psu_bypass",
    }))
}

// ─── : cold-environment target auto-adjust (api-types) ──────────

/// GET /api/thermal/cold_environment
///
/// Read-only cold-environment thermal policy for home / space-heater
/// operation: the canonical `ColdEnvironmentConfig` defaults plus a
/// deterministic sample of `effective_target_temp_c_default` across
/// ambient temperatures (pure function — no live sensor read). LuxOS
/// has a cold-environment auto-adjust; DCENT_OS exposes its policy
/// transparently. Issues no hardware I/O.
pub(super) async fn get_thermal_cold_environment() -> impl IntoResponse {
    use dcentrald_api_types::cold_environment::{
        effective_target_temp_c_default, ColdEnvironmentConfig,
    };

    let user_target_c = 55.0_f32;
    let sample_curve: Vec<serde_json::Value> =
        [None, Some(35.0_f32), Some(20.0), Some(10.0), Some(-5.0)]
            .iter()
            .map(|amb| {
                serde_json::json!({
                    "ambient_c": amb,
                    "user_target_c": user_target_c,
                    "effective_target_c": effective_target_temp_c_default(user_target_c, *amb),
                })
            })
            .collect();

    Json(serde_json::json!({
        "schema": "dcentrald-api-types::cold_environment v1",
        "read_only": true,
        "note": "Canonical cold-environment target auto-adjust policy + a deterministic sample curve (pure fn; no live sensor read, no hardware I/O).",
        "config_default": serde_json::to_value(ColdEnvironmentConfig::default()).unwrap_or(serde_json::Value::Null),
        "sample_curve_user_target_55c": sample_curve,
        "notes": "DCENT_OS transparently exposes its cold-environment chip-temp-target auto-adjust (home/space-heater first). Below cold_threshold_c the effective target steps down (bounded by max_downshift_c / min_effective_target_c).",
        "_source": "dcentrald-api-types::cold_environment",
    }))
}

// ─── : pool-failover policy reference (api-types) ────────────────

/// GET /api/pools/failover_policy
///
/// Read-only pool-failover policy reference distilled from the LuxOS RE
/// (`luxos_pool_failover`): the pool state machine + mining-readiness
/// predicates, the failover-trigger taxonomy with
/// error-counter/reconnect/drop classifiers, the live-`a lab unit` config
/// defaults, a deterministic linear-backoff sample, and the
/// smart-switch states. Reliability transparency no competitor exposes.
/// Read-only; issues no pool connection and no hardware I/O.
pub(super) async fn get_pools_failover_policy() -> impl IntoResponse {
    use dcentrald_api_types::luxos_pool_failover::{
        linear_backoff_seconds, LuxosPoolFailoverConfig, ALL_FAILOVER_TRIGGERS, ALL_POOL_STATES,
        ALL_SMART_SWITCH_STATES,
    };

    let pool_states: Vec<serde_json::Value> = ALL_POOL_STATES
        .iter()
        .map(|s| {
            serde_json::json!({
                "state": s,
                "is_mining_ready": s.is_mining_ready(),
                "is_in_handshake": s.is_in_handshake(),
            })
        })
        .collect();

    let failover_triggers: Vec<serde_json::Value> = ALL_FAILOVER_TRIGGERS
        .iter()
        .map(|t| {
            serde_json::json!({
                "trigger": t,
                "increments_error_counter": t.increments_error_counter(),
                "reconnects_same_pool": t.reconnects_same_pool(),
                "drops_from_list": t.drops_from_list(),
            })
        })
        .collect();

    let backoff_sample: Vec<serde_json::Value> = (1u32..=6)
        .map(|a| serde_json::json!({ "attempt": a, "backoff_secs": linear_backoff_seconds(a) }))
        .collect();

    Json(serde_json::json!({
        "schema": "dcentrald-api-types::pool_failover v1",
        "read_only": true,
        "note": "Pool-failover policy reference (LuxOS-RE-distilled). Read-only; issues no pool connection or hardware I/O.",
        "pool_states": pool_states,
        "failover_triggers": failover_triggers,
        "config_default_live_79": serde_json::to_value(LuxosPoolFailoverConfig::default()).unwrap_or(serde_json::Value::Null),
        "linear_backoff_sample": backoff_sample,
        "smart_switch_states": serde_json::to_value(ALL_SMART_SWITCH_STATES).unwrap_or(serde_json::Value::Null),
        "notes": "DCENT_OS exposes the full pool-failover state machine + trigger taxonomy + backoff schedule transparently. See also /api/pools.failover for the live runtime snapshot.",
        "_source": "dcentrald-api-types::luxos_pool_failover",
    }))
}

// ─── : tuning-constraint catalog (api-types) ────────────────────

/// GET /api/tuning/constraints
///
/// Read-only BraiinsOS-RE'd tuning-constraint catalog: for every
/// documented `ConstraintParam` its category and the documented
/// min/max bound (u32 / f64 / pct as applicable). Operator transparency
/// on what tuning limits exist — competitors don't surface this. Backed
/// by `dcentrald-api-types::braiinsos_constraints`. Issues no hardware
/// I/O and changes no tuning state.
pub(super) async fn get_tuning_constraints() -> impl IntoResponse {
    use dcentrald_api_types::braiinsos_constraints::ALL_CONSTRAINT_PARAMS;

    let constraints: Vec<serde_json::Value> = ALL_CONSTRAINT_PARAMS
        .iter()
        .map(|p| {
            serde_json::json!({
                "param": p,
                "category": p.category(),
                "u32": serde_json::to_value(p.documented_u32()).unwrap_or(serde_json::Value::Null),
                "f64": serde_json::to_value(p.documented_f64()).unwrap_or(serde_json::Value::Null),
                "pct": serde_json::to_value(p.documented_pct()).unwrap_or(serde_json::Value::Null),
            })
        })
        .collect();

    Json(serde_json::json!({
        "schema": "dcentrald-api-types::tuning_constraints v1",
        "read_only": true,
        "note": "BraiinsOS-RE'd documented tuning-constraint catalog. Read-only; changes no tuning state and issues no hardware I/O.",
        "constraint_count": constraints.len(),
        "constraints": constraints,
        "notes": "DCENT_OS transparently publishes the documented tuning-constraint envelope (param → category → min/max).",
        "_source": "dcentrald-api-types::braiinsos_constraints",
    }))
}

// ─── : temp-sensor outlier-rejection policy (api-types) ──────────

/// GET /api/diagnostics/sensor_outlier
///
/// Read-only temperature-sensor outlier-rejection policy: the canonical
/// `OutlierConfig` defaults + the `SensorVerdict` taxonomy. DCENT_OS
/// drops persistently-bad sensor readings from the thermal aggregate
/// (subject to a `min_per_board` invariant) — a thermal-robustness
/// quality competitors don't surface. Read-only; no live sensor read;
/// no hardware I/O.
pub(super) async fn get_diagnostics_sensor_outlier() -> impl IntoResponse {
    use dcentrald_api_types::sensor_outlier::OutlierConfig;

    let cfg = OutlierConfig::default();

    Json(serde_json::json!({
        "schema": "dcentrald-api-types::sensor_outlier v1",
        "read_only": true,
        "note": "Canonical temp-sensor outlier-rejection policy. Read-only; no live sensor read; no hardware I/O.",
        "config_default": {
            "bad_average_threshold_c": cfg.bad_average_threshold_c,
            "max_bad_readings": cfg.max_bad_readings,
            "min_per_board": cfg.min_per_board,
        },
        "sensor_verdicts": [
            { "verdict": "healthy", "meaning": "within tolerance — used in the thermal aggregate" },
            { "verdict": "suspect", "meaning": "deviates but has not crossed the consecutive-bad streak threshold" },
            { "verdict": "dropped", "meaning": "bad for max_bad_readings consecutive ticks — dropped from aggregate (subject to min_per_board)" },
        ],
        "notes": "DCENT_OS rejects persistently-bad temp-sensor readings from the thermal aggregate (bounded by min_per_board so a board never goes fully unmonitored) and publishes the policy.",
        "_source": "dcentrald-api-types::sensor_outlier",
    }))
}

// ─── : VNish REST response-shape reference (api-types) ───────────

/// GET /api/firmware/vnish_schema
///
/// Read-only reverse-engineered VNish REST response-shape reference
/// (default-instantiated). Lets DCENT fleet tooling /
/// firmware-detection map VNish's API shapes through DCENT_OS without
/// touching a VNish unit. Backed by
/// `dcentrald-api-types::vnish_settings`. Issues no network or hardware
/// I/O.
pub(super) async fn get_firmware_vnish_schema() -> impl IntoResponse {
    use dcentrald_api_types::vnish_settings::{
        VnishChainEntry, VnishChipEntry, VnishChipsResponse, VnishFactoryInfoResponse,
        VnishInfoResponse, VnishSettingsResponse,
    };

    Json(serde_json::json!({
        "schema": "dcentrald-api-types::vnish_settings v1",
        "read_only": true,
        "note": "RE-derived VNish REST response-shape reference (default-instantiated). Read-only; touches no VNish unit, no network, no hardware I/O.",
        "shapes": {
            "settings": serde_json::to_value(VnishSettingsResponse::default()).unwrap_or(serde_json::Value::Null),
            "info": serde_json::to_value(VnishInfoResponse::default()).unwrap_or(serde_json::Value::Null),
            "factory_info": serde_json::to_value(VnishFactoryInfoResponse::default()).unwrap_or(serde_json::Value::Null),
            "chips": serde_json::to_value(VnishChipsResponse::default()).unwrap_or(serde_json::Value::Null),
            "chip_entry": serde_json::to_value(VnishChipEntry::default()).unwrap_or(serde_json::Value::Null),
            "chain_entry": serde_json::to_value(VnishChainEntry::default()).unwrap_or(serde_json::Value::Null),
        },
        "interop": "DCENT_OS ships a clean-room RE reference of VNish REST shapes so fleet tooling can detect/interop across firmwares from one API.",
        "_source": "dcentrald-api-types::vnish_settings",
    }))
}

// ─── : LuxOS system-architecture reference (api-types) ──────────

/// GET /api/firmware/luxos_architecture
///
/// Read-only RE'd LuxOS NAND/MTD + init + kernel layout reference —
/// supports safe uninstall-to-stock / recovery planning on a
/// LuxOS-running unit before a DCENT_OS install. Backed by
/// `dcentrald-api-types::luxos_system_architecture`. Issues no flash,
/// no network, no hardware I/O.
pub(super) async fn get_firmware_luxos_architecture() -> impl IntoResponse {
    use dcentrald_api_types::luxos_system_architecture::{
        ALL_LUXOS_MTD_PARTITIONS, LUXOS_DEFAULT_HOSTNAME, LUXOS_INITTAB_DEFAULT_RUNLEVEL,
        LUXOS_KERNEL_PLATFORM, LUXOS_KERNEL_VERSION, LUXOS_KERNEL_VERSION_TAG,
        LUXOS_RAMDISK_TMPFS_SIZE_MB, LUXOS_SPECIFIC_INIT_SCRIPTS,
    };

    let mtd_partitions: Vec<serde_json::Value> = ALL_LUXOS_MTD_PARTITIONS
        .iter()
        .map(|p| {
            serde_json::json!({
                "partition": p,
                "mtd_index": p.mtd_index(),
                "size_bytes": p.partition_size_bytes(),
                "name": p.name(),
                "role": p.role(),
            })
        })
        .collect();

    let init_scripts: Vec<serde_json::Value> = LUXOS_SPECIFIC_INIT_SCRIPTS
        .iter()
        .map(|s| serde_json::json!({ "script": s, "filename": s.filename() }))
        .collect();

    Json(serde_json::json!({
        "schema": "dcentrald-api-types::luxos_system_architecture v1",
        "read_only": true,
        "note": "RE'd LuxOS NAND/MTD + init + kernel layout. Read-only recovery-planning reference; issues no flash, network, or hardware I/O.",
        "mtd_partitions": mtd_partitions,
        "luxos_specific_init_scripts": init_scripts,
        "kernel": {
            "version": LUXOS_KERNEL_VERSION,
            "version_tag": LUXOS_KERNEL_VERSION_TAG,
            "platform": LUXOS_KERNEL_PLATFORM,
            "ramdisk_tmpfs_size_mb": LUXOS_RAMDISK_TMPFS_SIZE_MB,
            "default_hostname": LUXOS_DEFAULT_HOSTNAME,
            "inittab_default_runlevel": LUXOS_INITTAB_DEFAULT_RUNLEVEL,
        },
        "interop": "DCENT_OS documents the LuxOS NAND/init/kernel layout so uninstall-to-stock / DCENT_OS install can be planned safely from one transparent reference.",
        "_source": "dcentrald-api-types::luxos_system_architecture",
    }))
}

// ─── : cooling-mode taxonomy reference (api-types) ──────────────

/// GET /api/thermal/cooling_modes
///
/// Read-only cooling-mode taxonomy (BraiinsOS-RE-distilled): the
/// `CoolingMode` set (Auto/Manual/Immersion/Hydro) with
/// fans-disabled / requires-PID predicates, the `FanPauseRuntime`
/// variants, and the version-introduced anchors. Backed by
/// `dcentrald-api-types::braiinsos_cooling_mode`. Issues no hardware
/// I/O and changes no cooling state.
pub(super) async fn get_thermal_cooling_modes() -> impl IntoResponse {
    use dcentrald_api_types::braiinsos_cooling_mode::{
        CoolingMode, FanPauseRuntime, HYDRO_MODE_INTRODUCED_VERSION, PAUSE_MODE_INTRODUCED_VERSION,
    };

    let cooling_modes: Vec<serde_json::Value> = [
        CoolingMode::Auto,
        CoolingMode::Manual,
        CoolingMode::Immersion,
        CoolingMode::Hydro,
    ]
    .iter()
    .map(|m| {
        serde_json::json!({
            "mode": m,
            "value": m.as_u8(),
            "fans_disabled": m.fans_disabled(),
            "requires_pid_loop": m.requires_pid_loop(),
        })
    })
    .collect();

    let fan_pause_runtimes: Vec<serde_json::Value> =
        [FanPauseRuntime::Limited, FanPauseRuntime::Indefinite]
            .iter()
            .map(|r| serde_json::json!({ "runtime": r }))
            .collect();

    Json(serde_json::json!({
        "schema": "dcentrald-api-types::cooling_mode v1",
        "read_only": true,
        "note": "Cooling-mode taxonomy reference. Read-only; changes no cooling state and issues no hardware I/O.",
        "cooling_modes": cooling_modes,
        "fan_pause_runtimes": fan_pause_runtimes,
        "hydro_mode_introduced_version": HYDRO_MODE_INTRODUCED_VERSION,
        "pause_mode_introduced_version": PAUSE_MODE_INTRODUCED_VERSION,
        "notes": "DCENT_OS publishes the full cooling-mode taxonomy (incl. immersion/hydro) + fan-pause semantics transparently.",
        "_source": "dcentrald-api-types::braiinsos_cooling_mode",
    }))
}

// ─── : Dynamic-Power-Scaling reference (api-types) ──────────────

/// GET /api/power/dps
///
/// Read-only Dynamic-Power-Scaling reference (BraiinsOS-RE-distilled):
/// the `DpsMode` set, the per-family `DpsThermalProfile`
/// target/hot/dangerous thresholds, the `DpsScaleUpConditions` defaults,
/// and the version-introduced anchors. Backed by
/// `dcentrald-api-types::braiinsos_dps_configuration`. Changes no power
/// state; issues no hardware I/O.
pub(super) async fn get_power_dps() -> impl IntoResponse {
    use dcentrald_api_types::braiinsos_dps_configuration::{
        DpsMode, DpsScaleUpConditions, DpsThermalProfile, DPS_INTRODUCED_VERSION,
        ON_START_TARGET_PERCENT_INTRODUCED_VERSION,
    };

    let dps_modes: Vec<serde_json::Value> = [DpsMode::Normal, DpsMode::Boost]
        .iter()
        .map(|m| serde_json::json!({ "mode": m, "value": m.as_u8() }))
        .collect();

    let thermal_profiles: Vec<serde_json::Value> = [
        DpsThermalProfile::S9,
        DpsThermalProfile::S17Family,
        DpsThermalProfile::S19Family,
        DpsThermalProfile::S21Family,
    ]
    .iter()
    .map(|p| {
        let (target, hot, dangerous) = p.thresholds();
        serde_json::json!({
            "profile": p,
            "target_c": p.target_c(),
            "hot_c": p.hot_c(),
            "dangerous_c": p.dangerous_c(),
            "thresholds_c": [target, hot, dangerous],
        })
    })
    .collect();

    Json(serde_json::json!({
        "schema": "dcentrald-api-types::dps_configuration v1",
        "read_only": true,
        "note": "Dynamic-Power-Scaling reference. Read-only; changes no power state and issues no hardware I/O.",
        "dps_modes": dps_modes,
        "thermal_profiles": thermal_profiles,
        "scale_up_conditions_default": serde_json::to_value(DpsScaleUpConditions::default()).unwrap_or(serde_json::Value::Null),
        "dps_introduced_version": DPS_INTRODUCED_VERSION,
        "on_start_target_percent_introduced_version": ON_START_TARGET_PERCENT_INTRODUCED_VERSION,
        "notes": "DCENT_OS publishes the DPS mode set + per-family thermal-threshold table + scale-up conditions transparently.",
        "_source": "dcentrald-api-types::braiinsos_dps_configuration",
    }))
}

// ─── : network-configuration schema reference (api-types) ───────

/// GET /api/network/config_schema
///
/// Read-only network-configuration schema reference: the `NetworkMode`
/// set, the default `NetworkConfiguration` + `NetworkInfo` shapes, the
/// RFC-1035 hostname-length bound, and the version-introduced anchor.
/// Backed by `dcentrald-api-types::braiinsos_network_configuration`.
/// Changes no network state; issues no network or hardware I/O.
pub(super) async fn get_network_config_schema() -> impl IntoResponse {
    use dcentrald_api_types::braiinsos_network_configuration::{
        NetworkConfiguration, NetworkInfo, NetworkMode, MAX_HOSTNAME_LEN,
        NETWORK_INFO_INTRODUCED_VERSION,
    };

    let network_modes: Vec<serde_json::Value> = [NetworkMode::Dhcp, NetworkMode::Static]
        .iter()
        .map(|m| serde_json::json!({ "mode": m }))
        .collect();

    Json(serde_json::json!({
        "schema": "dcentrald-api-types::network_configuration v1",
        "read_only": true,
        "note": "Network-configuration schema reference. Read-only; changes no network state and issues no network or hardware I/O.",
        "network_modes": network_modes,
        "configuration_default": serde_json::to_value(NetworkConfiguration::default()).unwrap_or(serde_json::Value::Null),
        "info_default": serde_json::to_value(NetworkInfo::default()).unwrap_or(serde_json::Value::Null),
        "max_hostname_len": MAX_HOSTNAME_LEN,
        "network_info_introduced_version": NETWORK_INFO_INTRODUCED_VERSION,
        "notes": "DCENT_OS publishes its network-config schema + RFC-1035 hostname bound transparently.",
        "_source": "dcentrald-api-types::braiinsos_network_configuration",
    }))
}

// ─── : LuxOS web-UI surface map reference (api-types) ───────────

/// GET /api/firmware/luxos_web_map
///
/// Read-only RE'd LuxOS web-UI surface map (`LUXOS_PAGES`) — supports
/// operator migration / forensics transparency by documenting the
/// competitor SPA page surface DCENT replaces. Backed by
/// `dcentrald-api-types::luxos_web_pages`. Touches no LuxOS unit; no
/// network or hardware I/O.
pub(super) async fn get_firmware_luxos_web_map() -> impl IntoResponse {
    use dcentrald_api_types::luxos_web_pages::LUXOS_PAGES;

    Json(serde_json::json!({
        "schema": "dcentrald-api-types::luxos_web_pages v1",
        "read_only": true,
        "note": "RE'd LuxOS web-UI surface map. Read-only; touches no LuxOS unit, no network or hardware I/O.",
        "page_count": LUXOS_PAGES.len(),
        "pages": serde_json::to_value(LUXOS_PAGES).unwrap_or(serde_json::Value::Null),
        "interop": "DCENT_OS documents the LuxOS web-UI surface so operators migrating off LuxOS have a transparent map of what DCENT_OS replaces.",
        "_source": "dcentrald-api-types::luxos_web_pages",
    }))
}

// ─── : BraiinsOS proto wire-type reference (api-types) ──────────

/// GET /api/firmware/proto_wire_types
///
/// Read-only BraiinsOS gRPC proto unit-wrapper reference: the
/// default-instantiated unit types (Power / Frequency / Voltage /
/// Temperature / Tera-Giga-Mega-Hashrate / PowerEfficiency / Hours) +
/// the documented conversion helpers. The 10-window `RealHashrate`
/// sliding aggregator is named (not instantiated). Backed by
/// `dcentrald-api-types::braiinsos_proto_wire_types`. Issues no
/// network or hardware I/O.
pub(super) async fn get_firmware_proto_wire_types() -> impl IntoResponse {
    use dcentrald_api_types::braiinsos_proto_wire_types::{
        Frequency, GigaHashrate, Hours, MegaHashrate, Power, PowerEfficiency, Temperature,
        TeraHashrate, Voltage,
    };

    Json(serde_json::json!({
        "schema": "dcentrald-api-types::braiinsos_proto_wire_types v1",
        "read_only": true,
        "note": "BraiinsOS gRPC proto unit-wrapper reference (default-instantiated). Read-only; no network or hardware I/O.",
        "unit_types": {
            "power": serde_json::to_value(Power::default()).unwrap_or(serde_json::Value::Null),
            "frequency": serde_json::to_value(Frequency::default()).unwrap_or(serde_json::Value::Null),
            "voltage": serde_json::to_value(Voltage::default()).unwrap_or(serde_json::Value::Null),
            "temperature": serde_json::to_value(Temperature::default()).unwrap_or(serde_json::Value::Null),
            "tera_hashrate": serde_json::to_value(TeraHashrate::default()).unwrap_or(serde_json::Value::Null),
            "giga_hashrate": serde_json::to_value(GigaHashrate::default()).unwrap_or(serde_json::Value::Null),
            "mega_hashrate": serde_json::to_value(MegaHashrate::default()).unwrap_or(serde_json::Value::Null),
            "power_efficiency": serde_json::to_value(PowerEfficiency::default()).unwrap_or(serde_json::Value::Null),
            "hours": serde_json::to_value(Hours::default()).unwrap_or(serde_json::Value::Null),
        },
        "conversions": [
            "Power::from_watts(u64)",
            "TeraHashrate::from_giga(GigaHashrate)",
            "GigaHashrate::from_mega(MegaHashrate)",
            "GigaHashrate::from_tera(TeraHashrate)",
            "PowerEfficiency::from_watts_and_ths(watts:u64, ths:f64)",
        ],
        "real_hashrate": "RealHashrate is a 10-window sliding aggregator (not instantiated here).",
        "interop": "DCENT_OS publishes the BraiinsOS gRPC proto unit-type contract + conversions transparently for cross-firmware tooling.",
        "_source": "dcentrald-api-types::braiinsos_proto_wire_types",
    }))
}

// ─── : LuxOS CGMiner-compat response shapes (api-types) ────────

/// GET /api/firmware/luxos_responses
///
/// Read-only RE'd LuxOS CGMiner-compatible response-shape reference
/// (default-instantiated): version / summary / devs / fans / tempctrl /
/// atm envelopes. Lets pyasic / hass-miner tooling map LuxOS's
/// CGMiner-style responses through DCENT_OS. Backed by
/// `dcentrald-api-types::luxos_response_payloads`. Touches no LuxOS
/// unit; no network or hardware I/O.
pub(super) async fn get_firmware_luxos_responses() -> impl IntoResponse {
    use dcentrald_api_types::luxos_response_payloads::{
        LuxosAtmResponse, LuxosDevsResponse, LuxosFansResponse, LuxosSummaryResponse,
        LuxosTempCtrlResponse, LuxosVersionResponse,
    };

    Json(serde_json::json!({
        "schema": "dcentrald-api-types::luxos_response_payloads v1",
        "read_only": true,
        "note": "RE'd LuxOS CGMiner-compat response-shape reference (default-instantiated). Read-only; touches no LuxOS unit, no network or hardware I/O.",
        "shapes": {
            "version": serde_json::to_value(LuxosVersionResponse::default()).unwrap_or(serde_json::Value::Null),
            "summary": serde_json::to_value(LuxosSummaryResponse::default()).unwrap_or(serde_json::Value::Null),
            "devs": serde_json::to_value(LuxosDevsResponse::default()).unwrap_or(serde_json::Value::Null),
            "fans": serde_json::to_value(LuxosFansResponse::default()).unwrap_or(serde_json::Value::Null),
            "tempctrl": serde_json::to_value(LuxosTempCtrlResponse::default()).unwrap_or(serde_json::Value::Null),
            "atm": serde_json::to_value(LuxosAtmResponse::default()).unwrap_or(serde_json::Value::Null),
        },
        "interop": "DCENT_OS publishes a clean-room RE reference of LuxOS CGMiner-compat response shapes so pyasic/hass-miner tooling can interop across firmwares from one API.",
        "_source": "dcentrald-api-types::luxos_response_payloads",
    }))
}

// ─── : LuxOS REST status-code reference (api-types) ─────────────

/// GET /api/firmware/luxos_status_codes
///
/// Read-only RE'd LuxOS REST envelope reference: the named CGMiner-style
/// status codes (`codes::*`) and the HTTP-status mapping
/// (`LuxosHttpStatus::http_code`). Lets fleet tooling decode LuxOS
/// responses through DCENT_OS. Backed by
/// `dcentrald-api-types::luxos_rest_envelope`. Touches no LuxOS unit; no
/// network or hardware I/O.
pub(super) async fn get_firmware_luxos_status_codes() -> impl IntoResponse {
    use dcentrald_api_types::luxos_rest_envelope::{codes, LuxosHttpStatus};

    let http_statuses: Vec<serde_json::Value> = [
        LuxosHttpStatus::Ok,
        LuxosHttpStatus::BadRequest,
        LuxosHttpStatus::Unauthorized,
        LuxosHttpStatus::Forbidden,
        LuxosHttpStatus::NotFound,
        LuxosHttpStatus::InternalServerError,
        LuxosHttpStatus::ServiceUnavailable,
    ]
    .iter()
    .map(|s| serde_json::json!({ "status": s, "http_code": s.http_code() }))
    .collect();

    Json(serde_json::json!({
        "schema": "dcentrald-api-types::luxos_rest_envelope v1",
        "read_only": true,
        "note": "RE'd LuxOS REST status-code reference. Read-only; touches no LuxOS unit, no network or hardware I/O.",
        "named_status_codes": {
            "session_created": codes::SESSION_CREATED,
            "kill_session": codes::KILL_SESSION,
            "session_info": codes::SESSION_INFO,
            "profiles_list": codes::PROFILES_LIST,
            "groups_list": codes::GROUPS_LIST,
            "miner_events": codes::MINER_EVENTS,
            "missing_parameter": codes::MISSING_PARAMETER,
        },
        "http_statuses": http_statuses,
        "interop": "DCENT_OS publishes the LuxOS REST status-code + HTTP-status contract so fleet tooling can decode LuxOS responses through one transparent reference.",
        "_source": "dcentrald-api-types::luxos_rest_envelope",
    }))
}

// ─── : VNish-on-stock overlay layout reference (api-types) ──────

/// GET /api/firmware/vnish_overlay
///
/// Read-only RE'd VNish-on-stock overlay layout + recovery reference:
/// the killed-process / disabled-init-script lists, the `bootos.sh`
/// phase order, the AnthillOS service start order, and the canonical
/// overlay-removal recovery command. Supports forensics + safe
/// uninstall planning on a VNish-overlaid unit. Backed by
/// `dcentrald-api-types::vnish_overlay_layout`. Touches no VNish unit;
/// no flash/network/hardware I/O.
pub(super) async fn get_firmware_vnish_overlay() -> impl IntoResponse {
    use dcentrald_api_types::vnish_overlay_layout::{
        ANTHILL_SERVICE_START_ORDER, BOOTOS_SH_PHASES, RECOVERY_REMOVE_OVERLAY_COMMAND,
        STOCK_DISABLED_INIT_SCRIPTS, STOCK_KILLED_PROCESSES,
    };

    Json(serde_json::json!({
        "schema": "dcentrald-api-types::vnish_overlay_layout v1",
        "read_only": true,
        "note": "RE'd VNish-on-stock overlay layout + recovery reference. Read-only; touches no VNish unit, no flash/network/hardware I/O.",
        "stock_killed_processes": STOCK_KILLED_PROCESSES,
        "stock_disabled_init_scripts": STOCK_DISABLED_INIT_SCRIPTS,
        "bootos_sh_phases": serde_json::to_value(BOOTOS_SH_PHASES).unwrap_or(serde_json::Value::Null),
        "anthill_service_start_order": ANTHILL_SERVICE_START_ORDER,
        "recovery_remove_overlay_command": RECOVERY_REMOVE_OVERLAY_COMMAND,
        "interop": "DCENT_OS documents the VNish overlay-on-stock layout + the exact recovery command so a VNish→DCENT_OS migration can be planned safely from one transparent reference.",
        "_source": "dcentrald-api-types::vnish_overlay_layout",
    }))
}

// ─── Autotuner Endpoints ────────────────────────────────────────────────

/// Helper: load all chain tuning profiles from disk.
///
/// Scans the profile directory for `autotune-chain{6,7,8}.json` files.
/// Safe for concurrent reads because profile save uses atomic rename.
pub(super) fn load_profiles(
    profile_path: &str,
) -> std::collections::HashMap<u8, dcentrald_autotuner::TuningProfile> {
    let mut profiles = std::collections::HashMap::new();
    for chain_id in [6u8, 7, 8] {
        if let Some(profile) = dcentrald_autotuner::TuningProfile::load(profile_path, chain_id) {
            profiles.insert(chain_id, profile);
        }
    }
    profiles
}

pub(super) const AUTOTUNER_STATE_FILENAME: &str = "state.toml";

pub(super) fn read_autotuner_resume_state(
    profile_path: &str,
) -> (
    Option<dcentrald_autotuner::AutotunerResumeState>,
    serde_json::Value,
) {
    let path = Path::new(profile_path).join(AUTOTUNER_STATE_FILENAME);
    match std::fs::read_to_string(&path) {
        Ok(contents) => {
            match toml::from_str::<dcentrald_autotuner::AutotunerResumeState>(&contents) {
                Ok(state) => {
                    let manifest = serde_json::json!({
                        "available": true,
                        "file": AUTOTUNER_STATE_FILENAME,
                        "present": true,
                        "read_ok": true,
                        "parse_ok": true,
                        "version": state.version,
                        "saved_at_unix_s": state.saved_at_unix_s,
                        "platform": state.fingerprint.platform.as_deref(),
                        "chain_count": state.chains.len(),
                        "fingerprint": &state.fingerprint,
                        "reason": "Hardware-fingerprinted last-known-good autotuner state is available."
                    });
                    (Some(state), manifest)
                }
                Err(error) => (
                    None,
                    serde_json::json!({
                        "available": false,
                        "file": AUTOTUNER_STATE_FILENAME,
                        "present": true,
                        "read_ok": true,
                        "parse_ok": false,
                        "reason": format!("Autotuner resume state exists but could not be parsed: {error}")
                    }),
                ),
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => (
            None,
            serde_json::json!({
                "available": false,
                "file": AUTOTUNER_STATE_FILENAME,
                "present": false,
                "read_ok": false,
                "parse_ok": false,
                "reason": "No hardware-fingerprinted autotuner resume state exists yet."
            }),
        ),
        Err(error) => (
            None,
            serde_json::json!({
                "available": false,
                "file": AUTOTUNER_STATE_FILENAME,
                "present": true,
                "read_ok": false,
                "parse_ok": false,
                "reason": format!("Autotuner resume state exists but could not be read: {error}")
            }),
        ),
    }
}

pub(super) fn autotuner_resume_chain_summary(
    resume_state: Option<&dcentrald_autotuner::AutotunerResumeState>,
    chain_id: u8,
) -> serde_json::Value {
    let Some(state) = resume_state else {
        return serde_json::json!({
            "available": false,
            "reason": "No hardware-fingerprinted autotuner resume state exists yet."
        });
    };

    let fingerprint = state
        .fingerprint
        .chains
        .iter()
        .find(|chain| chain.chain_id == chain_id);
    let last_known_good = state
        .chains
        .iter()
        .find(|chain| chain.chain_id == chain_id)
        .map(|chain| {
            serde_json::json!({
                "chain_id": chain.chain_id,
                "chip_count": chain.chip_count,
                "voltage_mv": chain.voltage_mv,
                "avg_freq_mhz": chain.avg_freq_mhz,
                "estimated_power_w": chain.estimated_power_w,
                "estimated_efficiency_jth": chain.estimated_efficiency_jth,
                "chip_state_count": chain.chips.len(),
            })
        });

    serde_json::json!({
        "available": fingerprint.is_some() || last_known_good.is_some(),
        "version": state.version,
        "saved_at_unix_s": state.saved_at_unix_s,
        "fingerprint": fingerprint,
        "last_known_good": last_known_good,
        "reason": if fingerprint.is_some() || last_known_good.is_some() {
            "Resume-state evidence exists for this chain."
        } else {
            "Resume-state file exists but has no entry for this chain."
        }
    })
}

#[derive(Debug, Clone)]
pub(super) struct AutotunerTargetConfig {
    target_watts: u32,
    target_hashrate_ths: f64,
    power_step_w: u32,
    hashrate_step_ths: f64,
    max_power_w: u32,
    dps_high_performance_mode: bool,
}

impl Default for AutotunerTargetConfig {
    fn default() -> Self {
        Self {
            target_watts: 0,
            target_hashrate_ths: 0.0,
            power_step_w: 300,
            hashrate_step_ths: 11.0,
            max_power_w: dcentrald_autotuner::config::ABSOLUTE_MAX_WATTS,
            dps_high_performance_mode: false,
        }
    }
}

pub(super) fn autotuner_u32(table: &toml::Table, key: &str, default: u32) -> u32 {
    table
        .get(key)
        .and_then(|value| value.as_integer())
        .and_then(|value| u32::try_from(value).ok())
        .unwrap_or(default)
}

pub(super) fn autotuner_f64(table: &toml::Table, key: &str, default: f64) -> f64 {
    table
        .get(key)
        .and_then(|value| {
            value
                .as_float()
                .or_else(|| value.as_integer().map(|v| v as f64))
        })
        .filter(|value| value.is_finite())
        .unwrap_or(default)
}

pub(super) fn read_autotuner_target_config() -> AutotunerTargetConfig {
    let mut config = AutotunerTargetConfig::default();
    let Ok(table) = load_config_table_for_write() else {
        return config;
    };
    let Some(autotuner) = table.get("autotuner").and_then(|value| value.as_table()) else {
        return config;
    };

    config.target_watts = autotuner_u32(autotuner, "target_watts", config.target_watts);
    config.target_hashrate_ths =
        autotuner_f64(autotuner, "target_hashrate_ths", config.target_hashrate_ths);
    config.power_step_w = autotuner_u32(autotuner, "power_step_w", config.power_step_w).max(1);
    config.hashrate_step_ths =
        autotuner_f64(autotuner, "hashrate_step_ths", config.hashrate_step_ths).max(0.1);
    config.max_power_w = autotuner_u32(autotuner, "total_power_limit_w", config.max_power_w);
    if config.max_power_w == 0
        || config.max_power_w > dcentrald_autotuner::config::ABSOLUTE_MAX_WATTS
    {
        config.max_power_w = dcentrald_autotuner::config::ABSOLUTE_MAX_WATTS;
    }
    config.dps_high_performance_mode = autotuner
        .get("dps_high_performance_mode")
        .and_then(|value| value.as_bool())
        .unwrap_or(config.dps_high_performance_mode);
    config
}

// ───────────────────────────────────────────────────────────────────────
// CE-052: MQTT / Home-Assistant command sink impl.
//
// Relocated here from `rest.rs` (the struct `AppStateMqttCommandSink` and the
// `app_state_mqtt_command_sink` factory stay in `rest.rs`) to keep `rest.rs`
// under the 10000-line CI gate after the CE-052 bridge capability guards were
// added. `late` is a child module of `rest`, so `use super::*` reaches the
// private struct + its private `state` field, `grpc_bridge_set_fan`, and
// `bridge_runtime_capability_guard`. Each write setpoint routes through the SAME
// clamped setters the REST control plane uses AND now runs the fail-closed
// runtime-capability guard FIRST (before any HAL write / persist / dispatch).
// ───────────────────────────────────────────────────────────────────────
#[async_trait::async_trait]
impl crate::mqtt::MqttCommandSink for AppStateMqttCommandSink {
    async fn set_fan_pwm(&self, requested_pwm: u32) -> Result<u8, String> {
        // Guard inherited from `grpc_bridge_set_fan` (it runs
        // `bridge_runtime_capability_guard(.., PowerControl, ..)` first, before
        // the HAL fan write), which now also derives the live mode itself.
        grpc_bridge_set_fan(&self.state, requested_pwm)
    }

    async fn set_target_watts(&self, requested_watts: u32) -> Result<u32, String> {
        // CE-052: fail-closed `AsicOptions` gate FIRST — before persist/dispatch.
        bridge_runtime_capability_guard(
            &self.state,
            RuntimeCapability::AsicOptions,
            "mqtt:target_watts",
        )?;
        let clamped = requested_watts.clamp(
            crate::mqtt::CMD_TARGET_WATTS_MIN,
            crate::mqtt::CMD_TARGET_WATTS_MAX,
        );
        let mode = dcentrald_autotuner::config::TunerMode::PowerTarget { watts: clamped };
        // Persist + dispatch through the SAME path the REST power-target
        // endpoints use; the autotuner runtime owns the downstream clamps.
        persist_autotuner_mode(&mode)?; // CE-242: confirm live cap write below
        let runtime = dispatch_autotuner_mode_command(&self.state, mode).await;
        if !crate::mqtt::target_watts_cap_write_confirmed(&runtime) {
            return Err(crate::mqtt::unconfirmed_target_watts_error(&runtime));
        }
        Ok(clamped)
    }

    async fn set_target_temp_c(&self, requested_temp_c: f64) -> Result<u8, String> {
        // CE-052: fail-closed `ConfigRw` gate FIRST — before the TOML write.
        bridge_runtime_capability_guard(
            &self.state,
            RuntimeCapability::ConfigRw,
            "mqtt:target_temp_c",
        )?;
        if !requested_temp_c.is_finite() {
            return Err("target temperature must be a finite number".to_string());
        }
        // CE-052: the thermal clamp lives in `rest.rs::mqtt_clamp_target_temp_c`
        // so the safety-clamp manifest keeps its `rest.rs` fingerprint after this
        // impl was relocated here. Same bound, same fail-closed behaviour.
        let clamped = mqtt_clamp_target_temp_c(requested_temp_c);
        write_toml_section(
            "thermal",
            &[("target_temp_c", toml::Value::Integer(clamped as i64))],
        )?;
        Ok(clamped)
    }
}

pub(crate) fn persist_autotuner_mode(
    mode: &dcentrald_autotuner::config::TunerMode,
) -> Result<(), String> {
    let tuner_mode_value = toml::Value::try_from(mode)
        .map_err(|error| format!("Failed to serialize tuner mode: {}", error))?;
    let target_mode = mode.legacy_target_mode();
    let target_mode_value = match target_mode {
        dcentrald_autotuner::config::TuneTarget::Hashrate => "hashrate",
        dcentrald_autotuner::config::TuneTarget::Power => "power",
        dcentrald_autotuner::config::TuneTarget::Efficiency => "efficiency",
        dcentrald_autotuner::config::TuneTarget::HashrateTarget => "hashrate_target",
        dcentrald_autotuner::config::TuneTarget::EfficiencyJTH => "efficiency_jth",
    };

    let mut entries = vec![
        (
            "target_mode",
            toml::Value::String(target_mode_value.to_string()),
        ),
        ("tuner_mode", tuner_mode_value),
    ];
    if let Some(watts) = mode.target_watts() {
        entries.push(("target_watts", toml::Value::Integer(watts as i64)));
        entries.push(("target_hashrate_ths", toml::Value::Float(0.0)));
    }
    if let Some(ths) = mode.target_hashrate_ths() {
        entries.push(("target_hashrate_ths", toml::Value::Float(ths)));
        entries.push(("target_watts", toml::Value::Integer(0)));
    }

    write_toml_section("autotuner", &entries)
}

pub(super) fn autotuner_mode_response(
    action: &str,
    mode: &dcentrald_autotuner::config::TunerMode,
    step: serde_json::Value,
    runtime_command: serde_json::Value,
) -> serde_json::Value {
    let live_command_channel = runtime_command
        .get("channel_available")
        .and_then(|value| value.as_bool())
        .unwrap_or(false);
    let applied_runtime = runtime_command
        .get("applied_runtime")
        .and_then(|value| value.as_bool())
        .unwrap_or(false);

    serde_json::json!({
        "status": "ok",
        "action": action,
        "mode": mode,
        "step": step,
        "config_path": get_writable_config_path(),
        "applies": if applied_runtime {
            "persisted_and_applied_to_live_autotuner"
        } else {
            "next_autotuner_cycle_or_daemon_restart"
        },
        "live_command_channel": live_command_channel,
        "runtime_command": runtime_command,
        "runtime": runtime_command,
    })
}

pub(crate) async fn dispatch_autotuner_mode_command(
    state: &AppState,
    mode: dcentrald_autotuner::config::TunerMode,
) -> serde_json::Value {
    // W21 audit-coverage: every operator-initiated autotuner mode/target change
    // funnels through here (the REST increment/decrement/set-default endpoints
    // AND the cgminer/LuxOS autotunerset paths). Record it with the
    // purpose-built AutotunerProfileSelect event before dispatching to the live
    // runtime channel (the mode was already persisted by the caller, so this is
    // the right place to audit the operator's intent regardless of whether the
    // live channel is currently available).
    crate::push_audit_event(
        state,
        "rest_dashboard",
        dcentrald_api_types::audit_log::AuditEvent::AutotunerProfileSelect {
            profile_name: format!("{mode:?}"),
        },
    );

    let Some(tx) = &state.autotuner_command_tx else {
        return serde_json::json!({
            "channel_available": false,
            "accepted": false,
            "applied_runtime": false,
            "status": "unavailable",
            "message": "live autotuner command channel is not available in this runtime",
        });
    };

    let (ack_tx, ack_rx) = tokio::sync::oneshot::channel();
    let command = dcentrald_autotuner::AutoTunerCommand::ApplyMode { mode, ack_tx };
    if tx.send(command).await.is_err() {
        return serde_json::json!({
            "channel_available": false,
            "accepted": false,
            "applied_runtime": false,
            "status": "closed",
            "message": "live autotuner command channel is closed",
        });
    }

    match tokio::time::timeout(std::time::Duration::from_secs(2), ack_rx).await {
        Ok(Ok(result)) => serde_json::json!({
            "channel_available": true,
            "accepted": true,
            "applied_runtime": result.applied_runtime,
            "status": result.status,
            "message": result.message,
            "mode": result.mode,
        }),
        Ok(Err(_)) => serde_json::json!({
            "channel_available": false,
            "accepted": false,
            "applied_runtime": false,
            "status": "closed_before_ack",
            "message": "live autotuner command channel closed before acknowledgement",
        }),
        Err(_) => serde_json::json!({
            "channel_available": true,
            "accepted": true,
            "applied_runtime": false,
            "status": "ack_timeout",
            "message": "live autotuner command was sent but no acknowledgement arrived within 2s",
        }),
    }
}

pub(super) fn autotuner_dps_walker(
    config: &AutotunerTargetConfig,
) -> dcentrald_autotuner::dps::DpsWalkerConfig {
    dcentrald_autotuner::dps::DpsWalkerConfig {
        power_step_w: config.power_step_w,
        hashrate_step_ths: config.hashrate_step_ths,
        min_power_w: 200,
        max_power_w: config.max_power_w,
        min_hashrate_ths: 1.0,
        max_hashrate_ths: 300.0,
        high_performance_mode: config.dps_high_performance_mode,
    }
}

pub(super) fn dispatcher_limit_rank(source: &str) -> u8 {
    match source {
        "sensor_safety" => 0,
        "thermal" => 1,
        "off_grid" => 2,
        "power_cap" => 3,
        "quiet_mode" => 4,
        "fan_clamp" => 5,
        _ => 255,
    }
}

pub(super) fn dominant_dispatcher_limit(
    limits: &[dcentrald_autotuner::power_budget::DispatcherChainLimit],
) -> Option<String> {
    limits
        .iter()
        .filter_map(|limit| {
            Some((
                limit.dominant_source.as_deref()?,
                limit.effective_ceiling_mhz.unwrap_or(u16::MAX),
            ))
        })
        .min_by_key(|(source, ceiling)| (*ceiling, dispatcher_limit_rank(source)))
        .map(|(source, _)| source.to_string())
}

/// GET /api/autotuner/status -- Live autotuner runtime status.
pub(super) async fn get_autotuner_status(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let mode = *state.mode_rx.borrow();
    if let Err(resp) = crate::mode_middleware::check_mode_access("/api/autotuner/status", mode) {
        return resp.into_response();
    }

    let mut status = state.autotuner_status_rx.borrow().clone();
    let power = state.power_rx.borrow().clone();
    let (age_s, stale, live_runtime) =
        autotuner_runtime_freshness(status.last_update_s, status.live_runtime);
    status.age_s = age_s;
    status.stale = stale;
    status.live_runtime = live_runtime;

    if let Some(policy) = status.policy.as_mut() {
        if policy.active_limiting_factor.is_none() {
            policy.active_limiting_factor = dominant_dispatcher_limit(&power.dispatcher_limits);
        }
    }

    let mut response = serde_json::to_value(status)
        .unwrap_or_else(|_| serde_json::json!({ "error": "status_encode_failed" }));
    if let Some(obj) = response.as_object_mut() {
        obj.insert(
            "dispatcher_limits".to_string(),
            serde_json::to_value(power.dispatcher_limits).unwrap_or_else(|_| serde_json::json!([])),
        );
        obj.insert(
            "live_command_channel".to_string(),
            serde_json::json!(state
                .autotuner_command_tx
                .as_ref()
                .map(|tx| !tx.is_closed())
                .unwrap_or(false)),
        );
    }

    Json(response).into_response()
}

/// W1.3 — slim accessor for the active `TuneTarget` plus the per-mode
/// default that would apply if the operator hasn't customized it.
///
/// Lets the dashboard onboarding prompt and migration logic see what's
/// going to drive the autotuner without parsing the full /status payload.
/// Returns:
/// ```json
/// {
///   "active": "efficiency",
///   "operating_mode": "home",
///   "mode_default": "efficiency",
///   "is_mode_default": true
/// }
/// ```
pub(super) async fn get_autotuner_target(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let mode = *state.mode_rx.borrow();
    if let Err(resp) = crate::mode_middleware::check_mode_access("/api/autotuner/target", mode) {
        return resp.into_response();
    }

    let mode_str = mode.to_string();
    let mode_default = dcentrald_autotuner::config::TuneTarget::for_mode(&mode_str);
    let active = read_autotuner_target_mode_with_default(mode_default);

    let target_str = |t: dcentrald_autotuner::config::TuneTarget| -> &'static str {
        match t {
            dcentrald_autotuner::config::TuneTarget::Hashrate => "hashrate",
            dcentrald_autotuner::config::TuneTarget::Power => "power",
            dcentrald_autotuner::config::TuneTarget::Efficiency => "efficiency",
            dcentrald_autotuner::config::TuneTarget::HashrateTarget => "hashrate_target",
            dcentrald_autotuner::config::TuneTarget::EfficiencyJTH => "efficiency_jth",
        }
    };

    Json(serde_json::json!({
        "active": target_str(active),
        "operating_mode": mode_str,
        "mode_default": target_str(mode_default),
        "is_mode_default": active == mode_default,
    }))
    .into_response()
}

/// Read the persisted `target_mode` from the on-disk autotuner TOML
/// section, falling back to the supplied per-mode default when the field
/// is absent or invalid. Used by `/api/autotuner/target` so the dashboard
/// migration prompt can honestly distinguish "operator set this" from
/// "this is the mode default".
pub(super) fn read_autotuner_target_mode_with_default(
    fallback: dcentrald_autotuner::config::TuneTarget,
) -> dcentrald_autotuner::config::TuneTarget {
    let Ok(table) = load_config_table_for_write() else {
        return fallback;
    };
    let Some(autotuner) = table.get("autotuner").and_then(|value| value.as_table()) else {
        return fallback;
    };
    match autotuner
        .get("target_mode")
        .and_then(|value| value.as_str())
    {
        Some("hashrate") => dcentrald_autotuner::config::TuneTarget::Hashrate,
        Some("power") => dcentrald_autotuner::config::TuneTarget::Power,
        Some("efficiency") => dcentrald_autotuner::config::TuneTarget::Efficiency,
        Some("hashrate_target") => dcentrald_autotuner::config::TuneTarget::HashrateTarget,
        Some("efficiency_jth") => dcentrald_autotuner::config::TuneTarget::EfficiencyJTH,
        _ => fallback,
    }
}

pub(super) async fn put_autotuner_active(
    State(state): State<Arc<AppState>>,
    Json(mode): Json<dcentrald_autotuner::config::TunerMode>,
) -> impl IntoResponse {
    if let Err(response) = require_antminer_runtime_capability(
        &state,
        RuntimeCapability::AsicOptions,
        "/api/autotuner/active",
    ) {
        return response;
    }

    let operating_mode = *state.mode_rx.borrow();
    if let Err(resp) =
        crate::mode_middleware::check_mode_access("/api/autotuner/active", operating_mode)
    {
        return resp.into_response();
    }

    if let Err(message) = mode.validate() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "status": "error", "message": message })),
        )
            .into_response();
    }

    match persist_autotuner_mode(&mode) {
        Ok(()) => {
            let runtime_command = dispatch_autotuner_mode_command(&state, mode.clone()).await;
            Json(autotuner_mode_response(
                "set_active",
                &mode,
                serde_json::json!({
                    "source": "put_autotuner_active",
                    "mode": &mode,
                }),
                runtime_command,
            ))
            .into_response()
        }
        Err(message) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "status": "error", "message": message })),
        )
            .into_response(),
    }
}

pub(super) async fn post_autotuner_increment_power_target(
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    autotuner_power_target_step(state, true).await
}

pub(super) async fn post_autotuner_decrement_power_target(
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    autotuner_power_target_step(state, false).await
}

pub(super) async fn autotuner_power_target_step(state: Arc<AppState>, increment: bool) -> Response {
    let path = if increment {
        "/api/autotuner/increment_power_target"
    } else {
        "/api/autotuner/decrement_power_target"
    };
    if let Err(response) =
        require_antminer_runtime_capability(&state, RuntimeCapability::AsicOptions, path)
    {
        return response;
    }

    let mode = *state.mode_rx.borrow();
    if let Err(resp) = crate::mode_middleware::check_mode_access(path, mode) {
        return resp.into_response();
    }

    let config = read_autotuner_target_config();
    let walker = autotuner_dps_walker(&config);
    let current = if config.target_watts > 0 {
        config.target_watts
    } else if increment {
        walker.min_power_w
    } else {
        walker.max_power_w
    };
    let step_size = walker.effective_power_step_w();
    let desired = if increment {
        current.saturating_add(step_size)
    } else {
        current.saturating_sub(step_size)
    };
    let step = walker.walk_power_target(current, desired);
    let mode = dcentrald_autotuner::config::TunerMode::PowerTarget { watts: step.next };

    match persist_autotuner_mode(&mode) {
        Ok(()) => {
            let runtime_command = dispatch_autotuner_mode_command(&state, mode.clone()).await;
            Json(autotuner_mode_response(
                if increment {
                    "increment_power_target"
                } else {
                    "decrement_power_target"
                },
                &mode,
                serde_json::to_value(step).unwrap_or_else(|_| serde_json::json!({})),
                runtime_command,
            ))
            .into_response()
        }
        Err(message) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "status": "error", "message": message })),
        )
            .into_response(),
    }
}

pub(super) async fn post_autotuner_increment_hashrate_target(
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    autotuner_hashrate_target_step(state, true).await
}

pub(super) async fn post_autotuner_decrement_hashrate_target(
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    autotuner_hashrate_target_step(state, false).await
}

pub(super) async fn autotuner_hashrate_target_step(
    state: Arc<AppState>,
    increment: bool,
) -> Response {
    let path = if increment {
        "/api/autotuner/increment_hashrate_target"
    } else {
        "/api/autotuner/decrement_hashrate_target"
    };
    if let Err(response) =
        require_antminer_runtime_capability(&state, RuntimeCapability::AsicOptions, path)
    {
        return response;
    }

    let mode = *state.mode_rx.borrow();
    if let Err(resp) = crate::mode_middleware::check_mode_access(path, mode) {
        return resp.into_response();
    }

    let config = read_autotuner_target_config();
    let walker = autotuner_dps_walker(&config);
    let live_ths = state.state_rx.borrow().hashrate_ghs / 1000.0;
    let current = if config.target_hashrate_ths > 0.0 {
        config.target_hashrate_ths
    } else if live_ths.is_finite() && live_ths > 0.0 {
        live_ths
    } else if increment {
        walker.min_hashrate_ths
    } else {
        default_autotuner_hashrate_target_ths(&state)
    };
    let step_size = walker.effective_hashrate_step_ths();
    let desired = if increment {
        current + step_size
    } else {
        current - step_size
    };
    let step = walker.walk_hashrate_target(current, desired);
    let mode = dcentrald_autotuner::config::TunerMode::HashrateTarget { ths: step.next };

    match persist_autotuner_mode(&mode) {
        Ok(()) => {
            let runtime_command = dispatch_autotuner_mode_command(&state, mode.clone()).await;
            Json(autotuner_mode_response(
                if increment {
                    "increment_hashrate_target"
                } else {
                    "decrement_hashrate_target"
                },
                &mode,
                serde_json::to_value(step).unwrap_or_else(|_| serde_json::json!({})),
                runtime_command,
            ))
            .into_response()
        }
        Err(message) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "status": "error", "message": message })),
        )
            .into_response(),
    }
}

pub(super) fn default_autotuner_hashrate_target_ths(state: &AppState) -> f64 {
    let live_ths = state.state_rx.borrow().hashrate_ghs / 1000.0;
    if live_ths.is_finite() && live_ths > 0.0 {
        return live_ths;
    }

    let profiles = load_profiles(&state.profile_path);
    let saved_ghs: f64 = profiles
        .values()
        .map(|profile| profile.stats.estimated_hashrate_ghs)
        .sum();
    if saved_ghs.is_finite() && saved_ghs > 0.0 {
        return saved_ghs / 1000.0;
    }

    120.0
}

pub(super) async fn post_autotuner_set_default_hashrate_target(
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    if let Err(response) = require_antminer_runtime_capability(
        &state,
        RuntimeCapability::AsicOptions,
        "/api/autotuner/set_default_hashrate_target",
    ) {
        return response;
    }

    let mode = *state.mode_rx.borrow();
    if let Err(resp) = crate::mode_middleware::check_mode_access(
        "/api/autotuner/set_default_hashrate_target",
        mode,
    ) {
        return resp.into_response();
    }

    let default_ths = default_autotuner_hashrate_target_ths(&state);
    let mode = dcentrald_autotuner::config::TunerMode::HashrateTarget { ths: default_ths };
    match persist_autotuner_mode(&mode) {
        Ok(()) => {
            let runtime_command = dispatch_autotuner_mode_command(&state, mode.clone()).await;
            Json(autotuner_mode_response(
                "set_default_hashrate_target",
                &mode,
                serde_json::json!({
                    "current": default_ths,
                    "desired": default_ths,
                    "next": default_ths,
                    "direction": "hold",
                    "source": if state.state_rx.borrow().hashrate_ghs > 0.0 {
                        "live_hashrate"
                    } else {
                        "saved_profile_or_family_default"
                    }
                }),
                runtime_command,
            ))
            .into_response()
        }
        Err(message) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "status": "error", "message": message })),
        )
            .into_response(),
    }
}

/// GET /api/autotuner/tuned_profiles -- Saved profiles with MCR fit summaries.
pub(super) async fn get_autotuner_tuned_profiles(
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    let mode = *state.mode_rx.borrow();
    if let Err(resp) =
        crate::mode_middleware::check_mode_access("/api/autotuner/tuned_profiles", mode)
    {
        return resp.into_response();
    }

    let profiles = load_profiles(&state.profile_path);
    let (resume_state, resume_state_manifest) = read_autotuner_resume_state(&state.profile_path);
    let mut chain_ids: Vec<u8> = profiles.keys().copied().collect();
    chain_ids.sort_unstable();

    let chains: Vec<serde_json::Value> = chain_ids
        .iter()
        .filter_map(|chain_id| profiles.get(chain_id).map(|profile| (*chain_id, profile)))
        .map(|(chain_id, profile)| {
            serde_json::json!({
                "chain_id": chain_id,
                "chip_type": profile.chip_type,
                "chip_count": profile.chip_count,
                "voltage_mv": profile.voltage_mv,
                "optimal_voltage_mv": profile.optimal_voltage_mv,
                "avg_freq_mhz": profile.stats.avg_freq_mhz,
                "estimated_hashrate_ghs": profile.stats.estimated_hashrate_ghs,
                "estimated_power_w": profile.estimated_power_w,
                "estimated_efficiency_jth": profile.estimated_efficiency_jth,
                "mcr_fit": profile.mcr_fit_summary(),
                "resume_state": autotuner_resume_chain_summary(resume_state.as_ref(), chain_id),
                "tuned_at": profile.tuned_at,
            })
        })
        .collect();

    Json(serde_json::json!({
        "status": if chains.is_empty() { "saved_profile_unavailable" } else { "ok" },
        "profile_path": state.profile_path,
        "resume_state": resume_state_manifest,
        "chains": chains,
    }))
    .into_response()
}

pub(super) fn autotuner_profile_file_manifest(
    profile_path: &str,
    chain_id: u8,
    backup: bool,
) -> serde_json::Value {
    let filename = if backup {
        format!("autotune-chain{}.backup.json", chain_id)
    } else {
        format!("autotune-chain{}.json", chain_id)
    };
    let path = Path::new(profile_path).join(&filename);
    match std::fs::read_to_string(&path) {
        Ok(json) => match serde_json::from_str::<dcentrald_autotuner::TuningProfile>(&json) {
            Ok(profile) => serde_json::json!({
                "chain_id": chain_id,
                "file": filename,
                "present": true,
                "read_ok": true,
                "parse_ok": true,
                "chip_count": profile.chip_count,
                "tuned_at": profile.tuned_at,
                "avg_freq_mhz": profile.stats.avg_freq_mhz,
                "reason": if backup {
                    "Backup profile is readable and parseable."
                } else {
                    "Saved profile is readable and parseable."
                }
            }),
            Err(_) => serde_json::json!({
                "chain_id": chain_id,
                "file": filename,
                "present": true,
                "read_ok": true,
                "parse_ok": false,
                "chip_count": null,
                "tuned_at": null,
                "avg_freq_mhz": null,
                "reason": "Profile file exists but could not be parsed."
            }),
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => serde_json::json!({
            "chain_id": chain_id,
            "file": filename,
            "present": false,
            "read_ok": false,
            "parse_ok": false,
            "chip_count": null,
            "tuned_at": null,
            "avg_freq_mhz": null,
            "reason": if backup {
                "No backup profile exists for this chain."
            } else {
                "No saved profile exists for this chain."
            }
        }),
        Err(_) => serde_json::json!({
            "chain_id": chain_id,
            "file": filename,
            "present": true,
            "read_ok": false,
            "parse_ok": false,
            "chip_count": null,
            "tuned_at": null,
            "avg_freq_mhz": null,
            "reason": "Profile file exists but could not be read."
        }),
    }
}

/// GET /api/autotuner/visibility -- Read-only autotuner evidence and rollback readiness.
pub(super) async fn get_autotuner_visibility(
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    let mode = *state.mode_rx.borrow();
    if let Err(resp) = crate::mode_middleware::check_mode_access("/api/autotuner/visibility", mode)
    {
        return resp.into_response();
    }

    let mut status = state.autotuner_status_rx.borrow().clone();
    let power = state.power_rx.borrow().clone();
    let telemetry = state.autotuner_telemetry_rx.borrow().clone();
    let profiles = load_profiles(&state.profile_path);
    let (_resume_state, resume_state_manifest) = read_autotuner_resume_state(&state.profile_path);
    let (age_s, stale, live_runtime) =
        autotuner_runtime_freshness(status.last_update_s, status.live_runtime);
    status.age_s = age_s;
    status.stale = stale;
    status.live_runtime = live_runtime;

    let mut chain_ids: Vec<u8> = [6u8, 7, 8].to_vec();
    for chain_id in profiles.keys().copied() {
        if !chain_ids.contains(&chain_id) {
            chain_ids.push(chain_id);
        }
    }
    chain_ids.sort_unstable();

    let saved_profiles: Vec<serde_json::Value> = chain_ids
        .iter()
        .map(|&chain_id| autotuner_profile_file_manifest(&state.profile_path, chain_id, false))
        .collect();
    let rollback_backups: Vec<serde_json::Value> = chain_ids
        .iter()
        .map(|&chain_id| autotuner_profile_file_manifest(&state.profile_path, chain_id, true))
        .collect();
    let profile_count = saved_profiles
        .iter()
        .filter(|entry| {
            entry
                .get("parse_ok")
                .and_then(|value| value.as_bool())
                .unwrap_or(false)
        })
        .count();
    let backup_count = rollback_backups
        .iter()
        .filter(|entry| {
            entry
                .get("parse_ok")
                .and_then(|value| value.as_bool())
                .unwrap_or(false)
        })
        .count();
    let latest_run = telemetry.runs.last().map(|run| {
        serde_json::json!({
            "started_at_s": run.started_at,
            "duration_s": run.duration_s,
            "completed": run.completed,
            "sample_count": run.samples.len(),
        })
    });

    Json(serde_json::json!({
        "schema": "dcentos.autotuner.visibility.v1",
        "status": "ok",
        "read_only": true,
        "control_actions": false,
        "hardware_writes": false,
        "filesystem_mutation": false,
        "generated_at_s": unix_time_ms() / 1000,
        "source": "existing_autotuner_state",
        "fetched_at_ms": unix_time_ms(),
        "runtime": {
            "available": live_runtime,
            "enabled": status.enabled,
            "state": status.state,
            "phase": status.phase,
            "source": status.source,
            "stale": stale,
            "age_s": age_s,
            "message": status.message,
            "dispatcher_limits_visible": true,
            "dispatcher_limit_count": power.dispatcher_limits.len(),
        },
        "saved_profiles": {
            "available": profile_count > 0,
            "chains_with_profiles": profile_count,
            "expected_chains": chain_ids.len(),
            "entries": saved_profiles,
            "reason": if profile_count > 0 {
                "Saved autotuner profiles were found and parsed."
            } else {
                "No parseable saved autotuner profiles are present."
            }
        },
        "resume_state": resume_state_manifest,
        "telemetry": {
            "available": telemetry.recording || !telemetry.runs.is_empty(),
            "live_runtime": telemetry.live_runtime,
            "recording": telemetry.recording,
            "run_count": telemetry.runs.len(),
            "last_update_s": telemetry.last_update_s,
            "csv_available": !telemetry.runs.is_empty(),
            "json_endpoint": "/api/autotuner/telemetry",
            "csv_endpoint": "/api/autotuner/telemetry/csv",
            "latest_run": latest_run,
            "reason": telemetry.message,
        },
        "rollback": {
            "available": backup_count > 0,
            "backup_profiles": rollback_backups,
            "backup_profile_count": backup_count,
            "config_visible": false,
            "automatic_rollback_visible": false,
            "reason": if backup_count > 0 {
                "One or more rollback backup profiles are readable and parseable."
            } else {
                "No parseable rollback backup profiles are present. Autotuner config flags are not exposed through this read-only API state yet."
            }
        },
        "simulation": {
            "available": false,
            "simulation_only": false,
            "reason": "No production autotune simulator is wired. This endpoint reports evidence only and does not simulate tuning outcomes."
        },
        "limitations": [
            "This endpoint is read-only and does not apply profiles, change frequency, change voltage, restart services, or write files.",
            "Rollback configuration flags are not yet exposed in API state; this loop reports only backup-profile evidence.",
            "Saved profile data is loaded from existing profile files and may be stale relative to live hardware.",
            "Telemetry availability depends on the runtime autotuner publishing telemetry."
        ]
    }))
    .into_response()
}

/// GET /api/autotuner/saved-status -- Saved autotuner profile summary.
///
/// Loads all chain profiles from disk and returns aggregate tuning data:
/// chip count, average frequency, grade distribution, voltage, power estimates.
pub(super) async fn get_autotuner_saved_status(
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    let mode = *state.mode_rx.borrow();
    if let Err(resp) =
        crate::mode_middleware::check_mode_access("/api/autotuner/saved-status", mode)
    {
        return resp.into_response();
    }

    let profiles = load_profiles(&state.profile_path);

    if profiles.is_empty() {
        let hardware_policy = state
            .hardware_info
            .lock()
            .ok()
            .and_then(|hw| hw.autotuner.clone());
        return Json(serde_json::json!({
            "status": "saved_profile_unavailable",
            "live_runtime": false,
            "message": "No saved autotuner profiles found. Use /api/autotuner/status for live runtime state.",
            "policy": hardware_policy,
            "chains": [],
        })).into_response();
    }

    let mut chains_json = Vec::new();
    let mut total_chips: u16 = 0;
    let mut total_freq_sum: f64 = 0.0;
    let mut total_grades = [0u16; 4];

    let mut chain_ids: Vec<u8> = profiles.keys().copied().collect();
    chain_ids.sort();

    for &chain_id in &chain_ids {
        let p = &profiles[&chain_id];
        total_chips += p.chip_count as u16;
        total_freq_sum += p.stats.avg_freq_mhz * p.chip_count as f64;
        total_grades[0] += p.stats.grade_a;
        total_grades[1] += p.stats.grade_b;
        total_grades[2] += p.stats.grade_c;
        total_grades[3] += p.stats.grade_d;

        chains_json.push(serde_json::json!({
            "chain_id": chain_id,
            "chip_count": p.chip_count,
            "avg_freq_mhz": p.stats.avg_freq_mhz,
            "min_freq_mhz": p.stats.min_freq_mhz,
            "max_freq_mhz": p.stats.max_freq_mhz,
            "voltage_mv": p.voltage_mv,
            "optimal_voltage_mv": p.optimal_voltage_mv,
            "grades": {
                "A": p.stats.grade_a,
                "B": p.stats.grade_b,
                "C": p.stats.grade_c,
                "D": p.stats.grade_d,
            },
            "estimated_power_w": p.estimated_power_w,
            "estimated_efficiency_jth": p.estimated_efficiency_jth,
            "tuned_at": p.tuned_at,
        }));
    }

    let overall_avg_freq = if total_chips > 0 {
        total_freq_sum / total_chips as f64
    } else {
        0.0
    };

    let saved_policy = state
        .autotuner_status_rx
        .borrow()
        .policy
        .clone()
        .or_else(|| {
            state
                .hardware_info
                .lock()
                .ok()
                .and_then(|hw| hw.autotuner.clone())
        });

    Json(serde_json::json!({
        "status": "saved_profile_summary",
        "live_runtime": false,
        "message": "This endpoint summarizes saved autotuner profiles from disk, not live runtime tuner state.",
        "policy": saved_policy,
        "total_chips": total_chips,
        "chains_tuned": chain_ids.len(),
        "avg_freq_mhz": overall_avg_freq,
        "grades": {
            "A": total_grades[0],
            "B": total_grades[1],
            "C": total_grades[2],
            "D": total_grades[3],
        },
        "chains": chains_json,
    }))
    .into_response()
}

/// GET /api/autotuner/silicon-report -- Full silicon quality analytics.
///
/// Generates a comprehensive silicon quality report with quality scoring,
/// grade distribution, frequency statistics, and per-chain breakdowns.
///
/// PURE TELEMETRY (read-only). The A/B/C/D grade distribution is the
/// effective grade — each chip's stored frequency-bin grade refined by its
/// measured error-rate and nonce count (see
/// `dcentrald_autotuner::SiliconReport`). When tuning profiles exist on disk
/// but no chip has measured nonce data yet, the report returns
/// `characterized: false` + `quality_tier: "Not Characterized"` (HTTP 200)
/// rather than fabricating a quality verdict; a fully-absent profile set still
/// returns 404 `no_profiles` ("run tuning first"). Drives no hardware.
pub(super) async fn get_autotuner_silicon_report(
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    let mode = *state.mode_rx.borrow();
    if let Err(resp) =
        crate::mode_middleware::check_mode_access("/api/autotuner/silicon-report", mode)
    {
        return resp.into_response();
    }

    let profiles = load_profiles(&state.profile_path);

    if profiles.is_empty() {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({
                "error": "no_profiles",
                "message": "No autotuner profiles found — run tuning first",
            })),
        )
            .into_response();
    }

    let report = dcentrald_autotuner::SiliconReport::generate(&profiles);
    Json(report).into_response()
}

/// GET /api/autotuner/chip-health -- Per-chip health status.
///
/// Creates a health tracker from saved profiles and returns baseline health
/// for all chips. For live EMA data, the background monitor must be running.
pub(super) async fn get_autotuner_chip_health(
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    let mode = *state.mode_rx.borrow();
    if let Err(resp) = crate::mode_middleware::check_mode_access("/api/autotuner/chip-health", mode)
    {
        return resp.into_response();
    }

    if let Some(runtime) = state.autotuner_chip_health_rx.borrow().clone() {
        let last_update_s = runtime.last_update_s;
        let (age_s, stale, live_runtime) = autotuner_runtime_freshness(last_update_s, true);
        let chips = runtime.chips;
        return Json(serde_json::json!({
            "total_chips": chips.len(),
            "chips": chips,
            "source": "runtime",
            "live_runtime": live_runtime,
            "message": "Chip health is sourced from the live autotuner background monitor.",
            "last_update_s": last_update_s,
            "age_s": age_s,
            "stale": stale,
        }))
        .into_response();
    }

    let profiles = load_profiles(&state.profile_path);

    if profiles.is_empty() {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({
                "error": "no_profiles",
                "message": "No autotuner profiles found — run tuning first",
            })),
        )
            .into_response();
    }

    let tracker = dcentrald_autotuner::ChipHealthTracker::new(&profiles);
    let statuses = tracker.all_statuses();

    Json(serde_json::json!({
        "source": "saved_profile_baseline",
        "live_runtime": false,
        "stale": false,
        "age_s": 0,
        "last_update_s": 0,
        "message": "Chip health is reconstructed from saved profiles because no live autotuner background data is available.",
        "total_chips": statuses.len(),
        "chips": statuses,
    }))
    .into_response()
}

/// GET /api/chips -- Per-chip telemetry snapshot (Wave D RE-010 closure).
///
/// Live drill-in surface for the per-chip `ChipMapCell` data already exposed
/// (aggregated) by `/api/status` and (diagnostic snapshot) by
/// `POST /api/diagnostics/chip-health/start`. This is the GET equivalent —
/// request-time snapshot, no persisted artifact, no `report_id` consumption.
///
/// Reuses `build_chip_health_snapshot()` (the same data source as the
/// diagnostic POST) for zero duplication. Optional `?chain=N` filter scopes
/// to a single chain; omit for all chains.
///
/// Returns the existing `ChipHealthSnapshot` JSON shape (with `chains[].chipmap.cells[]`
/// containing the per-chip detail). No new DTO layer — the existing snapshot
/// type is already operator-facing per the RE-010 corpus resolution.
///
/// §RE-010.
pub(super) async fn get_chips(
    State(state): State<Arc<AppState>>,
    Query(query): Query<ChipsQuery>,
) -> impl IntoResponse {
    let mode = *state.mode_rx.borrow();
    if let Err(resp) = crate::mode_middleware::check_mode_access("/api/chips", mode) {
        return resp.into_response();
    }
    let context = snapshot_context(&state);
    let report = build_chip_health_snapshot(&context, query.chain);
    Json(report).into_response()
}

/// POST /api/autotuner/fleet-profile/export -- Export fleet profile.
///
/// Packages tuning data from all chains into a portable fleet profile
/// that can be transferred to identical miners.
pub(super) async fn post_autotuner_fleet_export(
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    let profiles = load_profiles(&state.profile_path);

    if profiles.is_empty() {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({
                "error": "no_profiles",
                "message": "No autotuner profiles found — run tuning first",
            })),
        )
            .into_response();
    }

    let hostname = std::fs::read_to_string("/etc/hostname")
        .unwrap_or_else(|_| "dcentos".to_string())
        .trim()
        .to_string();

    let fleet = dcentrald_autotuner::FleetProfile::export(&profiles, &hostname);
    Json(fleet).into_response()
}

// ─── New Autotuner Endpoints (Best-in-Class Features) ──────────────────

/// GET /api/autotuner/efficiency -- Real-time per-chip efficiency heat map.
///
/// Produces a full EfficiencySnapshot with per-chip power, hashrate, J/TH,
/// health score, and grade. Designed for the dashboard's 189-chip heat map.
/// No competitor exposes per-chip efficiency data via API.
pub(super) async fn get_autotuner_efficiency(
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    if let Some(snapshot) = state.autotuner_efficiency_rx.borrow().clone() {
        let mut body = serde_json::to_value(&snapshot).unwrap_or_else(|_| serde_json::json!({}));
        if let Some(obj) = body.as_object_mut() {
            insert_autotuner_runtime_meta(
                obj,
                snapshot.timestamp,
                "runtime",
                true,
                "Efficiency snapshot is sourced from the live autotuner background monitor.",
            );
            // The snapshot's watts are model-derived even on the live-runtime
            // path; surface the provenance next to the `source: "runtime"`
            // label so it is never read as a measured wattage.
            insert_efficiency_power_provenance(obj);
        }
        return Json(body).into_response();
    }

    let profiles = load_profiles(&state.profile_path);

    if profiles.is_empty() {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({
                "error": "no_profiles",
                "message": "No autotuner profiles — run tuning first",
            })),
        )
            .into_response();
    }

    // Thread the operator wall-meter calibration into the estimate-only path
    // instead of a hardcoded 1.0 scale, so a calibrated unit's saved-profile
    // efficiency matches its meter.
    let calibration = state
        .power_calibration
        .read()
        .map(|cal| cal.clone())
        .unwrap_or_default();
    let snapshot = saved_profile_efficiency_snapshot(&profiles, &calibration);
    let mut body = serde_json::to_value(snapshot).unwrap_or_else(|_| serde_json::json!({}));
    if let Some(obj) = body.as_object_mut() {
        insert_autotuner_runtime_meta(
            obj,
            0,
            "saved_profile_estimate",
            false,
            "Efficiency is estimated from saved profiles, not the live autotuner runtime.",
        );
        insert_efficiency_power_provenance(obj);
    }
    Json(body).into_response()
}

/// GET /api/autotuner/telemetry -- Export tuning telemetry as JSON.
///
/// Returns the last 3 tuning runs with per-chip, per-window time-series data.
/// Includes nonces, errors, frequencies, temperatures, and tuner decisions.
/// Designed for MCP AI optimization and remote debugging.
pub(super) async fn get_autotuner_telemetry(
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    let telemetry = state.autotuner_telemetry_rx.borrow().clone();
    let mut body = serde_json::to_value(&telemetry).unwrap_or_else(|_| serde_json::json!({}));
    if let Some(obj) = body.as_object_mut() {
        insert_autotuner_runtime_meta(
            obj,
            telemetry.last_update_s,
            if telemetry.live_runtime {
                "runtime"
            } else {
                "runtime_unavailable"
            },
            telemetry.live_runtime,
            &telemetry.message,
        );
    }
    Json(body).into_response()
}

/// GET /api/autotuner/telemetry/csv -- Export last tuning run as CSV.
///
/// Format: elapsed_s,chain_id,chip_index,nonces,errors,freq_mhz,board_temp_c,state,difficulty
/// Designed for spreadsheet analysis and external data pipelines.
pub(super) async fn get_autotuner_telemetry_csv(
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    let telemetry = state.autotuner_telemetry_rx.borrow().clone();
    if telemetry.runs.is_empty() {
        return (
            StatusCode::NOT_FOUND,
            [("content-type", "application/json")],
            serde_json::json!({
                "error": "no_runs",
                "message": telemetry.message,
            })
            .to_string(),
        )
            .into_response();
    }

    (
        StatusCode::OK,
        [("content-type", "text/csv; charset=utf-8")],
        dcentrald_autotuner::export_runs_csv(&telemetry.runs),
    )
        .into_response()
}

/// Request body for profitability calculation.
#[derive(Debug, Deserialize)]
pub struct ProfitabilityRequest {
    pub electricity_cost_kwh: f64,
    pub btc_price_usd: f64,
    pub network_difficulty: f64,
}

/// POST /api/autotuner/profitability -- Calculate mining profitability.
///
/// Given electricity cost, BTC price, and network difficulty, returns
/// daily/monthly profit estimates based on current tuning profile.
/// Space Home mode shows this as "heating cost offset."
pub(super) async fn post_autotuner_profitability(
    State(state): State<Arc<AppState>>,
    Json(req): Json<ProfitabilityRequest>,
) -> impl IntoResponse {
    // Input validation (SE CRITICAL-2, RE WARNING-4)
    if req.electricity_cost_kwh < 0.0 {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": "invalid_input",
                "message": "electricity_cost_kwh must be >= 0",
            })),
        )
            .into_response();
    }
    if req.btc_price_usd <= 0.0 || req.btc_price_usd.is_nan() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": "invalid_input",
                "message": "btc_price_usd must be > 0",
            })),
        )
            .into_response();
    }
    if req.network_difficulty <= 0.0 || req.network_difficulty.is_nan() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": "invalid_input",
                "message": "network_difficulty must be > 0",
            })),
        )
            .into_response();
    }

    let profiles = load_profiles(&state.profile_path);

    if profiles.is_empty() {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({
                "error": "no_profiles",
                "message": "No autotuner profiles found — run tuning first",
            })),
        )
            .into_response();
    }

    // Thread the operator wall-meter calibration into the profitability power
    // basis instead of a hardcoded 1.0 scale, so daily cost/profit reflect the
    // operator's meter rather than raw modeled watts.
    let calibration = state
        .power_calibration
        .read()
        .map(|cal| cal.clone())
        .unwrap_or_default();
    let snapshot = saved_profile_efficiency_snapshot(&profiles, &calibration);

    let mut estimate = dcentrald_autotuner::estimate_profitability(
        snapshot.total_hashrate_ghs / 1000.0,
        snapshot.total_power_w,
        req.electricity_cost_kwh,
        req.btc_price_usd,
        req.network_difficulty,
    );
    // Provenance: the watts are always modeled; note when an operator
    // wall-meter calibration scaled them, so the daily cost is never read as a
    // metered figure.
    estimate.power_basis = if calibration.is_active() {
        "calibrated_model".to_string()
    } else {
        "model".to_string()
    };

    Json(estimate).into_response()
}

/// Request body for noise profile computation.
#[derive(Debug, Deserialize)]
pub struct NoiseProfileRequest {
    pub max_noise_db: f32,
}

/// POST /api/autotuner/noise-profile -- Compute noise-aware optimization.
///
/// Disabled for AM2/XIL until the daemon has tach/RPM-backed acoustic
/// calibration. PWM-to-dB tables from S9 must not be used as proof that an
/// S19j Pro home unit is quiet.
pub(super) async fn post_autotuner_noise_profile(
    Json(req): Json<NoiseProfileRequest>,
) -> impl IntoResponse {
    (
        StatusCode::NOT_IMPLEMENTED,
        Json(serde_json::json!({
            "error": "noise_profile_requires_tach_calibration",
            "message": "Noise-profile tuning is disabled until live fan RPM/acoustic calibration is available. PWM-derived dB is not valid proof on AM2/XIL.",
            "requested_max_noise_db": req.max_noise_db,
        })),
    )
        .into_response()
}

/// Request body for room temperature feedback.
#[derive(Debug, Deserialize)]
pub struct RoomTempFeedbackRequest {
    pub room_temp_c: f32,
    pub target_temp_c: f32,
    pub hysteresis_c: Option<f32>,
}

/// POST /api/autotuner/room-temp-factor -- Get power scaling for room temp.
///
/// Used by Home Assistant integration: reports room temperature, gets back
/// the power scaling factor. Miner throttles when room reaches target temp.
/// The miner IS the thermostat.
pub(super) async fn post_autotuner_room_temp_factor(
    Json(req): Json<RoomTempFeedbackRequest>,
) -> impl IntoResponse {
    let hysteresis = req.hysteresis_c.unwrap_or(2.0);
    let factor =
        dcentrald_autotuner::room_temp_power_factor(req.room_temp_c, req.target_temp_c, hysteresis);

    Json(serde_json::json!({
        "room_temp_c": req.room_temp_c,
        "target_temp_c": req.target_temp_c,
        "hysteresis_c": hysteresis,
        "power_factor": factor,
        "action": if factor < 0.2 { "throttle" } else if factor < 1.0 { "reduce" } else { "full_power" },
    })).into_response()
}

// ─── Authentication Handlers ─────────────────────────────────────────

/// GET /api/auth/status -- Check if password is configured.
pub(super) async fn get_auth_status() -> impl IntoResponse {
    Json(serde_json::json!({
        "password_set": crate::auth::is_password_set(),
        "active_sessions": crate::auth::active_session_count(),
        "auth_required": true,
    }))
}

/// Request body for password setup.
#[derive(Debug, Deserialize)]
pub struct AuthSetupRequest {
    pub password: String,
}

/// Request body for issuing a revocable dashboard session.
#[derive(Debug, Deserialize)]
pub struct AuthSessionRequest {
    pub password: String,
    pub label: Option<String>,
}

pub(super) fn session_response(issued: &crate::auth::IssuedSession) -> serde_json::Value {
    serde_json::json!({
        "session_id": issued.id,
        "session_token": issued.token,
        // Backward-compatible alias for one release while the dashboard migrates.
        "api_token": issued.token,
        "expires_at": issued.expires_at,
    })
}

/// POST /api/auth/setup -- Set initial password (only when no password exists).
///
/// SECURITY (2026-04-11): Rate-limited to 3 attempts per 60 seconds per IP
/// to prevent brute-force password setting on first boot.
pub(super) async fn post_auth_setup(
    ConnectInfo(addr): ConnectInfo<std::net::SocketAddr>,
    Json(body): Json<AuthSetupRequest>,
) -> impl IntoResponse {
    // Rate limit check
    if let Err(response) = crate::auth::check_setup_rate_limit(addr.ip()) {
        return response.into_response();
    }

    if crate::auth::is_password_set() {
        return (
            StatusCode::CONFLICT,
            Json(serde_json::json!({
                "error": "Password already configured",
                "detail": "Use the physical reset button (hold 15s) to reset the password",
            })),
        )
            .into_response();
    }

    if body.password.len() < 8 {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": "Password too short",
                "detail": "Password must be at least 8 characters",
            })),
        )
            .into_response();
    }

    let hash = crate::auth::hash_password(&body.password);
    let mut auth_data = crate::auth::AuthData {
        version: 2,
        password_hash: hash,
        api_token: None,
        sessions: Vec::new(),
    };
    let issued = crate::auth::issue_session(&mut auth_data, Some("dashboard-setup"));

    match crate::auth::save_auth(&auth_data) {
        Ok(()) => {
            let session_id = issued.id.clone();
            let session_token = issued.token.clone();
            if let Err(e) = update_onboarding_state(|state| {
                state.auth.password_set = true;
                state.auth.token_issued = true;
                state.trust.credentials_rotated = true;
            }) {
                tracing::warn!(error = %e, "Failed to update onboarding state after password setup");
            }
            tracing::info!("Initial admin password configured");
            // Defense-in-depth: the response body carries a bearer token —
            // forbid any intermediary/browser cache from storing it.
            (
                [(axum::http::header::CACHE_CONTROL, "no-store")],
                Json(serde_json::json!({
                    "status": "ok",
                    "message": "Password configured successfully",
                    "session": session_response(&issued),
                    "session_id": session_id,
                    "session_token": session_token.clone(),
                    "api_token": session_token,
                })),
            )
                .into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "Failed to save auth data");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({
                    "error": "Failed to save credentials",
                    "detail": e.to_string(),
                })),
            )
                .into_response()
        }
    }
}

/// POST /api/auth/session -- Verify password and issue a revocable session token.
pub(super) async fn post_auth_session(
    ConnectInfo(addr): ConnectInfo<std::net::SocketAddr>,
    Json(body): Json<AuthSessionRequest>,
) -> impl IntoResponse {
    // C1 (GROUP-C HIGH, W9): per-IP login brute-force rate limit — checked before
    // any password work, mirroring post_auth_setup's check_setup_rate_limit. Without
    // this call site the limiter in auth.rs is inert (W9 review C1-INERT).
    if let Err(response) = crate::auth::check_login_rate_limit(addr.ip()) {
        return response.into_response();
    }
    if body.password.len() < 8 {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": "Password too short",
                "detail": "Password must be at least 8 characters",
            })),
        )
            .into_response();
    }

    let mut auth_data = match crate::auth::load_auth() {
        Some(auth) => auth,
        None => {
            return (
                StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({
                    "error": "Password not configured",
                    "detail": "Set the owner password via POST /api/auth/setup first",
                })),
            )
                .into_response()
        }
    };

    if !crate::auth::verify_password(&body.password, &auth_data.password_hash) {
        crate::auth::record_login_failure(addr.ip());
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({
                "error": "Unauthorized",
                "detail": "Invalid credentials",
            })),
        )
            .into_response();
    }

    // Auth succeeded — clear this IP's failure streak so a legitimate user is not
    // throttled by earlier typos (W9 C1 brute-force limiter).
    crate::auth::record_login_success(addr.ip());
    let issued = crate::auth::issue_session(&mut auth_data, body.label.as_deref());

    match crate::auth::save_auth(&auth_data) {
        Ok(()) => {
            let session_id = issued.id.clone();
            let session_token = issued.token.clone();
            if let Err(e) = update_onboarding_state(|state| {
                state.auth.token_issued = true;
            }) {
                tracing::warn!(error = %e, "Failed to update onboarding state after session creation");
            }

            // Defense-in-depth: the response body carries a bearer token —
            // forbid any intermediary/browser cache from storing it.
            (
                [(axum::http::header::CACHE_CONTROL, "no-store")],
                Json(serde_json::json!({
                    "status": "ok",
                    "message": "Session created",
                    "session": session_response(&issued),
                    "session_id": session_id,
                    "session_token": session_token.clone(),
                    "api_token": session_token,
                })),
            )
                .into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "Failed to save auth data after session creation");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({
                    "error": "Failed to create session",
                    "detail": e.to_string(),
                })),
            )
                .into_response()
        }
    }
}

/// POST /api/auth/ws-ticket -- Mint a short-lived one-time WebSocket ticket.
///
/// Protected by the normal bearer middleware. The ticket path is default-off
/// (`[api].websocket_tickets = false`) so the existing `?token=` browser
/// compatibility path remains the default contract.
pub(super) async fn post_auth_ws_ticket(headers: HeaderMap) -> impl IntoResponse {
    let authorization = headers
        .get("authorization")
        .and_then(|value| value.to_str().ok());
    match crate::auth::issue_ws_ticket(authorization) {
        Ok(issued) => (
            [(axum::http::header::CACHE_CONTROL, "no-store")],
            Json(serde_json::json!({
                "status": "ok",
                "ticket": issued.ticket,
                "expires_in_s": issued.expires_in_s,
                "query_param": "ticket",
            })),
        )
            .into_response(),
        Err(response) => response.into_response(),
    }
}

/// DELETE /api/auth/session/current -- Revoke the caller's current bearer session.
pub(super) async fn delete_auth_session_current(headers: HeaderMap) -> impl IntoResponse {
    let authorization = headers
        .get("authorization")
        .and_then(|value| value.to_str().ok());

    let session_id = match crate::auth::current_session_id(authorization) {
        Ok(session_id) => session_id,
        Err(response) => return response.into_response(),
    };

    let mut auth_data = match crate::auth::load_auth() {
        Some(auth) => auth,
        None => {
            return (
                StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({
                    "error": "Password not configured",
                    "detail": "Set the owner password via POST /api/auth/setup first",
                })),
            )
                .into_response()
        }
    };

    if !crate::auth::revoke_session(&mut auth_data, &session_id) {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({
                "error": "Session not found",
                "detail": "The requested session is already revoked or does not exist",
            })),
        )
            .into_response();
    }

    match crate::auth::save_auth(&auth_data) {
        Ok(()) => {
            if let Err(e) = update_onboarding_state(|state| {
                state.auth.token_issued = crate::auth::has_active_sessions();
            }) {
                tracing::warn!(error = %e, "Failed to update onboarding state after session revocation");
            }

            Json(serde_json::json!({
                "status": "ok",
                "message": "Session revoked",
                "session_id": session_id,
                "active_sessions": crate::auth::active_session_count(),
            }))
            .into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "Failed to save auth data after session revocation");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({
                    "error": "Failed to revoke session",
                    "detail": e.to_string(),
                })),
            )
                .into_response()
        }
    }
}

// ─── Safety Warning Handlers ─────────────────────────────────────────

pub(super) const SAFETY_ACK_FILE: &str = "/data/dcent/safety_ack.json";
pub(super) const SETUP_FILE: &str = "/data/dcent/setup.json";
pub(super) const ONBOARDING_FILE: &str = "/data/dcent/onboarding.json";
pub(super) const INSTALL_INTENT_FILE: &str = "/data/dcent/install_intent.json";

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub(super) struct OnboardingAuthState {
    password_set: bool,
    token_issued: bool,
    /// Operator-chosen freedom path: the owner deliberately declined to set
    /// a dashboard password and accepted running with the default (no owner
    /// password). This stops the onboarding nag (`onboarding_is_complete`)
    /// but does NOT widen write access — `auth.rs::is_pre_setup_safe` still
    /// restricts a passwordless unit to read-only GETs. serde-default so old
    /// onboarding.json files deserialize cleanly (defaults to false).
    password_opt_out: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub(super) struct OnboardingTrustState {
    install_origin: String,
    bootstrap_transport: String,
    hardening_profile: String,
    credentials_rotated: bool,
    ssh_keys_enrolled: bool,
    password_auth_disabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub(super) struct OnboardingStepState {
    safety_ack: bool,
    /// Operator-chosen freedom path: the owner deliberately declined to
    /// complete the circuit/breaker/safety acknowledgement and accepted
    /// running without it. This stops the onboarding nag
    /// (`onboarding_is_complete`) but changes NOTHING about access — the
    /// dashboard + logs were already reachable on a passwordless unit via
    /// `auth.rs::is_pre_setup_safe` (read-only GETs); all write/control
    /// endpoints still require a password regardless of this flag. The
    /// parallel of `OnboardingAuthState::password_opt_out`. serde-default
    /// so old onboarding.json files deserialize cleanly (defaults to
    /// false — never an accidental opt-out on upgrade).
    safety_opt_out: bool,
    circuit_configured: bool,
    mode_configured: bool,
    pool_configured: bool,
    complete: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub(super) struct OnboardingState {
    version: u8,
    phase: String,
    created_at: String,
    updated_at: String,
    completed_at: Option<String>,
    power_source: String,
    auth: OnboardingAuthState,
    trust: OnboardingTrustState,
    steps: OnboardingStepState,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub(super) struct OnboardingPoolSnapshot {
    url: String,
    worker: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub(super) struct OnboardingCurrentSnapshot {
    hostname: String,
    mode: String,
    power_source: String,
    circuit_voltage_v: Option<u16>,
    circuit_amperage_a: Option<u16>,
    pool: OnboardingPoolSnapshot,
    // P2-4 (§4.E): the daemon-persisted electricity economics, surfaced so the
    // setup wizard resumes with the real rate/currency and can show whether the
    // operator has already confirmed one (`electricity_rate_calibrated`).
    electricity_rate: f64,
    currency: String,
    electricity_rate_calibrated: bool,
}

impl Default for OnboardingState {
    fn default() -> Self {
        Self {
            version: 1,
            phase: "first_boot".to_string(),
            created_at: String::new(),
            updated_at: String::new(),
            completed_at: None,
            power_source: String::new(),
            auth: OnboardingAuthState::default(),
            trust: OnboardingTrustState {
                install_origin: "unknown".to_string(),
                bootstrap_transport: "unknown".to_string(),
                hardening_profile: "standard".to_string(),
                credentials_rotated: false,
                ssh_keys_enrolled: false,
                password_auth_disabled: false,
            },
            steps: OnboardingStepState::default(),
        }
    }
}

pub(super) fn read_json_value(path: &str) -> Option<serde_json::Value> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
}

pub(super) fn read_safety_acknowledged() -> bool {
    read_json_value(SAFETY_ACK_FILE)
        .and_then(|v| v.get("acknowledged").and_then(|a| a.as_bool()))
        .unwrap_or(false)
}

pub(super) fn read_legacy_setup_completed_at() -> Option<String> {
    read_json_value(SETUP_FILE).and_then(|v| {
        v.get("completed_at")
            .and_then(|ts| ts.as_str())
            .map(|s| s.to_string())
    })
}

pub(super) fn read_install_intent_field(path: &str) -> Option<String> {
    read_json_value(INSTALL_INTENT_FILE).and_then(|v| {
        v.get(path)
            .and_then(|value| value.as_str())
            .map(|s| s.to_string())
    })
}

pub(super) fn config_string(table: &toml::Table, section: &str, key: &str) -> String {
    table
        .get(section)
        .and_then(|value| value.as_table())
        .and_then(|section| section.get(key))
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string()
}

pub(super) fn config_u16(table: &toml::Table, section: &str, key: &str) -> Option<u16> {
    table
        .get(section)
        .and_then(|value| value.as_table())
        .and_then(|section| section.get(key))
        .and_then(|value| value.as_integer())
        .and_then(|value| u16::try_from(value).ok())
}

pub(super) fn config_bool(table: &toml::Table, section: &str, key: &str) -> Option<bool> {
    table
        .get(section)
        .and_then(|value| value.as_table())
        .and_then(|section| section.get(key))
        .and_then(|value| value.as_bool())
}

pub(super) fn config_f32(table: &toml::Table, section: &str, key: &str) -> Option<f32> {
    table
        .get(section)
        .and_then(|value| value.as_table())
        .and_then(|section| section.get(key))
        .and_then(|value| {
            value
                .as_float()
                .map(|v| v as f32)
                .or_else(|| value.as_integer().map(|v| v as f32))
        })
}

pub(super) fn shared_primary_pool(
    table: &toml::Table,
    miner: &crate::MinerState,
) -> SharedPoolConfig {
    let configured = read_configured_pool();
    let url = configured
        .as_ref()
        .map(|pool| pool.url.clone())
        .unwrap_or_else(|| miner.pool.url.clone());
    // SEC (W20 / parity #66): this URL is served to clients via
    // GET /api/config/shared — strip any inline stratum credentials. host:port
    // is preserved, so the port parse below is unaffected.
    let url = dcentrald_stratum::pool_api::sanitize_pool_url(&url);
    let port = parse_pool_host_port(&url).ok().map(|(_, port)| port);
    let password_set = table
        .get("pool")
        .and_then(|value| value.as_table())
        .and_then(|pool| pool.get("password"))
        .and_then(|value| value.as_str())
        .map(|password| !password.trim().is_empty())
        .unwrap_or(false);
    SharedPoolConfig {
        url,
        port,
        worker: configured
            .as_ref()
            .map(|pool| pool.worker.clone())
            .unwrap_or_default(),
        password_set,
        protocol: configured
            .as_ref()
            .and_then(|pool| pool.protocol.clone())
            .or_else(|| Some(miner.pool.protocol.clone())),
        enabled: true,
    }
}

pub(super) fn shared_config_snapshot(
    state: &AppState,
    table: &toml::Table,
    miner: &crate::MinerState,
    hw: &crate::HardwareInfo,
) -> SharedConfigSnapshot {
    let hostname = config_string(table, "general", "hostname");
    let profile = chip_type_to_chip_id(&hw.chip_type).and_then(MinerProfile::for_chip);
    SharedConfigSnapshot {
        schema: CONFIG_SCHEMA_VERSION,
        family: "antminer".to_string(),
        device_model: profile.map(|p| p.name).unwrap_or("Antminer").to_string(),
        board_target: antminer_board_target(hw),
        board_version: Some(antminer_board_version(hw)),
        network: SharedNetworkConfig {
            hostname: if hostname.is_empty() {
                std::fs::read_to_string("/etc/hostname")
                    .unwrap_or_else(|_| "dcentos".to_string())
                    .trim()
                    .to_string()
            } else {
                hostname
            },
            ipv4: Some(eth0_ipv4()),
            ssid: None,
        },
        primary_pool: shared_primary_pool(table, miner),
        fallback_pool: None,
        mining: SharedMiningConfig {
            enabled: config_bool(table, "mining", "enabled")
                .unwrap_or(miner.hashrate_ghs > 0.0 || is_pool_mining_capable(&miner.pool.status)),
            frequency_mhz: config_f32(table, "mining", "frequency_mhz"),
            voltage_mv: config_u16(table, "mining", "voltage_mv"),
            overclock_enabled: None,
        },
        thermal: SharedThermalConfig {
            target_temp_c: config_u16(table, "thermal", "target_temp_c")
                .and_then(|value| u8::try_from(value).ok()),
            manual_fan_speed_pct: None,
        },
        auth: SharedAuthConfig {
            password_set: crate::auth::is_password_set(),
            allow_unsigned_ota: false,
            metrics_require_auth: state.config.metrics_require_auth,
            session_auth: true,
        },
    }
}

pub(super) fn load_shared_install_intent() -> Option<InstallIntent> {
    let raw = read_json_value(INSTALL_INTENT_FILE)?;
    Some(InstallIntent {
        schema_version: raw
            .get("schema_version")
            .and_then(|value| value.as_u64())
            .unwrap_or(1) as u8,
        installer: raw
            .get("installer")
            .and_then(|value| value.as_str())
            .unwrap_or("dcent-toolbox")
            .to_string(),
        install_origin: raw
            .get("install_origin")
            .and_then(|value| value.as_str())
            .unwrap_or("unknown")
            .to_string(),
        bootstrap_transport: raw
            .get("bootstrap_transport")
            .and_then(|value| value.as_str())
            .unwrap_or("unknown")
            .to_string(),
        install_method: raw
            .get("install_method")
            .and_then(|value| value.as_str())
            .unwrap_or("unknown")
            .to_string(),
        hardening_profile: raw
            .get("hardening_profile")
            .and_then(|value| value.as_str())
            .unwrap_or("standard")
            .to_string(),
        target_ip: raw
            .get("target_ip")
            .and_then(|value| value.as_str())
            .map(|value| value.to_string()),
        model: raw
            .get("model")
            .and_then(|value| value.as_str())
            .map(|value| value.to_string()),
        hostname: raw
            .get("hostname")
            .and_then(|value| value.as_str())
            .map(|value| value.to_string()),
        mac: raw
            .get("mac")
            .and_then(|value| value.as_str())
            .map(|value| value.to_string()),
        hwid: raw
            .get("hwid")
            .and_then(|value| value.as_str())
            .map(|value| value.to_string()),
        package_version: raw
            .get("package_version")
            .and_then(|value| value.as_str())
            .map(|value| value.to_string()),
        package_model: raw
            .get("package_model")
            .and_then(|value| value.as_str())
            .map(|value| value.to_string()),
        board_target: raw
            .get("board_target")
            .and_then(|value| value.as_str())
            .map(|value| value.to_string()),
        package_type: raw
            .get("package_type")
            .and_then(|value| value.as_str())
            .map(|value| value.to_string()),
        created_at: raw
            .get("created_at")
            .and_then(|value| value.as_str())
            .unwrap_or_default()
            .to_string(),
    })
}

pub(super) fn update_metadata_payload(
    miner: &crate::MinerState,
    hw: &crate::HardwareInfo,
) -> UpdateMetadata {
    let profile = chip_type_to_chip_id(&hw.chip_type).and_then(MinerProfile::for_chip);
    let board_target = antminer_board_target(hw);
    let board_desc = antminer_board_desc(hw);
    let runtime_descriptor = build_antminer_capability_descriptor(miner, hw);
    let update_authorized = runtime_descriptor
        .runtime_caps
        .contains(&RuntimeCapability::FlashOta);
    let inactive_slot_supported = board_desc
        .map(|desc| {
            desc.enablement.storage_topology == dcentrald_common::StorageTopology::RedundantSlots
        })
        .unwrap_or(false);
    let upload_endpoint = if update_authorized {
        Some("/api/system/upgrade".to_string())
    } else {
        None
    };
    // WAVE 0 STABILIZE (2026-06-05) — OTA honesty: derive the signature
    // posture from the trust anchors that ACTUALLY exist at runtime instead of
    // hardcoding `signature_required: true`. With no compiled-in key AND no
    // on-disk `/etc/dcentos/release_ed25519.pub`, the verifier can't verify a
    // signature, so claiming a required gate is dishonest (it would reject
    // every signed update / is inert on the production unsigned-rejecting
    // path). `signature_capable`/`signature_required` track the real state;
    // `key_id` is only surfaced when a key is genuinely pinned.
    let sig_state = crate::ota_signature::ota_signature_state();
    let sig_enforced = sig_state.is_enforced();
    UpdateMetadata {
        schema: UPDATE_SCHEMA_VERSION,
        product: "DCENT_OS".to_string(),
        family: "antminer".to_string(),
        device_model: profile.map(|p| p.name).unwrap_or("Antminer").to_string(),
        board_target: board_target.clone(),
        current_version: miner.firmware_version.clone(),
        package_type: board_desc
            .map(|desc| desc.enablement.artifact_kind.as_str())
            .unwrap_or("unsupported")
            .to_string(),
        upload_endpoint: upload_endpoint.clone(),
        board_target_header: None,
        device_model_header: None,
        inactive_slot_supported,
        // Capable iff we can establish a trust anchor; required iff enforced.
        signature_capable: sig_enforced,
        signature_required: sig_enforced,
        // The production write path (`system_upgrade`) hardcodes
        // `allow_unsigned = false`, so unsigned bundles are rejected there
        // regardless. When inert (no key), we honestly report that an
        // unsigned/untrusted bundle would NOT pass on the production path.
        allow_unsigned: false,
        key_id: crate::ota_signature::honest_key_id().map(|s| s.to_string()),
        install_intent: load_shared_install_intent(),
        toolbox: ToolboxPackageInfo {
            install_command: if update_authorized {
                "dcent install <ip> -f dcentos-sysupgrade.tar".to_string()
            } else {
                String::new()
            },
            update_command: if update_authorized {
                "dcent install <ip> -f dcentos-sysupgrade.tar".to_string()
            } else {
                String::new()
            },
            upload_endpoint,
            board_target_header: None,
            device_model_header: None,
            requires_inactive_slot: inactive_slot_supported,
        },
    }
}

pub(super) fn load_onboarding_current_snapshot(
    state: &OnboardingState,
) -> OnboardingCurrentSnapshot {
    let table = load_config_table_for_write().unwrap_or_else(|_| toml::Table::new());
    let economics = home_economics_from_table(&table);
    OnboardingCurrentSnapshot {
        hostname: config_string(&table, "general", "hostname"),
        mode: config_string(&table, "mode", "active"),
        power_source: if !config_string(&table, "power", "source_profile").is_empty() {
            config_string(&table, "power", "source_profile")
        } else {
            state.power_source.clone()
        },
        circuit_voltage_v: config_u16(&table, "power", "circuit_voltage_v"),
        circuit_amperage_a: config_u16(&table, "power", "circuit_amperage_a"),
        pool: OnboardingPoolSnapshot {
            url: config_string(&table, "pool", "url"),
            worker: config_string(&table, "pool", "worker"),
        },
        electricity_rate: economics.rate_usd_per_kwh,
        currency: economics.currency,
        electricity_rate_calibrated: economics.rate_calibrated,
    }
}

pub(super) fn header_origin_host(value: &str) -> &str {
    value
        .trim_start_matches("http://")
        .trim_start_matches("https://")
        .split('/')
        .next()
        .unwrap_or("")
}

pub(super) fn is_same_origin_setup_headers(headers: &HeaderMap) -> bool {
    let host = match headers.get("host").and_then(|v| v.to_str().ok()) {
        Some(host) => host,
        None => return false,
    };

    if let Some(origin) = headers.get("origin").and_then(|v| v.to_str().ok()) {
        return header_origin_host(origin) == host;
    }

    if let Some(referer) = headers.get("referer").and_then(|v| v.to_str().ok()) {
        return header_origin_host(referer) == host;
    }

    if let Some(fetch_site) = headers.get("sec-fetch-site").and_then(|v| v.to_str().ok()) {
        return fetch_site == "same-origin";
    }

    false
}

pub(super) fn onboarding_is_complete(state: &OnboardingState) -> bool {
    state.steps.complete
        && state.steps.mode_configured
        // Freedom-first: the circuit/breaker/safety acknowledgement is
        // strongly recommended but never forced. The operator can
        // deliberately opt out (`safety_opt_out`) and run without it,
        // exactly like the password opt-out below. Onboarding is still
        // gated on mode + a password decision.
        && (state.steps.safety_ack || state.steps.safety_opt_out)
        // Freedom-first: a password is strongly recommended but never forced.
        // The operator can deliberately opt out (`password_opt_out`) and run
        // with the default. Onboarding is still gated on mode + safety.
        && (state.auth.password_set || state.auth.password_opt_out)
}

/// Freedom-first: a password decision has been made when the operator
/// either set a real password OR explicitly opted out. `post_setup_complete`
/// is gated on this (NOT on `password_set`) so completing setup never forces
/// a password. Pure — host-safe for unit tests.
pub(super) fn onboarding_password_decision_made(auth: &OnboardingAuthState) -> bool {
    auth.password_set || auth.password_opt_out
}

/// Freedom-first: a circuit/safety decision has been made when the operator
/// either acknowledged the safety/circuit step OR explicitly opted out. The
/// exact parallel of `onboarding_password_decision_made`. Pure — host-safe
/// for unit tests.
pub(super) fn onboarding_safety_decision_made(steps: &OnboardingStepState) -> bool {
    steps.safety_ack || steps.safety_opt_out
}

/// Reconciliation rule: a real owner password supersedes the opt-out. When
/// `password_set` becomes true, the opt-out is cleared so the security
/// advisory self-clears and phase reporting stays consistent. Returns true
/// if `auth` was mutated (the caller's dirty flag). Pure — host-safe.
pub(super) fn reconcile_password_opt_out(auth: &mut OnboardingAuthState) -> bool {
    if auth.password_set && auth.password_opt_out {
        auth.password_opt_out = false;
        return true;
    }
    false
}

/// Reconciliation rule: a real safety acknowledgement supersedes the
/// opt-out. When `safety_ack` becomes true (the operator later completes
/// the circuit/safety check from Settings or the wizard), the opt-out is
/// cleared so the "circuit check not done" advisory self-clears and phase
/// reporting stays consistent. Returns true if `steps` was mutated (the
/// caller's dirty flag). Pure — host-safe. The exact parallel of
/// `reconcile_password_opt_out`.
pub(super) fn reconcile_safety_opt_out(steps: &mut OnboardingStepState) -> bool {
    if steps.safety_ack && steps.safety_opt_out {
        steps.safety_opt_out = false;
        return true;
    }
    false
}

pub(crate) fn onboarding_device_ready() -> bool {
    onboarding_is_complete(&load_onboarding_state())
}

/// Whether the operator EXPLICITLY opted out of an owner password during setup
/// (`POST /api/setup/skip-password` → persisted `auth.password_opt_out=true` in
/// `/data/dcent/onboarding.json`).
///
/// BUG-7/8 FIX (2026-06-05, the .100/.138 live install): the password opt-out is
/// an explicit "I accept running with no owner password (default-credential /
/// no-auth control posture)" choice — it must GRANT write/control access on a
/// dev/home image, not silently lock the operator out of every write endpoint.
/// Before this, `auth.rs::auth_middleware` only consulted `is_password_set()`
/// (the `auth.json` existence check) and had NO knowledge of the opt-out, so an
/// opted-out operator got 403 "Password setup required" on every mutation
/// (restart / mining on-off / pools) — including the wizard's final reboot that
/// engages mining. The auth middleware now consults this accessor; on a RELEASE
/// image the opt-out can never be recorded in the first place (the
/// `skip-password` route is 403'd there), so this stays dev/home-only and the
/// release password requirement + placeholder-pubkey rule are unaffected.
///
/// Read live from disk (no cache) so a freshly-set opt-out is honoured on the
/// very next request without a daemon restart. This is consulted only on the
/// rare passwordless-write path, which already reads onboarding state for the
/// device-ready gate, so it is not a hot-path regression.
pub(crate) fn onboarding_password_opt_out_active() -> bool {
    load_onboarding_state().auth.password_opt_out
}

pub(super) fn onboarding_power_source(state: &OnboardingState) -> String {
    load_onboarding_current_snapshot(state).power_source
}

pub(super) fn onboarding_requires_solar_provider(state: &OnboardingState) -> bool {
    onboarding_power_source(state) == "solar_battery"
}

/// Mirror of `/api/system/health`'s `is_mining` predicate ( HIGH-1):
/// the daemon is mining when the live topline reports hashrate OR any chain
/// reports hashrate / enumerated chips. Single-sourced here so the
/// setup-status `mining_ready` can never contradict the `is_mining` the
/// dashboard reads from `/api/system/health`. Pure — host-safe.
pub(super) fn miner_is_actively_mining(miner: &crate::MinerState) -> bool {
    miner.hashrate_5s_ghs > 0.0
        || miner
            .chains
            .iter()
            .any(|c| c.hashrate_ghs > 0.0 || c.chips > 0)
}

/// Whether the unit is mining-ready for the onboarding / setup-status surface.
///
/// OMEGA P2-9 / D-20 (2026-06-06): reconciled so a unit that is *actually
/// mining* is never reported `mining_ready:false`. Two coupled fixes:
///
/// 1. **Ground truth wins.** When the daemon is genuinely mining
///    (`actively_mining`, mirroring `/api/system/health`'s `is_mining`), the
///    unit is mining-ready by definition. The onboarding wizard's optional
///    circuit step must never report a truth-contradicting
///    `mining_ready:false` over an `is_mining:true` unit.
/// 2. **Decoupled from the skippable circuit step.** The previous predicate
///    AND-gated `steps.circuit_configured`, which is only set when AC
///    volts+amps are entered. That wrongly blocked (a) operators who opted
///    out of the circuit/breaker check (`safety_opt_out`) and (b) DC/solar
///    installs (which never enter AC values) from EVER reporting
///    mining-ready — even though the daemon's real gate
///    (`mining_start_enabled()` = `mining.enabled && pool configured`) has no
///    circuit dependency. The circuit/safety *decision* is still required,
///    but it is already enforced by `onboarding_is_complete`
///    (`safety_ack || safety_opt_out`), so the extra AC-values gate is dropped.
///
/// Pure — host-safe for unit tests.
pub(super) fn onboarding_is_mining_ready(
    state: &OnboardingState,
    solar_provider_ready: bool,
    actively_mining: bool,
) -> bool {
    if actively_mining {
        return true;
    }
    onboarding_is_complete(state)
        && state.steps.pool_configured
        && (!onboarding_requires_solar_provider(state) || solar_provider_ready)
}

pub(super) fn onboarding_phase_for(state: &OnboardingState) -> &'static str {
    if state.trust.password_auth_disabled || state.trust.ssh_keys_enrolled {
        "hardened"
    } else if onboarding_is_complete(state) {
        "configured"
    } else if state.auth.password_set {
        "owner_auth_set"
    } else {
        "first_boot"
    }
}

pub(super) fn load_onboarding_state() -> OnboardingState {
    let password_set = crate::auth::is_password_set();
    let token_issued = crate::auth::has_active_sessions();
    let safety_ack = read_safety_acknowledged();
    let legacy_complete = std::path::Path::new(SETUP_FILE).exists();
    let install_origin = read_install_intent_field("install_origin");
    let bootstrap_transport = read_install_intent_field("bootstrap_transport");
    let hardening_profile = read_install_intent_field("hardening_profile");

    if let Some(value) = read_json_value(ONBOARDING_FILE) {
        if let Ok(mut state) = serde_json::from_value::<OnboardingState>(value) {
            let now = chrono_now_iso();
            let mut dirty = false;
            if state.version == 0 {
                state.version = 1;
                dirty = true;
            }
            if state.created_at.is_empty() {
                state.created_at = now.clone();
                dirty = true;
            }
            if state.updated_at.is_empty() {
                state.updated_at = now.clone();
                dirty = true;
            }
            if state.auth.password_set != password_set {
                state.auth.password_set = password_set;
                dirty = true;
            }
            if state.auth.token_issued != token_issued {
                state.auth.token_issued = token_issued;
                dirty = true;
            }
            // Reconciliation: a real owner password supersedes the opt-out.
            // If the operator later sets a password, clear the opt-out so the
            // security warning self-clears and phase reporting is consistent.
            if reconcile_password_opt_out(&mut state.auth) {
                dirty = true;
            }
            if !state.auth.password_set && state.trust.credentials_rotated {
                state.trust.credentials_rotated = false;
                dirty = true;
            }
            if state.auth.password_set && !state.trust.credentials_rotated {
                state.trust.credentials_rotated = true;
                dirty = true;
            }
            if let Some(ref origin) = install_origin {
                if state.trust.install_origin != *origin {
                    state.trust.install_origin = origin.clone();
                    dirty = true;
                }
            }
            if let Some(ref transport) = bootstrap_transport {
                if state.trust.bootstrap_transport != *transport {
                    state.trust.bootstrap_transport = transport.clone();
                    dirty = true;
                }
            }
            if let Some(ref profile) = hardening_profile {
                if state.trust.hardening_profile != *profile {
                    state.trust.hardening_profile = profile.clone();
                    dirty = true;
                }
            }
            if safety_ack && !state.steps.safety_ack {
                state.steps.safety_ack = true;
                dirty = true;
            }
            // Reconciliation: a real safety acknowledgement supersedes the
            // opt-out. If the operator later completes the circuit/safety
            // check, clear the opt-out so the "circuit check not done"
            // advisory self-clears and phase reporting is consistent. The
            // exact parallel of the password opt-out reconciliation above.
            if reconcile_safety_opt_out(&mut state.steps) {
                dirty = true;
            }
            if legacy_complete && !state.steps.complete {
                state.steps.complete = true;
                state.steps.mode_configured = true;
                state.steps.safety_ack = true;
                dirty = true;
            }
            if state.steps.complete && state.completed_at.is_none() {
                state.completed_at = read_legacy_setup_completed_at().or_else(|| Some(now.clone()));
                dirty = true;
            }
            state.phase = onboarding_phase_for(&state).to_string();
            if dirty {
                let _ = save_onboarding_state(&state);
            }
            return state;
        }
    }

    let now = chrono_now_iso();
    let mut state = OnboardingState {
        created_at: now.clone(),
        updated_at: now.clone(),
        completed_at: read_legacy_setup_completed_at(),
        ..OnboardingState::default()
    };
    state.auth.password_set = password_set;
    state.auth.token_issued = token_issued;
    state.trust.credentials_rotated = state.auth.password_set;
    if let Some(origin) = install_origin {
        state.trust.install_origin = origin;
    }
    if let Some(transport) = bootstrap_transport {
        state.trust.bootstrap_transport = transport;
    }
    if let Some(profile) = hardening_profile {
        state.trust.hardening_profile = profile;
    }
    state.steps.safety_ack = safety_ack;
    state.steps.complete = legacy_complete;
    if state.steps.complete {
        // Legacy completed setups predate structured onboarding; treat mode as satisfied
        // and safety as acknowledged so existing miners don't get forced back into
        // setup on migration.
        state.steps.mode_configured = true;
        state.steps.safety_ack = true;
    }
    state.phase = onboarding_phase_for(&state).to_string();
    let _ = save_onboarding_state(&state);
    state
}

pub(super) fn save_onboarding_state(state: &OnboardingState) -> std::io::Result<()> {
    if let Some(parent) = std::path::Path::new(ONBOARDING_FILE).parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(state).map_err(std::io::Error::other)?;
    atomic_write(ONBOARDING_FILE, json)
}

pub(super) fn update_onboarding_state<F>(mutate: F) -> std::result::Result<OnboardingState, String>
where
    F: FnOnce(&mut OnboardingState),
{
    let mut state = load_onboarding_state();
    mutate(&mut state);
    if state.created_at.is_empty() {
        state.created_at = chrono_now_iso();
    }
    state.updated_at = chrono_now_iso();
    if state.steps.complete && state.completed_at.is_none() {
        state.completed_at = Some(state.updated_at.clone());
    }
    state.phase = onboarding_phase_for(&state).to_string();
    save_onboarding_state(&state).map_err(|e| format!("Failed to save onboarding state: {}", e))?;
    Ok(state)
}

/// GET /api/safety/warnings -- Safety warnings for residential deployment.
pub(super) async fn get_safety_warnings() -> impl IntoResponse {
    let acknowledged = read_safety_acknowledged();

    Json(serde_json::json!({
        "acknowledged": acknowledged,
        "warnings": [
            {
                "id": "circuit_capacity",
                "severity": "critical",
                "title": "Circuit Capacity",
                "message": "This device draws up to 1400W. Ensure it is on a dedicated 15A circuit. Do not share the circuit with other high-power appliances.",
            },
            {
                "id": "ventilation",
                "severity": "critical",
                "title": "Ventilation Required",
                "message": "Maintain 12 inches of clearance on all sides for ventilation. Do not cover or enclose the device.",
            },
            {
                "id": "fire_risk",
                "severity": "warning",
                "title": "Fire Safety",
                "message": "Keep flammable materials away from the device. Do not operate unattended for extended periods without thermal monitoring configured.",
            },
            {
                "id": "not_certified",
                "severity": "info",
                "title": "Product Certification",
                "message": "This product has not been certified as a space heater (CSA/UL). Use at your own risk.",
            },
        ],
    }))
}

/// POST /api/safety/acknowledge -- User confirms they've read safety warnings.
pub(super) async fn post_safety_acknowledge(
    State(app_state): State<Arc<AppState>>,
) -> impl IntoResponse {
    let data = serde_json::json!({ "acknowledged": true });
    if let Some(parent) = std::path::Path::new(SAFETY_ACK_FILE).parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    match atomic_write(
        SAFETY_ACK_FILE,
        serde_json::to_string_pretty(&data).unwrap_or_default(),
    ) {
        Ok(()) => {
            if let Err(e) = update_onboarding_state(|state| {
                state.steps.safety_ack = true;
            }) {
                tracing::warn!(error = %e, "Failed to update onboarding state after safety acknowledgement");
            }
            crate::push_audit_event(
                &app_state,
                "operator",
                dcentrald_api_types::audit_log::AuditEvent::Free {
                    category: "safety_acknowledgement".to_string(),
                    message: "operator acknowledged residential safety warnings".to_string(),
                },
            );
            Json(serde_json::json!({
                "status": "ok",
                "acknowledged": true,
            }))
            .into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({
                "error": "Failed to save acknowledgement",
                "detail": e.to_string(),
            })),
        )
            .into_response(),
    }
}

// ─── Setup Wizard Handlers ───────────────────────────────────────────

/// GET /api/setup/status -- Check if first-boot setup is complete.
pub(super) async fn get_setup_status(
    State(app_state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let onboarding = load_onboarding_state();
    let source = onboarding_power_source(&onboarding);
    let solar_provider_required = source == "solar_battery";
    let solar_config = load_solar_config_payload();
    let solar_provider_saved = solar_provider_required && solar_provider_saved(&solar_config);
    let solar_provider_runtime_adopted = solar_provider_required
        && app_state
            .solar_rx
            .as_ref()
            .map(|rx| rx.borrow().enabled)
            .unwrap_or(false);
    let solar_provider_trust = if solar_provider_required {
        Some(
            if crate::solar_provider_telemetry_backed(&solar_config.inverter_brand) {
                "telemetry"
            } else {
                "manual"
            },
        )
    } else {
        None
    };
    let device_ready = onboarding_is_complete(&onboarding);
    // OMEGA P2-9 / D-20: reconcile `mining_ready` with ground truth. A unit
    // that is actively mining (live hashrate / enumerated chips, the same
    // signal `/api/system/health` reports as `is_mining`) is mining-ready by
    // definition, regardless of whether the optional circuit step was skipped.
    let actively_mining = miner_is_actively_mining(&app_state.state_rx.borrow());
    let mining_ready =
        onboarding_is_mining_ready(&onboarding, solar_provider_runtime_adopted, actively_mining);
    let needs_setup = !device_ready;
    let password_opt_out = onboarding.auth.password_opt_out;
    let password_decision_made = onboarding_password_decision_made(&onboarding.auth);
    let safety_opt_out = onboarding.steps.safety_opt_out;
    let safety_decision_made = onboarding_safety_decision_made(&onboarding.steps);
    let expose_details = !onboarding.auth.password_set || is_same_origin_setup_headers(&headers);
    let auth = if expose_details {
        serde_json::json!(onboarding.auth)
    } else {
        serde_json::json!({
            "password_set": onboarding.auth.password_set,
            "password_opt_out": password_opt_out,
            "token_issued": false,
        })
    };
    let trust = if expose_details {
        serde_json::json!(onboarding.trust)
    } else {
        serde_json::Value::Null
    };
    let current = if expose_details {
        serde_json::json!(load_onboarding_current_snapshot(&onboarding))
    } else {
        serde_json::Value::Null
    };
    Json(serde_json::json!({
        "needs_setup": needs_setup,
        "device_ready": device_ready,
        "mining_ready": mining_ready,
        "resume_requires_auth": onboarding.auth.password_set,
        // Freedom-first: surfaced so the dashboard can (a) drive the
        // self-clearing "no owner password" security warning and (b) treat
        // an explicit opt-out as a completed password decision.
        "password_opt_out": password_opt_out,
        "password_decision_made": password_decision_made,
        // Freedom-first: the exact parallel for the circuit/safety step.
        // Drives the self-clearing "circuit check not done" advisory and
        // lets the wizard treat an explicit opt-out as a completed safety
        // decision (so completing setup never forces the circuit check).
        "safety_opt_out": safety_opt_out,
        "safety_decision_made": safety_decision_made,
        "phase": onboarding.phase,
        "steps": ["safety", "circuit", "solar_provider", "password", "mode", "pool", "complete"],
        "progress": {
            // Truthy when the operator has made ANY safety decision —
            // acknowledged the circuit/safety check OR explicitly opted
            // out. Mirrors the `password` field's decision-made semantics.
            "safety": safety_decision_made,
            "circuit": onboarding.steps.circuit_configured,
            "solar_provider": if solar_provider_required { solar_provider_runtime_adopted } else { true },
            // Truthy when the operator has made ANY password decision —
            // set one OR explicitly opted out.
            "password": password_decision_made,
            "mode": onboarding.steps.mode_configured,
            "pool": onboarding.steps.pool_configured,
            "complete": onboarding.steps.complete,
        },
        "auth": auth,
        "trust": trust,
        "power_source": if expose_details { serde_json::json!(source) } else { serde_json::Value::Null },
        "commissioning": if expose_details {
            serde_json::json!({
                "solar_provider_required": solar_provider_required,
                "solar_provider_saved": solar_provider_saved,
                "solar_provider_runtime_adopted": solar_provider_runtime_adopted,
                "solar_provider": if solar_provider_required { serde_json::Value::String(solar_config.inverter_brand.clone()) } else { serde_json::Value::Null },
                "solar_provider_trust": solar_provider_trust,
            })
        } else {
            serde_json::Value::Null
        },
        "current": current,
        "completed_at": onboarding.completed_at,
    }))
}

/// POST /api/setup/step1-safety -- User acknowledges safety warnings.
pub(super) async fn post_setup_safety(State(app_state): State<Arc<AppState>>) -> impl IntoResponse {
    // Reuse the safety acknowledge handler
    post_safety_acknowledge(State(app_state)).await
}

/// Request body for circuit configuration.
#[derive(Debug, Deserialize)]
pub struct SetupCircuitRequest {
    /// Intended install power source (grid, direct_dc, solar_battery, hybrid).
    pub source: Option<String>,
    /// Circuit voltage (120 or 240) for AC-backed setups.
    pub voltage: Option<u16>,
    /// Circuit amperage (15 or 20) for AC-backed setups.
    pub amperage: Option<u16>,
}

/// CE-360: fail-closed validation of the operator-declared install circuit
/// BEFORE the derived watt-cap is persisted and armed as live throttle
/// authority. `[power].circuit_capacity_watts` written here becomes the runtime
/// power ceiling consumed by `daemon.rs` / `work_dispatcher.rs`, so an absurd
/// declared circuit (near-zero → throttle-to-zero, or effectively-infinite → no
/// breaker protection) must be rejected at the boundary rather than persisted.
///
/// PURE + host-testable (no filesystem/HAL). Additive: the XOR (both-or-neither)
/// pairing check, the empty-source/no-AC "skip" path, the source-only persist,
/// and the DC (`direct_dc`/`solar_battery`) sources — all of which arm NO cap —
/// stay valid here (they return `Ok`), so no benign onboarding flow starts
/// failing. The numeric bands cover 120/208/240 V home/industrial circuits at
/// 15/20/30/50 A and are intentionally conservative.
fn validate_setup_circuit(
    source: &str,
    voltage: Option<u16>,
    amperage: Option<u16>,
) -> std::result::Result<(), &'static str> {
    // (0) A full AC pair is both-or-neither. The caller enforces this too (and
    // returns 400 first), but keeping it here makes the validator self-contained
    // and fail-closed if that outer guard is ever refactored away.
    if voltage.is_some() ^ amperage.is_some() {
        return Err("both voltage and amperage are required together");
    }

    // (1) A declared source must be a known install source. Empty source is the
    // "skip"/not-yet-declared path and stays allowed.
    if !source.is_empty() && !matches!(source, "grid" | "direct_dc" | "solar_battery" | "hybrid") {
        return Err("source must be one of: grid, direct_dc, solar_battery, hybrid");
    }

    // (2)+(3) AC circuit sanity — only when a full AC pair is present (the
    // caller enforces the both-or-neither XOR separately). DC/source-only paths
    // supply no AC values and correctly skip this block.
    if let (Some(voltage), Some(amperage)) = (voltage, amperage) {
        if !(90..=300).contains(&voltage) {
            return Err("circuit voltage must be between 90 and 300 V");
        }
        if !(1..=200).contains(&amperage) {
            return Err("circuit amperage must be between 1 and 200 A");
        }
        let declared_watts = voltage as u32 * amperage as u32;
        if !(300..=60_000).contains(&declared_watts) {
            return Err(
                "declared circuit power (voltage x amperage) must be between 300 and 60000 W",
            );
        }
    }

    Ok(())
}

/// POST /api/setup/step2-circuit -- User declares circuit capacity.
pub(super) async fn post_setup_circuit(Json(body): Json<SetupCircuitRequest>) -> impl IntoResponse {
    let source = body.source.as_deref().unwrap_or("").trim().to_string();
    let has_ac_values = body.voltage.is_some() && body.amperage.is_some();

    if body.voltage.is_some() ^ body.amperage.is_some() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "status": "error",
                "message": "Both voltage and amperage are required together",
            })),
        )
            .into_response();
    }

    // CE-360: reject an out-of-band circuit declaration BEFORE it is persisted
    // and armed as the runtime watt-cap. Fail closed (400 + no persist +
    // circuit_configured stays false).
    if let Err(reason) = validate_setup_circuit(&source, body.voltage, body.amperage) {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "status": "error",
                "message": reason,
            })),
        )
            .into_response();
    }

    let declared_watts = body
        .voltage
        .zip(body.amperage)
        .map(|(voltage, amperage)| voltage as u32 * amperage as u32);
    let circuit_watts = declared_watts.map(|watts| watts.saturating_mul(80) / 100);
    tracing::info!(
        source = %source,
        voltage = ?body.voltage,
        amperage = ?body.amperage,
        declared_watts,
        circuit_watts,
        "Power commissioning updated"
    );

    let persisted = if let Some(circuit_watts) = circuit_watts {
        write_toml_section(
            "power",
            &[
                (
                    "circuit_capacity_watts",
                    toml::Value::Integer(circuit_watts as i64),
                ),
                (
                    "circuit_voltage_v",
                    toml::Value::Integer(body.voltage.unwrap_or_default() as i64),
                ),
                (
                    "circuit_amperage_a",
                    toml::Value::Integer(body.amperage.unwrap_or_default() as i64),
                ),
                ("source_profile", toml::Value::String(source.clone())),
            ],
        )
        .map(|_| true)
        .unwrap_or_else(|e| {
            tracing::error!(error = %e, "Failed to persist circuit config");
            false
        })
    } else if !source.is_empty() {
        write_toml_section(
            "power",
            &[("source_profile", toml::Value::String(source.clone()))],
        )
        .map(|_| true)
        .unwrap_or_else(|e| {
            tracing::error!(error = %e, "Failed to persist power source profile");
            false
        })
    } else {
        true
    };

    let circuit_configured = has_ac_values;

    if persisted {
        if let Err(e) = update_onboarding_state(|state| {
            state.steps.circuit_configured = circuit_configured;
            if !source.is_empty() {
                state.power_source = source.clone();
            }
        }) {
            tracing::warn!(error = %e, "Failed to update onboarding state after circuit configuration");
        }
    }

    Json(serde_json::json!({
        "status": "ok",
        "persisted": persisted,
        "source": body.source,
        "declared_watts": declared_watts,
        "circuit_watts": circuit_watts,
        "voltage": body.voltage,
        "amperage": body.amperage,
    }))
    .into_response()
}

/// Request body for the economics setup step (electricity rate + currency).
#[derive(Debug, Deserialize)]
pub struct SetupEconomicsRequest {
    /// Electricity rate in the operator's local currency per kWh.
    pub electricity_rate: Option<f64>,
    /// Display currency code (e.g. "USD", "CAD"). Defaults to USD when omitted.
    pub currency: Option<String>,
}

/// POST /api/setup/step-economics -- Operator confirms their electricity rate +
/// currency. P2-4 (§4.E): this is the SINGLE SOURCE OF TRUTH for cost/earnings
/// math. It persists `[home].electricity_rate` + `[home].currency` and sets
/// `[home].electricity_rate_calibrated = true` so every cost surface stops
/// labelling itself "uncalibrated". The dashboard reads these back (via
/// `/api/home/status` or `/api/setup/status`) instead of its own localStorage
/// guess (the previous `0.10` client default that disagreed with the daemon's
/// `0.12`).
pub(super) async fn post_setup_economics(
    Json(body): Json<SetupEconomicsRequest>,
) -> impl IntoResponse {
    // A real residential rate is small and positive. Reject negatives / absurd /
    // non-finite values rather than poisoning every downstream cost estimate.
    let rate = match body.electricity_rate {
        Some(r) if r.is_finite() && (0.0..=10.0).contains(&r) => r,
        Some(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "status": "error",
                    "message": "electricity_rate must be a finite value between 0 and 10 per kWh",
                    "persisted": false,
                })),
            )
                .into_response();
        }
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "status": "error",
                    "message": "electricity_rate is required",
                    "persisted": false,
                })),
            )
                .into_response();
        }
    };
    let currency = body
        .currency
        .as_deref()
        .map(str::trim)
        .filter(|c| !c.is_empty())
        .unwrap_or(DEFAULT_CURRENCY)
        .to_string();

    tracing::info!(electricity_rate = rate, currency = %currency, "Electricity economics confirmed at setup");

    let persisted = write_toml_section(
        "home",
        &[
            ("electricity_rate", toml::Value::Float(rate)),
            ("currency", toml::Value::String(currency.clone())),
            ("electricity_rate_calibrated", toml::Value::Boolean(true)),
        ],
    )
    .map(|_| true)
    .unwrap_or_else(|e| {
        tracing::error!(error = %e, "Failed to persist electricity economics");
        false
    });

    Json(serde_json::json!({
        "status": if persisted { "ok" } else { "error" },
        "persisted": persisted,
        "electricity_rate": rate,
        "currency": currency,
        "electricity_rate_calibrated": true,
    }))
    .into_response()
}

/// POST /api/setup/step3-password -- User sets admin password.
pub(super) async fn post_setup_password(
    connect_info: ConnectInfo<std::net::SocketAddr>,
    Json(body): Json<AuthSetupRequest>,
) -> impl IntoResponse {
    post_auth_setup(connect_info, Json(body)).await
}

/// Request body for mode selection.
#[derive(Debug, Deserialize)]
pub struct SetupModeRequest {
    /// Operating mode: "home", "standard", or "hacker".
    pub mode: String,
    /// Optional hostname chosen during first-boot setup.
    pub hostname: Option<String>,
}

/// POST /api/setup/step4-mode -- User selects operating mode.
pub(super) async fn post_setup_mode(Json(body): Json<SetupModeRequest>) -> impl IntoResponse {
    tracing::info!(mode = %body.mode, "Operating mode selected: {}", body.mode);

    // BUG FIX (2026-04-11): Persist to dcentrald.toml (was log-only).
    let mut persisted = write_toml_section(
        "mode",
        &[("active", toml::Value::String(body.mode.clone()))],
    )
    .map(|_| true)
    .unwrap_or_else(|e| {
        tracing::error!(error = %e, "Failed to persist mode config");
        false
    });

    if let Some(hostname) = body.hostname.as_ref() {
        let hostname_persisted = write_toml_section(
            "general",
            &[("hostname", toml::Value::String(hostname.clone()))],
        )
        .map(|_| true)
        .unwrap_or_else(|e| {
            tracing::error!(error = %e, hostname = %hostname, "Failed to persist hostname config");
            false
        });
        persisted &= hostname_persisted;
    }

    if persisted {
        if let Err(e) = update_onboarding_state(|state| {
            state.steps.mode_configured = true;
        }) {
            tracing::warn!(error = %e, "Failed to update onboarding state after mode configuration");
        }
    }

    Json(serde_json::json!({
        "status": "ok",
        "persisted": persisted,
        "mode": body.mode,
    }))
}

/// Request body for pool configuration.
#[derive(Debug, Deserialize)]
pub struct SetupPoolRequest {
    pub url: Option<String>,
    pub worker: Option<String>,
    pub password: Option<String>,
}

/// POST /api/setup/step5-pool -- User configures pool (optional for Home mode).
pub(super) async fn post_setup_pool(Json(body): Json<SetupPoolRequest>) -> impl IntoResponse {
    // W1.4: setup wizard worker is the operator's wallet address — mask it.
    let masked_worker = body
        .worker
        .as_deref()
        .map(dcentrald_common::wallet_mask::mask_wallet);
    tracing::info!(
        // SEC (W20 / parity #66): strip inline stratum credentials before
        // logging — the daemon log is readable via /api/debug/log + MCP +
        // support bundle. (The worker/wallet beside it is already masked.)
        url = ?dcentrald_stratum::pool_api::sanitize_pool_url(body.url.as_deref().unwrap_or("")),
        worker = ?masked_worker,
        "Pool configured in setup wizard"
    );

    if let Some(url) = body.url.as_deref().filter(|url| !url.trim().is_empty()) {
        if let Err(message) = validate_pool_url_support(url) {
            return pool_validation_error(message);
        }
    }

    // BUG FIX (2026-04-11): Persist to dcentrald.toml (was log-only).
    let mut entries: Vec<(&str, toml::Value)> = Vec::new();
    if let Some(ref url) = body.url {
        entries.push(("url", toml::Value::String(url.clone())));
    }
    if let Some(ref worker) = body.worker {
        entries.push(("worker", toml::Value::String(worker.clone())));
    }
    if let Some(ref password) = body.password {
        entries.push(("password", toml::Value::String(password.clone())));
    }
    let pool_configured = body
        .url
        .as_ref()
        .map(|url| !url.trim().is_empty())
        .unwrap_or(false)
        && body
            .worker
            .as_ref()
            .map(|worker| !worker.trim().is_empty())
            .unwrap_or(false);
    let persisted = if !entries.is_empty() {
        write_toml_section("pool", &entries)
            .map(|_| true)
            .unwrap_or_else(|e| {
                tracing::error!(error = %e, "Failed to persist pool config");
                false
            })
    } else {
        true
    };

    if persisted {
        if let Err(e) = update_onboarding_state(|state| {
            state.steps.pool_configured = pool_configured;
        }) {
            tracing::warn!(error = %e, "Failed to update onboarding state after pool configuration");
        }
    }

    Json(serde_json::json!({
        "status": "ok",
        "persisted": persisted,
        "pool": {
            "url": body.url,
            "worker": body.worker,
        },
        "mining_ready": pool_configured,
    }))
    .into_response()
}

/// W1.1 default-credential lockdown -- shell out to /usr/sbin/dcent-enable-ssh
/// to flip the dropbear gate.
///
/// `reason` is forwarded as `argv[1]` and ends up in the gate flag file
/// so the dashboard / log tail can show *why* SSH was enabled. Use
/// `"wizard"` for first-boot finalize and `"keys"` for the
/// authorized_keys-upload path (TODO: wire that route in W1.x).
///
/// Returns `Err(String)` describing the failure mode without panicking.
/// We never `unwrap()` -- shelling out at end-of-wizard must not break
/// setup completion if the helper is missing or non-executable.
pub(super) fn invoke_enable_ssh_helper(reason: &str) -> Result<(), String> {
    const HELPER: &str = "/usr/sbin/dcent-enable-ssh";
    // Reject non-token reasons defensively (the helper writes the value
    // verbatim into a gate file). Allowed alphabet matches the two
    // documented call sites; keeps shell metacharacters out even though
    // we're not going through a shell.
    if !reason
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(format!("invalid reason token: {:?}", reason));
    }
    match std::process::Command::new(HELPER).arg(reason).output() {
        Ok(out) if out.status.success() => Ok(()),
        Ok(out) => Err(format!(
            "{} exited {:?}: {}",
            HELPER,
            out.status.code(),
            String::from_utf8_lossy(&out.stderr).trim()
        )),
        Err(e) => Err(format!("failed to spawn {}: {}", HELPER, e)),
    }
}

/// POST /api/setup/skip-password -- Operator explicitly declines to set an
/// owner password and accepts running with the default (no owner password).
///
/// Freedom-first: this is the load-bearing backend half of "let people
/// choose". It does NOT widen write access — `auth.rs::is_pre_setup_safe`
/// still restricts a passwordless unit to read-only GETs; all
/// write/control endpoints continue to 403 without a password. This flag
/// only stops the onboarding nag so the operator can reach the dashboard.
///
/// Returns 409 if a password is already set (opting out would be
/// nonsensical and the reconciliation would clear it anyway).
pub(super) async fn post_setup_skip_password() -> impl IntoResponse {
    if crate::auth::is_password_set() {
        return (
            StatusCode::CONFLICT,
            Json(serde_json::json!({
                "error": "Password already set",
                "detail": "An owner password is already configured; opting out is not applicable",
            })),
        )
            .into_response();
    }

    match update_onboarding_state(|state| {
        state.auth.password_opt_out = true;
    }) {
        Ok(_) => {
            tracing::info!(
                "Operator opted out of an owner password during setup (default-credential posture; write/control endpoints still require a password)"
            );
            Json(serde_json::json!({
                "status": "ok",
                "password_opt_out": true,
                "message": "Continuing without an owner password. You can set one anytime in Settings — write and control actions stay locked until you do.",
            }))
            .into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({
                "error": "Failed to record password opt-out",
                "detail": e,
            })),
        )
            .into_response(),
    }
}

/// POST /api/setup/skip-safety -- Operator explicitly declines to complete
/// the circuit/breaker/safety acknowledgement and accepts running without
/// it.
///
/// Freedom-first: the exact backend parallel of
/// `post_setup_skip_password`. This is the load-bearing half of "the full
/// wizard is skippable straight to the dashboard". It changes NOTHING
/// about access — the dashboard + logs were already reachable on a
/// passwordless unit via `auth.rs::is_pre_setup_safe` (read-only GETs);
/// all write/control endpoints still require a password regardless of this
/// flag. This flag only stops the onboarding nag so the operator can reach
/// the dashboard, and drives a persistent, dismissible "circuit check not
/// done" advisory until the operator completes it.
///
/// Returns 409 if the safety step is already acknowledged (opting out
/// would be nonsensical and the reconciliation would clear it anyway).
pub(super) async fn post_setup_skip_safety() -> impl IntoResponse {
    if read_safety_acknowledged() {
        return (
            StatusCode::CONFLICT,
            Json(serde_json::json!({
                "error": "Safety already acknowledged",
                "detail": "The circuit/safety check is already acknowledged; opting out is not applicable",
            })),
        )
            .into_response();
    }

    match update_onboarding_state(|state| {
        state.steps.safety_opt_out = true;
    }) {
        Ok(_) => {
            tracing::info!(
                "Operator opted out of the circuit/safety acknowledgement during setup (running without the recommended circuit check; dashboard/log access and write/control gating are unchanged)"
            );
            Json(serde_json::json!({
                "status": "ok",
                "safety_opt_out": true,
                "message": "Continuing without the circuit/safety check. We strongly recommend verifying your circuit can handle the load — you can complete this anytime in Settings.",
            }))
            .into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({
                "error": "Failed to record safety opt-out",
                "detail": e,
            })),
        )
            .into_response(),
    }
}

/// POST /api/setup/complete -- Finalize first-boot setup.
pub(super) async fn post_setup_complete() -> impl IntoResponse {
    let onboarding = load_onboarding_state();
    // Freedom-first: a password is strongly recommended but never forced.
    // Completion is blocked ONLY when the operator has made no password
    // decision at all (neither set one nor explicitly opted out). The
    // wizard's "Continue without a password" / "Skip setup" paths call
    // POST /api/setup/skip-password first, which flips password_opt_out.
    if !onboarding_password_decision_made(&onboarding.auth) {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": "Password decision required",
                "detail": "Set an owner password or explicitly opt out (POST /api/setup/skip-password) before completing setup",
            })),
        )
            .into_response();
    }
    let enable_mining = onboarding.steps.pool_configured
        && onboarding.steps.safety_ack
        && onboarding.steps.mode_configured
        && onboarding.steps.circuit_configured;
    let data = serde_json::json!({
        "completed": true,
        "completed_at": chrono_now_iso(),
    });
    if let Some(parent) = std::path::Path::new(SETUP_FILE).parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    match atomic_write(
        SETUP_FILE,
        serde_json::to_string_pretty(&data).unwrap_or_default(),
    ) {
        Ok(()) => {
            if let Err(e) = write_toml_section(
                "mining",
                &[("enabled", toml::Value::Boolean(enable_mining))],
            ) {
                tracing::error!(error = %e, "Failed to persist mining enabled state during setup completion");
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({
                        "error": "Failed to finalize setup",
                        "detail": e,
                    })),
                )
                    .into_response();
            }
            if let Err(e) = update_onboarding_state(|state| {
                state.steps.complete = true;
            }) {
                tracing::warn!(error = %e, "Failed to update onboarding state after setup completion");
            }
            // W1.1 default-credential lockdown: flip the dropbear gate
            // now that the wizard has completed (Argon2id password is
            // set, onboarding is finalized). Failure here must NOT block
            // setup completion -- the operator can still re-run the
            // helper from the dashboard. We log loudly so the dashboard
            // log tail surfaces it.
            if let Err(e) = invoke_enable_ssh_helper("wizard") {
                tracing::warn!(
                    error = %e,
                    "Failed to enable SSH gate after wizard completion; SSH stays disabled until dcent-enable-ssh runs"
                );
            }
            tracing::info!("First-boot setup completed");
            Json(serde_json::json!({
                "status": "ok",
                "message": if enable_mining {
                    "Setup complete — reboot to begin mining with your configured pool."
                } else {
                    "Setup complete — reboot to enter dashboard-ready idle mode. Mining stays disabled until you opt in."
                },
            }))
            .into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({
                "error": "Failed to save setup state",
                "detail": e.to_string(),
            })),
        )
            .into_response(),
    }
}

/// Get current time as ISO 8601 string (no chrono dependency).
pub(super) fn chrono_now_iso() -> String {
    let dur = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    format!("{}s_since_epoch", dur.as_secs())
}

// ─── LED Control Endpoints ────────────────────────────────────────────

/// Request body for POST /api/led/locate.
#[derive(Debug, Deserialize)]
pub struct LocateRequest {
    /// Pattern ID (e.g., "imperial_march"). None = use config default.
    pub pattern_id: Option<String>,
}

/// Request body for POST /api/led/config.
#[derive(Debug, Deserialize)]
pub struct LedConfigUpdateRequest {
    pub locate_pattern: Option<String>,
    pub heartbeat_on_ms: Option<u16>,
    pub heartbeat_off_ms: Option<u16>,
    pub locate_duration_s: Option<u8>,
    pub flash_on_accepted_share: Option<bool>,
    pub flash_on_rejected_share: Option<bool>,
    pub night_mode_disable: Option<bool>,
    pub celebration_on_lucky_share: Option<bool>,
    pub chain_status_blink_codes: Option<bool>,
    pub enabled: Option<bool>,
}

/// Request body for POST /api/led/pattern.
#[derive(Debug, Deserialize)]
pub struct PatternSetRequest {
    /// Pattern name matching a LedPattern variant (e.g., "mining", "error", "sleep").
    pub pattern: String,
}

/// GET /api/led/status — Current LED engine state (live from watch channel).
pub(super) async fn get_led_status(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    if let Some(ref rx) = state.led_status_rx {
        let status = rx.borrow().clone();
        Json(serde_json::json!({
            "enabled": status.enabled,
            "current_pattern": format!("{:?}", status.current_pattern),
            "locate_active": status.locate_active,
            "locate_remaining_s": status.locate_remaining_s,
            "night_mode_active": status.night_mode_active,
            "temperature_c": status.temperature_c,
        }))
    } else {
        // Fallback when LED engine is not running (no GPIO)
        Json(serde_json::json!({
            "enabled": false,
            "current_pattern": "Shutdown",
            "locate_active": false,
            "locate_remaining_s": null,
            "night_mode_active": false,
            "temperature_c": 0.0,
            "note": "LED engine not available (GPIO controller not initialized)",
        }))
    }
}

/// POST /api/led/pattern — Set the active background LED pattern.
///
/// Accepts pattern names: "mining", "error", "sleep", "initializing",
/// "pool_disconnected", "thermal_warning", "fan_failure", "firmware_update", "shutdown".
pub(super) async fn post_led_pattern(
    State(state): State<Arc<AppState>>,
    Json(body): Json<PatternSetRequest>,
) -> impl IntoResponse {
    // CE-111: dedicated config writer — gate on runtime ConfigRw.
    if let Err(response) =
        require_antminer_runtime_capability(&state, RuntimeCapability::ConfigRw, "/api/led/pattern")
    {
        return response;
    }

    use dcentrald_hal::led::{LedCommand, LedPattern};

    let pattern = match body.pattern.to_lowercase().as_str() {
        "booting" => LedPattern::Booting,
        "initializing" | "init" => LedPattern::Initializing,
        "mining" => LedPattern::Mining,
        "error" => LedPattern::Error,
        "fan_failure" | "fanfailure" => LedPattern::FanFailure,
        "thermal_warning" | "thermalwarning" => LedPattern::ThermalWarning,
        "pool_disconnected" | "pooldisconnected" | "idle" => LedPattern::PoolDisconnected,
        "shutdown" | "off" => LedPattern::Shutdown,
        "sleep" => LedPattern::Sleep,
        "firmware_update" | "firmwareupdate" | "update" => LedPattern::FirmwareUpdate,
        _ => {
            return (StatusCode::BAD_REQUEST, Json(serde_json::json!({
                "error": "Unknown pattern",
                "detail": format!("Pattern '{}' not recognized. Valid: mining, error, sleep, initializing, pool_disconnected, thermal_warning, fan_failure, firmware_update, shutdown, booting", body.pattern),
            }))).into_response();
        }
    };

    if let Some(ref led_tx) = state.led_tx {
        match led_tx.try_send(LedCommand::SetPattern(pattern)) {
            Ok(()) => {
                tracing::info!(pattern = %body.pattern, "LED pattern set via API");
                Json(serde_json::json!({
                    "status": "ok",
                    "pattern": body.pattern,
                }))
                .into_response()
            }
            Err(e) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({
                    "error": "Failed to send LED command",
                    "detail": e.to_string(),
                })),
            )
                .into_response(),
        }
    } else {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({
                "error": "LED engine not available",
            })),
        )
            .into_response()
    }
}

/// POST /api/led/locate — Trigger "Find My Miner" blink pattern.
pub(crate) async fn post_led_locate(
    State(state): State<Arc<AppState>>,
    Json(body): Json<LocateRequest>,
) -> impl IntoResponse {
    use dcentrald_hal::led::LedCommand;

    let pattern_id = body.pattern_id.unwrap_or_default();

    if let Some(ref led_tx) = state.led_tx {
        match led_tx.try_send(LedCommand::Locate {
            pattern_id: pattern_id.clone(),
        }) {
            Ok(()) => {
                tracing::info!(pattern = %pattern_id, "Find My Miner triggered via API");
                Json(serde_json::json!({
                    "status": "ok",
                    "message": "Locate pattern started",
                    "pattern_id": pattern_id,
                }))
                .into_response()
            }
            Err(e) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({
                    "error": "Failed to send LED command",
                    "detail": e.to_string(),
                })),
            )
                .into_response(),
        }
    } else {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({
                "error": "LED engine not available",
                "detail": "GPIO controller not initialized (running on non-S9 hardware?)",
            })),
        )
            .into_response()
    }
}

/// POST /api/led/locate/stop — Cancel active locate sequence.
pub(super) async fn post_led_locate_stop(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    use dcentrald_hal::led::LedCommand;

    if let Some(ref led_tx) = state.led_tx {
        let _ = led_tx.try_send(LedCommand::StopLocate);
        Json(serde_json::json!({
            "status": "ok",
            "message": "Locate stopped",
        }))
    } else {
        Json(serde_json::json!({
            "status": "ok",
            "message": "LED engine not available",
        }))
    }
}

/// POST /api/system/identify — stock AxeOS-compatible locate toggle.
pub(super) async fn post_system_identify(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    use dcentrald_hal::led::LedCommand;

    let locate_active = state
        .led_status_rx
        .as_ref()
        .map(|rx| rx.borrow().locate_active)
        .unwrap_or(false);

    if let Some(ref led_tx) = state.led_tx {
        if locate_active {
            let _ = led_tx.try_send(LedCommand::StopLocate);
            Json(serde_json::json!({
                "message": "The device no longer says \"Hi!\".",
                "active": false,
            }))
            .into_response()
        } else {
            match led_tx.try_send(LedCommand::Locate {
                pattern_id: String::new(),
            }) {
                Ok(()) => Json(serde_json::json!({
                    "message": "The device says \"Hi!\" for 30 seconds.",
                    "active": true,
                }))
                .into_response(),
                Err(e) => (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({
                        "error": "Failed to send LED command",
                        "detail": e.to_string(),
                    })),
                )
                    .into_response(),
            }
        }
    } else {
        Json(serde_json::json!({
            "message": "LED engine not available",
            "active": false,
        }))
        .into_response()
    }
}

/// GET /api/led/patterns — List all available blink patterns (for "Find My Miner" locate).
pub(super) async fn get_led_patterns(State(_state): State<Arc<AppState>>) -> impl IntoResponse {
    use dcentrald_hal::led_patterns::BLINK_PATTERNS;

    let patterns: Vec<serde_json::Value> = BLINK_PATTERNS
        .iter()
        .map(|p| {
            serde_json::json!({
                "id": p.id,
                "name": p.name,
                "description": p.description,
                "duration_s": p.duration_ms() as f32 / 1000.0,
                "frame_count": p.frames.len(),
            })
        })
        .collect();

    // Also list the available background LED patterns (state-driven, not sequences)
    let background_patterns = vec![
        serde_json::json!({ "id": "booting", "description": "Boot animation wipe" }),
        serde_json::json!({ "id": "initializing", "description": "Green blinks 2Hz during chain init" }),
        serde_json::json!({ "id": "mining", "description": "Green heartbeat (temp-proportional)" }),
        serde_json::json!({ "id": "error", "description": "Solid red" }),
        serde_json::json!({ "id": "fan_failure", "description": "Fast red blink (3Hz)" }),
        serde_json::json!({ "id": "thermal_warning", "description": "Slow red blink (1Hz)" }),
        serde_json::json!({ "id": "pool_disconnected", "description": "Alternating green/red (0.5Hz)" }),
        serde_json::json!({ "id": "shutdown", "description": "All LEDs off" }),
        serde_json::json!({ "id": "sleep", "description": "Very slow green pulse (0.2Hz)" }),
        serde_json::json!({ "id": "firmware_update", "description": "Fast alternating green/red" }),
    ];

    Json(serde_json::json!({
        "locate_patterns": patterns,
        "locate_count": patterns.len(),
        "background_patterns": background_patterns,
    }))
}

/// GET /api/led/config — Get current LED configuration.
pub(super) async fn get_led_config(State(_state): State<Arc<AppState>>) -> impl IntoResponse {
    // Read current config from TOML file
    let config_path = if std::path::Path::new("/data/dcentrald.toml").exists() {
        "/data/dcentrald.toml"
    } else {
        "/etc/dcentrald.toml"
    };

    match crate::read_led_config(config_path) {
        Ok(led_cfg) => Json(serde_json::json!(led_cfg)).into_response(),
        Err(e) => {
            // Return defaults on error
            tracing::warn!(error = %e, "Failed to read LED config, returning defaults");
            Json(serde_json::json!({
                "enabled": true,
                "heartbeat_on_ms": 100,
                "heartbeat_off_ms": 900,
                "locate_pattern": "imperial_march",
                "locate_duration_s": 30,
                "flash_on_accepted_share": true,
                "flash_on_rejected_share": true,
                "night_mode_disable": false,
                "celebration_on_lucky_share": true,
                "chain_status_blink_codes": true,
            }))
            .into_response()
        }
    }
}

/// POST /api/led/config — Update LED configuration (persists to TOML).
pub(super) async fn post_led_config(
    State(state): State<Arc<AppState>>,
    Json(body): Json<LedConfigUpdateRequest>,
) -> impl IntoResponse {
    // CE-111: dedicated config writer — gate on runtime ConfigRw.
    if let Err(response) =
        require_antminer_runtime_capability(&state, RuntimeCapability::ConfigRw, "/api/led/config")
    {
        return response;
    }

    let config_path = if std::path::Path::new("/data/dcentrald.toml").exists() {
        "/data/dcentrald.toml"
    } else {
        "/etc/dcentrald.toml"
    };

    // Read-modify-write pattern (atomically update the [led] section)
    match crate::update_led_config(config_path, &body) {
        Ok(()) => {
            tracing::info!("LED config updated and saved");
            Json(serde_json::json!({
                "status": "ok",
                "message": "LED configuration saved. Changes take effect on next daemon restart.",
            }))
            .into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({
                "error": "Failed to save LED config",
                "detail": e.to_string(),
            })),
        )
            .into_response(),
    }
}

// ─── Prometheus Metrics Endpoint ─────────────────────────────────────

/// GET /metrics -- Prometheus/OpenMetrics text format.
///
/// Exposes key mining metrics in Prometheus text exposition format
/// (v0.0.4). Supports scraping by Prometheus, Grafana Agent,
/// VictoriaMetrics, etc. Includes: hashrate (instant + 5s rolling),
/// power/wall/efficiency/BTU, per-board temp/hashrate/chips/freq/voltage/
/// errors, aggregate HW-error count + rate, per-fan RPM/PWM, accepted/
/// rejected shares, pool connected/connecting/difficulty, uptime.
///
/// G5: the line buffer is built by the pure, host-testable
/// `dcentrald_api_types::prometheus_metrics` encoder so the exposition
/// format is unit-tested independently of the Windows/HAL test blocker.
/// This is additive — the LuxOS-style 3-tier CSV ring
/// (`/data/metrics/{5s,1m,5m}.csv`, written by the daemon's
/// `metrics_export` task) is a separate, untouched surface.
/// GROUP-B: mean `hashrate_ghs` over the history samples whose `timestamp_s`
/// falls within the last `window_s` seconds of `now_s`.
///
/// LuxOS / BraiinsOS expose 15-minute and 24-hour rolling hashrate averages on
/// their Prometheus surfaces; DCENT_OS already retains the 5-minute history
/// ring buffer (up to 288 samples = 24 h, see `/api/history`), so the averages
/// are computed from that ring at scrape time rather than maintained by a
/// separate publisher task.
///
/// Returns `None` when no sample lands inside the window — the Prometheus
/// encoder then omits the family entirely (no fabricated `0`). Samples with a
/// non-finite or negative hashrate are skipped so a single bad row can't poison
/// the average. Pure + host-testable (no HAL, no clock).
pub(super) fn hashrate_avg_over_window(
    samples: &[SnapshotHistorySample],
    now_s: u64,
    window_s: u64,
) -> Option<f64> {
    let cutoff = now_s.saturating_sub(window_s);
    let mut sum = 0.0_f64;
    let mut count = 0_u64;
    for s in samples {
        if s.timestamp_s < cutoff {
            continue;
        }
        if !s.hashrate_ghs.is_finite() || s.hashrate_ghs < 0.0 {
            continue;
        }
        sum += s.hashrate_ghs;
        count += 1;
    }
    if count == 0 {
        None
    } else {
        Some(sum / count as f64)
    }
}

/// P1-3 (D-9): rated nameplate hashrate (TH/s) for a chip profile and the live
/// chip population.
///
/// Rated capacity = installed silicon at the chip's *rated* clock
/// (`default_freq_mhz`), NOT the live measured hashrate. When the chains have
/// been enumerated (`live_chips > 0`) we scale the real installed chip count by
/// the profile's rated per-chip hashrate; before enumeration we fall back to the
/// profile's full nameplate. Pure + host-testable (no HAL, no clock).
pub(super) fn rated_nominal_ths(
    profile: &dcentrald_asic::drivers::MinerProfile,
    live_chips: u64,
) -> f64 {
    if live_chips > 0 {
        let rated_per_chip_ghs = profile.chip_hashrate_ghs(profile.default_freq_mhz);
        live_chips as f64 * rated_per_chip_ghs / 1000.0
    } else {
        profile.total_hashrate_ths(profile.default_freq_mhz)
    }
}

/// P2-6 §4.C — derive the optional-integration health tri-state for the
/// Prometheus `dcentrald_integration_up{kind=...}` family from the daemon
/// config table. Returns `(mqtt, webhook)` where each is:
///   - `None`        => the `[mqtt]` / `[webhook]` block is absent (not
///                      configured) → its sample is omitted (no fabricated 0)
///   - `Some(true)`  => enabled AND a target is set (broker / URL)
///   - `Some(false)` => block present but disabled or missing its target
///
/// CONTRACT (truth): this reflects CONFIGURED + ENABLED intent — the same
/// condition under which the daemon actually spawns the MQTT publisher /
/// keeps the webhook dispatcher live — NOT a verified broker/endpoint
/// connection. Pure + host-testable (no HAL, no IO).
pub(super) fn integration_up_from_config_table(
    table: &toml::Table,
) -> (Option<bool>, Option<bool>) {
    let mqtt = table.get("mqtt").and_then(|v| v.as_table()).map(|mqtt| {
        let enabled = mqtt
            .get("enabled")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let has_broker = mqtt
            .get("broker")
            .and_then(|v| v.as_str())
            .map(|s| !s.trim().is_empty())
            .unwrap_or(false);
        enabled && has_broker
    });
    let webhook = table.get("webhook").and_then(|v| v.as_table()).map(|wh| {
        let enabled = wh.get("enabled").and_then(|v| v.as_bool()).unwrap_or(false);
        let has_url = wh
            .get("url")
            .and_then(|v| v.as_str())
            .map(|s| !s.trim().is_empty())
            .unwrap_or(false);
        enabled && has_url
    });
    (mqtt, webhook)
}

pub(super) fn build_prometheus_snapshot(
    miner: &crate::MinerState,
    power: &dcentrald_autotuner::LivePowerEstimate,
    hardware: &crate::HardwareInfo,
    hashrate_15m_ghs: Option<f64>,
    hashrate_24h_ghs: Option<f64>,
    autotuner: Option<&dcentrald_autotuner::AutotunerRuntimeStatus>,
    thermal: Option<&dcentrald_thermal::supervisor::SupervisorSnapshot>,
    // P2-6 §4.C — optional-integration health (tri-state, see
    // `PrometheusSnapshot::mqtt_integration_up`). `None` => the integration is
    // not configured and its `dcentrald_integration_up` sample is omitted.
    mqtt_integration_up: Option<bool>,
    webhook_integration_up: Option<bool>,
) -> dcentrald_api_types::prometheus_metrics::PrometheusSnapshot {
    use dcentrald_api_types::prometheus_metrics::{ChainMetric, FanMetric, PrometheusSnapshot};
    let power_projection = project_power_telemetry(power, miner, hardware);

    // Per-fan: prefer the detailed per-fan readings; fall back to the
    // single legacy aggregate fan when no per-fan list is published.
    let fans: Vec<FanMetric> = if miner.fans.per_fan.is_empty() {
        vec![FanMetric {
            id: 0,
            rpm: miner.fans.rpm,
            pwm_percent: miner.fans.pwm,
        }]
    } else {
        miner
            .fans
            .per_fan
            .iter()
            .map(|f| FanMetric {
                id: f.id,
                rpm: f.rpm,
                pwm_percent: f.pwm_percent,
            })
            .collect()
    };

    PrometheusSnapshot {
        firmware_version: miner.firmware_version.clone(),
        chip_model: hardware.chip_type.clone(),
        chip_model_source: if hardware.chip_type.trim().is_empty() {
            "unknown".to_string()
        } else {
            "hardware_info.chip_type".to_string()
        },
        mode: miner.mode.to_string(),
        hashrate_ghs: miner.hashrate_ghs,
        hashrate_5s_ghs: miner.hashrate_5s_ghs,
        // GROUP-B: 15m / 24h rolling averages computed from the 5-minute
        // history ring at scrape time (see `hashrate_avg_over_window`). The
        // encoder omits these families when `None` (empty/cold ring buffer)
        // — never a fabricated 0.
        hashrate_15m_ghs,
        hashrate_24h_ghs,
        board_watts: power_projection.board_watts as f64,
        wall_watts: power_projection.wall_watts as f64,
        efficiency_jth: power_projection.efficiency_jth,
        btu_h: power_projection.btu_h,
        power_live_available: power_projection.live_power_available,
        power_modeled: power_projection.modeled,
        power_source: power_projection.source,
        power_source_detail: power_projection.source_detail.to_string(),
        shares_accepted: miner.accepted,
        shares_rejected: miner.rejected,
        pool_connected: is_pool_connected(&miner.pool.status),
        pool_connecting: is_pool_connecting(&miner.pool.status),
        pool_difficulty: miner.pool.difficulty,
        pool_latency_ms: miner.pool.latency_ms,
        uptime_seconds: miner.uptime_s,
        chains: miner
            .chains
            .iter()
            .map(|c| ChainMetric {
                id: c.id,
                chips: c.chips as u32,
                frequency_mhz: c.frequency_mhz as u32,
                voltage_mv: c.voltage_mv as u32,
                temp_c: c.temp_c,
                hashrate_ghs: c.hashrate_ghs,
                errors: c.errors as u64,
            })
            .collect(),
        fans,
        // W17 fleet/Grafana parity — surface the W9/W15 autotuner-silicon +
        // Wave-G chip-imbalance telemetry on the Prometheus consumer too (the
        // dashboard got it in W16). All detail fields stay `None` until real
        // data exists, so the encoder omits the families rather than reporting
        // a fabricated zero/grade-A distribution.
        autotuner_enabled: autotuner.map(|a| a.enabled).unwrap_or(false),
        autotuner_percent_complete: autotuner.map(|a| a.percent_complete),
        autotuner_active_chips: autotuner.map(|a| a.active_chips as u32),
        autotuner_total_chips: autotuner.map(|a| a.total_chips as u32),
        silicon_grade_counts: autotuner
            .and_then(|a| a.silicon_grades.as_ref())
            .map(|g| [g.a as u32, g.b as u32, g.c as u32, g.d as u32]),
        thermal_supervisor_enabled: thermal.map(|t| t.enabled).unwrap_or(false),
        chip_imbalance_worst_c: thermal
            .and_then(|t| t.worst_chip_imbalance_c)
            .map(|v| v as f64),
        chip_imbalance_threshold_c: thermal.map(|t| t.chip_imbalance_threshold_c as f64),
        chip_imbalance_flagged: thermal
            .map(|t| t.board_states.iter().any(|b| b.chip_imbalance_flagged))
            .unwrap_or(false),
        // P2-6 §4.C — fleet-grade {pool,worker} share-counter labels +
        // donation gauge sourced from the live pool state. OBS-1/OBS-2: the
        // worker is the operator's full BTC payout address on V1 solo and the
        // pool URL can embed user:pass@ creds — so mask/sanitize before they
        // become Prometheus labels (every other surface already does). The
        // masked/sanitized forms are still stable, unique-enough label series.
        pool_label: dcentrald_stratum::pool_api::sanitize_pool_url(&miner.pool.url),
        worker_label: dcentrald_common::wallet_mask::mask_wallet(&miner.pool.worker),
        donation_active: miner.pool.donating,
        mqtt_integration_up,
        webhook_integration_up,
    }
}

pub(super) async fn get_metrics(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let miner = state.state_rx.borrow().clone();
    let power = state.power_rx.borrow().clone();
    let hardware = state
        .hardware_info
        .lock()
        .map(|g| g.clone())
        .unwrap_or_default();

    // GROUP-B: derive the LuxOS/BOS-style 15m + 24h rolling hashrate averages
    // from the 5-minute history ring buffer (same source as `/api/history`).
    // Both are `None` until the ring has at least one sample inside the window,
    // which keeps the families out of the exposition until there's real data.
    let history = read_history_samples(&state);
    let now_s = unix_time_ms() / 1000;
    let hashrate_15m_ghs = hashrate_avg_over_window(&history, now_s, 15 * 60);
    let hashrate_24h_ghs = hashrate_avg_over_window(&history, now_s, 24 * 60 * 60);

    // W17: also publish the W9/W15 autotuner-silicon + Wave-G chip-imbalance
    // telemetry (read-only watch-channel snapshots, same sources the REST
    // routes use). Absent/None telemetry is omitted by the encoder.
    let autotuner_status = state.autotuner_status_rx.borrow().clone();
    let thermal = crate::thermal_supervisor_snapshot();

    // P2-6 §4.C — optional-integration health from the daemon config table.
    // An absent `[mqtt]`/`[webhook]` block yields `None` (sample omitted); a
    // present block is reported enabled-with-target vs not. Reading the config
    // at scrape time matches how the daemon hot-reloads these toggles (no
    // restart), and mirrors how `/api/status` already reads the donation block.
    // P3-2: post-write-fresh in-memory config cache instead of re-parsing
    // dcentrald.toml on every Prometheus scrape. An empty table (absent config)
    // yields (None, None), identical to the prior unwrap_or((None, None)).
    let (mqtt_integration_up, webhook_integration_up) =
        integration_up_from_config_table(&state.config_cache.snapshot());

    let snapshot = build_prometheus_snapshot(
        &miner,
        &power,
        &hardware,
        hashrate_15m_ghs,
        hashrate_24h_ghs,
        Some(&autotuner_status),
        thermal.as_ref(),
        mqtt_integration_up,
        webhook_integration_up,
    );
    let body = snapshot.to_exposition();
    (
        [(
            axum::http::header::CONTENT_TYPE,
            dcentrald_api_types::prometheus_metrics::CONTENT_TYPE,
        )],
        body,
    )
}

// ─── System Upgrade ──────────────────────────────────────────────────────

pub(super) async fn get_system_upgrade_status() -> impl IntoResponse {
    let stage_root = Path::new(SYSTEM_UPGRADE_STAGE_ROOT);
    let stage_entries = read_upgrade_stage_entries(stage_root);
    let fwenv = read_upgrade_fwenv_snapshot();
    Json(build_system_upgrade_status_payload(
        SYSTEM_UPGRADE_STAGE_ROOT,
        stage_root.exists(),
        &stage_entries,
        &fwenv,
    ))
}

#[derive(Debug, Default)]
pub(super) struct UpgradeFwEnvSnapshot {
    upgrade_stage: Option<String>,
    bootcount: Option<String>,
    bootlimit: Option<String>,
    boot_slot: Option<String>,
    dcent_boot_slot: Option<String>,
    active_slot: Option<String>,
}

impl UpgradeFwEnvSnapshot {
    fn selected_boot_slot(&self) -> Option<&str> {
        self.boot_slot
            .as_deref()
            .or(self.dcent_boot_slot.as_deref())
            .or(self.active_slot.as_deref())
    }
}

pub(super) fn read_upgrade_fwenv_snapshot() -> UpgradeFwEnvSnapshot {
    UpgradeFwEnvSnapshot {
        upgrade_stage: read_fw_printenv_key("upgrade_stage"),
        bootcount: read_fw_printenv_key("bootcount"),
        bootlimit: read_fw_printenv_key("bootlimit"),
        boot_slot: read_fw_printenv_key("boot_slot"),
        dcent_boot_slot: read_fw_printenv_key("dcent_boot_slot"),
        active_slot: read_fw_printenv_key("active_slot"),
    }
}

pub(super) fn build_system_upgrade_status_payload(
    stage_root: &str,
    stage_root_present: bool,
    stage_entries: &[UpgradeStageEntry],
    fwenv: &UpgradeFwEnvSnapshot,
) -> serde_json::Value {
    let staged_packages: Vec<serde_json::Value> = stage_entries
        .iter()
        .filter(|entry| entry.is_tar)
        .map(|entry| {
            serde_json::json!({
                "path": entry.path,
                "filename": entry.filename,
                "size_bytes": entry.size_bytes,
                "modified_ms": entry.modified_ms,
                "source": "browser_staging_dir",
            })
        })
        .collect();
    let staged_package_count = staged_packages.len();

    let stage_state = if fwenv.upgrade_stage.is_some() {
        "pending_boot_commit"
    } else if !staged_packages.is_empty() {
        "validated_or_staged"
    } else {
        "idle"
    };

    serde_json::json!({
        "status": "ok",
        "read_only": true,
        "state": stage_state,
        "stage_root": stage_root,
        "stage_root_present": stage_root_present,
        "staged_packages": staged_packages,
        "staged_package_count": staged_package_count,
        "upgrade_stage": fwenv.upgrade_stage.as_deref(),
        "bootcount": fwenv.bootcount.as_deref(),
        "bootlimit": fwenv.bootlimit.as_deref(),
        "boot_slot": fwenv.selected_boot_slot(),
        "sources": {
            "staged_packages": stage_root,
            "upgrade_stage": "fw_printenv upgrade_stage",
            "bootcount": "fw_printenv bootcount",
            "bootlimit": "fw_printenv bootlimit",
            "boot_slot": "fw_printenv boot_slot|dcent_boot_slot|active_slot"
        },
        "limitations": [
            "This endpoint is read-only and does not validate, flash, reboot, or clear rollback state.",
            "If fw_printenv is unavailable, U-Boot environment fields are null rather than guessed.",
            "Validated package details are best-effort from the browser staging directory only."
        ],
    })
}

#[derive(Debug)]
pub(super) struct UpgradeStageEntry {
    path: String,
    filename: String,
    size_bytes: u64,
    modified_ms: Option<u64>,
    is_tar: bool,
}

pub(super) fn read_upgrade_stage_entries(stage_root: &Path) -> Vec<UpgradeStageEntry> {
    let mut entries = Vec::new();
    collect_upgrade_stage_entries(stage_root, 0, &mut entries);
    entries.sort_by(|a, b| b.modified_ms.cmp(&a.modified_ms));
    entries
}

pub(super) fn collect_upgrade_stage_entries(
    path: &Path,
    depth: usize,
    entries: &mut Vec<UpgradeStageEntry>,
) {
    if depth > 2 || entries.len() >= 32 {
        return;
    }

    let Ok(read_dir) = std::fs::read_dir(path) else {
        return;
    };

    for item in read_dir.flatten() {
        if entries.len() >= 32 {
            return;
        }

        let item_path = item.path();
        let Ok(meta) = item.metadata() else {
            continue;
        };

        if meta.is_dir() {
            collect_upgrade_stage_entries(&item_path, depth + 1, entries);
            continue;
        }

        if !meta.is_file() {
            continue;
        }

        let filename = item_path
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or_default()
            .to_string();
        let is_tar = item_path
            .extension()
            .and_then(|value| value.to_str())
            .map(|value| value.eq_ignore_ascii_case("tar"))
            .unwrap_or(false);

        entries.push(UpgradeStageEntry {
            path: item_path.to_string_lossy().into_owned(),
            filename,
            size_bytes: meta.len(),
            modified_ms: meta.modified().ok().and_then(system_time_to_ms),
            is_tar,
        });
    }
}

pub(super) fn system_time_to_ms(value: std::time::SystemTime) -> Option<u64> {
    value
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|duration| duration.as_millis() as u64)
}

pub(super) fn read_fw_printenv_key(key: &str) -> Option<String> {
    let output = std::process::Command::new("fw_printenv")
        .arg("-n")
        .arg(key)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let value = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if value.is_empty() {
        None
    } else {
        Some(value)
    }
}

async fn read_running_board_target_for_update() -> std::result::Result<String, String> {
    let path = Path::new(SYSTEM_BOARD_TARGET_PATH);
    let metadata = tokio::fs::symlink_metadata(path).await.map_err(|error| {
        format!(
            "OTA policy: failed to inspect canonical board target marker '{}': {error}",
            path.display()
        )
    })?;
    if !metadata.file_type().is_file() {
        return Err(format!(
            "OTA policy: canonical board target marker '{}' must be a direct regular file",
            path.display()
        ));
    }
    if metadata.len() > crate::ota_signature::MAX_BOARD_TARGET_MARKER_BYTES as u64 {
        return Err(format!(
            "OTA policy: canonical board target marker exceeds {} bytes",
            crate::ota_signature::MAX_BOARD_TARGET_MARKER_BYTES
        ));
    }
    let bytes = tokio::fs::read(path).await.map_err(|error| {
        format!(
            "OTA policy: failed to read canonical board target marker '{}': {error}",
            path.display()
        )
    })?;
    crate::ota_signature::parse_board_target_marker(&bytes)
        .map_err(|error| format!("OTA policy: invalid canonical board target marker: {error}"))
}

pub(super) async fn post_system_upgrade(
    State(state): State<Arc<AppState>>,
    mut multipart: Multipart,
) -> impl IntoResponse {
    let descriptor = current_antminer_capability_descriptor(&state);
    if let Some(error) = runtime_capability_guard_error(
        &descriptor,
        RuntimeCapability::FlashOta,
        "/api/system/upgrade",
    ) {
        return capability_error_tuple(error);
    }

    // Bind the endpoint to the immutable exact target before creating a stage
    // directory or accepting bytes. Unknown, lab-only, single-slot, and
    // offline-analysis policies fail without mutating local staging state.
    let running_board_target = match read_running_board_target_for_update().await {
        Ok(target) => target,
        Err(message) => return system_upgrade_error(StatusCode::FORBIDDEN, message),
    };
    if let Err(message) = crate::ota_signature::require_public_update_policy(&running_board_target)
    {
        return system_upgrade_error(StatusCode::FORBIDDEN, message);
    }

    let mut apply_update = false;
    let mut staged_path: Option<String> = None;
    let mut requested_staged_path: Option<String> = None;
    let mut uploaded_name: Option<String> = None;
    let mut bytes_written = 0u64;

    if let Err(err) = tokio::fs::create_dir_all(SYSTEM_UPGRADE_STAGE_ROOT).await {
        return system_upgrade_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to create staging directory: {err}"),
        );
    }

    loop {
        let next_field = match multipart.next_field().await {
            Ok(field) => field,
            Err(err) => {
                return system_upgrade_error(
                    StatusCode::BAD_REQUEST,
                    format!("Failed to read multipart upload: {err}"),
                );
            }
        };

        let Some(mut field) = next_field else {
            break;
        };

        let field_name = field.name().unwrap_or_default().to_string();

        if field.file_name().is_none() {
            if field_name == "apply" {
                match field.text().await {
                    Ok(value) => {
                        let value = value.trim().to_ascii_lowercase();
                        apply_update = matches!(value.as_str(), "1" | "true" | "yes" | "on");
                    }
                    Err(err) => {
                        return system_upgrade_error(
                            StatusCode::BAD_REQUEST,
                            format!("Failed to read apply flag: {err}"),
                        );
                    }
                }
            } else if field_name == "staged_path" {
                match field.text().await {
                    Ok(value) => {
                        let value = value.trim();
                        if !value.is_empty() {
                            requested_staged_path = Some(value.to_string());
                        }
                    }
                    Err(err) => {
                        return system_upgrade_error(
                            StatusCode::BAD_REQUEST,
                            format!("Failed to read staged package path: {err}"),
                        );
                    }
                }
            }
            continue;
        }

        if requested_staged_path.is_some() {
            return system_upgrade_error(
                StatusCode::BAD_REQUEST,
                "Choose either a new upload or a previously staged package, not both",
            );
        }

        if staged_path.is_some() {
            return system_upgrade_error(
                StatusCode::BAD_REQUEST,
                "Upload exactly one signed sysupgrade package per request",
            );
        }

        let original_name = field.file_name().unwrap_or("dcentos-sysupgrade.tar");
        let safe_name = sanitize_upload_filename(original_name);
        //  W9-A — accept stock Bitmain tarballs (.tar.gz / .tgz / .bmu)
        // alongside DCENT_OS signed sysupgrade .tar packages, per R3-CRITICAL-3.
        // The restore-to-stock flash path (routes::restore_to_stock) consumes the
        // stock tarball directly via the `revert_to_stock.sh` shell logic (which
        // already handles `tar xzf`). DCENT_OS sysupgrade still rejects the
        // gzipped/.bmu variants downstream — this filter only governs upload
        // staging.
        let lower = safe_name.to_ascii_lowercase();
        let extension_ok = lower.ends_with(".tar")
            || lower.ends_with(".tar.gz")
            || lower.ends_with(".tgz")
            || lower.ends_with(".bmu");
        if !extension_ok {
            return system_upgrade_error(
                StatusCode::BAD_REQUEST,
                "Only sysupgrade packages (.tar) or stock Bitmain firmware archives (.tar.gz, .tgz, .bmu) are accepted by the browser updater",
            );
        }

        let stage_dir = format!("{}/{}", SYSTEM_UPGRADE_STAGE_ROOT, uuid::Uuid::new_v4());
        if let Err(err) = tokio::fs::create_dir_all(&stage_dir).await {
            return system_upgrade_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to create upload staging directory: {err}"),
            );
        }

        let output_path = format!("{}/{}", stage_dir, safe_name);
        let mut output = match tokio::fs::File::create(&output_path).await {
            Ok(file) => file,
            Err(err) => {
                return system_upgrade_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("Failed to create staged package: {err}"),
                );
            }
        };

        loop {
            match field.chunk().await {
                Ok(Some(chunk)) => {
                    bytes_written = bytes_written.saturating_add(chunk.len() as u64);
                    if bytes_written > SYSTEM_UPGRADE_MAX_UPLOAD_BYTES as u64 {
                        let _ = tokio::fs::remove_file(&output_path).await;
                        return system_upgrade_error(
                            StatusCode::PAYLOAD_TOO_LARGE,
                            format!(
                                "Package exceeds {} MiB upload limit",
                                SYSTEM_UPGRADE_MAX_UPLOAD_BYTES / 1024 / 1024
                            ),
                        );
                    }

                    if let Err(err) = output.write_all(&chunk).await {
                        let _ = tokio::fs::remove_file(&output_path).await;
                        return system_upgrade_error(
                            StatusCode::INTERNAL_SERVER_ERROR,
                            format!("Failed to stage uploaded package: {err}"),
                        );
                    }
                }
                Ok(None) => break,
                Err(err) => {
                    let _ = tokio::fs::remove_file(&output_path).await;
                    return system_upgrade_error(
                        StatusCode::BAD_REQUEST,
                        format!("Failed while reading upload stream: {err}"),
                    );
                }
            }
        }

        if let Err(err) = output.flush().await {
            return system_upgrade_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to flush staged package to disk: {err}"),
            );
        }

        staged_path = Some(output_path);
        uploaded_name = Some(safe_name);
    }

    let (staged_path, reused_staged_path) = if let Some(staged_path) = staged_path {
        if bytes_written == 0 {
            return system_upgrade_error(StatusCode::BAD_REQUEST, "Uploaded package is empty");
        }
        (staged_path, false)
    } else if let Some(requested_staged_path) = requested_staged_path {
        match resolve_staged_upgrade_path(&requested_staged_path).await {
            Ok(resolved_path) => {
                uploaded_name = Path::new(&resolved_path)
                    .file_name()
                    .and_then(|value| value.to_str())
                    .map(|value| value.to_string());
                (resolved_path, true)
            }
            Err(message) => {
                return system_upgrade_error(StatusCode::BAD_REQUEST, message);
            }
        }
    } else {
        return system_upgrade_error(
            StatusCode::BAD_REQUEST,
            "Upload a firmware package or provide a previously staged package path",
        );
    };

    //  A3-HIGH-1: route stock Bitmain shapes (.tar.gz/.tgz/.bmu)
    // OUT of the Ed25519-signed sysupgrade verifier. The W9-A widening
    // accepts these for STAGING so the restore-to-stock flow can find
    // them later via /api/system/restore-to-stock with `confirm:true`,
    // but they are NOT signed DCENT_OS bundles and would always fail
    // `verify_sysupgrade_bundle()`. Returning success here means the
    // operator/dashboard sees the staged_path and can hand it to the
    // restore-to-stock handler, which has its own preflight + IOC scan
    // + UBI shape check. The legacy `sysupgrade --test` path below is
    // ALSO skipped for stock tarballs — it would reject them, and the
    // restore-to-stock flow uses revert_to_stock.sh, not sysupgrade.
    let staged_lower = staged_path.to_ascii_lowercase();
    // Content-first routing (SEC hardening): decide whether to skip the Ed25519
    // sysupgrade verifier by the file's actual CONTENT, not its
    // attacker-controllable filename suffix. A file that is genuinely
    // gzip-compressed (magic 1f 8b) cannot be a valid UNCOMPRESSED DCENT_OS
    // sysupgrade tar, so it is a compressed/stock shape regardless of its name —
    // and, conversely, an uncompressed tar named `foo.tar.gz` is NOT diverted away
    // from verification by its name. (BMU is a distinct Bitmain container with no
    // signed-tar shape to be confused with; it stays name-recognized, and either
    // way the restore-to-stock flow re-gates with its own preflight/IOC/UBI checks.)
    let staged_is_gzip = {
        use std::io::Read;
        std::fs::File::open(&staged_path)
            .ok()
            .and_then(|mut f| {
                let mut magic = [0u8; 2];
                f.read_exact(&mut magic).ok().map(|_| magic)
            })
            .map(|magic| magic == [0x1f, 0x8b])
            .unwrap_or(false)
    };
    let is_stock_bitmain_shape = staged_is_gzip || staged_lower.ends_with(".bmu");

    if is_stock_bitmain_shape {
        tracing::info!(
            staged_path = %staged_path,
            "Browser upload staged stock Bitmain firmware shape — skipping DCENT_OS Ed25519 \
             verification + sysupgrade --test (caller should hand staged_path to /api/system/restore-to-stock)"
        );
        crate::push_audit_event(
            &state,
            "operator",
            dcentrald_api_types::audit_log::AuditEvent::Free {
                category: "firmware_update".to_string(),
                message: format!(
                    "stock Bitmain firmware staged for restore-to-stock: path={staged_path}"
                ),
            },
        );
        return (
            StatusCode::OK,
            Json(serde_json::json!({
                "status": "staged",
                "staged_path": staged_path,
                "uploaded_name": uploaded_name,
                "shape": "stock_bitmain",
                "next_step": "POST /api/system/restore-to-stock with stock_firmware_staged_path = staged_path",
                "warning": "This is a stock Bitmain firmware archive, NOT a signed DCENT_OS sysupgrade. It cannot be installed via the legacy sysupgrade flow. Use Restore-to-Stock instead."
            })),
        );
    }

    let verified_bundle = match crate::ota_signature::verify_sysupgrade_bundle(
        Path::new(&staged_path),
        false,
        Some(Path::new(SYSTEM_UPGRADE_RELEASE_PUBKEY)),
    ) {
        Ok(bundle) => bundle,
        Err(message) => {
            tracing::warn!(
                staged_path = %staged_path,
                verifier_error = %message,
                "Browser sysupgrade in-Rust validation failed before sysupgrade --test"
            );
            return system_upgrade_error(
                StatusCode::BAD_REQUEST,
                format!("Package signature verification failed: {message}"),
            );
        }
    };
    let authorized_sysupgrade = match verified_bundle.authorize_public_update(&running_board_target)
    {
        Ok(authorized) => authorized,
        Err(message) => {
            tracing::warn!(
                staged_path = %staged_path,
                running_board_target = %running_board_target,
                authorization_error = %message,
                "Browser sysupgrade policy authorization failed before sysupgrade --test"
            );
            return system_upgrade_error(
                StatusCode::BAD_REQUEST,
                format!("Package authorization failed: {message}"),
            );
        }
    };

    tracing::info!(
        staged_path = %staged_path,
        board_target = authorized_sysupgrade.board_target(),
        candidate_version = authorized_sysupgrade.version(),
        "Browser sysupgrade in-Rust validation succeeded before sysupgrade --test"
    );

    // W24-OTA-1: enforce downgrade protection ON THE WRITE PATH. The signature
    // is valid, but a signed-but-OLDER package (rollback attack with a leaked
    // prior signing key, or an accidental fleet downgrade to a known-bad
    // release) must still be refused. `assess_rollback` was implemented +
    // tested + advertised by /api/system/update_capability
    // ("default_denies_downgrade": true) but was never called here. We read the
    // candidate `version` from the (now signature-verified) MANIFEST.json and
    // compare it to the running firmware version, FAIL-CLOSED: a denied
    // downgrade, a malformed version, OR a missing version (F2) is HTTP 400,
    // no flash scheduled. There
    // is no operator downgrade-override flag in the codebase today, so
    // allow_downgrade is hard-false (matches the advertised default).
    let candidate_version = authorized_sysupgrade.version().to_string();
    {
        let current_version = env!("CARGO_PKG_VERSION");
        let verdict = dcentrald_api_types::ota_rollback_protection::assess_rollback(
            &candidate_version,
            current_version,
            false, // no operator downgrade-override exists; deny by default
        );
        if !verdict.is_allowed() {
            tracing::warn!(
                staged_path = %staged_path,
                candidate_version = %candidate_version,
                current_version = %current_version,
                ?verdict,
                "OTA rollback protection DENIED the sysupgrade on the write path"
            );
            let reason = match &verdict {
                    dcentrald_api_types::ota_rollback_protection::RollbackVerdict::DenyOlderVersion {
                        candidate,
                        current,
                    } => format!(
                        "Downgrade refused: package version {candidate} is older than the \
                         running firmware version {current}. DCENT_OS denies firmware \
                         downgrades by default."
                    ),
                    dcentrald_api_types::ota_rollback_protection::RollbackVerdict::DenyMalformedVersion {
                        problem,
                    } => format!("Refusing sysupgrade: unparseable firmware version ({problem})."),
                    _ => "Refusing sysupgrade: rollback protection denied the package.".to_string(),
                };
            crate::push_audit_event(
                    &state,
                    "operator",
                    dcentrald_api_types::audit_log::AuditEvent::Free {
                        category: "firmware_update".to_string(),
                        message: format!(
                            "OTA rollback protection denied sysupgrade: candidate={candidate_version} current={current_version}"
                        ),
                    },
                );
            return system_upgrade_error(StatusCode::BAD_REQUEST, reason);
        }
        tracing::info!(
            staged_path = %staged_path,
            candidate_version = %candidate_version,
            current_version = %current_version,
            ?verdict,
            "OTA rollback protection ALLOWED the sysupgrade"
        );
    }

    // Only signed sysupgrade .tar packages are accepted by the browser updater.
    let verify_output = match tokio::process::Command::new("sysupgrade")
        .args(["--test", &staged_path])
        .output()
        .await
    {
        Ok(output) if output.status.success() => output,
        Ok(output) => {
            let message = command_output_message(&output);
            tracing::warn!(staged_path = %staged_path, verifier_output = %message, "Browser sysupgrade validation failed");
            return system_upgrade_error(
                StatusCode::BAD_REQUEST,
                format!("Package verification failed: {message}"),
            );
        }
        Err(err) => {
            return system_upgrade_error(
                StatusCode::SERVICE_UNAVAILABLE,
                format!("sysupgrade verifier is unavailable on this target: {err}"),
            );
        }
    };

    let verifier_message = command_output_message(&verify_output);
    tracing::info!(staged_path = %staged_path, reused_staged_path, verifier_output = %verifier_message, "Browser sysupgrade validation succeeded");
    crate::push_audit_event(
        &state,
        "operator",
        dcentrald_api_types::audit_log::AuditEvent::Free {
            category: "firmware_update".to_string(),
            message: format!(
                "DCENT_OS sysupgrade staged and validated: path={staged_path}; reused_staged_path={reused_staged_path}"
            ),
        },
    );

    if apply_update {
        let upgrade_path = staged_path.clone();
        crate::push_audit_event(
            &state,
            "operator",
            dcentrald_api_types::audit_log::AuditEvent::Free {
                category: "firmware_update".to_string(),
                message: format!("DCENT_OS sysupgrade scheduled: path={staged_path}"),
            },
        );
        // wf_c00e5d9e no-brick A/B follow-up (2026-05-29): the flash runs in a
        // detached task AFTER the HTTP 200, so previously its success/failure was
        // visible ONLY in the daemon tracing log — an operator polling the API
        // could not tell whether the inactive-slot write actually completed. Push
        // a terminal firmware_update audit event on every outcome so the result is
        // operator-pollable via the audit-log API (mirrors the "scheduled" event
        // pushed above). Pure observability addition — the flash behavior, the
        // tracing logs, and the HTTP response are all unchanged. The completion
        // event is still NOT version/boot/rollback proof (the unit reboots after);
        // it only reports the sysupgrade command's own exit disposition.
        let audit_state = state.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            tracing::info!(upgrade_path = %upgrade_path, "Launching validated sysupgrade from browser upload");
            match tokio::process::Command::new("sysupgrade")
                .args(["-f", &upgrade_path])
                .output()
                .await
            {
                Ok(output) => {
                    let message = command_output_message(&output);
                    if output.status.success() {
                        tracing::info!(upgrade_path = %upgrade_path, sysupgrade_output = %message, "Browser sysupgrade command completed");
                        crate::push_audit_event(
                            &audit_state,
                            "system",
                            dcentrald_api_types::audit_log::AuditEvent::Free {
                                category: "firmware_update".to_string(),
                                message: format!(
                                    "DCENT_OS sysupgrade command completed (inactive slot written; reboot/boot-commit pending — not yet version/rollback proof): path={upgrade_path}"
                                ),
                            },
                        );
                    } else {
                        tracing::error!(upgrade_path = %upgrade_path, sysupgrade_output = %message, "Browser sysupgrade command failed after validation");
                        crate::push_audit_event(
                            &audit_state,
                            "system",
                            dcentrald_api_types::audit_log::AuditEvent::Free {
                                category: "firmware_update".to_string(),
                                message: format!(
                                    "DCENT_OS sysupgrade command FAILED after validation (inactive slot NOT confirmed written): path={upgrade_path}: {message}"
                                ),
                            },
                        );
                    }
                }
                Err(err) => {
                    tracing::error!(upgrade_path = %upgrade_path, error = %err, "Failed to launch browser sysupgrade command");
                    crate::push_audit_event(
                        &audit_state,
                        "system",
                        dcentrald_api_types::audit_log::AuditEvent::Free {
                            category: "firmware_update".to_string(),
                            message: format!(
                                "DCENT_OS sysupgrade FAILED to launch (no flash performed): path={upgrade_path}: {err}"
                            ),
                        },
                    );
                }
            }
        });

        return (
            StatusCode::OK,
            Json(serde_json::json!({
                "status": "ok",
                "message": if reused_staged_path {
                    "Previously staged package signature and target preflight passed; inactive-slot sysupgrade has been scheduled. The miner may reboot after flashing completes."
                } else {
                    "Package signature and target preflight passed; inactive-slot sysupgrade has been scheduled. The miner may reboot after flashing completes."
                },
                "filename": uploaded_name,
                "staged_path": staged_path,
                "bytes_written": if reused_staged_path { serde_json::Value::Null } else { serde_json::json!(bytes_written) },
                "validation_only": false,
                "update_started": true,
                "reused_staged_path": reused_staged_path,
            })),
        );
    }

    (
        StatusCode::OK,
        Json(serde_json::json!({
            "status": "ok",
            "message": if reused_staged_path {
                "Previously staged package signature and target preflight passed. Schedule flashing explicitly to write the inactive slot."
            } else {
                "Package signature and target preflight passed; package is staged under /tmp. Schedule flashing explicitly to write the inactive slot."
            },
            "filename": uploaded_name,
            "staged_path": staged_path,
            "bytes_written": if reused_staged_path { serde_json::Value::Null } else { serde_json::json!(bytes_written) },
            "validation_only": true,
            "update_started": false,
            "reused_staged_path": reused_staged_path,
        })),
    )
}

///  HIGH-1/2/3 (2026-05-24) inline tests for the pure helpers
/// above. Host-runnable; no HAL dependency. Pins the
/// fingerprint-detection logic + the env-constant counts so the
/// dashboard's `a lab unit`-class detection contract cannot regress silently.
#[cfg(test)]
mod wave55a_recipe_rest_tests {
    use super::*;

    #[test]
    fn fingerprint_matches_canonical_xil_25_shapes() {
        assert!(w55a_is_xil_25_class(
            "zynq-bm3-am2",
            "am2-xil",
            Some("loki")
        ));
        assert!(w55a_is_xil_25_class(
            "zynq-bm3-am2",
            "am2-s19jpro-xil",
            Some("loki")
        ));
        // psu_hardware_variant absent — still matches.
        assert!(w55a_is_xil_25_class("zynq-bm3-am2", "am2-xil", None));
        // psu_hardware_variant empty-string treated as absent.
        assert!(w55a_is_xil_25_class("zynq-bm3-am2", "am2-xil", Some("")));
    }

    #[test]
    fn fingerprint_rejects_non_xil_25_units() {
        // S9
        assert!(!w55a_is_xil_25_class("zynq-bm1-s9", "am1-s9", None));
        // .109 XIL but not `a lab unit`
        assert!(!w55a_is_xil_25_class(
            "zynq-bm3-am2",
            "am2-s19jpro",
            Some("loki")
        ));
        // .135 AML
        assert!(!w55a_is_xil_25_class("amlogic-a113d", "am3-aml-s21", None));
        // .79 BB
        assert!(!w55a_is_xil_25_class("am335x-bb", "am3-bb-s19jpro", None));
        // .129 S19 Pro
        assert!(!w55a_is_xil_25_class("zynq-bm3-am2", "am2-s19pro", None));
    }

    #[test]
    fn fingerprint_rejects_explicit_non_loki_psu_variant() {
        // Operator declared bare-apw3 (not Loki) → don't match.
        assert!(!w55a_is_xil_25_class(
            "zynq-bm3-am2",
            "am2-xil",
            Some("bare-apw3")
        ));
    }

    #[test]
    fn env_truthy_matches_dcent_convention() {
        for v in &["1", "true", "TRUE", "yes", "YES", "on", "ON"] {
            assert!(w55a_env_value_is_truthy(v), "{v:?} must be truthy");
        }
        for v in &["", "0", "false", "FALSIFIED", "no", "off"] {
            assert!(!w55a_env_value_is_truthy(v), "{v:?} must NOT be truthy");
        }
    }

    #[test]
    fn forbidden_list_matches_guard_constant_count() {
        // Defensive: if the guard adds a 5th forbidden var, this REST
        // module's mirror must be updated in lockstep. Drift check.
        assert_eq!(
            WAVE54_FORBIDDEN_ENV_VARS.len(),
            4,
            "Wave-55a REST mirror of WAVE54_FORBIDDEN_ENV_VARS must match the \
             dcentrald::wave55a_recipe_guard count (4); if the guard added \
             vars, update both here and in tests/wave54_proven_mining_recipe.rs."
        );
    }

    #[test]
    fn required_list_matches_test_constant_count() {
        // Mirror the 13-required count from
        // `tests/wave54_proven_mining_recipe.rs`. Drift = silent dashboard lie.
        assert_eq!(
            WAVE54_REQUIRED_ENV_VARS.len(),
            13,
            "Wave-55a REST mirror of WAVE54_REQUIRED_ENV_VARS must match the \
             13-env recipe; any subset has been LIVE-FALSIFIED on `a lab unit`."
        );
    }
}

#[cfg(test)]
mod capability_contract_tests {
    use super::*;
    use dcent_schema::capability::CapabilityErrorKind;

    fn empty_miner() -> crate::MinerState {
        crate::MinerState::empty(crate::OperatingMode::Standard)
    }

    fn unproven_hardware(chip_type: &str, control_board: &str) -> crate::HardwareInfo {
        crate::HardwareInfo {
            chip_type: chip_type.to_string(),
            control_board: control_board.to_string(),
            ..crate::HardwareInfo::default()
        }
    }

    fn measured_hardware(
        chip_type: &str,
        chip_id: u16,
        control_board: &str,
        board_target: &str,
    ) -> crate::HardwareInfo {
        crate::HardwareInfo {
            chip_type: chip_type.to_string(),
            control_board: control_board.to_string(),
            identification: crate::HardwareIdentification::from_evidence(
                vec![
                    crate::HardwareIdentityEvidence::declared_asic_board_target(
                        board_target,
                        chip_type,
                    ),
                    crate::HardwareIdentityEvidence::measured_asic_enumeration(
                        chip_id,
                        chip_type,
                        crate::HardwareCompositionToken::new(1, format!("test:{board_target}")),
                    ),
                ],
                Some("test identity with enumeration evidence".to_string()),
            ),
            ..crate::HardwareInfo::default()
        }
    }

    fn declared_hardware(
        chip_type: &str,
        control_board: &str,
        board_target: &str,
    ) -> crate::HardwareInfo {
        crate::HardwareInfo {
            chip_type: chip_type.to_string(),
            control_board: control_board.to_string(),
            identification: crate::HardwareIdentification::from_evidence(
                vec![crate::HardwareIdentityEvidence::declared_asic_board_target(
                    board_target,
                    chip_type,
                )],
                Some("test declared identity".to_string()),
            ),
            ..crate::HardwareInfo::default()
        }
    }

    #[test]
    fn capabilities_unknown_identity_is_read_only_and_fails_safe() {
        let descriptor =
            build_antminer_capability_descriptor(&empty_miner(), &crate::HardwareInfo::default());

        assert_eq!(descriptor.schema_version, CAPABILITY_SCHEMA_VERSION);
        assert_eq!(descriptor.family, DeviceFamily::Antminer);
        assert_eq!(descriptor.support, CapabilitySupportTier::Unknown);
        assert_eq!(descriptor.identity.confidence, IdentityConfidence::Unknown);
        assert_eq!(descriptor.identity.board_target, None);
        assert_eq!(descriptor.board.board_target, None);
        assert!(descriptor
            .runtime_caps
            .iter()
            .all(|cap| READ_ONLY_RUNTIME_CAPABILITIES.contains(cap)));
        assert_eq!(descriptor.safe_defaults.fan_pwm_cap, 30);
        assert!(!descriptor.safe_defaults.mining_enabled);
        assert!(descriptor.fail_safe.read_only);
        assert!(!descriptor.fail_safe.mutating_routes_allowed);
        assert!(!descriptor.power.writes_enabled);
        assert_eq!(
            descriptor.install.planner_outcome,
            PlannerOutcome::EvidenceGap
        );
    }

    #[test]
    fn hardware_identity_bm1362_chip_type_alone_does_not_promote_to_beta() {
        let descriptor = build_antminer_capability_descriptor(
            &empty_miner(),
            &unproven_hardware("BM1362", "am2"),
        );

        assert_eq!(descriptor.support, CapabilitySupportTier::Experimental);
        assert_eq!(descriptor.identity.confidence, IdentityConfidence::Low);
        assert!(descriptor.fail_safe.read_only);
        assert!(!descriptor
            .runtime_caps
            .contains(&RuntimeCapability::ConfigRw));
        assert_eq!(
            descriptor.install.planner_outcome,
            PlannerOutcome::EvidenceGap
        );
    }

    #[test]
    fn hardware_identity_bm1387_chip_id_alone_does_not_promote_unknown_composition() {
        let descriptor = build_antminer_capability_descriptor(
            &empty_miner(),
            &unproven_hardware("BM1387", "unknown-zynq"),
        );

        assert_eq!(descriptor.support, CapabilitySupportTier::Unknown);
        assert_eq!(descriptor.identity.confidence, IdentityConfidence::Low);
        assert!(descriptor.fail_safe.read_only);
        assert!(!descriptor
            .runtime_caps
            .contains(&RuntimeCapability::ConfigRw));
    }

    #[test]
    fn hardware_identity_bm1387_t9_declaration_is_not_the_s9_beta_anchor() {
        let descriptor = build_antminer_capability_descriptor(
            &empty_miner(),
            &declared_hardware("BM1387", "zynq-t9", "am1-t9"),
        );

        assert_eq!(descriptor.support, CapabilitySupportTier::Unknown);
        assert_eq!(descriptor.identity.confidence, IdentityConfidence::Low);
        assert!(descriptor.fail_safe.read_only);
    }

    #[test]
    fn hardware_identity_exact_s9_declaration_is_beta_but_read_only() {
        let descriptor = build_antminer_capability_descriptor(
            &empty_miner(),
            &declared_hardware("BM1387", "am1-s9", "am1-s9"),
        );

        assert_eq!(descriptor.support, CapabilitySupportTier::Beta);
        assert_eq!(descriptor.identity.confidence, IdentityConfidence::Low);
        assert!(descriptor.fail_safe.read_only);
        assert!(!descriptor
            .runtime_caps
            .contains(&RuntimeCapability::ConfigRw));
        assert!(!descriptor.power.writes_enabled);
    }

    #[test]
    fn hardware_identity_measured_s19jpro_is_authorized_beta_anchor() {
        let descriptor = build_antminer_capability_descriptor(
            &empty_miner(),
            &measured_hardware("BM1362", 0x1362, "am2-s19j", "am2-s19j"),
        );

        assert_eq!(descriptor.support, CapabilitySupportTier::Beta);
        assert_eq!(descriptor.identity.confidence, IdentityConfidence::High);
        assert!(!descriptor.fail_safe.read_only);
        assert!(descriptor
            .runtime_caps
            .contains(&RuntimeCapability::ConfigRw));
        assert!(descriptor
            .runtime_caps
            .contains(&RuntimeCapability::AsicOptions));
        assert!(descriptor
            .runtime_caps
            .contains(&RuntimeCapability::FlashOta));
        assert!(!descriptor
            .runtime_caps
            .contains(&RuntimeCapability::Restore));
        assert!(descriptor.runtime_caps.contains(&RuntimeCapability::Reboot));
        assert!(descriptor
            .runtime_caps
            .contains(&RuntimeCapability::PowerControl));
        assert!(descriptor.power.writes_enabled);
        assert_eq!(
            descriptor.install.planner_outcome,
            PlannerOutcome::RuntimeOnly
        );
        assert_eq!(
            descriptor.install.proof_scope,
            Some(ProofScope::ExactTargetLabOnly)
        );
    }

    #[test]
    fn hardware_identity_legacy_exact_without_typed_evidence_is_not_authority() {
        let legacy: crate::HardwareIdentification = serde_json::from_value(serde_json::json!({
            "confidence": "exact",
            "sources": ["board_target:am1-s9->BM1387"]
        }))
        .unwrap();
        let hardware = crate::HardwareInfo {
            chip_type: "BM1387".to_string(),
            control_board: "am1-s9".to_string(),
            identification: legacy,
            ..crate::HardwareInfo::default()
        };
        let descriptor = build_antminer_capability_descriptor(&empty_miner(), &hardware);

        assert_eq!(descriptor.support, CapabilitySupportTier::Unknown);
        assert_eq!(descriptor.identity.confidence, IdentityConfidence::Low);
        assert!(descriptor.fail_safe.read_only);
        assert!(!descriptor
            .runtime_caps
            .contains(&RuntimeCapability::ConfigRw));
        assert_eq!(descriptor.identity.board_target, None);
    }

    #[test]
    fn conflicting_typed_board_targets_fail_closed_without_coarse_fallback() {
        let hardware = crate::HardwareInfo {
            chip_type: "BM1362".to_string(),
            control_board: "Zynq am2".to_string(),
            identification: crate::HardwareIdentification::from_evidence(
                vec![
                    crate::HardwareIdentityEvidence::declared_asic_board_target(
                        "am2-s19j", "BM1362",
                    ),
                    crate::HardwareIdentityEvidence::declared_asic_board_target(
                        "cv1835-s19jpro",
                        "BM1362",
                    ),
                    crate::HardwareIdentityEvidence::measured_asic_enumeration(
                        0x1362,
                        "BM1362",
                        crate::HardwareCompositionToken::new(1, "test:conflict"),
                    ),
                ],
                Some("conflicting test declarations".to_string()),
            ),
            ..crate::HardwareInfo::default()
        };
        let descriptor = build_antminer_capability_descriptor(&empty_miner(), &hardware);

        assert_eq!(antminer_board_target(&hardware), "unknown");
        assert_eq!(descriptor.support, CapabilitySupportTier::Experimental);
        assert!(descriptor.fail_safe.read_only);
        assert_eq!(descriptor.identity.board_target, None);
        assert!(!descriptor
            .runtime_caps
            .contains(&RuntimeCapability::ConfigRw));
        assert!(!descriptor
            .runtime_caps
            .contains(&RuntimeCapability::FlashOta));
        assert!(!descriptor
            .runtime_caps
            .contains(&RuntimeCapability::Restore));
    }

    #[test]
    fn update_metadata_is_derived_from_exact_typed_enablement_policy() {
        let public = measured_hardware("BM1362", 0x1362, "am2-s19j", "am2-s19j");
        let public_metadata = update_metadata_payload(&empty_miner(), &public);
        assert_eq!(public_metadata.board_target, "am2-s19j");
        assert_eq!(public_metadata.package_type, "sysupgrade");
        assert!(public_metadata.inactive_slot_supported);
        assert_eq!(
            public_metadata.upload_endpoint.as_deref(),
            Some("/api/system/upgrade")
        );
        assert!(!public_metadata.toolbox.install_command.is_empty());

        let lab_zynq = declared_hardware("BM1397", "am2-s19pro", "am2-s19pro");
        let lab_metadata = update_metadata_payload(&empty_miner(), &lab_zynq);
        assert_eq!(lab_metadata.board_target, "am2-s19pro");
        assert_eq!(lab_metadata.package_type, "sysupgrade");
        assert!(lab_metadata.inactive_slot_supported);
        assert_eq!(lab_metadata.upload_endpoint, None);
        assert!(lab_metadata.toolbox.install_command.is_empty());

        let amlogic = declared_hardware("BM1368", "AML Amlogic", "am3-s21");
        let amlogic_metadata = update_metadata_payload(&empty_miner(), &amlogic);
        assert_eq!(amlogic_metadata.board_target, "am3-s21");
        assert_eq!(amlogic_metadata.package_type, "sysupgrade");
        assert!(!amlogic_metadata.inactive_slot_supported);
        assert_eq!(amlogic_metadata.upload_endpoint, None);

        let cv = declared_hardware("BM1362", "CVITEK CV1835", "cv1835-s19jpro");
        let cv_metadata = update_metadata_payload(&empty_miner(), &cv);
        assert_eq!(cv_metadata.board_target, "cv1835-s19jpro");
        assert_eq!(cv_metadata.package_type, "offline_analysis");
        assert!(!cv_metadata.inactive_slot_supported);
        assert_eq!(cv_metadata.upload_endpoint, None);
        assert!(cv_metadata.toolbox.install_command.is_empty());

        let unknown_metadata =
            update_metadata_payload(&empty_miner(), &crate::HardwareInfo::default());
        assert_eq!(unknown_metadata.board_target, "unknown");
        assert_eq!(unknown_metadata.package_type, "unsupported");
        assert!(!unknown_metadata.inactive_slot_supported);
        assert_eq!(unknown_metadata.upload_endpoint, None);
    }

    #[test]
    fn bm1366_bm1368_bm1370_baud_reports_antminer_runtime_value() {
        for chip in ["BM1366", "BM1368", "BM1370"] {
            let descriptor = build_antminer_capability_descriptor(
                &empty_miner(),
                &unproven_hardware(chip, "am3-aml"),
            );

            assert_eq!(
                descriptor.asic.baud,
                Some(3_125_000),
                "{chip} must not inherit the ESP/Bitaxe 1 Mbaud convention"
            );
            assert_ne!(descriptor.asic.baud, Some(1_000_000));
        }
    }

    #[test]
    fn runtime_guard_blocks_unknown_identity_with_typed_error() {
        let descriptor =
            build_antminer_capability_descriptor(&empty_miner(), &crate::HardwareInfo::default());
        let error = runtime_capability_guard_error(
            &descriptor,
            RuntimeCapability::Reboot,
            "/api/action/reboot",
        )
        .expect("unknown identity must not grant reboot");

        assert_eq!(error.kind, CapabilityErrorKind::UnknownHardware);
        assert_eq!(error.http_status, 409);
        assert_eq!(error.capability, None);
        assert!(error.message.contains("/api/action/reboot"));
        assert!(error.message.contains("reboot"));
    }

    #[test]
    fn runtime_guard_blocks_experimental_identity_as_conflict() {
        let descriptor = build_antminer_capability_descriptor(
            &empty_miner(),
            &unproven_hardware("BM1362", "am2"),
        );
        let error = runtime_capability_guard_error(
            &descriptor,
            RuntimeCapability::Reboot,
            "/api/action/reboot",
        )
        .expect("experimental identity must not grant reboot");

        assert_eq!(error.kind, CapabilityErrorKind::Conflict);
        assert_eq!(error.http_status, 409);
        assert_eq!(error.capability, Some(RuntimeCapability::Reboot));
    }

    #[test]
    fn runtime_guard_allows_exact_beta_reboot_and_power_control() {
        let descriptor = build_antminer_capability_descriptor(
            &empty_miner(),
            &measured_hardware("BM1362", 0x1362, "am2-s19j", "am2-s19j"),
        );

        assert_eq!(
            runtime_capability_guard_error(
                &descriptor,
                RuntimeCapability::Reboot,
                "/api/action/reboot"
            ),
            None
        );
        assert_eq!(
            runtime_capability_guard_error(
                &descriptor,
                RuntimeCapability::PowerControl,
                "/api/fan"
            ),
            None
        );
        assert_eq!(
            runtime_capability_guard_error(&descriptor, RuntimeCapability::PoolsRw, "/api/pools"),
            None
        );
        assert_eq!(
            runtime_capability_guard_error(&descriptor, RuntimeCapability::ConfigRw, "/api/config"),
            None
        );
        assert_eq!(
            runtime_capability_guard_error(
                &descriptor,
                RuntimeCapability::AsicOptions,
                "/api/debug/asic-command"
            ),
            None
        );
        assert_eq!(
            runtime_capability_guard_error(
                &descriptor,
                RuntimeCapability::FlashOta,
                "/api/system/upgrade"
            ),
            None
        );
    }

    /// CE-111: the 11 dedicated config / power-policy writers must fail closed
    /// on Unknown / Experimental identities (ConfigRw) and the six power-policy
    /// writers must additionally require PowerControl. The exact beta anchor
    /// grants both so no beta-operator flow regresses.
    #[test]
    fn ce111_dedicated_writers_require_runtime_capabilities() {
        let unknown =
            build_antminer_capability_descriptor(&empty_miner(), &crate::HardwareInfo::default());
        let experimental = build_antminer_capability_descriptor(
            &empty_miner(),
            &unproven_hardware("BM1362", "am2"),
        );
        let beta = build_antminer_capability_descriptor(
            &empty_miner(),
            &measured_hardware("BM1362", 0x1362, "am2-s19j", "am2-s19j"),
        );

        let config_rw_routes = [
            "/api/config/mqtt",
            "/api/config/webhook",
            "/api/config/psu-override",
            "/api/offgrid/config",
            "/api/solar/config",
            "/api/tou/schedule",
            "/api/home/target",
            "/api/home/room-temp",
            "/api/home/night-mode",
            "/api/led/pattern",
            "/api/led/config",
        ];
        for route in config_rw_routes {
            let unknown_err =
                runtime_capability_guard_error(&unknown, RuntimeCapability::ConfigRw, route)
                    .unwrap_or_else(|| panic!("{route}: unknown must not grant ConfigRw"));
            assert_eq!(unknown_err.http_status, 409, "{route}");
            assert_eq!(
                unknown_err.kind,
                CapabilityErrorKind::UnknownHardware,
                "{route}"
            );

            let exp_err =
                runtime_capability_guard_error(&experimental, RuntimeCapability::ConfigRw, route)
                    .unwrap_or_else(|| panic!("{route}: experimental must not grant ConfigRw"));
            assert_eq!(exp_err.http_status, 409, "{route}");
            assert_eq!(exp_err.kind, CapabilityErrorKind::Conflict, "{route}");

            assert_eq!(
                runtime_capability_guard_error(&beta, RuntimeCapability::ConfigRw, route),
                None,
                "{route}: exact beta anchor must grant ConfigRw"
            );
        }

        let power_routes = [
            "/api/config/psu-override",
            "/api/offgrid/config",
            "/api/solar/config",
            "/api/tou/schedule",
            "/api/home/target",
            "/api/home/night-mode",
        ];
        for route in power_routes {
            assert!(
                runtime_capability_guard_error(&unknown, RuntimeCapability::PowerControl, route)
                    .is_some(),
                "{route}: unknown must not grant PowerControl"
            );
            assert!(
                runtime_capability_guard_error(
                    &experimental,
                    RuntimeCapability::PowerControl,
                    route
                )
                .is_some(),
                "{route}: experimental must not grant PowerControl"
            );
            assert_eq!(
                runtime_capability_guard_error(&beta, RuntimeCapability::PowerControl, route),
                None,
                "{route}: exact beta anchor must grant PowerControl"
            );
        }
    }

    /// CE-174: the restore-to-stock POST routes must require
    /// `RuntimeCapability::Restore`. No registered BoardDesc currently has
    /// verified recovery, so even exact public-beta targets must fail closed.
    #[test]
    fn ce174_restore_routes_require_restore_capability() {
        let restore = RuntimeCapability::Restore;
        let preflight_route = "/api/system/restore-to-stock/preflight";
        let restore_route = "/api/system/restore-to-stock";

        // Exact public-beta identity is insufficient without verified recovery.
        let am2 = build_antminer_capability_descriptor(
            &empty_miner(),
            &measured_hardware("BM1362", 0x1362, "am2-s19j", "am2-s19j"),
        );
        let am1_s9 = build_antminer_capability_descriptor(
            &empty_miner(),
            &measured_hardware("BM1387", 0x1387, "am1-s9", "am1-s9"),
        );
        for descriptor in [&am2, &am1_s9] {
            assert!(runtime_capability_guard_error(descriptor, restore, restore_route).is_some());
            assert!(runtime_capability_guard_error(descriptor, restore, preflight_route).is_some());
        }

        // NEGATIVE: unknown identity → 409 UnknownHardware.
        let unknown =
            build_antminer_capability_descriptor(&empty_miner(), &crate::HardwareInfo::default());
        let unknown_err = runtime_capability_guard_error(&unknown, restore, restore_route)
            .expect("unknown identity must not grant Restore");
        assert_eq!(unknown_err.http_status, 409);
        assert_eq!(unknown_err.kind, CapabilityErrorKind::UnknownHardware);

        // NEGATIVE: experimental BM1362 (not the exact anchor) → 409 Conflict.
        let experimental = build_antminer_capability_descriptor(
            &empty_miner(),
            &unproven_hardware("BM1362", "am2"),
        );
        let exp_err = runtime_capability_guard_error(&experimental, restore, restore_route)
            .expect("experimental identity must not grant Restore");
        assert_eq!(exp_err.kind, CapabilityErrorKind::Conflict);
        assert_eq!(exp_err.capability, Some(RuntimeCapability::Restore));

        // NEGATIVE: Amlogic never grants Restore.
        let amlogic = build_antminer_capability_descriptor(
            &empty_miner(),
            &unproven_hardware("BM1368", "am3-aml"),
        );
        assert!(
            runtime_capability_guard_error(&amlogic, restore, restore_route).is_some(),
            "amlogic must never grant Restore"
        );
    }
}

#[cfg(test)]
mod group_b_monitoring_profiles_tests {
    use super::*;

    fn sample(timestamp_s: u64, hashrate_ghs: f64) -> SnapshotHistorySample {
        SnapshotHistorySample {
            timestamp_s,
            hashrate_ghs,
            power_watts: 0.0,
            power_source: "unavailable".to_string(),
            power_source_detail: "live_power_unavailable".to_string(),
            live_power_available: false,
            power_modeled: false,
            power_calibrated: false,
            power_calibration_multiplier: None,
            power_note: "Live power has not published positive board and wall watts for this history sample.".to_string(),
            temp_c: 0.0,
            fan_pwm: 0,
            fan_rpm: 0,
            accepted: 0,
            rejected: 0,
            pool_status: String::new(),
        }
    }

    // Minimal `MinerState` for the Prometheus encoder tests. `MinerState`
    // intentionally has no `Default`; this mirrors the struct-literal shape
    // the other rest.rs tests use, kept tiny so a field add is a one-line fix.
    fn minimal_miner_state() -> crate::MinerState {
        crate::MinerState {
            hashrate_ghs: 0.0,
            hashrate_5s_ghs: 0.0,
            accepted: 0,
            rejected: 0,
            chains: Vec::new(),
            fans: crate::FanState {
                pwm: 0,
                rpm: 0,
                per_fan: Vec::new(),
            },
            pool: crate::PoolState {
                url: String::new(),
                worker: String::new(),
                status: String::new(),
                difficulty: 0.0,
                last_share_at: 0,
                protocol: "sv1".to_string(),
                encrypted: false,
                encrypted_source: crate::pool_quality_honest_default_source(),
                sv2_session: None,
                sv2_session_source: crate::pool_quality_honest_default_source(),
                sv2_custom_job: None,
                donating: false,
                donating_source: crate::pool_quality_honest_default_source(),
                donation_active_url: String::new(),
                donation_active_worker: String::new(),
                donation_pool_index: 0,
                share_efficiency: None,
                auto_fallback_active: false,
                auto_fallback_source: crate::pool_quality_honest_default_source(),
                auto_retry_sv2_after_s: None,
                auto_fallback_reason: None,
                failover: dcentrald_stratum::types::PoolFailoverStatus::default(),
                failover_source: crate::pool_quality_honest_default_source(),
                hashrate_split: dcentrald_stratum::types::HashrateSplitStatus::default(),
                hashrate_split_source: crate::pool_quality_honest_default_source(),
                latency_ms: 0,
                latency_ms_source: crate::pool_quality_honest_default_source(),
                reject_reason_counts: [0; 6],
                reject_reason_counts_source: crate::pool_quality_honest_default_source(),
                rolling_acceptance_pct_30min: 0.0,
                rolling_acceptance_count_30min: (0, 0),
                rolling_acceptance_source: crate::pool_quality_honest_default_source(),
                worst_chip_hw_err_rate: None,
            },
            uptime_s: 0,
            firmware_version: "test".to_string(),
            mode: crate::OperatingMode::Standard,
        }
    }

    // ── Task 1: Prometheus 15m / 24h hashrate windows ──────────────────

    #[test]
    fn stats_chain_rows_mark_share_counts_not_tracked_per_chain() {
        let mut miner = minimal_miner_state();
        miner.accepted = 42;
        miner.rejected = 3;
        miner.chains.push(crate::ChainState {
            id: 6,
            chips: 63,
            frequency_mhz: 550,
            voltage_mv: 8_700,
            temp_c: 68.0,
            temp_source: Some(crate::ChainTempSource::BOARD_SENSOR.to_string()),
            hashrate_ghs: 4_500.0,
            errors: 7,
            status: "ok".to_string(),
        });

        let chain_rows = build_stats_chain_rows(&miner);
        let row = &chain_rows[0];
        assert_eq!(row["frequency_source"].as_str(), Some("chain_state"));
        assert_eq!(row["accepted"].as_u64(), Some(0));
        assert_eq!(row["rejected"].as_u64(), Some(0));
        assert_eq!(
            row["accepted_source"].as_str(),
            Some("not_tracked_per_chain")
        );
        assert_eq!(
            row["rejected_source"].as_str(),
            Some("not_tracked_per_chain")
        );
        assert_eq!(row["share_accounting"]["tracked"].as_bool(), Some(false));
        assert_eq!(
            row["share_accounting"]["scope"].as_str(),
            Some("miner_pool_session")
        );

        let response_meta = build_stats_share_accounting_meta();
        assert_eq!(response_meta["totals_tracked"].as_bool(), Some(true));
        assert_eq!(
            response_meta["totals_source"].as_str(),
            Some("miner_state.accepted_rejected")
        );
        assert_eq!(response_meta["per_chain_tracked"].as_bool(), Some(false));
        assert_eq!(
            response_meta["per_chain_source"].as_str(),
            Some("not_tracked_per_chain")
        );
    }

    #[test]
    fn power_projection_labels_static_fallback_as_modeled() {
        let mut miner = minimal_miner_state();
        miner.hashrate_ghs = 4_500.0;
        miner.chains.push(crate::ChainState {
            id: 6,
            chips: 63,
            frequency_mhz: 550,
            voltage_mv: 8_700,
            temp_c: 68.0,
            temp_source: Some(crate::ChainTempSource::BOARD_SENSOR.to_string()),
            hashrate_ghs: 4_500.0,
            errors: 0,
            status: "ok".to_string(),
        });
        let hardware = crate::HardwareInfo {
            chip_type: "BM1387".to_string(),
            ..crate::HardwareInfo::default()
        };

        let projected = project_power_telemetry(
            &dcentrald_autotuner::LivePowerEstimate::default(),
            &miner,
            &hardware,
        );

        assert_eq!(projected.source, "static_model_fallback");
        assert_eq!(
            projected.source_detail,
            "static_power_fallback_from_miner_state"
        );
        assert!(!projected.live_power_available);
        assert!(projected.modeled);
        assert!(!projected.calibrated);
        assert!(projected.board_watts > 0);
        assert!(projected.wall_watts > projected.board_watts);
    }

    #[test]
    fn power_projection_preserves_pmbus_as_measured() {
        let live = dcentrald_autotuner::LivePowerEstimate {
            board_watts: 1_000.0,
            wall_watts: 1_080.0,
            efficiency_jth: 27.0,
            btu_h: 3_684.96,
            calibrated: true,
            calibration_multiplier: Some(1.02),
            source: "pmbus".to_string(),
            ..dcentrald_autotuner::LivePowerEstimate::default()
        };

        let projected = project_power_telemetry(
            &live,
            &minimal_miner_state(),
            &crate::HardwareInfo::default(),
        );

        assert_eq!(projected.source, "pmbus");
        assert_eq!(projected.source_detail, "pmbus_measured");
        assert!(projected.live_power_available);
        assert!(!projected.modeled);
        assert!(projected.calibrated);
        assert_eq!(projected.calibration_multiplier, Some(1.02));
        assert_eq!(projected.wall_watts, 1_080);
    }

    #[test]
    fn power_projection_preserves_adc_as_measured() {
        let live = dcentrald_autotuner::LivePowerEstimate {
            board_watts: 900.0,
            wall_watts: 960.0,
            efficiency_jth: 32.0,
            btu_h: 3_275.52,
            source: "adc".to_string(),
            ..dcentrald_autotuner::LivePowerEstimate::default()
        };

        let projected = project_power_telemetry(
            &live,
            &minimal_miner_state(),
            &crate::HardwareInfo::default(),
        );

        assert_eq!(projected.source_detail, "adc_measured");
        assert!(projected.live_power_available);
        assert!(!projected.modeled);
    }

    #[test]
    fn power_projection_rejects_partial_live_sample() {
        let mut miner = minimal_miner_state();
        miner.hashrate_ghs = 4_500.0;
        miner.chains.push(crate::ChainState {
            id: 6,
            chips: 63,
            frequency_mhz: 550,
            voltage_mv: 8_700,
            temp_c: 68.0,
            temp_source: Some(crate::ChainTempSource::BOARD_SENSOR.to_string()),
            hashrate_ghs: 4_500.0,
            errors: 0,
            status: "ok".to_string(),
        });
        let live = dcentrald_autotuner::LivePowerEstimate {
            board_watts: 900.0,
            wall_watts: f64::NAN,
            source: "pmbus".to_string(),
            ..dcentrald_autotuner::LivePowerEstimate::default()
        };

        let projected = project_power_telemetry(
            &live,
            &miner,
            &crate::HardwareInfo {
                chip_type: "BM1387".to_string(),
                ..crate::HardwareInfo::default()
            },
        );

        assert_eq!(projected.source, "static_model_fallback");
        assert!(!projected.live_power_available);
        assert!(projected.modeled);
        assert!(projected.wall_watts > projected.board_watts);
    }

    #[test]
    fn prometheus_snapshot_suppresses_static_power_fallback_gauges() {
        let mut miner = minimal_miner_state();
        miner.hashrate_ghs = 4_500.0;
        miner.chains.push(crate::ChainState {
            id: 6,
            chips: 63,
            frequency_mhz: 550,
            voltage_mv: 8_700,
            temp_c: 68.0,
            temp_source: Some(crate::ChainTempSource::BOARD_SENSOR.to_string()),
            hashrate_ghs: 4_500.0,
            errors: 0,
            status: "ok".to_string(),
        });
        let hardware = crate::HardwareInfo {
            chip_type: "BM1387".to_string(),
            ..crate::HardwareInfo::default()
        };

        let snapshot = build_prometheus_snapshot(
            &miner,
            &dcentrald_autotuner::LivePowerEstimate::default(),
            &hardware,
            None,
            None,
            None,
            None,
            None,
            None,
        );
        let exposition = snapshot.to_exposition();

        assert!(!snapshot.power_live_available);
        assert!(snapshot.power_modeled);
        assert_eq!(snapshot.power_source, "static_model_fallback");
        assert!(exposition.contains(
            "dcentrald_power_live_available{source=\"static_model_fallback\",source_detail=\"static_power_fallback_from_miner_state\",modeled=\"true\"} 0"
        ));
        assert!(!exposition.contains("dcentrald_power_watts "));
        assert!(!exposition.contains("dcentrald_wall_watts "));
        assert!(!exposition.contains("dcentrald_efficiency_jth "));
        assert!(!exposition.contains("dcentrald_btu_h "));
    }

    #[test]
    fn prometheus_snapshot_surfaces_measured_power_gauges() {
        let live = dcentrald_autotuner::LivePowerEstimate {
            board_watts: 1_000.0,
            wall_watts: 1_080.0,
            efficiency_jth: 27.0,
            btu_h: 3_684.96,
            source: "pmbus".to_string(),
            ..dcentrald_autotuner::LivePowerEstimate::default()
        };

        let snapshot = build_prometheus_snapshot(
            &minimal_miner_state(),
            &live,
            &crate::HardwareInfo::default(),
            None,
            None,
            None,
            None,
            None,
            None,
        );
        let exposition = snapshot.to_exposition();

        assert!(snapshot.power_live_available);
        assert!(!snapshot.power_modeled);
        assert_eq!(snapshot.power_source_detail, "pmbus_measured");
        assert!(exposition.contains(
            "dcentrald_power_live_available{source=\"pmbus\",source_detail=\"pmbus_measured\",modeled=\"false\"} 1"
        ));
        assert!(exposition.contains("dcentrald_power_watts 1000\n"));
        assert!(exposition.contains("dcentrald_wall_watts 1080\n"));
        assert!(exposition.contains("dcentrald_efficiency_jth 27.0\n"));
        assert!(exposition.contains("dcentrald_btu_h 3685\n"));
    }

    #[test]
    fn status_power_section_labels_static_fallback_as_non_live() {
        let projected = PowerTelemetryProjection {
            board_watts: 1_000,
            wall_watts: 1_100,
            efficiency_jth: 80.0,
            btu_h: 3_753.0,
            source: "static_model_fallback".to_string(),
            source_detail: "static_power_fallback_from_miner_state",
            live_power_available: false,
            modeled: true,
            calibrated: false,
            calibration_multiplier: None,
            note: "Live power has not published a positive reading; values are modeled from miner state and chip-profile defaults.",
        };

        let section = build_status_power_section(
            &projected,
            &dcentrald_autotuner::LivePowerEstimate::default(),
            PowerTargetingState {
                active: false,
                source: None,
                mode: None,
                preset: None,
                schedule_label: None,
                target_watts: None,
                current_wall_watts: 0,
                current_wall_watts_measured: false,
                current_wall_watts_source_detail: None,
                delta_watts: None,
                comparison: None,
            },
        );

        assert_eq!(section["wall_watts"].as_u64(), Some(1_100));
        assert_eq!(section["source"].as_str(), Some("static_model_fallback"));
        assert_eq!(
            section["source_detail"].as_str(),
            Some("static_power_fallback_from_miner_state")
        );
        assert_eq!(section["live_power_available"].as_bool(), Some(false));
        assert_eq!(section["modeled"].as_bool(), Some(true));
        assert_eq!(section["targeting"]["active"].as_bool(), Some(false));
    }

    #[test]
    fn thermal_posture_power_section_hides_static_fallback_watts() {
        let projected = PowerTelemetryProjection {
            board_watts: 1_000,
            wall_watts: 1_100,
            efficiency_jth: 80.0,
            btu_h: 3_753.0,
            source: "static_model_fallback".to_string(),
            source_detail: "static_power_fallback_from_miner_state",
            live_power_available: false,
            modeled: true,
            calibrated: false,
            calibration_multiplier: None,
            note: "Live power has not published a positive reading; values are modeled from miner state and chip-profile defaults.",
        };

        let section = build_thermal_posture_power_section(
            &projected,
            &dcentrald_autotuner::LivePowerEstimate::default(),
            None,
        );

        assert_eq!(section["available"].as_bool(), Some(false));
        assert!(section["board_watts"].is_null());
        assert!(section["wall_watts"].is_null());
        assert!(section["efficiency_jth"].is_null());
        assert!(section["btu_h"].is_null());
        assert_eq!(section["source"].as_str(), Some("static_model_fallback"));
        assert_eq!(
            section["source_detail"].as_str(),
            Some("static_power_fallback_from_miner_state")
        );
        assert_eq!(section["live_power_available"].as_bool(), Some(false));
        assert_eq!(section["modeled"].as_bool(), Some(true));
    }

    #[test]
    fn thermal_posture_power_section_surfaces_live_measured_watts() {
        let live = dcentrald_autotuner::LivePowerEstimate {
            board_watts: 1_000.0,
            wall_watts: 1_080.0,
            efficiency_jth: 27.0,
            btu_h: 3_684.96,
            source: "pmbus".to_string(),
            ..dcentrald_autotuner::LivePowerEstimate::default()
        };
        let projected = project_power_telemetry(
            &live,
            &minimal_miner_state(),
            &crate::HardwareInfo::default(),
        );

        let section = build_thermal_posture_power_section(&projected, &live, Some(3));

        assert_eq!(section["available"].as_bool(), Some(true));
        assert_eq!(section["board_watts"].as_u64(), Some(1_000));
        assert_eq!(section["wall_watts"].as_u64(), Some(1_080));
        assert_eq!(section["source"].as_str(), Some("pmbus"));
        assert_eq!(section["source_detail"].as_str(), Some("pmbus_measured"));
        assert_eq!(section["live_power_available"].as_bool(), Some(true));
        assert_eq!(section["modeled"].as_bool(), Some(false));
        assert_eq!(section["age_s"].as_u64(), Some(3));
    }

    #[test]
    fn solar_mining_power_status_marks_missing_power_unavailable() {
        let status =
            crate::solar_mining_power_status(&dcentrald_autotuner::LivePowerEstimate::default());

        assert_eq!(status.watts, 0);
        assert_eq!(status.source, "unavailable");
        assert!(!status.live);
        assert!(!status.modeled);
        assert!(status.note.contains("has not published"));
    }

    #[test]
    fn solar_mining_power_status_labels_measured_and_modeled_sources() {
        let measured = crate::solar_mining_power_status(&dcentrald_autotuner::LivePowerEstimate {
            wall_watts: 1_234.4,
            source: "pmbus".to_string(),
            ..dcentrald_autotuner::LivePowerEstimate::default()
        });
        assert_eq!(measured.watts, 1_234);
        assert_eq!(measured.source, "pmbus");
        assert!(measured.live);
        assert!(!measured.modeled);

        let modeled = crate::solar_mining_power_status(&dcentrald_autotuner::LivePowerEstimate {
            wall_watts: 1_100.0,
            source: "estimated".to_string(),
            ..dcentrald_autotuner::LivePowerEstimate::default()
        });
        assert_eq!(modeled.watts, 1_100);
        assert_eq!(modeled.source, "estimated");
        assert!(modeled.live);
        assert!(modeled.modeled);
    }

    #[test]
    fn hashrate_avg_empty_ring_is_none() {
        // Cold ring buffer → both windows omit the family (no fabricated 0).
        assert_eq!(hashrate_avg_over_window(&[], 100_000, 15 * 60), None);
        assert_eq!(hashrate_avg_over_window(&[], 100_000, 24 * 60 * 60), None);
    }

    #[test]
    fn hashrate_avg_15m_window_excludes_older_samples() {
        let now = 100_000u64;
        let samples = [
            sample(now - 2_000, 50_000.0), // 33m ago — outside 15m
            sample(now - 800, 100_000.0),  // 13m ago — inside 15m
            sample(now - 60, 110_000.0),   // 1m ago  — inside 15m
        ];
        // 15m window averages only the two recent rows: (100k + 110k)/2.
        let avg15 = hashrate_avg_over_window(&samples, now, 15 * 60).unwrap();
        assert!((avg15 - 105_000.0).abs() < 1e-6, "got {avg15}");
        // 24h window averages all three: (50k + 100k + 110k)/3.
        let avg24 = hashrate_avg_over_window(&samples, now, 24 * 60 * 60).unwrap();
        assert!((avg24 - 86_666.666_666).abs() < 1e-3, "got {avg24}");
    }

    #[test]
    fn hashrate_avg_skips_non_finite_and_negative_rows() {
        let now = 100_000u64;
        let samples = [
            sample(now - 60, f64::NAN),
            sample(now - 50, -5.0),
            sample(now - 40, 90_000.0),
            sample(now - 30, 110_000.0),
        ];
        // Only the two valid rows count.
        let avg = hashrate_avg_over_window(&samples, now, 15 * 60).unwrap();
        assert!((avg - 100_000.0).abs() < 1e-6, "got {avg}");
    }

    #[test]
    fn hashrate_avg_all_invalid_rows_is_none() {
        let now = 100_000u64;
        let samples = [sample(now - 60, f64::NAN), sample(now - 30, -1.0)];
        assert_eq!(hashrate_avg_over_window(&samples, now, 15 * 60), None);
    }

    #[test]
    fn prometheus_snapshot_emits_15m_24h_families_when_present() {
        let miner = minimal_miner_state();
        let power = dcentrald_autotuner::LivePowerEstimate::default();
        let hardware = crate::HardwareInfo::default();
        let snap = build_prometheus_snapshot(
            &miner,
            &power,
            &hardware,
            Some(104_000.0),
            Some(103_500.0),
            None,
            None,
            None,
            None,
        );
        let body = snap.to_exposition();
        assert!(
            body.contains("dcentrald_hashrate_15m_ghs"),
            "15m family must appear when Some; body: {body}"
        );
        assert!(
            body.contains("dcentrald_hashrate_24h_ghs"),
            "24h family must appear when Some"
        );
    }

    #[test]
    fn prometheus_snapshot_omits_15m_24h_families_when_absent() {
        let miner = minimal_miner_state();
        let power = dcentrald_autotuner::LivePowerEstimate::default();
        let hardware = crate::HardwareInfo::default();
        let snap = build_prometheus_snapshot(
            &miner, &power, &hardware, None, None, None, None, None, None,
        );
        let body = snap.to_exposition();
        assert!(
            !body.contains("dcentrald_hashrate_15m_ghs"),
            "15m family must be omitted (not fabricated) when None; body: {body}"
        );
        assert!(
            !body.contains("dcentrald_hashrate_24h_ghs"),
            "24h family must be omitted when None"
        );
    }

    // ── P2-6 §4.C: fleet-grade {pool,worker} labels + integration health ──

    #[test]
    fn p2_6_snapshot_surfaces_pool_worker_donation_from_miner_state() {
        // The rest.rs wiring must lift pool URL + worker + donating from the
        // live MinerState into the per-pool labeled counters + donation gauge.
        let mut miner = minimal_miner_state();
        miner.accepted = 12;
        miner.rejected = 2;
        miner.pool.url = "stratum+tcp://pool.example.com:3333".to_string();
        miner.pool.worker = "bc1qworker.rig9".to_string();
        miner.pool.donating = true;
        let power = dcentrald_autotuner::LivePowerEstimate::default();
        let hardware = crate::HardwareInfo::default();
        let snap = build_prometheus_snapshot(
            &miner,
            &power,
            &hardware,
            None,
            None,
            None,
            None,
            Some(true),  // mqtt up
            Some(false), // webhook present-but-down
        );
        let body = snap.to_exposition();
        // OBS-2 (security): the worker is the operator's payout identity and is
        // masked via `mask_wallet` before it reaches the Prometheus label
        // (rest.rs build_prometheus_snapshot `worker_label`). Assert the MASKED
        // worker is present (computed via the same masker so the test is robust
        // to the mask format), and that the RAW worker never leaks. (This test
        // previously asserted the raw worker — the pre-masking, insecure
        // expectation — and was silently red because the dcentrald-api lib tests
        // do not run on the Windows dev host.)
        let masked_worker = dcentrald_common::wallet_mask::mask_wallet("bc1qworker.rig9");
        assert_ne!(
            masked_worker, "bc1qworker.rig9",
            "wallet-shaped worker must be masked"
        );
        assert!(body.contains(&format!(
            "dcentrald_pool_shares_accepted_total{{pool=\"stratum+tcp://pool.example.com:3333\",worker=\"{masked_worker}\"}} 12"
        )), "accepted counter must carry the masked worker; body: {body}");
        assert!(body.contains(&format!(
            "dcentrald_pool_shares_rejected_total{{pool=\"stratum+tcp://pool.example.com:3333\",worker=\"{masked_worker}\"}} 2"
        )), "rejected counter must carry the masked worker; body: {body}");
        assert!(
            !body.contains("bc1qworker.rig9"),
            "the raw (unmasked) worker must NEVER appear in the Prometheus exposition; body: {body}"
        );
        assert!(body.contains("dcentrald_donation_active 1"));
        assert!(body.contains("dcentrald_integration_up{kind=\"mqtt\"} 1"));
        assert!(body.contains("dcentrald_integration_up{kind=\"webhook\"} 0"));
    }

    #[test]
    fn p2_6_snapshot_omits_pool_labels_and_integration_when_unconfigured() {
        // Empty pool URL + both integrations None => no labeled families, no
        // integration family; the global unlabeled counters + donation 0 remain.
        let miner = minimal_miner_state(); // url/worker empty, donating false
        let power = dcentrald_autotuner::LivePowerEstimate::default();
        let hardware = crate::HardwareInfo::default();
        let snap = build_prometheus_snapshot(
            &miner, &power, &hardware, None, None, None, None, None, None,
        );
        let body = snap.to_exposition();
        assert!(!body.contains("dcentrald_pool_shares_accepted_total"));
        assert!(!body.contains("dcentrald_integration_up"));
        assert!(body.contains("dcentrald_shares_accepted_total 0"));
        assert!(body.contains("dcentrald_donation_active 0"));
    }

    #[test]
    fn p2_6_integration_up_from_config_table_tristate() {
        // Absent block => None (omitted). Present+enabled+target => Some(true).
        // Present+disabled or no-target => Some(false).
        let empty: toml::Table = toml::from_str("").unwrap();
        assert_eq!(integration_up_from_config_table(&empty), (None, None));

        let up: toml::Table = toml::from_str(
            "[mqtt]\nenabled = true\nbroker = \"mqtt://203.0.113.2:1883\"\n[webhook]\nenabled = true\nurl = \"https://hooks.example/x\"\n",
        )
        .unwrap();
        assert_eq!(
            integration_up_from_config_table(&up),
            (Some(true), Some(true))
        );

        // Present but disabled, and enabled-without-target => Some(false).
        let down: toml::Table = toml::from_str(
            "[mqtt]\nenabled = false\nbroker = \"mqtt://203.0.113.2:1883\"\n[webhook]\nenabled = true\nurl = \"\"\n",
        )
        .unwrap();
        assert_eq!(
            integration_up_from_config_table(&down),
            (Some(false), Some(false))
        );
    }

    // ── Task 2: read-only 21-step BM1362 silicon table ─────────────────

    #[test]
    fn silicon_table_response_exposes_21_step_bm1362_ladder() {
        let body = build_silicon_table_response();
        assert_eq!(body["chip_family"].as_str(), Some("BM1362"));
        assert_eq!(body["step_count"].as_u64(), Some(21));
        assert_eq!(body["min_step"].as_i64(), Some(-16));
        assert_eq!(body["max_step"].as_i64(), Some(4));
        assert_eq!(body["default_step"].as_i64(), Some(0));
        assert_eq!(body["sweet_spot_step"].as_i64(), Some(-9));

        let levels = body["levels"].as_array().expect("levels array");
        assert_eq!(levels.len(), 21);

        // Bottom of the ladder: step -16 @ 145 MHz / 11.880 V.
        assert_eq!(levels[0]["step"].as_i64(), Some(-16));
        assert_eq!(levels[0]["freq_mhz"].as_u64(), Some(145));
        // Nameplate `default` lands at step 0 / 545 MHz.
        let default_row = levels
            .iter()
            .find(|r| r["step"].as_i64() == Some(0))
            .expect("step 0 present");
        assert_eq!(default_row["name"].as_str(), Some("default"));
        assert_eq!(default_row["freq_mhz"].as_u64(), Some(545));
        // Top of the ladder: step +4 @ 645 MHz.
        assert_eq!(levels[20]["step"].as_i64(), Some(4));
        assert_eq!(levels[20]["freq_mhz"].as_u64(), Some(645));
    }

    #[test]
    fn silicon_table_rows_carry_provenance_and_efficiency() {
        let body = build_silicon_table_response();
        let levels = body["levels"].as_array().unwrap();
        // Every row exposes a provenance source string.
        for row in levels {
            let src = row["source"].as_str().expect("source string");
            assert!(
                matches!(src, "LiveConfirmed" | "Reconstructed" | "OperatorConfirmed"),
                "unexpected source provenance: {src}"
            );
        }
        // The sweet spot (step -9) should carry a computed J/TH efficiency
        // (it has live watts + hashrate).
        let sweet = levels
            .iter()
            .find(|r| r["step"].as_i64() == Some(-9))
            .unwrap();
        let jth = sweet["efficiency_jth"].as_f64().expect("sweet-spot J/TH");
        // 1714 W / 62.1 TH ≈ 27.6 J/TH.
        assert!((jth - 27.6).abs() < 0.2, "sweet-spot J/TH off: {jth}");
    }

    // ── Task 3: donation_active_worker masking ─────────────────────────

    #[test]
    fn donation_active_worker_wallet_is_masked() {
        // A wallet-shaped donation worker must be masked, not exposed raw.
        let raw = "bc1q04lzwddzgmtjex6jlsv2fwhe4se4jxje6rhzp6";
        let masked = dcentrald_common::wallet_mask::mask_wallet(raw);
        assert_ne!(masked, raw, "wallet-shaped worker must be masked");
        assert!(
            !masked.contains("lzwddzgmtjex"),
            "masked worker must not leak the middle of the address: {masked}"
        );
    }

    #[test]
    fn donation_active_worker_short_label_passes_through() {
        // `mask_wallet` only masks strings >= 12 bytes; a short worker label
        // stays readable so dashboards aren't garbled for non-wallet names.
        let label = "dm.work1"; // 8 bytes
        assert_eq!(
            dcentrald_common::wallet_mask::mask_wallet(label),
            label,
            "short (<12 byte) donation worker label must pass through unmasked"
        );
    }

    #[test]
    fn donation_active_worker_empty_stays_empty() {
        // Empty when not in a donation window — masking must keep it empty.
        assert_eq!(dcentrald_common::wallet_mask::mask_wallet(""), "");
    }

    // ── P1-3 (D-8): real 10-minute vs 1-hour hashrate windows ───────────

    #[test]
    fn hashrate_10m_and_1h_windows_are_distinct_real_averages() {
        // The D-8 bug had hashRate_10m == hashRate_1h (both a non-windowed
        // value). With a ring spanning > 10 min the two windows must average
        // DIFFERENT sample sets and therefore differ.
        let now = 100_000u64;
        let samples = [
            sample(now - 50 * 60, 60_000.0), // 50m ago — only in the 1h window
            sample(now - 40 * 60, 60_000.0), // 40m ago — only in the 1h window
            sample(now - 5 * 60, 120_000.0), // 5m ago  — in both windows
            sample(now - 60, 120_000.0),     // 1m ago  — in both windows
        ];
        let avg_10m = hashrate_avg_over_window(&samples, now, 10 * 60).unwrap();
        let avg_1h = hashrate_avg_over_window(&samples, now, 60 * 60).unwrap();
        assert!((avg_10m - 120_000.0).abs() < 1e-6, "10m={avg_10m}");
        assert!((avg_1h - 90_000.0).abs() < 1e-6, "1h={avg_1h}");
        assert_ne!(avg_10m, avg_1h, "distinct windows must differ");
    }

    #[test]
    fn hashrate_windows_fall_back_when_ring_empty() {
        // Early boot (no sample inside the window) → None, so the handler falls
        // back to the lifetime average rather than fabricating a windowed value.
        assert_eq!(hashrate_avg_over_window(&[], 100_000, 10 * 60), None);
        assert_eq!(hashrate_avg_over_window(&[], 100_000, 60 * 60), None);
    }

    // ── P1-3 (D-9): rated nameplate hashrate (nominal != measured) ──────

    #[test]
    fn rated_nominal_ths_is_rated_capacity_not_measured() {
        let profile = dcentrald_asic::drivers::MinerProfile::for_chip(0x1387)
            .expect("BM1387 profile present");
        // Enumerated silicon: 189 chips (3×63) at the rated default clock yields
        // the nameplate capacity, independent of whatever the unit is currently
        // measuring — so "% of rated" (effective / nominal) can finally render.
        let nominal = rated_nominal_ths(profile, 189);
        let expected = 189.0 * profile.chip_hashrate_ghs(profile.default_freq_mhz) / 1000.0;
        assert!((nominal - expected).abs() < 1e-9, "nominal {nominal}");
        assert!(nominal > 0.0);
    }

    #[test]
    fn rated_nominal_ths_falls_back_to_full_nameplate_before_enumeration() {
        let profile = dcentrald_asic::drivers::MinerProfile::for_chip(0x1387)
            .expect("BM1387 profile present");
        // No live chips yet → the profile's full nameplate (all chains).
        let nominal = rated_nominal_ths(profile, 0);
        let expected = profile.total_hashrate_ths(profile.default_freq_mhz);
        assert!((nominal - expected).abs() < 1e-9, "nominal {nominal}");
        assert!(nominal > 0.0);
    }
}

#[cfg(test)]
mod p2_5_target_temp_control_tests {
    use super::*;

    /// P2-5 regression: the swarm node must NOT advertise `target_temp_control`
    /// while no live room-temp PID + setpoint endpoint exist. The
    /// `HeaterController` PID in `dcentrald-thermal` is defined but never
    /// instantiated/run, and there is no REST surface to set a room-temp
    /// setpoint — so the capability is honestly false. If someone re-wires a
    /// live closed-loop controller they should update this test deliberately.
    #[test]
    fn swarm_capabilities_do_not_advertise_unwired_target_temp_control() {
        let caps = swarm_node_capabilities(true);
        assert!(
            !caps.target_temp_control,
            "target_temp_control must stay false until a live room-temp PID + \
             setpoint endpoint are wired"
        );
        // The genuinely-wired capabilities must still be advertised.
        assert!(caps.room_temp_input, "observed room-temp input IS wired");
        assert!(
            caps.target_watts_control,
            "home power-target control IS wired (POST /api/home/target)"
        );
        assert!(caps.can_coordinate);
        assert!(caps.mcp);
        // `identify` reflects the LED channel availability passed in.
        assert!(caps.identify);
        assert!(!swarm_node_capabilities(false).identify);
    }
}

#[cfg(test)]
mod setup_circuit_validation_tests {
    use super::*;

    // CE-360: the pure setup-circuit validator must fail closed on impossible /
    // unknown declarations BEFORE the derived cap is persisted and armed.
    #[test]
    fn setup_circuit_validation_fails_closed() {
        // Known presets pass.
        assert!(validate_setup_circuit("grid", Some(120), Some(15)).is_ok());
        assert!(validate_setup_circuit("grid", Some(240), Some(20)).is_ok());
        // Valid custom AC pair (208 V / 30 A industrial) passes.
        assert!(validate_setup_circuit("hybrid", Some(208), Some(30)).is_ok());
        // DC / source-only paths (no AC values) arm no cap and stay valid.
        assert!(validate_setup_circuit("solar_battery", None, None).is_ok());
        assert!(validate_setup_circuit("direct_dc", None, None).is_ok());
        // Empty-source "skip" path stays valid.
        assert!(validate_setup_circuit("", None, None).is_ok());
        assert!(validate_setup_circuit("grid", None, None).is_ok());

        // Impossible-high (would arm an effectively-infinite cap → no breaker
        // protection) is rejected.
        assert!(validate_setup_circuit("grid", Some(65535), Some(65535)).is_err());
        // Impossible-low / zero (would arm a throttle-to-zero cap) is rejected.
        assert!(validate_setup_circuit("grid", Some(0), Some(0)).is_err());
        // Unknown source is rejected.
        assert!(validate_setup_circuit("plutonium", Some(120), Some(15)).is_err());

        // In-range voltage/amperage but a derived power below the sane floor is
        // rejected (e.g. 90 V x 2 A = 180 W < 300 W).
        assert!(validate_setup_circuit("grid", Some(90), Some(2)).is_err());
        // Absurd amperage alone is rejected even at a normal voltage.
        assert!(validate_setup_circuit("grid", Some(120), Some(500)).is_err());
        // Half-specified AC pair is rejected (self-contained pairing guard).
        assert!(validate_setup_circuit("grid", Some(120), None).is_err());
        assert!(validate_setup_circuit("grid", None, Some(15)).is_err());
    }
}
