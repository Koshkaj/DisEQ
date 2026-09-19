//! The DisEQ audio engine: equaliser, routing, and per-app mixing.
//!
//! No UI. `kd-app` drives this; `kd-core` stays with the displays.

pub mod bridge;
pub mod devices;
pub mod engine;
pub mod eq;
pub mod format;
pub mod mixer;
pub mod offline;
pub mod playable;
pub mod playback;
pub mod processes;
pub mod ring;
pub mod router;
pub mod shared;
pub mod tap;
pub mod unit;

pub use devices::Device;
pub use engine::EqUnit;
pub use eq::{Equalizer, Preset, Settings, BAND_COUNT, FREQUENCIES, PRESETS};
pub use router::{Health, Route, RouteError};
