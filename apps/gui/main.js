// LitePack GUI page (E5-3, the archive browser): asks Rust for the strings and the archive given
// on the command line (or dropped on the window), opens it through its grant and shows it in the
// virtual list (list.js): flat or tree, sorted and filtered in Rust, with keyboard navigation,
// multi-selection and a status bar. No HTML sink anywhere (Trusted Types would refuse it): text
// goes in through textContent.
import { VirtualList } from "./list.js";

// What went wrong on the page; reported with the listing (CI's check of the built exe fails on
// any) and shown in the status bar.
const problems = [];
// The same events as flags on <html> for E5-11's WebDriver harness: a count per kind
// (data-lpk-error, data-lpk-rejection, data-lpk-csp, data-lpk-failure), "0" once the listeners
// are in, and the last message (data-lpk-last). The driver reads them as attributes, so no
// script is injected and the CSP stays as it is.
const flags = document.documentElement.dataset;
for (const kind of ["lpkError", "lpkRejection", "lpkCsp", "lpkFailure"]) {
  flags[kind] = "0";
}
function problem(kind, text) {
  problems.push(text);
  flags[kind] = String(Number(flags[kind]) + 1);
  flags.lpkLast = text;
}
window.addEventListener("error", (e) => problem("lpkError", `error: ${e.message}`));
window.addEventListener("unhandledrejection", (e) => problem("lpkRejection", `rejection: ${describe(e.reason)}`));
document.addEventListener("securitypolicyviolation", (e) => problem("lpkCsp", `csp: ${e.violatedDirective}`));

const core = window.__TAURI__.core;
const $ = (id) => document.getElementById(id);
let strings = {};
let nextOp = 1;
// The open archive's session, closed before another is opened (a drop).
let session = null;
let reported = false;
// What the view shows; Rust builds it (`view`).
const state = { mode: "flat", dir: null, key: "path", descending: false, filter: "", glob: false };
let info = null;

function t(key, vars = {}) {
  const text = strings[key] ?? key;
  return text.replace(/\{(\w+)\}/g, (_, k) => String(vars[k] ?? ""));
}

function describe(e) {
  if (e && typeof e === "object") {
    return e.detail ? `${e.message} ${e.detail}` : (e.message ?? JSON.stringify(e));
  }
  return String(e);
}

function applyStrings() {
  document.title = t("app.title");
  for (const el of document.querySelectorAll("[data-string]")) {
    el.textContent = t(el.dataset.string);
  }
  for (const el of document.querySelectorAll("[data-label]")) {
    el.setAttribute("aria-label", t(el.dataset.label));
  }
  $("filter").placeholder = t("filter.placeholder");
}

function setStatus(text) {
  $("status").textContent = text;
}

const bytes = (n) => Number(n).toLocaleString();

function showSelection(s) {
  const parts = [t("status.view", { entries: bytes(s.entries), files: bytes(s.files), folders: bytes(s.folders), bytes: bytes(s.bytes) })];
  if (s.picked > 0) {
    parts.push(t("status.picked", { count: bytes(s.picked), bytes: bytes(s.pickedBytes) }));
  }
  $("selection").textContent = parts.join(" · ");
}

function cells(r) {
  const when = r.mtimeNs === -(2n ** 63n) ? "" : new Date(Number(r.mtimeNs / 1000000n)).toLocaleString();
  const size = r.kind === 1 ? "" : r.size.toLocaleString();
  return [r.name, size, when];
}

const list = new VirtualList({
  grid: $("list"),
  viewport: $("viewport"),
  spacer: $("spacer"),
  layer: $("rows"),
  cells,
  fetch: (first, count) => core.invoke("rows", { session, first, count }),
  activate: (index, row, generation, folderOnly = false) => activate(index, row, generation, folderOnly).catch(showError),
  // The generation of the batch the row index came from: Rust refuses another.
  select: async (action, row, generation) => {
    const s = await core.invoke("select", { session, action, row, generation });
    showSelection(s);
    await list.refresh();
  },
  onError: (e) => showError(e),
});
list.parent = (e) => {
  if (state.mode === "tree" && info && info.crumbs.length > 1) {
    e.preventDefault();
    const up = info.crumbs[info.crumbs.length - 2];
    navigate(up.id).catch(showError);
  }
};

function renderCrumbs() {
  const nav = $("crumbs");
  nav.replaceChildren();
  if (!info || state.mode !== "tree") {
    return;
  }
  info.crumbs.forEach((c, i) => {
    const b = document.createElement("button");
    b.type = "button";
    b.textContent = c.id === null ? t("crumbs.root") : c.name;
    if (i === info.crumbs.length - 1) {
      b.setAttribute("aria-current", "location");
    }
    b.addEventListener("click", () => navigate(c.id).catch(showError));
    nav.append(b);
  });
}

function renderHeader() {
  for (const h of document.querySelectorAll("[role=columnheader]")) {
    const key = h.dataset.key;
    const active = key === state.key;
    h.setAttribute("aria-sort", active ? (state.descending ? "descending" : "ascending") : "none");
  }
  $("col-name").textContent = state.mode === "flat" ? t("list.path") : t("list.name");
  for (const m of ["flat", "tree"]) {
    $(`mode-${m}`).setAttribute("aria-checked", String(state.mode === m));
  }
}

async function applyView(keep = false) {
  const request = { ...state };
  info = await core.invoke("view", { session, request, op: nextOp++ });
  renderCrumbs();
  renderHeader();
  showSelection(info.status);
  await list.reset(info, keep);
}

async function navigate(dir) {
  state.mode = "tree";
  state.dir = dir;
  await applyView();
}

async function activate(index, row, generation, folderOnly) {
  if (row.kind === 1) {
    if (state.mode === "tree") {
      await navigate(row.id);
    }
    return;
  }
  if (folderOnly) {
    return;
  }
  const r = await core.invoke("open_entry", { session, row: index, generation, op: nextOp++ });
  if (r.outcome === "shell") {
    setStatus(t("open.done", { name: r.name }));
  } else {
    setStatus(t("open.refused", { name: r.name, extension: r.extension }));
  }
}

// A native picker in Rust: the page names what to pick and receives grants (never a path).
function pick(purpose) {
  return core.invoke("pick", { purpose });
}

async function openPicked() {
  const [archive] = await pick("archive");
  if (archive) {
    await openArchive(archive);
  }
}

// A new archive: what goes in (files, or a folder when no file is chosen), then where it goes;
// named after the first input, written by Rust through a `.partial` that never replaces a file.
async function createPicked() {
  let inputs = await pick("files");
  if (inputs.length === 0) {
    inputs = await pick("folder");
  }
  if (inputs.length === 0) {
    return;
  }
  const [folder] = await pick("destination");
  if (!folder) {
    return;
  }
  const name = inputs[0].name.replace(/\.[^.]*$/, "") || "archive";
  setStatus(t("create.running", { name: `${name}.lpk` }));
  const progress = new core.Channel();
  progress.onmessage = () => {};
  await core.invoke("create_archive", {
    inputs: inputs.map((i) => i.grant),
    folder: folder.grant,
    name,
    tier: "balanced",
    op: nextOp++,
    progress,
  });
  setStatus(t("create.done", { name: `${name}.lpk` }));
}

async function report(total, first) {
  if (reported) {
    return;
  }
  reported = true;
  const rendered = document.querySelectorAll("#rows [role=row]").length;
  await core.invoke("listing_shown", { session, rendered, total, first, problems });
}

async function openArchive(granted) {
  setStatus(t("status.loading"));
  if (session !== null) {
    const previous = session;
    session = null;
    await core.invoke("close_archive", { session: previous });
  }
  const opened = await core.invoke("open_archive", { grant: granted.grant, op: nextOp++ });
  session = opened.session;
  Object.assign(state, { mode: "flat", dir: null, key: "path", descending: false });
  info = opened.view;
  $("hint").hidden = true;
  $("bar").hidden = false;
  $("list").hidden = false;
  renderCrumbs();
  renderHeader();
  showSelection(info.status);
  await list.reset(info);
  setStatus(t("status.entries", { name: opened.name, count: bytes(opened.total) }));
  $("list").focus();
  await report(opened.total, list.rowAt(0)?.name ?? "");
}

// A row request made against a view Rust has already replaced is refused ("stale view",
// crates/lpk-gui/src/browse.rs STALE_VIEW); the next view re-syncs the page, so it is ignored.
function stale(e) {
  return Boolean(e && typeof e === "object" && e.class === "Usage" && String(e.detail).startsWith("stale view"));
}

function showError(e) {
  if (stale(e)) {
    return;
  }
  problem("lpkFailure", `failure: ${describe(e)}`);
  setStatus(`${t("error.title")}: ${describe(e)}`);
  if (!reported) {
    reported = true;
    core.invoke("listing_shown", { session: null, rendered: 0, total: 0, first: "", problems }).catch(() => {});
  }
}

function wireControls() {
  $("action-open").addEventListener("click", () => openPicked().catch(showError));
  $("action-create").addEventListener("click", () => createPicked().catch(showError));
  $("mode-flat").addEventListener("click", () => {
    Object.assign(state, { mode: "flat", dir: null });
    applyView().catch(showError);
  });
  $("mode-tree").addEventListener("click", () => {
    Object.assign(state, { mode: "tree", dir: null, key: state.key === "path" ? "kind" : state.key });
    applyView().catch(showError);
  });
  // Each header holds a button: focusable, and Enter or Space click it (the click bubbles here).
  for (const h of document.querySelectorAll("[role=columnheader]")) {
    h.addEventListener("click", () => {
      const key = h.dataset.key === "name" && state.mode === "flat" ? "path" : h.dataset.key;
      state.descending = state.key === key ? !state.descending : false;
      state.key = key;
      applyView(true).catch(showError);
    });
  }
  let timer = null;
  const refilter = () => {
    clearTimeout(timer);
    timer = setTimeout(() => {
      state.filter = $("filter").value;
      state.glob = $("glob").checked;
      if (session !== null) {
        applyView().catch(showError);
      }
    }, 200);
  };
  $("filter").addEventListener("input", refilter);
  $("glob").addEventListener("change", refilter);
}

async function main() {
  const startup = await core.invoke("startup");
  strings = startup.strings;
  applyStrings();
  wireControls();
  if (startup.recovered > 0) {
    setStatus(t("status.recovered", { count: startup.recovered }));
  }
  // Interrupted writes the recovery gave up on after its tries: said once, not retried.
  for (const g of startup.gave_up) {
    setStatus(t("notice.gaveup", { name: g.name, tries: g.tries }));
  }
  if (startup.error) {
    showError({ message: startup.error });
    return;
  }
  const channel = new core.Channel();
  channel.onmessage = (notice) => {
    if (notice.type === "dropped") {
      const archive = notice.items.find((item) => item.kind === "archive");
      if (archive) {
        openArchive(archive).catch(showError);
      }
    }
  };
  await core.invoke("subscribe", { channel });
  if (startup.archive) {
    await openArchive(startup.archive);
  }
}

main().catch(showError);
