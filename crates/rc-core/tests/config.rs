use std::fs;

use rc_core::config::{Config, ConfigError, LogLevel, TransportPolicy, CONFIG_FILE_NAME};

fn invalid_field(result: Result<(), ConfigError>) -> &'static str {
    match result {
        Err(ConfigError::Invalid { field, .. }) => field,
        other => panic!("expected an invalid-field error, got {other:?}"),
    }
}

#[test]
fn defaults_are_valid_and_secure() {
    let config = Config::default();
    config.validate().expect("defaults must validate");

    assert!(!config.security.allow_input);
    assert!(!config.security.allow_clipboard);
    assert!(!config.security.allow_file_transfer);
    assert!(config.security.approval_timeout_secs > 0);
    assert!(config.network.rendezvous_servers.is_empty());
    assert!(config.network.relay_servers.is_empty());
}

#[test]
fn load_or_create_writes_defaults() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("nested").join(CONFIG_FILE_NAME);

    let created = Config::load_or_create(&path).unwrap();
    assert!(path.is_file());
    assert_eq!(created, Config::default());
    assert_eq!(Config::load(&path).unwrap(), created);
}

#[test]
fn save_then_load_round_trips() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(CONFIG_FILE_NAME);

    let mut config = Config::default();
    config.device.name = "Workshop PC".into();
    config.network.rendezvous_servers = vec!["rv.example.org:443".into()];
    config.network.relay_servers = vec!["[2001:db8::1]:443".into()];
    config.network.direct_port = 47000;
    config.network.lan_discovery = false;
    config.network.transport = TransportPolicy::RelayOnly;
    config.security.approval_timeout_secs = 60;
    config.security.allow_input = true;
    config.security.allow_clipboard = true;
    config.security.allow_file_transfer = true;
    config.security.idle_timeout_minutes = 0;
    config.video.max_fps = 60;
    config.video.max_bitrate_kbps = 20_000;
    config.video.adaptive_quality = false;
    config.logging.level = LogLevel::Debug;
    config.logging.file_enabled = false;
    config.logging.retention_days = 3;

    config.save(&path).unwrap();
    assert_eq!(Config::load(&path).unwrap(), config);
}

#[test]
fn partial_file_uses_defaults() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(CONFIG_FILE_NAME);
    fs::write(&path, "[video]\nmax_fps = 15\n").unwrap();

    let config = Config::load(&path).unwrap();
    assert_eq!(config.video.max_fps, 15);

    let mut expected = Config::default();
    expected.video.max_fps = 15;
    assert_eq!(config, expected);
}

#[test]
fn rejects_bad_device_names() {
    for name in ["", "   ", "line\nbreak", &"x".repeat(65)] {
        let mut config = Config::default();
        config.device.name = name.to_owned();
        assert_eq!(invalid_field(config.validate()), "device.name", "{name:?}");
    }
}

#[test]
fn rejects_bad_server_addresses() {
    for entry in [
        "example.org",
        ":443",
        "example.org:0",
        "example.org:99999",
        "bad host:443",
    ] {
        let mut config = Config::default();
        config.network.rendezvous_servers = vec![entry.to_owned()];
        assert_eq!(
            invalid_field(config.validate()),
            "network.rendezvous_servers",
            "{entry:?}"
        );

        let mut config = Config::default();
        config.network.relay_servers = vec![entry.to_owned()];
        assert_eq!(
            invalid_field(config.validate()),
            "network.relay_servers",
            "{entry:?}"
        );
    }
}

#[test]
fn rejects_relay_only_without_servers() {
    let mut config = Config::default();
    config.network.transport = TransportPolicy::RelayOnly;
    assert_eq!(invalid_field(config.validate()), "network.transport");
}

#[test]
fn rejects_out_of_range_numbers() {
    let cases: [(&str, fn(&mut Config)); 6] = [
        ("security.approval_timeout_secs", |c| {
            c.security.approval_timeout_secs = 0
        }),
        ("security.idle_timeout_minutes", |c| {
            c.security.idle_timeout_minutes = 100_000
        }),
        ("video.max_fps", |c| c.video.max_fps = 0),
        ("video.max_fps", |c| c.video.max_fps = 1000),
        ("video.max_bitrate_kbps", |c| c.video.max_bitrate_kbps = 1),
        ("logging.retention_days", |c| c.logging.retention_days = 0),
    ];
    for (field, mutate) in cases {
        let mut config = Config::default();
        mutate(&mut config);
        assert_eq!(invalid_field(config.validate()), field);
    }
}

#[test]
fn invalid_config_is_never_saved() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(CONFIG_FILE_NAME);

    let mut config = Config::default();
    config.video.max_fps = 0;
    assert!(matches!(
        config.save(&path),
        Err(ConfigError::Invalid { .. })
    ));
    assert!(!path.exists());
}

#[test]
fn invalid_file_is_rejected_on_load() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(CONFIG_FILE_NAME);
    fs::write(&path, "[video]\nmax_fps = 0\n").unwrap();

    assert!(matches!(
        Config::load_or_create(&path),
        Err(ConfigError::Invalid {
            field: "video.max_fps",
            ..
        })
    ));
}

#[test]
fn malformed_file_is_not_overwritten() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(CONFIG_FILE_NAME);
    let garbage = "this is = not [valid toml";
    fs::write(&path, garbage).unwrap();

    assert!(matches!(
        Config::load_or_create(&path),
        Err(ConfigError::Parse { .. })
    ));
    assert_eq!(fs::read_to_string(&path).unwrap(), garbage);
}

#[test]
fn save_is_atomic() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(CONFIG_FILE_NAME);

    Config::default().save(&path).unwrap();
    let mut config = Config::default();
    config.video.max_fps = 24;
    config.save(&path).unwrap();

    let names: Vec<_> = fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, vec![CONFIG_FILE_NAME.to_owned()]);
    assert_eq!(Config::load(&path).unwrap().video.max_fps, 24);
}
