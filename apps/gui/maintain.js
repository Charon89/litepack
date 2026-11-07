// The maintenance dialogs (E5-8; docs/notes/e5-design.md section 6): Test, Check and repair,
// History (Browse, Roll back) and Update from folder for the open archive, and the queue the verb
// --test runs. The page never holds a path: folders are grants Rust minted (the native folder
// dialog), passwords are asked for by Rust's own native dialog, and the rollback's confirmation is
// a native box Rust shows (the page cannot answer it). DOM built with createElement and
// textContent only (Trusted Types); every string is in strings/en.json.

import { announce, setPhase, trapTab } from "./a11y.js";

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

export function createMaintainer(env) {
  const { core, t, nextOp, session, archive, showError, adopt, verbs } = env;
  const count = (n) => Number(n).toLocaleString();
  let dialog = null;
  // One live region for the dialog: it stays in the dialog for good (phase() replaces the other
  // children) and is set, a moment after each render, to the phase's first line, so a result is announced (the phase's own nodes are replaced).
  let live = null;
  let opener = null;
  let onEscape = () => {};
  // One dialog at a time: the verb's archives queue on the chain `main.js` shares with the
  // extract verbs, behind an open dialog.

  function ensureDialog() {
    if (dialog) {
      return dialog;
    }
    dialog = el("dialog", { class: "wizard", "aria-labelledby": "maintain-title", "aria-describedby": "maintain-body" });
    trapTab(dialog);
    live = el("p", { role: "status", class: "sr-only" });
    // Never detached: phase() replaces the other children around it.
    dialog.append(live);
    dialog.addEventListener("cancel", (e) => {
      e.preventDefault();
      onEscape();
    });
    dialog.addEventListener("keydown", (e) => {
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

  // One phase: a title, a body and the buttons ([label, handler, role, disabled]); the "primary"
  // one takes Enter, the "cancel" one takes Esc.
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
    setPhase(d, live, el("h2", { id: "maintain-title", text: title }), el("div", { class: "wizard-body", id: "maintain-body" }, ...body), footer);
    announce(d, live);
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

  // What the user sees of a failure.
  function failure(name, e) {
    if (e && e.class === "Cancelled") {
      return [el("p", { role: "alert", text: t("maintain.cancelled", { name }) })];
    }
    const message = e && typeof e === "object" ? `${e.message ?? ""} ${e.detail ?? ""}`.trim() : String(e);
    const nodes = [el("p", { text: t("maintain.error", { name, message }) })];
    for (const x of (e && e.entries) || []) {
      nodes.push(el("p", { text: t("maintain.entry", { path: x.path }) }));
    }
    // One alert per failure: its summary line.
    nodes[0].setAttribute("role", "alert");
    return nodes;
  }

  // A progress area: a bar when the engine knows the total, else a count.
  function progressArea() {
    const bar = el("progress", { max: 100, value: 0, "aria-label": t("progress.label") });
    const line = el("p", { role: "status", "aria-live": "polite" });
    const update = (ev) => {
      const total = ev.files_total ?? null;
      if (total) {
        bar.value = Math.round(Math.min(ev.files_done / total, 1) * 100);
        line.textContent = t("maintain.progress", { done: count(ev.files_done), total: count(total) });
      } else {
        line.textContent = t("maintain.progress.unknown", { done: count(ev.files_done) });
      }
    };
    return { nodes: [bar, line], update, line };
  }

  // Run command `name` with `args` under a progress phase and Cancel; the result or the error.
  async function heavy(title, intro, name, args) {
    const op = nextOp();
    const area = progressArea();
    const cancel = () => {
      core.invoke("cancel", { op }).catch(showError);
      area.line.textContent = t("maintain.cancelling");
    };
    phase(title, [el("p", { text: intro }), ...area.nodes], [[t("maintain.cancel"), cancel, "cancel"]]);
    const channel = new core.Channel();
    channel.onmessage = area.update;
    return core.invoke(name, { ...args, op, progress: channel });
  }

  // ---- Test ----

  // What a test found: the counts and the check made, or the failure.
  function testNodes(r) {
    if (r.state === "cancelled") {
      return [el("p", { text: t("test.cancelled", { name: r.archive }) })];
    }
    if (r.state === "failed") {
      return [el("p", { text: t("test.failed", { name: r.archive }) }), ...failure(r.archive, r.error)];
    }
    const nodes = [el("p", { text: t("test.passed", { name: r.archive, entries: count(r.entries), chunks: count(r.chunks), blocks: count(r.blocks) }) })];
    nodes.push(el("p", { class: "check", text: r.chunksChecked ? t("test.check.full") : t("test.check.keyless") }));
    return nodes;
  }

  function testButtons(r, again) {
    const buttons = [];
    if (r.offerUnlock) {
      buttons.push([t("test.unlock"), () => again(true), "primary"]);
    }
    if (r.offerRepair && r.fromSession) {
      buttons.push([t("test.repair"), () => checkDialog(), "primary"]);
    }
    buttons.push([t("maintain.close"), closeDialog, `${buttons.length === 0 ? "primary " : ""}cancel`]);
    return buttons;
  }

  async function testDialog(unlock = false) {
    const sid = session();
    if (sid === null) {
      return;
    }
    const title = t("test.title", { name: archive().name });
    let r;
    try {
      r = await heavy(title, t("test.running", { name: archive().name }), "test_run", { session: sid, unlock });
    } catch (e) {
      phase(title, failure(archive().name, e), [[t("maintain.close"), closeDialog, "primary cancel"]]);
      return;
    }
    r.fromSession = true;
    phase(title, testNodes(r), testButtons(r, (u) => testDialog(u)));
  }

  // ---- Check and repair ----

  async function checkDialog() {
    const sid = session();
    if (sid === null) {
      return;
    }
    const title = t("check.title", { name: archive().name });
    let r;
    try {
      r = await heavy(title, t("check.running", { name: archive().name }), "check_run", { session: sid });
    } catch (e) {
      phase(title, failure(archive().name, e), [[t("maintain.close"), closeDialog, "primary cancel"]]);
      return;
    }
    if (r.state === "cancelled") {
      phase(title, [el("p", { text: t("maintain.cancelled", { name: r.archive }) })], [[t("maintain.close"), closeDialog, "primary cancel"]]);
      return;
    }
    const close = [t("maintain.close"), closeDialog, "cancel"];
    if (!r.hasRecovery) {
      phase(title, [el("p", { text: t("check.none", { name: r.archive }) })], [[t("maintain.close"), closeDialog, "primary cancel"]]);
      return;
    }
    const nodes = [el("p", { text: t("check.counts", { frames: count(r.frames), unusable: count(r.framesUnusable), damaged: count(r.shardsDamaged) }) })];
    if (!r.damaged) {
      nodes.push(el("p", { class: "check", text: t("check.intact") }));
      phase(title, nodes, [[t("maintain.close"), closeDialog, "primary cancel"]]);
      return;
    }
    nodes.push(el("p", { text: t("check.damaged") }));
    const view = archive().generation !== archive().latest;
    if (view) {
      nodes.push(el("p", { text: t("maintain.why.view") }));
    }
    phase(title, nodes, [[t("check.repair"), () => repairDialog(sid, null), "primary", view], close]);
  }

  // "Repair to a copy": the folder is the archive's own unless the native picker chose another.
  async function repairDialog(sid, folder) {
    const title = t("repair.title", { name: archive().name });
    const choose = async () => {
      const [g] = await core.invoke("pick", { purpose: "destination" });
      return repairDialog(sid, g ?? folder);
    };
    phase(
      title,
      [
        el("p", { text: t("repair.about") }),
        el("p", { class: "check", text: folder ? t("repair.folder.chosen", { name: folder.name }) : t("repair.folder.own") }),
      ],
      [
        [t("repair.go"), () => repairRun(sid, folder), "primary"],
        [t("repair.choose"), choose],
        [t("maintain.cancel"), closeDialog, "cancel"],
      ],
    );
  }

  async function repairRun(sid, folder) {
    const title = t("repair.title", { name: archive().name });
    let r;
    try {
      r = await heavy(title, t("repair.running", { name: archive().name }), "repair_run", { session: sid, folder: folder ? folder.grant : null });
    } catch (e) {
      phase(title, failure(archive().name, e), [[t("maintain.close"), closeDialog, "primary cancel"]]);
      return;
    }
    if (r.state === "cancelled") {
      phase(title, [el("p", { text: t("repair.cancelled") })], [[t("maintain.close"), closeDialog, "primary cancel"]]);
      return;
    }
    const nodes = [el("p", { text: t("repair.done", { copy: r.copy, repaired: count(r.shardsRepaired), damaged: count(r.shardsDamaged) }) })];
    if (r.unrepairable) {
      nodes.push(el("p", { text: t("repair.unrepairable", { detail: r.unrepairable.detail }) }));
    }
    if (r.test) {
      nodes.push(...testNodes(r.test));
    }
    nodes.push(el("p", { class: "check", text: t("repair.original") }));
    const open = async () => {
      closeDialog();
      await adopt({ grant: r.grant.grant, name: r.grant.name });
    };
    const suspect = Boolean(r.unrepairable) || !r.test || r.test.state !== "passed";
    if (suspect) {
      nodes.push(el("p", { text: t("repair.suspect") }));
    }
    phase(title, nodes, [
      [t("repair.open"), open, suspect ? "" : "primary"],
      [t("maintain.close"), closeDialog, suspect ? "primary cancel" : "cancel"],
    ]);
  }

  // ---- History ----

  function generationLabel(g, open) {
    const parts = [t("history.generation", { generation: g.generation, size: count(g.size) })];
    if (g.latest) {
      parts.push(t("history.latest"));
    }
    if (g.generation === open) {
      parts.push(t("history.open"));
    }
    return parts.join(" · ");
  }

  async function historyDialog(selected = null) {
    const sid = session();
    if (sid === null) {
      return;
    }
    const title = t("history.title", { name: archive().name });
    let h;
    try {
      h = await core.invoke("history", { session: sid, op: nextOp() });
    } catch (e) {
      phase(title, failure(archive().name, e), [[t("maintain.close"), closeDialog, "primary cancel"]]);
      return;
    }
    const view = archive().generation !== archive().latest;
    let chosen = selected ?? h.open;
    const box = el("fieldset");
    box.append(el("legend", { text: t("history.legend") }));
    for (const g of h.generations) {
      const input = el("input", { type: "radio", name: "generation", value: g.generation, checked: g.generation === chosen });
      input.addEventListener("change", () => {
        chosen = g.generation;
        sync();
      });
      box.append(el("label", { class: "choice" }, input, el("span", { text: generationLabel(g, h.open) })));
    }
    const latest = h.generations.find((g) => g.latest).generation;
    const note = el("p", { class: "check" });
    const buttons = [
      [t("history.browse"), () => browse(sid, chosen), "primary"],
      [t("history.rollback"), () => rollback(sid, chosen, latest), "", false],
      [t("maintain.close"), closeDialog, "cancel"],
    ];
    phase(title, [box, note], buttons);
    const rollbackButton = dialog.querySelectorAll("button")[1];
    const sync = () => {
      const blocked = view || chosen === latest;
      rollbackButton.disabled = blocked;
      note.textContent = view ? t("maintain.why.view") : chosen === latest ? t("history.why.latest") : t("history.why.rollback", { generation: chosen });
    };
    sync();
  }

  async function browse(sid, generation) {
    let opened;
    try {
      opened = await core.invoke("history_browse", { session: sid, generation, op: nextOp() });
    } catch (e) {
      // A cancelled password prompt is the user's answer: nothing to say. Others stay here.
      if (!(e && e.class === "Cancelled")) {
        phase(t("history.title", { name: archive().name }), failure(archive().name, e), [[t("maintain.close"), closeDialog, "primary cancel"]]);
      }
      return;
    }
    closeDialog();
    await adopt(null, opened, sid);
  }

  async function rollback(sid, generation, latest) {
    const title = t("history.title", { name: archive().name });
    const op = nextOp();
    // The confirmation is a native box Rust shows once the rollback may start (it queues behind a
    // running operation); Cancel here ends the wait.
    phase(title, [el("p", { text: t("history.rolling", { generation }) })], [[t("maintain.cancel"), () => core.invoke("cancel", { op }).catch(showError), "cancel"]]);
    let r;
    try {
      r = await core.invoke("rollback_run", { session: sid, generation, op });
    } catch (e) {
      phase(title, failure(archive().name, e), [[t("maintain.close"), closeDialog, "primary cancel"]]);
      return;
    }
    if (r.state === "declined") {
      return historyDialog(latest);
    }
    const nodes =
      r.state === "failed"
        ? [el("p", { text: t("history.failed", { name: r.archive }) }), ...failure(r.archive, r.error)]
        : [el("p", { text: t("history.rolled", { name: r.archive, generation: r.generation, lost: r.lost }) })];
    if (r.reopenError) {
      nodes.push(...failure(r.archive, r.reopenError));
    }
    phase(title, nodes, [[t("maintain.close"), closeDialog, "primary cancel"]]);
    if (r.opened) {
      await adopt(null, r.opened, sid);
    } else {
      await adopt(null, null, sid);
    }
  }

  // ---- Update from folder (append) ----

  async function updateDialog(folder = null, choices = null) {
    const sid = session();
    if (sid === null) {
      return;
    }
    const title = t("update.title", { name: archive().name });
    let plan;
    try {
      plan = await core.invoke("append_plan", { session: sid });
    } catch (e) {
      phase(title, failure(archive().name, e), [[t("maintain.close"), closeDialog, "primary cancel"]]);
      return;
    }
    const state = choices ?? { tier: plan.tier, recovery: plan.recovery, test: plan.test };
    const choose = async () => {
      const [g] = await core.invoke("pick", { purpose: "folder" });
      return updateDialog(g ?? folder, state);
    };
    const tiers = el("fieldset");
    tiers.append(el("legend", { text: t("update.tier.legend") }));
    for (const id of ["fast", "balanced", "store"]) {
      const input = el("input", { type: "radio", name: "tier", value: id, checked: id === state.tier });
      input.addEventListener("change", () => {
        state.tier = id;
      });
      const label = id === "fast" ? t("create.tier.fast") : id === "balanced" ? t("create.tier.balanced") : t("create.tier.store");
      tiers.append(el("label", { class: "choice" }, input, el("span", { text: label })));
    }
    const recovery = el("input", { type: "number", min: 0, max: plan.maxRecovery, value: state.recovery, "aria-label": t("update.recovery.label", { max: plan.maxRecovery }) });
    recovery.addEventListener("input", () => {
      state.recovery = Math.max(0, Math.min(plan.maxRecovery, Number(recovery.value) || 0));
    });
    const test = el("input", { type: "checkbox", checked: state.test });
    test.addEventListener("change", () => {
      state.test = test.checked;
    });
    const options = el("details", {});
    options.append(
      el("summary", { text: t("update.options") }),
      tiers,
      el("label", {}, el("span", { text: t("update.recovery.label", { max: plan.maxRecovery }) }), recovery),
      el("label", { class: "choice" }, test, el("span", { text: t("update.test.label") })),
    );
    const body = [
      el("p", { text: t("update.mirror") }),
      el("p", { class: "check", text: folder ? t("update.folder.chosen", { name: folder.name }) : t("update.folder.none") }),
      options,
    ];
    if (plan.encrypted) {
      body.push(el("p", { class: "check", text: t("update.password") }));
    }
    phase(title, body, [
      [t("update.go"), () => updateRun(sid, folder, state), "primary", folder === null],
      [t("update.choose"), choose],
      [t("maintain.cancel"), closeDialog, "cancel"],
    ]);
  }

  async function updateRun(sid, folder, state) {
    const title = t("update.title", { name: archive().name });
    let r;
    try {
      r = await heavy(title, t("update.running", { name: archive().name }), "append_run", {
        session: sid,
        request: { folder: folder.grant, tier: state.tier, recovery: state.recovery, test: state.test },
      });
    } catch (e) {
      // Every refusal comes before the session is closed (a failure after it is a result with
      // state "failed", the archive reopened), so the page keeps its session.
      phase(title, failure(archive().name, e), [[t("maintain.close"), closeDialog, "primary cancel"]]);
      return;
    }
    const nodes = [];
    if (r.state === "cancelled") {
      nodes.push(el("p", { text: t("update.cancelled", { name: r.archive }) }));
    } else if (r.state === "failed") {
      nodes.push(el("p", { text: t("update.failed", { name: r.archive }) }), ...failure(r.archive, r.error));
    } else {
      nodes.push(el("p", { text: t("update.done", { name: r.archive, generation: r.generation, entries: count(r.entries), chunks: count(r.chunks), blocks: count(r.blocks), size: count(r.archiveLen) }) }));
      if (r.test) {
        nodes.push(...testNodes(r.test));
      }
    }
    if (r.reopenError) {
      nodes.push(...failure(r.archive, r.reopenError));
    }
    phase(title, nodes, [[t("maintain.close"), closeDialog, "primary cancel"]]);
    await adopt(null, r.opened ?? null, sid);
  }

  // ---- The verb --test ----

  function runVerb(items) {
    verbs.chain = verbs.chain.then(() => queue(items)).catch(showError);
    return verbs.chain;
  }

  // Each archive tested in order through the heavy queue, one result each, "k of n" while it
  // runs; Cancel ends the queue. An archive's result may offer Unlock, which re-runs that one.
  async function queue(items) {
    const results = el("ul", { class: "results" });
    let stopped = false;
    let currentOp = null;
    const title = t("test.dialog");
    const show = (head, area, done) => {
      phase(title, [el("p", { text: head }), ...area, results], [
        done
          ? [t("maintain.close"), closeDialog, "primary cancel"]
          : [
              t("maintain.cancel"),
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
    const attempt = async (item, index, unlock) => {
      const li = el("li");
      const op = nextOp();
      currentOp = op;
      const area = progressArea();
      show(t("test.queue", { n: index + 1, total: items.length, name: item.name }), area.nodes, false);
      const channel = new core.Channel();
      channel.onmessage = area.update;
      try {
        const r = await core.invoke("test_archive", { grant: item.grant, unlock, op, progress: channel });
        li.replaceChildren(...testNodes(r));
        if (r.state === "cancelled") {
          stopped = true;
        }
        if (r.offerUnlock) {
          const b = el("button", { type: "button", text: t("test.unlock") });
          b.addEventListener("click", () => {
            b.disabled = true;
            verbs.chain = verbs.chain
              .then(async () => {
                stopped = false;
                li.replaceWith(await attempt(item, index, true));
              })
              .catch(showError)
              .then(finish);
          });
          li.append(b);
        }
      } catch (e) {
        li.replaceChildren(...failure(item.name, e));
        if (e && e.class === "Cancelled") {
          stopped = true;
        }
      }
      currentOp = null;
      return li;
    };
    const finish = () => show(t("test.queue", { n: items.length, total: items.length, name: items[items.length - 1].name }), [], true);
    try {
      for (let i = 0; i < items.length && !stopped; i += 1) {
        results.append(await attempt(items[i], i, false));
      }
    } finally {
      finish();
    }
  }

  return { testDialog, checkDialog, historyDialog, updateDialog, runVerb };
}
