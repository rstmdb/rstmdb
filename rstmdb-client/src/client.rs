//! High-level client API.

use crate::connection::{Connection, ConnectionConfig};
use crate::error::ClientError;
use rstmdb_protocol::message::*;
use serde_json::{json, Value};
use std::sync::Arc;

/// High-level client for rstmdb.
pub struct Client {
    conn: Arc<Connection>,
}

impl Client {
    /// Creates a new client with the given configuration.
    pub fn new(config: ConnectionConfig) -> Self {
        Self {
            conn: Arc::new(Connection::new(config)),
        }
    }

    /// Connects to the server.
    pub async fn connect(&self) -> Result<(), ClientError> {
        self.conn.connect().await
    }

    /// Returns whether the client is connected.
    pub fn is_connected(&self) -> bool {
        self.conn.is_connected()
    }

    /// Closes the connection.
    pub async fn close(&self) -> Result<(), ClientError> {
        self.conn.close().await
    }

    /// Returns the underlying connection (for background read loop).
    pub fn connection(&self) -> Arc<Connection> {
        self.conn.clone()
    }

    // =========================================================================
    // Helper methods
    // =========================================================================

    async fn request(&self, op: Operation, params: Value) -> Result<Value, ClientError> {
        let response = self.conn.request(op, params).await?;

        if response.is_error() {
            let err = response.error.unwrap();
            return Err(ClientError::ServerError {
                code: err.code,
                message: err.message,
                retryable: err.retryable,
            });
        }

        Ok(response.result.unwrap_or(Value::Null))
    }

    // =========================================================================
    // System operations
    // =========================================================================

    /// Pings the server.
    pub async fn ping(&self) -> Result<(), ClientError> {
        self.request(Operation::Ping, json!({})).await?;
        Ok(())
    }

    /// Gets server info.
    pub async fn info(&self) -> Result<Value, ClientError> {
        self.request(Operation::Info, json!({})).await
    }

    // =========================================================================
    // Machine operations
    // =========================================================================

    /// Registers a machine definition.
    pub async fn put_machine(
        &self,
        machine: &str,
        version: u32,
        definition: Value,
    ) -> Result<PutMachineResult, ClientError> {
        let params = json!({
            "machine": machine,
            "version": version,
            "definition": definition,
        });

        let result = self.request(Operation::PutMachine, params).await?;
        Ok(serde_json::from_value(result)?)
    }

    /// Gets a machine definition.
    pub async fn get_machine(
        &self,
        machine: &str,
        version: u32,
    ) -> Result<GetMachineResult, ClientError> {
        let params = json!({
            "machine": machine,
            "version": version,
        });

        let result = self.request(Operation::GetMachine, params).await?;
        Ok(serde_json::from_value(result)?)
    }

    /// Lists all machines.
    pub async fn list_machines(&self) -> Result<Value, ClientError> {
        self.request(Operation::ListMachines, json!({})).await
    }

    // =========================================================================
    // Instance operations
    // =========================================================================

    /// Creates a new instance.
    pub async fn create_instance(
        &self,
        machine: &str,
        version: u32,
        instance_id: Option<&str>,
        initial_ctx: Option<Value>,
        idempotency_key: Option<&str>,
    ) -> Result<CreateInstanceResult, ClientError> {
        let mut params = json!({
            "machine": machine,
            "version": version,
        });

        if let Some(id) = instance_id {
            params["instance_id"] = json!(id);
        }
        if let Some(ctx) = initial_ctx {
            params["initial_ctx"] = ctx;
        }
        if let Some(key) = idempotency_key {
            params["idempotency_key"] = json!(key);
        }

        let result = self.request(Operation::CreateInstance, params).await?;
        Ok(serde_json::from_value(result)?)
    }

    /// Gets an instance.
    pub async fn get_instance(&self, instance_id: &str) -> Result<GetInstanceResult, ClientError> {
        let params = json!({
            "instance_id": instance_id,
        });

        let result = self.request(Operation::GetInstance, params).await?;
        Ok(serde_json::from_value(result)?)
    }

    /// Deletes an instance.
    pub async fn delete_instance(
        &self,
        instance_id: &str,
        idempotency_key: Option<&str>,
    ) -> Result<Value, ClientError> {
        let mut params = json!({
            "instance_id": instance_id,
        });

        if let Some(key) = idempotency_key {
            params["idempotency_key"] = json!(key);
        }

        self.request(Operation::DeleteInstance, params).await
    }

    // =========================================================================
    // Event operations
    // =========================================================================

    /// Applies an event to an instance.
    pub async fn apply_event(
        &self,
        instance_id: &str,
        event: &str,
        payload: Option<Value>,
        expected_state: Option<&str>,
        idempotency_key: Option<&str>,
    ) -> Result<ApplyEventResult, ClientError> {
        let mut params = json!({
            "instance_id": instance_id,
            "event": event,
        });

        if let Some(p) = payload {
            params["payload"] = p;
        }
        if let Some(state) = expected_state {
            params["expected_state"] = json!(state);
        }
        if let Some(key) = idempotency_key {
            params["idempotency_key"] = json!(key);
        }

        let result = self.request(Operation::ApplyEvent, params).await?;
        Ok(serde_json::from_value(result)?)
    }

    // =========================================================================
    // Batch operations
    // =========================================================================

    /// Executes a batch of operations.
    pub async fn batch(&self, ops: Vec<Value>, atomic: bool) -> Result<Value, ClientError> {
        let params = json!({
            "mode": if atomic { "atomic" } else { "best_effort" },
            "ops": ops,
        });

        self.request(Operation::Batch, params).await
    }

    // =========================================================================
    // WAL operations
    // =========================================================================

    /// Reads WAL entries.
    pub async fn wal_read(
        &self,
        from_offset: u64,
        limit: Option<u64>,
    ) -> Result<Value, ClientError> {
        let mut params = json!({
            "from_offset": from_offset,
        });

        if let Some(l) = limit {
            params["limit"] = json!(l);
        }

        self.request(Operation::WalRead, params).await
    }

    // =========================================================================
    // Snapshot operations
    // =========================================================================

    /// Creates a snapshot for an instance.
    pub async fn snapshot_instance(&self, instance_id: &str) -> Result<Value, ClientError> {
        let params = json!({
            "instance_id": instance_id,
        });

        self.request(Operation::SnapshotInstance, params).await
    }

    /// Compacts WAL by snapshotting instances and deleting old segments.
    pub async fn compact(&self, force_snapshot: bool) -> Result<Value, ClientError> {
        let params = json!({
            "force_snapshot": force_snapshot,
        });

        self.request(Operation::Compact, params).await
    }

    // =========================================================================
    // Watch operations
    // =========================================================================

    /// Watches a specific instance for state changes.
    /// Returns the subscription info (use read_stream_events to receive events).
    pub async fn watch_instance(
        &self,
        instance_id: &str,
        include_ctx: bool,
    ) -> Result<WatchInstanceResult, ClientError> {
        let params = json!({
            "instance_id": instance_id,
            "include_ctx": include_ctx,
        });

        let result = self.request(Operation::WatchInstance, params).await?;
        Ok(serde_json::from_value(result)?)
    }

    /// Watches all events with optional filters.
    /// Returns the subscription info (use read_stream_events to receive events).
    pub async fn watch_all(
        &self,
        machines: Option<Vec<String>>,
        from_states: Option<Vec<String>>,
        to_states: Option<Vec<String>>,
        events: Option<Vec<String>>,
        include_ctx: bool,
    ) -> Result<WatchAllResult, ClientError> {
        let mut params = json!({
            "include_ctx": include_ctx,
        });

        if let Some(m) = machines {
            params["machines"] = json!(m);
        }
        if let Some(f) = from_states {
            params["from_states"] = json!(f);
        }
        if let Some(t) = to_states {
            params["to_states"] = json!(t);
        }
        if let Some(e) = events {
            params["events"] = json!(e);
        }

        let result = self.request(Operation::WatchAll, params).await?;
        Ok(serde_json::from_value(result)?)
    }

    /// Cancels a watch subscription.
    pub async fn unwatch(&self, subscription_id: &str) -> Result<UnwatchResult, ClientError> {
        let params = json!({
            "subscription_id": subscription_id,
        });

        let result = self.request(Operation::Unwatch, params).await?;
        Ok(serde_json::from_value(result)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_client_creation() {
        let config = ConnectionConfig::new("127.0.0.1:7401".parse().unwrap());
        let client = Client::new(config);
        assert!(!client.is_connected());
    }
}
