//! # rstmdb-core
//!
//! State machine engine for rstmdb.
//!
//! This crate provides:
//! - Machine definition parsing and validation
//! - State transition logic
//! - Guard expression evaluation
//! - Instance state management

pub mod definition;
pub mod engine;
pub mod error;
pub mod guard;
pub mod instance;

pub use definition::{MachineDefinition, State, Transition};
pub use engine::StateMachineEngine;
pub use error::CoreError;
pub use guard::{GuardEvaluator, GuardExpr};
pub use instance::{Instance, InstanceState};
