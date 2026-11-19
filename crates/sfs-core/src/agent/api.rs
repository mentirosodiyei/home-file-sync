//! Herramientas que se ofrecen a las IA, sobre el [`Hub`].
//!
//! Las respuestas son texto en inglés pensado para un modelo de lenguaje
//! (el idioma en que mejor siguen instrucciones casi todos); los errores que
//! vienen del motor llegan en el idioma de la app.

use super::{exec, PeerAgentView, RunRequest, RunState, RunView};
use crate::hub::Hub;
use crate::Engine;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Contexto de quien llama: carpeta en la que trabaja la IA y su nombre.
#[derive(Clone, Debug, Default)]
pub struct CallCtx {
    pub cwd: Option<PathBuf>,
    pub client: String,
}

const DEFAULT_WAIT_S: u64 = 50;
const MAX_WAIT_S: u64 = 600;
const DEFAULT_OUTPUT: usize = 20_000;
const MAX_OUTPUT: usize = 200_000;

/// Descripción de las herramientas (`tools/list` de MCP).
pub fn tools() -> Value {
    json!([
        {
            "name": "list_machines",
            "description": "List the folders this computer shares with other computers through Home File Sync, and for each one the other computers (name, operating system, online, whether they accept commands from AI agents and which commands run without asking). Call this first to know which machine names you can use. The folder that contains your current working directory is marked.",
            "inputSchema": { "type": "object", "properties": {}, "additionalProperties": false },
            "annotations": { "readOnlyHint": true }
        },
        {
            "name": "run_command",
            "description": "Run a command on another computer that shares this project folder (for example the Windows PC when you are on a Mac), typically to build or run the tests there. Before running, Home File Sync makes sure that computer has received all your current changes; afterwards, files the command creates sync back. The command is executed directly, WITHOUT a shell: no pipes, redirections, globbing, variables or `&&`; pass the program and its arguments (e.g. \"cargo test -p core\" or [\"npm\",\"test\"]). Commands that the other computer has not allowed may need someone to approve them on its screen. Returns the exit code and output if it finishes within wait_seconds; otherwise returns a run_id to poll with get_run.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "machine": { "type": "string", "description": "Name of the other computer (from list_machines). Optional if only one other computer accepts commands." },
                    "command": {
                        "description": "Program and arguments, as a string (quotes allowed) or as an array of strings.",
                        "oneOf": [ { "type": "string" }, { "type": "array", "items": { "type": "string" } } ]
                    },
                    "folder": { "type": "string", "description": "Shared folder name. Optional: by default, the one that contains your working directory." },
                    "directory": { "type": "string", "description": "Subdirectory, relative to the shared folder, to run in. Default: the same relative directory you are working in." },
                    "timeout_seconds": { "type": "integer", "minimum": 1, "maximum": exec::MAX_TIMEOUT_S, "description": "Kill the command after this long. Default 1800." },
                    "wait_seconds": { "type": "integer", "minimum": 0, "maximum": MAX_WAIT_S, "description": "How long to wait for it to finish before returning a run_id. Default 50." },
                    "sync": { "type": "boolean", "description": "Wait until the other computer has your latest changes before running, and for generated files to come back afterwards. Default true." },
                    "max_output_chars": { "type": "integer", "minimum": 1000, "maximum": MAX_OUTPUT, "description": "Return at most this many characters of output (the end). Default 20000." }
                },
                "required": ["command"],
                "additionalProperties": false
            }
        },
        {
            "name": "get_run",
            "description": "Get the status and output of a command started with run_command, waiting up to wait_seconds for it to finish.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "run_id": { "type": "string" },
                    "wait_seconds": { "type": "integer", "minimum": 0, "maximum": MAX_WAIT_S, "description": "Default 50." },
                    "max_output_chars": { "type": "integer", "minimum": 1000, "maximum": MAX_OUTPUT }
                },
                "required": ["run_id"],
                "additionalProperties": false
            },
            "annotations": { "readOnlyHint": true }
        },
        {
            "name": "cancel_run",
            "description": "Stop a command started with run_command (and everything it launched).",
            "inputSchema": {
                "type": "object",
                "properties": { "run_id": { "type": "string" } },
                "required": ["run_id"],
                "additionalProperties": false
            }
        },
        {
            "name": "sync_now",
            "description": "Make sure your latest file changes have reached the other computers of the shared folder (and that you have theirs), waiting up to wait_seconds. Useful before asking a human to look at the other computer, or after editing many files.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "folder": { "type": "string", "description": "Shared folder name. Optional: by default, the one that contains your working directory." },
                    "wait_seconds": { "type": "integer", "minimum": 1, "maximum": MAX_WAIT_S, "description": "Default 60." }
                },
                "additionalProperties": false
            }
        }
    ])
}

/// Instrucciones generales para la IA (`initialize` de MCP).
pub const INSTRUCTIONS: &str = "Home File Sync keeps a folder identical on several computers of the same home network (for example a Mac and a Windows PC) and lets you run commands on the other computers. Use it to build and test your changes on another operating system: call list_machines, then run_command with the machine name. Files are synchronized automatically in both directions; you don't need to copy anything. Version-control folders such as .git are not synchronized when the folder is marked as a code project, so commit on one computer only. Commands run without a shell.";

fn arg_str<'a>(args: &'a Value, k: &str) -> Option<&'a str> {
    args.get(k).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty())
}

fn arg_u64(args: &Value, k: &str) -> Option<u64> {
    args.get(k).and_then(|v| v.as_u64().or_else(|| v.as_f64().map(|f| f.max(0.0) as u64)).or_else(|| v.as_str().and_then(|s| s.trim().parse().ok())))
}

struct Share {
    id: String,
    name: String,
    root: PathBuf,
    engine: Engine,
}

fn shares(hub: &Hub) -> Vec<Share> {
    let snap = hub.snapshot();
    snap.shares
        .iter()
        .filter_map(|s| {
            let engine = hub.share(&s.id)?;
            let root = engine.folder()?;
            Some(Share { id: s.id.clone(), name: s.state.group.as_ref().map(|g| g.name.clone()).unwrap_or_else(|| s.id.clone()), root, engine })
        })
        .collect()
}

fn canonical(p: &Path) -> PathBuf {
    crate::settings::canonical(p).unwrap_or_else(|_| p.to_path_buf())
}

/// Carpeta compartida: la pedida, la que contiene la carpeta de trabajo o la única.
fn pick_share(hub: &Hub, wanted: Option<&str>, ctx: &CallCtx) -> Result<Share, String> {
    let mut all = shares(hub);
    if all.is_empty() {
        return Err("This computer has no shared folders in Home File Sync yet. A human needs to create or join one in the app.".into());
    }
    if let Some(w) = wanted {
        let lw = w.to_lowercase();
        if let Some(i) = all.iter().position(|s| s.id == w || s.name.to_lowercase() == lw) {
            return Ok(all.swap_remove(i));
        }
        return Err(format!("There is no shared folder called \"{w}\". Shared folders: {}.", all.iter().map(|s| format!("\"{}\"", s.name)).collect::<Vec<_>>().join(", ")));
    }
    if let Some(cwd) = &ctx.cwd {
        let cwd = canonical(cwd);
        if let Some(i) = all.iter().position(|s| cwd.starts_with(&s.root)) {
            return Ok(all.swap_remove(i));
        }
    }
    if all.len() == 1 {
        return Ok(all.swap_remove(0));
    }
    Err(format!(
        "Your working directory is not inside a shared folder, and there are several. Pass \"folder\": {}.",
        all.iter().map(|s| format!("\"{}\"", s.name)).collect::<Vec<_>>().join(", ")
    ))
}

/// Nombres que ve el usuario (con sus alias) de los equipos de una carpeta.
fn display_names(hub: &Hub, share: &str) -> std::collections::HashMap<String, String> {
    hub.snapshot().shares.into_iter().find(|s| s.id == share).map(|s| s.state.devices.into_iter().map(|d| (d.id, d.name)).collect()).unwrap_or_default()
}

fn peers_of(hub: &Hub, s: &Share) -> Vec<PeerAgentView> {
    let names = display_names(hub, &s.id);
    let mut v = s.engine.agent_peers();
    for p in v.iter_mut() {
        if let Some(n) = names.get(&p.id) {
            p.name = n.clone();
        }
    }
    v.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    v
}

fn pick_machine(peers: &[PeerAgentView], wanted: Option<&str>) -> Result<PeerAgentView, String> {
    let names = || peers.iter().map(|p| format!("\"{}\"", p.name)).collect::<Vec<_>>().join(", ");
    if peers.is_empty() {
        return Err("No other computer has joined this shared folder yet.".into());
    }
    match wanted {
        Some(w) => {
            let lw = w.to_lowercase();
            let found: Vec<&PeerAgentView> = peers.iter().filter(|p| p.name.to_lowercase() == lw).collect();
            let found = if found.is_empty() { peers.iter().filter(|p| w.len() >= 6 && p.id.starts_with(&lw)).collect() } else { found };
            let found = if found.is_empty() { peers.iter().filter(|p| p.name.to_lowercase().contains(&lw)).collect() } else { found };
            match found.as_slice() {
                [one] => Ok((*one).clone()),
                [] => Err(format!("There is no computer called \"{w}\" in this shared folder. Computers: {}.", names())),
                _ => Err(format!("\"{w}\" matches several computers: {}. Use the full name.", found.iter().map(|p| format!("\"{}\"", p.name)).collect::<Vec<_>>().join(", "))),
            }
        }
        None => {
            let ready: Vec<&PeerAgentView> = peers.iter().filter(|p| p.online && p.exec).collect();
            match ready.as_slice() {
                [one] => Ok((*one).clone()),
                [] => Err(format!("Pass \"machine\". No other computer that is online accepts commands right now. Computers: {}.", names())),
                _ => Err(format!("Several computers accept commands; pass \"machine\": {}.", ready.iter().map(|p| format!("\"{}\"", p.name)).collect::<Vec<_>>().join(", "))),
            }
        }
    }
}

fn os_name(os: &str) -> &str {
    match os {
        "macos" => "macOS",
        "windows" => "Windows",
        "linux" => "Linux",
        o => o,
    }
}

fn run_id(share: &str, run: u64) -> String {
    format!("{share}-{run}")
}

fn parse_run_id(id: &str) -> Option<(&str, u64)> {
    let (s, r) = id.trim().rsplit_once('-')?;
    Some((s, r.parse().ok()?))
}

fn state_text(s: RunState) -> &'static str {
    match s {
        RunState::Preparing => "preparing (scanning local changes)",
        RunState::Queued => "queued",
        RunState::Approval => "waiting for a human to approve it on the other computer",
        RunState::Syncing => "waiting for the other computer to receive your changes",
        RunState::Running => "running",
        RunState::SyncingBack => "finished; receiving the files it generated",
        RunState::Finished => "finished",
        RunState::Refused => "refused",
        RunState::Cancelled => "cancelled",
        RunState::Lost => "connection lost",
    }
}

fn render(share: &str, os: Option<&str>, v: &RunView) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "Machine: {}{} · Command: {} · Directory: {}\n",
        v.machine,
        os.map(|o| format!(" ({})", os_name(o))).unwrap_or_default(),
        v.command,
        if v.subdir.is_empty() { "(shared folder root)".to_string() } else { v.subdir.clone() }
    ));
    out.push_str(&format!("Status: {}", state_text(v.state)));
    if let Some(c) = v.exit_code {
        out.push_str(&format!(" · Exit code: {c}"));
    }
    if let Some(ms) = v.duration_ms {
        out.push_str(&format!(" · Duration: {:.1} s", ms as f64 / 1000.0));
    }
    if let Some(n) = v.sync_files.filter(|n| *n > 0) {
        out.push_str(&format!(" · Files still syncing: {n}"));
    }
    out.push('\n');
    if let Some(r) = &v.reason {
        out.push_str(&format!("Reason: {r}\n"));
    }
    if !v.state.is_final() {
        out.push_str(&format!("Still in progress. Call get_run with run_id \"{}\" to wait for the result.\n", run_id(share, v.run)));
    }
    if v.output_bytes > 0 || v.state.is_final() {
        if v.truncated {
            out.push_str(&format!("--- output (last part of {} bytes) ---\n", v.output_bytes));
        } else {
            out.push_str("--- output ---\n");
        }
        out.push_str(&v.output);
        if !v.output.ends_with('\n') && !v.output.is_empty() {
            out.push('\n');
        }
        if v.output.is_empty() {
            out.push_str("(no output)\n");
        }
    }
    out
}

/// Ejecuta una herramienta. `Err` se devuelve a la IA como error de la herramienta.
pub async fn call(hub: &Hub, tool: &str, args: &Value, ctx: &CallCtx) -> Result<String, String> {
    match tool {
        "list_machines" => Ok(list_machines(hub, ctx)),
        "run_command" => run_command(hub, args, ctx).await,
        "get_run" => get_run(hub, args).await,
        "cancel_run" => {
            let id = arg_str(args, "run_id").ok_or("Missing run_id.")?;
            let (share, run) = parse_run_id(id).ok_or("Invalid run_id.")?;
            let e = hub.share(share).ok_or("Unknown run_id.")?;
            e.run_cancel(run)?;
            Ok(format!("Cancel requested for {id}."))
        }
        "sync_now" => {
            let s = pick_share(hub, arg_str(args, "folder"), ctx)?;
            let wait = Duration::from_secs(arg_u64(args, "wait_seconds").unwrap_or(60).clamp(1, MAX_WAIT_S));
            let rows = s.engine.sync_wait(wait).await;
            let names = display_names(hub, &s.id);
            if rows.is_empty() {
                return Ok(format!("Shared folder \"{}\": no other computer is connected right now; your changes will sync when they connect.", s.name));
            }
            let peers = s.engine.agent_peers();
            let mut out = format!("Shared folder \"{}\":\n", s.name);
            for (name, mine, theirs) in rows {
                let shown = peers.iter().find(|p| p.name == name).and_then(|p| names.get(&p.id)).cloned().unwrap_or(name);
                if mine == 0 && theirs == 0 {
                    out.push_str(&format!("- {shown}: in sync\n"));
                } else {
                    out.push_str(&format!("- {shown}: still syncing ({theirs} file(s) pending there, {mine} here)\n"));
                }
            }
            Ok(out)
        }
        other => Err(format!("Unknown tool: {other}")),
    }
}

fn list_machines(hub: &Hub, ctx: &CallCtx) -> String {
    let snap = hub.snapshot();
    let all = shares(hub);
    if all.is_empty() {
        return "This computer has no shared folders in Home File Sync yet. A human needs to create or join one in the app.".into();
    }
    let cwd = ctx.cwd.as_deref().map(canonical);
    let mut out = format!("This computer: \"{}\" ({})\n", snap.device_name, os_name(std::env::consts::OS));
    for s in &all {
        let here = cwd.as_ref().is_some_and(|c| c.starts_with(&s.root));
        out.push_str(&format!("\nShared folder \"{}\"{}\n  Local path: {}\n", s.name, if here { " ← your working directory is here" } else { "" }, s.root.display()));
        if s.engine.code_project() {
            out.push_str("  Code project: .gitignore rules apply and .git is not synchronized.\n");
        }
        let peers = peers_of(hub, s);
        if peers.is_empty() {
            out.push_str("  No other computers yet.\n");
        }
        for p in peers {
            let os = p.os.as_deref().map(os_name).unwrap_or("unknown OS");
            let status = if !p.online {
                "offline".to_string()
            } else if !p.supported {
                "online, version without command support".to_string()
            } else if !p.exec {
                "online, does not accept commands (a human can enable it in that computer's app)".to_string()
            } else {
                let allowed = if p.allowed.is_empty() { "none".to_string() } else { p.allowed.join(", ") };
                format!("online, accepts commands. Run without asking: {allowed}.{}", if p.ask { " Other commands need approval on its screen." } else { " Other commands are refused." })
            };
            out.push_str(&format!("  - \"{}\" ({os}): {status}\n", p.name));
        }
    }
    out
}

async fn run_command(hub: &Hub, args: &Value, ctx: &CallCtx) -> Result<String, String> {
    let argv: Vec<String> = match args.get("command") {
        Some(Value::String(s)) => exec::split_command(s)?,
        Some(Value::Array(a)) => a.iter().map(|v| v.as_str().map(String::from).ok_or("\"command\" must contain only strings.")).collect::<Result<_, _>>()?,
        _ => return Err("Missing \"command\".".into()),
    };
    if argv.is_empty() {
        return Err("The command is empty.".into());
    }
    if let Some(bad) = ["|", "&&", "||", ";", ">", ">>", "<", "2>&1"].iter().find(|op| argv.iter().any(|a| a == *op)) {
        return Err(format!("Commands run without a shell, so \"{bad}\" would be passed as a literal argument. Run a single program; for several steps make separate calls or use a script in the project."));
    }
    let s = pick_share(hub, arg_str(args, "folder"), ctx)?;
    let peers = peers_of(hub, &s);
    let peer = pick_machine(&peers, arg_str(args, "machine"))?;
    let subdir = match arg_str(args, "directory") {
        Some(d) => d.replace('\\', "/").trim_matches('/').trim_start_matches("./").to_string(),
        None => ctx
            .cwd
            .as_deref()
            .map(canonical)
            .and_then(|c| c.strip_prefix(&s.root).ok().map(|r| r.to_string_lossy().replace('\\', "/")))
            .unwrap_or_default(),
    };
    let subdir = if subdir == "." { String::new() } else { subdir };
    let sync = args.get("sync").and_then(Value::as_bool).unwrap_or(true);
    let req = RunRequest {
        argv,
        subdir,
        timeout_s: arg_u64(args, "timeout_seconds").map(|t| t.clamp(1, exec::MAX_TIMEOUT_S as u64) as u32).unwrap_or(exec::DEFAULT_TIMEOUT_S),
        client: ctx.client.clone(),
        wait_sync: sync,
        sync_back: sync,
    };
    let run = s.engine.run_remote(&peer.id, req)?;
    let wait = Duration::from_secs(arg_u64(args, "wait_seconds").unwrap_or(DEFAULT_WAIT_S).min(MAX_WAIT_S));
    let max = arg_u64(args, "max_output_chars").map(|m| m.clamp(1000, MAX_OUTPUT as u64) as usize).unwrap_or(DEFAULT_OUTPUT);
    let mut v = s.engine.run_wait(run, wait, max).await.ok_or("The run disappeared.")?;
    v.machine = peer.name.clone();
    let text = render(&s.id, peer.os.as_deref(), &v);
    if v.state == RunState::Refused {
        return Err(text);
    }
    Ok(text)
}

async fn get_run(hub: &Hub, args: &Value) -> Result<String, String> {
    let id = arg_str(args, "run_id").ok_or("Missing run_id.")?;
    let (share, run) = parse_run_id(id).ok_or("Invalid run_id.")?;
    let e = hub.share(share).ok_or("Unknown run_id.")?;
    let wait = Duration::from_secs(arg_u64(args, "wait_seconds").unwrap_or(DEFAULT_WAIT_S).min(MAX_WAIT_S));
    let max = arg_u64(args, "max_output_chars").map(|m| m.clamp(1000, MAX_OUTPUT as u64) as usize).unwrap_or(DEFAULT_OUTPUT);
    let v = e.run_wait(run, wait, max).await.ok_or("Unknown run_id (results are kept for one hour).")?;
    let peers = e.agent_peers();
    let names = display_names(hub, share);
    let mut v = v;
    let os = peers.iter().find(|p| p.name == v.machine).and_then(|p| {
        if let Some(n) = names.get(&p.id) {
            v.machine = n.clone();
        }
        p.os.clone()
    });
    Ok(render(share, os.as_deref(), &v))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_ids_roundtrip() {
        assert_eq!(parse_run_id(&run_id("a1b2c3", 42)), Some(("a1b2c3", 42)));
        assert_eq!(parse_run_id("nope"), None);
    }

    #[test]
    fn tools_are_valid_json_schemas() {
        let t = tools();
        let names: Vec<&str> = t.as_array().unwrap().iter().map(|x| x["name"].as_str().unwrap()).collect();
        assert_eq!(names, ["list_machines", "run_command", "get_run", "cancel_run", "sync_now"]);
        for x in t.as_array().unwrap() {
            assert_eq!(x["inputSchema"]["type"], "object");
        }
    }
}
