//! Session management.

use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Instant;
use uuid::Uuid;

/// Subscription type for session tracking.
#[derive(Debug, Clone)]
pub enum SessionSubscriptionType {
    /// Watch a specific instance.
    Instance { instance_id: String },
    /// Watch all events.
    All,
}

/// Wire mode for the session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WireMode {
    /// Binary framing with JSON payload.
    #[default]
    BinaryJson,
    /// Line-delimited JSON (debug mode).
    Jsonl,
}

/// Session state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionState {
    /// Initial state, waiting for HELLO.
    Connected,
    /// Handshake complete, ready for commands.
    Ready,
    /// Authenticated (if auth is required).
    Authenticated,
    /// Session is closing.
    Closing,
}

/// A client session.
pub struct Session {
    /// Unique session ID.
    pub id: String,

    /// Remote address.
    pub remote_addr: SocketAddr,

    /// Session state.
    state: SessionState,

    /// Wire mode after handshake.
    wire_mode: WireMode,

    /// Negotiated protocol version.
    protocol_version: u16,

    /// Client name from HELLO.
    client_name: Option<String>,

    /// Negotiated features.
    features: HashSet<String>,

    /// Whether authentication is required but not yet provided.
    auth_required: bool,

    /// Is authenticated.
    authenticated: AtomicBool,

    /// Request counter.
    request_count: AtomicU64,

    /// Session creation time.
    created_at: Instant,

    /// Last activity time.
    last_activity: std::sync::Mutex<Instant>,

    /// Active subscriptions: subscription_id -> type.
    subscriptions: std::sync::Mutex<HashMap<String, SessionSubscriptionType>>,
}

impl Session {
    /// Creates a new session.
    pub fn new(remote_addr: SocketAddr, auth_required: bool) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            remote_addr,
            state: SessionState::Connected,
            wire_mode: WireMode::default(),
            protocol_version: 0,
            client_name: None,
            features: HashSet::new(),
            auth_required,
            authenticated: AtomicBool::new(!auth_required),
            request_count: AtomicU64::new(0),
            created_at: Instant::now(),
            last_activity: std::sync::Mutex::new(Instant::now()),
            subscriptions: std::sync::Mutex::new(HashMap::new()),
        }
    }

    /// Returns the session state.
    pub fn state(&self) -> SessionState {
        self.state
    }

    /// Sets the session state.
    pub fn set_state(&mut self, state: SessionState) {
        self.state = state;
    }

    /// Returns the wire mode.
    pub fn wire_mode(&self) -> WireMode {
        self.wire_mode
    }

    /// Returns the protocol version.
    pub fn protocol_version(&self) -> u16 {
        self.protocol_version
    }

    /// Returns the client name.
    pub fn client_name(&self) -> Option<&str> {
        self.client_name.as_deref()
    }

    /// Returns whether the session is authenticated.
    pub fn is_authenticated(&self) -> bool {
        self.authenticated.load(Ordering::Acquire)
    }

    /// Sets the authenticated flag.
    pub fn set_authenticated(&self, authenticated: bool) {
        self.authenticated.store(authenticated, Ordering::Release);
    }

    /// Completes the handshake.
    pub fn complete_handshake(
        &mut self,
        protocol_version: u16,
        wire_mode: WireMode,
        client_name: Option<String>,
        features: HashSet<String>,
    ) {
        self.protocol_version = protocol_version;
        self.wire_mode = wire_mode;
        self.client_name = client_name;
        self.features = features;
        self.state = if self.auth_required && !self.is_authenticated() {
            SessionState::Ready
        } else {
            SessionState::Authenticated
        };
    }

    /// Records a request.
    pub fn record_request(&self) {
        self.request_count.fetch_add(1, Ordering::Relaxed);
        *self.last_activity.lock().unwrap() = Instant::now();
    }

    /// Returns the request count.
    pub fn request_count(&self) -> u64 {
        self.request_count.load(Ordering::Relaxed)
    }

    /// Returns the time since last activity.
    pub fn idle_duration(&self) -> std::time::Duration {
        self.last_activity.lock().unwrap().elapsed()
    }

    /// Returns the session age.
    pub fn age(&self) -> std::time::Duration {
        self.created_at.elapsed()
    }

    /// Checks if a feature is enabled.
    pub fn has_feature(&self, feature: &str) -> bool {
        self.features.contains(feature)
    }

    /// Adds a subscription for a specific instance.
    pub fn add_subscription(&self, subscription_id: String) {
        self.subscriptions.lock().unwrap().insert(
            subscription_id,
            SessionSubscriptionType::All, // Default for backwards compatibility
        );
    }

    /// Adds a subscription for a specific instance.
    pub fn add_instance_subscription(&self, subscription_id: String, instance_id: String) {
        self.subscriptions.lock().unwrap().insert(
            subscription_id,
            SessionSubscriptionType::Instance { instance_id },
        );
    }

    /// Adds a subscription for all events.
    pub fn add_all_subscription(&self, subscription_id: String) {
        self.subscriptions
            .lock()
            .unwrap()
            .insert(subscription_id, SessionSubscriptionType::All);
    }

    /// Removes a subscription.
    pub fn remove_subscription(&self, subscription_id: &str) -> bool {
        self.subscriptions
            .lock()
            .unwrap()
            .remove(subscription_id)
            .is_some()
    }

    /// Gets the instance_id for a subscription, if it's an instance subscription.
    pub fn get_subscription_instance(&self, subscription_id: &str) -> Option<String> {
        match self.subscriptions.lock().unwrap().get(subscription_id) {
            Some(SessionSubscriptionType::Instance { instance_id }) => Some(instance_id.clone()),
            _ => None,
        }
    }

    /// Returns subscription IDs.
    pub fn subscriptions(&self) -> Vec<String> {
        self.subscriptions.lock().unwrap().keys().cloned().collect()
    }

    /// Returns the number of active subscriptions.
    pub fn subscription_count(&self) -> usize {
        self.subscriptions.lock().unwrap().len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr};

    fn test_addr() -> SocketAddr {
        SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 12345)
    }

    #[test]
    fn test_session_creation() {
        let session = Session::new(test_addr(), false);
        assert_eq!(session.state(), SessionState::Connected);
        assert!(session.is_authenticated()); // No auth required
    }

    #[test]
    fn test_session_with_auth() {
        let session = Session::new(test_addr(), true);
        assert!(!session.is_authenticated());
    }

    #[test]
    fn test_session_handshake() {
        let mut session = Session::new(test_addr(), false);
        session.complete_handshake(
            1,
            WireMode::BinaryJson,
            Some("test-client".to_string()),
            HashSet::from(["idempotency".to_string()]),
        );

        assert_eq!(session.state(), SessionState::Authenticated);
        assert_eq!(session.protocol_version(), 1);
        assert_eq!(session.client_name(), Some("test-client"));
        assert!(session.has_feature("idempotency"));
    }

    #[test]
    fn test_session_subscriptions() {
        let session = Session::new(test_addr(), false);
        session.add_instance_subscription("sub-1".to_string(), "instance-1".to_string());
        session.add_all_subscription("sub-2".to_string());

        assert_eq!(session.subscriptions().len(), 2);
        assert_eq!(session.subscription_count(), 2);

        // Check instance subscription lookup
        assert_eq!(
            session.get_subscription_instance("sub-1"),
            Some("instance-1".to_string())
        );
        assert_eq!(session.get_subscription_instance("sub-2"), None); // All subscription

        assert!(session.remove_subscription("sub-1"));
        assert_eq!(session.subscriptions().len(), 1);

        // Can't remove twice
        assert!(!session.remove_subscription("sub-1"));
    }
}
