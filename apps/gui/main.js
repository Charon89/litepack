// LitePack GUI page (E5-3, the archive browser): asks Rust for the strings and the archive given
// on the command line (or dropped on the window), opens it through its grant and shows it in the
// virtual list (list.js): flat or tree, sorted and filtered in Rust, with keyboard navigation,
// multi-selection and a status bar. No HTML sink anywhere (Trusted Types would refuse it): text
// goes in through textContent.
import { VirtualList } from "./list.js";
import { createExtractor } from "./extract.js";
import { createCreator } from "./create.js";
import { createPreview } from "./preview.js";
import { createMaintainer } from "./maintain.js";
import { keyLabel, keyLetter, syncRadios, trapTab, wirePanes, wireRadios } from "./a11y.js";

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
let sessionName = "";
// What the open session is: the maintenance buttons' and dialogs' facts (E5-8).
let current = { name: "", format: "", generation: 0, latest: 0, encrypted: false };
// Selected rows of the current view (the status bar's count: what an extraction takes).
let pickedCount = 0;
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

// A button's words: its icon carries none, so its label is the text of its own `data-string` span.
function labelOf(button) {
  return (button.querySelector("[data-string]") ?? button).textContent.trim();
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
  // The gear has no words of its own: its name is its tooltip too.
  $("settings-open").title = t("settings.open");
  // The key of each command in its tooltip (and in aria-keyshortcuts, in the markup).
  for (const el of document.querySelectorAll("button[aria-keyshortcuts]")) {
    el.title = t("shortcut.tooltip", { label: labelOf(el), keys: keyLabel(el.getAttribute("aria-keyshortcuts")) });
  }
}

function setStatus(text) {
  $("status").textContent = text;
}

const bytes = (n) => Number(n).toLocaleString();

function showSelection(s) {
  pickedCount = s.picked;
  const parts = [t("status.view", { entries: bytes(s.entries), files: bytes(s.files), folders: bytes(s.folders), bytes: bytes(s.bytes) })];
  if (s.picked > 0) {
    parts.push(t("status.picked", { count: bytes(s.picked), bytes: bytes(s.pickedBytes) }));
  }
  // Separated by spacing (an em space), not by a mark.
  $("selection").textContent = parts.join(" ");
}

// A size for people: B, KB, MB or GB, one decimal (units from the table); `n` may be a BigInt.
function humanSize(n) {
  const v = Number(n);
  if (v < 1024) {
    return t("size.b", { n: v.toLocaleString() });
  }
  const one = (x) => ({ n: x.toLocaleString(undefined, { minimumFractionDigits: 1, maximumFractionDigits: 1 }) });
  if (v < 1024 ** 2) {
    return t("size.kb", one(v / 1024));
  }
  if (v < 1024 ** 3) {
    return t("size.mb", one(v / 1024 ** 2));
  }
  return t("size.gb", one(v / 1024 ** 3));
}

// A short date (the day), with the time only when it is today.
const dayFormat = new Intl.DateTimeFormat(undefined, { dateStyle: "medium" });
const todayFormat = new Intl.DateTimeFormat(undefined, { dateStyle: "medium", timeStyle: "short" });
function shortDate(ms) {
  const d = new Date(ms);
  return (d.toDateString() === new Date().toDateString() ? todayFormat : dayFormat).format(d);
}

function cells(r) {
  const when = r.mtimeNs === -(2n ** 63n) ? "" : shortDate(Number(r.mtimeNs / 1000000n));
  const size = r.kind === 1 ? "" : humanSize(r.size);
  // The exact byte count in the size cell's tooltip.
  const exact = r.kind === 1 ? "" : t("preview.size", { bytes: Number(r.size).toLocaleString() });
  return [r.name, size, when, exact];
}

const preview = createPreview({
  core,
  t,
  nextOp: () => nextOp++,
  session: () => session,
  showError: (e) => showError(e),
});

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
  // The focused row rests for a moment, then the pane reads it (preview.js).
  onFocus: (index, row, generation) => preview.focused(index, row, generation),
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
  syncRadios($("mode-tree").parentElement);
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
    // Not opened directly: the pane shows that entry now.
    preview.show(index, row, generation);
  }
}

// A native picker in Rust: the page names what to pick and receives grants (never a path).
// One picker at a time (Rust refuses a second one too): the buttons are disabled meanwhile.
async function pick(purpose) {
  const buttons = document.querySelectorAll(".actions button, .dropzone button");
  for (const b of buttons) {
    b.disabled = true;
  }
  try {
    return await core.invoke("pick", { purpose });
  } finally {
    for (const b of buttons) {
      b.disabled = false;
    }
    // The buttons that need an open archive go back to what the session allows.
    $("action-extract").disabled = session === null;
    updateMaintenance();
  }
}

async function openPicked() {
  const [archive] = await pick("archive");
  if (archive) {
    await openArchive(archive);
  }
}

// The create wizard (create.js): from the bar's button and Ctrl+N, from `--create <paths>`, and
// `Add to "<name>.lpk"` (`--create ... --name`), which creates at once through the wizard's
// progress dialog and summary.
const creator = createCreator({
  core,
  t,
  nextOp: () => nextOp++,
  showError: (e) => showError(e),
  openArchive: (g) => openArchive(g),
});

// One chain for every verb's dialog (extract.js, maintain.js): two verbs arriving together
// show their dialogs one after the other, never stacked.
const verbs = { chain: Promise.resolve() };

// The extract wizard (extract.js) and the verbs' queue.
const extractor = createExtractor({
  verbs,
  core,
  t,
  nextOp: () => nextOp++,
  session: () => session,
  picked: () => pickedCount,
  archiveName: () => sessionName,
  showError: (e) => showError(e),
});

// The maintenance dialogs (maintain.js) and the verb --test's queue. `adopt` shows what they
// opened: a granted archive (a repaired copy), or a session Rust already opened (a browsed
// generation, the archive after a rollback or an update; `previous` is the session it replaces,
// which Rust may have closed already).
const maintainer = createMaintainer({
  verbs,
  core,
  t,
  nextOp: () => nextOp++,
  session: () => session,
  archive: () => current,
  showError: (e) => showError(e),
  adopt: async (granted, opened, previous) => {
    if (granted) {
      await openArchive(granted);
      return;
    }
    if (previous !== undefined && previous !== null && previous !== opened?.session) {
      await core.invoke("close_archive", { session: previous }).catch(() => {});
    }
    if (opened) {
      await showOpened(opened);
    } else {
      clearSession();
    }
  },
});

// Which maintenance actions the open archive offers, and why not: Test works on whatever
// Session::test supports (a foreign archive too); the others are .lpk only; the ones that write
// need the latest generation (an earlier one is a read-only view).
function updateMaintenance() {
  const none = session === null;
  const foreign = !none && current.format !== "lpk";
  const view = !none && current.generation !== current.latest;
  const set = (id, off, why) => {
    const b = $(id);
    b.disabled = off;
    if (off) {
      b.setAttribute("title", why);
    } else {
      b.title = t("shortcut.tooltip", { label: labelOf(b), keys: keyLabel(b.getAttribute("aria-keyshortcuts")) });
    }
  };
  set("action-test", none, t("maintain.why.none"));
  set("action-check", none || foreign, none ? t("maintain.why.none") : t("maintain.why.foreign"));
  set("action-history", none || foreign, none ? t("maintain.why.none") : t("maintain.why.foreign"));
  set("action-update", none || foreign || view, none ? t("maintain.why.none") : foreign ? t("maintain.why.foreign") : t("maintain.why.view"));
  const banner = $("banner");
  banner.hidden = none || !view;
  if (!banner.hidden) {
    $("banner-text").textContent = t("banner.generation", { generation: current.generation, latest: current.latest });
  }
}

// The session is gone (a failed reopen): back to the empty window.
function clearSession() {
  session = null;
  sessionName = "";
  current = { name: "", format: "", generation: 0, latest: 0, encrypted: false };
  $("action-extract").disabled = true;
  $("bar").hidden = true;
  $("split").hidden = true;
  $("list").hidden = true;
  $("hint").hidden = false;
  $("label").hidden = true;
  preview.clear();
  updateMaintenance();
  renderRecents().catch(showError);
}

// `--extract-here` (Smart) and `--extract-to` (a new folder) beside each archive, no question.
function extractHere(a) {
  return extractor.runVerb("here", a.items);
}

function extractTo(a) {
  return extractor.runVerb("to", a.items);
}

// The handlers of the verbs whose action a later task brings; each says so in a notice until then.

// `--create <paths>`: the create wizard with these inputs.
function createWizard(a) {
  return creator.open(a.items);
}

// `--test <archives>`: each archive tested through the queue, one result each.
function testArchives(a) {
  return maintainer.runVerb(a.items);
}

// E4's broker path into create completes convertArchive (a foreign archive re-created as .lpk).
function convertArchive() {
  showNotices([{ kind: "text", text: t("verb.convert") }]);
}

// An activation verb (E5-7), from this instance's command line or forwarded by a second one: its
// paths arrive as grants (src/activation.rs), the handler is Rust's choice.
async function runActivation(a) {
  switch (a.handler) {
    case "open":
      return openArchive(a.items[0]);
    case "create-named":
      return creator.runNamed(a);
    case "extract-here":
      return extractHere(a);
    case "extract-to":
      return extractTo(a);
    case "create-wizard":
      return createWizard(a);
    case "test":
      return testArchives(a);
    default:
      return convertArchive(a);
  }
}

// The update check's switch (E5-7): Rust saves it; off means no request at all.
async function wireSettings() {
  const box = $("update-check");
  const settings = await core.invoke("settings");
  box.checked = settings.update_check;
  box.addEventListener("change", () => {
    core.invoke("set_settings", { settings: { update_check: box.checked } }).catch((e) => {
      box.checked = !box.checked;
      showError(e);
    });
  });
}

// E6-4: the "Open with" switch and the "Default apps…" button. Rust says whether this copy is the
// registered one (the installer's, named by the registration record); if not, they stay hidden.
// The switch re-registers in process; a failure shows in this area's own alert.
// The alert lies outside the group: a failed re-registration can leave this copy unregistered,
// which hides the group, and the error must stay.
function shellAlert(e) {
  const alert = $("shell-alert");
  if (!e) {
    alert.textContent = "";
  } else if (e.kind === "RegisterFailed") {
    alert.textContent = t("settings.shell.failed", { cause: e.detail });
  } else {
    alert.textContent = `${t("error.title")}: ${describe(e)}`;
  }
  alert.hidden = !e;
  // Behind the open settings dialog the page is inert: the dialog says it too.
  const inDialog = $("settings-alert");
  inDialog.textContent = $("settings").open ? alert.textContent : "";
  inDialog.hidden = !$("settings").open || !e;
}

function showShell(status) {
  const group = $("shell-setting");
  group.hidden = !status.registered;
  if (!status.registered) {
    return;
  }
  $("open-with").checked = status.open_with;
  $("open-with-text").textContent = t("settings.openwith", {
    formats: status.formats.map((f) => `.${f}`).join(", "),
  });
}

async function wireShell() {
  const box = $("open-with");
  const status = await core.invoke("shell_status").catch(() => ({ registered: false }));
  showShell(status);
  let working = false;
  box.addEventListener("change", async () => {
    // Not `disabled` (that may drop the keyboard focus to the body): the focus stays, and a
    // second change during the call is undone and ignored.
    if (working) {
      box.checked = !box.checked;
      return;
    }
    working = true;
    box.setAttribute("aria-disabled", "true");
    shellAlert(null);
    try {
      showShell(await core.invoke("set_open_with", { on: box.checked }));
    } catch (e) {
      shellAlert(e);
      // The record is consistent whatever happened: show what it says now.
      showShell(await core.invoke("shell_status").catch(() => ({ registered: false })));
    } finally {
      working = false;
      box.removeAttribute("aria-disabled");
    }
  });
  $("default-apps").addEventListener("click", () => {
    shellAlert(null);
    core.invoke("open_default_apps").catch(shellAlert);
  });
}

// The recovery's notices (D-117) and the others (a second instance's refused command line, the
// settings file's warning, an update, a verb not in this version): each stays in its own list
// item, which the status line never replaces, until the user dismisses it.
function noticeText(n) {
  switch (n.kind) {
    case "text":
      return n.text;
    case "settings":
      return t("notice.settings", { error: n.error });
    case "refused":
      return t("notice.refused", { error: n.error });
    case "update":
      return t("notice.update", { version: n.version });
    case "gaveup":
      return t("notice.gaveup", { name: n.name, tries: n.tries });
    case "held":
      return t("notice.held", { name: n.name, tries: n.tries });
    default:
      return t("notice.failed", { name: n.name, error: n.error });
  }
}

function showNotices(notices) {
  const list = $("notices");
  for (const n of notices) {
    const item = document.createElement("li");
    item.className = "notice";
    const text = document.createElement("span");
    text.textContent = noticeText(n);
    const dismiss = document.createElement("button");
    dismiss.type = "button";
    dismiss.textContent = t("notice.dismiss");
    dismiss.addEventListener("click", () => {
      item.remove();
    });
    item.append(text, dismiss);
    list.append(item);
  }
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
  await showOpened(opened);
}

// The label band (E5-12): the name, the gauge (the file against its contents; hidden when either
// is unknown) and the facts.
function renderLabel(o) {
  $("label-name").textContent = o.name;
  const known = o.archive_bytes !== null && o.content_bytes !== null;
  $("label-gauge").hidden = !known;
  if (known) {
    const share = Math.min(1, o.archive_bytes / o.content_bytes);
    $("label-fill").style.width = `${(share * 100).toFixed(1)}%`;
    $("label-gauge-text").textContent = t("band.gauge", { content: humanSize(o.content_bytes), archive: humanSize(o.archive_bytes) });
  }
  $("label-entries").textContent = t("band.entries", { count: bytes(o.total) });
  $("label-format").textContent = o.format === "lpk" ? t("band.format.lpk") : t("band.format.other", { format: o.format });
  $("label-generation").textContent = t("band.generation", { generation: o.generation });
  $("label-generation").hidden = o.format !== "lpk";
  $("label-encrypted").textContent = t("band.encrypted");
  $("label-encrypted").hidden = !o.encrypted;
  $("label-readonly").textContent = t("band.readonly");
  $("label-readonly").hidden = o.generation === o.latest_generation;
}

// Show a session Rust opened (a new archive, a generation's view, the archive after a rollback).
async function showOpened(opened) {
  session = opened.session;
  sessionName = opened.name;
  current = { name: opened.name, format: opened.format, generation: opened.generation, latest: opened.latest_generation, encrypted: opened.encrypted };
  $("action-extract").disabled = false;
  updateMaintenance();
  Object.assign(state, { mode: "flat", dir: null, key: "path", descending: false });
  info = opened.view;
  $("hint").hidden = true;
  renderLabel(opened);
  $("label").hidden = false;
  $("recents").hidden = true;
  $("bar").hidden = false;
  $("list").hidden = false;
  $("split").hidden = false;
  preview.clear();
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
  // Behind an open modal dialog the status line is inert: the error shows in the dialog's own alert.
  const open = document.querySelector("dialog[open]");
  if (open) {
    let alert = open.querySelector(".dialog-error");
    if (!alert) {
      alert = document.createElement("p");
      alert.className = "dialog-error";
      alert.setAttribute("role", "alert");
      open.querySelector(".wizard-body")?.append(alert);
    }
    alert.textContent = `${t("error.title")}: ${describe(e)}`;
  } else {
    setStatus(`${t("error.title")}: ${describe(e)}`);
  }
  if (!reported) {
    reported = true;
    core.invoke("listing_shown", { session: null, rendered: 0, total: 0, first: "", problems }).catch(() => {});
  }
}

// The recent archives on the empty window (Rust lists the ones that still exist, as fresh
// grants); one button clears the recent archives and destinations.
async function renderRecents() {
  const box = $("recents");
  if (session !== null) {
    box.hidden = true;
    return;
  }
  const lists = await core.invoke("recents");
  const ul = $("recent-list");
  ul.replaceChildren();
  for (const g of lists.archives) {
    const li = document.createElement("li");
    const b = document.createElement("button");
    b.type = "button";
    b.textContent = g.name;
    b.addEventListener("click", () => openArchive(g).catch(showError));
    li.append(b);
    ul.append(li);
  }
  box.hidden = lists.archives.length === 0;
}

// The settings dialog (E5-12): opened by the gear; Tab stays inside, Esc or Close shuts it, and
// the focus goes back to the gear.
function wireSettingsDialog() {
  const dialog = $("settings");
  const gear = $("settings-open");
  trapTab(dialog);
  gear.addEventListener("click", () => {
    $("settings-alert").hidden = true;
    dialog.showModal();
  });
  $("settings-close").addEventListener("click", () => dialog.close());
  dialog.addEventListener("close", () => gear.focus());
}

function wireControls() {
  $("action-extract").disabled = true;
  $("action-extract").addEventListener("click", () => extractor.openDialog().catch(showError));
  updateMaintenance();
  $("action-test").addEventListener("click", () => maintainer.testDialog().catch(showError));
  $("action-check").addEventListener("click", () => maintainer.checkDialog().catch(showError));
  $("action-history").addEventListener("click", () => maintainer.historyDialog().catch(showError));
  $("action-update").addEventListener("click", () => maintainer.updateDialog().catch(showError));
  // Back from a generation's view to the latest one.
  $("banner-latest").addEventListener("click", () => {
    const previous = session;
    core
      .invoke("history_browse", { session: previous, generation: current.latest, op: nextOp++ })
      .then(async (opened) => {
        await core.invoke("close_archive", { session: previous }).catch(() => {});
        await showOpened(opened);
      })
      .catch((e) => {
        // A cancelled password prompt is the user's answer, not a failure.
        if (!(e && e.class === "Cancelled")) {
          showError(e);
        }
      });
  });
  // The command keys: each button with aria-keyshortcuts (Alt+P belongs to the preview pane);
  // a disabled command, an open dialog or a running picker takes none.
  document.addEventListener("keydown", (e) => {
    if (e.key === "Control" || e.key === "Shift" || e.key === "Alt") {
      return;
    }
    const mods = (e.ctrlKey || e.metaKey ? "Control+" : "") + (e.altKey ? "Alt+" : "") + (e.shiftKey ? "Shift+" : "");
    // The Latin letter typed, else the physical key's: Dvorak and Cyrillic layouts both keep the keys.
    const letter = keyLetter(e);
    const chord = `${mods}${letter}`;
    // Ctrl+F is the app's always (never the webview's find bar): the filter, or nothing.
    if (chord === "Control+F") {
      e.preventDefault();
      if (!$("bar").hidden && !document.querySelector("dialog[open]")) {
        $("filter").focus();
        $("filter").select();
      }
      return;
    }
    if (document.querySelector("dialog[open]")) {
      return;
    }
    for (const b of document.querySelectorAll("button[aria-keyshortcuts]")) {
      const keys = b.getAttribute("aria-keyshortcuts");
      if (b.id !== "preview-toggle" && keys.toUpperCase() === chord.toUpperCase()) {
        e.preventDefault();
        if (!b.disabled) {
          b.click();
        }
        return;
      }
    }
  });
  wireSettingsDialog();
  wirePanes();
  wireRadios($("mode-tree").parentElement);
  syncRadios($("mode-tree").parentElement);
  $("recents-clear").addEventListener("click", () => {
    core
      .invoke("clear_recents")
      .then(() => renderRecents())
      .catch(showError);
  });
  $("action-open").addEventListener("click", () => openPicked().catch(showError));
  $("action-new").addEventListener("click", () => creator.open([]).catch(showError));
  $("hint-open").addEventListener("click", () => openPicked().catch(showError));
  $("hint-new").addEventListener("click", () => creator.open([]).catch(showError));
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
  showNotices(startup.notices);
  if (startup.settings_warning) {
    showNotices([{ kind: "settings", error: startup.settings_warning }]);
  }
  await wireSettings();
  await wireShell();
  // A command line that did not parse is shown, and the window still takes later verbs.
  if (startup.error) {
    showError({ message: startup.error });
  }
  // This instance's own verb first; a second instance's verbs, held by Rust until the page
  // subscribes, run after it.
  if (startup.activation) {
    await runActivation(startup.activation).catch(showError);
  }
  await renderRecents().catch(showError);
  const channel = new core.Channel();
  channel.onmessage = (notice) => {
    if (notice.type === "dropped") {
      const archive = notice.items.find((item) => item.kind === "archive");
      if (archive) {
        openArchive(archive).catch(showError);
      }
    } else if (notice.type === "activated") {
      runActivation(notice.activation).catch(showError);
    } else if (notice.type === "refused") {
      showNotices([{ kind: "refused", error: notice.error }]);
    } else if (notice.type === "update") {
      showNotices([{ kind: "update", version: notice.version }]);
    }
  };
  await core.invoke("subscribe", { channel });
}

main().catch(showError);
