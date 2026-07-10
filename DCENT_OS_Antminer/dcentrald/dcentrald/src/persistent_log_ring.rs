//! Persistent bounded log ring for dcentrald.
//!
//! The runtime logger can tee formatted tracing output into two fixed-size
//! files. This preserves recent logs across daemon restarts without unbounded
//! growth. The default directory is tmpfs-oriented to avoid write amplification
//! on miner flash; set `DCENTOS_LOG_RING_DIR` to opt into a persistent path.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

pub const LOG_RING_DIR_ENV: &str = "DCENTOS_LOG_RING_DIR";
pub const LOG_RING_DISABLE_ENV: &str = "DCENTOS_LOG_RING_DISABLE";
pub const DEFAULT_LOG_RING_FILE_COUNT: usize = 2;
pub const DEFAULT_LOG_RING_FILE_SIZE_BYTES: u64 = 5 * 1024 * 1024;
const CURSOR_FILE: &str = "dcentrald-log-ring.cursor.json";

#[derive(Debug, Clone)]
pub struct LogRingConfig {
    pub dir: PathBuf,
    pub file_count: usize,
    pub file_size_bytes: u64,
}

impl LogRingConfig {
    pub fn default_runtime() -> Self {
        Self {
            dir: default_log_ring_dir(),
            file_count: DEFAULT_LOG_RING_FILE_COUNT,
            file_size_bytes: DEFAULT_LOG_RING_FILE_SIZE_BYTES,
        }
    }

    fn ring_path(&self, index: usize) -> PathBuf {
        self.dir.join(format!("dcentrald-log-ring{index}.log"))
    }

    fn cursor_path(&self) -> PathBuf {
        self.dir.join(CURSOR_FILE)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct LogRingCursor {
    active_index: usize,
    offsets: Vec<u64>,
    sequence: u64,
}

impl LogRingCursor {
    fn fresh(file_count: usize) -> Self {
        Self {
            active_index: 0,
            offsets: vec![0; file_count],
            sequence: 0,
        }
    }

    fn sanitize(mut self, file_count: usize, file_size_bytes: u64) -> Self {
        if file_count == 0 {
            return Self::fresh(1);
        }
        if self.active_index >= file_count {
            self.active_index = 0;
        }
        self.offsets.resize(file_count, 0);
        self.offsets.truncate(file_count);
        for offset in &mut self.offsets {
            *offset = (*offset).min(file_size_bytes);
        }
        self
    }
}

#[derive(Debug)]
pub struct PersistentLogRing {
    config: LogRingConfig,
    cursor: LogRingCursor,
}

impl PersistentLogRing {
    pub fn open_default() -> Result<Self> {
        Self::open(LogRingConfig::default_runtime())
    }

    pub fn open(config: LogRingConfig) -> Result<Self> {
        anyhow::ensure!(
            config.file_count > 0,
            "log ring file_count must be non-zero"
        );
        anyhow::ensure!(
            config.file_size_bytes > 0,
            "log ring file_size_bytes must be non-zero"
        );

        fs::create_dir_all(&config.dir)
            .with_context(|| format!("creating log ring dir {}", config.dir.display()))?;

        for index in 0..config.file_count {
            let path = config.ring_path(index);
            let file = OpenOptions::new()
                .create(true)
                .read(true)
                .truncate(false)
                .write(true)
                .open(&path)
                .with_context(|| format!("opening log ring file {}", path.display()))?;
            file.set_len(config.file_size_bytes)
                .with_context(|| format!("preallocating log ring file {}", path.display()))?;
        }

        let cursor =
            read_cursor(&config).unwrap_or_else(|| LogRingCursor::fresh(config.file_count));
        let cursor = cursor.sanitize(config.file_count, config.file_size_bytes);
        let mut ring = Self { config, cursor };
        ring.persist_cursor()?;
        Ok(ring)
    }

    pub fn append_bytes(&mut self, bytes: &[u8]) -> Result<()> {
        if bytes.is_empty() {
            return Ok(());
        }

        let payload = if bytes.len() as u64 > self.config.file_size_bytes {
            &bytes[(bytes.len() - self.config.file_size_bytes as usize)..]
        } else {
            bytes
        };

        if self.cursor.offsets[self.cursor.active_index] + payload.len() as u64
            > self.config.file_size_bytes
        {
            self.advance_file();
        }

        let offset = self.cursor.offsets[self.cursor.active_index];
        let path = self.config.ring_path(self.cursor.active_index);
        let mut file = OpenOptions::new()
            .write(true)
            .open(&path)
            .with_context(|| format!("opening log ring file {}", path.display()))?;
        file.seek(SeekFrom::Start(offset))
            .with_context(|| format!("seeking log ring file {}", path.display()))?;
        file.write_all(payload)
            .with_context(|| format!("writing log ring file {}", path.display()))?;
        file.flush()
            .with_context(|| format!("flushing log ring file {}", path.display()))?;

        self.cursor.offsets[self.cursor.active_index] = offset + payload.len() as u64;
        self.cursor.sequence = self.cursor.sequence.saturating_add(1);
        self.persist_cursor()
    }

    pub fn read_recent_lossy(&self, max_bytes: usize) -> Result<Vec<u8>> {
        if max_bytes == 0 {
            return Ok(Vec::new());
        }

        let mut output = Vec::new();
        for index in self.chronological_indices() {
            let len = self.cursor.offsets[index] as usize;
            if len == 0 {
                continue;
            }
            let path = self.config.ring_path(index);
            let mut file = File::open(&path)
                .with_context(|| format!("opening log ring file {}", path.display()))?;
            let mut buf = vec![0; len];
            file.read_exact(&mut buf)
                .with_context(|| format!("reading log ring file {}", path.display()))?;
            output.extend(buf);
            if output.len() > max_bytes {
                let drain = output.len() - max_bytes;
                output.drain(0..drain);
            }
        }
        Ok(output)
    }

    pub fn file_paths(&self) -> Vec<PathBuf> {
        (0..self.config.file_count)
            .map(|index| self.config.ring_path(index))
            .collect()
    }

    fn advance_file(&mut self) {
        self.cursor.active_index = (self.cursor.active_index + 1) % self.config.file_count;
        self.cursor.offsets[self.cursor.active_index] = 0;
    }

    fn chronological_indices(&self) -> Vec<usize> {
        let mut indices = Vec::with_capacity(self.config.file_count);
        for step in 1..=self.config.file_count {
            indices.push((self.cursor.active_index + step) % self.config.file_count);
        }
        indices
    }

    fn persist_cursor(&mut self) -> Result<()> {
        let path = self.config.cursor_path();
        let temp = path.with_extension("json.tmp");
        let body = serde_json::to_vec(&self.cursor).context("serializing log ring cursor")?;
        fs::write(&temp, body).with_context(|| format!("writing {}", temp.display()))?;
        let _ = fs::remove_file(&path);
        fs::rename(&temp, &path)
            .with_context(|| format!("committing log ring cursor {}", path.display()))?;
        Ok(())
    }
}

pub fn default_log_ring_dir() -> PathBuf {
    if let Ok(dir) = std::env::var(LOG_RING_DIR_ENV) {
        return PathBuf::from(dir);
    }
    PathBuf::from("/tmp/dcent/log")
}

pub fn log_ring_disabled_by_env() -> bool {
    std::env::var(LOG_RING_DISABLE_ENV)
        .map(|value| {
            matches!(
                value.as_str(),
                "1" | "true" | "TRUE" | "yes" | "YES" | "on" | "ON"
            )
        })
        .unwrap_or(false)
}

fn read_cursor(config: &LogRingConfig) -> Option<LogRingCursor> {
    let body = fs::read(config.cursor_path()).ok()?;
    serde_json::from_slice(&body).ok()
}

#[derive(Clone)]
pub struct RingTeeMakeWriter {
    ring: Arc<Mutex<PersistentLogRing>>,
}

impl RingTeeMakeWriter {
    pub fn new(ring: PersistentLogRing) -> Self {
        Self {
            ring: Arc::new(Mutex::new(ring)),
        }
    }
}

pub struct RingTeeWriter {
    ring: Arc<Mutex<PersistentLogRing>>,
    stdout: std::io::Stdout,
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for RingTeeMakeWriter {
    type Writer = RingTeeWriter;

    fn make_writer(&'a self) -> Self::Writer {
        RingTeeWriter {
            ring: self.ring.clone(),
            stdout: std::io::stdout(),
        }
    }
}

impl Write for RingTeeWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.stdout.write_all(buf)?;
        if let Ok(mut ring) = self.ring.lock() {
            let _ = ring.append_bytes(buf);
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.stdout.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_dir(name: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        dir.push(format!("dcentos-{name}-{unique}"));
        dir
    }

    fn test_config(name: &str, size: u64) -> LogRingConfig {
        LogRingConfig {
            dir: temp_dir(name),
            file_count: 2,
            file_size_bytes: size,
        }
    }

    #[test]
    fn log_ring_preallocates_fixed_size_files() {
        let config = test_config("prealloc", 128);
        let ring = PersistentLogRing::open(config.clone()).unwrap();

        for path in ring.file_paths() {
            assert_eq!(fs::metadata(path).unwrap().len(), 128);
        }
        assert!(config.cursor_path().exists());
    }

    #[test]
    fn log_ring_wraps_without_file_growth() {
        let config = test_config("wrap", 32);
        let mut ring = PersistentLogRing::open(config.clone()).unwrap();

        ring.append_bytes(b"first-line\n").unwrap();
        ring.append_bytes(b"second-line-with-more-bytes\n").unwrap();
        ring.append_bytes(b"third-line\n").unwrap();

        for path in ring.file_paths() {
            assert_eq!(fs::metadata(path).unwrap().len(), 32);
        }

        let recent = String::from_utf8_lossy(&ring.read_recent_lossy(128).unwrap()).to_string();
        assert!(recent.contains("second-line") || recent.contains("third-line"));
        assert!(recent.contains("third-line"));
    }

    #[test]
    fn log_ring_keeps_tail_of_oversized_record() {
        let config = test_config("oversized", 16);
        let mut ring = PersistentLogRing::open(config).unwrap();

        ring.append_bytes(b"0123456789abcdefTAIL").unwrap();

        let recent = ring.read_recent_lossy(64).unwrap();
        assert_eq!(String::from_utf8_lossy(&recent), "456789abcdefTAIL");
    }

    #[test]
    fn log_ring_restores_cursor_after_reopen() {
        let config = test_config("reopen", 64);
        {
            let mut ring = PersistentLogRing::open(config.clone()).unwrap();
            ring.append_bytes(b"before\n").unwrap();
        }

        let mut reopened = PersistentLogRing::open(config).unwrap();
        reopened.append_bytes(b"after\n").unwrap();
        let recent = String::from_utf8_lossy(&reopened.read_recent_lossy(64).unwrap()).to_string();
        assert!(recent.contains("before"));
        assert!(recent.contains("after"));
    }
}
