//! Shopfloor simulation engine.
//!
//! - [`scenario`]: the TOML layout file (floor, machines, mobile manipulators, sensors, outputs).
//! - [`scene`]: scripted runs (parallel tracks of commands and waits) defined in the scenario.
//! - [`engine`]: synchronous, deterministic world state. Changes only via commands and ticks.
//! - [`command`] / [`event`]: what goes in and what comes out. The event schema is the contract
//!   with consumers.
//! - [`runtime`]: drives an engine in real time on tokio and fans events out to [`sinks`].
//! - [`headless`]: runs a scenario as fast as possible (recordings, test data).

pub mod command;
pub mod engine;
pub mod event;
pub mod geometry;
pub mod headless;
pub mod motion;
pub mod runtime;
pub mod scenario;
pub mod scene;
pub mod sinks;

pub use command::{CommandEnvelope, Control, Origin, SimCommand, Target};
pub use engine::{Engine, WorldState};
pub use event::{EventBody, SimEvent};
pub use runtime::{EventBatch, SimRuntime};
pub use scenario::{Scenario, load_scenario, parse_scenario};
