// The preview pane (E5-6; docs/notes/e5-design.md section 4, "Preview"). One capped read in Rust
// (`preview`): Rust classifies the bytes by content and sends one binary message (magic LPKV,
// kind u8, MIME length u8, size u64, MIME, body; crates/lpk-gui/src/preview.rs). The page shows
// text with textContent in a <pre> (hidden and bidirectional characters were escaped in Rust),
// an image through a Blob of Rust's MIME type, and everything else as a virtualized hex view.
// Archive content is never rendered as markup: there is no HTML sink on this page.

const MAGIC = 0x5650504c; // "LPKV" read as a little-endian u32
const TEXT = 0;
const IMAGE = 1;
const HEX = 2;
const FOLDER = 3;

// A hex line: 16 bytes. Its height in CSS pixels (styles.css: .hex-line height).
const PER_LINE = 16;
const LINE_PX = 18;
// The most pixels the hex spacer takes (as the list's); beyond it scrolling is scaled.
const MAX_SCROLL_PX = 8000000;
const HEX_MARGIN = 16;
// How long focus rests on a row before it is read.
export const REST_MS = 150;

// The characters Rust escapes in text, escaped here too where the page shows an entry's name.
const HIDDEN = /[\u0000-\u0008\u000b\u000c\u000e-\u001f\u007f-\u009f­؜​-‏‪-‮⁠-⁤⁦-⁩﻿]/g;

/** A name with hidden and bidirectional characters written out. */
export function visibleName(name) {
  return name.replace(HIDDEN, (c) => `\\u{${c.codePointAt(0).toString(16).toUpperCase().padStart(4, "0")}}`);
}

/** Decode a preview message. */
export function decodePreview(buffer) {
  const view = new DataView(buffer);
  if (view.getUint32(0, true) !== MAGIC) {
    throw new Error("not a preview message");
  }
  const kind = view.getUint8(4);
  const mimeLen = view.getUint8(5);
  const size = view.getBigUint64(6, true);
  const mime = new TextDecoder().decode(new Uint8Array(buffer, 14, mimeLen));
  return { kind, mime, size, body: new Uint8Array(buffer, 14 + mimeLen) };
}

const DIGITS = "0123456789abcdef";

/** One hex line: the offset, 16 bytes in hex and their printable ASCII. */
export function hexLine(bytes, line) {
  const from = line * PER_LINE;
  const to = Math.min(bytes.length, from + PER_LINE);
  let hex = "";
  let ascii = "";
  for (let i = from; i < from + PER_LINE; i++) {
    if (i < to) {
      const b = bytes[i];
      hex += `${DIGITS[b >> 4]}${DIGITS[b & 15]} `;
      ascii += b >= 0x20 && b < 0x7f ? String.fromCharCode(b) : ".";
    } else {
      hex += "   ";
    }
  }
  return `${from.toString(16).padStart(8, "0")}  ${hex} ${ascii}`;
}

export function createPreview({ core, t, nextOp, session, showError }) {
  const pane = document.getElementById("preview");
  const title = document.getElementById("preview-title");
  const meta = document.getElementById("preview-meta");
  const body = document.getElementById("preview-body");
  const toggle = document.getElementById("preview-toggle");
  const count = (n) => Number(n).toLocaleString();

  let shown = true;
  let seq = 0; // the newest request
  let op = 0; // its operation (cancelled when another comes)
  let timer = null;
  let current = null; // the row last asked for: {index, row, generation}
  let url = null; // the image's object URL
  let hex = null; // the hex view held: {bytes, viewport, spacer, layer, lines}

  function release() {
    if (url !== null) {
      URL.revokeObjectURL(url);
      url = null;
    }
    hex = null;
    body.replaceChildren();
  }

  function head(name, text) {
    title.textContent = visibleName(name);
    meta.textContent = text;
  }

  function note(text) {
    release();
    const p = document.createElement("p");
    p.className = "preview-note";
    p.textContent = text;
    body.append(p);
  }

  function empty() {
    head("", "");
    note(t("preview.empty"));
  }

  function showText(text) {
    release();
    const pre = document.createElement("pre");
    pre.className = "preview-text";
    pre.tabIndex = 0;
    pre.setAttribute("aria-label", t("preview.text.label"));
    pre.textContent = text;
    body.append(pre);
  }

  function showImage(bytes, mime, name) {
    release();
    const img = document.createElement("img");
    img.className = "preview-image";
    img.alt = visibleName(name);
    url = URL.createObjectURL(new Blob([bytes], { type: mime }));
    // The webview could not decode it (a truncated or lying file): the bytes as hex instead.
    img.addEventListener("error", () => showHex(bytes, name));
    img.src = url;
    body.append(img);
  }

  // The hex view: only the visible lines are rendered, from the bytes held.
  function showHex(bytes, name) {
    release();
    const viewport = document.createElement("div");
    viewport.className = "hex";
    viewport.tabIndex = 0;
    viewport.setAttribute("role", "region");
    viewport.setAttribute("aria-label", t("preview.hex.label", { name: visibleName(name) }));
    const spacer = document.createElement("div");
    spacer.className = "hex-spacer";
    const layer = document.createElement("div");
    layer.className = "hex-layer";
    viewport.append(spacer, layer);
    body.append(viewport);
    const lines = Math.max(1, Math.ceil(bytes.length / PER_LINE));
    hex = { bytes, viewport, spacer, layer, lines, pool: [] };
    spacer.style.height = `${Math.min(lines * LINE_PX, MAX_SCROLL_PX)}px`;
    viewport.addEventListener("scroll", () => renderHex(hex));
    renderHex(hex);
  }

  function renderHex(h) {
    if (h !== hex) {
      return;
    }
    const visible = Math.max(1, Math.ceil(h.viewport.clientHeight / LINE_PX));
    const scaled = h.lines * LINE_PX > MAX_SCROLL_PX;
    const top = h.viewport.scrollTop;
    const scrollable = Math.max(1, h.spacer.offsetHeight - h.viewport.clientHeight);
    const topFloat = scaled ? (top / scrollable) * Math.max(0, h.lines - visible) : top / LINE_PX;
    const first = Math.max(0, Math.floor(topFloat) - HEX_MARGIN);
    const last = Math.min(h.lines, Math.floor(topFloat) + visible + HEX_MARGIN);
    const n = Math.max(0, last - first);
    while (h.pool.length > n) {
      h.pool.pop().remove();
    }
    while (h.pool.length < n) {
      const el = document.createElement("div");
      el.className = "hex-line";
      h.layer.append(el);
      h.pool.push(el);
    }
    for (let i = 0; i < n; i++) {
      h.pool[i].textContent = hexLine(h.bytes, first + i);
    }
    h.layer.style.transform = `translateY(${Math.max(0, top - (topFloat - first) * LINE_PX)}px)`;
    h.viewport.dataset.lines = String(h.lines);
    h.viewport.dataset.rendered = String(n);
  }

  function kindName(kind) {
    switch (kind) {
      case TEXT:
        return t("preview.kind.text");
      case IMAGE:
        return t("preview.kind.image");
      default:
        return t("preview.kind.hex");
    }
  }

  function render(name, m) {
    if (m.kind === FOLDER) {
      head(name, t("preview.folder", { count: count(m.size) }));
      note(t("preview.folder.note"));
      return;
    }
    head(name, `${kindName(m.kind)} · ${t("preview.size", { bytes: count(m.size) })}`);
    if (m.kind === TEXT) {
      showText(new TextDecoder().decode(m.body));
    } else if (m.kind === IMAGE) {
      showImage(m.body, m.mime, name);
    } else {
      showHex(m.body, name);
    }
  }

  async function read(index, row, generation) {
    const mine = ++seq;
    if (op !== 0) {
      core.invoke("cancel", { op }).catch(() => {});
    }
    const mineOp = nextOp();
    op = mineOp;
    head(row.name, t("preview.loading"));
    try {
      const buffer = await core.invoke("preview", { session: session(), row: index, generation, op: mineOp });
      if (mine !== seq) {
        return;
      }
      render(row.name, decodePreview(buffer));
    } catch (e) {
      // A superseded read ends Cancelled, a view replaced meanwhile is stale: neither is shown.
      if (mine !== seq || (e && e.class === "Cancelled") || (e && e.class === "Usage" && String(e.detail).startsWith("stale view"))) {
        return;
      }
      // A refusal (the cap, an unsupported archive, a password cancelled) belongs to the pane,
      // not to the page's error flags.
      head(row.name, "");
      const text = e && typeof e === "object" ? (e.detail ? `${e.message} ${e.detail}` : (e.message ?? JSON.stringify(e))) : String(e);
      note(t("preview.error", { error: text }));
    } finally {
      if (op === mineOp && mine === seq) {
        op = 0;
      }
    }
  }

  function ask(index, row, generation) {
    clearTimeout(timer);
    current = { index, row, generation };
    if (!shown) {
      return;
    }
    if (row.kind === 2) {
      seq++;
      head(row.name, "");
      note(t("preview.link"));
      return;
    }
    timer = setTimeout(() => read(index, row, generation).catch(showError), REST_MS);
  }

  function setShown(on) {
    shown = on;
    pane.hidden = !on;
    toggle.setAttribute("aria-pressed", String(on));
    if (!on) {
      clearTimeout(timer);
      seq++;
      if (op !== 0) {
        core.invoke("cancel", { op }).catch(() => {});
        op = 0;
      }
      release();
    } else if (current) {
      ask(current.index, current.row, current.generation);
    } else {
      empty();
    }
  }

  toggle.addEventListener("click", () => setShown(!shown));
  document.addEventListener("keydown", (e) => {
    if (e.altKey && !e.ctrlKey && !e.metaKey && e.key.toLowerCase() === "p") {
      e.preventDefault();
      setShown(!shown);
    }
  });
  empty();

  return {
    /** The focused row changed (it rests for REST_MS before it is read). */
    focused(index, row, generation) {
      ask(index, row, generation);
    },
    /** Show this entry now (a double-click on a type that is not opened directly). */
    show(index, row, generation) {
      current = { index, row, generation };
      if (!shown) {
        setShown(true);
        return;
      }
      clearTimeout(timer);
      read(index, row, generation).catch(showError);
    },
    /** No row is focused (a new archive, an empty view). */
    clear() {
      clearTimeout(timer);
      current = null;
      seq++;
      if (op !== 0) {
        core.invoke("cancel", { op }).catch(() => {});
        op = 0;
      }
      empty();
    },
    isShown: () => shown,
  };
}
