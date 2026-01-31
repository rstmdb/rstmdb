//! State machine definition types.
//!
//! Machine definitions use a JSON DSL:
//!
//! ```json
//! {
//!   "states": ["created", "paid", "shipped", "delivered"],
//!   "initial": "created",
//!   "transitions": [
//!     {"from": "created", "event": "PAY", "to": "paid"},
//!     {"from": "paid", "event": "SHIP", "to": "shipped", "guard": "ctx.items_in_stock"},
//!     {"from": "shipped", "event": "DELIVER", "to": "delivered"}
//!   ]
//! }
//! ```

use crate::error::CoreError;
use crate::guard::GuardExpr;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

/// A state in the machine.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct State(pub String);

impl State {
    pub fn new(name: impl Into<String>) -> Self {
        Self(name.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&str> for State {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

impl From<String> for State {
    fn from(s: String) -> Self {
        Self(s)
    }
}

/// A transition in the machine.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Transition {
    /// Source state(s). Can be a single state or multiple.
    #[serde(deserialize_with = "deserialize_from_states")]
    pub from: Vec<State>,

    /// Event that triggers this transition.
    pub event: String,

    /// Target state.
    pub to: State,

    /// Optional guard expression.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guard: Option<String>,
}

fn deserialize_from_states<'de, D>(deserializer: D) -> Result<Vec<State>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::{self, Visitor};
    use std::fmt;

    struct FromStatesVisitor;

    impl<'de> Visitor<'de> for FromStatesVisitor {
        type Value = Vec<State>;

        fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
            formatter.write_str("a string or array of strings")
        }

        fn visit_str<E>(self, v: &str) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            Ok(vec![State(v.to_string())])
        }

        fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
        where
            A: de::SeqAccess<'de>,
        {
            let mut states = Vec::new();
            while let Some(s) = seq.next_element::<String>()? {
                states.push(State(s));
            }
            Ok(states)
        }
    }

    deserializer.deserialize_any(FromStatesVisitor)
}

/// Raw machine definition as stored/transmitted.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MachineDefinitionRaw {
    /// All valid states.
    pub states: Vec<String>,

    /// Initial state for new instances.
    pub initial: String,

    /// Transitions.
    pub transitions: Vec<Transition>,

    /// Optional metadata.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub meta: Option<serde_json::Value>,
}

/// Validated and indexed machine definition.
#[derive(Debug, Clone)]
pub struct MachineDefinition {
    /// Machine name.
    pub name: String,

    /// Version number.
    pub version: u32,

    /// All valid states.
    pub states: HashSet<State>,

    /// Initial state for new instances.
    pub initial: State,

    /// Transitions indexed by (from_state, event) -> (to_state, guard).
    transitions: HashMap<(State, String), (State, Option<GuardExpr>)>,

    /// Original raw definition for storage.
    pub raw: MachineDefinitionRaw,

    /// Hash of the definition for integrity checks.
    pub checksum: String,
}

impl MachineDefinition {
    /// Parses and validates a machine definition from JSON.
    pub fn from_json(
        name: impl Into<String>,
        version: u32,
        json: &serde_json::Value,
    ) -> Result<Self, CoreError> {
        let raw: MachineDefinitionRaw = serde_json::from_value(json.clone())?;
        Self::from_raw(name, version, raw)
    }

    /// Creates a machine definition from raw parts.
    pub fn from_raw(
        name: impl Into<String>,
        version: u32,
        raw: MachineDefinitionRaw,
    ) -> Result<Self, CoreError> {
        let name = name.into();

        // Build state set
        let states: HashSet<State> = raw.states.iter().map(|s| State(s.clone())).collect();

        // Validate initial state
        let initial = State(raw.initial.clone());
        if !states.contains(&initial) {
            return Err(CoreError::InvalidDefinition {
                reason: format!("initial state '{}' not in states list", initial.as_str()),
            });
        }

        // Build and validate transitions
        let mut transitions = HashMap::new();
        for t in &raw.transitions {
            // Validate target state
            if !states.contains(&t.to) {
                return Err(CoreError::InvalidDefinition {
                    reason: format!("transition target '{}' not in states list", t.to.as_str()),
                });
            }

            // Parse guard if present
            let guard = if let Some(guard_str) = &t.guard {
                Some(GuardExpr::parse(guard_str)?)
            } else {
                None
            };

            // Add transition for each source state
            for from in &t.from {
                if !states.contains(from) {
                    return Err(CoreError::InvalidDefinition {
                        reason: format!("transition source '{}' not in states list", from.as_str()),
                    });
                }

                let key = (from.clone(), t.event.clone());
                if transitions.contains_key(&key) {
                    return Err(CoreError::InvalidDefinition {
                        reason: format!(
                            "duplicate transition from '{}' on event '{}'",
                            from.as_str(),
                            t.event
                        ),
                    });
                }

                transitions.insert(key, (t.to.clone(), guard.clone()));
            }
        }

        // Compute checksum
        let json_bytes = serde_json::to_vec(&raw)?;
        let checksum = format!("{:08x}", crc32c::crc32c(&json_bytes));

        Ok(Self {
            name,
            version,
            states,
            initial,
            transitions,
            raw,
            checksum,
        })
    }

    /// Looks up a transition for the given state and event.
    pub fn get_transition(
        &self,
        state: &State,
        event: &str,
    ) -> Option<(&State, Option<&GuardExpr>)> {
        self.transitions
            .get(&(state.clone(), event.to_string()))
            .map(|(to, guard)| (to, guard.as_ref()))
    }

    /// Returns true if the given state is valid for this machine.
    pub fn has_state(&self, state: &State) -> bool {
        self.states.contains(state)
    }

    /// Returns all valid events from the given state.
    pub fn events_from(&self, state: &State) -> Vec<&str> {
        self.transitions
            .keys()
            .filter(|(s, _)| s == state)
            .map(|(_, e)| e.as_str())
            .collect()
    }

    /// Returns the raw definition as JSON.
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::to_value(&self.raw).unwrap()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_definition() -> serde_json::Value {
        serde_json::json!({
            "states": ["created", "paid", "shipped", "delivered", "refunded"],
            "initial": "created",
            "transitions": [
                {"from": "created", "event": "PAY", "to": "paid"},
                {"from": "paid", "event": "SHIP", "to": "shipped"},
                {"from": "shipped", "event": "DELIVER", "to": "delivered"},
                {"from": ["paid", "shipped"], "event": "REFUND", "to": "refunded", "guard": "ctx.refund_available"}
            ]
        })
    }

    #[test]
    fn test_parse_definition() {
        let def = MachineDefinition::from_json("order", 1, &sample_definition()).unwrap();

        assert_eq!(def.name, "order");
        assert_eq!(def.version, 1);
        assert_eq!(def.initial.as_str(), "created");
        assert_eq!(def.states.len(), 5);
    }

    #[test]
    fn test_transition_lookup() {
        let def = MachineDefinition::from_json("order", 1, &sample_definition()).unwrap();

        // Valid transition
        let (to, guard) = def.get_transition(&State::from("created"), "PAY").unwrap();
        assert_eq!(to.as_str(), "paid");
        assert!(guard.is_none());

        // Transition with guard
        let (to, guard) = def.get_transition(&State::from("paid"), "REFUND").unwrap();
        assert_eq!(to.as_str(), "refunded");
        assert!(guard.is_some());

        // Invalid transition
        assert!(def
            .get_transition(&State::from("created"), "SHIP")
            .is_none());
    }

    #[test]
    fn test_multi_source_transition() {
        let def = MachineDefinition::from_json("order", 1, &sample_definition()).unwrap();

        // REFUND from paid
        let (to, _) = def.get_transition(&State::from("paid"), "REFUND").unwrap();
        assert_eq!(to.as_str(), "refunded");

        // REFUND from shipped
        let (to, _) = def
            .get_transition(&State::from("shipped"), "REFUND")
            .unwrap();
        assert_eq!(to.as_str(), "refunded");
    }

    #[test]
    fn test_invalid_initial_state() {
        let json = serde_json::json!({
            "states": ["a", "b"],
            "initial": "c",
            "transitions": []
        });

        let result = MachineDefinition::from_json("test", 1, &json);
        assert!(matches!(result, Err(CoreError::InvalidDefinition { .. })));
    }

    #[test]
    fn test_invalid_transition_target() {
        let json = serde_json::json!({
            "states": ["a", "b"],
            "initial": "a",
            "transitions": [
                {"from": "a", "event": "GO", "to": "c"}
            ]
        });

        let result = MachineDefinition::from_json("test", 1, &json);
        assert!(matches!(result, Err(CoreError::InvalidDefinition { .. })));
    }
}
