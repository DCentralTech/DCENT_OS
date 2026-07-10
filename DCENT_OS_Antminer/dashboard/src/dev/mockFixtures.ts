/* AUTO-GENERATED QA mock fixtures (dcentos-dashboard-mock-fixtures workflow).
   Complete, type-shaped /api responses for the ?mock harness. Regenerate by
   re-running the fixture workflow + /tmp/extract_fixtures.mjs. Not for production
   behaviour — consumed only by src/dev/mockApi.ts when the QA flag is set. */
export const FIXTURES: Record<string, unknown> = {
  "/api/status": {
    "hashrate_ghs": 95000,
    "hashrate_5s_ghs": 96200,
    "accepted": 1284,
    "rejected": 7,
    "uptime_s": 18432,
    "firmware_version": "DCENT_OS v0.6.0",
    "mode": "standard",
    "chains": [
      {
        "id": 6,
        "chips": 76,
        "frequency_mhz": 525,
        "voltage_mv": 1280,
        "temp_c": 62,
        "hashrate_ghs": 31700,
        "errors": 0,
        "status": "mining"
      },
      {
        "id": 7,
        "chips": 76,
        "frequency_mhz": 525,
        "voltage_mv": 1280,
        "temp_c": 62,
        "hashrate_ghs": 31650,
        "errors": 1,
        "status": "mining"
      },
      {
        "id": 8,
        "chips": 76,
        "frequency_mhz": 525,
        "voltage_mv": 1280,
        "temp_c": 62,
        "hashrate_ghs": 31650,
        "errors": 2,
        "status": "mining"
      }
    ],
    "fans": {
      "pwm": 28,
      "rpm": 3120,
      "per_fan": [
        {
          "id": 0,
          "rpm": 3100,
          "pwm_percent": 28
        },
        {
          "id": 1,
          "rpm": 3140,
          "pwm_percent": 28
        }
      ]
    },
    "pool": {
      "url": "stratum+tcp://public-pool.io:21496",
      "status": "connected",
      "difficulty": 512,
      "last_share_s": 3,
      "donating": false,
      "protocol": "sv1",
      "encrypted": false,
      "auto_fallback_active": false,
      "auto_retry_sv2_after_s": null,
      "auto_fallback_reason": null,
      "share_efficiency": {
        "window_s": 600,
        "accepted_share_count": 1284,
        "accepted_difficulty_sum": 657408,
        "accepted_pool_target_difficulty_sum": 657408,
        "achieved_difficulty_sum": 712900,
        "estimated_wall_energy_kwh": 0.53,
        "accepted_shares_per_kwh": 2422,
        "accepted_difficulty_per_kwh": 1240000,
        "accepted_pool_target_difficulty_per_kwh": 1240000,
        "achieved_difficulty_per_kwh": 1345000,
        "difficulty_source": "achieved",
        "power_source": "estimated",
        "calibrated": false
      }
    },
    "share_efficiency": {
      "window_s": 600,
      "accepted_share_count": 1284,
      "accepted_difficulty_sum": 657408,
      "accepted_pool_target_difficulty_sum": 657408,
      "achieved_difficulty_sum": 712900,
      "estimated_wall_energy_kwh": 0.53,
      "accepted_shares_per_kwh": 2422,
      "accepted_difficulty_per_kwh": 1240000,
      "accepted_pool_target_difficulty_per_kwh": 1240000,
      "achieved_difficulty_per_kwh": 1345000,
      "difficulty_source": "achieved",
      "power_source": "estimated",
      "calibrated": false
    },
    "power": {
      "watts": 3100,
      "wall_watts": 3180,
      "efficiency_jth": 33.5,
      "btu_h": 10850,
      "source": "estimated",
      "calibrated": false,
      "calibration_multiplier": null,
      "watt_cap": {
        "cap_watts": 3600,
        "headroom_watts": 420,
        "overage_watts": 0,
        "utilization_pct": 88.3,
        "throttling": false
      },
      "targeting": {
        "active": false,
        "source": null,
        "mode": null,
        "preset": null,
        "schedule_label": null,
        "target_watts": null,
        "current_wall_watts": 3180,
        "delta_watts": null,
        "comparison": null
      },
      "runtime_limits": []
    }
  },
  "/api/stats": {
    "hashrate_ghs": 95000,
    "hashrate_ths": 95,
    "uptime_s": 18432,
    "chains": [
      {
        "id": 6,
        "chips": 76,
        "frequency_mhz": 525,
        "voltage_mv": 1280,
        "voltage_v": 1.28,
        "temp_c": 62,
        "hashrate_ghs": 31700,
        "hashrate_ths": 31.7,
        "errors": 0,
        "status": "mining",
        "accepted": 428,
        "rejected": 2,
        "hw_errors": 0
      },
      {
        "id": 7,
        "chips": 76,
        "frequency_mhz": 525,
        "voltage_mv": 1280,
        "voltage_v": 1.28,
        "temp_c": 62,
        "hashrate_ghs": 31650,
        "hashrate_ths": 31.65,
        "errors": 1,
        "status": "mining",
        "accepted": 428,
        "rejected": 2,
        "hw_errors": 1
      },
      {
        "id": 8,
        "chips": 76,
        "frequency_mhz": 525,
        "voltage_mv": 1280,
        "voltage_v": 1.28,
        "temp_c": 62,
        "hashrate_ghs": 31650,
        "hashrate_ths": 31.65,
        "errors": 2,
        "status": "mining",
        "accepted": 428,
        "rejected": 3,
        "hw_errors": 2
      }
    ],
    "fans": {
      "pwm": 28,
      "rpm": 3120,
      "per_fan": [
        {
          "id": 0,
          "rpm": 3100,
          "pwm_percent": 28
        },
        {
          "id": 1,
          "rpm": 3140,
          "pwm_percent": 28
        }
      ]
    },
    "share_efficiency": {
      "window_s": 600,
      "accepted_share_count": 1284,
      "accepted_difficulty_sum": 657408,
      "accepted_pool_target_difficulty_sum": 657408,
      "achieved_difficulty_sum": 712900,
      "estimated_wall_energy_kwh": 0.53,
      "accepted_shares_per_kwh": 2422,
      "accepted_difficulty_per_kwh": 1240000,
      "accepted_pool_target_difficulty_per_kwh": 1240000,
      "achieved_difficulty_per_kwh": 1345000,
      "difficulty_source": "achieved",
      "power_source": "estimated",
      "calibrated": false
    },
    "power": {
      "watts": 3100,
      "wall_watts": 3180,
      "efficiency_jth": 33.5,
      "btu_h": 10850,
      "per_chain_w": [
        1033,
        1033,
        1034
      ],
      "source": "estimated",
      "calibrated": false,
      "calibration_multiplier": null,
      "watt_cap": {
        "cap_watts": 3600,
        "headroom_watts": 420,
        "overage_watts": 0,
        "utilization_pct": 88.3,
        "throttling": false
      },
      "targeting": {
        "active": false,
        "source": null,
        "mode": null,
        "preset": null,
        "schedule_label": null,
        "target_watts": null,
        "current_wall_watts": 3180,
        "delta_watts": null,
        "comparison": null
      },
      "runtime_limits": []
    }
  },
  "/api/dashboard/health": {
    "pid": 1234,
    "alive": true,
    "uptime_s": 18432,
    "last_log_lines": [
      "[INFO] mining 95.0 TH/s",
      "[INFO] share accepted diff 512"
    ],
    "last_health_probe_ts": 1717400000
  },
  "/api/setup/status": {
    "needs_setup": false,
    "device_ready": true,
    "mining_ready": true,
    "power_source": "grid",
    "resume_requires_auth": false,
    "password_opt_out": false,
    "password_decision_made": true,
    "safety_opt_out": false,
    "safety_decision_made": true,
    "steps": [
      "safety",
      "circuit",
      "password",
      "mode",
      "pool",
      "complete"
    ],
    "phase": "complete",
    "progress": {
      "safety": true,
      "circuit": true,
      "solar_provider": true,
      "password": true,
      "mode": true,
      "pool": true,
      "complete": true
    },
    "auth": {
      "password_set": true,
      "token_issued": true,
      "password_opt_out": false
    },
    "trust": {
      "install_origin": "operator",
      "bootstrap_transport": "ssh",
      "hardening_profile": "dev",
      "credentials_rotated": true,
      "ssh_keys_enrolled": true,
      "password_auth_disabled": false
    },
    "current": {
      "hostname": "dcentos-miner",
      "mode": "standard",
      "power_source": "grid",
      "circuit_voltage_v": 240,
      "circuit_amperage_a": 20,
      "pool": {
        "url": "stratum+tcp://public-pool.io:21496",
        "worker": "bc1qexampleworker"
      }
    },
    "commissioning": {
      "solar_provider_required": false,
      "solar_provider_saved": false,
      "solar_provider_runtime_adopted": false,
      "solar_provider": null,
      "solar_provider_trust": null
    },
    "completed_at": "2026-06-03T00:00:00Z"
  },
  "/api/config": {
    "mode": {
      "active": "standard"
    },
    "firmware_version": "DCENT_OS v0.6.0",
    "donation": {
      "enabled": true,
      "percent": 2,
      "pool_url": "stratum+tcp://pool.d-central.tech:3333",
      "worker": "DungeonMaster",
      "password": "x",
      "fallback_enabled": true,
      "fallback_pool_url": "stratum+tcp://stratum.braiins.com:3333",
      "fallback_worker": "DungeonMaster",
      "fallback_password": "x",
      "cycle_duration_s": 3600
    },
    "api": {
      "cgminer_port": 4028,
      "http_port": 8080,
      "websocket_enabled": true,
      "auth_enabled": true
    }
  },
  "/api/config/donation": {
    "enabled": true,
    "percent": 2,
    "pool_url": "stratum+tcp://pool.d-central.tech:3333",
    "worker": "DungeonMaster",
    "password": "x",
    "fallback_enabled": true,
    "fallback_pool_url": "stratum+tcp://stratum.braiins.com:3333",
    "fallback_worker": "DungeonMaster",
    "fallback_password": "x",
    "cycle_duration_s": 3600
  },
  "/api/config/psu-override": {
    "active": false,
    "model": "APW3",
    "voltage_v": 12.8,
    "voltage_range": "10.0-20.0V",
    "available_models": [
      {
        "id": "APW3",
        "name": "APW3++",
        "voltage_range": "10.0-14.5V"
      },
      {
        "id": "APW7",
        "name": "APW7",
        "voltage_range": "12.0-15.0V"
      },
      {
        "id": "APW9",
        "name": "APW9",
        "voltage_range": "12.0-15.0V"
      },
      {
        "id": "APW12",
        "name": "APW12",
        "voltage_range": "12.0-15.0V"
      },
      {
        "id": "custom",
        "name": "Custom",
        "voltage_range": "10.0-20.0V"
      }
    ]
  },
  "/api/config/power-calibration": {
    "status": "ok",
    "message": "calibration available",
    "enabled": false,
    "multiplier": 1,
    "reference_wall_watts": null,
    "estimated_wall_watts": 3180,
    "estimated_unit_watts": 3100,
    "updated_at_ms": 1717400000000,
    "current_reported_wall_watts": 3180,
    "current_reported_unit_watts": 3100,
    "power_source": "estimated",
    "power_source_detail": "live_runtime_model",
    "live_power_available": true,
    "power_modeled": true,
    "power_note": "Power is modeled from the live dispatcher estimate; it is not a direct wall-meter measurement.",
    "calibrated": false,
    "calibration_multiplier": null
  },
  "/api/config/mqtt": {
    "enabled": false,
    "broker_host": "",
    "broker_port": 1883,
    "base_topic": "dcentos",
    "username": "",
    "password": "",
    "use_tls": false,
    "publish_interval_s": 30,
    "client_id": "dcentos-miner",
    "retain": false,
    "qos": 0,
    "restart_required": false
  },
  "/api/config/backup/manifest": {
    "status": "ok",
    "read_only": true,
    "content_collected": false,
    "restore_supported": true,
    "daemon_config_export_supported": true,
    "dashboard_preferences_export_supported": true,
    "sources": [
      {
        "id": "daemon_config",
        "label": "Daemon config",
        "path": "/data/dcentrald.toml",
        "active": true,
        "writable_target": true,
        "metadata_status": "ok",
        "exists": true,
        "size_bytes": 4096,
        "modified_ms": 1717400000000
      }
    ],
    "redaction_policy": {
      "content_included": false,
      "secret_key_patterns": [
        "password",
        "worker"
      ],
      "notes": [
        "Secrets are redacted before export."
      ]
    },
    "limitations": []
  },
  "/api/system/info": {
    "firmware": "DCENT_OS",
    "version": "0.6.0",
    "model": "Antminer S19j Pro",
    "hostname": "dcentos-miner",
    "mac": "02:00:00:00:00:25",
    "uptime_s": 18432,
    "chip_type": "BM1362",
    "chip_count": 228,
    "chain_count": 3,
    "mode": "standard",
    "hashrate_ghs": 95000,
    "api_version": "1.0",
    "board": "am2-zynq",
    "soc": "Zynq-7007S",
    "hardware": {
      "capabilities": {
        "voltage_control": "dspic",
        "fan_rpm_feedback": true,
        "sleep_wake_supported": true
      },
      "autotuner": null,
      "miner_serial": "DCENTOS-0001",
      "control_board": "am2-zynq",
      "hb_type": "S19J_HB",
      "chip_type": "BM1362",
      "psu_model": "APW3",
      "psu_fw_version": "0x71",
      "psu_serial": "APW3-0001",
      "psu_voltage_range": "10.0-14.5V",
      "psu_override_active": false
    }
  },
  "/api/system/health": {
    "mode": "native",
    "daemon": {
      "version": "0.6.0",
      "uptime_s": 18432,
      "pid": 1234,
      "is_mining": true
    },
    "bosminer": {
      "alive": false,
      "pid": null,
      "pid_history": [],
      "last_seen_ms": null,
      "blockers": [],
      "last_summary": null
    },
    "rail": {
      "verdict": "ALIVE",
      "last_multimeter_reading_v": 13.7,
      "last_reading_at_ms": 1717400000000,
      "uart_rx_bytes_post_enable": 1134,
      "test_steps": [
        {
          "id": "enable",
          "label": "Rail enable",
          "status": "pass"
        }
      ],
      "steps_url": "/api/diagnostics/chain"
    },
    "recovery": {
      "next_action": null
    },
    "scrape": {
      "cgminer_url": "http://127.0.0.1:4028",
      "cgminer_reachable": true,
      "last_poll_ms": 1717400000000,
      "consecutive_failures": 0
    },
    "watchdog": {
      "available": true,
      "source": "kernel",
      "state": "active",
      "reason": "ok",
      "identity": "xilinx_wdt",
      "status": "running",
      "state_text": "active",
      "bootstatus": 0,
      "timeout_s": 60,
      "timeleft_s": 58,
      "nowayout": false,
      "read_only": true
    },
    "fingerprint": {
      "platform": "zynq-bm3-am2",
      "board_target": "am2-s19jpro",
      "psu_hardware_variant": "override",
      "is_xil_25_class": true
    }
  },
  "/api/system/api-compatibility/manifest": {
    "status": "ok",
    "schema_version": 1,
    "read_only": true,
    "content_collected": false,
    "probe_performed": false,
    "handlers_executed": false,
    "surfaces": [
      {
        "id": "rest",
        "label": "DCENT_OS REST API",
        "protocol": "http",
        "default_port": 8080,
        "default_bind": "0.0.0.0",
        "compatibility": [
          "dcentos"
        ],
        "routes": [
          {
            "method": "GET",
            "path": "/api/status",
            "support": "implemented",
            "mutates": false,
            "compatibility": [
              "dcentos"
            ],
            "provenance": "native",
            "unsupported_fields": [],
            "limitations": []
          }
        ],
        "commands": [
          {
            "name": "summary",
            "support": "implemented",
            "mutates": false,
            "provenance": "cgminer",
            "limitations": []
          }
        ],
        "limitations": []
      }
    ],
    "omissions": [],
    "limitations": []
  },
  "/api/network/block": {
    "status": "ok",
    "read_only": true,
    "internet_dependency": false,
    "available": true,
    "source": "public_fallback",
    "source_label": "Mempool.space",
    "fetched_at_ms": 1717400000000,
    "cache_ttl_ms": 30000,
    "block_height": 887421,
    "height": 887421,
    "block_hash": "00000000000000000002a7c4c1e48d76b1b2c3d4e5f60718293a4b5c6d7e8f90",
    "hash": "00000000000000000002a7c4c1e48d76b1b2c3d4e5f60718293a4b5c6d7e8f90",
    "timestamp_ms": 1717400000000,
    "age_s": 180,
    "difficulty": 110000000000000,
    "previous_hash": "0000000000000000000311a1b2c3d4e5f60718293a4b5c6d7e8f90112233445566",
    "tx_count": 3120,
    "transaction_count": 3120,
    "subsidy_btc": 3.125,
    "fees_btc": 0.18,
    "reward_btc": 3.305,
    "reward_source": "subsidy_only",
    "mempool": {
      "available": true,
      "source": "public_fallback",
      "fee_rate_sat_vb": 9,
      "fastest_fee_sat_vb": 14,
      "half_hour_fee_sat_vb": 9,
      "hour_fee_sat_vb": 6,
      "reason": null
    },
    "pool_job": {
      "available": true,
      "source": "recent_share_history",
      "job_id": "a1b2c3",
      "last_share_timestamp_ms": 1717400000000,
      "difficulty": 512,
      "protocol_meta_present": true,
      "reason": null
    },
    "source_manifest": {
      "local_node": {
        "enabled": false,
        "configured": false,
        "available": false,
        "live_rpc": false,
        "endpoint_label": null,
        "credential_mode": "none",
        "request_timeout_ms": null,
        "reason": "not configured"
      },
      "public_fallback": {
        "enabled": true,
        "available": true,
        "reason": null
      },
      "cache": {
        "enabled": true,
        "ttl_ms": 30000,
        "age_ms": 5000,
        "reason": null
      }
    },
    "reasons": [],
    "limitations": []
  },
  "/api/network/info": {
    "hostname": "dcentos-miner",
    "mac": "02:00:00:00:00:25",
    "primary_interface": "eth0",
    "ipv4_cidr": "192.0.2.25/24",
    "ipv4": "192.0.2.25",
    "ipv6": "fe80::ff:fe00:25",
    "gateway": "192.0.2.1",
    "dns": "1.1.1.1",
    "link_state": "up",
    "dhcp": true,
    "warnings": []
  },
  "/api/network/troubleshoot": {
    "ethernet": {
      "mac": "02:00:00:00:00:25",
      "link_up": true
    },
    "dns_ok": true,
    "gateway_reachable": true,
    "pool_reachable": true,
    "ntp_synced": true,
    "message": "All network checks passed."
  },
  "/api/miner/type": {
    "model": "Antminer S19j Pro",
    "asic": "BM1362",
    "chip_count": 228,
    "chain_count": 3,
    "control_board": "am2-zynq",
    "soc": "Zynq-7007S",
    "hashboard": "S19J_HB",
    "mac": "02:00:00:00:00:25",
    "hostname": "dcentos-miner",
    "firmware": "DCENT_OS",
    "firmware_version": "v0.6.0",
    "pvt_grade": "standard",
    "pvt_voltage_min_mv": 1100,
    "pvt_voltage_max_mv": 1400,
    "pvt_freq_min_mhz": 245,
    "pvt_freq_max_mhz": 545,
    "voltage_fixed": false,
    "mix_levels_supported": true,
    "requires_apw12_plus": false,
    "inverted_curve": false,
    "sku_chain_count": 3,
    "sku_asics_per_chain": 76
  },
  "/api/miner/pvt-table": {
    "sku": "S19j Pro",
    "grade": "standard",
    "voltage_fixed": false,
    "mix_levels": true,
    "requires_apw12_plus": false,
    "inverted_curve": false,
    "chain_count": 3,
    "asics_per_chain": 76,
    "levels": [
      {
        "freq_mhz": 245,
        "voltages_mv": [
          1100,
          1150,
          1200
        ]
      },
      {
        "freq_mhz": 525,
        "voltages_mv": [
          1250,
          1280,
          1320
        ]
      },
      {
        "freq_mhz": 545,
        "voltages_mv": [
          1300,
          1350,
          1400
        ]
      }
    ]
  },
  "/api/history": {
    "history": [
      {
        "timestamp": 1717400000,
        "timestamp_s": 1717400000,
        "hashrate_ghs": 94800,
        "temp_c": 62,
        "power_watts": 3180,
        "fan_rpm": 3120
      },
      {
        "timestamp": 1717403600,
        "timestamp_s": 1717403600,
        "hashrate_ghs": 95200,
        "temp_c": 63,
        "power_watts": 3184,
        "fan_rpm": 3140
      },
      {
        "timestamp": 1717407200,
        "timestamp_s": 1717407200,
        "hashrate_ghs": 95000,
        "temp_c": 62,
        "power_watts": 3180,
        "fan_rpm": 3120
      }
    ],
    "interval_s": 3600,
    "count": 3,
    "message": "ok"
  },
  "/api/history/audit": {
    "schema": "dcentos.history.audit.v1",
    "ring_capacity": 256,
    "total_seen": 1,
    "returned": 1,
    "events": [
      {
        "timestamp_ms": 1717400000000,
        "schema_version": 1,
        "actor": "system",
        "event": {
          "event": "pool_connected",
          "url": "stratum+tcp://public-pool.io:21496"
        }
      }
    ]
  },
  "/api/history/shares": {
    "events": [
      {
        "timestamp_ms": 1717400000000,
        "result": "accepted",
        "job_id": "a1b2c3",
        "difficulty": 689,
        "target_difficulty": 512,
        "error_code": null,
        "error_msg": null,
        "worker_name": "bc1qexampleworker",
        "nonce": "1a2b3c4d",
        "ntime": "665d4f00",
        "extranonce2": "00000001",
        "version_bits": "1fffe000",
        "version": 536870912,
        "protocol_meta_present": true
      },
      {
        "timestamp_ms": 1717399900000,
        "result": "accepted",
        "job_id": "a1b2c2",
        "difficulty": 540,
        "target_difficulty": 512,
        "error_code": null,
        "error_msg": null,
        "worker_name": "bc1qexampleworker",
        "nonce": "2b3c4d5e",
        "ntime": "665d4e9c",
        "extranonce2": "00000002",
        "version_bits": "00002000",
        "version": 536870912,
        "protocol_meta_present": true
      }
    ]
  },
  "/api/perf/efficiency": {
    "j_per_th": 33.5,
    "source": "model",
    "confidence": "medium",
    "measured_at_ms": 1717400000000,
    "operator_wall_watts": null,
    "operator_hashrate_ths": null,
    "jth_target_active": false
  },
  "/api/pools": {
    "pools": [
      {
        "id": 0,
        "url": "stratum+tcp://public-pool.io:21496",
        "worker": "bc1qexampleworker",
        "password": "x",
        "status": "connected",
        "priority": 0,
        "difficulty": 512,
        "accepted": 1284,
        "rejected": 7,
        "last_share_s": 3,
        "latency_ms": 84,
        "latency_measured": true,
        "latency_ms_source": "stratum_status",
        "stratum_active": true,
        "protocol": "sv1",
        "encrypted": false,
        "donating": false,
        "telemetry_source": "stratum",
        "health_limitations": [],
        "no_notify_age_s": 3,
        "failover_policy": "ordered",
        "failover_active_pool_index": 0,
        "failover_last_switch_reason": null,
        "failover_switch_count": 0,
        "failover_stale_jobs_flushed_on_switch": false,
        "pending_submit_correlations_cleared": 0,
        "shares_unresolved": 0,
        "pending_submit_dropped": 0,
        "auto_fallback_active": false,
        "auto_retry_sv2_after_s": null,
        "auto_fallback_reason": null,
        "hashrate_split_bps": 0,
        "hashrate_split_pct": 100,
        "hashrate_split_active": false,
        "hashrate_split_route": "user_pool",
        "share_efficiency": {
          "window_s": 600,
          "accepted_share_count": 1284,
          "accepted_difficulty_sum": 657408,
          "accepted_pool_target_difficulty_sum": 657408,
          "achieved_difficulty_sum": 712900,
          "estimated_wall_energy_kwh": 0.53,
          "accepted_shares_per_kwh": 2422,
          "accepted_difficulty_per_kwh": 1240000,
          "accepted_pool_target_difficulty_per_kwh": 1240000,
          "achieved_difficulty_per_kwh": 1345000,
          "difficulty_source": "achieved",
          "power_source": "estimated",
          "calibrated": false
        }
      }
    ],
    "failover": {
      "schema": "dcentos.pool_failover.v1",
      "read_only": true,
      "enabled": true,
      "configured_pool_count": 1,
      "active_pool_index": 0,
      "active_pool_priority": 0,
      "active_pool_url": "stratum+tcp://public-pool.io:21496",
      "active_pool_host": "public-pool.io",
      "active_worker_redacted": "bc1q…ker",
      "active_route_kind": "user",
      "current_pool_role": "primary",
      "pools": [
        {
          "index": 0,
          "priority": 0,
          "url": "stratum+tcp://public-pool.io:21496",
          "worker_redacted": "bc1q…ker",
          "configured": true,
          "active": true,
          "status": "connected",
          "protocol": "sv1",
          "telemetry_source": "stratum"
        }
      ],
      "consecutive_failures": 0,
      "switch_count": 0,
      "last_switch_reason": null,
      "last_failure_reason": null,
      "stale_jobs_flushed_on_switch": false,
      "pending_submit_correlations_cleared": 0,
      "pending_share_preserved": true,
      "backoff_ms": 0,
      "source_basis": [
        "stratum"
      ],
      "event": "steady",
      "telemetry_source": "stratum",
      "last_update_ms": 1717400000000,
      "stale": false,
      "limitations": []
    },
    "hashrate_split": {
      "schema": "dcentos.hashrate_split.v1",
      "enabled": false,
      "runtime_active": false,
      "routing_mode": "user_pool",
      "algorithm": "none",
      "v1_only": true,
      "simultaneous_clients": false,
      "primary_pool_index": 0,
      "secondary_pool_index": 1,
      "active_route": "user_pool",
      "active_pool_index": 0,
      "active_pool_priority": 0,
      "primary_bps": 0,
      "secondary_bps": 0,
      "primary_pct": 100,
      "secondary_pct": 0,
      "cycle_duration_s": 3600,
      "cycle_remaining_s": 0,
      "switch_count": 0,
      "secondary_shares": 0,
      "donation_composed": false,
      "donation_pct": null,
      "telemetry_source": "stratum"
    },
    "donation": {
      "active": false,
      "route": "user_pool",
      "active_url": "",
      "active_worker": "",
      "pool_index": 0
    }
  },
  "/api/pools/failover": {
    "schema": "dcentos.pool_failover.v1",
    "read_only": true,
    "control_actions": false,
    "hardware_writes": false,
    "filesystem_mutation": false,
    "external_calls": false,
    "license_required": false,
    "license_server_required": false,
    "activation_required": false,
    "mandatory_fee": false,
    "fee_route": "none",
    "local_first": true,
    "secrets_included": false,
    "redacted_fields": [
      "worker"
    ],
    "enabled": true,
    "configured_pool_count": 1,
    "active_pool_index": 0,
    "active_pool_priority": 0,
    "active_pool_url": "stratum+tcp://public-pool.io:21496",
    "active_pool_host": "public-pool.io",
    "active_worker_redacted": "bc1q…ker",
    "active_route_kind": "user",
    "current_pool_role": "primary",
    "pools": [
      {
        "index": 0,
        "priority": 0,
        "url": "stratum+tcp://public-pool.io:21496",
        "worker_redacted": "bc1q…ker",
        "configured": true,
        "active": true,
        "status": "connected",
        "protocol": "sv1",
        "telemetry_source": "stratum"
      }
    ],
    "consecutive_failures": 0,
    "switch_count": 0,
    "last_switch_reason": null,
    "last_failure_reason": null,
    "last_failure_pool_index": null,
    "last_failure_pool_priority": null,
    "stale_jobs_flushed_on_switch": false,
    "pending_submit_correlations_cleared": 0,
    "pending_share_preserved": true,
    "backoff_ms": 0,
    "return_to_primary_policy": "sticky",
    "primary_stable_since_ms": 1717400000000,
    "return_blocked_reason": null,
    "last_flush_at_ms": null,
    "pending_submit_correlations": 0,
    "oldest_pending_submit_age_ms": null,
    "shares_unresolved": 0,
    "pending_submit_dropped": 0,
    "shares_dropped_while_disconnected": 0,
    "donation": {
      "enabled": true,
      "active": false,
      "percent": 2,
      "cycle_duration_s": 3600,
      "cycle_remaining_s": null,
      "pool_visible": true,
      "pool_host": "pool.d-central.tech",
      "fallback_enabled": true,
      "fallback_pool_host": "stratum.braiins.com",
      "fallback_worker_redacted": "DungeonMaster",
      "fallback_policy": "ordered",
      "disable_supported": true,
      "excluded_from_user_failover": true,
      "telemetry_source": "config"
    },
    "hashrate_split": {
      "schema": "dcentos.hashrate_split.v1",
      "enabled": false,
      "runtime_active": false,
      "routing_mode": "user_pool",
      "algorithm": "none",
      "v1_only": true,
      "simultaneous_clients": false,
      "primary_pool_index": 0,
      "secondary_pool_index": 1,
      "active_route": "user_pool",
      "active_pool_index": 0,
      "active_pool_priority": 0,
      "primary_bps": 0,
      "secondary_bps": 0,
      "primary_pct": 100,
      "secondary_pct": 0,
      "cycle_duration_s": 3600,
      "cycle_remaining_s": 0,
      "switch_count": 0,
      "secondary_shares": 0,
      "donation_composed": false,
      "donation_pct": null,
      "telemetry_source": "stratum"
    },
    "source_basis": [
      "stratum"
    ],
    "event": "steady",
    "telemetry_source": "stratum",
    "last_update_ms": 1717400000000,
    "stale_after_ms": 30000,
    "stale": false,
    "limitations": []
  },
  "/api/pool/sv2/status": {
    "connected": false,
    "protocol_version": "2.0",
    "session": {
      "cipher_suite": "ChaChaPoly",
      "handshake_latency_ms": 0,
      "pool_pubkey_fingerprint": "",
      "certificate_valid_from": 1717400000,
      "certificate_not_after": 1717400000,
      "channel_id": 0,
      "noise_nonce_tx": 0,
      "noise_nonce_rx": 0,
      "bytes_encrypted": 0,
      "bytes_decrypted": 0,
      "messages_sent": 0,
      "messages_received": 0
    }
  },
  "/api/pool/sv2/handshake": {
    "cipher_suite": "ChaChaPoly",
    "handshake_latency_ms": 42,
    "pool_pubkey_fingerprint": "9f86d081884c7d659a2feaa0c55ad015",
    "certificate_valid_from": 1717400000,
    "certificate_not_after": 1717400000
  },
  "/api/pool/sv2/messages": {
    "messages": [
      {
        "direction": "sent",
        "msg_type": 0,
        "msg_name": "SetupConnection",
        "timestamp_ms": 1717400000000,
        "payload_size": 75
      },
      {
        "direction": "recv",
        "msg_type": 1,
        "msg_name": "SetupConnectionSuccess",
        "timestamp_ms": 1717400000000,
        "payload_size": 6
      }
    ],
    "total": 2
  },
  "/api/jd/status": {
    "enabled": false,
    "configured": false,
    "connected": false,
    "template_provider_connected": false,
    "job_declarator_connected": false,
    "mining_job_token_available": false,
    "template_prev_hash_ready": false,
    "custom_job_candidate_ready": false,
    "custom_job_injection_ready": false,
    "custom_job_injection_active": false,
    "custom_job_bridge": null,
    "protocol_ready": false,
    "live_jdc_runtime": false,
    "restart_required": false,
    "mode": "coordinated",
    "bitcoind_url": "",
    "template_provider_url": "",
    "job_declarator_url": "",
    "templates_constructed": 0,
    "last_template_age_s": 0,
    "current_template_id": 0,
    "last_declared_job_id": 0,
    "custom_job_last_request_id": 0,
    "custom_job_last_template_id": 0,
    "coinbase_value_remaining_sats": 0,
    "coinbase_output_count": 0,
    "last_connection_attempt_s": 1717400000,
    "last_update_s": 1717400000,
    "last_error": "",
    "current_tx_count": 0,
    "current_fees_btc": 0,
    "runtime_state": "idle",
    "reason": "Job declaration is disabled.",
    "config": {
      "enabled": false,
      "mode": "coordinated",
      "bitcoind_rpc_url": "",
      "bitcoind_rpc_user": "",
      "template_provider_url": "",
      "job_declarator_url": "",
      "coinbase_output_address": "",
      "configured": false,
      "bitcoind_rpc_password_set": false
    }
  },
  "/api/mining/work/posture": {
    "schema": "dcentos.mining.work.posture.v1",
    "status": "active",
    "read_only": true,
    "control_actions": false,
    "hardware_writes": false,
    "filesystem_mutation": false,
    "telemetry_source": "stratum",
    "source": "live",
    "mode": "standard",
    "generated_at_s": 1717400000,
    "fetched_at_ms": 1717400000000,
    "pool": {
      "available": true,
      "url": "stratum+tcp://public-pool.io:21496",
      "status": "connected",
      "active": true,
      "connected": true,
      "connecting": false,
      "mining_capable": true,
      "published_authorized": true,
      "published_authorize_state": "authorized",
      "protocol": "sv1",
      "encrypted": false,
      "pool_target_difficulty": 512,
      "difficulty": 512,
      "last_accepted_share_s": 3,
      "telemetry_source": "stratum",
      "health_limitations": [],
      "no_notify_age_s": 3,
      "failover_policy": "ordered",
      "auto_fallback_active": false,
      "auto_retry_sv2_after_s": null,
      "auto_fallback_reason": null
    },
    "protocol": {
      "name": "sv1",
      "encrypted": false,
      "source": "config",
      "reason": "Stratum V1 active."
    },
    "asic_version_rolling": {
      "bm1362_status": "rolling",
      "claim_default_enabled": true,
      "source": "config",
      "operator_label": "BIP320 enabled",
      "reason": "BM1362 rolls version bits."
    },
    "donation": {
      "active": false,
      "source": "config",
      "reason": "Donation cycle not active."
    },
    "sv2": {
      "available": false,
      "encrypted": false,
      "session": null,
      "source": "config",
      "reason": "SV2 not connected."
    },
    "job_declaration": {
      "available": false,
      "enabled": false,
      "configured": false,
      "connected": false,
      "runtime_state": "idle",
      "mining_job_token_available": false,
      "template_prev_hash_ready": false,
      "custom_job_candidate_ready": false,
      "custom_job_injection_ready": false,
      "custom_job_injection_active": false,
      "custom_job_bridge": null,
      "mode": "coordinated",
      "endpoint": "",
      "template_provider_url": "",
      "job_declarator_url": "",
      "source": "config",
      "reason": "Job declaration disabled."
    },
    "jobs": {
      "available": true,
      "current_job_available": true,
      "latest_observed_job_id": "a1b2c3",
      "latest_observed_job_age_s": 2,
      "latest_observed_job_source": "stratum",
      "recent_job_ids": [
        "a1b2c3",
        "a1b2c2",
        "a1b2c1"
      ],
      "reason": "Jobs flowing."
    },
    "work": {
      "available": true,
      "active_hashrate": true,
      "hashrate_ghs": 95000,
      "hashrate_5s_ghs": 96200,
      "current_notify_age_s": 2,
      "work_ring_occupancy": 12,
      "dispatch_queue_depth": 3,
      "source": "dispatcher",
      "reason": "Work dispatching."
    },
    "shares": {
      "available": true,
      "accepted_total": 1284,
      "rejected_total": 7,
      "total": 1291,
      "accept_rate_pct": 99.46,
      "reject_rate_pct": 0.54,
      "recent_count": 2,
      "accepted_recent": 2,
      "rejected_recent": 0,
      "unknown_recent": 0,
      "latest_event_timestamp_ms": 1717400000000,
      "latest_event_age_s": 3,
      "latest_result": "accepted",
      "latest_job_id": "a1b2c3",
      "source": "stratum",
      "recent_events": [
        {
          "timestamp_ms": 1717400000000,
          "result": "accepted",
          "job_id": "a1b2c3",
          "difficulty": 689,
          "target_difficulty": 512,
          "error_code": null,
          "error_msg": null,
          "worker_name": "bc1qexampleworker",
          "nonce": "1a2b3c4d",
          "ntime": "665d4f00",
          "extranonce2": "00000001",
          "version_bits": "1fffe000",
          "version": 536870912,
          "protocol_meta_present": true
        }
      ],
      "reason": "Shares accepted."
    },
    "sources": [
      "stratum",
      "dispatcher"
    ],
    "limitations": []
  },
  "/api/mining/pipeline/snapshot": {
    "schema": "dcentos.mining.pipeline.snapshot.v1",
    "status": "live",
    "publisher_enabled": true,
    "snapshot_available": true,
    "read_only": true,
    "control_actions": false,
    "hardware_writes": false,
    "filesystem_mutation": false,
    "generated_at_ms": 1717400000000,
    "publisher_last_update_ms": 1717400000000,
    "snapshot_age_ms": 1000,
    "last_notify_timestamp_ms": 1717400000000,
    "last_notify_age_ms": 2000,
    "current_job_id": "a1b2c3",
    "clean_jobs_total": 14,
    "dispatch_bursts_total": 920,
    "nonce_bursts_total": 238,
    "stale_nonce_drops_total": 0,
    "unsupported_version_drops_total": 0,
    "local_validation_drops_total": 0,
    "work_ring_occupancy": 12,
    "dispatch_queue_depth": 3,
    "source": "publisher",
    "limitations": []
  },
  "/api/mining/pipeline/manifest": {
    "schema": "dcentos.mining.pipeline.manifest.v1",
    "status": "available",
    "read_only": true,
    "control_actions": false,
    "hardware_writes": false,
    "filesystem_mutation": false,
    "content_collected": false,
    "probe_performed": false,
    "handlers_executed": false,
    "telemetry_source": "publisher",
    "source": "live",
    "generated_at_s": 1717400000,
    "fetched_at_ms": 1717400000000,
    "publisher_live": true,
    "snapshot_available": true,
    "snapshot_schema": "dcentos.mining.pipeline.snapshot.v1",
    "snapshot_contract": {
      "schema": "dcentos.mining.pipeline.snapshot.v1",
      "status": "live",
      "publisher_enabled": true,
      "snapshot_available": true,
      "read_only": true,
      "control_actions": false,
      "hardware_writes": false,
      "filesystem_mutation": false,
      "generated_at_ms": 1717400000000,
      "publisher_last_update_ms": 1717400000000,
      "snapshot_age_ms": 1000,
      "last_notify_timestamp_ms": 1717400000000,
      "last_notify_age_ms": 2000,
      "current_job_id": "a1b2c3",
      "clean_jobs_total": 14,
      "dispatch_bursts_total": 920,
      "nonce_bursts_total": 238,
      "stale_nonce_drops_total": 0,
      "unsupported_version_drops_total": 0,
      "local_validation_drops_total": 0,
      "work_ring_occupancy": 12,
      "dispatch_queue_depth": 3,
      "source": "publisher",
      "limitations": []
    },
    "publisher_gate": {
      "app_state_field": "pipeline_publisher",
      "receiver_configured": true,
      "receiver_default": "off",
      "config_toml_path": "[mining.pipeline].publisher_enabled",
      "config_default_enabled": false,
      "enabled_configs_rejected": false,
      "publisher_default_enabled": false,
      "live_snapshot_endpoint": "/api/mining/pipeline/snapshot",
      "promotion_requires": []
    },
    "freshness_contract": {
      "default_stale_after_ms": 30000,
      "status_unavailable_when": [
        "publisher disabled"
      ],
      "status_live_when": [
        "age < stale_after"
      ],
      "status_stale_when": [
        "age >= stale_after"
      ],
      "snapshot_available_only_when": "publisher enabled",
      "does_not_populate": []
    },
    "freshness_classifier_schema": "dcentos.mining.pipeline.freshness.classifier.v1",
    "freshness_classifier": {
      "schema": "dcentos.mining.pipeline.freshness.classifier.v1",
      "status": "design_only",
      "implemented": true,
      "runtime_wired": false,
      "publisher_enabled": true,
      "snapshot_available": true,
      "live_route_mounted": true,
      "read_only": true,
      "control_actions": false,
      "hardware_writes": false,
      "filesystem_mutation": false,
      "content_collected": false,
      "probe_performed": false,
      "handlers_executed": false,
      "telemetry_source": "none",
      "default_stale_after_ms": 30000,
      "max_future_skew_ms": 5000,
      "inputs": [
        "domain_last_update_ms",
        "generated_at_ms"
      ],
      "outputs": [
        "unavailable",
        "live",
        "stale",
        "future_clock_skew",
        "invalid"
      ],
      "fail_closed_when": [
        "null inputs"
      ],
      "snapshot_status_mapping": {
        "live": "live",
        "stale": "stale",
        "unavailable": "unavailable"
      },
      "example_fixtures_schema": "dcentos.mining.pipeline.freshness.classifier.fixture.v1",
      "example_fixture_count": 1,
      "example_fixtures_are_design_only": true,
      "example_fixtures_live_telemetry": false,
      "example_fixtures": [
        {
          "id": "live",
          "label": "Live",
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
            "domain_last_update_ms": 1717400000000,
            "generated_at_ms": 1717400000000,
            "stale_after_ms": 30000,
            "max_future_skew_ms": 5000
          },
          "expected_classifier_status": "live",
          "expected_snapshot_status": "live",
          "snapshot_available": true,
          "reason": "fresh sample"
        }
      ],
      "does_not_read": [
        "dispatcher"
      ],
      "does_not_populate": [
        "live snapshot"
      ],
      "promotion_note": "design only"
    },
    "publisher_design": {
      "schema": "dcentos.mining.pipeline.publisher.design.v1",
      "status": "implemented_default_off",
      "implemented": true,
      "publisher_enabled": true,
      "live_route_mounted": true,
      "config_gate": "[mining.pipeline].publisher_enabled",
      "enabled_configs_rejected": false,
      "owner": "dispatcher",
      "transport": "app_state",
      "rest_consumer": "rest",
      "runtime_source": "publisher",
      "bounded_publish_cadence": {
        "required": true,
        "max_hz": 5,
        "min_interval_ms": 200,
        "publish_per_nonce": false,
        "reason": "bounded cadence"
      },
      "promotion_blockers": [],
      "forbidden": [
        "per-nonce publish"
      ],
      "hardware_smoke_required": [
        {
          "model": "s19jpro",
          "required": true,
          "status": "pass",
          "checks": [
            "enum",
            "shares"
          ]
        }
      ],
      "promotion_requires": [
        "hardware smoke"
      ]
    },
    "snapshot_design_schema": "dcentos.mining.pipeline.snapshot.design.v2",
    "snapshot_design": {
      "schema": "dcentos.mining.pipeline.snapshot.design.v2",
      "status": "implemented_default_off",
      "implemented": true,
      "publisher_enabled": true,
      "snapshot_available": true,
      "live_route_mounted": true,
      "read_only": true,
      "control_actions": false,
      "hardware_writes": false,
      "filesystem_mutation": false,
      "content_collected": false,
      "probe_performed": false,
      "handlers_executed": false,
      "source": "publisher",
      "target_snapshot_schema": "dcentos.mining.pipeline.snapshot.v1",
      "config_gate": "[mining.pipeline]",
      "enabled_configs_rejected": false,
      "publisher_required": true,
      "domain_freshness_status": "unavailable",
      "blocks": {
        "job_freshness": {
          "status": "unavailable",
          "last_update_ms": null,
          "age_ms": null,
          "stale_after_ms": 30000,
          "source": null,
          "null_reason": "not wired",
          "future_fields": [],
          "control_authority": false
        },
        "work_freshness": {
          "status": "unavailable",
          "last_update_ms": null,
          "age_ms": null,
          "stale_after_ms": 30000,
          "source": null,
          "null_reason": "not wired",
          "future_fields": [],
          "control_authority": false
        },
        "nonce_freshness": {
          "status": "unavailable",
          "last_update_ms": null,
          "age_ms": null,
          "stale_after_ms": 30000,
          "source": null,
          "null_reason": "not wired",
          "future_fields": [],
          "control_authority": false
        },
        "share_freshness": {
          "status": "unavailable",
          "last_update_ms": null,
          "age_ms": null,
          "stale_after_ms": 30000,
          "source": null,
          "null_reason": "not wired",
          "future_fields": [],
          "control_authority": false
        }
      },
      "forbidden": [
        "per-nonce publish"
      ],
      "hardware_smoke_required": [
        {
          "model": "s19jpro",
          "required": true,
          "status": "pass"
        }
      ],
      "promotion_requires": [
        "hardware smoke"
      ],
      "limitations": []
    },
    "promotion_checklist_schema": "dcentos.mining.pipeline.publisher.promotion.checklist.v1",
    "publisher_promotion_checklist": {
      "schema": "dcentos.mining.pipeline.publisher.promotion.checklist.v1",
      "status": "implemented_default_off",
      "promotion_state": "blocked",
      "implemented": true,
      "source": "publisher",
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
      "publisher_enabled": true,
      "snapshot_available": true,
      "live_route_mounted": true,
      "target_snapshot_design_schema": "dcentos.mining.pipeline.snapshot.design.v2",
      "target_snapshot_schema": "dcentos.mining.pipeline.snapshot.v1",
      "config_gate": "[mining.pipeline]",
      "enabled_configs_rejected": false,
      "required_publisher_owner": "dispatcher",
      "required_transport": "app_state",
      "required_rest_consumer": "rest",
      "required_rollback_path": "config flag",
      "blockers_schema": "dcentos.mining.pipeline.publisher.promotion.blocker.v1",
      "blocker_count": 1,
      "active_blocker_count": 0,
      "all_blockers_active": false,
      "active_blocker_ids": [],
      "requirements": [
        {
          "id": "publisher_wired",
          "label": "Publisher wired",
          "status": "pass",
          "required": true,
          "current_state": "wired",
          "evidence_source": "app_state",
          "reason": "ok"
        }
      ],
      "blockers": [
        {
          "id": "publisher_not_wired",
          "label": "Publisher not wired",
          "active": false,
          "severity": "cleared",
          "evidence_source": "app_state",
          "reason": "wired",
          "clears_when": "publisher running"
        }
      ],
      "forbidden": [
        "per-nonce publish"
      ],
      "promotion_allowed_only_when": [
        "hardware smoke pass"
      ]
    },
    "fleet_parser_notes_schema": "dcentos.mining.pipeline.fleet_parser_notes.v1",
    "fleet_parser_notes": {
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
          "source_path": "checklist.active_blocker_ids",
          "kind": "alias",
          "readiness_evidence": false,
          "telemetry_source": "none",
          "not_authoritative_for": [
            "miner_state"
          ]
        },
        "freshness_classifier_example_fixtures": {
          "source_path": "freshness_classifier.example_fixtures",
          "kind": "alias",
          "readiness_evidence": false,
          "telemetry_source": "none",
          "not_authoritative_for": [
            "miner_state"
          ]
        }
      },
      "authoritative_sources": [
        {
          "field": "status",
          "source_path": "snapshot.status",
          "reason": "live snapshot"
        }
      ],
      "live_promotion_requires": [
        "hardware smoke"
      ],
      "does_not_read": [
        "dispatcher"
      ],
      "does_not_clear": [],
      "operator_note": "schema only"
    },
    "live_publisher": {
      "available": true,
      "enabled": true,
      "snapshot_available": true,
      "source": "publisher",
      "reason": "publisher running"
    },
    "existing_surfaces": [
      {
        "id": "status",
        "label": "Status",
        "available": true,
        "persistent": false,
        "rest_queryable": true,
        "source": "rest",
        "fields": [
          "hashrate_ghs"
        ],
        "limitations": []
      }
    ],
    "candidate_snapshot_fields": [
      {
        "id": "current_job_id",
        "label": "Current job",
        "status": "available",
        "source_hint": "publisher",
        "publisher_required": true,
        "hardware_required": false,
        "regression_risk": "low",
        "validation": "automated",
        "reason": "ok"
      }
    ],
    "publisher_contract": {
      "owner": "dispatcher",
      "transport": "app_state",
      "update_budget": "5 Hz",
      "rest_consumer": "rest",
      "control_scope": "read-only",
      "forbidden": [
        "per-nonce publish"
      ]
    },
    "validation_plan": {
      "automated": [
        "unit tests"
      ],
      "hardware_required": [
        "s19jpro smoke"
      ]
    },
    "related_endpoints": [
      "/api/mining/pipeline/snapshot"
    ],
    "limitations": []
  },
  "/api/mining/pipeline/snapshot/schema": {
    "schema": "dcentos.mining.pipeline.snapshot.schema.v1",
    "snapshot_schema": "dcentos.mining.pipeline.snapshot.v1",
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
      "toml_path": "[mining.pipeline].publisher_enabled",
      "default_enabled": false,
      "current_config_read": false,
      "enabled_configs_rejected": false,
      "live_snapshot_endpoint": "/api/mining/pipeline/snapshot",
      "reason": "default off"
    },
    "generated_at_s": 1717400000,
    "fetched_at_ms": 1717400000000,
    "default_snapshot": {
      "schema": "dcentos.mining.pipeline.snapshot.v1",
      "status": "unavailable",
      "publisher_enabled": false,
      "snapshot_available": false,
      "read_only": true,
      "control_actions": false,
      "hardware_writes": false,
      "filesystem_mutation": false,
      "generated_at_ms": 1717400000000,
      "publisher_last_update_ms": null,
      "snapshot_age_ms": null,
      "last_notify_timestamp_ms": null,
      "last_notify_age_ms": null,
      "current_job_id": null,
      "clean_jobs_total": null,
      "dispatch_bursts_total": null,
      "nonce_bursts_total": null,
      "stale_nonce_drops_total": null,
      "unsupported_version_drops_total": null,
      "local_validation_drops_total": null,
      "work_ring_occupancy": null,
      "dispatch_queue_depth": null,
      "source": "default",
      "limitations": [
        "publisher disabled"
      ]
    },
    "freshness_contract": {
      "default_stale_after_ms": 30000,
      "status_unavailable_when": [
        "publisher disabled"
      ],
      "status_live_when": [
        "age < stale"
      ],
      "status_stale_when": [
        "age >= stale"
      ],
      "snapshot_available_only_when": "publisher enabled",
      "does_not_populate": []
    },
    "freshness_classifier_schema": "dcentos.mining.pipeline.freshness.classifier.v1",
    "freshness_classifier": {
      "schema": "dcentos.mining.pipeline.freshness.classifier.v1",
      "status": "design_only",
      "implemented": true,
      "runtime_wired": false,
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
      "telemetry_source": "none",
      "default_stale_after_ms": 30000,
      "max_future_skew_ms": 5000,
      "inputs": [
        "domain_last_update_ms"
      ],
      "outputs": [
        "unavailable",
        "live",
        "stale"
      ],
      "fail_closed_when": [
        "null inputs"
      ],
      "snapshot_status_mapping": {
        "live": "live"
      },
      "example_fixtures_schema": "dcentos.mining.pipeline.freshness.classifier.fixture.v1",
      "example_fixture_count": 1,
      "example_fixtures_are_design_only": true,
      "example_fixtures_live_telemetry": false,
      "example_fixtures": [
        {
          "id": "unavailable",
          "label": "Unavailable",
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
            "domain_last_update_ms": null,
            "generated_at_ms": 1717400000000,
            "stale_after_ms": 30000,
            "max_future_skew_ms": 5000
          },
          "expected_classifier_status": "unavailable",
          "expected_snapshot_status": "unavailable",
          "snapshot_available": false,
          "reason": "publisher off"
        }
      ],
      "does_not_read": [
        "dispatcher"
      ],
      "does_not_populate": [
        "live snapshot"
      ],
      "promotion_note": "design only"
    },
    "publisher_design": {
      "schema": "dcentos.mining.pipeline.publisher.design.v1",
      "status": "design_only",
      "implemented": true,
      "publisher_enabled": false,
      "live_route_mounted": true,
      "config_gate": "[mining.pipeline].publisher_enabled",
      "enabled_configs_rejected": false,
      "owner": "dispatcher",
      "transport": "app_state",
      "rest_consumer": "rest",
      "bounded_publish_cadence": {
        "required": true,
        "max_hz": 5,
        "min_interval_ms": 200,
        "publish_per_nonce": false,
        "reason": "bounded"
      },
      "promotion_blockers": [
        "not wired"
      ],
      "forbidden": [
        "per-nonce publish"
      ],
      "hardware_smoke_required": [
        {
          "model": "s19jpro",
          "required": true,
          "status": "not_run",
          "checks": [
            "enum"
          ]
        }
      ],
      "promotion_requires": [
        "hardware smoke"
      ]
    },
    "snapshot_design_schema": "dcentos.mining.pipeline.snapshot.design.v2",
    "snapshot_design": {
      "schema": "dcentos.mining.pipeline.snapshot.design.v2",
      "status": "design_only",
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
      "source": "design",
      "target_snapshot_schema": "dcentos.mining.pipeline.snapshot.v1",
      "config_gate": "[mining.pipeline]",
      "enabled_configs_rejected": false,
      "publisher_required": true,
      "domain_freshness_status": "unavailable",
      "blocks": {
        "job_freshness": {
          "status": "unavailable",
          "last_update_ms": null,
          "age_ms": null,
          "stale_after_ms": 30000,
          "source": null,
          "null_reason": "not wired",
          "future_fields": [],
          "control_authority": false
        },
        "work_freshness": {
          "status": "unavailable",
          "last_update_ms": null,
          "age_ms": null,
          "stale_after_ms": 30000,
          "source": null,
          "null_reason": "not wired",
          "future_fields": [],
          "control_authority": false
        },
        "nonce_freshness": {
          "status": "unavailable",
          "last_update_ms": null,
          "age_ms": null,
          "stale_after_ms": 30000,
          "source": null,
          "null_reason": "not wired",
          "future_fields": [],
          "control_authority": false
        },
        "share_freshness": {
          "status": "unavailable",
          "last_update_ms": null,
          "age_ms": null,
          "stale_after_ms": 30000,
          "source": null,
          "null_reason": "not wired",
          "future_fields": [],
          "control_authority": false
        }
      },
      "forbidden": [
        "per-nonce publish"
      ],
      "hardware_smoke_required": [
        {
          "model": "s19jpro",
          "required": true,
          "status": "not_run"
        }
      ],
      "promotion_requires": [
        "hardware smoke"
      ],
      "limitations": []
    },
    "promotion_checklist_schema": "dcentos.mining.pipeline.publisher.promotion.checklist.v1",
    "publisher_promotion_checklist": {
      "schema": "dcentos.mining.pipeline.publisher.promotion.checklist.v1",
      "status": "design_only",
      "promotion_state": "blocked",
      "implemented": true,
      "source": "design",
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
      "target_snapshot_schema": "dcentos.mining.pipeline.snapshot.v1",
      "config_gate": "[mining.pipeline]",
      "enabled_configs_rejected": false,
      "required_publisher_owner": "dispatcher",
      "required_transport": "app_state",
      "required_rest_consumer": "rest",
      "required_rollback_path": "config flag",
      "blockers_schema": "dcentos.mining.pipeline.publisher.promotion.blocker.v1",
      "blocker_count": 1,
      "active_blocker_count": 1,
      "all_blockers_active": true,
      "active_blocker_ids": [
        "publisher_not_wired"
      ],
      "requirements": [
        {
          "id": "publisher_wired",
          "label": "Publisher wired",
          "status": "blocked",
          "required": true,
          "current_state": "not wired",
          "evidence_source": "app_state",
          "reason": "default off"
        }
      ],
      "blockers": [
        {
          "id": "publisher_not_wired",
          "label": "Publisher not wired",
          "active": true,
          "severity": "promotion_blocking",
          "evidence_source": "app_state",
          "reason": "default off",
          "clears_when": "publisher running"
        }
      ],
      "forbidden": [
        "per-nonce publish"
      ],
      "promotion_allowed_only_when": [
        "hardware smoke pass"
      ]
    },
    "fleet_parser_notes_schema": "dcentos.mining.pipeline.fleet_parser_notes.v1",
    "fleet_parser_notes": {
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
          "source_path": "checklist.active_blocker_ids",
          "kind": "alias",
          "readiness_evidence": false,
          "telemetry_source": "none",
          "not_authoritative_for": [
            "miner_state"
          ]
        },
        "freshness_classifier_example_fixtures": {
          "source_path": "freshness_classifier.example_fixtures",
          "kind": "alias",
          "readiness_evidence": false,
          "telemetry_source": "none",
          "not_authoritative_for": [
            "miner_state"
          ]
        }
      },
      "authoritative_sources": [
        {
          "field": "status",
          "source_path": "snapshot.status",
          "reason": "live snapshot"
        }
      ],
      "live_promotion_requires": [
        "hardware smoke"
      ],
      "does_not_read": [
        "dispatcher"
      ],
      "does_not_clear": [],
      "operator_note": "schema only"
    },
    "fields": [
      {
        "name": "current_job_id",
        "type": "string|null",
        "default": null,
        "source": "publisher"
      }
    ],
    "forbidden": [
      "per-nonce publish"
    ],
    "validation_required": [
      "unit tests"
    ],
    "limitations": [
      "publisher default off"
    ]
  },
  "/api/mining/chain/presence": {
    "chains": [
      {
        "idx": 0,
        "chips_responding": 76,
        "chips_expected": 76,
        "mv_actual": 13700,
        "mv_target": 13700
      },
      {
        "idx": 1,
        "chips_responding": 76,
        "chips_expected": 76,
        "mv_actual": 13700,
        "mv_target": 13700
      },
      {
        "idx": 2,
        "chips_responding": 76,
        "chips_expected": 76,
        "mv_actual": 13700,
        "mv_target": 13700
      }
    ]
  },
  "/api/thermal/posture": {
    "schema": "dcentos.thermal.posture.v1",
    "status": "ok",
    "read_only": true,
    "control_actions": false,
    "hardware_writes": false,
    "filesystem_mutation": false,
    "telemetry_source": "chain_temps",
    "source": "live",
    "mode": "standard",
    "generated_at_s": 1717400000,
    "fetched_at_ms": 1717400000000,
    "thermal": {
      "available": true,
      "reason": "ok",
      "avg_temp_c": 62.5,
      "max_temp_c": 64.1,
      "hottest_chain_id": 7,
      "valid_chain_count": 3,
      "missing_chain_count": 0,
      "chains": [
        {
          "id": 6,
          "temp_c": 62,
          "status": "ok",
          "source": "chain"
        },
        {
          "id": 7,
          "temp_c": 64.1,
          "status": "ok",
          "source": "chain"
        },
        {
          "id": 8,
          "temp_c": 62,
          "status": "ok",
          "source": "chain"
        }
      ],
      "thresholds": {
        "target_c": 55,
        "hot_c": 65,
        "dangerous_c": 70,
        "hysteresis_c": 2,
        "source": "profile",
        "reason": "home"
      }
    },
    "fans": {
      "available": true,
      "pwm": 28,
      "rpm": 3120,
      "per_fan": [
        {
          "id": 0,
          "rpm": 3100,
          "pwm_percent": 28
        },
        {
          "id": 1,
          "rpm": 3140,
          "pwm_percent": 28
        }
      ],
      "rpm_feedback_available": true,
      "tach_suspect": false,
      "min_pwm": 10,
      "max_pwm": 100,
      "range_source": "profile",
      "reason": "ok"
    },
    "power": {
      "available": true,
      "board_watts": 3100,
      "wall_watts": 3180,
      "efficiency_jth": 33.5,
      "btu_h": 10850,
      "source": "estimated",
      "calibrated": false,
      "calibration_multiplier": null,
      "age_s": 1,
      "watt_cap": {
        "cap_watts": 3600,
        "headroom_watts": 420,
        "overage_watts": 0,
        "utilization_pct": 88.3,
        "throttling": false
      },
      "runtime_limits_visible": false,
      "dispatcher_limit_count": 0,
      "runtime_limits": [],
      "reason": "ok"
    },
    "curtailment": {
      "available": false,
      "state": "inactive",
      "source": "none",
      "read_only": true,
      "reason": "not curtailed"
    },
    "hardware_support": {
      "fan_rpm_feedback": true,
      "power_source": "estimated",
      "power_calibrated": false,
      "pmbus_measured": false,
      "reason": "ok"
    },
    "runtime_ownership": {
      "dispatcher_limits_visible": false,
      "thermal_related_limit": false,
      "power_cap_active": false,
      "reason": "ok"
    },
    "safety": {
      "mode": "standard",
      "envelope": {
        "dangerous_temp_c": 70,
        "max_frequency_mhz": 545,
        "allow_overclock": false,
        "allow_raw_registers": false,
        "min_fan_pwm": 10,
        "max_power_watts": 3600
      },
      "thermal_blocker": false,
      "reason": "ok"
    },
    "sources": [
      "chain_temps"
    ],
    "limitations": []
  },
  "/api/autotuner/status": {
    "enabled": false,
    "live_runtime": false,
    "stale": false,
    "age_s": 2,
    "source": "idle",
    "state": "idle",
    "phase": "idle",
    "percent_complete": 0,
    "completed_chips": 0,
    "active_chips": 0,
    "total_chips": 228,
    "active_chain_id": null,
    "active_chain_total_chips": null,
    "target_chains": 3,
    "tuned_chains": 0,
    "failed_chains": 0,
    "tuned_chain_ids": [],
    "failed_chain_ids": [],
    "estimated_remaining_s": null,
    "avg_frequency_mhz": 523,
    "efficiency_jth": 33.5,
    "silicon_grades": null,
    "policy": null,
    "dispatcher_limits": [],
    "last_update_s": 1717400000,
    "message": "Autotuner idle."
  },
  "/api/autotuner/visibility": {
    "status": "ok",
    "read_only": true,
    "control_actions": false,
    "hardware_writes": false,
    "filesystem_mutation": false,
    "generated_at_s": 1717400000,
    "source": "live",
    "fetched_at_ms": 1717400000000,
    "runtime": {
      "available": true,
      "enabled": false,
      "state": "idle",
      "phase": "idle",
      "source": "runtime",
      "stale": false,
      "age_s": 2,
      "message": "idle",
      "dispatcher_limits_visible": false,
      "dispatcher_limit_count": 0
    },
    "saved_profiles": {
      "available": true,
      "chains_with_profiles": 3,
      "expected_chains": 3,
      "entries": [
        {
          "chain_id": 6,
          "file": "chain6.json",
          "present": true,
          "read_ok": true,
          "parse_ok": true,
          "chip_count": 76,
          "tuned_at": "2026-06-02T00:00:00Z",
          "avg_freq_mhz": 525,
          "reason": null
        },
        {
          "chain_id": 7,
          "file": "chain7.json",
          "present": true,
          "read_ok": true,
          "parse_ok": true,
          "chip_count": 76,
          "tuned_at": "2026-06-02T00:00:00Z",
          "avg_freq_mhz": 523,
          "reason": null
        },
        {
          "chain_id": 8,
          "file": "chain8.json",
          "present": true,
          "read_ok": true,
          "parse_ok": true,
          "chip_count": 76,
          "tuned_at": "2026-06-02T00:00:00Z",
          "avg_freq_mhz": 521,
          "reason": null
        }
      ],
      "reason": "Profiles present on disk."
    },
    "telemetry": {
      "available": true,
      "live_runtime": false,
      "recording": false,
      "run_count": 1,
      "last_update_s": 1717400000,
      "csv_available": true,
      "json_endpoint": "/api/autotuner/status",
      "csv_endpoint": "/api/autotuner/telemetry.csv",
      "latest_run": {
        "started_at_s": 1717400000,
        "duration_s": 600,
        "completed": true,
        "sample_count": 120
      },
      "reason": "ok"
    },
    "rollback": {
      "available": true,
      "backup_profiles": [],
      "backup_profile_count": 0,
      "config_visible": true,
      "automatic_rollback_visible": true,
      "reason": "ok"
    },
    "simulation": {
      "available": false,
      "simulation_only": false,
      "reason": "not in simulation"
    },
    "limitations": []
  },
  "/api/autotuner/chip-health": {
    "source": "runtime",
    "live_runtime": false,
    "stale": false,
    "age_s": 2,
    "last_update_s": 1717400000,
    "message": "ok",
    "total_chips": 228,
    "chips": [
      {
        "chain_id": 6,
        "chip_index": 0,
        "health_score": 98,
        "trend": 0,
        "estimated_days_to_warning": null,
        "error_rate_pct": 0.1,
        "freq_mhz": 525,
        "backoff_count": 0,
        "hashrate_ratio": 1,
        "status": "healthy"
      },
      {
        "chain_id": 7,
        "chip_index": 0,
        "health_score": 96,
        "trend": -1,
        "estimated_days_to_warning": null,
        "error_rate_pct": 0.3,
        "freq_mhz": 523,
        "backoff_count": 0,
        "hashrate_ratio": 0.99,
        "status": "healthy"
      }
    ]
  },
  "/api/autotuner/silicon-report": {
    "characterized": true,
    "not_characterized_chips": 0,
    "quality_score": 82,
    "quality_tier": "Good",
    "total_chips": 228,
    "grade_a_count": 60,
    "grade_b_count": 120,
    "grade_c_count": 40,
    "grade_d_count": 8,
    "grade_a_pct": 26.3,
    "grade_b_pct": 52.6,
    "grade_c_pct": 17.5,
    "grade_d_pct": 3.5,
    "avg_max_stable_mhz": 545,
    "best_chip_mhz": 600,
    "worst_chip_mhz": 480,
    "frequency_std_dev_mhz": 18,
    "chain_reports": [
      {
        "chain_id": 6,
        "chip_count": 76,
        "quality_score": 84,
        "avg_max_stable_mhz": 548,
        "grade_distribution": [
          22,
          40,
          12,
          2
        ]
      },
      {
        "chain_id": 7,
        "chip_count": 76,
        "quality_score": 82,
        "avg_max_stable_mhz": 545,
        "grade_distribution": [
          20,
          40,
          13,
          3
        ]
      },
      {
        "chain_id": 8,
        "chip_count": 76,
        "quality_score": 80,
        "avg_max_stable_mhz": 542,
        "grade_distribution": [
          18,
          40,
          15,
          3
        ]
      }
    ],
    "top_5_chips": [
      {
        "chain_id": 6,
        "chip_index": 12,
        "max_stable_mhz": 600,
        "grade": "A",
        "effective_grade": "A",
        "error_rate": 0.05,
        "nonces_counted": 4200,
        "characterized": true
      }
    ],
    "bottom_5_chips": [
      {
        "chain_id": 8,
        "chip_index": 71,
        "max_stable_mhz": 480,
        "grade": "D",
        "effective_grade": "D",
        "error_rate": 1.8,
        "nonces_counted": 1900,
        "characterized": true
      }
    ]
  },
  "/api/autotuner/target": {
    "target_jth": 33,
    "target_freq_mhz": 525,
    "mode": "efficiency",
    "enabled": false
  },
  "/api/profiles": {
    "profiles": [
      {
        "name": "Quiet Home",
        "frequency_mhz": 525,
        "voltage_mv": 1280,
        "fan_mode": "quiet"
      },
      {
        "name": "Efficiency",
        "frequency_mhz": 500,
        "voltage_mv": 1250,
        "fan_mode": "auto"
      }
    ],
    "active_profile": "Quiet Home"
  },
  "/api/profiles/silicon": [
    {
      "id": "s9-bm1387-baked",
      "miner_model": "Antminer S9",
      "hashboard": "BHB42601",
      "chip": "bm1387",
      "source_class": "baked",
      "preset_count": 12
    },
    {
      "id": "s17-bm1397-baked",
      "miner_model": "Antminer S17",
      "hashboard": "S17_HB",
      "chip": "bm1397",
      "source_class": "baked",
      "preset_count": 12
    },
    {
      "id": "s19pro-bm1398-baked",
      "miner_model": "Antminer S19 Pro",
      "hashboard": "S19_HB",
      "chip": "bm1398",
      "source_class": "baked",
      "preset_count": 12
    },
    {
      "id": "s19jpro-bm1362-baked",
      "miner_model": "Antminer S19j Pro",
      "hashboard": "S19J_HB",
      "chip": "bm1362",
      "source_class": "baked",
      "preset_count": 12
    },
    {
      "id": "s21-bm1368-baked",
      "miner_model": "Antminer S21",
      "hashboard": "S21_HB",
      "chip": "bm1368",
      "source_class": "baked",
      "preset_count": 12
    },
    {
      "id": "s19kpro-bm1366-baked",
      "miner_model": "Antminer S19k Pro",
      "hashboard": "S19K_HB",
      "chip": "bm1366",
      "source_class": "baked",
      "preset_count": 12
    }
  ],
  "/api/debug/pid-state": {
    "kp": 1.2,
    "ki": 0.1,
    "kd": 0.01,
    "setpoint": 60,
    "current_temp": 62.5,
    "output": 28,
    "integral": 4.2,
    "last_error": -2.5,
    "message": "ok"
  },
  "/api/home/status": {
    "power_watts": 3100,
    "wall_watts": 3180,
    "btu_h": 10850,
    "source": "estimated",
    "calibrated": false,
    "calibration_multiplier": null,
    "targeting": {
      "active": false,
      "source": null,
      "mode": null,
      "preset": null,
      "schedule_label": null,
      "target_watts": null,
      "current_wall_watts": 3180,
      "delta_watts": null,
      "comparison": null
    },
    "noise_db": 42,
    "noise_source": "tach_estimate",
    "noise_note": "Estimated from fan tachometer RPM.",
    "airflow_cfm": 220,
    "preset": "balanced",
    "room_temp_c": 21.5,
    "cost_today_usd": 2.14,
    "sats_today": 1180,
    "sats_today_calibrated": true,
    "sats_today_note": "Modeled from live network difficulty and the current block subsidy at this hashrate — a statistical projection, not measured payout.",
    "network_difficulty": 500000000000000,
    "night_mode_active": false,
    "night_mode_starts_in_s": 7200,
    "hashrate_ghs": 95000,
    "fans": {
      "pwm": 30,
      "rpm": 3200,
      "max_rpm": 6000,
      "rpm_feedback_available": true
    }
  },
  "/api/home/presets": {
    "presets": [
      {
        "name": "eco",
        "display_name": "Eco",
        "watts": 1800,
        "wall_watts": 1850,
        "btu_h": 6300,
        "noise_db": 36,
        "estimated_noise_db_s9": 38,
        "noise_note": "Quiet for living spaces.",
        "hashrate_ths": 60,
        "description": "Low-power quiet heating."
      },
      {
        "name": "balanced",
        "display_name": "Balanced",
        "watts": 3100,
        "wall_watts": 3180,
        "btu_h": 10850,
        "noise_db": 42,
        "estimated_noise_db_s9": 44,
        "noise_note": "Good heat-to-noise ratio.",
        "hashrate_ths": 95,
        "description": "Balanced heat output and noise."
      },
      {
        "name": "max",
        "display_name": "Max Heat",
        "watts": 3400,
        "wall_watts": 3500,
        "btu_h": 11900,
        "noise_db": 50,
        "estimated_noise_db_s9": 52,
        "noise_note": "Maximum heat output.",
        "hashrate_ths": 104,
        "description": "Maximum heating for cold rooms."
      }
    ],
    "scope": {
      "kind": "chip_family",
      "family": "BM1362",
      "chip_type": "BM1362",
      "label": "S19j Pro",
      "universal": false
    }
  },
  "/api/home/history": {
    "history": [
      {
        "timestamp": 1717400000,
        "timestamp_s": 1717400000,
        "time": 1717400000,
        "ts": 1717400000,
        "hashrate_ghs": 94800,
        "hashrate_ths": 94.8,
        "temp_c": 61,
        "power_w": 3180,
        "value": 94800,
        "accepted": 1280,
        "rejected": 7,
        "sats": 1170,
        "room_temp_c": 21.4,
        "btu_h": 10840,
        "cost_usd": 2.1
      },
      {
        "timestamp": 1717400000,
        "timestamp_s": 1717400000,
        "time": 1717400000,
        "ts": 1717400000,
        "hashrate_ghs": 95200,
        "hashrate_ths": 95.2,
        "temp_c": 62,
        "power_w": 3184,
        "value": 95200,
        "accepted": 1284,
        "rejected": 7,
        "sats": 1180,
        "room_temp_c": 21.5,
        "btu_h": 10860,
        "cost_usd": 2.14
      }
    ]
  },
  "/api/home/night-mode": {
    "enabled": true,
    "start_hour": 22,
    "end_hour": 7,
    "max_fan_pwm": 30,
    "power_reduction_pct": 20,
    "active": false
  },
  "/api/heater": {
    "power_watts": 3100,
    "wall_watts": 3180,
    "btu_h": 10850,
    "source": "estimated",
    "calibrated": false,
    "calibration_multiplier": null,
    "targeting": {
      "active": false,
      "source": null,
      "mode": null,
      "preset": null,
      "schedule_label": null,
      "target_watts": null,
      "current_wall_watts": 3180,
      "delta_watts": null,
      "comparison": null
    },
    "noise_db": 42,
    "noise_source": "tach_estimate",
    "noise_note": "Estimated from fan tachometer RPM.",
    "airflow_cfm": 220,
    "preset": "balanced",
    "room_temp_c": 21.5,
    "cost_today_usd": 2.14,
    "sats_today": 1180,
    "sats_today_calibrated": true,
    "sats_today_note": "Modeled from live network difficulty and the current block subsidy at this hashrate — a statistical projection, not measured payout.",
    "network_difficulty": 500000000000000,
    "night_mode_active": false,
    "night_mode_starts_in_s": 7200,
    "hashrate_ghs": 95000,
    "fans": {
      "pwm": 30,
      "rpm": 3200,
      "max_rpm": 6000,
      "rpm_feedback_available": true
    }
  },
  "/api/offgrid/status": {
    "enabled": true,
    "zone": "normal",
    "state": "running",
    "bus_voltage_v": 51.2,
    "current_a": 62.1,
    "power_w": 3180,
    "battery_soc_pct": 84,
    "target_freq_mhz": 525,
    "freq_pct": 100,
    "voltage_rate_vps": 0,
    "uptime_battery_s": 18432,
    "energy_consumed_wh": 16280,
    "critical_v": 44,
    "low_v": 47,
    "high_v": 56,
    "full_v": 57.6,
    "sensor_source": "ina226",
    "has_current": true,
    "sensor_ok": true,
    "message": "Battery within normal operating band."
  },
  "/api/offgrid/config": {
    "source_profile": "solar_battery",
    "enabled": true,
    "battery_preset": "lifepo4_48v",
    "adc": {
      "type": "ina226",
      "i2c_bus": 1,
      "i2c_addr": 64,
      "shunt_mohm": 0.5,
      "voltage_divider": 16
    },
    "freq_step_mhz": 25,
    "min_frequency_mhz": 300,
    "loop_interval_ms": 1000,
    "custom_critical_v": null,
    "custom_low_v": null,
    "custom_high_v": null,
    "custom_full_v": null,
    "custom_recovery_v": null,
    "ready": true,
    "restart_required": false,
    "readiness_message": "Off-grid controller configured and ready."
  },
  "/api/offgrid/presets": {
    "presets": [
      {
        "id": "lifepo4_48v",
        "label": "LiFePO4 48V (16S)",
        "critical_v": 44,
        "low_v": 47,
        "normal_v": 51.2,
        "high_v": 56,
        "full_v": 57.6,
        "recovery_v": 50
      },
      {
        "id": "lead_acid_48v",
        "label": "Lead-Acid 48V",
        "critical_v": 42,
        "low_v": 46,
        "normal_v": 48,
        "high_v": 54,
        "full_v": 55.2,
        "recovery_v": 49
      }
    ]
  },
  "/api/solar/config": {
    "providerLiveBackend": true,
    "providerTelemetryBacked": true,
    "providerStage": "live",
    "providerStageReason": null,
    "recommendedProvider": "enphase",
    "providerBackendScope": "production",
    "acceptedPayloadShapes": [
      "enphase-v1"
    ],
    "enabled": true,
    "inverterBrand": "enphase",
    "apiEndpoint": "http://203.0.113.50/api/v1/production",
    "apiKey": "demo-key",
    "bridgeBaseUrl": "",
    "bridgeApiKey": "",
    "teslaGatewayHost": "",
    "teslaPassword": "",
    "solarOnlyMode": false,
    "baseLoadWatts": 800,
    "batteryThresholdPct": 40,
    "batteryWakeHysteresisPct": 5,
    "providerMaxSampleAgeMs": 60000,
    "providerFailureHysteresisSamples": 3,
    "hybridImportDeadbandWatts": 100,
    "manualProductionWatts": 0,
    "manualSiteLoadWatts": 0,
    "manualBatterySocPct": null
  },
  "/api/solar/status": {
    "providerLiveBackend": true,
    "providerTelemetryBacked": true,
    "providerStage": "live",
    "providerStageReason": null,
    "recommendedProvider": "enphase",
    "providerBackendScope": "production",
    "acceptedPayloadShapes": [
      "enphase-v1"
    ],
    "enabled": true,
    "provider": "enphase",
    "providerConfigured": true,
    "runtimeAdopted": true,
    "commissioningState": "telemetry_live",
    "sourceProfile": "solar_battery",
    "productionWatts": 5200,
    "consumptionWatts": 4100,
    "miningWatts": 3180,
    "netGridWatts": -1100,
    "solarSurplusWatts": 1100,
    "batterySocPct": 84,
    "connected": true,
    "transport": "http-json",
    "matchedFields": [
      "production",
      "consumption"
    ],
    "matched_fields": [
      "production",
      "consumption"
    ],
    "solarOnlyMode": false,
    "controlActive": true,
    "sleeping": false,
    "batteryFloorActive": false,
    "targetFreqMhz": 525,
    "action": "mining",
    "sampleAgeMs": 4200,
    "stale": false,
    "consecutiveFailures": 0,
    "lastSuccessMs": 1717400000000,
    "lastUpdateMs": 1717400000000,
    "message": "Solar telemetry live; surplus available."
  },
  "/api/solar/verification-history": {
    "generatedAtMs": 1717400000000,
    "entries": [
      {
        "timestampMs": 1717400000000,
        "provider": "enphase",
        "transport": "http-json",
        "connected": true,
        "sampleAgeMs": 4200,
        "stale": false,
        "consecutiveFailures": 0,
        "lastSuccessMs": 1717400000000,
        "matchedFields": [
          "production",
          "consumption"
        ],
        "matched_fields": [
          "production",
          "consumption"
        ],
        "productionWatts": 5200,
        "consumptionWatts": 4100,
        "netGridWatts": -1100,
        "batterySocPct": 84,
        "message": "Sample OK."
      }
    ]
  },
  "/api/donation/info": {
    "pool_url": "stratum+tcp://pool.d-central.tech:3333",
    "pool_host": "pool.d-central.tech",
    "worker": "DungeonMaster",
    "payout_address": "bc1qexampledcentraldonationpayoutaddress0000",
    "explorer_url": "https://mempool.space/address/bc1qexampledcentraldonationpayoutaddress0000",
    "explorer_name": "mempool.space",
    "verify_label": "Verify donation payouts on-chain",
    "trust_model": "Trust-but-verify: the donation slice is published to a public address you can audit.",
    "disclosure": "DCENT_OS forwards a voluntary 2% donation to D-Central's pool to fund open-source development."
  },
  "/api/hardware/pic_info": {
    "schema": "dcentos.hardware.pic_info.v1",
    "count": 1,
    "variants": [
      {
        "fw_byte": "0x89",
        "fw_byte_decimal": 137,
        "architecture": "dsPIC33",
        "wire_form": "framed",
        "reset_safe": true,
        "voltage_trusted": true,
        "label": "fw=0x89 FRAMED app mode"
      }
    ],
    "live_per_slot": null,
    "live_per_slot_note": "Live per-slot dsPIC firmware probe is not populated in this snapshot."
  },
  "/api/hardware/psu_catalog": {
    "schema": "dcentos.hardware.psu_catalog.v1",
    "count": 2,
    "models": [
      {
        "model": "APW12",
        "voltage_min_v": 12,
        "voltage_max_v": 15,
        "max_current_a": 226,
        "max_wattage_220v_w": 3360,
        "max_wattage_110v_w": 1800,
        "ac_input_min_v": 100,
        "ac_input_max_v": 264,
        "efficiency_pct": 93,
        "has_voltage_feedback": true,
        "label": "APW12 (smart PSU)",
        "compatible_miners": [
          "S19j Pro",
          "S19 Pro",
          "S21"
        ]
      },
      {
        "model": "APW3",
        "voltage_min_v": 12,
        "voltage_max_v": 12.8,
        "max_current_a": 133,
        "max_wattage_220v_w": 1600,
        "max_wattage_110v_w": 1200,
        "ac_input_min_v": 200,
        "ac_input_max_v": 240,
        "efficiency_pct": 85,
        "has_voltage_feedback": false,
        "label": "APW3++ (PSU bypass)",
        "compatible_miners": [
          "S9",
          "S17",
          "S19j Pro"
        ]
      }
    ]
  },
  "/api/competitive/readiness": {
    "schema": "dcentos.competitive.readiness.v1",
    "status": "proven",
    "read_only": true,
    "control_actions": false,
    "hardware_writes": false,
    "filesystem_mutation": false,
    "content_collected": false,
    "probe_performed": false,
    "handlers_executed": false,
    "telemetry_source": "config_snapshot",
    "source": "dcentrald",
    "generated_at_s": 1717400000,
    "fetched_at_ms": 1717400000000,
    "decentralization_gate": {
      "license_required": false,
      "license_server_required": false,
      "activation_required": false,
      "license_check_performed": false,
      "mandatory_fee": false,
      "fee_route": "none",
      "donation": {
        "default_enabled": true,
        "current_enabled": true,
        "default_percent": 2,
        "current_percent": 2,
        "cycle_duration_s_default": 3600,
        "current_cycle_duration_s": 3600,
        "pool_visible": true,
        "disable_supported": true,
        "donation_off_test_status": "passed",
        "current_state_source": "config"
      },
      "offline_behavior": "mines_normally_offline",
      "external_dependencies": [
        {
          "id": "pool",
          "purpose": "Stratum work source",
          "default_state": "configured",
          "required": "yes",
          "disable_impact": "no work"
        }
      ],
      "source_basis": [
        "clean_room"
      ],
      "repair_diagnostic": "read_only",
      "write_surfaces": [
        {
          "surface": "config",
          "default": "read_only",
          "write_gate": "auth",
          "audit_status": "audited"
        }
      ],
      "home_miner_safe": true,
      "home_miner_safe_status": "safe",
      "docs_link": "https://d-central.tech/docs",
      "docs_link_status": "live",
      "recovery_link": "https://d-central.tech/recovery",
      "recovery_link_status": "live"
    },
    "feature_count": 1,
    "features": [
      {
        "id": "pool_failover",
        "label": "User Pool Failover",
        "status": "proven",
        "priority": "high",
        "competitor_reference": "BraiinsOS",
        "home_miner_value": "Stays mining when a pool drops.",
        "current_behavior": "Automatic failover with stale-work flush.",
        "risk": "low",
        "clean_room_path": "Stratum V1 client",
        "acceptance_test": "mock failover suite",
        "source_basis": "clean_room",
        "telemetry_source": "stratum_runtime",
        "confidence": "high",
        "blockers": [],
        "docs_link": "https://d-central.tech/docs/failover",
        "recovery_link": "https://d-central.tech/recovery",
        "license_required": false,
        "mandatory_fee": false,
        "promotion_allowed": true,
        "decentralization": {
          "license_required": false,
          "mandatory_fee": false,
          "fee_route": "none",
          "offline_behavior": "mines_normally_offline",
          "source_basis": [
            "clean_room"
          ],
          "repair_diagnostic": "read_only",
          "home_miner_safe_status": "safe"
        }
      }
    ],
    "promotion_allowed_only_when": [
      "clean_room",
      "home_miner_safe"
    ],
    "limitations": []
  },
  "/api/competitive/manifest": {
    "status": "ok",
    "schema_version": 1,
    "read_only": true,
    "content_collected": false,
    "probe_performed": false,
    "handlers_executed": false,
    "surfaces": [
      {
        "id": "cgminer",
        "label": "CGMiner API",
        "protocol": "tcp-json",
        "default_port": 4028,
        "default_bind": "0.0.0.0",
        "compatibility": [
          "pyasic",
          "hass-miner"
        ],
        "routes": [
          {
            "method": "GET",
            "path": "/api/status",
            "support": "implemented",
            "mutates": false,
            "compatibility": [
              "pyasic"
            ],
            "provenance": "native",
            "unsupported_fields": [],
            "limitations": []
          }
        ],
        "commands": [
          {
            "name": "summary",
            "support": "implemented",
            "mutates": false,
            "provenance": "native",
            "limitations": []
          }
        ],
        "limitations": []
      }
    ],
    "omissions": [
      {
        "path": null,
        "surface": "grpc",
        "reason": "gRPC control surface not implemented."
      }
    ],
    "limitations": []
  },
  "/api/cgminer/catalog": {
    "schema": "dcentos.cgminer.catalog.v1",
    "count": 2,
    "total": 2,
    "set_count": 1,
    "get_count": 1,
    "luxor_extensions": 1,
    "destructive": 0,
    "commands": [
      {
        "name": "summary",
        "kind": "get",
        "luxor_extension": false,
        "destructive": false,
        "doc": "Returns a one-line mining summary."
      },
      {
        "name": "pools",
        "kind": "get",
        "luxor_extension": true,
        "destructive": false,
        "doc": "Lists configured pools."
      }
    ]
  },
  "/api/led/status": {
    "enabled": true,
    "current_pattern": "heartbeat",
    "locate_active": false,
    "locate_remaining_s": null,
    "night_mode_active": false
  },
  "/api/led/patterns": {
    "patterns": [
      {
        "id": "heartbeat",
        "name": "Heartbeat",
        "description": "Steady heartbeat blink.",
        "duration_s": 0
      }
    ],
    "locate_patterns": [
      {
        "id": "locate_blink",
        "name": "Locate Blink",
        "description": "Fast blink to find the unit.",
        "duration_s": 60
      }
    ],
    "background_patterns": [
      {
        "id": "breathing",
        "name": "Breathing",
        "description": "Slow breathing glow.",
        "duration_s": 0
      }
    ],
    "selected": "heartbeat",
    "locate_count": 1
  },
  "/api/led/config": {
    "enabled": true,
    "heartbeat_on_ms": 500,
    "heartbeat_off_ms": 1500,
    "locate_pattern": "locate_blink",
    "locate_duration_s": 60,
    "flash_on_accepted_share": true,
    "flash_on_rejected_share": false,
    "night_mode_disable": true,
    "celebration_on_lucky_share": true,
    "chain_status_blink_codes": true
  },
  "/api/boot/phase": {
    "phase": {
      "kind": "generic",
      "phase": "mining"
    },
    "started_at_unix_ms": 1717400000000,
    "is_live": true
  },
  "/api/boot/timeline": {
    "entries": [
      {
        "phase": {
          "kind": "generic",
          "phase": "booting"
        },
        "started_at_unix_ms": 1717400000000,
        "ended_at_unix_ms": 1717400000000
      },
      {
        "phase": {
          "kind": "generic",
          "phase": "starting"
        },
        "started_at_unix_ms": 1717400000000,
        "ended_at_unix_ms": 1717400000000
      },
      {
        "phase": {
          "kind": "generic",
          "phase": "mining"
        },
        "started_at_unix_ms": 1717400000000,
        "ended_at_unix_ms": null
      }
    ]
  },
  "/api/diagnostics/failure_modes": {
    "schema": "dcentos.diagnostics.failure_modes.v1",
    "count": 2,
    "modes": [
      {
        "mode": "chain_dead",
        "severity": "high",
        "recovery": "Reseat the hashboard ribbon and AC-cycle the unit."
      },
      {
        "mode": "psu_undervolt",
        "severity": "medium",
        "recovery": "Verify PSU model and rail voltage in PSU Override."
      }
    ]
  },
  "/api/diagnostics/recovery_actions": {
    "schema": "dcentos.diagnostics.recovery_actions.v1",
    "actions": [
      {
        "action": "restart_daemon",
        "is_destructive": false
      },
      {
        "action": "reboot",
        "is_destructive": false
      },
      {
        "action": "restore_to_stock",
        "is_destructive": true
      }
    ],
    "cgi_routes": [
      {
        "cgi": "get_system_info",
        "path": "/cgi-bin/get_system_info.cgi"
      }
    ],
    "log_groups_whitelist": [
      "mining",
      "system"
    ],
    "uninstall_steps": [
      "Stop dcentrald",
      "Restore stock firmware",
      "Reboot"
    ],
    "luxos_recovery_requires_auth": true,
    "note": "Recovery actions are read-only listings; destructive ones require explicit confirmation."
  },
  "/api/diagnostics/logs/manifest": {
    "status": "ok",
    "read_only": true,
    "content_collected": false,
    "sources": [
      {
        "id": "dcentrald",
        "label": "Mining daemon log",
        "path": "/tmp/dcentrald.log",
        "content_endpoint": "/api/debug/log",
        "content_access": "available",
        "metadata_status": "ok",
        "exists": true,
        "size_bytes": 184320,
        "modified_ms": 1717400000000,
        "limitations": []
      }
    ],
    "limitations": []
  },
  "/api/diagnostics/troubleshoot/psu": {
    "detected": true,
    "model": "APW3",
    "fw_version": "0x71",
    "transport": "bypass",
    "control_mode": "override",
    "output_enabled": true,
    "output_gate_enabled": true,
    "voltage_range": "12.0-12.8V",
    "voltage_in": 240,
    "voltage_out": 12.8,
    "current_a": 248,
    "power_w": 3180,
    "temp_c": 48,
    "supports_output_gate": true,
    "supports_voltage_set": false,
    "supports_watchdog": false,
    "message": "PSU operating in bypass/override mode; telemetry estimated."
  },
  "/api/diagnostics/troubleshoot/fpga": {
    "fpga_version": "0xB013",
    "build_id": "braiins-am2",
    "chains": [
      {
        "id": 6,
        "alive": true
      },
      {
        "id": 7,
        "alive": true
      },
      {
        "id": 8,
        "alive": true
      }
    ],
    "message": "FPGA chain UARTs responding on all chains."
  },
  "/api/diagnostics/troubleshoot/network": {
    "ethernet": {
      "mac": "02:00:00:00:00:25",
      "link_up": true
    },
    "dns_ok": true,
    "gateway_reachable": true,
    "pool_reachable": true,
    "ntp_synced": true,
    "message": "Network healthy; pool and gateway reachable."
  },
  "/api/diagnostics/shares/local_rejects": {
    "schema": "dcentos.diagnostics.local_rejects.v1",
    "ring_capacity": 256,
    "total_seen": 7,
    "returned": 1,
    "rejects": [
      {
        "seq": 7,
        "timestamp_ms": 1717400000000,
        "chain_id": 7,
        "chip_index": 12,
        "nonce": 305419896,
        "work_id": 42,
        "midstate_idx": 0,
        "fpga_work_id_raw": 42,
        "generation_age": 1,
        "computed_hash_be_first8": [
          0,
          0,
          0,
          1,
          35,
          69,
          103,
          137
        ],
        "share_target_be_first8": [
          0,
          0,
          0,
          0,
          255,
          255,
          255,
          255
        ],
        "reason": "high_hash"
      }
    ]
  },
  "/api/re/catalog/index": {
    "schema": "dcentos.re.catalog.index.v1",
    "read_only": true,
    "hardware_reads": false,
    "hardware_writes": false,
    "config_writes": false,
    "mining_control": false,
    "source_crate": "dcentrald-api",
    "base_path": "/api/re/catalog",
    "catalogs": [
      {
        "name": "psu_catalog",
        "path": "/api/hardware/psu_catalog",
        "description": "Known PSU models and rail envelopes."
      },
      {
        "name": "cgminer_catalog",
        "path": "/api/cgminer/catalog",
        "description": "Supported CGMiner API commands."
      }
    ]
  },
  "/api/system/restore-to-stock/status": {
    "state": "idle",
    "state_detail": {
      "phase": "idle"
    },
    "last_preflight_at_ms": null,
    "last_preflight_verdict": null,
    "last_backup_path": null,
    "last_scheduled_reboot_at_ms": null,
    "last_safety_findings": [],
    "last_active_slot": "1",
    "last_inactive_slot": "2",
    "transitions": 0,
    "last_transition_at_ms": null,
    "last_backup_fw_setenv_present": null,
    "recent_log_lines": []
  },
  "/api/system/restore-to-stock/preflight-checks": {
    "setsid_path": "/usr/bin/setsid",
    "revert_script_path": "/usr/sbin/revert_to_stock_am2.sh",
    "fw_setenv_path": "/usr/sbin/fw_setenv",
    "data_free_mib": 512,
    "tar_path": "/bin/tar",
    "nandwrite_path": "/usr/sbin/nandwrite",
    "flash_erase_path": "/usr/sbin/flash_erase",
    "platform_signature": "zynq-am2-bm1362",
    "platform_supported": true,
    "platform_verified_revertable": true,
    "all_present": true
  },
  "/api/fleet": {
    "generated_at_ms": 1717400000000,
    "miners": [
      {
        "id": "s19jpro-xil",
        "hostname": "dcentos-25",
        "ip": "192.0.2.25",
        "model": "S19j Pro",
        "hashrate_ghs": 95000,
        "temp_c": 62,
        "fan_pwm": 30,
        "status": "alive",
        "last_seen_ms": 1717400000000,
        "pool_target_difficulty": 512,
        "achieved_difficulty": 712
      }
    ],
    "source": "api",
    "source_label": "Local miner API",
    "demo": false,
    "message": "Fleet inventory live."
  }
};
