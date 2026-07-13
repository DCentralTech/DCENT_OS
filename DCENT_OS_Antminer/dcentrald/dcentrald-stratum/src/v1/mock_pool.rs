//! Reusable host-only Stratum V1 loopback pool.
//!
//! Compiled only with `mock-pool`. It completes configure/subscribe/authorize,
//! publishes a configurable difficulty and deterministic job, and positively
//! acknowledges every `mining.submit` while counting accepted shares.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;

#[derive(Debug, Clone)]
pub struct MockV1PoolConfig {
    pub difficulty: f64,
    pub job_id: String,
    pub version_mask: String,
    pub ntime: String,
}

impl Default for MockV1PoolConfig {
    fn default() -> Self {
        Self {
            // Difficulty 1 keeps deterministic simulator nonces practical.
            difficulty: 1.0,
            job_id: "dcent-sim-job-1".to_string(),
            version_mask: "1fffe000".to_string(),
            ntime: "66112233".to_string(),
        }
    }
}

pub struct MockV1Pool;

pub struct MockV1PoolHandle {
    accepted: Arc<AtomicU64>,
    requests: Arc<Mutex<Vec<String>>>,
    task: JoinHandle<()>,
}

impl MockV1PoolHandle {
    pub fn accepted_shares(&self) -> u64 {
        self.accepted.load(Ordering::SeqCst)
    }

    pub fn requests(&self) -> Vec<String> {
        self.requests
            .lock()
            .map(|requests| requests.clone())
            .unwrap_or_default()
    }

    pub fn url(addr: SocketAddr) -> String {
        format!("stratum+tcp://{addr}")
    }
}

impl Drop for MockV1PoolHandle {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl MockV1Pool {
    pub async fn spawn() -> std::io::Result<(SocketAddr, MockV1PoolHandle)> {
        Self::spawn_with_config(MockV1PoolConfig::default()).await
    }

    pub async fn spawn_with_config(
        config: MockV1PoolConfig,
    ) -> std::io::Result<(SocketAddr, MockV1PoolHandle)> {
        if !config.difficulty.is_finite() || config.difficulty <= 0.0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "mock V1 difficulty must be finite and positive",
            ));
        }
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let addr = listener.local_addr()?;
        let accepted = Arc::new(AtomicU64::new(0));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let accepted_for_task = Arc::clone(&accepted);
        let requests_for_task = Arc::clone(&requests);

        let task = tokio::spawn(async move {
            while let Ok((stream, _peer)) = listener.accept().await {
                let config = config.clone();
                let accepted = Arc::clone(&accepted_for_task);
                let requests = Arc::clone(&requests_for_task);
                tokio::spawn(async move {
                    let _ = serve_connection(stream, config, accepted, requests).await;
                });
            }
        });

        Ok((
            addr,
            MockV1PoolHandle {
                accepted,
                requests,
                task,
            },
        ))
    }
}

async fn write_json_line(
    writer: &mut tokio::net::tcp::OwnedWriteHalf,
    value: Value,
) -> std::io::Result<()> {
    writer.write_all(value.to_string().as_bytes()).await?;
    writer.write_all(b"\n").await?;
    writer.flush().await
}

fn response(id: Value, result: Value) -> Value {
    json!({"id": id, "result": result, "error": Value::Null})
}

fn set_difficulty(difficulty: f64) -> Value {
    json!({"id": Value::Null, "method": "mining.set_difficulty", "params": [difficulty]})
}

fn notify(config: &MockV1PoolConfig) -> Value {
    json!({
        "id": Value::Null,
        "method": "mining.notify",
        "params": [
            config.job_id,
            "00".repeat(32),
            "01000000",
            "ffffffff",
            [],
            "20000000",
            "1d00ffff",
            config.ntime,
            true
        ]
    })
}

async fn serve_connection(
    stream: TcpStream,
    config: MockV1PoolConfig,
    accepted: Arc<AtomicU64>,
    requests: Arc<Mutex<Vec<String>>>,
) -> std::io::Result<()> {
    let (reader, mut writer) = stream.into_split();
    let mut lines = BufReader::new(reader).lines();
    while let Some(line) = lines.next_line().await? {
        if let Ok(mut captured) = requests.lock() {
            captured.push(line.clone());
        }
        let Ok(message) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let id = message.get("id").cloned().unwrap_or(Value::Null);
        let method = message
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or_default();
        match method {
            "mining.configure" => {
                write_json_line(
                    &mut writer,
                    response(
                        id,
                        json!({
                            "version-rolling": true,
                            "version-rolling.mask": config.version_mask,
                        }),
                    ),
                )
                .await?;
            }
            "mining.subscribe" => {
                write_json_line(&mut writer, response(id, json!([[], "deadbeef", 4]))).await?;
            }
            "mining.authorize" => {
                write_json_line(&mut writer, response(id, Value::Bool(true))).await?;
                write_json_line(&mut writer, set_difficulty(config.difficulty)).await?;
                write_json_line(&mut writer, notify(&config)).await?;
            }
            "mining.suggest_difficulty" => {
                write_json_line(&mut writer, response(id, Value::Bool(true))).await?;
                write_json_line(&mut writer, set_difficulty(config.difficulty)).await?;
            }
            "mining.submit" => {
                accepted.fetch_add(1, Ordering::SeqCst);
                write_json_line(&mut writer, response(id, Value::Bool(true))).await?;
            }
            _ if !id.is_null() => {
                write_json_line(&mut writer, response(id, Value::Bool(true))).await?;
            }
            _ => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn accepts_submit_and_counts_it() {
        let (addr, handle) = MockV1Pool::spawn().await.expect("spawn mock V1 pool");
        let stream = TcpStream::connect(addr)
            .await
            .expect("connect mock V1 pool");
        let (reader, mut writer) = stream.into_split();
        let mut lines = BufReader::new(reader).lines();

        for request in [
            json!({"id": 1, "method": "mining.configure", "params": []}),
            json!({"id": 2, "method": "mining.subscribe", "params": []}),
            json!({"id": 3, "method": "mining.authorize", "params": ["worker", "x"]}),
            json!({"id": 4, "method": "mining.submit", "params": ["worker", "dcent-sim-job-1", "00000000", "66112233", "00000001"]}),
        ] {
            writer
                .write_all(format!("{request}\n").as_bytes())
                .await
                .expect("write request");
        }

        let mut saw_submit_accept = false;
        for _ in 0..7 {
            let line = tokio::time::timeout(std::time::Duration::from_secs(1), lines.next_line())
                .await
                .expect("pool response timeout")
                .expect("read pool response")
                .expect("pool response line");
            let value: Value = serde_json::from_str(&line).expect("response JSON");
            if value.get("id") == Some(&json!(4)) && value.get("result") == Some(&Value::Bool(true))
            {
                saw_submit_accept = true;
                break;
            }
        }
        assert!(saw_submit_accept);
        assert_eq!(handle.accepted_shares(), 1);
        assert!(handle
            .requests()
            .iter()
            .any(|request| request.contains("mining.submit")));
    }

    #[tokio::test]
    async fn rejects_invalid_difficulty_before_binding() {
        let config = MockV1PoolConfig {
            difficulty: 0.0,
            ..MockV1PoolConfig::default()
        };
        assert!(MockV1Pool::spawn_with_config(config).await.is_err());
    }
}
