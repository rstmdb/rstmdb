//! Server configuration.
//!
//! Configuration is loaded in the following order (later overrides earlier):
//! 1. Default values
//! 2. YAML config file (if specified via RSTMDB_CONFIG or --config)
//! 3. Environment variables

use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Server configuration.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Network configuration.
    pub network: NetworkConfig,
    /// Storage configuration.
    pub storage: StorageConfig,
    /// Compaction configuration.
    pub compaction: CompactionConfig,
    /// Authentication configuration.
    pub auth: AuthConfig,
    /// TLS configuration.
    pub tls: TlsConfig,
    /// Metrics configuration.
    pub metrics: MetricsConfig,
    /// Replication configuration.
    pub replication: ReplicationConfig,
}

impl Config {
    /// Loads configuration from file, then applies environment variable overrides.
    pub fn load() -> Result<Self, ConfigError> {
        // Start with defaults
        let mut config = Self::default();

        // Load from file if specified
        if let Ok(path) = std::env::var("RSTMDB_CONFIG") {
            config = Self::from_file(&path)?;
        }

        // Apply environment variable overrides
        config.apply_env_overrides();

        Ok(config)
    }

    /// Loads configuration from a YAML file.
    pub fn from_file(path: impl AsRef<Path>) -> Result<Self, ConfigError> {
        let path = path.as_ref();
        let content = std::fs::read_to_string(path)
            .map_err(|e| ConfigError::IoError(path.to_path_buf(), e))?;
        let config: Config = serde_yaml::from_str(&content)
            .map_err(|e| ConfigError::ParseError(path.to_path_buf(), e.to_string()))?;
        Ok(config)
    }

    /// Loads configuration from environment variables only.
    pub fn from_env() -> Self {
        let mut config = Self::default();
        config.apply_env_overrides();
        config
    }

    /// Validates cross-section constraints that individual section validators
    /// can't see on their own. Call this at startup after loading and applying
    /// env overrides; a returned error means the server must refuse to start.
    pub fn validate(&self) -> Result<(), ConfigError> {
        // FLUSH_ALL wipes all state, but it is NOT a replicated operation: on a
        // primary it would clear the primary while leaving replicas holding the
        // old data (silent divergence), and on a replica it's a local-only write
        // that breaks WAL parity with the primary. Refuse to start rather than
        // ship a data-loss / split-brain footgun — even when explicitly enabled.
        if !self.replication.is_standalone() && self.storage.allow_flush_all {
            return Err(ConfigError::ValidationError(format!(
                "storage.allow_flush_all=true is not permitted when replication is enabled \
                 (replication.role={:?}). FLUSH_ALL does not replicate and would diverge the \
                 cluster; set storage.allow_flush_all=false, or run replication.role=standalone.",
                self.replication.role
            )));
        }
        Ok(())
    }

    /// Applies environment variable overrides to the configuration.
    fn apply_env_overrides(&mut self) {
        self.network.apply_env_overrides();
        self.storage.apply_env_overrides();
        self.compaction.apply_env_overrides();
        self.auth.apply_env_overrides();
        self.tls.apply_env_overrides();
        self.metrics.apply_env_overrides();
        self.replication.apply_env_overrides();
    }

    /// Loads secrets from external file if configured.
    pub fn load_secrets(&mut self) -> Result<(), ConfigError> {
        self.auth.load_secrets()
    }

    /// Saves configuration to a YAML file.
    pub fn save(&self, path: impl AsRef<Path>) -> Result<(), ConfigError> {
        let path = path.as_ref();
        let content = serde_yaml::to_string(self)
            .map_err(|e| ConfigError::ParseError(path.to_path_buf(), e.to_string()))?;
        std::fs::write(path, content).map_err(|e| ConfigError::IoError(path.to_path_buf(), e))?;
        Ok(())
    }
}

/// Network configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct NetworkConfig {
    /// Address to bind to.
    #[serde(with = "socket_addr_serde")]
    pub bind_addr: SocketAddr,
    /// Idle connection timeout in seconds.
    pub idle_timeout_secs: u64,
    /// Maximum concurrent connections.
    pub max_connections: usize,
}

impl Default for NetworkConfig {
    fn default() -> Self {
        Self {
            bind_addr: "127.0.0.1:7401".parse().unwrap(),
            idle_timeout_secs: 300,
            max_connections: 1000,
        }
    }
}

impl NetworkConfig {
    fn apply_env_overrides(&mut self) {
        if let Ok(addr) = std::env::var("RSTMDB_BIND") {
            if let Ok(parsed) = addr.parse() {
                self.bind_addr = parsed;
            }
        }

        if let Ok(timeout) = std::env::var("RSTMDB_IDLE_TIMEOUT") {
            if let Ok(secs) = timeout.parse() {
                self.idle_timeout_secs = secs;
            }
        }

        if let Ok(max) = std::env::var("RSTMDB_MAX_CONNECTIONS") {
            if let Ok(n) = max.parse() {
                self.max_connections = n;
            }
        }
    }

    /// Returns idle timeout as Duration.
    pub fn idle_timeout(&self) -> Duration {
        Duration::from_secs(self.idle_timeout_secs)
    }
}

/// Authentication configuration.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct AuthConfig {
    /// Whether authentication is required for commands.
    #[serde(default)]
    pub required: bool,
    /// List of valid token hashes (SHA-256 hex strings).
    /// Generate hashes with: `rstmdb-cli hash-token <your-token>`
    #[serde(default)]
    pub token_hashes: Vec<String>,
    /// Optional path to external secrets file containing token hashes (one per line).
    #[serde(default)]
    pub secrets_file: Option<PathBuf>,
}

impl AuthConfig {
    fn apply_env_overrides(&mut self) {
        if let Ok(auth) = std::env::var("RSTMDB_AUTH_REQUIRED") {
            self.required = auth == "1" || auth.to_lowercase() == "true";
        }

        if let Ok(hash) = std::env::var("RSTMDB_AUTH_TOKEN_HASH") {
            if !hash.is_empty() {
                self.token_hashes.push(hash);
            }
        }

        if let Ok(path) = std::env::var("RSTMDB_AUTH_SECRETS_FILE") {
            self.secrets_file = Some(PathBuf::from(path));
        }
    }

    /// Loads token hashes from the secrets file if configured.
    pub fn load_secrets(&mut self) -> Result<(), ConfigError> {
        if let Some(ref path) = self.secrets_file {
            let content =
                std::fs::read_to_string(path).map_err(|e| ConfigError::IoError(path.clone(), e))?;
            for line in content.lines() {
                let line = line.trim();
                // Skip empty lines and comments
                if !line.is_empty() && !line.starts_with('#') {
                    self.token_hashes.push(line.to_string());
                }
            }
        }
        Ok(())
    }

    /// Returns whether authentication is effectively disabled.
    pub fn is_disabled(&self) -> bool {
        !self.required
    }
}

/// Storage configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct StorageConfig {
    /// Data directory.
    pub data_dir: PathBuf,
    /// WAL segment size in megabytes.
    pub wal_segment_size_mb: u64,
    /// Fsync policy.
    pub fsync_policy: FsyncPolicy,
    /// Maximum number of versions per machine (0 = unlimited).
    pub max_machine_versions: u32,
    /// Allow re-creating instances after deletion.
    pub allow_instance_recreate: bool,
    /// Allow the FLUSH_ALL operation to clear all data.
    pub allow_flush_all: bool,
}

/// Fsync policy for WAL writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FsyncPolicy {
    /// Fsync after every write (safest, slowest).
    EveryWrite,
    /// Fsync after N writes.
    EveryN(u32),
    /// Fsync after N milliseconds.
    EveryMs(u32),
    /// Never fsync, rely on OS (fastest, least safe).
    Never,
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            data_dir: PathBuf::from("./data"),
            wal_segment_size_mb: 64,
            fsync_policy: FsyncPolicy::EveryWrite,
            max_machine_versions: 0, // unlimited
            allow_instance_recreate: true,
            allow_flush_all: false,
        }
    }
}

impl StorageConfig {
    fn apply_env_overrides(&mut self) {
        if let Ok(dir) = std::env::var("RSTMDB_DATA") {
            self.data_dir = PathBuf::from(dir);
        }

        if let Ok(size) = std::env::var("RSTMDB_WAL_SEGMENT_SIZE_MB") {
            if let Ok(mb) = size.parse() {
                self.wal_segment_size_mb = mb;
            }
        }

        if let Ok(policy) = std::env::var("RSTMDB_FSYNC_POLICY") {
            self.fsync_policy = match policy.to_lowercase().as_str() {
                "every_write" | "everywrite" => FsyncPolicy::EveryWrite,
                "never" => FsyncPolicy::Never,
                s if s.starts_with("every_n:") => {
                    let n = s[8..].parse().unwrap_or(100);
                    FsyncPolicy::EveryN(n)
                }
                s if s.starts_with("every_ms:") => {
                    let ms = s[9..].parse().unwrap_or(100);
                    FsyncPolicy::EveryMs(ms)
                }
                _ => FsyncPolicy::EveryWrite,
            };
        }

        if let Ok(max) = std::env::var("RSTMDB_MAX_MACHINE_VERSIONS") {
            if let Ok(n) = max.parse() {
                self.max_machine_versions = n;
            }
        }

        if let Ok(val) = std::env::var("RSTMDB_ALLOW_INSTANCE_RECREATE") {
            self.allow_instance_recreate = val == "1" || val.to_lowercase() == "true";
        }

        if let Ok(val) = std::env::var("RSTMDB_ALLOW_FLUSH_ALL") {
            self.allow_flush_all = val == "1" || val.to_lowercase() == "true";
        }
    }

    /// Returns the WAL segment size in bytes.
    pub fn wal_segment_size(&self) -> u64 {
        self.wal_segment_size_mb * 1024 * 1024
    }

    /// Returns the WAL directory path.
    pub fn wal_dir(&self) -> PathBuf {
        self.data_dir.join("wal")
    }

    /// Returns the snapshots directory path.
    pub fn snapshots_dir(&self) -> PathBuf {
        self.data_dir.join("snapshots")
    }
}

/// Compaction configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CompactionConfig {
    /// Enable automatic compaction.
    pub enabled: bool,
    /// Compact after this many events (0 = disabled).
    pub events_threshold: u64,
    /// Compact when WAL exceeds this size in megabytes (0 = disabled).
    pub size_threshold_mb: u64,
    /// Minimum interval between auto-compactions in seconds.
    pub min_interval_secs: u64,
}

impl Default for CompactionConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            events_threshold: 10000,
            size_threshold_mb: 100,
            min_interval_secs: 60,
        }
    }
}

impl CompactionConfig {
    fn apply_env_overrides(&mut self) {
        if let Ok(enabled) = std::env::var("RSTMDB_COMPACT_ENABLED") {
            self.enabled = enabled == "1" || enabled.to_lowercase() == "true";
        }

        if let Ok(events) = std::env::var("RSTMDB_COMPACT_EVENTS") {
            if let Ok(n) = events.parse() {
                self.events_threshold = n;
            }
        }

        if let Ok(size) = std::env::var("RSTMDB_COMPACT_SIZE_MB") {
            if let Ok(mb) = size.parse() {
                self.size_threshold_mb = mb;
            }
        }

        if let Ok(interval) = std::env::var("RSTMDB_COMPACT_INTERVAL") {
            if let Ok(secs) = interval.parse() {
                self.min_interval_secs = secs;
            }
        }
    }

    /// Returns the size threshold in bytes.
    pub fn size_threshold(&self) -> u64 {
        self.size_threshold_mb * 1024 * 1024
    }

    /// Returns the minimum interval as Duration.
    pub fn min_interval(&self) -> Duration {
        Duration::from_secs(self.min_interval_secs)
    }

    /// Returns whether compaction should be disabled.
    pub fn is_disabled(&self) -> bool {
        !self.enabled || (self.events_threshold == 0 && self.size_threshold_mb == 0)
    }
}

/// TLS configuration.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct TlsConfig {
    /// Enable TLS.
    #[serde(default)]
    pub enabled: bool,
    /// Path to PEM-encoded server certificate file.
    #[serde(default)]
    pub cert_path: Option<PathBuf>,
    /// Path to PEM-encoded private key file.
    #[serde(default)]
    pub key_path: Option<PathBuf>,
    /// Require client certificate authentication (mTLS).
    #[serde(default)]
    pub require_client_cert: bool,
    /// Path to PEM-encoded CA certificate(s) for verifying client certs.
    /// Required if require_client_cert is true.
    #[serde(default)]
    pub client_ca_path: Option<PathBuf>,
}

impl TlsConfig {
    fn apply_env_overrides(&mut self) {
        if let Ok(enabled) = std::env::var("RSTMDB_TLS_ENABLED") {
            self.enabled = enabled == "1" || enabled.to_lowercase() == "true";
        }
        if let Ok(path) = std::env::var("RSTMDB_TLS_CERT") {
            self.cert_path = Some(PathBuf::from(path));
        }
        if let Ok(path) = std::env::var("RSTMDB_TLS_KEY") {
            self.key_path = Some(PathBuf::from(path));
        }
        if let Ok(require) = std::env::var("RSTMDB_TLS_REQUIRE_CLIENT_CERT") {
            self.require_client_cert = require == "1" || require.to_lowercase() == "true";
        }
        if let Ok(path) = std::env::var("RSTMDB_TLS_CLIENT_CA") {
            self.client_ca_path = Some(PathBuf::from(path));
        }
    }

    /// Validates TLS configuration.
    pub fn validate(&self) -> Result<(), ConfigError> {
        if !self.enabled {
            return Ok(());
        }

        if self.cert_path.is_none() {
            return Err(ConfigError::ValidationError(
                "TLS enabled but cert_path not set".to_string(),
            ));
        }
        if self.key_path.is_none() {
            return Err(ConfigError::ValidationError(
                "TLS enabled but key_path not set".to_string(),
            ));
        }
        if self.require_client_cert && self.client_ca_path.is_none() {
            return Err(ConfigError::ValidationError(
                "mTLS enabled but client_ca_path not set".to_string(),
            ));
        }

        Ok(())
    }
}

/// Metrics configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct MetricsConfig {
    /// Enable metrics HTTP server.
    #[serde(default)]
    pub enabled: bool,
    /// Address to bind the metrics server to.
    #[serde(with = "socket_addr_serde")]
    pub bind_addr: SocketAddr,
}

impl Default for MetricsConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            bind_addr: "0.0.0.0:9090".parse().unwrap(),
        }
    }
}

impl MetricsConfig {
    fn apply_env_overrides(&mut self) {
        if let Ok(enabled) = std::env::var("RSTMDB_METRICS_ENABLED") {
            self.enabled = enabled == "1" || enabled.to_lowercase() == "true";
        }
        if let Ok(addr) = std::env::var("RSTMDB_METRICS_BIND") {
            if let Ok(parsed) = addr.parse() {
                self.bind_addr = parsed;
            }
        }
    }
}

/// Replication role.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReplicationRole {
    /// Standalone server (no replication).
    #[default]
    Standalone,
    /// Primary server (accepts writes, streams to replicas).
    Primary,
    /// Replica server (read-only, receives stream from primary).
    Replica,
}

/// Replication mode.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReplicationMode {
    /// Async: primary streams entries in background, writes return immediately.
    #[default]
    Async,
    /// Sync: primary waits for ACKs from replicas before returning write response.
    Sync,
}

/// Replication configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ReplicationConfig {
    /// Replication role.
    pub role: ReplicationRole,
    /// Replication mode (async or sync).
    pub mode: ReplicationMode,
    /// Upstream primary address (replica only).
    pub upstream: Option<String>,
    /// Minimum number of replica ACKs required before responding (sync mode).
    pub sync_replicas: u32,
    /// Timeout in milliseconds for waiting for sync ACKs.
    pub sync_timeout_ms: u64,
    /// Alerting threshold: max lag in seconds before warning.
    pub max_lag_seconds: u64,
    /// Alerting threshold: max lag in entries before warning.
    pub max_lag_entries: u64,
    /// Plaintext authentication token for replication connections.
    /// **Deprecated** — prefer `auth_token_hashes` to avoid storing secrets in
    /// config files. If set, this value is hashed at load time and added to
    /// `auth_token_hashes`. Replicas also use this value as the token to send.
    pub auth_token: Option<String>,
    /// SHA-256 hex hashes of accepted replication tokens (primary side).
    /// Generate with: `rstmdb-cli hash-token <your-token>`. Supports multiple
    /// tokens for rotation. Replicas still send a plaintext token via
    /// `auth_token`; the primary compares `sha256(received_token)` against
    /// this list.
    pub auth_token_hashes: Vec<String>,
    /// Optional path to a file containing token hashes (one per line, # for
    /// comments). Loaded at startup and merged with `auth_token_hashes`.
    pub auth_secrets_file: Option<PathBuf>,
    /// How often the primary polls the WAL for new entries to stream (milliseconds).
    pub poll_interval_ms: u64,
    /// How often the primary sends heartbeats to replicas (seconds).
    pub heartbeat_interval_secs: u64,
    /// Base reconnect delay in seconds. Exponential backoff with jitter starts here.
    pub reconnect_delay_secs: u64,
    /// Maximum reconnect delay in seconds (cap for exponential backoff).
    pub reconnect_max_delay_secs: u64,
    /// How often replication lag is checked and logged (seconds).
    pub lag_check_interval_secs: u64,
    /// Enable TLS for replication connections (replica → primary).
    pub tls_enabled: bool,
    /// Path to CA certificate for verifying the primary's TLS cert.
    /// If None, uses the system root store.
    pub tls_ca_path: Option<PathBuf>,
    /// Skip TLS verification (insecure — for development only).
    pub tls_insecure: bool,
}

impl Default for ReplicationConfig {
    fn default() -> Self {
        Self {
            role: ReplicationRole::Standalone,
            mode: ReplicationMode::Async,
            upstream: None,
            sync_replicas: 1,
            sync_timeout_ms: 5000,
            max_lag_seconds: 30,
            max_lag_entries: 10000,
            auth_token: None,
            auth_token_hashes: Vec::new(),
            auth_secrets_file: None,
            poll_interval_ms: 10,
            heartbeat_interval_secs: 5,
            reconnect_delay_secs: 1,
            reconnect_max_delay_secs: 60,
            lag_check_interval_secs: 10,
            tls_enabled: false,
            tls_ca_path: None,
            tls_insecure: false,
        }
    }
}

impl ReplicationConfig {
    fn apply_env_overrides(&mut self) {
        if let Ok(role) = std::env::var("RSTMDB_REPL_ROLE") {
            match role.to_lowercase().as_str() {
                "standalone" => self.role = ReplicationRole::Standalone,
                "primary" => self.role = ReplicationRole::Primary,
                "replica" => self.role = ReplicationRole::Replica,
                _ => {}
            }
        }

        if let Ok(mode) = std::env::var("RSTMDB_REPL_MODE") {
            match mode.to_lowercase().as_str() {
                "async" => self.mode = ReplicationMode::Async,
                "sync" => self.mode = ReplicationMode::Sync,
                _ => {}
            }
        }

        if let Ok(upstream) = std::env::var("RSTMDB_REPL_UPSTREAM") {
            if !upstream.is_empty() {
                self.upstream = Some(upstream);
            }
        }

        if let Ok(n) = std::env::var("RSTMDB_REPL_SYNC_REPLICAS") {
            if let Ok(v) = n.parse() {
                self.sync_replicas = v;
            }
        }

        if let Ok(ms) = std::env::var("RSTMDB_REPL_SYNC_TIMEOUT_MS") {
            if let Ok(v) = ms.parse() {
                self.sync_timeout_ms = v;
            }
        }

        if let Ok(token) = std::env::var("RSTMDB_REPL_AUTH_TOKEN") {
            if !token.is_empty() {
                self.auth_token = Some(token);
            }
        }

        if let Ok(hash) = std::env::var("RSTMDB_REPL_AUTH_TOKEN_HASH") {
            if !hash.is_empty() {
                self.auth_token_hashes.push(hash);
            }
        }

        if let Ok(path) = std::env::var("RSTMDB_REPL_AUTH_SECRETS_FILE") {
            if !path.is_empty() {
                self.auth_secrets_file = Some(PathBuf::from(path));
            }
        }

        if let Ok(ms) = std::env::var("RSTMDB_REPL_POLL_INTERVAL_MS") {
            if let Ok(v) = ms.parse() {
                self.poll_interval_ms = v;
            }
        }

        if let Ok(s) = std::env::var("RSTMDB_REPL_HEARTBEAT_INTERVAL_SECS") {
            if let Ok(v) = s.parse() {
                self.heartbeat_interval_secs = v;
            }
        }

        if let Ok(s) = std::env::var("RSTMDB_REPL_RECONNECT_DELAY_SECS") {
            if let Ok(v) = s.parse() {
                self.reconnect_delay_secs = v;
            }
        }

        if let Ok(s) = std::env::var("RSTMDB_REPL_LAG_CHECK_INTERVAL_SECS") {
            if let Ok(v) = s.parse() {
                self.lag_check_interval_secs = v;
            }
        }

        if let Ok(s) = std::env::var("RSTMDB_REPL_RECONNECT_MAX_DELAY_SECS") {
            if let Ok(v) = s.parse() {
                self.reconnect_max_delay_secs = v;
            }
        }

        if let Ok(v) = std::env::var("RSTMDB_REPL_TLS_ENABLED") {
            self.tls_enabled = v == "1" || v.to_lowercase() == "true";
        }

        if let Ok(p) = std::env::var("RSTMDB_REPL_TLS_CA") {
            if !p.is_empty() {
                self.tls_ca_path = Some(PathBuf::from(p));
            }
        }

        if let Ok(v) = std::env::var("RSTMDB_REPL_TLS_INSECURE") {
            self.tls_insecure = v == "1" || v.to_lowercase() == "true";
        }
    }

    /// Returns the sync timeout as Duration.
    pub fn sync_timeout(&self) -> Duration {
        Duration::from_millis(self.sync_timeout_ms)
    }

    /// Returns the WAL poll interval as Duration.
    pub fn poll_interval(&self) -> Duration {
        Duration::from_millis(self.poll_interval_ms)
    }

    /// Returns the heartbeat interval as Duration.
    pub fn heartbeat_interval(&self) -> Duration {
        Duration::from_secs(self.heartbeat_interval_secs)
    }

    /// Returns the base reconnect delay as Duration.
    pub fn reconnect_delay(&self) -> Duration {
        Duration::from_secs(self.reconnect_delay_secs)
    }

    /// Returns the maximum reconnect delay as Duration.
    pub fn reconnect_max_delay(&self) -> Duration {
        Duration::from_secs(self.reconnect_max_delay_secs)
    }

    /// Returns the lag check interval as Duration.
    pub fn lag_check_interval(&self) -> Duration {
        Duration::from_secs(self.lag_check_interval_secs)
    }

    /// Returns whether this server is a primary.
    pub fn is_primary(&self) -> bool {
        self.role == ReplicationRole::Primary
    }

    /// Returns whether this server is a replica.
    pub fn is_replica(&self) -> bool {
        self.role == ReplicationRole::Replica
    }

    /// Returns whether this server is standalone (no replication).
    pub fn is_standalone(&self) -> bool {
        self.role == ReplicationRole::Standalone
    }

    /// Loads token hashes from `auth_secrets_file` if configured.
    /// Call after `Config::load()` to merge external secrets into `auth_token_hashes`.
    pub fn load_secrets(&mut self) -> Result<(), ConfigError> {
        if let Some(ref path) = self.auth_secrets_file {
            let content =
                std::fs::read_to_string(path).map_err(|e| ConfigError::IoError(path.clone(), e))?;
            for line in content.lines() {
                let line = line.trim();
                if !line.is_empty() && !line.starts_with('#') {
                    self.auth_token_hashes.push(line.to_string());
                }
            }
        }
        Ok(())
    }

    /// Returns the full set of accepted token hashes. If `auth_token` (plaintext)
    /// is set, it's hashed and included. This is what the primary uses to validate
    /// incoming replica connections.
    pub fn resolved_token_hashes(&self) -> Vec<String> {
        use sha2::{Digest, Sha256};
        let mut out = self.auth_token_hashes.clone();
        if let Some(ref t) = self.auth_token {
            let mut hasher = Sha256::new();
            hasher.update(t.as_bytes());
            out.push(hex::encode(hasher.finalize()));
        }
        out
    }

    /// Returns whether replication auth is enforced (any token hash configured).
    pub fn auth_required(&self) -> bool {
        !self.auth_token_hashes.is_empty() || self.auth_token.is_some()
    }

    /// Validates the replication configuration.
    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.role == ReplicationRole::Replica && self.upstream.is_none() {
            return Err(ConfigError::ValidationError(
                "replication role is 'replica' but no upstream address configured".to_string(),
            ));
        }
        if self.mode == ReplicationMode::Sync && self.sync_replicas == 0 {
            return Err(ConfigError::ValidationError(
                "sync replication mode requires sync_replicas > 0".to_string(),
            ));
        }
        Ok(())
    }
}

/// Configuration error.
#[derive(Debug)]
pub enum ConfigError {
    IoError(PathBuf, std::io::Error),
    ParseError(PathBuf, String),
    ValidationError(String),
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConfigError::IoError(path, e) => {
                write!(f, "failed to read config file '{}': {}", path.display(), e)
            }
            ConfigError::ParseError(path, e) => {
                write!(f, "failed to parse config file '{}': {}", path.display(), e)
            }
            ConfigError::ValidationError(msg) => {
                write!(f, "configuration validation failed: {}", msg)
            }
        }
    }
}

impl std::error::Error for ConfigError {}

/// Custom serde module for SocketAddr (to handle as string in YAML).
mod socket_addr_serde {
    use serde::{Deserialize, Deserializer, Serializer};
    use std::net::SocketAddr;

    pub fn serialize<S>(addr: &SocketAddr, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&addr.to_string())
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<SocketAddr, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = Config::default();
        assert_eq!(config.network.bind_addr.port(), 7401);
        assert_eq!(config.storage.wal_segment_size(), 64 * 1024 * 1024);
        assert_eq!(config.storage.max_machine_versions, 0); // unlimited by default
        assert!(config.compaction.enabled);
        assert!(config.replication.is_standalone());
    }

    #[test]
    fn test_flush_all_forbidden_when_replication_enabled() {
        // Standalone: allow_flush_all is permitted.
        let mut config = Config::default();
        config.storage.allow_flush_all = true;
        assert!(config.replication.is_standalone());
        assert!(
            config.validate().is_ok(),
            "standalone + flush-all should be OK"
        );

        // Primary: allow_flush_all must be rejected.
        config.replication.role = ReplicationRole::Primary;
        assert!(
            config.validate().is_err(),
            "primary + flush-all must fail validation"
        );

        // Replica: allow_flush_all must be rejected.
        config.replication.role = ReplicationRole::Replica;
        config.replication.upstream = Some("primary:7401".to_string());
        assert!(
            config.validate().is_err(),
            "replica + flush-all must fail validation"
        );

        // With flush-all off, replication roles validate fine.
        config.storage.allow_flush_all = false;
        assert!(
            config.validate().is_ok(),
            "replica without flush-all should be OK"
        );
    }

    #[test]
    fn test_replication_config_defaults() {
        let config = ReplicationConfig::default();
        assert_eq!(config.role, ReplicationRole::Standalone);
        assert_eq!(config.mode, ReplicationMode::Async);
        assert!(config.upstream.is_none());
        assert_eq!(config.sync_replicas, 1);
        assert_eq!(config.sync_timeout_ms, 5000);
        assert!(config.is_standalone());
        assert!(!config.is_primary());
        assert!(!config.is_replica());
    }

    #[test]
    fn test_replication_config_validation() {
        let mut config = ReplicationConfig::default();
        assert!(config.validate().is_ok());

        // Replica without upstream should fail
        config.role = ReplicationRole::Replica;
        assert!(config.validate().is_err());

        // Replica with upstream should succeed
        config.upstream = Some("primary:7401".to_string());
        assert!(config.validate().is_ok());

        // Sync mode with 0 replicas should fail
        config.role = ReplicationRole::Primary;
        config.mode = ReplicationMode::Sync;
        config.sync_replicas = 0;
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_replication_auth_required_disabled_by_default() {
        let config = ReplicationConfig::default();
        assert!(!config.auth_required());
        assert!(config.resolved_token_hashes().is_empty());
    }

    #[test]
    fn test_replication_auth_required_when_hashes_set() {
        let config = ReplicationConfig {
            auth_token_hashes: vec!["abc123".to_string()],
            ..Default::default()
        };
        assert!(config.auth_required());
        let hashes = config.resolved_token_hashes();
        assert_eq!(hashes.len(), 1);
        assert_eq!(hashes[0], "abc123");
    }

    #[test]
    fn test_replication_plaintext_token_is_hashed() {
        let config = ReplicationConfig {
            auth_token: Some("my-secret".to_string()),
            ..Default::default()
        };
        assert!(config.auth_required());
        let hashes = config.resolved_token_hashes();
        assert_eq!(hashes.len(), 1);
        // SHA-256 of "my-secret"
        assert_eq!(
            hashes[0],
            "186ef76e9d6a723ecb570d4d9c287487d001e5d35f7ed4a313350a407950318e"
        );
    }

    #[test]
    fn test_replication_hashes_and_plaintext_combined() {
        let config = ReplicationConfig {
            auth_token: Some("plaintext-token".to_string()),
            auth_token_hashes: vec!["existing-hash".to_string()],
            ..Default::default()
        };
        let hashes = config.resolved_token_hashes();
        assert_eq!(hashes.len(), 2);
        assert!(hashes.contains(&"existing-hash".to_string()));
    }

    #[test]
    fn test_replication_load_secrets_from_file() {
        use std::io::Write;
        let mut file = tempfile::NamedTempFile::new().unwrap();
        writeln!(file, "# This is a comment").unwrap();
        writeln!(file, "hash-one").unwrap();
        writeln!(file).unwrap();
        writeln!(file, "hash-two").unwrap();
        writeln!(file, "   ").unwrap();

        let mut config = ReplicationConfig {
            auth_token_hashes: vec!["existing".to_string()],
            auth_secrets_file: Some(file.path().to_path_buf()),
            ..Default::default()
        };
        config.load_secrets().unwrap();
        assert_eq!(config.auth_token_hashes.len(), 3);
        assert!(config.auth_token_hashes.contains(&"existing".to_string()));
        assert!(config.auth_token_hashes.contains(&"hash-one".to_string()));
        assert!(config.auth_token_hashes.contains(&"hash-two".to_string()));
    }

    #[test]
    fn test_replication_load_secrets_missing_file_fails() {
        let mut config = ReplicationConfig {
            auth_secrets_file: Some(PathBuf::from("/nonexistent/tokens.secret")),
            ..Default::default()
        };
        assert!(config.load_secrets().is_err());
    }

    #[test]
    fn test_storage_paths() {
        let config = StorageConfig::default();
        assert_eq!(config.wal_dir(), PathBuf::from("./data/wal"));
        assert_eq!(config.snapshots_dir(), PathBuf::from("./data/snapshots"));
    }

    #[test]
    fn test_yaml_roundtrip() {
        let config = Config::default();
        let yaml = serde_yaml::to_string(&config).unwrap();
        let parsed: Config = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(parsed.network.bind_addr, config.network.bind_addr);
    }
}
