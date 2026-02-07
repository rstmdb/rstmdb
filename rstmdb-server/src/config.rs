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

    /// Applies environment variable overrides to the configuration.
    fn apply_env_overrides(&mut self) {
        self.network.apply_env_overrides();
        self.storage.apply_env_overrides();
        self.compaction.apply_env_overrides();
        self.auth.apply_env_overrides();
        self.tls.apply_env_overrides();
        self.metrics.apply_env_overrides();
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
