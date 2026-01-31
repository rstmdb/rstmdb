//! Event broadcasting for watch subscriptions.

use dashmap::DashMap;
use serde_json::Value;
use std::sync::Arc;
use tokio::sync::broadcast;

/// Event data sent through broadcast channels.
#[derive(Debug, Clone)]
pub struct InstanceEvent {
    pub instance_id: String,
    pub machine: String,
    pub version: u32,
    pub wal_offset: u64,
    pub from_state: String,
    pub to_state: String,
    pub event: String,
    pub payload: Value,
    pub ctx: Value,
}

/// Filter for WATCH_ALL subscriptions.
#[derive(Debug, Clone, Default)]
pub struct EventFilter {
    /// Only events for these machines (empty = all).
    pub machines: Vec<String>,
    /// Only events FROM these states (empty = all).
    pub from_states: Vec<String>,
    /// Only events TO these states (empty = all).
    pub to_states: Vec<String>,
    /// Only these event types (empty = all).
    pub events: Vec<String>,
}

impl EventFilter {
    /// Returns true if the event matches this filter.
    pub fn matches(&self, event: &InstanceEvent) -> bool {
        (self.machines.is_empty() || self.machines.contains(&event.machine))
            && (self.from_states.is_empty() || self.from_states.contains(&event.from_state))
            && (self.to_states.is_empty() || self.to_states.contains(&event.to_state))
            && (self.events.is_empty() || self.events.contains(&event.event))
    }
}

/// Subscription type.
#[derive(Debug, Clone)]
pub enum SubscriptionType {
    /// Watch a specific instance.
    Instance {
        instance_id: String,
        include_ctx: bool,
    },
    /// Watch all events (optionally filtered).
    All {
        filter: EventFilter,
        include_ctx: bool,
    },
}

/// Subscription info.
#[derive(Debug, Clone)]
pub struct Subscription {
    pub subscription_id: String,
    pub subscription_type: SubscriptionType,
}

/// Manages event broadcasting for watch subscriptions.
pub struct EventBroadcaster {
    /// Per-instance broadcast channels.
    channels: DashMap<String, broadcast::Sender<InstanceEvent>>,

    /// Global broadcast channel (for WATCH_ALL).
    global_sender: broadcast::Sender<InstanceEvent>,

    /// Subscription metadata.
    subscriptions: DashMap<String, Subscription>,

    /// Channel capacity.
    channel_capacity: usize,
}

impl EventBroadcaster {
    /// Creates a new EventBroadcaster with the specified channel capacity.
    pub fn new(channel_capacity: usize) -> Self {
        let (global_sender, _) = broadcast::channel(channel_capacity);
        Self {
            channels: DashMap::new(),
            global_sender,
            subscriptions: DashMap::new(),
            channel_capacity,
        }
    }

    /// Subscribes to a specific instance's events.
    ///
    /// Returns (subscription_id, receiver).
    pub fn subscribe_instance(
        &self,
        instance_id: &str,
        include_ctx: bool,
    ) -> (String, broadcast::Receiver<InstanceEvent>) {
        let subscription_id = format!("sub-{}", uuid::Uuid::new_v4());

        // Get or create channel for this instance
        let sender = self
            .channels
            .entry(instance_id.to_string())
            .or_insert_with(|| broadcast::channel(self.channel_capacity).0)
            .clone();

        let receiver = sender.subscribe();

        // Store subscription metadata
        self.subscriptions.insert(
            subscription_id.clone(),
            Subscription {
                subscription_id: subscription_id.clone(),
                subscription_type: SubscriptionType::Instance {
                    instance_id: instance_id.to_string(),
                    include_ctx,
                },
            },
        );

        (subscription_id, receiver)
    }

    /// Subscribes to ALL events (with optional filter).
    ///
    /// Returns (subscription_id, receiver).
    pub fn subscribe_all(
        &self,
        filter: EventFilter,
        include_ctx: bool,
    ) -> (String, broadcast::Receiver<InstanceEvent>) {
        let subscription_id = format!("sub-{}", uuid::Uuid::new_v4());
        let receiver = self.global_sender.subscribe();

        // Store subscription metadata
        self.subscriptions.insert(
            subscription_id.clone(),
            Subscription {
                subscription_id: subscription_id.clone(),
                subscription_type: SubscriptionType::All {
                    filter,
                    include_ctx,
                },
            },
        );

        (subscription_id, receiver)
    }

    /// Unsubscribes from events.
    ///
    /// Returns true if the subscription was found and removed.
    pub fn unsubscribe(&self, subscription_id: &str) -> bool {
        self.subscriptions.remove(subscription_id).is_some()
    }

    /// Notifies all watchers of an instance event.
    ///
    /// Sends to: instance-specific channel + global channel.
    pub fn notify(&self, event: InstanceEvent) {
        // Send to instance-specific channel if it exists
        if let Some(sender) = self.channels.get(&event.instance_id) {
            // Ignore send errors (no receivers)
            let _ = sender.send(event.clone());
        }

        // Always send to global channel for WATCH_ALL subscribers
        let _ = self.global_sender.send(event);
    }

    /// Gets subscription info.
    pub fn get_subscription(&self, subscription_id: &str) -> Option<Subscription> {
        self.subscriptions.get(subscription_id).map(|r| r.clone())
    }

    /// Returns the number of active subscriptions.
    pub fn subscription_count(&self) -> usize {
        self.subscriptions.len()
    }

    /// Returns the global channel sender (for creating new receivers).
    pub fn global_sender(&self) -> &broadcast::Sender<InstanceEvent> {
        &self.global_sender
    }
}

impl Default for EventBroadcaster {
    fn default() -> Self {
        Self::new(1024)
    }
}

/// Creates a shared EventBroadcaster.
pub fn create_broadcaster(channel_capacity: usize) -> Arc<EventBroadcaster> {
    Arc::new(EventBroadcaster::new(channel_capacity))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_subscribe_instance() {
        let broadcaster = EventBroadcaster::new(16);
        let (sub_id, _rx) = broadcaster.subscribe_instance("instance-1", true);

        assert!(sub_id.starts_with("sub-"));
        assert!(broadcaster.get_subscription(&sub_id).is_some());
    }

    #[test]
    fn test_subscribe_all() {
        let broadcaster = EventBroadcaster::new(16);
        let filter = EventFilter {
            machines: vec!["order".to_string()],
            ..Default::default()
        };
        let (sub_id, _rx) = broadcaster.subscribe_all(filter, true);

        assert!(sub_id.starts_with("sub-"));
        let sub = broadcaster.get_subscription(&sub_id).unwrap();
        match sub.subscription_type {
            SubscriptionType::All { filter, .. } => {
                assert_eq!(filter.machines, vec!["order".to_string()]);
            }
            _ => panic!("Expected All subscription type"),
        }
    }

    #[test]
    fn test_unsubscribe() {
        let broadcaster = EventBroadcaster::new(16);
        let (sub_id, _rx) = broadcaster.subscribe_instance("instance-1", true);

        assert!(broadcaster.unsubscribe(&sub_id));
        assert!(!broadcaster.unsubscribe(&sub_id)); // Already removed
        assert!(broadcaster.get_subscription(&sub_id).is_none());
    }

    #[tokio::test]
    async fn test_notify_instance() {
        let broadcaster = EventBroadcaster::new(16);
        let (_, mut rx) = broadcaster.subscribe_instance("instance-1", true);

        let event = InstanceEvent {
            instance_id: "instance-1".to_string(),
            machine: "order".to_string(),
            version: 1,
            wal_offset: 42,
            from_state: "created".to_string(),
            to_state: "paid".to_string(),
            event: "PAY".to_string(),
            payload: serde_json::json!({"amount": 100}),
            ctx: serde_json::json!({"user": "alice"}),
        };

        broadcaster.notify(event);

        let received = rx.recv().await.unwrap();
        assert_eq!(received.instance_id, "instance-1");
        assert_eq!(received.event, "PAY");
        assert_eq!(received.from_state, "created");
        assert_eq!(received.to_state, "paid");
    }

    #[tokio::test]
    async fn test_notify_global() {
        let broadcaster = EventBroadcaster::new(16);
        let (_, mut rx) = broadcaster.subscribe_all(EventFilter::default(), true);

        let event = InstanceEvent {
            instance_id: "instance-2".to_string(),
            machine: "workflow".to_string(),
            version: 1,
            wal_offset: 100,
            from_state: "pending".to_string(),
            to_state: "done".to_string(),
            event: "COMPLETE".to_string(),
            payload: serde_json::json!(null),
            ctx: serde_json::json!({}),
        };

        broadcaster.notify(event);

        let received = rx.recv().await.unwrap();
        assert_eq!(received.instance_id, "instance-2");
        assert_eq!(received.machine, "workflow");
    }

    #[test]
    fn test_event_filter_matches() {
        let event = InstanceEvent {
            instance_id: "i-1".to_string(),
            machine: "order".to_string(),
            version: 1,
            wal_offset: 1,
            from_state: "created".to_string(),
            to_state: "paid".to_string(),
            event: "PAY".to_string(),
            payload: serde_json::json!(null),
            ctx: serde_json::json!({}),
        };

        // Empty filter matches all
        let filter = EventFilter::default();
        assert!(filter.matches(&event));

        // Machine filter
        let filter = EventFilter {
            machines: vec!["order".to_string()],
            ..Default::default()
        };
        assert!(filter.matches(&event));

        let filter = EventFilter {
            machines: vec!["workflow".to_string()],
            ..Default::default()
        };
        assert!(!filter.matches(&event));

        // State filters
        let filter = EventFilter {
            to_states: vec!["paid".to_string()],
            ..Default::default()
        };
        assert!(filter.matches(&event));

        let filter = EventFilter {
            to_states: vec!["shipped".to_string()],
            ..Default::default()
        };
        assert!(!filter.matches(&event));

        // Event filter
        let filter = EventFilter {
            events: vec!["PAY".to_string()],
            ..Default::default()
        };
        assert!(filter.matches(&event));

        // Combined filters
        let filter = EventFilter {
            machines: vec!["order".to_string()],
            to_states: vec!["paid".to_string(), "shipped".to_string()],
            ..Default::default()
        };
        assert!(filter.matches(&event));
    }

    #[test]
    fn test_subscription_count() {
        let broadcaster = EventBroadcaster::new(16);
        assert_eq!(broadcaster.subscription_count(), 0);

        let (sub1, _) = broadcaster.subscribe_instance("i-1", true);
        assert_eq!(broadcaster.subscription_count(), 1);

        let (sub2, _) = broadcaster.subscribe_all(EventFilter::default(), true);
        assert_eq!(broadcaster.subscription_count(), 2);

        broadcaster.unsubscribe(&sub1);
        assert_eq!(broadcaster.subscription_count(), 1);

        broadcaster.unsubscribe(&sub2);
        assert_eq!(broadcaster.subscription_count(), 0);
    }
}
