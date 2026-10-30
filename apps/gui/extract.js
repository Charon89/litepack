// The extract wizard (E5-4; docs/notes/e5-design.md section 6): one modal dialog for the open
// archive, and the queue the verbs --extract-here and --extract-to run. The page never holds a
// path: destinations are named by kind or by a grant Rust minted (the native folder dialog, a
// recent folder), and passwords are asked for by Rust's own native dialog. DOM built with
// createElement and textContent only (Trusted Types); every string is in strings/en.json.
//
// The dialog: Smart (default), a new folder, this folder, Choose... and the recent folders; all
// of the archive or the picks inside the current view. Before anything is written Rust counts
// what would be written, what exists and the free space (extract_plan); existing items are
// asked about once (Skip, Keep both, Replace, Cancel). Then progress with Cancel, and a result.

const GIB = 2 ** 30;
const MIB = 2 ** 20;

function el(tag, attrs = {}, ...children) {
  const e = document.createElement(tag);
  for (const [k, v] of Object.entries(attrs)) {
    if (k === "text") {
      e.textContent = v;
    } else if (v === true) {
      e.setAttribute(k, "");
    } else if (v !== false && v !== null && v !== undefined) {
      e.setAttribute(k, String(v));
    }
  }
  e.append(...children);
  return e;
}

export function createExtractor(env) {
  const { core, t, nextOp, session, picked, showError } = env;
  const count = (n) => Number(n).toLocaleString();
  const size = (n) => (n >= GIB ? `${(n / GIB).toFixed(1)} GiB` : `${Math.ceil(n / MIB).toLocaleString()} MiB`);
  let dialog = null;
  let opener = null;
  let onEscape = () => {};
  // One dialog at a time: verbs queue behind one another and behind an open dialog.
  let chain = Promise.resolve();

  function ensureDialog() {
    if (dialog) {
      return dialog;
    }
    dialog = el("dialog", { class: "wizard", "aria-labelledby": "wizard-title" });
    dialog.addEventListener("cancel", (e) => {
      e.preventDefault();
      onEscape();
    });
    dialog.addEventListener("keydown", (e) => {
      // Enter takes the primary action everywhere but on a button (which it presses) and in a
      // text field; a radio button or checkbox does not keep it.
      const own = ["BUTTON", "SELECT", "TEXTAREA"].includes(e.target.tagName) || (e.target.tagName === "INPUT" && !["radio", "checkbox"].includes(e.target.type));
      if (e.key === "Enter" && !own) {
        const primary = dialog.querySelector("button.primary:not([disabled])");
        if (primary) {
          e.preventDefault();
          primary.click();
        }
      }
    });
    dialog.addEventListener("close", () => {
      if (opener && document.contains(opener)) {
        opener.focus();
      }
    });
    document.body.append(dialog);
    return dialog;
  }

  // Show one phase: a title, a body and the buttons ([label, handler, role]); the first button
  // with role "primary" takes Enter, the one with role "cancel" takes Esc.
  function phase(title, body, buttons) {
    const d = ensureDialog();
    const footer = el("div", { class: "wizard-buttons" });
    onEscape = () => {};
    let first = null;
    for (const [label, handler, role, disabled] of buttons) {
      const roles = (role ?? "").split(" ");
      const b = el("button", { type: "button", text: label, class: roles.includes("primary") ? "primary" : null, disabled: Boolean(disabled) });
      b.addEventListener("click", () => Promise.resolve(handler(b)).catch(showError));
      footer.append(b);
      if (roles.includes("cancel")) {
        onEscape = () => b.click();
      }
      if (roles.includes("primary") && !first) {
        first = b;
      }
    }
    d.replaceChildren(el("h2", { id: "wizard-title", text: title }), el("div", { class: "wizard-body" }, ...body), footer);
    if (!d.open) {
      opener = document.activeElement;
      d.showModal();
    }
    (first ?? footer.querySelector("button"))?.focus();
  }

  function closeDialog() {
    onEscape = () => {};
    dialog?.close();
  }

  function radios(name, items, current, onChange) {
    const box = el("fieldset");
    for (const item of items) {
      const input = el("input", { type: "radio", name, value: item.id, checked: item.id === current, disabled: Boolean(item.disabled) });
      input.addEventListener("change", () => onChange(item));
      box.append(el("label", { class: "choice" }, input, el("span", { text: item.label })));
    }
    return box;
  }

  // What the user sees of a failure: the text, the entries that failed, the refusal's figures.
  function failure(name, e) {
    const nodes = [];
    let allow = null;
    if (e && e.class === "Cancelled") {
      nodes.push(el("p", { text: t("extract.cancelled.none", { name }) }));
    } else if (e && e.refusal && e.refusal.field === "--memory") {
      allow = e.refusal.needed;
      nodes.push(el("p", { text: t("extract.refusal", { name, needed: size(e.refusal.needed), allowed: size(e.refusal.allowed) }) }));
    } else if (e && e.refusal && e.refusal.field === "disk") {
      nodes.push(el("p", { text: t("extract.disk", { needed: count(e.refusal.needed), free: count(e.refusal.allowed) }) }));
    } else {
      const message = e && typeof e === "object" ? `${e.message ?? ""} ${e.detail ?? ""}`.trim() : String(e);
      nodes.push(el("p", { text: t("extract.error", { name, message }) }));
    }
    if (e && e.entries && e.entries.length > 0) {
      nodes.push(el("p", { text: t("extract.entries") }));
      const ul = el("ul", { class: "entries" });
      for (const x of e.entries) {
        ul.append(el("li", { text: t("extract.entry", { path: x.path, reason: x.reason }) }));
      }
      nodes.push(ul);
    }
    return { nodes, allow };
  }

  // A finished extraction: counts, what was skipped, the links, the Mark of the Web, the check.
  function summary(r) {
    if (r.outcome === "cancelled-before-writing") {
      return [el("p", { text: t("extract.cancelled.none", { name: r.archive }) })];
    }
    const nodes = [];
    if (r.outcome === "cancelled-after-writing") {
      // D-87 (2): the finished files stay, the unfinished one is removed.
      nodes.push(el("p", { text: t("extract.cancelled.kept", { name: r.archive, folder: r.folder }) }));
    } else {
      nodes.push(el("p", { text: t("extract.done", { name: r.archive, files: count(r.files), bytes: count(r.bytes), folder: r.folder }) }));
    }
    if (r.skipped > 0) {
      nodes.push(el("p", { text: t("extract.skipped", { count: count(r.skipped) }) }));
    }
    if (r.renamed > 0) {
      nodes.push(el("p", { text: t("extract.renamed", { count: count(r.renamed) }) }));
    }
    if (r.leftOut > 0) {
      nodes.push(el("p", { text: t("extract.leftout", { count: count(r.leftOut) }) }));
    }
    if (r.skippedSymlinks.length > 0) {
      nodes.push(el("p", { text: t("extract.links.skipped", { list: r.skippedSymlinks.join(", ") }) }));
    }
    for (const x of r.refusedSymlinks) {
      nodes.push(el("p", { text: t("extract.links.refused", { reason: x.reason, path: x.path }) }));
    }
    if (r.unprivilegedSymlinks.length > 0) {
      nodes.push(el("p", { text: t("extract.links.unprivileged", { list: r.unprivilegedSymlinks.join(", ") }) }));
    }
    if (r.motwUnmarked) {
      nodes.push(el("p", { text: t("extract.motw") }));
    }
    // No check line without a written file, and none for a cancelled run.
    const kind = r.check ? r.check.kind : null;
    if (kind === "blake3") {
      nodes.push(el("p", { class: "check", text: t("extract.check.blake3") }));
    } else if (kind === "members") {
      nodes.push(el("p", { class: "check", text: t("extract.check.members") }));
    } else if (kind === "partial") {
      nodes.push(el("p", { class: "check", text: t("extract.check.partial", { matched: count(r.check.matched), none: count(r.check.none) }) }));
    } else if (kind === "unverifiable") {
      nodes.push(el("p", { class: "check", text: t("extract.check.unverifiable") }));
    }
    return nodes;
  }

  // A progress area and its updater; `planned` (bytes) gives an ETA when the total is known.
  function progressArea(planned) {
    const bar = el("progress", { max: 100, value: 0 });
    const line = el("p", { role: "status", "aria-live": "polite" });
    const started = Date.now();
    const update = (ev) => {
      const elapsed = Math.max((Date.now() - started) / 1000, 0.001);
      const rate = (ev.bytes_out / elapsed / 1e6).toFixed(1);
      const total = ev.files_total ?? null;
      if (total) {
        const f = Math.min(ev.files_done / total, 1);
        bar.value = Math.round(f * 100);
        const eta = f > 0.02 && f < 1 ? t("extract.eta", { seconds: Math.ceil((elapsed * (1 - f)) / f) }) : "";
        line.textContent = t("extract.progress", { done: count(ev.files_done), total: count(total), bytes: count(ev.bytes_out), rate, eta });
      } else {
        line.textContent = t("extract.progress.unknown", { done: count(ev.files_done), bytes: count(ev.bytes_out), rate });
      }
    };
    void planned;
    return { nodes: [bar, line], update, line };
  }

  // The dialog for the open archive.
  async function openDialog() {
    const sid = session();
    if (sid === null) {
      return;
    }
    const recents = await core.invoke("recents");
    const dests = [
      { id: "smart", label: t("extract.dest.smart"), dest: { kind: "smart" } },
      { id: "newfolder", label: t("extract.dest.newfolder"), dest: { kind: "newfolder" } },
      { id: "here", label: t("extract.dest.here"), dest: { kind: "here" } },
    ];
    recents.destinations.forEach((g, i) => {
      dests.push({ id: `recent-${i}`, label: t("extract.dest.recent", { name: g.name }), dest: { kind: "folder", grant: g.grant } });
    });
    const state = { dest: dests[0], scope: picked() > 0 ? "selected" : "all" };
    let chosen = 0;

    const form = (note = null) => {
      const scopes = [
        { id: "all", label: t("extract.scope.all") },
        { id: "selected", label: t("extract.scope.selected", { count: count(picked()) }), disabled: picked() === 0 },
      ];
      const choose = el("button", { type: "button", text: t("extract.dest.choose") });
      choose.addEventListener("click", async () => {
        let g;
        try {
          [g] = await core.invoke("pick", { purpose: "destination" });
        } catch (e) {
          // A refused or failed picker is shown, not left as an unhandled rejection.
          showError(e);
          return;
        }
        if (g) {
          chosen += 1;
          const item = { id: `chosen-${chosen}`, label: t("extract.dest.chosen", { name: g.name }), dest: { kind: "folder", grant: g.grant } };
          dests.splice(3, 0, item);
          state.dest = item;
          form();
        }
      });
      const body = [
        el("legend", { text: t("extract.dest.legend") }),
        radios("dest", dests, state.dest.id, (item) => {
          state.dest = item;
        }),
        choose,
        el("legend", { text: t("extract.scope.legend") }),
        radios("scope", scopes, state.scope, (item) => {
          state.scope = item.id;
        }),
      ];
      if (note) {
        body.push(...note);
      }
      phase(t("extract.title", { name: env.archiveName() }), body, [
        [t("extract.go"), () => plan(), "primary"],
        [t("extract.cancel"), closeDialog, "cancel"],
      ]);
    };

    const plan = async () => {
      let p;
      try {
        p = await core.invoke("extract_plan", { session: sid, dest: state.dest.dest, scope: state.scope, op: nextOp() });
      } catch (e) {
        form(failure(env.archiveName(), e).nodes);
        return;
      }
      if (p.existing === 0) {
        return run(p, "refuse", null);
      }
      const answer = (c) => () => run(p, c, null);
      phase(
        t("extract.title", { name: env.archiveName() }),
        [el("p", { text: t("extract.conflict", { count: count(p.existing), folder: p.folder }) })],
        [
          [t("extract.skip"), answer("skip"), "primary"],
          [t("extract.keep"), answer("rename")],
          [t("extract.replace"), answer("replace")],
          [t("extract.cancel"), closeDialog, "cancel"],
        ],
      );
    };

    const run = async (p, conflict, memory) => {
      const op = nextOp();
      const area = progressArea(p.bytes);
      const cancel = () => {
        core.invoke("cancel", { op }).catch(showError);
        area.line.textContent = t("extract.cancelling");
      };
      area.nodes.unshift(el("p", { text: t("extract.plan", { files: count(p.files), bytes: count(p.bytes), folder: p.folder }) }));
      phase(t("extract.title", { name: env.archiveName() }), area.nodes, [[t("extract.cancel"), cancel, "cancel"]]);
      const channel = new core.Channel();
      channel.onmessage = area.update;
      const request = { dest: state.dest.dest, scope: state.scope, conflict, memory };
      try {
        const r = await core.invoke("extract_run", { session: sid, request, op, progress: channel });
        phase(t("extract.title", { name: env.archiveName() }), summary(r), [[t("extract.close"), closeDialog, "primary cancel"]]);
      } catch (e) {
        const f = failure(env.archiveName(), e);
        const buttons = [[t("extract.close"), closeDialog, "cancel"]];
        if (f.allow !== null) {
          buttons.unshift([t("extract.allow", { needed: size(f.allow) }), () => run(p, conflict, f.allow), "primary"]);
        }
        phase(t("extract.title", { name: env.archiveName() }), f.nodes, buttons);
      }
    };

    form();
  }

  // The verbs: each archive extracted beside itself (`kind` "here": Smart; "to": a new folder),
  // in order, one result each, "k of n" while it runs.
  function runVerb(kind, items) {
    chain = chain.then(() => queue(kind, items)).catch(showError);
    return chain;
  }

  // The queue's states, each with a way out:
  //   idle     no dialog; a verb arrives and runVerb chains it behind any run (and any Allow).
  //   running  one archive is extracting (shown with "k of n"); Cancel/Esc cancels it and ends
  //            the queue; an archive's password question is Rust's native dialog, which
  //            Cancel ends too.
  //   refused  an archive's result offers "Allow X"; this is part of `done`, never a state of
  //            its own: the dialog stays open on `done` and Allow queues a re-run on `chain`.
  //   done     every archive has its result; Close/Esc/Enter close the dialog. Every path of
  //            queue() and of an Allow re-run ends in show(..., done = true), also after errors.
  async function queue(kind, items) {
    const results = el("ul", { class: "results" });
    let stopped = false;
    let currentOp = null;
    const title = t("extract.dialog");
    const show = (head, area, done) => {
      phase(title, [el("p", { text: head }), ...area, results], [
        done
          ? [t("extract.close"), closeDialog, "primary cancel"]
          : [
              t("extract.cancel"),
              () => {
                stopped = true;
                if (currentOp !== null) {
                  core.invoke("cancel", { op: currentOp }).catch(showError);
                }
              },
              "cancel",
            ],
      ]);
    };
    const attempt = async (item, index, memory) => {
      const li = el("li");
      const op = nextOp();
      currentOp = op;
      const head = t("extract.queue", { n: index + 1, total: items.length, name: item.name });
      const area = progressArea(null);
      show(head, area.nodes, false);
      const channel = new core.Channel();
      channel.onmessage = area.update;
      try {
        const r = await core.invoke("extract_archive", { grant: item.grant, verb: kind, memory, op, progress: channel });
        li.replaceChildren(...summary(r));
        if (r.outcome !== "done") {
          stopped = true;
        }
      } catch (e) {
        const f = failure(item.name, e);
        li.replaceChildren(...f.nodes);
        if (f.allow !== null) {
          const allow = el("button", { type: "button", text: t("extract.allow", { needed: size(f.allow) }) });
          allow.addEventListener("click", () => {
            allow.disabled = true;
            // Inside the chain, so that a later verb queues behind it; ends in `done`.
            chain = chain
              .then(async () => {
                stopped = false;
                const again = await attempt(item, index, f.allow);
                li.replaceWith(again);
              })
              .catch(showError)
              .then(finish);
          });
          li.append(allow);
        }
        if (e && e.class === "Cancelled") {
          stopped = true;
        }
      }
      currentOp = null;
      return li;
    };
    const finish = () => show(t("extract.queue", { n: items.length, total: items.length, name: items[items.length - 1].name }), [], true);
    try {
      for (let i = 0; i < items.length && !stopped; i += 1) {
        results.append(await attempt(items[i], i, null));
      }
    } finally {
      finish();
    }
  }

  return { openDialog, runVerb };
}
