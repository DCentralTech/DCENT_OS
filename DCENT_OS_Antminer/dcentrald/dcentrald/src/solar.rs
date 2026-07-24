use std::collections::BTreeMap;

use anyhow::{anyhow, Result};
use dcentrald_api::{solar_provider_support, solar_transport};
use reqwest::header::{HeaderValue, COOKIE};

use crate::config::SolarConfig;

struct EcoFlowPayloadShape {
    label: &'static str,
    production_patterns: &'static [&'static str],
    consumption_patterns: &'static [&'static str],
    grid_patterns: &'static [&'static str],
    battery_soc_patterns: &'static [&'static str],
    age_patterns: &'static [&'static str],
}

const ECOFLOW_PAYLOAD_SHAPES: &[EcoFlowPayloadShape] = &[
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

fn unix_time_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[derive(Debug, Clone)]
pub struct SolarProviderSnapshot {
    pub production_watts: u32,
    pub consumption_watts: u32,
    pub net_grid_watts: i64,
    pub battery_soc_pct: Option<f32>,
    pub connected: bool,
    pub message: String,
    pub matched_fields: Vec<String>,
    pub transport: String,
    pub sample_age_ms: Option<u64>,
    pub stale: bool,
}

#[derive(Debug, Clone)]
pub struct SolarControlDecision {
    pub action: String,
    pub control_active: bool,
    pub sleep: bool,
    pub wake: bool,
    pub target_freq_mhz: Option<u16>,
    pub battery_floor_active: bool,
    pub message: String,
}

fn battery_backed_profile(source_profile: &str) -> bool {
    matches!(source_profile, "solar_battery" | "direct_dc")
}

fn require_freshness_proof(source_profile: &str) -> bool {
    battery_backed_profile(source_profile)
}

fn provider_sample_is_stale(config: &SolarConfig, sample_age_ms: Option<u64>) -> bool {
    let timeout_ms = config.provider_max_sample_age_ms;
    timeout_ms > 0 && sample_age_ms.map(|age| age > timeout_ms).unwrap_or(false)
}

fn normalize_metric_key(key: &str) -> String {
    key.to_ascii_lowercase()
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric() || *ch == '/')
        .collect()
}

fn extract_numeric_value(value: &serde_json::Value) -> Option<f64> {
    match value {
        serde_json::Value::Number(number) => number.as_f64(),
        serde_json::Value::Object(map) => map.get("value").and_then(extract_numeric_value),
        _ => None,
    }
}

fn extract_string_value(value: &serde_json::Value) -> Option<&str> {
    match value {
        serde_json::Value::String(text) => Some(text.as_str()),
        serde_json::Value::Object(map) => map.get("value").and_then(extract_string_value),
        _ => None,
    }
}

fn append_query_param(endpoint: &str, key: &str, value: &str) -> String {
    if value.trim().is_empty() || endpoint.contains(&format!("{}=", key)) {
        return endpoint.to_string();
    }

    let separator = if endpoint.contains('?') { '&' } else { '?' };
    format!("{}{}{}={}", endpoint, separator, key, value)
}

fn object_number_field(value: &serde_json::Value, key: &str) -> Option<f64> {
    value.get(key).and_then(extract_numeric_value)
}

fn object_string_field<'a>(value: &'a serde_json::Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(extract_string_value)
}

fn array_item_by_measurement<'a>(
    value: &'a serde_json::Value,
    field: &str,
    measurement_type: &str,
) -> Option<&'a serde_json::Value> {
    value.get(field)?.as_array()?.iter().find(|entry| {
        object_string_field(entry, "measurementType") == Some(measurement_type)
            && object_number_field(entry, "activeCount").unwrap_or(1.0) > 0.0
    })
}

fn collect_numeric_metrics(
    prefix: &str,
    value: &serde_json::Value,
    metrics: &mut BTreeMap<String, f64>,
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

fn lookup_metric(metrics: &BTreeMap<String, f64>, patterns: &[&str]) -> Option<(f64, String)> {
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

fn sum_metrics(metrics: &BTreeMap<String, f64>, patterns: &[&str]) -> Option<(f64, Vec<String>)> {
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

fn parse_mqtt_endpoint(endpoint: &str) -> Result<(String, u16)> {
    let trimmed = endpoint.trim().trim_start_matches("mqtt://");
    let host_port = trimmed.split('/').next().unwrap_or(trimmed);
    let mut parts = host_port.split(':');
    let host = parts.next().unwrap_or("").trim();
    if host.is_empty() {
        return Err(anyhow!("Victron MQTT endpoint must include a hostname"));
    }
    let port = parts
        .next()
        .map(|part| {
            part.parse::<u16>()
                .map_err(|_| anyhow!("Invalid MQTT port: {}", part))
        })
        .transpose()?
        .unwrap_or(1883);
    Ok((host.to_string(), port))
}

fn parse_mqtt_endpoint_with_topic(endpoint: &str) -> Result<(String, u16, String)> {
    let trimmed = endpoint.trim().trim_start_matches("mqtt://");
    let mut parts = trimmed.splitn(2, '/');
    let host_port = parts.next().unwrap_or("").trim();
    let topic = parts.next().unwrap_or("").trim().trim_matches('/');
    let (host, port) = parse_mqtt_endpoint(host_port)?;
    if topic.is_empty() {
        return Err(anyhow!(
            "Bridge MQTT endpoint must include a topic path, for example mqtt://broker:1883/dcentos/solar"
        ));
    }
    Ok((host, port, topic.to_string()))
}

fn apply_http_auth(
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

fn sample_age_from_metrics(metrics: &BTreeMap<String, f64>) -> Option<u64> {
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

fn sample_age_from_patterns(metrics: &BTreeMap<String, f64>, patterns: &[&str]) -> Option<u64> {
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

async fn fetch_http_json(
    endpoint: &str,
    api_key: &str,
    accept_invalid_certs: bool,
    provider_label: &str,
    prefer_cookie: bool,
) -> Result<serde_json::Value> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(4))
        .danger_accept_invalid_certs(accept_invalid_certs)
        .build()?;
    let request = apply_http_auth(client.get(endpoint), api_key, prefer_cookie);
    let response = request.send().await?;
    if !response.status().is_success() {
        if response.status().as_u16() == 401 || response.status().as_u16() == 403 {
            return Err(anyhow!(
                "{} HTTP endpoint rejected the request with status {}. Authentication is missing or invalid.",
                provider_label,
                response.status()
            ));
        }

        return Err(anyhow!(
            "{} HTTP endpoint returned status {}",
            provider_label,
            response.status()
        ));
    }

    Ok(response.json::<serde_json::Value>().await?)
}

fn normalize_bridge_metrics(
    metrics: &BTreeMap<String, f64>,
    mining_watts: u32,
    base_load_watts: u32,
    transport: &str,
) -> Result<SolarProviderSnapshot> {
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
        return Err(anyhow!(
            "Bridge provider did not expose a normalized productionWatts field"
        ));
    };

    let consumption_value = if let Some((value, _)) = consumption {
        value
    } else if let Some((grid_value, _)) = grid {
        production_value + grid_value
    } else {
        return Err(anyhow!(
            "Bridge provider must expose consumptionWatts or netGridWatts alongside productionWatts"
        ));
    };

    let net_grid_value = if let Some((value, _)) = grid {
        value
    } else {
        consumption_value - production_value
    };

    let production_watts = production_value.max(0.0).round() as u32;
    let consumption_watts = consumption_value
        .max(base_load_watts.saturating_add(mining_watts) as f64)
        .round() as u32;
    let net_grid_watts = net_grid_value.round() as i64;

    Ok(SolarProviderSnapshot {
        production_watts,
        consumption_watts,
        net_grid_watts,
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
        stale: false,
    })
}

fn read_tesla_metric(value: &serde_json::Value, path: &[&str]) -> Option<f64> {
    let mut current = value;
    for key in path {
        current = current.get(*key)?;
    }
    current.as_f64()
}

fn normalize_tesla_snapshot(
    aggregates: &serde_json::Value,
    soe: Option<&serde_json::Value>,
) -> Result<SolarProviderSnapshot> {
    let production = read_tesla_metric(aggregates, &["solar", "instant_power"]);
    let consumption = read_tesla_metric(aggregates, &["load", "instant_power"]);
    let net_grid = read_tesla_metric(aggregates, &["site", "instant_power"]);
    let battery_soc = soe
        .and_then(|value| value.get("percentage"))
        .and_then(|value| value.as_f64())
        .map(|value| value as f32);

    let (Some(production), Some(consumption), Some(net_grid)) = (production, consumption, net_grid)
    else {
        return Err(anyhow!(
            "Tesla local API response is missing one or more required fields: solar.instant_power, load.instant_power, site.instant_power"
        ));
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

fn kw_to_watts(value: f64, unit: &str) -> f64 {
    if unit.eq_ignore_ascii_case("kw") {
        value * 1000.0
    } else {
        value
    }
}

fn normalize_enphase_snapshot(
    production_details: &serde_json::Value,
    secctrl: Option<&serde_json::Value>,
) -> Result<SolarProviderSnapshot> {
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
        return Err(anyhow!(
            "Enphase local response is missing solar production (production[].wNow or inverters[].wNow)"
        ));
    };
    let consumption = if let Some(value) = consumption {
        value
    } else if let Some(value) = net_grid {
        production + value
    } else {
        return Err(anyhow!(
            "Enphase local response is missing total-consumption and net-consumption telemetry"
        ));
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
            "Enphase local provider connected via Envoy/IQ Gateway.".to_string()
        } else {
            "Enphase local provider connected, but battery SoC was unavailable from the local gateway.".to_string()
        },
        matched_fields,
        transport: "http-json".to_string(),
        sample_age_ms: Some(0),
        stale: false,
    })
}

fn normalize_solaredge_snapshot(
    current_power_flow: &serde_json::Value,
    overview: Option<&serde_json::Value>,
) -> Result<SolarProviderSnapshot> {
    let flow = current_power_flow
        .get("siteCurrentPowerFlow")
        .ok_or_else(|| anyhow!("SolarEdge response is missing siteCurrentPowerFlow"))?;
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
        return Err(anyhow!(
            "SolarEdge response is missing PV currentPower and overview fallback"
        ));
    };
    let Some(consumption) = consumption else {
        return Err(anyhow!("SolarEdge response is missing LOAD currentPower"));
    };

    let importing = flow
        .get("connections")
        .and_then(|value| value.as_array())
        .map(|connections| {
            connections.iter().any(|entry| {
                object_string_field(entry, "from") == Some("GRID")
                    || object_string_field(entry, "to") == Some("LOAD")
                        && object_string_field(entry, "from") == Some("GRID")
            })
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

    let production_watts = kw_to_watts(production, unit).max(0.0).round() as u32;
    let consumption_watts = kw_to_watts(consumption, unit).max(0.0).round() as u32;
    let net_grid_watts = kw_to_watts(signed_grid, unit).round() as i64;

    let mut matched_fields = vec![
        "siteCurrentPowerFlow.PV.currentPower".to_string(),
        "siteCurrentPowerFlow.LOAD.currentPower".to_string(),
        "siteCurrentPowerFlow.GRID.currentPower".to_string(),
    ];
    if battery_soc.is_some() {
        matched_fields.push("siteCurrentPowerFlow.STORAGE.chargeLevel".to_string());
    }

    Ok(SolarProviderSnapshot {
        production_watts,
        consumption_watts,
        net_grid_watts,
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

fn normalize_victron_metrics(
    metrics: &BTreeMap<String, f64>,
    mining_watts: u32,
    base_load_watts: u32,
    transport: &str,
) -> Result<SolarProviderSnapshot> {
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
        return Err(anyhow!(
            "Victron provider connected but no recognizable production/load/battery metrics were found"
        ));
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
        sample_age_ms: Some(0),
        stale: false,
    })
}

async fn fetch_victron_http_metrics(
    endpoint: &str,
    api_key: &str,
) -> Result<BTreeMap<String, f64>> {
    let json = fetch_http_json(endpoint, api_key, false, "Victron", false).await?;
    let mut metrics = BTreeMap::new();
    collect_numeric_metrics("", &json, &mut metrics);
    Ok(metrics)
}

async fn fetch_bridge_http_metrics(endpoint: &str, api_key: &str) -> Result<BTreeMap<String, f64>> {
    let json = fetch_http_json(endpoint, api_key, false, "Bridge", false).await?;
    let mut metrics = BTreeMap::new();
    collect_numeric_metrics("", &json, &mut metrics);
    Ok(metrics)
}

async fn fetch_ecoflow_http_metrics(
    endpoint: &str,
    api_key: &str,
) -> Result<BTreeMap<String, f64>> {
    if !(endpoint.starts_with("http://") || endpoint.starts_with("https://")) {
        return Err(anyhow!(
            "EcoFlow provider requires an http:// or https:// normalized HTTP endpoint"
        ));
    }

    let json = fetch_http_json(endpoint, api_key, false, "EcoFlow HTTP", false).await?;
    let mut metrics = BTreeMap::new();
    collect_numeric_metrics("", &json, &mut metrics);
    Ok(metrics)
}

async fn fetch_ecoflow_mqtt_metrics(
    endpoint: &str,
    api_key: &str,
) -> Result<BTreeMap<String, f64>> {
    let metrics = fetch_bridge_mqtt_metrics(endpoint, api_key).await?;
    if metrics.is_empty() {
        Err(anyhow!(
            "No EcoFlow MQTT bridge metrics were received on the configured topic"
        ))
    } else {
        Ok(metrics)
    }
}

fn normalize_ecoflow_shape(
    metrics: &BTreeMap<String, f64>,
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
        stale: false,
    })
}

fn normalize_ecoflow_metrics(
    metrics: &BTreeMap<String, f64>,
    mining_watts: u32,
    base_load_watts: u32,
) -> Result<SolarProviderSnapshot> {
    for shape in ECOFLOW_PAYLOAD_SHAPES {
        if let Some(snapshot) =
            normalize_ecoflow_shape(metrics, mining_watts, base_load_watts, shape)
        {
            return Ok(snapshot);
        }
    }

    Err(anyhow!(
        "EcoFlow provider did not match any supported normalized HTTP payload shape. Expected one of: bridge-contract (productionWatts + consumptionWatts|netGridWatts), site-summary (pvWatts|solarWatts + homeLoadWatts|loadPowerWatts|consumptionWatts), or power-summary (solarInputWatts|inputPowerWatts + outputWatts|homeLoadWatts|consumptionWatts)."
    ))
}

async fn fetch_victron_mqtt_metrics(
    endpoint: &str,
    api_key: &str,
) -> Result<BTreeMap<String, f64>> {
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
        .await?;

    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
    let mut metrics = BTreeMap::new();

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
            Ok(Err(e)) => return Err(anyhow!("Victron MQTT polling failed: {}", e)),
            Err(_) => break,
        }
    }

    if metrics.is_empty() {
        Err(anyhow!("No Victron MQTT metrics were received"))
    } else {
        Ok(metrics)
    }
}

async fn fetch_bridge_mqtt_metrics(endpoint: &str, api_key: &str) -> Result<BTreeMap<String, f64>> {
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
        .await?;

    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
    let mut metrics = BTreeMap::new();

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
                        anyhow!(
                            "Bridge MQTT payload on '{}' was not valid JSON: {}",
                            topic,
                            e
                        )
                    })?;
                collect_numeric_metrics("", &json, &mut metrics);
                break;
            }
            Ok(Ok(_)) => {}
            Ok(Err(e)) => return Err(anyhow!("Bridge MQTT polling failed: {}", e)),
            Err(_) => break,
        }
    }

    if metrics.is_empty() {
        Err(anyhow!(
            "No bridge MQTT metrics were received on topic '{}'",
            topic
        ))
    } else {
        Ok(metrics)
    }
}

async fn fetch_enphase_snapshot(endpoint: &str, api_key: &str) -> Result<SolarProviderSnapshot> {
    let base = endpoint.trim().trim_end_matches('/');
    let production_url = if base.contains("production.json") {
        if base.contains("details=") {
            base.to_string()
        } else {
            format!("{}?details=1", base)
        }
    } else {
        format!("{}/production.json?details=1", base)
    };
    let secctrl_url = if base.contains("production.json") {
        format!(
            "{}/ivp/ensemble/secctrl",
            base.split("/production.json").next().unwrap_or(base)
        )
    } else {
        format!("{}/ivp/ensemble/secctrl", base)
    };

    let production =
        fetch_http_json(&production_url, api_key, true, "Enphase local", false).await?;
    let secctrl = match fetch_http_json(&secctrl_url, api_key, true, "Enphase local", false).await {
        Ok(value) => Some(value),
        Err(error) => {
            let message = error.to_string();
            if message.contains("Authentication is missing or invalid")
                || message.contains("returned status 404")
            {
                None
            } else {
                return Err(anyhow!(message));
            }
        }
    };

    normalize_enphase_snapshot(&production, secctrl.as_ref())
}

async fn fetch_solaredge_snapshot(endpoint: &str, api_key: &str) -> Result<SolarProviderSnapshot> {
    let current_power_flow_url = if endpoint.contains("currentPowerFlow") {
        endpoint.to_string()
    } else {
        format!("{}/currentPowerFlow", endpoint.trim_end_matches('/'))
    };
    let overview_url = current_power_flow_url.replace("currentPowerFlow", "overview");
    let current_power_flow_url = append_query_param(&current_power_flow_url, "api_key", api_key);
    let overview_url = append_query_param(&overview_url, "api_key", api_key);

    let current_power_flow =
        fetch_http_json(&current_power_flow_url, "", false, "SolarEdge cloud", false).await?;
    let overview = fetch_http_json(&overview_url, "", false, "SolarEdge cloud", false)
        .await
        .ok();

    normalize_solaredge_snapshot(&current_power_flow, overview.as_ref())
}

async fn fetch_tesla_local_snapshot(
    endpoint: &str,
    api_key: &str,
) -> Result<SolarProviderSnapshot> {
    let base = endpoint.trim().trim_end_matches('/');
    if !(base.starts_with("http://") || base.starts_with("https://")) {
        return Err(anyhow!(
            "Tesla local backend requires an http:// or https:// gateway endpoint"
        ));
    }

    let aggregates_url = format!("{}/api/meters/aggregates", base);
    let soe_url = format!("{}/api/system_status/soe", base);
    let aggregates = fetch_http_json(&aggregates_url, api_key, true, "Tesla local", true).await?;
    let soe = match fetch_http_json(&soe_url, api_key, true, "Tesla local", true).await {
        Ok(value) => Some(value),
        Err(error) => {
            let message = error.to_string();
            if message.contains("Authentication is missing or invalid") {
                None
            } else {
                return Err(anyhow!(message));
            }
        }
    };

    normalize_tesla_snapshot(&aggregates, soe.as_ref())
}

pub async fn fetch_snapshot(
    config: &SolarConfig,
    mining_watts: u32,
) -> Result<SolarProviderSnapshot> {
    let provider = config.inverter_brand.trim();
    let support = solar_provider_support(provider);
    if provider == "manual" {
        let consumption_watts = config.manual_site_load_watts.saturating_add(mining_watts);
        return Ok(SolarProviderSnapshot {
            production_watts: config.manual_production_watts,
            consumption_watts,
            net_grid_watts: config.manual_site_load_watts as i64 + mining_watts as i64
                - config.manual_production_watts as i64,
            battery_soc_pct: config.manual_battery_soc_pct,
            connected: true,
            message: "Manual solar provider active.".to_string(),
            matched_fields: vec![
                "manual_production_watts".to_string(),
                "manual_site_load_watts".to_string(),
            ],
            transport: "manual".to_string(),
            sample_age_ms: Some(0),
            stale: false,
        });
    }

    let endpoint = config.api_endpoint.trim();
    if provider != "manual" && endpoint.is_empty() {
        return Err(anyhow!("{} endpoint is empty", provider));
    }

    if !support.live_backend {
        let reason = support
            .stage_reason
            .unwrap_or_else(|| format!("{} provider backend is not implemented yet", provider));
        return Err(anyhow!(reason));
    }

    match provider {
        "victron" => {
            let metrics = if endpoint.starts_with("http://") || endpoint.starts_with("https://") {
                fetch_victron_http_metrics(endpoint, &config.api_key).await?
            } else {
                fetch_victron_mqtt_metrics(endpoint, &config.api_key).await?
            };

            normalize_victron_metrics(
                &metrics,
                mining_watts,
                config.base_load_watts,
                &solar_transport(provider, endpoint),
            )
        }
        "bridge" => {
            let metrics = if endpoint.starts_with("http://") || endpoint.starts_with("https://") {
                fetch_bridge_http_metrics(endpoint, &config.api_key).await?
            } else {
                fetch_bridge_mqtt_metrics(endpoint, &config.api_key).await?
            };

            normalize_bridge_metrics(
                &metrics,
                mining_watts,
                config.base_load_watts,
                &solar_transport(provider, endpoint),
            )
        }
        "ecoflow" => {
            let metrics = if endpoint.starts_with("http://") || endpoint.starts_with("https://") {
                fetch_ecoflow_http_metrics(endpoint, &config.api_key).await?
            } else {
                fetch_ecoflow_mqtt_metrics(endpoint, &config.api_key).await?
            };
            let mut snapshot =
                normalize_ecoflow_metrics(&metrics, mining_watts, config.base_load_watts)?;
            snapshot.transport =
                if endpoint.starts_with("http://") || endpoint.starts_with("https://") {
                    "ecoflow-http-bridge".to_string()
                } else {
                    "ecoflow-mqtt-bridge".to_string()
                };
            Ok(snapshot)
        }
        "enphase" => fetch_enphase_snapshot(endpoint, &config.api_key).await,
        "solaredge" => fetch_solaredge_snapshot(endpoint, &config.api_key).await,
        "tesla" => fetch_tesla_local_snapshot(endpoint, &config.api_key).await,
        _ => Err(anyhow!("{} provider backend not implemented yet", provider)),
    }
    .map(|mut snapshot| {
        snapshot.stale = provider_sample_is_stale(config, snapshot.sample_age_ms);
        snapshot
    })
}

pub fn decide_policy(
    source_profile: &str,
    config: &SolarConfig,
    snapshot: &SolarProviderSnapshot,
    mining_watts: u32,
    max_freq_mhz: u16,
    min_freq_mhz: u16,
    reference_watts: u32,
    solar_forced_sleep: bool,
) -> SolarControlDecision {
    let battery_backed = battery_backed_profile(source_profile);
    let freshness_proof_missing =
        require_freshness_proof(source_profile) && snapshot.sample_age_ms.is_none();

    if freshness_proof_missing {
        return SolarControlDecision {
            action: "sleep".to_string(),
            control_active: true,
            sleep: true,
            wake: false,
            target_freq_mhz: None,
            battery_floor_active: battery_backed,
            message: "Battery-backed direct-DC solar policy requires a provider freshness proof, but the snapshot did not include sample age or timestamp metadata.".to_string(),
        };
    }

    if snapshot.stale && (config.solar_only_mode || battery_backed) {
        return SolarControlDecision {
            action: "sleep".to_string(),
            control_active: true,
            sleep: true,
            wake: false,
            target_freq_mhz: None,
            battery_floor_active: false,
            message: format!(
                "Provider telemetry is stale (age {:?} ms), so DCENT_OS will not mine blind in this power mode.",
                snapshot.sample_age_ms
            ),
        };
    }

    let base_load_estimate = snapshot.consumption_watts as i64 - mining_watts as i64;
    let available_mining_watts = snapshot.production_watts as i64 - base_load_estimate;
    if battery_backed && snapshot.battery_soc_pct.is_none() {
        return SolarControlDecision {
            action: "sleep".to_string(),
            control_active: true,
            sleep: true,
            wake: false,
            target_freq_mhz: None,
            battery_floor_active: true,
            message: "Battery-backed solar policy requires a live battery SoC reading, but the provider snapshot did not include one.".to_string(),
        };
    }
    let battery_floor_active = snapshot
        .battery_soc_pct
        .map(|soc| soc < config.battery_threshold_pct as f32)
        .unwrap_or(false);
    let wake_soc = config
        .battery_threshold_pct
        .saturating_add(config.battery_wake_hysteresis_pct) as f32;
    let battery_ready = snapshot
        .battery_soc_pct
        .map(|soc| soc >= wake_soc)
        .unwrap_or(true);
    let min_operating_watts = ((reference_watts as f64)
        * (min_freq_mhz as f64 / max_freq_mhz.max(1) as f64))
        .round() as i64;

    if battery_backed && battery_floor_active {
        return SolarControlDecision {
            action: "sleep".to_string(),
            control_active: true,
            sleep: true,
            wake: false,
            target_freq_mhz: None,
            battery_floor_active: true,
            message: format!(
                "Battery SoC {:.1}% is below the configured floor {}%. Solar policy requests sleep.",
                snapshot.battery_soc_pct.unwrap_or_default(),
                config.battery_threshold_pct
            ),
        };
    }

    if solar_forced_sleep && !battery_ready {
        return SolarControlDecision {
            action: "sleep".to_string(),
            control_active: true,
            sleep: true,
            wake: false,
            target_freq_mhz: None,
            battery_floor_active,
            message: format!(
                "Battery SoC {:.1}% has not recovered above the wake floor {:.1}% yet.",
                snapshot.battery_soc_pct.unwrap_or_default(),
                wake_soc
            ),
        };
    }

    if config.solar_only_mode {
        if available_mining_watts < min_operating_watts {
            return SolarControlDecision {
                action: "sleep".to_string(),
                control_active: true,
                sleep: true,
                wake: false,
                target_freq_mhz: None,
                battery_floor_active,
                message: format!(
                    "Solar-only mode: available mining power {}W is below the minimum operating budget {}W.",
                    available_mining_watts.max(0),
                    min_operating_watts
                ),
            };
        }

        let ratio = (available_mining_watts as f64 / reference_watts.max(1) as f64).clamp(0.0, 1.0);
        let target = ((max_freq_mhz as f64) * ratio).round() as u16;
        // Clamp the floor down to the ceiling: u16::clamp panics if min > max
        // (a valid eco config can set the mining ceiling below the off-grid floor,
        // e.g. frequency_mhz=50 vs the default 200 MHz floor), which would
        // abort the daemon and leave persistent session admission refused. The operator's ceiling wins.
        let target = target.clamp(min_freq_mhz.min(max_freq_mhz), max_freq_mhz);

        return SolarControlDecision {
            action: if solar_forced_sleep {
                "wake".to_string()
            } else {
                "cap".to_string()
            },
            control_active: true,
            sleep: false,
            wake: solar_forced_sleep,
            target_freq_mhz: Some(target),
            battery_floor_active,
            message: format!(
                "Solar-only mode: {}W available for mining, capping to {} MHz.",
                available_mining_watts.max(0),
                target
            ),
        };
    }

    if source_profile == "hybrid" {
        let desired_miner_watts = (mining_watts as i64 - snapshot.net_grid_watts).max(0);
        let bounded_target_watts = desired_miner_watts
            .max(min_operating_watts)
            .min(reference_watts.max(1) as i64);
        let ratio = (bounded_target_watts as f64 / reference_watts.max(1) as f64).clamp(0.0, 1.0);
        let target = ((max_freq_mhz as f64) * ratio).round() as u16;
        // Clamp the floor down to the ceiling: u16::clamp panics if min > max
        // (a valid eco config can set the mining ceiling below the off-grid floor,
        // e.g. frequency_mhz=50 vs the default 200 MHz floor), which would
        // abort the daemon and leave persistent session admission refused. The operator's ceiling wins.
        let target = target.clamp(min_freq_mhz.min(max_freq_mhz), max_freq_mhz);
        let deadband_watts = config.hybrid_import_deadband_watts as i64;
        let action = if snapshot.net_grid_watts > deadband_watts {
            "import_reduce"
        } else if snapshot.net_grid_watts < -deadband_watts {
            "surplus_ramp"
        } else {
            "hold"
        };

        return SolarControlDecision {
            action: action.to_string(),
            control_active: true,
            sleep: false,
            wake: false,
            target_freq_mhz: Some(target),
            battery_floor_active,
            message: if snapshot.net_grid_watts > deadband_watts {
                format!(
                    "Hybrid mode: importing {}W from grid, capping miner toward {}W (~{} MHz).",
                    snapshot.net_grid_watts, bounded_target_watts, target
                )
            } else if snapshot.net_grid_watts < -deadband_watts {
                format!(
                    "Hybrid mode: exporting {}W of surplus, allowing miner up to {} MHz.",
                    snapshot.net_grid_watts.abs(),
                    target
                )
            } else {
                format!(
                    "Hybrid mode: grid exchange within {}W deadband, holding around {} MHz.",
                    deadband_watts, target
                )
            },
        };
    }

    SolarControlDecision {
        action: "observe".to_string(),
        control_active: false,
        sleep: false,
        wake: false,
        target_freq_mhz: None,
        battery_floor_active,
        message: format!(
            "Solar provider connected in observe mode. Available mining headroom is {}W.",
            available_mining_watts.max(0)
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::{decide_policy, SolarConfig, SolarProviderSnapshot};

    fn base_config() -> SolarConfig {
        SolarConfig {
            enabled: true,
            inverter_brand: "manual".to_string(),
            api_endpoint: String::new(),
            api_key: String::new(),
            solar_only_mode: false,
            base_load_watts: 500,
            battery_threshold_pct: 20,
            battery_wake_hysteresis_pct: 3,
            provider_max_sample_age_ms: 60_000,
            provider_failure_hysteresis_samples: 2,
            hybrid_import_deadband_watts: 75,
            manual_production_watts: 0,
            manual_site_load_watts: 0,
            manual_battery_soc_pct: None,
        }
    }

    fn base_snapshot() -> SolarProviderSnapshot {
        SolarProviderSnapshot {
            production_watts: 2500,
            consumption_watts: 1800,
            net_grid_watts: 0,
            battery_soc_pct: Some(55.0),
            connected: true,
            message: String::new(),
            matched_fields: Vec::new(),
            transport: "manual".to_string(),
            sample_age_ms: Some(5_000),
            stale: false,
        }
    }

    #[test]
    fn battery_backed_policy_requires_freshness_proof() {
        let config = base_config();
        let mut snapshot = base_snapshot();
        snapshot.sample_age_ms = None;

        let decision = decide_policy("direct_dc", &config, &snapshot, 1200, 700, 200, 1400, false);

        assert!(decision.sleep);
        assert!(decision.control_active);
        assert!(decision.message.contains("freshness proof"));
    }

    #[test]
    fn hybrid_policy_does_not_fail_closed_without_freshness_proof() {
        let config = base_config();
        let mut snapshot = base_snapshot();
        snapshot.sample_age_ms = None;

        let decision = decide_policy("hybrid", &config, &snapshot, 1200, 700, 200, 1400, false);

        assert!(!decision.sleep);
    }

    #[test]
    fn hybrid_policy_survives_inverted_freq_bounds() {
        // Regression: an eco config with mining.frequency_mhz below the off-grid
        // min (50 MHz ceiling vs the default 200 MHz floor) inverts (min,max).
        // u16::clamp would panic on min>max and, under panic=abort, terminate the
        // hardware owner and leave persistent re-admission refused. decide_policy
        // must clamp the floor down to the ceiling instead.
        let config = base_config();
        let snapshot = base_snapshot();

        let decision = decide_policy("hybrid", &config, &snapshot, 1200, 50, 200, 1400, false);

        let target = decision.target_freq_mhz.expect("hybrid returns a target");
        assert!(
            target <= 50,
            "target {} must not exceed the mining ceiling of 50 MHz",
            target
        );
    }

    #[test]
    fn solar_only_policy_survives_inverted_freq_bounds() {
        // Same inverted-bounds hazard on the solar_only clamp site. Give it enough
        // surplus to pass the min-operating-watts gate so the clamp is reached.
        let mut config = base_config();
        config.solar_only_mode = true;
        let mut snapshot = base_snapshot();
        snapshot.production_watts = 100_000;

        let decision = decide_policy("solar", &config, &snapshot, 1200, 50, 200, 1400, false);

        if let Some(target) = decision.target_freq_mhz {
            assert!(
                target <= 50,
                "target {} must not exceed the mining ceiling of 50 MHz",
                target
            );
        }
    }
}
