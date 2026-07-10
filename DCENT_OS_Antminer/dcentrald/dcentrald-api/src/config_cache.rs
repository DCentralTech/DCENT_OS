//! P3-2 (Omega §6/§9-P3): in-memory mirror of the persisted daemon config
//! table (`dcentrald.toml`).
//!
//! Before this, a set of read-only REST status handlers each re-read and
//! re-parsed the whole TOML config file from disk on every request
//! (`read_config_table_or_default` / `std::fs::read_to_string(get_config_path())`),
//! adding per-request disk I/O and a config-drift risk between handlers that
//! read at slightly different instants.
//!
//! [`ConfigTableCache`] holds the most-recently-loaded parse of the config file
//! and reloads only when the file actually changed since the last load. Change
//! is detected by two complementary signals captured in a [`ConfigFingerprint`]:
//!
//!   1. The `atomic_io` **config-write generation** — bumped deterministically
//!      every time a handler persists config through `atomic_write` (the single
//!      funnel every API config write goes through). This is the load-bearing
//!      freshness contract: a GET issued after a POST that persisted config
//!      observes the new value because the persist bumped the generation, so the
//!      next snapshot reloads. It does NOT depend on filesystem timestamp
//!      resolution.
//!   2. The file mtime + length — a best-effort catch for out-of-band edits
//!      (operator hand-edit / toolbox SCP) that do not go through `atomic_write`.
//!      A same-second, same-length out-of-band edit can be missed — exactly as a
//!      restart-required hand-edit already would be — and it never affects the
//!      API-write contract in (1).
//!
//! On a warm hit, `snapshot()` is a cheap clone of the cached `toml::Table` with
//! zero disk I/O. The cache lives on `AppState` so each daemon instance owns its
//! view; the pure [`ConfigTableCache::snapshot_with`] core is host-testable with
//! injected fingerprints + loaders (no disk needed).
//!
//! IMPORTANT: this cache backs only handlers that read STATIC/startup config for
//! display. Read-modify-write paths still load from disk via
//! `load_config_table_for_write()` (they need the exact on-disk bytes plus the
//! schema-preservation round-trip) and must not be routed through this cache.

use std::sync::RwLock;
use std::time::SystemTime;

/// Cheap identity of the on-disk config file at a point in time. A change in any
/// field means the file may differ from the cached parse and the cache reloads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfigFingerprint {
    /// `atomic_io` config-write generation (the API-persist counter).
    pub generation: u64,
    /// File mtime, if `stat` succeeded.
    pub mtime: Option<SystemTime>,
    /// File length in bytes (0 when `stat` failed / the file is absent).
    pub len: u64,
}

/// In-memory, post-write-fresh mirror of the persisted config table.
#[derive(Default)]
pub struct ConfigTableCache {
    inner: RwLock<Option<(ConfigFingerprint, toml::Table)>>,
}

impl ConfigTableCache {
    /// Construct an empty cache (first `snapshot()` loads from disk).
    pub fn new() -> Self {
        Self {
            inner: RwLock::new(None),
        }
    }

    /// Compute the current fingerprint of the active config file.
    fn current_fingerprint(path: &str) -> ConfigFingerprint {
        let generation = crate::atomic_io::config_write_generation();
        let (mtime, len) = match std::fs::metadata(path) {
            Ok(meta) => (meta.modified().ok(), meta.len()),
            Err(_) => (None, 0),
        };
        ConfigFingerprint {
            generation,
            mtime,
            len,
        }
    }

    /// Cheap, post-write-fresh clone of the persisted config table. Reloads from
    /// disk only when the active config file changed since the last load. Read-
    /// only semantics identical to `rest::read_config_table_or_default` (active
    /// path `/data` then `/etc`; empty table on missing/unparseable).
    pub fn snapshot(&self) -> toml::Table {
        let path = crate::rest::get_config_path();
        let fp = Self::current_fingerprint(path);
        self.snapshot_with(fp, crate::rest::read_config_table_or_default)
    }

    /// Dependency-injected core (host-testable, no disk): return the cached
    /// table when `fp` matches the last load, otherwise call `load`, store the
    /// result under `fp`, and return it.
    pub fn snapshot_with<F>(&self, fp: ConfigFingerprint, load: F) -> toml::Table
    where
        F: FnOnce() -> toml::Table,
    {
        if let Ok(guard) = self.inner.read() {
            if let Some((cached_fp, table)) = guard.as_ref() {
                if *cached_fp == fp {
                    return table.clone();
                }
            }
        }
        let table = load();
        if let Ok(mut guard) = self.inner.write() {
            *guard = Some((fp, table.clone()));
        }
        table
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table_with_rate(rate: f64) -> toml::Table {
        let mut home = toml::Table::new();
        home.insert("electricity_rate".into(), toml::Value::Float(rate));
        let mut t = toml::Table::new();
        t.insert("home".into(), toml::Value::Table(home));
        t
    }

    fn rate_of(t: &toml::Table) -> f64 {
        t.get("home")
            .and_then(|v| v.as_table())
            .and_then(|h| h.get("electricity_rate"))
            .and_then(|v| v.as_float())
            .unwrap()
    }

    // Proves BOTH P3-2 contract halves on the pure cache core:
    //  (a) a warm hit returns the AppState-cached config WITHOUT reloading, and
    //  (b) a config write (modeled as a generation bump in the fingerprint) is
    //      observed on the next read (freshness where it matters).
    #[test]
    fn warm_hit_returns_cached_then_reloads_after_a_write() {
        let cache = ConfigTableCache::new();
        let loads = std::cell::Cell::new(0u32);

        let fp1 = ConfigFingerprint {
            generation: 7,
            mtime: None,
            len: 10,
        };

        // (1) cold: loads + caches 0.20.
        let t1 = cache.snapshot_with(fp1.clone(), || {
            loads.set(loads.get() + 1);
            table_with_rate(0.20)
        });
        assert_eq!(loads.get(), 1);
        assert_eq!(rate_of(&t1), 0.20);

        // (2) identical fingerprint => warm hit: loader NOT called, returns the
        //     cached table (0.20), NOT the loader's would-be 0.99.
        let t2 = cache.snapshot_with(fp1.clone(), || {
            loads.set(loads.get() + 1);
            table_with_rate(0.99)
        });
        assert_eq!(loads.get(), 1, "warm hit must not reload from disk");
        assert_eq!(rate_of(&t2), 0.20, "warm hit must return the cached config");

        // (3) a persisted config write bumps the generation => fingerprint
        //     changes => reload => the newly-written value (0.30) is observed.
        let fp2 = ConfigFingerprint {
            generation: 8,
            mtime: None,
            len: 12,
        };
        let t3 = cache.snapshot_with(fp2, || {
            loads.set(loads.get() + 1);
            table_with_rate(0.30)
        });
        assert_eq!(loads.get(), 2, "a generation bump must trigger a reload");
        assert_eq!(
            rate_of(&t3),
            0.30,
            "a read after a config write must observe the written value"
        );
    }

    // An mtime/length change (out-of-band edit) also invalidates the cache.
    #[test]
    fn metadata_change_reloads_even_without_generation_bump() {
        let cache = ConfigTableCache::new();
        let loads = std::cell::Cell::new(0u32);

        let fp1 = ConfigFingerprint {
            generation: 1,
            mtime: Some(SystemTime::UNIX_EPOCH),
            len: 10,
        };
        let _ = cache.snapshot_with(fp1, || {
            loads.set(loads.get() + 1);
            table_with_rate(0.20)
        });
        assert_eq!(loads.get(), 1);

        // Same generation, but length changed (a hand-edit) => reload.
        let fp2 = ConfigFingerprint {
            generation: 1,
            mtime: Some(SystemTime::UNIX_EPOCH),
            len: 11,
        };
        let t = cache.snapshot_with(fp2, || {
            loads.set(loads.get() + 1);
            table_with_rate(0.40)
        });
        assert_eq!(loads.get(), 2);
        assert_eq!(rate_of(&t), 0.40);
    }
}
