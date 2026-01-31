//! rstmdb-cli - Command-line interface for rstmdb
//!
//! Provides both a REPL and one-shot command execution.

mod commands;
mod repl;

use clap::{Parser, Subcommand};
use colored::Colorize;
use rstmdb_client::{Client, ConnectionConfig, TlsClientConfig};
use std::net::SocketAddr;
use std::path::PathBuf;
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(name = "rstmdb-cli")]
#[command(about = "Command-line interface for rstmdb state machine database")]
#[command(version)]
struct Cli {
    /// Server address
    #[arg(short, long, default_value = "127.0.0.1:7401")]
    server: SocketAddr,

    /// Authentication token
    #[arg(short = 't', long, env = "RSTMDB_TOKEN")]
    token: Option<String>,

    // ===== TLS Options =====
    /// Enable TLS connection
    #[arg(long, env = "RSTMDB_TLS")]
    tls: bool,

    /// Path to CA certificate for server verification
    #[arg(long, env = "RSTMDB_CA_CERT")]
    ca_cert: Option<PathBuf>,

    /// Path to client certificate (for mTLS)
    #[arg(long, env = "RSTMDB_CLIENT_CERT")]
    client_cert: Option<PathBuf>,

    /// Path to client private key (for mTLS)
    #[arg(long, env = "RSTMDB_CLIENT_KEY")]
    client_key: Option<PathBuf>,

    /// Skip server certificate verification (INSECURE)
    #[arg(long, short = 'k')]
    insecure: bool,

    /// Server name for TLS SNI (defaults to server hostname)
    #[arg(long)]
    server_name: Option<String>,

    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Start interactive REPL
    Repl,

    /// Ping the server
    Ping,

    /// Get server info
    Info,

    /// Register a machine definition
    PutMachine {
        /// Machine name
        #[arg(short, long)]
        name: String,

        /// Version number
        #[arg(short, long)]
        version: u32,

        /// Definition JSON (or @file.json to read from file)
        definition: String,
    },

    /// Get a machine definition
    GetMachine {
        /// Machine name
        #[arg(short, long)]
        name: String,

        /// Version number
        #[arg(short, long)]
        version: u32,
    },

    /// List all machines
    ListMachines,

    /// Create a new instance
    CreateInstance {
        /// Machine name
        #[arg(short, long)]
        machine: String,

        /// Version number
        #[arg(short = 'V', long)]
        version: u32,

        /// Instance ID (optional, auto-generated if not provided)
        #[arg(short, long)]
        id: Option<String>,

        /// Initial context JSON
        #[arg(short, long)]
        ctx: Option<String>,
    },

    /// Get an instance
    GetInstance {
        /// Instance ID
        id: String,
    },

    /// Apply an event to an instance
    ApplyEvent {
        /// Instance ID
        #[arg(short, long)]
        instance: String,

        /// Event name
        #[arg(short, long)]
        event: String,

        /// Event payload JSON
        #[arg(short, long)]
        payload: Option<String>,

        /// Expected state (for optimistic concurrency)
        #[arg(long)]
        expected_state: Option<String>,
    },

    /// Delete an instance
    DeleteInstance {
        /// Instance ID
        id: String,
    },

    /// Read WAL entries
    WalRead {
        /// Starting offset
        #[arg(short, long, default_value = "0")]
        from: u64,

        /// Maximum entries to return
        #[arg(short, long)]
        limit: Option<u64>,
    },

    /// Compact WAL by snapshotting instances and deleting old segments
    Compact {
        /// Force snapshot of all instances before compaction
        #[arg(short, long)]
        force: bool,
    },

    /// Generate SHA-256 hash of a token for config files
    HashToken {
        /// The token to hash
        token: String,
    },

    /// Watch a specific instance for state changes
    WatchInstance {
        /// Instance ID to watch
        id: String,

        /// Exclude context from events
        #[arg(long)]
        no_ctx: bool,
    },

    /// Watch all events across all instances
    WatchAll {
        /// Filter: only these machine types
        #[arg(long, value_delimiter = ',')]
        machines: Option<Vec<String>>,

        /// Filter: only events FROM these states
        #[arg(long, value_delimiter = ',')]
        from_states: Option<Vec<String>>,

        /// Filter: only events TO these states
        #[arg(long, value_delimiter = ',')]
        to_states: Option<Vec<String>>,

        /// Filter: only these event types
        #[arg(long, value_delimiter = ',')]
        events: Option<Vec<String>>,

        /// Exclude context from events
        #[arg(long)]
        no_ctx: bool,
    },

    /// Unsubscribe from a watch
    Unwatch {
        /// Subscription ID to cancel
        subscription_id: String,
    },
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize logging
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn")),
        )
        .init();

    let cli = Cli::parse();

    // Handle hash-token command locally (no server connection needed)
    if let Some(Commands::HashToken { token }) = &cli.command {
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        hasher.update(token.as_bytes());
        let hash = hex::encode(hasher.finalize());
        println!("{}", hash);
        return Ok(());
    }

    // Build TLS config if any TLS option is set
    let tls_config =
        if cli.tls || cli.ca_cert.is_some() || cli.client_cert.is_some() || cli.insecure {
            let mut tls = TlsClientConfig::new();
            tls.enabled = true;

            if let Some(ref path) = cli.ca_cert {
                tls.ca_cert_path = Some(path.clone());
            }
            if let (Some(cert), Some(key)) = (&cli.client_cert, &cli.client_key) {
                tls.client_cert_path = Some(cert.clone());
                tls.client_key_path = Some(key.clone());
            } else if cli.client_cert.is_some() || cli.client_key.is_some() {
                eprintln!(
                    "{}: --client-cert and --client-key must be used together",
                    "Error".red()
                );
                std::process::exit(1);
            }
            tls.insecure = cli.insecure;
            tls.server_name = cli.server_name.clone();

            Some(tls)
        } else {
            None
        };

    // Create client with optional auth token and TLS
    let mut config = ConnectionConfig::new(cli.server).with_client_name("rstmdb-cli");
    if let Some(ref token) = cli.token {
        config = config.with_auth_token(token);
    }
    if let Some(tls) = tls_config {
        config = config.with_tls(tls);
    }
    let client = Client::new(config);

    // Handle commands
    match cli.command {
        Some(Commands::Repl) | None => {
            // Start REPL (pass token for reconnects)
            repl::run(client, cli.server, cli.token).await?;
        }
        Some(Commands::HashToken { .. }) => unreachable!(), // Already handled above
        Some(Commands::WatchInstance { id, no_ctx }) => {
            // Watch instance - streams events until Ctrl+C
            client.connect().await.map_err(|e| {
                eprintln!("{}: {}", "Connection failed".red(), e);
                e
            })?;

            // Subscribe to stream events BEFORE starting read loop
            let mut event_rx = client.connection().subscribe_stream_events();

            // Spawn read loop in background
            let conn = client.connection();
            tokio::spawn(async move {
                let _ = conn.read_loop().await;
            });

            tokio::task::yield_now().await;

            // Start watch
            match client.watch_instance(&id, !no_ctx).await {
                Ok(result) => {
                    eprintln!(
                        "{} instance {} (sub_id: {}, state: {}, wal_offset: {})",
                        "Watching".green(),
                        id.cyan(),
                        result.subscription_id,
                        result.current_state.yellow(),
                        result.current_wal_offset
                    );
                    eprintln!("{}", "Press Ctrl+C to stop...".dimmed());

                    // Stream events until interrupted
                    loop {
                        tokio::select! {
                            event = event_rx.recv() => {
                                match event {
                                    Ok(e) => {
                                        let json = serde_json::to_string(&e).unwrap();
                                        println!("{}", json);
                                    }
                                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                                        eprintln!("{}: lagged {} events", "Warning".yellow(), n);
                                    }
                                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                                        eprintln!("{}", "Connection closed".red());
                                        break;
                                    }
                                }
                            }
                            _ = tokio::signal::ctrl_c() => {
                                eprintln!("\n{}", "Stopping watch...".dimmed());
                                let _ = client.unwatch(&result.subscription_id).await;
                                break;
                            }
                        }
                    }
                }
                Err(e) => {
                    eprintln!("{}: {}", "Error".red(), e);
                    std::process::exit(1);
                }
            }

            client.close().await?;
        }
        Some(Commands::WatchAll {
            machines,
            from_states,
            to_states,
            events,
            no_ctx,
        }) => {
            // Watch all - streams events until Ctrl+C
            client.connect().await.map_err(|e| {
                eprintln!("{}: {}", "Connection failed".red(), e);
                e
            })?;

            // Subscribe to stream events BEFORE starting read loop
            let mut event_rx = client.connection().subscribe_stream_events();

            // Spawn read loop in background
            let conn = client.connection();
            tokio::spawn(async move {
                let _ = conn.read_loop().await;
            });

            tokio::task::yield_now().await;

            // Start watch
            match client
                .watch_all(machines, from_states, to_states, events, !no_ctx)
                .await
            {
                Ok(result) => {
                    eprintln!(
                        "{} all events (sub_id: {}, wal_offset: {})",
                        "Watching".green(),
                        result.subscription_id,
                        result.wal_offset
                    );
                    eprintln!("{}", "Press Ctrl+C to stop...".dimmed());

                    // Stream events until interrupted
                    loop {
                        tokio::select! {
                            event = event_rx.recv() => {
                                match event {
                                    Ok(e) => {
                                        let json = serde_json::to_string(&e).unwrap();
                                        println!("{}", json);
                                    }
                                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                                        eprintln!("{}: lagged {} events", "Warning".yellow(), n);
                                    }
                                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                                        eprintln!("{}", "Connection closed".red());
                                        break;
                                    }
                                }
                            }
                            _ = tokio::signal::ctrl_c() => {
                                eprintln!("\n{}", "Stopping watch...".dimmed());
                                let _ = client.unwatch(&result.subscription_id).await;
                                break;
                            }
                        }
                    }
                }
                Err(e) => {
                    eprintln!("{}: {}", "Error".red(), e);
                    std::process::exit(1);
                }
            }

            client.close().await?;
        }
        Some(cmd) => {
            // Connect for one-shot command
            client.connect().await.map_err(|e| {
                eprintln!("{}: {}", "Connection failed".red(), e);
                e
            })?;

            // Spawn read loop in background
            let conn = client.connection();
            tokio::spawn(async move {
                let _ = conn.read_loop().await;
            });

            // Give read_loop a chance to start
            tokio::task::yield_now().await;

            // Execute command
            let result = commands::execute(&client, cmd).await;

            match result {
                Ok(output) => {
                    println!("{}", output);
                }
                Err(e) => {
                    eprintln!("{}: {}", "Error".red(), e);
                    std::process::exit(1);
                }
            }

            client.close().await?;
        }
    }

    Ok(())
}
