//! # sfs-core
//!
//! Motor de Show File Sync: sincronización de carpetas entre equipos de una
//! misma red local, sin servidores ni configuración.
//!
//! Ver `docs/ARQUITECTURA.md` para el diseño completo.

pub mod auth;
pub mod decide;
pub mod discovery;
pub mod engine;
pub mod fsops;
pub mod index;
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
