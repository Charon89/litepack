// The virtual list (E5-3; docs/notes/e5-design.md section 3). The webview holds only the rows
// it shows plus MARGIN_ROWS above and below: at most visible + 2·MARGIN_ROWS recycled row
// elements, filled from one binary batch (batch.js) that Rust cuts from its view. Sort, filter
// and selection live in Rust; a batch of an older view generation, or an older request, is
// dropped. The spacer is capped at MAX_SCROLL_PX and scrolling is scaled beyond it;
// aria-rowcount and aria-rowindex give the true size.
import { decodeBatch } from "./batch.js";

// The same values as crates/lpk-gui/src/browse.rs (MARGIN_ROWS, MAX_SCROLL_PX).
export const MARGIN_ROWS = 64;
export const MAX_SCROLL_PX = 8000000;
// A row's height in CSS pixels (styles.css: .row height).
export const ROW_PX = 24;

const MARK_PICKED = 1;
const KIND_FOLDER = 1;

export class VirtualList {
  /**
   * @param {object} o
   * @param {HTMLElement} o.grid the treegrid (the one tab stop)
   * @param {HTMLElement} o.viewport the scrolling element
   * @param {HTMLElement} o.spacer sized to the (capped) height of every row
   * @param {HTMLElement} o.layer holds the row elements
   * @param {(first: number, count: number) => Promise<ArrayBuffer>} o.fetch a batch
   * @param {(row: object, index: number) => Array<string>} o.cells a row's cell texts
   * @param {(index: number, row: object, generation: number, folderOnly?: boolean) => void} o.activate
   *   Enter or a double-click, with the generation of the batch the row came from
   * @param {(action: string, index: number|null, generation: number) => Promise<void>} o.select a
   *   selection change, with the generation of the batch the row index came from (Rust refuses
   *   another)
   */
  constructor(o) {
    Object.assign(this, o);
    this.total = 0;
    this.generation = -1;
    this.level = 1;
    this.batch = null; // {generation, total, first, rows}
    this.seq = 0;
    this.applied = 0;
    this.pending = 0;
    this.focus = 0;
    this.pool = [];
    this.scheduled = false;
    this.focusSeen = "";
    this.viewport.addEventListener("scroll", () => this.schedule());
    new ResizeObserver(() => this.schedule()).observe(this.viewport);
    this.layer.addEventListener("click", (e) => this.onClick(e));
    this.layer.addEventListener("dblclick", (e) => this.onDoubleClick(e));
    this.grid.addEventListener("keydown", (e) => this.onKey(e));
    this.markPending();
  }

  /** The rows the viewport shows. */
  visible() {
    return Math.max(1, Math.ceil(this.viewport.clientHeight / ROW_PX));
  }

  /** A new view: its generation and size; the scroll goes to the top unless `keep`. */
  reset(info, keep = false) {
    this.generation = info.generation;
    this.total = info.total;
    this.level = info.mode === "tree" ? info.crumbs.length : 1;
    this.batch = null;
    this.focusSeen = "";
    this.grid.setAttribute("aria-rowcount", String(this.total + 1));
    this.spacer.style.height = `${Math.min(this.total * ROW_PX, MAX_SCROLL_PX)}px`;
    if (!keep) {
      this.focus = 0;
      this.viewport.scrollTop = 0;
    }
    this.focus = Math.min(this.focus, Math.max(0, this.total - 1));
    return this.update();
  }

  /** Refetch the current window (after a selection change: the marks). */
  refresh() {
    this.batch = null;
    return this.update();
  }

  scaled() {
    return this.total * ROW_PX > MAX_SCROLL_PX;
  }

  /** The first visible row, fractional. */
  topRow() {
    const top = this.viewport.scrollTop;
    if (!this.scaled()) {
      return top / ROW_PX;
    }
    const scrollable = Math.max(1, this.spacer.offsetHeight - this.viewport.clientHeight);
    return (top / scrollable) * Math.max(0, this.total - this.visible());
  }

  /** Scroll so that row `index` is in view. */
  reveal(index) {
    const visible = this.visible();
    const top = Math.floor(this.topRow());
    if (index >= top && index < top + visible - 1) {
      return;
    }
    const want = index < top ? index : Math.max(0, index - visible + 2);
    if (!this.scaled()) {
      this.viewport.scrollTop = want * ROW_PX;
    } else {
      const scrollable = Math.max(1, this.spacer.offsetHeight - this.viewport.clientHeight);
      this.viewport.scrollTop = (want / Math.max(1, this.total - visible)) * scrollable;
    }
  }

  schedule() {
    if (this.scheduled) {
      return;
    }
    this.scheduled = true;
    requestAnimationFrame(() => {
      this.scheduled = false;
      this.update().catch((e) => this.onError(e));
    });
  }

  markPending() {
    this.grid.dataset.pending = String(this.pending);
    this.grid.dataset.visibleRows = String(this.visible());
    this.grid.dataset.marginRows = String(MARGIN_ROWS);
  }

  /** Fetch the window around the visible rows if the batch held does not cover it; render. */
  async update() {
    const visible = this.visible();
    const top = Math.floor(this.topRow());
    const first = Math.max(0, top - MARGIN_ROWS);
    const count = visible + 2 * MARGIN_ROWS;
    const b = this.batch;
    const covered =
      b &&
      b.generation === this.generation &&
      b.first <= Math.max(0, top - MARGIN_ROWS / 2) &&
      b.first + b.rows.length >= Math.min(this.total, top + visible + MARGIN_ROWS / 2);
    this.render();
    if (covered || this.total === 0) {
      this.markPending();
      return;
    }
    const seq = ++this.seq;
    this.pending++;
    this.markPending();
    try {
      const batch = decodeBatch(await this.fetch(first, count));
      // A batch of an older view, or an answer overtaken by a later request, is dropped.
      if (batch.generation === this.generation && seq > this.applied) {
        this.applied = seq;
        this.batch = batch;
        this.render();
      }
    } finally {
      this.pending--;
      this.markPending();
    }
  }

  /** Fill the recycled row elements from the batch held: the visible rows and the margins. */
  render() {
    const b = this.batch;
    if (!b || b.generation !== this.generation) {
      this.layer.replaceChildren();
      this.pool.length = 0;
      return;
    }
    const visible = this.visible();
    const topFloat = this.topRow();
    const top = Math.floor(topFloat);
    const from = Math.max(b.first, top - MARGIN_ROWS);
    const to = Math.min(b.first + b.rows.length, top + visible + MARGIN_ROWS);
    const n = Math.max(0, to - from);
    while (this.pool.length > n) {
      this.pool.pop().remove();
    }
    while (this.pool.length < n) {
      const row = document.createElement("div");
      row.className = "row";
      row.setAttribute("role", "row");
      for (let c = 0; c < 3; c++) {
        const cell = document.createElement("span");
        cell.setAttribute("role", "gridcell");
        if (c === 1) {
          cell.className = "num";
        }
        row.append(cell);
      }
      this.layer.append(row);
      this.pool.push(row);
    }
    for (let i = 0; i < n; i++) {
      const index = from + i;
      const r = b.rows[index - b.first];
      const el = this.pool[i];
      el.id = `lpk-row-${index}`;
      el.dataset.index = String(index);
      el.dataset.generation = String(b.generation);
      el.dataset.kind = String(r.kind);
      el.setAttribute("aria-rowindex", String(index + 2));
      el.setAttribute("aria-level", String(this.level));
      el.setAttribute("aria-selected", (r.marks & MARK_PICKED) !== 0 ? "true" : "false");
      // Folders only: a file never expands, whatever its level.
      if (r.kind === KIND_FOLDER) {
        el.setAttribute("aria-expanded", "false");
      } else {
        el.removeAttribute("aria-expanded");
      }
      el.classList.toggle("focused", index === this.focus);
      const texts = this.cells(r, index);
      for (let c = 0; c < 3; c++) {
        el.children[c].textContent = texts[c];
      }
    }
    // The focused row changed (or its view did): the pane previews it.
    const seen = `${b.generation}:${this.focus}`;
    const fr = this.rowAt(this.focus);
    if (this.total > 0 && fr && seen !== this.focusSeen) {
      this.focusSeen = seen;
      this.onFocus?.(this.focus, fr, b.generation);
    }
    // The layer sits where its first row belongs on screen.
    const y = this.viewport.scrollTop - (topFloat - from) * ROW_PX;
    this.layer.style.transform = `translateY(${Math.max(0, y)}px)`;
    // Only a rendered row may be named; a focus row scrolled out of the window names none.
    if (this.total > 0 && this.focus >= from && this.focus < to) {
      this.grid.setAttribute("aria-activedescendant", `lpk-row-${this.focus}`);
    } else {
      this.grid.removeAttribute("aria-activedescendant");
    }
  }

  /** The generation of the batch held (the view its row indices belong to), else the view's. */
  heldGeneration() {
    return this.batch ? this.batch.generation : this.generation;
  }

  /** The generation of the rendered row element of event `e`. */
  generationOf(e) {
    const el = e.target.closest("[role=row]");
    return el && el.dataset.generation !== undefined ? Number(el.dataset.generation) : this.heldGeneration();
  }

  /** The row held for view row `index`, if any. */
  rowAt(index) {
    const b = this.batch;
    if (!b || index < b.first || index >= b.first + b.rows.length) {
      return null;
    }
    return b.rows[index - b.first];
  }

  indexOf(e) {
    const el = e.target.closest("[role=row]");
    return el && el.dataset.index !== undefined ? Number(el.dataset.index) : null;
  }

  async moveTo(index, e) {
    if (this.total === 0) {
      return;
    }
    this.focus = Math.max(0, Math.min(this.total - 1, index));
    this.reveal(this.focus);
    if (e.ctrlKey && !e.shiftKey) {
      this.render();
      return;
    }
    await this.select(e.shiftKey ? (e.ctrlKey ? "extend" : "range") : "only", this.focus, this.heldGeneration());
  }

  onClick(e) {
    const index = this.indexOf(e);
    if (index === null) {
      return;
    }
    this.focus = index;
    this.grid.focus();
    const action = e.shiftKey ? (e.ctrlKey ? "extend" : "range") : e.ctrlKey ? "toggle" : "only";
    this.select(action, index, this.generationOf(e)).catch((err) => this.onError(err));
  }

  onDoubleClick(e) {
    const index = this.indexOf(e);
    const row = index === null ? null : this.rowAt(index);
    if (row) {
      this.activate(index, row, this.generationOf(e));
    }
  }

  onKey(e) {
    // Keys on a header's sort button are the button's (Enter and Space sort).
    if (e.target !== this.grid) {
      return;
    }
    const page = Math.max(1, this.visible() - 1);
    const go = (i) => {
      e.preventDefault();
      this.moveTo(i, e).catch((err) => this.onError(err));
    };
    switch (e.key) {
      case "ArrowDown":
        return go(this.focus + 1);
      case "ArrowUp":
        if (e.altKey) {
          return this.parent?.(e);
        }
        return go(this.focus - 1);
      case "PageDown":
        return go(this.focus + page);
      case "PageUp":
        return go(this.focus - page);
      case "Home":
        return go(0);
      case "End":
        return go(this.total - 1);
      case " ":
        e.preventDefault();
        this.select(e.ctrlKey ? "toggle" : "only", this.focus, this.heldGeneration()).catch((err) => this.onError(err));
        return;
      case "Enter":
      case "ArrowRight": {
        const row = this.rowAt(this.focus);
        if (row && (e.key === "Enter" || (row.kind === KIND_FOLDER && this.level > 0))) {
          e.preventDefault();
          this.activate(this.focus, row, this.heldGeneration(), e.key === "ArrowRight");
        }
        return;
      }
      case "ArrowLeft":
      case "Backspace":
        return this.parent?.(e);
      case "Escape":
        e.preventDefault();
        this.select("none", null, this.heldGeneration()).catch((err) => this.onError(err));
        return;
      default:
        if ((e.key === "a" || e.key === "A") && e.ctrlKey) {
          e.preventDefault();
          this.select("all", null, this.heldGeneration()).catch((err) => this.onError(err));
        }
    }
  }
}
