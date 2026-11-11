//! One test only: the subscriber is process-global and can be installed once.

use std::fs;

use rc_core::config::LoggingConfig;
use rc_core::logging::{self, LOG_FILE_PREFIX};

#[test]
fn file_logging_writes_json_lines() {
    let dir = tempfile::tempdir().unwrap();
    let log_dir = dir.path().join("logs");

    let guard = logging::init(&LoggingConfig::default(), &log_dir).unwrap();
    tracing::info!(session = 7, "logging smoke test");
    drop(guard);

    let files: Vec<_> = fs::read_dir(&log_dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    assert_eq!(files.len(), 1, "expected one log file, found {files:?}");
    let name = files[0].file_name().unwrap().to_string_lossy().into_owned();
    assert!(
        name.starts_with(LOG_FILE_PREFIX) && name.ends_with(".log"),
        "{name}"
    );

    let text = fs::read_to_string(&files[0]).unwrap();
    assert!(text.contains("logging smoke test"), "{text}");
    assert!(text.contains("\"session\":7"), "{text}");
}
