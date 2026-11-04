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

// ---------------------------------------------------------------------------
// Extensiones compatibles con versiones anteriores
// ---------------------------------------------------------------------------

/// Identificador reservado de `Block` para mensajes de extensión.
///
/// Los equipos con versiones anteriores enrutan cada `Block` por su id de
/// descarga y descartan en silencio los que no reconocen, así que pueden
/// recibir extensiones sin romper la sesión. Los ids de descarga reales
/// empiezan en 1 y nunca llegan a este valor.
pub const EXT_CHANNEL: u32 = u32::MAX;
/// Versión de las extensiones que entiende este equipo.
pub const EXT_VERSION: u32 = 1;
/// Límites de lo que se acepta de otro equipo.
pub const EXT_MAX_ADDRS: usize = 8;
pub const EXT_MAX_MEMBERS: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Ext {
    /// Datos del propio equipo: puerto de escucha y sus direcciones.
    Caps { version: u32, listen_port: u16, addrs: Vec<String> },
    /// Otros miembros del grupo conectados al emisor y direcciones por las
    /// que se les ha alcanzado. Permite conectar directamente aunque el
    /// descubrimiento no funcione entre algunos equipos.
    Members { members: Vec<MemberAddrs> },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemberAddrs {
    pub id: String,
    pub name: String,
    pub addrs: Vec<String>,
}

impl Ext {
    pub fn to_msg(&self) -> Msg {
        Msg::Block { id: EXT_CHANNEL, offset: 0, data: postcard::to_allocvec(self).expect("serialización de extensión") }
    }

    /// Decodifica una extensión. Las de versiones futuras que no se entienden se ignoran.
    pub fn from_block(data: &[u8]) -> Option<Ext> {
        postcard::from_bytes(data).ok()
    }
}

pub fn encode(m: &Msg) -> Vec<u8> {
    postcard::to_allocvec(m).expect("serialización de mensaje")
}

pub fn decode(b: &[u8]) -> Result<Msg, postcard::Error> {
    postcard::from_bytes(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extensions_travel_as_blocks() {
        let e = Ext::Members {
            members: vec![MemberAddrs { id: "a".repeat(32), name: "PC Sala 1".into(), addrs: vec!["192.168.1.66:47800".into()] }],
        };
        let wire = encode(&e.to_msg());
        // Un equipo antiguo solo ve un Block con un id que no reconoce.
        match decode(&wire).unwrap() {
            Msg::Block { id, data, .. } => {
                assert_eq!(id, EXT_CHANNEL);
                assert_eq!(Ext::from_block(&data), Some(e));
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(Ext::from_block(&[0xff, 0xff, 0x01]), None, "lo desconocido se ignora");
    }
}
