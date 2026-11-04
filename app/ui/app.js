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
  if (!$("settings").hidden) $("leave-row").hidden = !s.group;
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
  settings() {
    openSettings("general");
  },
  diagnostics() {
    openSettings("advanced");
  },
  "close-settings"() {
    closeSettings();
  },
  async forget() {
    await invoke("forget_addresses");
    loadRemembered();
  },
  async "copy-logs"() {
    const text = await invoke("logs_text", { level: $("log-level").value, query: $("log-query").value });
    try {
      await navigator.clipboard.writeText(text);
      $("log-status").textContent = "Registro copiado al portapapeles.";
    } catch {
      const r = document.createRange();
      r.selectNodeContents($("log"));
      getSelection().removeAllRanges();
      getSelection().addRange(r);
      document.execCommand("copy");
      $("log-status").textContent = "Registro copiado al portapapeles.";
    }
  },
  async "export-logs"() {
    const path = await invoke("export_logs");
    if (path) $("log-status").textContent = `Registro exportado a ${path}`;
  },
  async "open-logs"() {
    await invoke("open_logs_folder");
  },
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
    if (ok) {
      await invoke("leave_group");
      closeSettings();
    }
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

function showJoinError(text) {
  const box = $("join-error");
  const [main, detail] = String(text).split(/\n\nDetalle: /);
  box.innerHTML = `${esc(main)}${detail ? `<span class="detail">${esc(detail)}</span>` : ""}<button class="link" data-action="diagnostics">Ver registro de diagnóstico</button>`;
  box.hidden = false;
}

$("join-form").addEventListener("submit", async (e) => {
  e.preventDefault();
  const code = $("code").value.replace(/\D/g, "");
  const err = $("join-error");
  if (code.length !== 8) {
    showJoinError("El código tiene 8 cifras.");
    return;
  }
  const manual = $("manual").open && $("m-host").value.trim() !== "";
  if ($("manual").open && !manual) {
    showJoinError("Escribe la dirección IP o el nombre del equipo, o cierra «Conexión manual» para usar la detección automática.");
    return;
  }
  const btn = $("join-btn");
  btn.disabled = true;
  btn.textContent = "Uniendo…";
  err.hidden = true;
  try {
    if (manual) {
      await invoke("join_manual", {
        host: $("m-host").value.trim(),
        port: $("m-port").value.trim() || null,
        code,
        remember: $("m-remember").checked,
      });
      $("m-host").value = "";
      $("m-port").value = "";
      $("m-remember").checked = false;
    } else {
      const deviceId = $("open-groups").querySelector("input:checked")?.value ?? null;
      await invoke("join_group", { code, deviceId });
    }
    $("code").value = "";
  } catch (x) {
    showJoinError(x);
  } finally {
    btn.disabled = false;
    btn.textContent = "Unirse";
    refresh();
  }
});

$("m-port").addEventListener("input", (e) => (e.target.value = e.target.value.replace(/\D/g, "").slice(0, 5)));

// ---------------------------------------------------------------------------
// Ajustes y diagnóstico
// ---------------------------------------------------------------------------

let logTimer = null;

function openSettings(tab = "general") {
  $("settings").hidden = false;
  selectTab(tab);
  loadRemembered();
}

function closeSettings() {
  $("settings").hidden = true;
  clearInterval(logTimer);
  logTimer = null;
}

function selectTab(tab) {
  document.querySelectorAll(".tabs button").forEach((b) => b.classList.toggle("active", b.dataset.tab === tab));
  $("tab-general").hidden = tab !== "general";
  $("tab-advanced").hidden = tab !== "advanced";
  clearInterval(logTimer);
  logTimer = null;
  if (tab === "advanced") {
    loadLogs(true);
    logTimer = setInterval(() => loadLogs(false), 1500);
  }
}

async function loadRemembered() {
  try {
    const list = await invoke("remembered_addresses");
    $("remembered").textContent = list && list.length ? list.join(", ") : "Ninguna";
    $("forget-btn").hidden = !(list && list.length);
  } catch (e) {
    console.error(e);
  }
  $("leave-row").hidden = !state?.group;
}

async function loadLogs(scrollToEnd) {
  const pre = $("log");
  const atEnd = pre.scrollTop + pre.clientHeight >= pre.scrollHeight - 20;
  try {
    const v = await invoke("get_logs", { level: $("log-level").value, query: $("log-query").value });
    $("log-debug").checked = !!v.debug;
    pre.innerHTML = v.entries
      .map((e) => {
        const t = new Date(e.timeMs).toLocaleTimeString("es-ES", { hour: "2-digit", minute: "2-digit", second: "2-digit" });
        return `<span class="l-${e.level}"><time>${t}</time> ${e.level.padEnd(5)} ${esc(e.message)}</span>`;
      })
      .join("\n");
    $("log-status").textContent = `${v.entries.length} entrada(s) mostradas${v.dir ? ` · archivos en ${v.dir}` : ""}`;
    if (scrollToEnd || atEnd) pre.scrollTop = pre.scrollHeight;
  } catch (e) {
    $("log-status").textContent = String(e);
  }
}

document.querySelectorAll(".tabs button").forEach((b) => b.addEventListener("click", () => selectTab(b.dataset.tab)));
$("log-level").addEventListener("change", () => loadLogs(true));
$("log-query").addEventListener("input", () => loadLogs(true));
$("log-debug").addEventListener("change", async (e) => {
  await invoke("set_debug_logging", { on: e.target.checked });
  if (e.target.checked) $("log-level").value = "DEBUG";
  loadLogs(true);
});
document.addEventListener("keydown", (e) => {
  if (e.key === "Escape" && !$("settings").hidden && !$("dialog").open) closeSettings();
});

refresh();
setInterval(refresh, 700);

// ---------------------------------------------------------------------------
// Modo demostración (abrir index.html en un navegador): datos de ejemplo.
// ?demo=folder | group | main (por defecto)
// ---------------------------------------------------------------------------
function demoInvoke(cmd, args) {
  const mode = new URLSearchParams(location.search).get("demo") || "main";
  if (cmd === "get_logs") {
    const now = Date.now();
    const all = [
      ["INFO", "Show File Sync 0.1.1 iniciado (macos aarch64)"],
      ["INFO", "Descubierto «PORTATIL-SALON» (id 7a3f19c2, grupo «Grupo de PORTATIL-SALON», acepta equipos, protocolo v1)"],
      ["INFO", "«PORTATIL-SALON» anuncia 192.168.1.66:47800 vía baliza UDP (paquete recibido desde 192.168.1.66:52011), interfaz local: en8"],
      ["INFO", "«PORTATIL-SALON» anuncia 172.27.160.1:47800 vía mDNS, interfaz local: ninguna en esa subred"],
      ["INFO", "Emparejamiento: direcciones candidatas en orden: [192.168.1.66:47800 (baliza UDP, interfaz en8), 172.27.160.1:47800 (mDNS, fuera de las subredes locales)]"],
      ["WARN", "Intento de conexión fallido: 192.168.1.66:47800 (baliza UDP, interfaz en8) TCP → fallo en conexión TCP: No route to host (os error 65) [error del sistema 65]"],
      ["ERROR", "macOS deniega a Show File Sync las conexiones salientes a la red local (permiso «Red local»)"],
      ["DEBUG", "Conexión entrante desde 192.168.1.70:50122"],
    ].map(([level, message], i) => ({ timeMs: now - (8 - i) * 1000, level, target: "peer", message }));
    const rank = { ERROR: 4, WARN: 3, INFO: 2, DEBUG: 1 };
    const q = (args?.query || "").toLowerCase();
    return Promise.resolve({
      entries: all.filter((e) => rank[e.level] >= rank[args?.level || "INFO"] && e.message.toLowerCase().includes(q)),
      debug: false,
      dir: "/Users/ana/Library/Application Support/org.showfilesync.app/logs",
    });
  }
  if (cmd === "remembered_addresses") return Promise.resolve(["192.168.1.66:47800"]);
  if (cmd === "join_manual" || cmd === "join_group")
    return Promise.reject(
      "macOS está bloqueando las conexiones de Show File Sync hacia la red local. Abre Ajustes del Sistema → Privacidad y seguridad → Red local y activa Show File Sync.\n\nDetalle: 192.168.1.66:47800 (manual, interfaz en8) TCP → fallo en conexión TCP: No route to host (os error 65) [error del sistema 65]"
    );
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
