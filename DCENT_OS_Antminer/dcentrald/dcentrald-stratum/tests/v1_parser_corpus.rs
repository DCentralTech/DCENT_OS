use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use dcentrald_stratum::v1::messages::{parse_pool_message, PoolMessage};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct CorpusCase {
    name: String,
    input: String,
    expect: ExpectedMessage,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "variant", rename_all = "snake_case")]
enum ExpectedMessage {
    Notify {
        job_id: String,
        clean_jobs: bool,
    },
    SetDifficulty {
        value: f64,
    },
    SetExtranonce {
        extranonce1: String,
        extranonce2_size: usize,
    },
    SetVersionMask {
        mask: String,
    },
    Ping {
        id: u64,
    },
    Reconnect {
        host: String,
        port: u16,
        wait_seconds: u32,
    },
    GetVersion {
        id: u64,
    },
    ShowMessage {
        contains: String,
    },
    Response {
        id: u64,
    },
    Unknown {
        contains: String,
    },
    ParseError,
}

#[test]
fn v1_pool_message_parser_replays_persistent_corpus() {
    let corpus_files = corpus_files();
    assert!(
        !corpus_files.is_empty(),
        "V1 parser corpus directory must contain JSON fixtures"
    );

    let mut names = BTreeSet::new();
    let mut case_count = 0usize;

    for file in corpus_files {
        let raw = fs::read_to_string(&file)
            .unwrap_or_else(|err| panic!("failed to read {}: {err}", file.display()));
        let cases: Vec<CorpusCase> = serde_json::from_str(&raw)
            .unwrap_or_else(|err| panic!("failed to decode {}: {err}", file.display()));
        assert!(!cases.is_empty(), "{} has no cases", file.display());

        for case in cases {
            let unique_name = format!("{}:{}", file.display(), case.name);
            assert!(
                names.insert(unique_name.clone()),
                "duplicate case {unique_name}"
            );
            assert_case_matches(&unique_name, &case.input, &case.expect);
            case_count += 1;
        }
    }

    assert!(
        case_count >= 16,
        "V1 parser corpus should cover typed and fail-soft parser paths"
    );
}

fn corpus_files() -> Vec<PathBuf> {
    let corpus_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("v1_parser_corpus");
    let mut files: Vec<PathBuf> = fs::read_dir(&corpus_dir)
        .unwrap_or_else(|err| panic!("failed to open {}: {err}", corpus_dir.display()))
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .collect();
    files.sort();
    files
}

fn assert_case_matches(case_name: &str, input: &str, expected: &ExpectedMessage) {
    match expected {
        ExpectedMessage::ParseError => {
            assert!(
                parse_pool_message(input).is_err(),
                "{case_name}: expected malformed JSON to return an error"
            );
        }
        ExpectedMessage::Notify { job_id, clean_jobs } => {
            let parsed = parse_ok(case_name, input);
            match parsed {
                PoolMessage::Notify {
                    job_id: actual_job_id,
                    clean_jobs: actual_clean_jobs,
                    ..
                } => {
                    assert_eq!(&actual_job_id, job_id, "{case_name}: job_id");
                    assert_eq!(&actual_clean_jobs, clean_jobs, "{case_name}: clean_jobs");
                }
                other => panic!("{case_name}: expected Notify, got {other:?}"),
            }
        }
        ExpectedMessage::SetDifficulty { value } => {
            let parsed = parse_ok(case_name, input);
            match parsed {
                PoolMessage::SetDifficulty(actual) => {
                    assert!(
                        (actual - value).abs() < f64::EPSILON,
                        "{case_name}: expected difficulty {value}, got {actual}"
                    );
                }
                other => panic!("{case_name}: expected SetDifficulty, got {other:?}"),
            }
        }
        ExpectedMessage::SetExtranonce {
            extranonce1,
            extranonce2_size,
        } => {
            let parsed = parse_ok(case_name, input);
            match parsed {
                PoolMessage::SetExtranonce {
                    extranonce1: actual_extranonce1,
                    extranonce2_size: actual_extranonce2_size,
                } => {
                    assert_eq!(&actual_extranonce1, extranonce1, "{case_name}: extranonce1");
                    assert_eq!(
                        &actual_extranonce2_size, extranonce2_size,
                        "{case_name}: extranonce2_size"
                    );
                }
                other => panic!("{case_name}: expected SetExtranonce, got {other:?}"),
            }
        }
        ExpectedMessage::SetVersionMask { mask } => {
            let parsed = parse_ok(case_name, input);
            match parsed {
                PoolMessage::SetVersionMask(actual) => {
                    assert_eq!(&actual, mask, "{case_name}: version mask");
                }
                other => panic!("{case_name}: expected SetVersionMask, got {other:?}"),
            }
        }
        ExpectedMessage::Ping { id } => {
            let parsed = parse_ok(case_name, input);
            match parsed {
                PoolMessage::Ping(actual) => assert_eq!(&actual, id, "{case_name}: ping id"),
                other => panic!("{case_name}: expected Ping, got {other:?}"),
            }
        }
        ExpectedMessage::Reconnect {
            host,
            port,
            wait_seconds,
        } => {
            let parsed = parse_ok(case_name, input);
            match parsed {
                PoolMessage::Reconnect {
                    host: actual_host,
                    port: actual_port,
                    wait_seconds: actual_wait_seconds,
                } => {
                    assert_eq!(&actual_host, host, "{case_name}: reconnect host");
                    assert_eq!(&actual_port, port, "{case_name}: reconnect port");
                    assert_eq!(
                        &actual_wait_seconds, wait_seconds,
                        "{case_name}: reconnect wait_seconds"
                    );
                }
                other => panic!("{case_name}: expected Reconnect, got {other:?}"),
            }
        }
        ExpectedMessage::GetVersion { id } => {
            let parsed = parse_ok(case_name, input);
            match parsed {
                PoolMessage::GetVersion(actual) => {
                    assert_eq!(&actual, id, "{case_name}: get_version id");
                }
                other => panic!("{case_name}: expected GetVersion, got {other:?}"),
            }
        }
        ExpectedMessage::ShowMessage { contains } => {
            let parsed = parse_ok(case_name, input);
            match parsed {
                PoolMessage::ShowMessage(actual) => {
                    assert!(
                        actual.contains(contains),
                        "{case_name}: show_message {actual:?} missing {contains:?}"
                    );
                }
                other => panic!("{case_name}: expected ShowMessage, got {other:?}"),
            }
        }
        ExpectedMessage::Response { id } => {
            let parsed = parse_ok(case_name, input);
            match parsed {
                PoolMessage::Response { id: actual, .. } => {
                    assert_eq!(&actual, id, "{case_name}: response id");
                }
                other => panic!("{case_name}: expected Response, got {other:?}"),
            }
        }
        ExpectedMessage::Unknown { contains } => {
            let parsed = parse_ok(case_name, input);
            match parsed {
                PoolMessage::Unknown(actual) => {
                    assert!(
                        actual.contains(contains),
                        "{case_name}: unknown diagnostic {actual:?} missing {contains:?}"
                    );
                }
                other => panic!("{case_name}: expected Unknown, got {other:?}"),
            }
        }
    }
}

fn parse_ok(case_name: &str, input: &str) -> PoolMessage {
    parse_pool_message(input)
        .unwrap_or_else(|err| panic!("{case_name}: expected JSON to parse: {err}"))
}
