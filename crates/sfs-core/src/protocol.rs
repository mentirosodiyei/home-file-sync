//! Mensajes del protocolo entre equipos (serializados con `postcard`).
//!
//! Secuencia de una conexión:
//! 1. Handshake Noise XX (cifrado + claves estáticas de cada equipo).
//! 2. Autenticación de grupo (`Hello`/`Welcome`) o emparejamiento (`Pair*`).
//! 3. Intercambio del índice completo (`IndexPage`), luego cambios
//!    incrementales (`IndexUpdate`), estado (`Status`) y transferencias
//!    por bloques (`DlOpen`/`BlockReq`/`Block`).

use crate::index::FileEntry;
use serde::{Deserialize, Serialize};

/// Versión del protocolo. Equipos con versiones distintas no se conectan.
pub const PROTOCOL_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Msg {
    // --- Autenticación de grupo ---
    Hello { proto: u32, name: String, group_id: String, proof: Vec<u8> },
    Welcome { name: String, proof: Vec<u8> },
    Reject { reason: RejectReason },

    // --- Emparejamiento (SPAKE2 con el código de sesión) ---
    PairStart { proto: u32, name: String, spake: Vec<u8> },
    PairReply { spake: Vec<u8>, confirm: Vec<u8> },
    PairConfirm { confirm: Vec<u8> },
    PairGroup { group_id: String, group_name: String, group_key: Vec<u8> },

    // --- Índice y estado ---
    /// Índice completo por páginas. `reset` en la primera página.
    IndexPage { reset: bool, last: bool, entries: Vec<FileEntry> },
    IndexUpdate { entries: Vec<FileEntry> },
    Status(PeerStatus),

    // --- Transferencias ---
    DlOpen { id: u32, path: String, hash: String },
    DlOpenOk { id: u32, size: u64 },
    DlBusy { id: u32 },
    DlErr { id: u32, msg: String },
    BlockReq { id: u32, offset: u64, len: u32 },
    Block {
        id: u32,
        offset: u64,
        #[serde(with = "serde_bytes")]
        data: Vec<u8>,
    },
    DlClose { id: u32 },

    Ping,
    Pong,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RejectReason {
    Version,
    NotInGroup,
    BadProof,
    PairingClosed,
    PairingFailed,
    PairingLocked,
}

impl RejectReason {
    pub fn text(&self) -> &'static str {
        match self {
            RejectReason::Version => "versión de Show File Sync incompatible",
            RejectReason::NotInGroup => "el equipo pertenece a otro grupo",
            RejectReason::BadProof => "credenciales de grupo no válidas",
            RejectReason::PairingClosed => "el equipo no está aceptando nuevos equipos ahora",
            RejectReason::PairingFailed => "código incorrecto",
            RejectReason::PairingLocked => "demasiados intentos fallidos; genera un código nuevo",
        }
    }
}

/// Estado que cada equipo anuncia de sí mismo.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerStatus {
    pub pending_files: u32,
    pub pending_bytes: u64,
    pub receiving: bool,
    pub conflicts: u32,
    pub errors: u32,
    pub uploads: u32,
}

pub fn encode(m: &Msg) -> Vec<u8> {
    postcard::to_allocvec(m).expect("serialización de mensaje")
}

pub fn decode(b: &[u8]) -> Result<Msg, postcard::Error> {
    postcard::from_bytes(b)
}
