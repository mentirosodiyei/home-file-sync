//! # sfs-core
//!
//! Motor de Home File Sync: sincronización de carpetas entre equipos de una
//! misma red local, sin servidores ni configuración.
//!
//! Ver `docs/ARQUITECTURA.md` para el diseño completo.

#[macro_use]
pub mod i18n;
pub mod agent;
pub mod auth;
pub mod decide;
pub mod diag;
pub mod discovery;
pub mod engine;
pub mod fsops;
pub mod hub;
pub mod ignore_rules;
pub mod inbox;
pub mod relay;
pub mod secrets;
pub mod store;
pub mod tunnel;
pub mod index;
pub mod netpath;
pub mod pathsafe;
mod peer;
pub mod protocol;
pub mod scanner;
pub mod settings;
pub mod status;
mod transfer;
pub mod transport;
pub mod vv;

pub use engine::{Engine, EngineConfig, Stats};
pub use status::UiState;
