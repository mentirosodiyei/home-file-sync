//! Registro de diagnóstico persistente.
//!
//! Recoge los mensajes de `tracing` del propio programa (descubrimiento,
//! conexiones, emparejamiento, sincronización) y los guarda:
//! * en memoria, los últimos [`RING_CAPACITY`] para el visor de la interfaz;
//! * en disco, en `logs/showfilesync.log` con rotación automática
//!   ([`MAX_FILE_BYTES`] por archivo, [`MAX_FILES`] archivos como máximo).
//!
//! Niveles ERROR, WARN e INFO siempre; DEBUG solo si se activa el registro
//! detallado. De otras bibliotecas solo se guardan avisos y errores.
//!
//! El motor nunca registra códigos de emparejamiento, claves ni contenido de
//! archivos; aquí además se descartan campos con nombres sensibles.

use serde::Serialize;
use std::collections::VecDeque;
use std::fmt::Write as _;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use tracing::field::{Field, Visit};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::layer::{Context, Layer};

pub const RING_CAPACITY: usize = 3000;
pub const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;
pub const MAX_FILES: usize = 5;
const FILE_NAME: &str = "showfilesync.log";
const OWN_TARGETS: &[&str] = &["sfs_core", "show_file_sync", "sfs_cli", "showfilesync"];
const SENSITIVE_FIELDS: &[&str] = &[
    "code", "codigo", "key", "secret", "password", "token", "proof", "spake",
];

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LogEntry {
    pub time_ms: i64,
    pub level: &'static str,
    pub target: String,
    pub message: String,
}

impl LogEntry {
    fn line(&self) -> String {
        format!(
            "{} {:<5} {} {}\n",
            iso_time(self.time_ms),
            self.level,
            self.target,
            self.message
        )
    }
}

fn level_rank(l: &str) -> u8 {
    match l {
        "ERROR" => 4,
        "WARN" => 3,
        "INFO" => 2,
        _ => 1,
    }
}

/// Fecha UTC ISO-8601 con milisegundos.
fn iso_time(ms: i64) -> String {
    let secs = ms.div_euclid(1000);
    let st = std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs.max(0) as u64);
    let s = crate::fsops::stamp(st); // 2026-10-25_183005Z
    let (d, t) = s.split_once('_').unwrap_or((&s, "000000Z"));
    format!(
        "{d}T{}:{}:{}.{:03}Z",
        &t[0..2],
        &t[2..4],
        &t[4..6],
        ms.rem_euclid(1000)
    )
}

struct RotatingFile {
    dir: PathBuf,
    file: Option<File>,
    size: u64,
}

impl RotatingFile {
    fn open(dir: &Path) -> std::io::Result<Self> {
        std::fs::create_dir_all(dir)?;
        let path = dir.join(FILE_NAME);
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        let size = file.metadata()?.len();
        Ok(RotatingFile {
            dir: dir.to_path_buf(),
            file: Some(file),
            size,
        })
    }

    fn rotate(&mut self) {
        self.file = None;
        let p = |i: usize| {
            if i == 0 {
                self.dir.join(FILE_NAME)
            } else {
                self.dir.join(format!("{FILE_NAME}.{i}"))
            }
        };
        let _ = std::fs::remove_file(p(MAX_FILES - 1));
        for i in (0..MAX_FILES - 1).rev() {
            let _ = std::fs::rename(p(i), p(i + 1));
        }
        self.file = OpenOptions::new().create(true).append(true).open(p(0)).ok();
        self.size = 0;
    }

    fn write(&mut self, line: &str) {
        if self.size + line.len() as u64 > MAX_FILE_BYTES {
            self.rotate();
        }
        if let Some(f) = self.file.as_mut() {
            if f.write_all(line.as_bytes()).is_ok() {
                self.size += line.len() as u64;
            }
        }
    }

    /// Archivos existentes, del más antiguo al más reciente.
    fn files(&self) -> Vec<PathBuf> {
        (0..MAX_FILES)
            .rev()
            .map(|i| {
                if i == 0 {
                    self.dir.join(FILE_NAME)
                } else {
                    self.dir.join(format!("{FILE_NAME}.{i}"))
                }
            })
            .filter(|p| p.exists())
            .collect()
    }
}

/// Registro compartido (uno por proceso).
pub struct DiagLog {
    ring: Mutex<VecDeque<LogEntry>>,
    file: Mutex<Option<RotatingFile>>,
    debug: AtomicBool,
}

static GLOBAL: OnceLock<Arc<DiagLog>> = OnceLock::new();

/// Registro global del proceso.
pub fn global() -> Arc<DiagLog> {
    GLOBAL
        .get_or_init(|| {
            Arc::new(DiagLog {
                ring: Mutex::new(VecDeque::new()),
                file: Mutex::new(None),
                debug: AtomicBool::new(false),
            })
        })
        .clone()
}

/// Activa la escritura en disco en `dir` (normalmente `<datos>/logs`).
pub fn init_file(dir: &Path) -> std::io::Result<()> {
    let f = RotatingFile::open(dir)?;
    *global().file.lock().unwrap() = Some(f);
    Ok(())
}

/// Capa de `tracing` que alimenta el registro global.
pub fn layer() -> DiagLayer {
    DiagLayer { log: global() }
}

pub fn set_debug(on: bool) {
    global().debug.store(on, Ordering::Relaxed);
    tracing::info!(
        "Registro detallado (DEBUG) {}",
        if on { "activado" } else { "desactivado" }
    );
}

pub fn debug_enabled() -> bool {
    global().debug.load(Ordering::Relaxed)
}

/// Entradas en memoria filtradas por nivel mínimo y texto (sin distinguir mayúsculas).
pub fn entries(min_level: &str, query: &str, limit: usize) -> Vec<LogEntry> {
    let min = level_rank(min_level);
    let q = query.trim().to_lowercase();
    let log = global();
    let ring = log.ring.lock().unwrap();
    let mut v: Vec<LogEntry> = ring
        .iter()
        .rev()
        .filter(|e| level_rank(e.level) >= min)
        .filter(|e| {
            q.is_empty()
                || e.message.to_lowercase().contains(&q)
                || e.target.to_lowercase().contains(&q)
        })
        .take(limit)
        .cloned()
        .collect();
    v.reverse();
    v
}

/// Texto plano de las entradas (para copiar).
pub fn as_text(entries: &[LogEntry]) -> String {
    entries.iter().map(|e| e.line()).collect()
}

/// Exporta todo el registro en disco (y lo que haya en memoria si no hay
/// archivo) a `dest`, con una cabecera con datos del sistema.
pub fn export(dest: &Path, header: &str) -> std::io::Result<()> {
    let log = global();
    let mut out = File::create(dest)?;
    writeln!(out, "Show File Sync — registro de diagnóstico\n{header}\n")?;
    let files = log
        .file
        .lock()
        .unwrap()
        .as_ref()
        .map(|f| f.files())
        .unwrap_or_default();
    if files.is_empty() {
        out.write_all(
            as_text(&log.ring.lock().unwrap().iter().cloned().collect::<Vec<_>>()).as_bytes(),
        )?;
    } else {
        for f in files {
            out.write_all(&std::fs::read(f)?)?;
        }
    }
    out.flush()
}

/// Carpeta de los registros en disco, si está activa.
pub fn dir() -> Option<PathBuf> {
    global()
        .file
        .lock()
        .unwrap()
        .as_ref()
        .map(|f| f.dir.clone())
}

pub struct DiagLayer {
    log: Arc<DiagLog>,
}

#[derive(Default)]
struct MsgVisitor {
    message: String,
    fields: String,
}

impl Visit for MsgVisitor {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        let name = field.name();
        if name == "message" {
            let _ = write!(self.message, "{value:?}");
        } else if !SENSITIVE_FIELDS
            .iter()
            .any(|s| name.to_lowercase().contains(s))
        {
            let _ = write!(self.fields, " {name}={value:?}");
        }
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.message.push_str(value);
        } else {
            self.record_debug(field, &value);
        }
    }
}

impl<S: Subscriber> Layer<S> for DiagLayer {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let meta = event.metadata();
        let own = OWN_TARGETS.iter().any(|t| meta.target().starts_with(t));
        let level = *meta.level();
        let keep = match level {
            Level::ERROR | Level::WARN => true,
            Level::INFO => own,
            Level::DEBUG => own && self.log.debug.load(Ordering::Relaxed),
            Level::TRACE => false,
        };
        if !keep {
            return;
        }
        let mut v = MsgVisitor::default();
        event.record(&mut v);
        let entry = LogEntry {
            time_ms: crate::scanner::now_ns() / 1_000_000,
            level: match level {
                Level::ERROR => "ERROR",
                Level::WARN => "WARN",
                Level::INFO => "INFO",
                _ => "DEBUG",
            },
            target: meta.target().split("::").last().unwrap_or("").to_string(),
            message: format!("{}{}", v.message, v.fields),
        };
        if let Some(f) = self.log.file.lock().unwrap().as_mut() {
            f.write(&entry.line());
        }
        let mut ring = self.log.ring.lock().unwrap();
        if ring.len() >= RING_CAPACITY {
            ring.pop_front();
        }
        ring.push_back(entry);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rotation_respects_limits() {
        let t = tempfile::tempdir().unwrap();
        let mut f = RotatingFile::open(t.path()).unwrap();
        let line = "x".repeat(1000) + "\n";
        for _ in 0..(MAX_FILE_BYTES as usize / line.len()) * (MAX_FILES + 3) {
            f.write(&line);
        }
        let files = f.files();
        assert_eq!(files.len(), MAX_FILES, "nunca más de {MAX_FILES} archivos");
        for p in files {
            assert!(std::fs::metadata(&p).unwrap().len() <= MAX_FILE_BYTES);
        }
    }

    #[test]
    fn iso_format() {
        assert_eq!(iso_time(1_761_417_005_123), "2025-10-25T18:30:05.123Z");
    }
}
