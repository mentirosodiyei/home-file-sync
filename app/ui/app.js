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
  loadNetwork();
}

async function loadNetwork() {
  try {
    const n = await invoke("get_network");
    $("vpn-toggle").checked = !!n.listenVpn;
    const addr = n.addresses.map((a) => `<li><code class="selectable">${esc(a.ip)}</code> <span class="muted">${esc(a.interface)}${a.tunnel ? " · VPN" : ""}</span></li>`).join("");
    const ports = n.ports.map((p) => `<li>«${esc(p.share)}» · puerto <code class="selectable">${p.port}</code></li>`).join("");
    $("net-info").innerHTML =
      `<div><b class="small">Direcciones de este equipo</b><ul class="plain">${addr || '<li class="muted">Sin red</li>'}</ul></div>` +
      `<div><b class="small">Puerto de cada carpeta</b><ul class="plain">${ports || '<li class="muted">Ninguna carpeta todavía</li>'}</ul></div>`;
    if (!$("app-version").textContent) $("app-version").textContent = `Home File Sync ${await invoke("app_version")}`;
  } catch (e) {
    console.error(e);
  }
}

$("vpn-toggle").addEventListener("change", async (e) => {
  await invoke("set_listen_vpn", { on: e.target.checked });
  loadNetwork();
});

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

if (!window.__TAURI__) {
  const open = new URLSearchParams(location.search).get("open");
  const demo = { join: () => actions.join(), new: () => actions["new-share"](), receive: () => startReceive("a"), settings: () => openSettings("general") };
  if (demo[open]) setTimeout(async () => {
    await refresh();
    await demo[open]();
    if (open === "new") { $("new-name").value = "Fotos de la familia"; setPicked("new-folder", "/Users/papa/Fotos"); }
    if (open === "join") { $("join-code").value = "4821 0937"; setPicked("join-folder", "/Users/mama/Recetas"); }
  }, 300);
}

// ---------------------------------------------------------------------------
// Modo demostración (abrir index.html en un navegador): datos de ejemplo.
// ?demo=empty | main (por defecto)
// ---------------------------------------------------------------------------
var DEMO_QR = '<svg xmlns="http://www.w3.org/2000/svg" version="1.1" width="246" height="246" viewBox="0 0 246 246" shape-rendering="crispEdges"><rect x="0" y="0" width="246" height="246" fill="#fff"/><path fill="#000" d="M24 24h6v6H24V24M30 24h6v6H30V24M36 24h6v6H36V24M42 24h6v6H42V24M48 24h6v6H48V24M54 24h6v6H54V24M60 24h6v6H60V24M78 24h6v6H78V24M84 24h6v6H84V24M90 24h6v6H90V24M96 24h6v6H96V24M102 24h6v6H102V24M114 24h6v6H114V24M120 24h6v6H120V24M126 24h6v6H126V24M138 24h6v6H138V24M156 24h6v6H156V24M162 24h6v6H162V24M168 24h6v6H168V24M180 24h6v6H180V24M186 24h6v6H186V24M192 24h6v6H192V24M198 24h6v6H198V24M204 24h6v6H204V24M210 24h6v6H210V24M216 24h6v6H216V24M24 30h6v6H24V30M60 30h6v6H60V30M108 30h6v6H108V30M120 30h6v6H120V30M132 30h6v6H132V30M150 30h6v6H150V30M156 30h6v6H156V30M168 30h6v6H168V30M180 30h6v6H180V30M216 30h6v6H216V30M24 36h6v6H24V36M36 36h6v6H36V36M42 36h6v6H42V36M48 36h6v6H48V36M60 36h6v6H60V36M72 36h6v6H72V36M78 36h6v6H78V36M90 36h6v6H90V36M126 36h6v6H126V36M132 36h6v6H132V36M150 36h6v6H150V36M162 36h6v6H162V36M180 36h6v6H180V36M192 36h6v6H192V36M198 36h6v6H198V36M204 36h6v6H204V36M216 36h6v6H216V36M24 42h6v6H24V42M36 42h6v6H36V42M42 42h6v6H42V42M48 42h6v6H48V42M60 42h6v6H60V42M72 42h6v6H72V42M78 42h6v6H78V42M84 42h6v6H84V42M108 42h6v6H108V42M132 42h6v6H132V42M144 42h6v6H144V42M150 42h6v6H150V42M168 42h6v6H168V42M180 42h6v6H180V42M192 42h6v6H192V42M198 42h6v6H198V42M204 42h6v6H204V42M216 42h6v6H216V42M24 48h6v6H24V48M36 48h6v6H36V48M42 48h6v6H42V48M48 48h6v6H48V48M60 48h6v6H60V48M72 48h6v6H72V48M96 48h6v6H96V48M102 48h6v6H102V48M114 48h6v6H114V48M132 48h6v6H132V48M144 48h6v6H144V48M156 48h6v6H156V48M168 48h6v6H168V48M180 48h6v6H180V48M192 48h6v6H192V48M198 48h6v6H198V48M204 48h6v6H204V48M216 48h6v6H216V48M24 54h6v6H24V54M60 54h6v6H60V54M72 54h6v6H72V54M78 54h6v6H78V54M84 54h6v6H84V54M90 54h6v6H90V54M96 54h6v6H96V54M108 54h6v6H108V54M114 54h6v6H114V54M120 54h6v6H120V54M132 54h6v6H132V54M138 54h6v6H138V54M150 54h6v6H150V54M162 54h6v6H162V54M180 54h6v6H180V54M216 54h6v6H216V54M24 60h6v6H24V60M30 60h6v6H30V60M36 60h6v6H36V60M42 60h6v6H42V60M48 60h6v6H48V60M54 60h6v6H54V60M60 60h6v6H60V60M72 60h6v6H72V60M84 60h6v6H84V60M96 60h6v6H96V60M108 60h6v6H108V60M120 60h6v6H120V60M132 60h6v6H132V60M144 60h6v6H144V60M156 60h6v6H156V60M168 60h6v6H168V60M180 60h6v6H180V60M186 60h6v6H186V60M192 60h6v6H192V60M198 60h6v6H198V60M204 60h6v6H204V60M210 60h6v6H210V60M216 60h6v6H216V60M72 66h6v6H72V66M78 66h6v6H78V66M84 66h6v6H84V66M90 66h6v6H90V66M102 66h6v6H102V66M114 66h6v6H114V66M120 66h6v6H120V66M126 66h6v6H126V66M132 66h6v6H132V66M144 66h6v6H144V66M150 66h6v6H150V66M24 72h6v6H24V72M36 72h6v6H36V72M42 72h6v6H42V72M48 72h6v6H48V72M54 72h6v6H54V72M60 72h6v6H60V72M78 72h6v6H78V72M96 72h6v6H96V72M108 72h6v6H108V72M126 72h6v6H126V72M132 72h6v6H132V72M138 72h6v6H138V72M162 72h6v6H162V72M180 72h6v6H180V72M186 72h6v6H186V72M192 72h6v6H192V72M198 72h6v6H198V72M204 72h6v6H204V72M30 78h6v6H30V78M48 78h6v6H48V78M54 78h6v6H54V78M66 78h6v6H66V78M78 78h6v6H78V78M108 78h6v6H108V78M114 78h6v6H114V78M126 78h6v6H126V78M138 78h6v6H138V78M162 78h6v6H162V78M168 78h6v6H168V78M174 78h6v6H174V78M180 78h6v6H180V78M186 78h6v6H186V78M198 78h6v6H198V78M204 78h6v6H204V78M24 84h6v6H24V84M30 84h6v6H30V84M48 84h6v6H48V84M60 84h6v6H60V84M72 84h6v6H72V84M84 84h6v6H84V84M96 84h6v6H96V84M150 84h6v6H150V84M180 84h6v6H180V84M192 84h6v6H192V84M204 84h6v6H204V84M210 84h6v6H210V84M24 90h6v6H24V90M30 90h6v6H30V90M42 90h6v6H42V90M48 90h6v6H48V90M66 90h6v6H66V90M72 90h6v6H72V90M78 90h6v6H78V90M90 90h6v6H90V90M96 90h6v6H96V90M114 90h6v6H114V90M120 90h6v6H120V90M126 90h6v6H126V90M138 90h6v6H138V90M150 90h6v6H150V90M168 90h6v6H168V90M192 90h6v6H192V90M198 90h6v6H198V90M204 90h6v6H204V90M24 96h6v6H24V96M30 96h6v6H30V96M42 96h6v6H42V96M48 96h6v6H48V96M54 96h6v6H54V96M60 96h6v6H60V96M66 96h6v6H66V96M78 96h6v6H78V96M84 96h6v6H84V96M90 96h6v6H90V96M96 96h6v6H96V96M138 96h6v6H138V96M150 96h6v6H150V96M156 96h6v6H156V96M168 96h6v6H168V96M180 96h6v6H180V96M198 96h6v6H198V96M216 96h6v6H216V96M24 102h6v6H24V102M48 102h6v6H48V102M54 102h6v6H54V102M66 102h6v6H66V102M78 102h6v6H78V102M84 102h6v6H84V102M96 102h6v6H96V102M114 102h6v6H114V102M120 102h6v6H120V102M126 102h6v6H126V102M144 102h6v6H144V102M168 102h6v6H168V102M198 102h6v6H198V102M210 102h6v6H210V102M216 102h6v6H216V102M30 108h6v6H30V108M54 108h6v6H54V108M60 108h6v6H60V108M66 108h6v6H66V108M72 108h6v6H72V108M84 108h6v6H84V108M90 108h6v6H90V108M102 108h6v6H102V108M108 108h6v6H108V108M114 108h6v6H114V108M126 108h6v6H126V108M132 108h6v6H132V108M138 108h6v6H138V108M156 108h6v6H156V108M162 108h6v6H162V108M204 108h6v6H204V108M210 108h6v6H210V108M24 114h6v6H24V114M30 114h6v6H30V114M36 114h6v6H36V114M54 114h6v6H54V114M78 114h6v6H78V114M84 114h6v6H84V114M90 114h6v6H90V114M108 114h6v6H108V114M114 114h6v6H114V114M150 114h6v6H150V114M168 114h6v6H168V114M174 114h6v6H174V114M192 114h6v6H192V114M198 114h6v6H198V114M204 114h6v6H204V114M30 120h6v6H30V120M36 120h6v6H36V120M42 120h6v6H42V120M54 120h6v6H54V120M60 120h6v6H60V120M72 120h6v6H72V120M78 120h6v6H78V120M84 120h6v6H84V120M90 120h6v6H90V120M96 120h6v6H96V120M120 120h6v6H120V120M126 120h6v6H126V120M132 120h6v6H132V120M138 120h6v6H138V120M144 120h6v6H144V120M162 120h6v6H162V120M186 120h6v6H186V120M192 120h6v6H192V120M198 120h6v6H198V120M204 120h6v6H204V120M216 120h6v6H216V120M30 126h6v6H30V126M36 126h6v6H36V126M42 126h6v6H42V126M48 126h6v6H48V126M54 126h6v6H54V126M66 126h6v6H66V126M96 126h6v6H96V126M108 126h6v6H108V126M114 126h6v6H114V126M126 126h6v6H126V126M132 126h6v6H132V126M138 126h6v6H138V126M156 126h6v6H156V126M162 126h6v6H162V126M168 126h6v6H168V126M174 126h6v6H174V126M180 126h6v6H180V126M186 126h6v6H186V126M198 126h6v6H198V126M204 126h6v6H204V126M210 126h6v6H210V126M48 132h6v6H48V132M60 132h6v6H60V132M72 132h6v6H72V132M78 132h6v6H78V132M90 132h6v6H90V132M96 132h6v6H96V132M132 132h6v6H132V132M156 132h6v6H156V132M168 132h6v6H168V132M174 132h6v6H174V132M180 132h6v6H180V132M192 132h6v6H192V132M204 132h6v6H204V132M210 132h6v6H210V132M30 138h6v6H30V138M36 138h6v6H36V138M54 138h6v6H54V138M84 138h6v6H84V138M96 138h6v6H96V138M102 138h6v6H102V138M108 138h6v6H108V138M114 138h6v6H114V138M150 138h6v6H150V138M162 138h6v6H162V138M192 138h6v6H192V138M198 138h6v6H198V138M204 138h6v6H204V138M216 138h6v6H216V138M24 144h6v6H24V144M36 144h6v6H36V144M42 144h6v6H42V144M54 144h6v6H54V144M60 144h6v6H60V144M72 144h6v6H72V144M78 144h6v6H78V144M84 144h6v6H84V144M90 144h6v6H90V144M96 144h6v6H96V144M108 144h6v6H108V144M120 144h6v6H120V144M132 144h6v6H132V144M144 144h6v6H144V144M150 144h6v6H150V144M162 144h6v6H162V144M180 144h6v6H180V144M186 144h6v6H186V144M198 144h6v6H198V144M210 144h6v6H210V144M216 144h6v6H216V144M24 150h6v6H24V150M30 150h6v6H30V150M48 150h6v6H48V150M54 150h6v6H54V150M66 150h6v6H66V150M72 150h6v6H72V150M78 150h6v6H78V150M90 150h6v6H90V150M102 150h6v6H102V150M108 150h6v6H108V150M114 150h6v6H114V150M126 150h6v6H126V150M132 150h6v6H132V150M144 150h6v6H144V150M150 150h6v6H150V150M156 150h6v6H156V150M168 150h6v6H168V150M174 150h6v6H174V150M216 150h6v6H216V150M24 156h6v6H24V156M48 156h6v6H48V156M54 156h6v6H54V156M60 156h6v6H60V156M66 156h6v6H66V156M78 156h6v6H78V156M84 156h6v6H84V156M108 156h6v6H108V156M150 156h6v6H150V156M180 156h6v6H180V156M204 156h6v6H204V156M210 156h6v6H210V156M24 162h6v6H24V162M114 162h6v6H114V162M120 162h6v6H120V162M138 162h6v6H138V162M144 162h6v6H144V162M150 162h6v6H150V162M156 162h6v6H156V162M174 162h6v6H174V162M186 162h6v6H186V162M192 162h6v6H192V162M198 162h6v6H198V162M24 168h6v6H24V168M60 168h6v6H60V168M66 168h6v6H66V168M84 168h6v6H84V168M96 168h6v6H96V168M108 168h6v6H108V168M126 168h6v6H126V168M138 168h6v6H138V168M144 168h6v6H144V168M162 168h6v6H162V168M168 168h6v6H168V168M174 168h6v6H174V168M180 168h6v6H180V168M186 168h6v6H186V168M192 168h6v6H192V168M204 168h6v6H204V168M210 168h6v6H210V168M72 174h6v6H72V174M102 174h6v6H102V174M108 174h6v6H108V174M114 174h6v6H114V174M126 174h6v6H126V174M132 174h6v6H132V174M138 174h6v6H138V174M168 174h6v6H168V174M192 174h6v6H192V174M204 174h6v6H204V174M24 180h6v6H24V180M30 180h6v6H30V180M36 180h6v6H36V180M42 180h6v6H42V180M48 180h6v6H48V180M54 180h6v6H54V180M60 180h6v6H60V180M78 180h6v6H78V180M84 180h6v6H84V180M96 180h6v6H96V180M102 180h6v6H102V180M114 180h6v6H114V180M120 180h6v6H120V180M150 180h6v6H150V180M162 180h6v6H162V180M168 180h6v6H168V180M180 180h6v6H180V180M192 180h6v6H192V180M204 180h6v6H204V180M216 180h6v6H216V180M24 186h6v6H24V186M60 186h6v6H60V186M72 186h6v6H72V186M84 186h6v6H84V186M90 186h6v6H90V186M96 186h6v6H96V186M102 186h6v6H102V186M108 186h6v6H108V186M114 186h6v6H114V186M120 186h6v6H120V186M138 186h6v6H138V186M150 186h6v6H150V186M156 186h6v6H156V186M168 186h6v6H168V186M192 186h6v6H192V186M204 186h6v6H204V186M210 186h6v6H210V186M24 192h6v6H24V192M36 192h6v6H36V192M42 192h6v6H42V192M48 192h6v6H48V192M60 192h6v6H60V192M72 192h6v6H72V192M84 192h6v6H84V192M96 192h6v6H96V192M102 192h6v6H102V192M108 192h6v6H108V192M150 192h6v6H150V192M156 192h6v6H156V192M168 192h6v6H168V192M174 192h6v6H174V192M180 192h6v6H180V192M186 192h6v6H186V192M192 192h6v6H192V192M198 192h6v6H198V192M216 192h6v6H216V192M24 198h6v6H24V198M36 198h6v6H36V198M42 198h6v6H42V198M48 198h6v6H48V198M60 198h6v6H60V198M72 198h6v6H72V198M96 198h6v6H96V198M114 198h6v6H114V198M144 198h6v6H144V198M186 198h6v6H186V198M192 198h6v6H192V198M198 198h6v6H198V198M210 198h6v6H210V198M24 204h6v6H24V204M36 204h6v6H36V204M42 204h6v6H42V204M48 204h6v6H48V204M60 204h6v6H60V204M72 204h6v6H72V204M78 204h6v6H78V204M90 204h6v6H90V204M96 204h6v6H96V204M102 204h6v6H102V204M108 204h6v6H108V204M120 204h6v6H120V204M132 204h6v6H132V204M156 204h6v6H156V204M168 204h6v6H168V204M174 204h6v6H174V204M180 204h6v6H180V204M186 204h6v6H186V204M198 204h6v6H198V204M204 204h6v6H204V204M24 210h6v6H24V210M60 210h6v6H60V210M78 210h6v6H78V210M84 210h6v6H84V210M108 210h6v6H108V210M114 210h6v6H114V210M126 210h6v6H126V210M132 210h6v6H132V210M144 210h6v6H144V210M150 210h6v6H150V210M168 210h6v6H168V210M180 210h6v6H180V210M186 210h6v6H186V210M192 210h6v6H192V210M198 210h6v6H198V210M24 216h6v6H24V216M30 216h6v6H30V216M36 216h6v6H36V216M42 216h6v6H42V216M48 216h6v6H48V216M54 216h6v6H54V216M60 216h6v6H60V216M72 216h6v6H72V216M78 216h6v6H78V216M84 216h6v6H84V216M90 216h6v6H90V216M96 216h6v6H96V216M126 216h6v6H126V216M132 216h6v6H132V216M138 216h6v6H138V216M144 216h6v6H144V216M168 216h6v6H168V216M174 216h6v6H174V216M186 216h6v6H186V216M192 216h6v6H192V216M198 216h6v6H198V216M204 216h6v6H204V216M210 216h6v6H210V216"/></svg>';
var DEMO_RX = {
  url: "http://192.168.1.20:47830/r/3f9a1c7e5b2d4f60a8e19c3b7d5f2a41",
  otherUrls: [],
  qrSvg: DEMO_QR,
  label: "la carpeta compartida «Pareja»",
  expiresInSecs: 880,
  received: [{ name: "IMG_3301.HEIC", size: 2.4e6 }, { name: "Recibo luz octubre.pdf", size: 1.8e5 }],
};

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
      receive: new URLSearchParams(location.search).get("open") === "receive" ? DEMO_RX : null,
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
  if (cmd === "start_receive") return Promise.resolve(DEMO_RX);
  if (cmd === "get_logs") return Promise.resolve({ entries: [], debug: false, dir: null });
  if (cmd === "get_network")
    return Promise.resolve({
      listenVpn: false,
      addresses: [{ interface: "en0", ip: "192.168.1.20", tunnel: false }, { interface: "utun4", ip: "10.8.0.2", tunnel: true }],
      ports: [{ share: "Pareja", port: 47820 }, { share: "Papá e hijo", port: 47822 }],
    });
  if (cmd === "app_version") return Promise.resolve("0.3.0");
  return Promise.resolve(null);
}
