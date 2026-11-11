// Interfaz de Home File Sync. Sin dependencias: consulta el estado del hub
// cada 700 ms y lo pinta. Fuera de Tauri (navegador) usa datos de ejemplo.
"use strict";

const $ = (id) => document.getElementById(id);
const invoke = window.__TAURI__ ? window.__TAURI__.core.invoke : demoInvoke;

const STATE_TEXT = {
  noFolder: "Sin carpeta",
  noGroup: "Sin grupo",
  alone: "Esperando equipos",
  synced: "Al día",
  syncing: "Sincronizando",
  conflict: "Conflicto",
  error: "Error",
};

let state = null;
let busy = false;
const picked = { "new-folder": "", "join-folder": "" };

function esc(s) {
  return String(s ?? "").replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c]);
}

function bytes(n) {
  const u = ["B", "KB", "MB", "GB", "TB"];
  let i = 0;
  while (n >= 1000 && i < u.length - 1) { n /= 1000; i++; }
  return i === 0 ? `${n} B` : `${n.toFixed(n < 10 ? 1 : 0)} ${u[i]}`;
}

const rate = (n) => (n > 0 ? `${bytes(n)}/s` : "");

function bind(name, value) {
  document.querySelectorAll(`[data-bind="${name}"]`).forEach((el) => (el.textContent = value ?? ""));
}

// ---------------------------------------------------------------------------
// Pintado
// ---------------------------------------------------------------------------

function render(s) {
  state = s;
  bind("deviceName", s.deviceName);
  $("empty").hidden = s.shares.length > 0;

  $("net-warning").hidden = !s.networkWarning;
  $("net-warning").textContent = s.networkWarning || "";

  const inv = $("invites");
  inv.hidden = !s.invites.length;
  inv.innerHTML = s.invites.length
    ? `<span>${s.invites.length === 1 ? `«${esc(s.invites[0].groupName)}» de ${esc(s.invites[0].deviceName)} está esperando equipos.` : `${s.invites.length} carpetas esperando equipos en tu red.`}</span><button class="link" data-action="join">Unirme</button>`
    : "";

  $("shares").innerHTML = s.shares.map(renderShare).join("");
  if (!$("sheet-receive").hidden) renderReceive(s.receive);
  if (!$("sheet-join").hidden) renderJoinInvites();
}

function renderShare(sh) {
  const name = sh.group ? sh.group.name : "Carpeta";
  const backup = sh.mode === "backup";
  const devices = sh.devices.length
    ? sh.devices
        .map(
          (d) => `<li class="${d.state}" title="${esc(d.detail)}">
            <span class="dot ${d.state}"></span>
            <button class="name link-plain" data-action="alias" data-device="${esc(d.id)}" data-name="${esc(d.name)}" title="Cambiar cómo ves este equipo">${esc(d.name)}</button>
            ${d.backup ? '<span class="tag">copia</span>' : ""}
            <span class="detail">${esc(d.detail)}</span></li>`
        )
        .join("")
    : `<li class="muted empty-li">Nadie más todavía. Pulsa «Invitar» y escribe el código en el otro equipo.</li>`;

  const parts = [];
  if (sh.pendingFiles) parts.push(`${sh.pendingFiles} pendiente(s) · ${bytes(sh.pendingBytes)}`);
  if (sh.downRate) parts.push(`↓ ${rate(sh.downRate)}`);
  if (sh.upRate) parts.push(`↑ ${rate(sh.upRate)}`);
  if (!parts.length) parts.push(`${sh.fileCount} archivo(s) · ${bytes(sh.totalBytes)}`);

  const transfers = [...sh.transfers]
    .sort((a, b) => a.path.localeCompare(b.path))
    .map((t) => {
      const pct = t.size ? Math.floor((t.done / t.size) * 100) : 100;
      return `<li><div class="row"><span class="file">${esc(t.path)}</span><span class="pct">${pct} %</span></div>
        <div class="bar"><i style="width:${pct}%"></i></div>
        <div class="sub">${t.download ? `De ${esc(t.peer)}` : `A ${esc(t.peer)}`} · ${bytes(t.done)} de ${bytes(t.size)}${t.rate ? " · " + rate(t.rate) : ""}</div></li>`;
    })
    .join("");

  const pairing = sh.pairing
    ? `<div class="pairing"><div><div class="label">Código para unirse a «${esc(name)}»</div>
        <div class="code">${sh.pairing.locked ? "Bloqueado" : esc(sh.pairing.code)}</div>
        <div class="muted small">${sh.pairing.locked ? "Demasiados intentos fallidos. Pulsa «Invitar» para generar otro." : `En el otro equipo: «Unirme con código» · caduca en ${Math.floor(sh.pairing.expiresInSecs / 60)} min`}</div></div>
        <button class="link" data-action="close-invite" data-share="${sh.id}">Cerrar</button></div>`
    : "";

  const conflicts = sh.conflicts
    ? `<div class="banner conflict"><span>${sh.conflicts === 1 ? "Un archivo se cambió a la vez en dos equipos: la otra versión está en la carpeta «old»." : `${sh.conflicts} archivos se cambiaron a la vez en dos equipos: las otras versiones están en «old».`}</span><button class="link" data-action="ack" data-share="${sh.id}">Entendido</button></div>`
    : "";
  const errors = sh.errors.slice(0, 5).map((e) => `<li>${e.path ? `<b>${esc(e.path)}</b>: ` : ""}${esc(e.message)}</li>`).join("");

  return `<article class="share card" data-share="${sh.id}">
    <div class="share-head">
      <button class="share-name link-plain" data-action="rename-share" data-share="${sh.id}" data-name="${esc(name)}" title="Cambiar el nombre (solo en este equipo)">${esc(name)}</button>
      <span class="mode-tag ${backup ? "backup" : ""}">${backup ? "Copia de seguridad" : "Sincronizada"}</span>
      <span class="spacer-flex"></span>
      <span class="pill ${sh.overall}">${sh.overall === "syncing" && sh.pendingFiles ? `Sincronizando · ${sh.pendingFiles}` : STATE_TEXT[sh.overall]}</span>
    </div>
    <div class="folder-line"><code class="path">${esc(sh.folder || "")}</code>
      <span class="actions-inline"><button class="link" data-action="open-folder" data-share="${sh.id}">Abrir</button><button class="link" data-action="change-folder" data-share="${sh.id}">Cambiar</button></span></div>
    ${sh.folderError ? `<div class="banner error">${esc(sh.folderError)}</div>` : ""}
    ${pairing}
    <ul class="devices">${devices}</ul>
    <div class="summary">${parts.join("  ·  ")}</div>
    ${transfers ? `<ul class="transfers">${transfers}</ul>` : ""}
    ${conflicts}
    ${errors ? `<ul class="errors">${errors}</ul>` : ""}
    <div class="share-actions">
      <button class="secondary" data-action="invite" data-share="${sh.id}">Invitar equipo</button>
      <button class="secondary" data-action="receive" data-share="${sh.id}">📱 Recibir aquí</button>
      <button class="secondary" data-action="toggle-mode" data-share="${sh.id}" data-mode="${sh.mode}">${backup ? "Pasar a sincronizada" : "Pasar a copia de seguridad"}</button>
      <span class="spacer-flex"></span>
      <button class="link danger-text" data-action="leave" data-share="${sh.id}" data-name="${esc(name)}">Dejar de compartir</button>
    </div>
  </article>`;
}

async function refresh() {
  try {
    render(await invoke("get_state"));
  } catch (e) {
    console.error(e);
  }
}

// ---------------------------------------------------------------------------
// Diálogos
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

function openSheet(id) {
  document.querySelectorAll(".sheet").forEach((s) => (s.hidden = s.id !== id));
}

function closeSheets() {
  document.querySelectorAll(".sheet").forEach((s) => (s.hidden = true));
  clearInterval(logTimer);
  logTimer = null;
}

function showError(id, text) {
  const box = $(id);
  const [main, detail] = String(text).split(/\n\nDetalle: /);
  box.innerHTML = `${esc(main)}${detail ? `<span class="detail">${esc(detail)}</span>` : ""}<button type="button" class="link" data-action="diagnostics">Ver registro de diagnóstico</button>`;
  box.hidden = false;
}

function setPicked(id, path) {
  picked[id] = path || "";
  $(id).textContent = path || "Sin elegir";
}

function radio(name) {
  return document.querySelector(`input[name="${name}"]:checked`)?.value || "sync";
}

function renderJoinInvites() {
  const box = $("join-invites");
  const invites = state?.invites || [];
  if (!invites.length) {
    box.innerHTML = `<p class="muted small">No se ve ninguna. En el otro equipo pulsa «Invitar equipo» en la carpeta que quieras compartir.</p>`;
    return;
  }
  const sel = box.querySelector("input:checked")?.value;
  box.innerHTML = invites
    .map((i, n) => `<label class="invite-opt"><input type="radio" name="join-invite" value="${esc(i.key)}" ${sel === i.key || (!sel && n === 0) ? "checked" : ""}/> <b>${esc(i.groupName)}</b> <span class="muted">de ${esc(i.deviceName)}</span></label>`)
    .join("");
}

function renderReceive(rx) {
  if (!rx) {
    $("rx-qr").innerHTML = "";
    $("rx-url").textContent = "";
    $("rx-expiry").textContent = "La recepción está cerrada.";
    return;
  }
  if ($("rx-qr").dataset.url !== rx.url) {
    $("rx-qr").innerHTML = rx.qrSvg;
    $("rx-qr").dataset.url = rx.url;
  }
  $("rx-url").textContent = rx.url;
  $("rx-other").textContent = rx.otherUrls.length ? `Otras direcciones: ${rx.otherUrls.join("  ·  ")}` : "";
  $("rx-expiry").textContent = `Se guarda en ${rx.label} · el enlace caduca en ${Math.ceil(rx.expiresInSecs / 60)} min`;
  $("rx-list").innerHTML = rx.received.length
    ? rx.received.map((f) => `<li>✓ ${esc(f.name)} <span class="muted">${bytes(f.size)}</span></li>`).join("")
    : `<li class="muted">Todavía nada</li>`;
}

async function startReceive(share) {
  const sel = $("rx-dest");
  sel.innerHTML =
    (state?.shares || []).map((s) => `<option value="${s.id}">Carpeta compartida «${esc(s.group ? s.group.name : "")}»</option>`).join("") +
    `<option value="">Carpeta «Recibidos» de este equipo</option>`;
  sel.value = share ?? (state?.shares[0]?.id || "");
  openSheet("sheet-receive");
  renderReceive(await invoke("start_receive", { share: sel.value || null }));
}

$("rx-dest").addEventListener("change", async (e) => renderReceive(await invoke("start_receive", { share: e.target.value || null })));

// ---------------------------------------------------------------------------
// Acciones
// ---------------------------------------------------------------------------

const actions = {
  "new-share"() {
    $("new-name").value = "";
    setPicked("new-folder", "");
    $("new-error").hidden = true;
    openSheet("sheet-new");
    $("new-name").focus();
  },
  join() {
    $("join-code").value = "";
    setPicked("join-folder", "");
    $("join-error").hidden = true;
    renderJoinInvites();
    openSheet("sheet-join");
  },
  receive(b) {
    return startReceive(b.dataset.share);
  },
  async "stop-receive"() {
    await invoke("stop_receive");
    closeSheets();
  },
  async invite(b) {
    await invoke("open_invite", { id: b.dataset.share });
  },
  async "close-invite"(b) {
    await invoke("close_invite", { id: b.dataset.share });
  },
  async ack(b) {
    await invoke("ack_conflicts", { id: b.dataset.share });
  },
  async "open-folder"(b) {
    await invoke("open_folder", { id: b.dataset.share });
  },
  async "change-folder"(b) {
    await invoke("change_folder", { id: b.dataset.share });
  },
  async "toggle-mode"(b) {
    const toBackup = b.dataset.mode !== "backup";
    const ok = await ask(
      toBackup
        ? "Como copia de seguridad, este equipo solo recibirá: sus cambios no se enviarán y lo que se borre en los demás se conservará aquí."
        : "Como sincronizada, los cambios de este equipo se enviarán a los demás y los borrados se replicarán.",
      { ok: toBackup ? "Pasar a copia de seguridad" : "Pasar a sincronizada" }
    );
    if (ok) await invoke("set_mode", { id: b.dataset.share, shareMode: toBackup ? "backup" : "sync" });
  },
  async "rename-share"(b) {
    const name = await ask("Nombre de esta carpeta compartida (solo se cambia en este equipo):", { input: b.dataset.name });
    if (name && name.trim()) await invoke("rename_share", { id: b.dataset.share, name: name.trim() });
  },
  async alias(b) {
    const name = await ask("¿Cómo quieres ver a este equipo? Solo lo verás tú; déjalo vacío para usar su nombre propio.", { input: b.dataset.name });
    if (name !== null) await invoke("set_alias", { deviceId: b.dataset.device, alias: name.trim() });
  },
  async "rename-me"() {
    const name = await ask("Nombre de este equipo, tal como lo verán los demás:", { input: state?.deviceName ?? "" });
    if (name && name.trim()) await invoke("rename_device", { name: name.trim() });
  },
  async leave(b) {
    const ok = await ask(`¿Dejar de compartir «${b.dataset.name}» en este equipo? Los archivos de la carpeta no se borran. Para volver necesitarás un código nuevo.`, {
      ok: "Dejar de compartir",
      danger: true,
    });
    if (ok) await invoke("leave_share", { id: b.dataset.share });
  },
  settings() {
    openSettings("general");
  },
  diagnostics() {
    openSettings("advanced");
  },
  "close-settings"() {
    closeSheets();
  },
  async "copy-logs"() {
    const text = await invoke("logs_text", { level: $("log-level").value, query: $("log-query").value });
    try {
      await navigator.clipboard.writeText(text);
    } catch {
      const r = document.createRange();
      r.selectNodeContents($("log"));
      getSelection().removeAllRanges();
      getSelection().addRange(r);
      document.execCommand("copy");
    }
    $("log-status").textContent = "Registro copiado al portapapeles.";
  },
  async "export-logs"() {
    const p = await invoke("export_logs");
    if (p) $("log-status").textContent = `Registro exportado a ${p}`;
  },
  async "open-logs"() {
    await invoke("open_logs_folder");
  },
};

document.addEventListener("click", async (ev) => {
  if (ev.target.closest("[data-close]")) return closeSheets();
  const pick = ev.target.closest("[data-pick]");
  if (pick) {
    const p = await invoke("pick_folder");
    if (p) setPicked(pick.dataset.pick, p);
    return;
  }
  const btn = ev.target.closest("[data-action]");
  if (!btn || busy) return;
  busy = true;
  try {
    await actions[btn.dataset.action]?.(btn);
  } catch (e) {
    await ask(String(e));
  } finally {
    busy = false;
    refresh();
  }
});

$("join-code").addEventListener("input", (e) => {
  const d = e.target.value.replace(/\D/g, "").slice(0, 8);
  e.target.value = d.length > 4 ? `${d.slice(0, 4)} ${d.slice(4)}` : d;
});
$("m-port").addEventListener("input", (e) => (e.target.value = e.target.value.replace(/\D/g, "").slice(0, 5)));

$("form-new").addEventListener("submit", async (e) => {
  e.preventDefault();
  const name = $("new-name").value.trim();
  if (!name) return showError("new-error", "Ponle un nombre a la carpeta compartida.");
  if (!picked["new-folder"]) return showError("new-error", "Elige la carpeta de este equipo.");
  try {
    await invoke("create_share", { name, folder: picked["new-folder"], shareMode: radio("new-mode") });
    closeSheets();
    refresh();
  } catch (x) {
    showError("new-error", x);
  }
});

$("form-join").addEventListener("submit", async (e) => {
  e.preventDefault();
  const code = $("join-code").value.replace(/\D/g, "");
  if (code.length !== 8) return showError("join-error", "El código tiene 8 cifras.");
  if (!picked["join-folder"]) return showError("join-error", "Elige dónde guardar la carpeta en este equipo.");
  const manual = $("manual").open && $("m-host").value.trim();
  const btn = $("join-btn");
  btn.disabled = true;
  btn.textContent = "Uniendo…";
  $("join-error").hidden = true;
  try {
    if (manual) {
      await invoke("join_manual", {
        host: $("m-host").value.trim(),
        port: $("m-port").value.trim() || null,
        code,
        folder: picked["join-folder"],
        shareMode: radio("join-mode"),
        remember: $("m-remember").checked,
      });
    } else {
      const invite = document.querySelector('input[name="join-invite"]:checked')?.value ?? null;
      await invoke("join_share", { invite, code, folder: picked["join-folder"], shareMode: radio("join-mode") });
    }
    closeSheets();
  } catch (x) {
    showError("join-error", x);
  } finally {
    btn.disabled = false;
    btn.textContent = "Unirme";
    refresh();
  }
});

// ---------------------------------------------------------------------------
// Ajustes y diagnóstico
// ---------------------------------------------------------------------------

let logTimer = null;

function openSettings(tab) {
  openSheet("settings");
  selectTab(tab);
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
    $("log-status").textContent = `${v.entries.length} entrada(s)${v.dir ? ` · archivos en ${v.dir}` : ""}`;
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
  if (e.key === "Escape" && !$("dialog").open) closeSheets();
});

refresh();
setInterval(refresh, 700);

// ---------------------------------------------------------------------------
// Modo demostración (abrir index.html en un navegador): datos de ejemplo.
// ?demo=empty | main (por defecto)
// ---------------------------------------------------------------------------
function demoInvoke(cmd, args) {
  const mode = new URLSearchParams(location.search).get("demo") || "main";
  const share = (id, name, folder, m, devices, extra = {}) => ({
    id, mode: m, folder, folderError: null, group: { id, name }, pairing: null, overall: "synced",
    devices, transfers: [], pendingFiles: 0, pendingBytes: 0, downRate: 0, upRate: 0, fileCount: 214, totalBytes: 3.2e9,
    conflicts: 0, errors: [], events: [], openGroups: [], networkWarning: null, deviceName: "Portátil de Papá", deviceId: "demo", ...extra,
  });
  const dev = (id, name, st, detail, backup = false) => ({ id, name, state: st, detail, pendingFiles: 0, pendingBytes: 0, backup });
  if (cmd === "get_state")
    return Promise.resolve({
      deviceName: "Portátil de Papá",
      deviceId: "demo",
      networkWarning: null,
      receive: null,
      invites: mode === "main" ? [{ key: "k", deviceId: "x", deviceName: "Tablet Salón", groupId: "g", groupName: "Recetas" }] : [],
      shares:
        mode === "empty"
          ? []
          : [
              share("a", "Pareja", "/Users/papa/Pareja", "sync", [dev("1", "Mamá", "synced", "Sincronizado")], {
                overall: "syncing", pendingFiles: 2, pendingBytes: 4.1e8,
                transfers: [{ path: "Vacaciones/IMG_2041.MOV", peer: "Mamá", download: true, size: 3.8e8, done: 1.3e8, rate: 4.2e7 }],
              }),
              share("b", "Papá e hijo", "/Users/papa/Hijo", "sync", [dev("2", "PC de Leo", "offline", "Desconectado")], {
                overall: "alone", pairing: { code: "4821 0937", expiresInSecs: 812, locked: false },
              }),
              share("c", "Copia del salón", "/Volumes/Disco/Copias", "backup", [dev("3", "iMac Salón", "synced", "Sincronizado")]),
            ],
    });
  if (cmd === "pick_folder") return Promise.resolve("/Users/papa/Documentos/Nueva");
  if (cmd === "start_receive")
    return Promise.resolve({
      url: "http://192.168.1.20:47830/r/3f9a…",
      otherUrls: [],
      qrSvg: '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 10 10"><rect width="10" height="10" fill="#fff"/><path d="M1 1h3v3H1zM6 1h3v3H6zM1 6h3v3H1zM6 6h1v1H6zM8 8h1v1H8z" fill="#000"/></svg>',
      label: "la carpeta compartida «Pareja»",
      expiresInSecs: 880,
      received: [{ name: "IMG_3301.HEIC", size: 2.4e6 }],
    });
  if (cmd === "get_logs") return Promise.resolve({ entries: [], debug: false, dir: null });
  return Promise.resolve(null);
}
