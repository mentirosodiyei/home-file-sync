// Interfaz de Show File Sync. Sin dependencias: consulta el estado del motor
// cada 700 ms y lo pinta. Fuera de Tauri (navegador) usa datos de ejemplo.
"use strict";

const $ = (id) => document.getElementById(id);
const invoke = window.__TAURI__ ? window.__TAURI__.core.invoke : demoInvoke;

const STATE_TEXT = {
  noFolder: "Sin carpeta",
  noGroup: "Sin grupo",
  alone: "Esperando equipos",
  synced: "Sincronizado",
  syncing: "Sincronizando",
  conflict: "Conflicto",
  error: "Error",
};

let state = null;
let busy = false;

function esc(s) {
  return String(s ?? "").replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c]);
}

function bytes(n) {
  const u = ["B", "KB", "MB", "GB", "TB"];
  let i = 0;
  while (n >= 1000 && i < u.length - 1) { n /= 1000; i++; }
  return i === 0 ? `${n} B` : `${n.toFixed(n < 10 ? 1 : 0)} ${u[i]}`;
}

function rate(n) {
  return n > 0 ? `${bytes(n)}/s` : "";
}

function eta(t) {
  if (!t.rate) return "";
  const s = Math.max(0, (t.size - t.done) / t.rate);
  if (s < 60) return `${Math.ceil(s)} s`;
  if (s < 3600) return `${Math.ceil(s / 60)} min`;
  return `${(s / 3600).toFixed(1)} h`;
}

function bind(name, value) {
  document.querySelectorAll(`[data-bind="${name}"]`).forEach((el) => {
    el.textContent = value ?? "";
    if (el.hasAttribute("hidden") || name === "networkWarning") el.hidden = !value;
  });
}

function show(id) {
  ["screen-folder", "screen-group", "screen-main"].forEach((s) => ($(s).hidden = s !== id));
}

function render(s) {
  state = s;
  const pill = $("overall");
  pill.hidden = !s.folder;
  pill.className = `pill ${s.overall}`;
  pill.textContent = s.overall === "syncing" && s.pendingFiles ? `Sincronizando · ${s.pendingFiles}` : STATE_TEXT[s.overall];

  bind("folder", s.folder);
  bind("deviceName", s.deviceName);
  bind("groupName", s.group ? `· ${s.group.name}` : "");
  bind("networkWarning", s.networkWarning);
  $("footer").hidden = !s.group;

  if (!s.folder) return show("screen-folder");
  if (!s.group) {
    renderOpenGroups(s);
    return show("screen-group");
  }
  show("screen-main");

  $("net-warning").hidden = !s.networkWarning;
  $("net-warning").textContent = s.networkWarning || "";
  $("folder-banner").hidden = !s.folderError;
  $("folder-banner").textContent = s.folderError || "";

  // Código de emparejamiento.
  $("pairing").hidden = !s.pairing;
  if (s.pairing) {
    $("pair-code").textContent = s.pairing.locked ? "Bloqueado" : s.pairing.code;
    const m = Math.floor(s.pairing.expiresInSecs / 60), sec = s.pairing.expiresInSecs % 60;
    $("pair-expiry").textContent = s.pairing.locked
      ? "Demasiados intentos fallidos. Pulsa «Añadir equipo» para generar otro código."
      : `Escríbelo en el otro equipo · caduca en ${m}:${String(sec).padStart(2, "0")}`;
  }

  // Equipos.
  $("dev-count").textContent = s.devices.length ? `· ${s.devices.length}` : "";
  $("no-devices").hidden = s.devices.length > 0;
  $("devices").innerHTML = s.devices
    .map((d) => `<li class="${d.state}" title="${esc(d.detail)}"><span class="dot ${d.state}"></span><span class="name">${esc(d.name)}</span><span class="detail">${esc(d.detail)}</span></li>`)
    .join("");

  // Actividad.
  const parts = [];
  if (s.pendingFiles) parts.push(`${s.pendingFiles} archivo(s) pendiente(s) · ${bytes(s.pendingBytes)}`);
  if (s.downRate) parts.push(`↓ ${rate(s.downRate)}`);
  if (s.upRate) parts.push(`↑ ${rate(s.upRate)}`);
  if (!parts.length) parts.push(`${s.fileCount} archivo(s) · ${bytes(s.totalBytes)} · todo al día`);
  $("summary").textContent = parts.join("  ·  ");

  const transfers = [...s.transfers].sort((a, b) => a.path.localeCompare(b.path));
  $("transfers").innerHTML = transfers
    .map((t) => {
      const pct = t.size ? Math.floor((t.done / t.size) * 100) : 100;
      const verb = t.download ? `Recibiendo de ${esc(t.peer)}` : `Enviando a ${esc(t.peer)}`;
      const extra = [rate(t.rate), eta(t) && `quedan ${eta(t)}`].filter(Boolean).join(" · ");
      return `<li><div class="row"><span class="file">${esc(t.path)}</span><span class="pct">${pct} %</span></div>
        <div class="bar"><i style="width:${pct}%"></i></div>
        <div class="sub">${verb} · ${bytes(t.done)} de ${bytes(t.size)}${extra ? " · " + extra : ""}</div></li>`;
    })
    .join("");

  $("conflicts").hidden = !s.conflicts;
  $("conflict-text").textContent =
    s.conflicts === 1
      ? "Se ha resuelto un conflicto: dos equipos tenían versiones distintas del mismo archivo. Se han conservado ambas (busca «conflicto» en el nombre)."
      : `Se han resuelto ${s.conflicts} conflictos conservando ambas versiones (busca «conflicto» en el nombre).`;

  $("errors").innerHTML = s.errors
    .slice(0, 8)
    .map((e) => `<li>${e.path ? `<b>${esc(e.path)}</b>: ` : ""}${esc(e.message)}</li>`)
    .join("");

  $("events").innerHTML = s.events
    .map((e) => {
      const d = new Date(e.timeMs);
      const t = d.toLocaleTimeString("es-ES", { hour: "2-digit", minute: "2-digit", second: "2-digit" });
      return `<li><time>${t}</time><span>${esc(e.text)}</span></li>`;
    })
    .join("");
}

function renderOpenGroups(s) {
  const box = $("open-groups");
  const groups = s.openGroups || [];
  if (groups.length <= 1) {
    box.innerHTML = groups.length
      ? `<p class="muted">Grupo detectado: <b>${esc(groups[0].groupName)}</b> (${esc(groups[0].deviceName)})</p>`
      : `<p class="muted">Buscando grupos en la red…</p>`;
    return;
  }
  const selected = box.querySelector("input:checked")?.value;
  box.innerHTML = groups
    .map((g, i) => `<label><input type="radio" name="grp" value="${esc(g.deviceId)}" ${selected === g.deviceId || (!selected && i === 0) ? "checked" : ""}/> ${esc(g.groupName)} <span class="muted">(${esc(g.deviceName)})</span></label>`)
    .join("");
}

async function refresh() {
  try {
    render(await invoke("get_state"));
  } catch (e) {
    console.error(e);
  }
}

// ---------------------------------------------------------------------------
// Acciones
// ---------------------------------------------------------------------------

function ask(text, { input = null, ok = "Aceptar", danger = false } = {}) {
  return new Promise((resolve) => {
    const d = $("dialog");
    $("dialog-text").textContent = text;
    const inp = $("dialog-input");
    inp.hidden = input === null;
    inp.value = input ?? "";
    $("dialog-ok").textContent = ok;
    $("dialog-ok").style.background = danger ? "var(--red)" : "";
    d.onclose = () => resolve(d.returnValue === "ok" ? (input === null ? true : inp.value) : null);
    d.showModal();
    if (input !== null) inp.select();
  });
}

const actions = {
  async "choose-folder"() {
    const err = $("folder-error");
    try {
      await invoke("choose_folder");
      err.hidden = true;
    } catch (e) {
      err.textContent = String(e);
      err.hidden = false;
      if (state?.folder) await ask(String(e));
    }
  },
  async "create-group"() {
    await invoke("create_group");
  },
  async "add-device"() {
    await invoke("open_pairing");
  },
  async "close-pairing"() {
    await invoke("close_pairing");
  },
  async "open-folder"() {
    await invoke("open_folder");
  },
  async "ack-conflicts"() {
    await invoke("ack_conflicts");
  },
  async rename() {
    const name = await ask("Nombre de este equipo, tal como lo verán los demás:", { input: state?.deviceName ?? "" });
    if (name && name.trim()) await invoke("rename_device", { name: name.trim() });
  },
  async leave() {
    const ok = await ask(
      "¿Salir del grupo? Este equipo dejará de sincronizarse. Los archivos de tu carpeta no se borran. Para volver necesitarás un código nuevo.",
      { ok: "Salir del grupo", danger: true }
    );
    if (ok) await invoke("leave_group");
  },
};

document.addEventListener("click", async (ev) => {
  const menu = $("menu");
  if (ev.target.id === "menu-btn") {
    menu.hidden = !menu.hidden;
    return;
  }
  if (!menu.hidden && !menu.contains(ev.target)) menu.hidden = true;
  const btn = ev.target.closest("[data-action]");
  if (!btn || busy) return;
  menu.hidden = true;
  busy = true;
  try {
    await actions[btn.dataset.action]?.();
  } catch (e) {
    await ask(String(e));
  } finally {
    busy = false;
    refresh();
  }
});

// Código: solo cifras, formato "1234 5678".
$("code").addEventListener("input", (e) => {
  const d = e.target.value.replace(/\D/g, "").slice(0, 8);
  e.target.value = d.length > 4 ? `${d.slice(0, 4)} ${d.slice(4)}` : d;
});

$("join-form").addEventListener("submit", async (e) => {
  e.preventDefault();
  const code = $("code").value.replace(/\D/g, "");
  const err = $("join-error");
  if (code.length !== 8) {
    err.textContent = "El código tiene 8 cifras.";
    err.hidden = false;
    return;
  }
  const btn = $("join-btn");
  btn.disabled = true;
  btn.textContent = "Uniendo…";
  err.hidden = true;
  try {
    const deviceId = $("open-groups").querySelector("input:checked")?.value ?? null;
    await invoke("join_group", { code, deviceId });
    $("code").value = "";
  } catch (x) {
    err.textContent = String(x);
    err.hidden = false;
  } finally {
    btn.disabled = false;
    btn.textContent = "Unirse";
    refresh();
  }
});

refresh();
setInterval(refresh, 700);

// ---------------------------------------------------------------------------
// Modo demostración (abrir index.html en un navegador): datos de ejemplo.
// ?demo=folder | group | main (por defecto)
// ---------------------------------------------------------------------------
function demoInvoke(cmd) {
  const mode = new URLSearchParams(location.search).get("demo") || "main";
  if (cmd !== "get_state") return Promise.resolve(null);
  const t = Date.now() / 1000;
  const p = (t * 7) % 100;
  return Promise.resolve({
    deviceName: "MacBook Pro de Ana",
    deviceId: "demo",
    folder: mode === "folder" ? null : "/Users/ana/Presentaciones",
    folderError: null,
    group: mode === "main" ? { id: "g", name: "Grupo de MacBook Pro de Ana" } : null,
    pairing: mode === "main" ? { code: "4821 0937", expiresInSecs: 812, locked: false } : null,
    overall: "syncing",
    devices: [
      { id: "1", name: "PC Presentación 1", state: "synced", detail: "Sincronizado", pendingFiles: 0, pendingBytes: 0 },
      { id: "2", name: "PC Presentación 2", state: "receiving", detail: "Recibiendo archivos · faltan 3 (1.2 GB)", pendingFiles: 3, pendingBytes: 1.2e9 },
      { id: "3", name: "PC Backup", state: "synced", detail: "Sincronizado", pendingFiles: 0, pendingBytes: 0 },
      { id: "4", name: "Portátil Regiduría", state: "offline", detail: "Desconectado", pendingFiles: 0, pendingBytes: 0 },
    ],
    transfers: [
      { path: "Día 1/Presentacion_Final.pptx", peer: "PC Presentación 2", download: false, size: 84e6, done: (84e6 * p) / 100, rate: 61e6 },
      { path: "Vídeos/Apertura 4K.mov", peer: "PC Presentación 2", download: false, size: 2.4e9, done: 0.8e9, rate: 58e6 },
    ],
    pendingFiles: 3,
    pendingBytes: 1.2e9,
    downRate: 0,
    upRate: 119e6,
    fileCount: 128,
    totalBytes: 18.4e9,
    conflicts: 1,
    errors: [],
    events: [
      { timeMs: Date.now() - 4000, kind: "conflict", text: "Conflicto en «Agenda.pdf»: se conservan ambas versiones" },
      { timeMs: Date.now() - 60000, kind: "recv", text: "Recibido Día 1/Ponencia Dr. Núñez.key" },
      { timeMs: Date.now() - 120000, kind: "net", text: "Conectado con PC Backup" },
    ],
    openGroups: mode === "group" ? [{ groupId: "g", groupName: "Grupo de MacBook Pro de Ana", deviceId: "x", deviceName: "MacBook Pro de Ana" }] : [],
    networkWarning: null,
  });
}
