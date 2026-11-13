//! `rc-server`: the rendezvous and relay server.
//!
//!     rc-server [--listen ADDR:PORT] [--data-dir DIR]
//!
//! Environment: `RC_LOG` sets the log filter (default `info`).
//!
//! Stops cleanly on Ctrl-C, and on Unix on SIGTERM.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::ExitCode;

use rc_crypto::Identity;
use rc_server::{Config, Limits, Server};

const USAGE: &str = "\
rc-server: rendezvous and relay server for Remote Desk

Usage: rc-server [--listen ADDR:PORT] [--data-dir DIR]

  --listen ADDR:PORT  address to listen on (default 0.0.0.0:21117)
  --data-dir DIR      where the server's key is kept (default ./data)
  --help              show this text

The first run creates the server's key in DIR/server.key and prints its
fingerprint. Give the fingerprint to the people who use this server; their
devices refuse a server whose key does not match it. Keep DIR private: the
key file is not encrypted.";

struct Args {
    listen: SocketAddr,
    data_dir: PathBuf,
}

fn parse_args() -> Result<Option<Args>, String> {
    let mut args = Args {
        listen: SocketAddr::from(([0, 0, 0, 0], 21_117)),
        data_dir: PathBuf::from("data"),
    };
    let mut iter = std::env::args().skip(1);
    while let Some(flag) = iter.next() {
        match flag.as_str() {
            "--help" | "-h" => return Ok(None),
            "--listen" => {
                let value = iter.next().ok_or("--listen needs a value")?;
                args.listen = value
                    .parse()
                    .map_err(|_| format!("`{value}` is not an address like 0.0.0.0:21117"))?;
            }
            "--data-dir" => {
                args.data_dir = iter.next().ok_or("--data-dir needs a value")?.into();
            }
            other => return Err(format!("unknown option `{other}`")),
        }
    }
    Ok(Some(args))
}

#[tokio::main]
async fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(Some(args)) => args,
        Ok(None) => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Err(e) => {
            eprintln!("rc-server: {e}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };

    let filter = tracing_subscriber::EnvFilter::try_from_env("RC_LOG")
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();

    let identity = match Identity::load_or_create_unprotected(&args.data_dir.join("server.key")) {
        Ok(identity) => identity,
        Err(e) => {
            eprintln!("rc-server: {e}");
            return ExitCode::FAILURE;
        }
    };
    let server = match Server::start(Config {
        listen: args.listen,
        identity,
        limits: Limits::default(),
    })
    .await
    {
        Ok(server) => server,
        Err(e) => {
            eprintln!("rc-server: cannot listen on {}: {e}", args.listen);
            return ExitCode::FAILURE;
        }
    };
    println!("Server key: {}", server.fingerprint());
    println!("Listening on {}", server.local_addr());

    if let Err(e) = stop_requested().await {
        eprintln!("rc-server: cannot wait for a stop signal: {e}");
    }
    tracing::info!("shutting down");
    server.shutdown().await;
    ExitCode::SUCCESS
}

/// Waits until the server is asked to stop: Ctrl-C, or on Unix the
/// termination signal that service managers and containers send.
async fn stop_requested() -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut terminate = signal(SignalKind::terminate())?;
        tokio::select! {
            result = tokio::signal::ctrl_c() => result,
            _ = terminate.recv() => Ok(()),
        }
    }
    #[cfg(not(unix))]
    tokio::signal::ctrl_c().await
}
