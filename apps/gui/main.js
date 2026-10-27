// LitePack GUI page (E5-2 skeleton): asks Rust for the strings and the archive given on the
// command line, opens it through its grant, and shows the first rows of its listing. No HTML
// sink anywhere (Trusted Types would refuse it): text goes in through textContent. E5-3
// replaces the first-page list with the virtual list.
import { decodeBatch } from "./batch.js";

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
const FIRST_PAGE_ROWS = 200;
let strings = {};
let nextOp = 1;
// The open archive's session, closed before another is opened (a drop).
let currentSession = null;

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
}

function setStatus(text) {
  document.getElementById("status").textContent = text;
}

function cell(text, cls) {
  const span = document.createElement("span");
  span.setAttribute("role", "gridcell");
  if (cls) {
    span.className = cls;
  }
  span.textContent = text;
  return span;
}

function render(batch) {
  const list = document.getElementById("list");
  const rows = document.getElementById("rows");
  rows.replaceChildren();
  list.setAttribute("aria-rowcount", String(batch.total + 1));
  batch.rows.forEach((r, i) => {
    const row = document.createElement("div");
    row.className = "row";
    row.setAttribute("role", "row");
    row.setAttribute("aria-rowindex", String(batch.first + i + 2));
    row.tabIndex = i === 0 ? 0 : -1;
    const when = new Date(Number(r.mtimeNs / 1000000n));
    row.append(cell(r.name), cell(r.size.toLocaleString(), "num"), cell(when.toLocaleString()));
    rows.append(row);
  });
  document.getElementById("hint").hidden = true;
  list.hidden = false;
}

async function report(session, total, first) {
  const rendered = document.querySelectorAll("#rows [role=row]").length;
  await core.invoke("listing_shown", { session, rendered, total, first, problems });
}

async function openArchive(granted) {
  setStatus(t("status.loading"));
  if (currentSession !== null) {
    const previous = currentSession;
    currentSession = null;
    await core.invoke("close_archive", { session: previous });
  }
  const opened = await core.invoke("open_archive", { grant: granted.grant, op: nextOp++ });
  currentSession = opened.session;
  const buffer = await core.invoke("rows", { session: opened.session, first: 0, count: FIRST_PAGE_ROWS });
  const batch = decodeBatch(buffer);
  render(batch);
  setStatus(t("status.entries", { count: opened.total.toLocaleString() }));
  await report(opened.session, opened.total, batch.rows[0]?.name ?? "");
}

function showError(e) {
  problem("lpkFailure", `failure: ${describe(e)}`);
  setStatus(`${t("error.title")}: ${describe(e)}`);
  report(null, 0, "").catch(() => {});
}

async function main() {
  const info = await core.invoke("startup");
  strings = info.strings;
  applyStrings();
  if (info.recovered > 0) {
    setStatus(t("status.recovered", { count: info.recovered }));
  }
  if (info.error) {
    showError({ message: info.error });
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
  if (info.archive) {
    await openArchive(info.archive);
  }
}

main().catch(showError);
