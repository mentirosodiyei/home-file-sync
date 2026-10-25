//! Canal cifrado sobre TCP con el protocolo Noise (`Noise_XX_25519_ChaChaPoly_BLAKE2s`).
//!
//! Cada equipo tiene un par de claves X25519 permanente. Tras el handshake
//! ambos conocen la clave pública del otro (de la que sale su identificador)
//! y un `handshake_hash` único de la sesión, que se usa para atar la
//! autenticación de grupo y el emparejamiento a esta conexión concreta
//! (evita ataques de intermediario que reenvíen mensajes).
//!
//! Encima de Noise se envían mensajes de aplicación de hasta [`MAX_MSG`]
//! bytes, troceados en mensajes Noise de como mucho 64 KiB.

use crate::protocol::{decode, encode, Msg};
use snow::{Builder, HandshakeState, StatelessTransportState};
use std::io;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::TcpStream;

pub const NOISE_PARAMS: &str = "Noise_XX_25519_ChaChaPoly_BLAKE2s";
/// Tamaño máximo de un mensaje de aplicación.
pub const MAX_MSG: usize = 4 * 1024 * 1024;
const NOISE_MAX: usize = 65535;
const TAG: usize = 16;
const CHUNK: usize = NOISE_MAX - TAG;

/// Genera un par de claves (privada, pública).
pub fn generate_keypair() -> (Vec<u8>, Vec<u8>) {
    let kp = Builder::new(NOISE_PARAMS.parse().unwrap()).generate_keypair().expect("generar claves");
    (kp.private, kp.public)
}

fn err(e: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, e.to_string())
}

pub struct Handshaken {
    pub reader: SecureReader,
    pub writer: SecureWriter,
    pub remote_static: Vec<u8>,
    pub handshake_hash: Vec<u8>,
}

async fn send_raw(w: &mut OwnedWriteHalf, data: &[u8]) -> io::Result<()> {
    let mut buf = Vec::with_capacity(2 + data.len());
    buf.extend_from_slice(&(data.len() as u16).to_be_bytes());
    buf.extend_from_slice(data);
    w.write_all(&buf).await
}

async fn recv_raw(r: &mut OwnedReadHalf, buf: &mut [u8]) -> io::Result<usize> {
    let mut len = [0u8; 2];
    r.read_exact(&mut len).await?;
    let n = u16::from_be_bytes(len) as usize;
    r.read_exact(&mut buf[..n]).await?;
    Ok(n)
}

/// Realiza el handshake Noise XX como iniciador o respondedor.
pub async fn handshake(stream: TcpStream, private_key: &[u8], initiator: bool) -> io::Result<Handshaken> {
    stream.set_nodelay(true)?;
    let (mut r, mut w) = stream.into_split();
    let builder = Builder::new(NOISE_PARAMS.parse().unwrap()).local_private_key(private_key);
    let mut hs: HandshakeState =
        if initiator { builder.build_initiator() } else { builder.build_responder() }.map_err(err)?;
    let mut msg = vec![0u8; NOISE_MAX];
    let mut payload = vec![0u8; NOISE_MAX];
    // XX: -> e ; <- e, ee, s, es ; -> s, se
    if initiator {
        let n = hs.write_message(&[], &mut msg).map_err(err)?;
        send_raw(&mut w, &msg[..n]).await?;
        let n = recv_raw(&mut r, &mut msg).await?;
        hs.read_message(&msg[..n], &mut payload).map_err(err)?;
        let n = hs.write_message(&[], &mut msg).map_err(err)?;
        send_raw(&mut w, &msg[..n]).await?;
    } else {
        let n = recv_raw(&mut r, &mut msg).await?;
        hs.read_message(&msg[..n], &mut payload).map_err(err)?;
        let n = hs.write_message(&[], &mut msg).map_err(err)?;
        send_raw(&mut w, &msg[..n]).await?;
        let n = recv_raw(&mut r, &mut msg).await?;
        hs.read_message(&msg[..n], &mut payload).map_err(err)?;
    }
    let remote_static = hs.get_remote_static().ok_or_else(|| err("sin clave remota"))?.to_vec();
    let handshake_hash = hs.get_handshake_hash().to_vec();
    let state = Arc::new(hs.into_stateless_transport_mode().map_err(err)?);
    Ok(Handshaken {
        reader: SecureReader { r, state: state.clone(), nonce: 0, buf: vec![0u8; NOISE_MAX], plain: vec![0u8; NOISE_MAX] },
        writer: SecureWriter { w, state, nonce: 0, out: vec![0u8; NOISE_MAX] },
        remote_static,
        handshake_hash,
    })
}

pub struct SecureReader {
    r: OwnedReadHalf,
    state: Arc<StatelessTransportState>,
    nonce: u64,
    buf: Vec<u8>,
    plain: Vec<u8>,
}

impl SecureReader {
    async fn read_chunk(&mut self) -> io::Result<usize> {
        let n = recv_raw(&mut self.r, &mut self.buf).await?;
        let m = self.state.read_message(self.nonce, &self.buf[..n], &mut self.plain).map_err(err)?;
        self.nonce += 1;
        Ok(m)
    }

    pub async fn recv(&mut self) -> io::Result<Msg> {
        let n = self.read_chunk().await?;
        if n < 4 {
            return Err(err("trama corta"));
        }
        let total = u32::from_be_bytes(self.plain[..4].try_into().unwrap()) as usize;
        if total > MAX_MSG {
            return Err(err("mensaje demasiado grande"));
        }
        let mut data = Vec::with_capacity(total);
        data.extend_from_slice(&self.plain[4..n]);
        while data.len() < total {
            let n = self.read_chunk().await?;
            data.extend_from_slice(&self.plain[..n]);
        }
        if data.len() != total {
            return Err(err("longitud de mensaje incoherente"));
        }
        decode(&data).map_err(err)
    }
}

pub struct SecureWriter {
    w: OwnedWriteHalf,
    state: Arc<StatelessTransportState>,
    nonce: u64,
    out: Vec<u8>,
}

impl SecureWriter {
    pub async fn send(&mut self, m: &Msg) -> io::Result<()> {
        let body = encode(m);
        if body.len() > MAX_MSG {
            return Err(err("mensaje demasiado grande"));
        }
        let mut plain = Vec::with_capacity(4 + body.len());
        plain.extend_from_slice(&(body.len() as u32).to_be_bytes());
        plain.extend_from_slice(&body);
        let mut wire = Vec::with_capacity(plain.len() + plain.len() / CHUNK * (TAG + 2) + TAG + 2);
        for chunk in plain.chunks(CHUNK) {
            let n = self.state.write_message(self.nonce, chunk, &mut self.out).map_err(err)?;
            self.nonce += 1;
            wire.extend_from_slice(&(n as u16).to_be_bytes());
            wire.extend_from_slice(&self.out[..n]);
        }
        self.w.write_all(&wire).await
    }

    pub async fn shutdown(&mut self) {
        let _ = self.w.shutdown().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;

    #[tokio::test]
    async fn roundtrip_large_and_small() {
        let (ka, pa) = generate_keypair();
        let (kb, pb) = generate_keypair();
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (s, _) = l.accept().await.unwrap();
            let mut h = handshake(s, &kb, false).await.unwrap();
            assert_eq!(h.remote_static, pa);
            let m = h.reader.recv().await.unwrap();
            h.writer.send(&m).await.unwrap();
            h.handshake_hash
        });
        let s = TcpStream::connect(addr).await.unwrap();
        let mut h = handshake(s, &ka, true).await.unwrap();
        assert_eq!(h.remote_static, pb);
        let data: Vec<u8> = (0..1_500_000u32).map(|i| (i % 251) as u8).collect();
        h.writer.send(&Msg::Block { id: 7, offset: 9, data: data.clone() }).await.unwrap();
        match h.reader.recv().await.unwrap() {
            Msg::Block { id, offset, data: d } => {
                assert_eq!((id, offset), (7, 9));
                assert_eq!(d, data);
            }
            other => panic!("inesperado {other:?}"),
        }
        assert_eq!(server.await.unwrap(), h.handshake_hash);
    }
}
