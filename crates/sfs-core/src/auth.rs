//! Identidad de equipo, autenticación de grupo y emparejamiento por código.
//!
//! * **Identidad**: el identificador de un equipo es un hash de su clave
//!   pública Noise. No se puede suplantar sin la clave privada.
//! * **Grupo**: todos los equipos de un grupo comparten una clave secreta de
//!   32 bytes. Al conectar, cada lado demuestra que la conoce enviando un MAC
//!   (BLAKE3 con clave) del `handshake_hash` de la sesión Noise. El MAC no
//!   revela la clave y no sirve en ninguna otra conexión.
//! * **Emparejamiento**: un equipo del grupo muestra un código de 8 cifras.
//!   El equipo nuevo y el del código ejecutan SPAKE2 con ese código: si
//!   coincide, ambos obtienen la misma clave sin que el código viaje por la red
//!   y sin que un atacante pueda probar códigos sin conexión (solo un intento
//!   por conexión, y el código se bloquea tras unos pocos fallos). Después el
//!   equipo del grupo envía la clave del grupo por el canal cifrado.

use spake2::{Ed25519Group, Identity, Password, Spake2};

pub const CODE_DIGITS: usize = 8;

pub fn random_bytes<const N: usize>() -> [u8; N] {
    let mut b = [0u8; N];
    getrandom::getrandom(&mut b).expect("generador aleatorio del sistema");
    b
}

/// Identificador estable de un equipo (32 caracteres hex).
pub fn device_id(public_key: &[u8]) -> String {
    blake3::hash(public_key).to_hex()[..32].to_string()
}

/// Número compacto del equipo para los vectores de versión.
pub fn device_num(id: &str) -> u64 {
    u64::from_str_radix(&id[..16.min(id.len())], 16).unwrap_or(0)
}

fn mac(key: &[u8; 32], context: &str, role: &str, hh: &[u8]) -> [u8; 32] {
    let mut h = blake3::Hasher::new_keyed(key);
    h.update(context.as_bytes());
    h.update(&[0]);
    h.update(role.as_bytes());
    h.update(&[0]);
    h.update(hh);
    *h.finalize().as_bytes()
}

/// Prueba de pertenencia al grupo para esta sesión.
pub fn group_proof(group_key: &[u8; 32], role: &str, handshake_hash: &[u8]) -> Vec<u8> {
    mac(group_key, "sfs/group-auth/v1", role, handshake_hash).to_vec()
}

/// Comparación en tiempo constante.
pub fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Genera un código de emparejamiento de 8 cifras sin sesgo.
pub fn new_code() -> String {
    let mut out = String::new();
    while out.len() < CODE_DIGITS {
        let b = random_bytes::<1>()[0];
        if b < 250 {
            out.push(char::from(b'0' + b % 10));
        }
    }
    out
}

/// "48210937" → "4821 0937"
pub fn format_code(code: &str) -> String {
    format!("{} {}", &code[..4], &code[4..])
}

/// Acepta el código con espacios, guiones, etc. Devuelve solo las cifras.
pub fn normalize_code(input: &str) -> Option<String> {
    let digits: String = input.chars().filter(|c| c.is_ascii_digit()).collect();
    (digits.len() == CODE_DIGITS).then_some(digits)
}

pub struct PairState(Spake2<Ed25519Group>);

fn spake(code: &str, joiner: bool) -> (PairState, Vec<u8>) {
    let pw = Password::new(code.as_bytes());
    let a = Identity::new(b"homefilesync-joiner");
    let b = Identity::new(b"homefilesync-member");
    let (s, msg) = if joiner { Spake2::<Ed25519Group>::start_a(&pw, &a, &b) } else { Spake2::<Ed25519Group>::start_b(&pw, &a, &b) };
    (PairState(s), msg)
}

/// Lado del equipo que se une.
pub fn pair_start_joiner(code: &str) -> (PairState, Vec<u8>) {
    spake(code, true)
}

/// Lado del equipo que ya está en el grupo.
pub fn pair_start_member(code: &str) -> (PairState, Vec<u8>) {
    spake(code, false)
}

impl PairState {
    /// Clave compartida (idéntica en ambos lados solo si el código coincide).
    pub fn finish(self, other: &[u8]) -> Option<[u8; 32]> {
        let k = self.0.finish(other).ok()?;
        Some(blake3::derive_key("homefilesync pairing key v1", &k))
    }
}

/// Confirmación de clave atada a la sesión Noise.
pub fn pair_confirm(key: &[u8; 32], role: &str, handshake_hash: &[u8]) -> Vec<u8> {
    mac(key, "sfs/pair-confirm/v1", role, handshake_hash).to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spake_matches_only_with_same_code() {
        let (a, ma) = pair_start_joiner("12345678");
        let (b, mb) = pair_start_member("12345678");
        assert_eq!(a.finish(&mb).unwrap(), b.finish(&ma).unwrap());

        let (a, ma) = pair_start_joiner("12345678");
        let (b, mb) = pair_start_member("12345679");
        assert_ne!(a.finish(&mb).unwrap(), b.finish(&ma).unwrap());
    }

    #[test]
    fn codes() {
        let c = new_code();
        assert_eq!(c.len(), 8);
        assert_eq!(normalize_code(&format_code(&c)).unwrap(), c);
        assert_eq!(normalize_code("4821-0937").unwrap(), "48210937");
        assert!(normalize_code("123").is_none());
    }

    #[test]
    fn proofs_bind_session() {
        let k = [7u8; 32];
        assert_ne!(group_proof(&k, "client", b"h1"), group_proof(&k, "client", b"h2"));
        assert_ne!(group_proof(&k, "client", b"h1"), group_proof(&k, "server", b"h1"));
        assert!(ct_eq(&group_proof(&k, "c", b"h"), &group_proof(&k, "c", b"h")));
    }
}
