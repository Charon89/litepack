// The create wizard (E5-5; docs/notes/e5-design.md section 6): one modal dialog, "New archive...".
// The page never holds a path: inputs and the destination are grants Rust minted from native
// pickers or from a verb's paths, the password is asked for by Rust's own native dialog (twice)
// and stays in Rust, and every time and size figure on a tier card comes from a calibration Rust
// ran on this machine (create_calibrate) - the page holds no rate of its own. DOM built with
// createElement and textContent only (Trusted Types); every string is in strings/en.json.
//
// The dialog: Inputs (Add files, Add folder, the list with Remove, the totals), Destination (a
// name, validated in Rust, and a folder: the first input's, Choose... or a recent one), the tier
// cards (Fast, Balanced, Store; Balanced selected), a password (asked twice) with "Encrypt file
// names", recovery (proposed by Rust), "Test when done". Create runs with progress and Cancel,
// then a summary with Open, Close and "Copy as command".

import { announce, trapTab } from "./a11y.js";

const KIB = 2 ** 10;
const MB = 1000 * 1000;
const MIB = 2 ** 20;
const GIB = 2 ** 30;
const TIERS = ["fast", "balanced", "store"];

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

export function createCreator(env) {
  const { core, t, nextOp, showError, openArchive } = env;
  const count = (n) => Number(n).toLocaleString();
  // Each tier's name and one line about it (literal keys, so that the strings test sees them).
  const tierTexts = {
    fast: [t("create.tier.fast"), t("create.tier.fast.about")],
    balanced: [t("create.tier.balanced"), t("create.tier.balanced.about")],
    store: [t("create.tier.store"), t("create.tier.store.about")],
  };
  const size = (n) => {
    if (n >= GIB) {
      return `${(n / GIB).toFixed(1)} GiB`;
    }
    if (n >= MIB) {
      return `${(n / MIB).toFixed(n >= 100 * MIB ? 0 : 1)} MiB`;
    }
    return `${Math.max(1, Math.ceil(n / KIB)).toLocaleString()} KiB`;
  };
  // A duration as an estimate would say it: seconds, then minutes, then hours.
  const span = (s) => {
    if (s < 1) {
      return t("create.time.under");
    }
    if (s < 90) {
      return t("create.time.seconds", { n: Math.round(s) });
    }
    if (s < 5400) {
      return t("create.time.minutes", { n: Math.round(s / 60) });
    }
    return t("create.time.hours", { n: (s / 3600).toFixed(1) });
  };
  let dialog = null;
  // One live region for the dialog: phase() appends it again after each render and sets it to
  // the phase's first line, so a result is announced (the phase's own nodes are replaced).
  let live = null;
  let opener = null;
  let onEscape = () => {};
  let chain = Promise.resolve();

  function ensureDialog() {
    if (dialog) {
      return dialog;
    }
    dialog = el("dialog", { class: "wizard create", "aria-labelledby": "create-title", "aria-describedby": "create-body" });
    trapTab(dialog);
    live = el("p", { role: "status", class: "sr-only" });
    dialog.addEventListener("cancel", (e) => {
      e.preventDefault();
      onEscape();
    });
    dialog.addEventListener("keydown", (e) => {
      // Enter creates when the form is valid, from any field but a button (which it presses),
      // a list or a text area.
      const own = ["BUTTON", "SELECT", "TEXTAREA"].includes(e.target.tagName) || (e.target.tagName === "INPUT" && e.target.readOnly);
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

  // Show one phase: a title, a body and the buttons ([label, handler, role, disabled]); "primary"
  // takes Enter, "cancel" takes Esc. Returns the buttons by label.
  function phase(title, body, buttons, focus = null) {
    const d = ensureDialog();
    const footer = el("div", { class: "wizard-buttons" });
    onEscape = () => {};
    const made = [];
    let first = null;
    for (const [label, handler, role, disabled] of buttons) {
      const roles = (role ?? "").split(" ");
      const b = el("button", { type: "button", text: label, class: roles.includes("primary") ? "primary" : null, disabled: Boolean(disabled) });
      b.addEventListener("click", () => Promise.resolve(handler(b)).catch(showError));
      footer.append(b);
      made.push(b);
      if (roles.includes("cancel")) {
        onEscape = () => b.click();
      }
      if (roles.includes("primary") && !first) {
        first = b;
      }
    }
    d.replaceChildren(el("h2", { id: "create-title", text: title }), el("div", { class: "wizard-body", id: "create-body" }, ...body), footer, live);
    announce(d, live);
    if (!d.open) {
      opener = document.activeElement;
      d.showModal();
    }
    (focus ?? first ?? footer.querySelector("button"))?.focus();
    return made;
  }

  function closeDialog() {
    onEscape = () => {};
    dialog?.close();
  }

  const message = (e) => (e && typeof e === "object" ? `${e.message ?? ""} ${e.detail ?? ""}`.trim() : String(e));

  // The wizard for `items` (grants of kind input; none from the bar's button).
  async function openWizard(items) {
    const s = {
      inputs: [],
      folder: null,
      name: "",
      nameOk: false,
      nameNote: "",
      tier: "balanced",
      recovery: 5,
      maxRecovery: 20,
      test: true,
      encryptNames: true,
      password: false,
      totals: { files: 0, bytes: 0 },
      estimates: { fast: null, balanced: null, store: null },
      nameEdited: false,
      folderChosen: false,
      recents: [],
    };
    // The inputs' generation: a newer set supersedes the calibration running for the older.
    let generation = 0;
    let calibrating = null;
    let nameGeneration = 0;
    let nameTimer = null;

    const note = el("p", { class: "note", role: "alert" });
    const inputsList = el("ul", { class: "inputs" });
    const totals = el("p", { class: "totals", role: "status" });
    const nameInput = el("input", { type: "text", id: "create-name", autocomplete: "off", spellcheck: "false", "aria-describedby": "create-name-note" });
    const nameNote = el("p", { id: "create-name-note", class: "note", role: "alert" });
    const folderLine = el("span", { class: "folder" });
    const recentsSelect = el("select", { id: "create-recent", "aria-label": t("create.folder.recent") });
    const cards = {};
    const recovery = el("input", { type: "number", id: "create-recovery", min: 0, max: 20, step: 1 });
    const testBox = el("input", { type: "checkbox", id: "create-test", checked: true });
    const passwordState = el("span", { class: "password-state" });
    const passwordSet = el("button", { type: "button", text: t("create.password.set") });
    const passwordClear = el("button", { type: "button", text: t("create.password.clear") });
    const namesBox = el("input", { type: "checkbox", id: "create-names", checked: true });
    const namesLabel = el("label", { class: "choice" }, namesBox, el("span", { text: t("create.password.names") }));
    let createButton = null;

    const validRecovery = () => Number.isInteger(s.recovery) && s.recovery >= 0 && s.recovery <= s.maxRecovery;
    const valid = () => s.inputs.length > 0 && s.folder !== null && s.nameOk && Number.isInteger(s.recovery) && s.recovery >= 0 && s.recovery <= s.maxRecovery;
    const refreshCreate = () => {
      if (createButton) {
        createButton.disabled = !valid();
      }
    };

    function renderInputs() {
      inputsList.replaceChildren();
      for (const [i, item] of s.inputs.entries()) {
        const remove = el("button", { type: "button", text: t("create.inputs.remove"), "aria-label": t("create.inputs.remove.named", { name: item.name }) });
        remove.addEventListener("click", () => {
          s.inputs.splice(i, 1);
          inputsChanged().catch(showError);
        });
        inputsList.append(el("li", {}, el("span", { class: "name", text: item.name }), remove));
      }
      totals.textContent = s.inputs.length === 0 ? t("create.inputs.none") : t("create.inputs.totals", { files: count(s.totals.files), bytes: size(s.totals.bytes) });
    }

    function renderTiers() {
      for (const tier of TIERS) {
        const e = s.estimates[tier];
        const out = cards[tier].estimate;
        if (s.inputs.length === 0) {
          out.textContent = "";
        } else if (e === null) {
          out.textContent = t("create.estimating");
        } else if (e.seconds === null || e.bytes === null) {
          out.textContent = t("create.estimate.none");
        } else {
          out.textContent = t("create.estimate", { time: span(e.seconds), size: size(e.shown ?? e.bytes) });
        }
      }
    }

    function renderFolder() {
      folderLine.textContent = s.folder ? t("create.folder.is", { name: s.folder.name }) : t("create.folder.none");
      recentsSelect.replaceChildren(el("option", { value: "", text: t("create.folder.recent") }));
      for (const [i, g] of s.recents.entries()) {
        recentsSelect.append(el("option", { value: String(i), text: g.name }));
      }
      recentsSelect.hidden = s.recents.length === 0;
    }

    function renderPassword() {
      passwordState.textContent = s.password ? t("create.password.is") : t("create.password.none");
      passwordClear.hidden = !s.password;
      namesLabel.hidden = !s.password;
      passwordSet.textContent = s.password ? t("create.password.change") : t("create.password.set");
    }

    // The name is checked in Rust (the same rule the run applies); an existing target shows now.
    function checkName() {
      clearTimeout(nameTimer);
      const mine = (nameGeneration += 1);
      s.nameOk = false;
      refreshCreate();
      if (!s.folder) {
        nameNote.textContent = "";
        return Promise.resolve();
      }
      return new Promise((resolve) => {
        nameTimer = setTimeout(async () => {
          try {
            const c = await core.invoke("create_name", { folder: s.folder.grant, name: s.name });
            if (mine === nameGeneration) {
              s.nameOk = !c.exists;
              nameNote.textContent = c.exists ? t("create.name.exists", { name: c.file }) : "";
            }
          } catch (e) {
            if (mine === nameGeneration) {
              s.nameOk = false;
              nameNote.textContent = message(e);
            }
          }
          if (mine === nameGeneration) {
            refreshCreate();
          }
          resolve();
        }, 120);
      });
    }

    // The size shown includes the recovery records: Rust adds them to the sampled size, so a
    // change of the recovery field samples nothing again.
    async function rescale(tier) {
      const e = s.estimates[tier];
      if (e && e.bytes !== null && e.bytes !== undefined) {
        e.shown = await core.invoke("create_scale", { bytes: e.bytes, recovery: validRecovery() ? s.recovery : 0 });
      }
    }

    // One calibration at a time on Rust's blocking thread: the selected tier first, then the
    // others; a change of inputs cancels the running one and starts over.
    async function calibrate() {
      generation += 1;
      const mine = generation;
      if (calibrating !== null) {
        core.invoke("cancel", { op: calibrating }).catch(() => {});
        calibrating = null;
      }
      for (const tier of TIERS) {
        s.estimates[tier] = null;
      }
      renderTiers();
      if (s.inputs.length === 0) {
        return;
      }
      const order = [s.tier, ...TIERS.filter((x) => x !== s.tier)];
      const grants = s.inputs.map((i) => i.grant);
      for (const tier of order) {
        if (mine !== generation) {
          return;
        }
        const op = nextOp();
        calibrating = op;
        try {
          const e = await core.invoke("create_calibrate", { inputs: grants, tier, op });
          if (mine !== generation) {
            return;
          }
          s.estimates[tier] = e;
          await rescale(tier);
          if (mine !== generation) {
            return;
          }
        } catch (e) {
          if (mine !== generation) {
            return;
          }
          // A failed estimate is "no estimate", not an error dialog: Create still works.
          s.estimates[tier] = { seconds: null, bytes: null };
        }
        calibrating = null;
        renderTiers();
      }
    }

    // The inputs changed: Rust counts them and proposes the folder and name (unless the user
    // already chose), then the estimates start over.
    async function inputsChanged() {
      note.textContent = "";
      if (s.inputs.length === 0) {
        s.totals = { files: 0, bytes: 0 };
        renderInputs();
        refreshCreate();
        await calibrate();
        return;
      }
      let plan;
      try {
        plan = await core.invoke("create_plan", { inputs: s.inputs.map((i) => i.grant) });
      } catch (e) {
        note.textContent = message(e);
        s.inputs.pop();
        renderInputs();
        return;
      }
      s.totals = { files: plan.files, bytes: plan.bytes };
      s.maxRecovery = plan.maxRecovery;
      recovery.max = String(plan.maxRecovery);
      if (!s.folderChosen && plan.folder) {
        s.folder = plan.folder;
        renderFolder();
      }
      if (!s.nameEdited) {
        s.name = plan.name;
        nameInput.value = plan.name;
      }
      renderInputs();
      await checkName();
      calibrate().catch(showError);
    }

    async function add(purpose) {
      let got;
      try {
        got = await core.invoke("pick", { purpose });
      } catch (e) {
        showError(e);
        return;
      }
      if (got.length > 0) {
        s.inputs.push(...got);
        await inputsChanged();
      }
    }

    function build() {
      const bar = el("div", { class: "row" });
      const addFiles = el("button", { type: "button", text: t("create.inputs.files") });
      addFiles.addEventListener("click", () => add("files").catch(showError));
      const addFolder = el("button", { type: "button", text: t("create.inputs.folder") });
      addFolder.addEventListener("click", () => add("folder").catch(showError));
      bar.append(addFiles, addFolder);

      nameInput.addEventListener("input", () => {
        s.name = nameInput.value;
        s.nameEdited = true;
        checkName().catch(showError);
      });
      const choose = el("button", { type: "button", text: t("create.folder.choose") });
      choose.addEventListener("click", async () => {
        let g;
        try {
          [g] = await core.invoke("pick", { purpose: "destination" });
        } catch (e) {
          showError(e);
          return;
        }
        if (g) {
          s.folder = g;
          s.folderChosen = true;
          s.recents = (await core.invoke("recents")).destinations;
          renderFolder();
          await checkName();
        }
      });
      recentsSelect.addEventListener("change", async () => {
        const g = s.recents[Number(recentsSelect.value)];
        if (recentsSelect.value !== "" && g) {
          s.folder = g;
          s.folderChosen = true;
          renderFolder();
          await checkName();
        }
      });

      const tiersBox = el("fieldset", { class: "tiers" }, el("legend", { text: t("create.tier.legend") }));
      for (const tier of TIERS) {
        const input = el("input", { type: "radio", name: "create-tier", value: tier, checked: tier === s.tier });
        const estimate = el("span", { class: "estimate", "aria-live": "polite" });
        input.addEventListener("change", () => {
          s.tier = tier;
          // The chosen tier's estimate comes first.
          calibrate().catch(showError);
        });
        tiersBox.append(
          el("label", { class: "card" }, input, el("span", { class: "tier-name", text: tierTexts[tier][0] }), el("span", { class: "tier-about", text: tierTexts[tier][1] }), estimate),
        );
        cards[tier] = { input, estimate };
      }

      passwordSet.addEventListener("click", async () => {
        s.password = await core.invoke("create_password", { set: true, op: nextOp() });
        renderPassword();
      });
      passwordClear.addEventListener("click", async () => {
        s.password = await core.invoke("create_password", { set: false, op: nextOp() });
        renderPassword();
      });
      namesBox.addEventListener("change", () => {
        s.encryptNames = namesBox.checked;
      });
      recovery.addEventListener("input", () => {
        s.recovery = recovery.value === "" ? Number.NaN : Number(recovery.value);
        refreshCreate();
      });
      // The size estimates include the recovery records: a settled change rescales them.
      recovery.addEventListener("change", () => {
        if (validRecovery()) {
          Promise.all(TIERS.map(rescale))
            .then(renderTiers)
            .catch(showError);
        }
      });
      testBox.addEventListener("change", () => {
        s.test = testBox.checked;
      });

      return [
        note,
        el("fieldset", {}, el("legend", { text: t("create.inputs.legend") }), bar, inputsList, totals),
        el(
          "fieldset",
          {},
          el("legend", { text: t("create.dest.legend") }),
          el("label", { for: "create-name", text: t("create.name.label") }),
          el("span", { class: "row" }, nameInput, el("span", { class: "suffix", text: ".lpk" })),
          nameNote,
          el("span", { class: "row" }, folderLine, choose, recentsSelect),
        ),
        tiersBox,
        el("fieldset", {}, el("legend", { text: t("create.password.legend") }), el("span", { class: "row" }, passwordState, passwordSet, passwordClear), namesLabel),
        el(
          "fieldset",
          {},
          el("legend", { text: t("create.options.legend") }),
          el("label", { for: "create-recovery", text: t("create.recovery.label") }),
          recovery,
          el("p", { class: "hint", text: t("create.recovery.hint") }),
          el("label", { class: "choice" }, testBox, el("span", { text: t("create.test.label") })),
        ),
      ];
    }

    const body = build();
    renderInputs();
    renderTiers();
    renderFolder();
    renderPassword();

    function showForm(extra = null) {
      note.textContent = extra ?? "";
      const buttons = phase(t("create.title"), body, [
        [t("create.go"), run, "primary", !valid()],
        [t("create.cancel"), cancelWizard, "cancel"],
      ]);
      createButton = buttons[0];
      refreshCreate();
    }

    async function cancelWizard() {
      generation += 1;
      if (calibrating !== null) {
        core.invoke("cancel", { op: calibrating }).catch(() => {});
      }
      // The password never outlives the wizard.
      await core.invoke("create_password", { set: false, op: nextOp() }).catch(() => {});
      closeDialog();
    }

    async function finish() {
      generation += 1;
      await core.invoke("create_password", { set: false, op: nextOp() }).catch(() => {});
    }

    async function run() {
      if (!valid()) {
        return;
      }
      generation += 1;
      if (calibrating !== null) {
        core.invoke("cancel", { op: calibrating }).catch(() => {});
        calibrating = null;
      }
      const op = nextOp();
      const name = `${s.name.trim().replace(/\.lpk$/i, "")}.lpk`;
      const bar = el("progress", { max: 100, value: 0, "aria-label": t("progress.label") });
      const line = el("p", { role: "status", "aria-live": "polite", text: t("create.running", { name }) });
      const started = Date.now();
      const channel = new core.Channel();
      channel.onmessage = (ev) => {
        const elapsed = Math.max((Date.now() - started) / 1000, 0.001);
        if (ev.stage === "extract") {
          // The "Test when done" stage reads the finished archive back.
          const f = ev.files_total ? Math.min(ev.files_done / ev.files_total, 1) : 0;
          bar.value = Math.round(f * 100);
          line.textContent = t("create.testing", { name });
          return;
        }
        const mbs = (ev.bytes_in / elapsed / MB).toFixed(1);
        const total = s.totals.bytes;
        const f = total > 0 ? Math.min(ev.bytes_in / total, 1) : 0;
        bar.value = Math.round(f * 100);
        const eta = f > 0.02 && f < 1 ? t("create.eta", { time: span((elapsed * (1 - f)) / f) }) : "";
        line.textContent = t("create.progress", { files: count(ev.files_done), bytes: count(ev.bytes_in), rate: mbs, eta });
      };
      const cancel = () => {
        core.invoke("cancel", { op }).catch(showError);
        line.textContent = t("create.cancelling");
      };
      phase(t("create.title"), [el("p", { text: t("create.plan", { name, files: count(s.totals.files), bytes: size(s.totals.bytes) }) }), bar, line], [[t("create.cancel"), cancel, "cancel"]]);
      const request = {
        inputs: s.inputs.map((i) => i.grant),
        folder: s.folder.grant,
        name: s.name,
        tier: s.tier,
        recovery: s.recovery,
        encrypt: s.password,
        encryptNames: s.encryptNames,
        test: s.test,
      };
      try {
        const r = await core.invoke("create_run", { request, op, progress: channel });
        s.password = false;
        await finish();
        summary(r);
      } catch (e) {
        if (e && e.class === "Exists") {
          showForm(t("create.exists", { message: message(e) }));
          checkName().catch(showError);
        } else if (e && e.class === "Cancelled") {
          showForm(t("create.cancelled", { name }));
          calibrate().catch(showError);
        } else {
          showForm(t("create.error", { name, message: message(e) }));
          calibrate().catch(showError);
        }
      }
    }

    function summary(r) {
      const nodes = [];
      const saved = r.bytesIn > 0 ? t("create.summary.ratio", { percent: Math.round((r.bytesOut / r.bytesIn) * 100) }) : "";
      nodes.push(el("p", { text: t("create.summary.done", { name: r.name, entries: count(r.entries), bytesIn: count(r.bytesIn), bytesOut: count(r.bytesOut), saved, time: span(r.seconds) }) }));
      nodes.push(el("p", { text: t("create.summary.path", { path: r.path }) }));
      nodes.push(el("p", { text: t("create.summary.recovery", { percent: r.recovery }) }));
      if (r.encrypted) {
        nodes.push(el("p", { text: r.namesEncrypted ? t("create.summary.encrypted.names") : t("create.summary.encrypted") }));
      }
      const kind = r.test.kind;
      if (kind === "passed") {
        nodes.push(el("p", { class: "check", text: t("create.test.passed", { entries: count(r.test.entries) }) }));
      } else if (kind === "failed") {
        nodes.push(el("p", { class: "check bad", role: "alert", text: t("create.test.failed", { detail: r.test.detail }) }));
      } else if (kind === "cancelled") {
        nodes.push(el("p", { class: "check", text: t("create.test.cancelled") }));
      } else {
        nodes.push(el("p", { class: "check", text: t("create.test.skipped") }));
      }
      const command = el("input", { type: "text", readonly: true, class: "command", id: "create-command", value: r.command, "aria-describedby": "create-command-note" });
      nodes.push(
        el("label", { for: "create-command", text: t("create.summary.command") }),
        command,
        el("p", { id: "create-command-note", class: "hint", text: t("create.summary.command.note") }),
      );
      phase(t("create.title"), nodes, [
        [
          t("create.summary.open"),
          async () => {
            closeDialog();
            await openArchive(r.archive);
          },
          "primary",
        ],
        [t("create.close"), closeDialog, "cancel"],
      ]);
    }

    showForm();
    try {
      s.recents = (await core.invoke("recents")).destinations;
      renderFolder();
    } catch (e) {
      showError(e);
    }
    if (items && items.length > 0) {
      s.inputs.push(...items);
      await inputsChanged();
    } else {
      // From the bar's button: the inputs come from the dialog's own pickers.
      nameInput.focus();
    }
  }

  // One wizard at a time: a verb arriving while one is open waits for it to close.
  function open(items) {
    chain = chain
      .then(
        () =>
          new Promise((resolve, reject) => {
            ensureDialog().addEventListener("close", () => resolve(), { once: true });
            openWizard(items).catch((e) => {
              closeDialog();
              reject(e);
            });
          }),
      )
      .catch(showError);
    return chain;
  }

  // `--create ... --name`: created at once with the wizard's defaults, through the same dialog's
  // progress and summary (no question is asked).
  async function runNamed(a) {
    chain = chain.then(() => named(a)).catch(showError);
    return chain;
  }

  async function named(a) {
    const name = a.name;
    const plan = await core.invoke("create_plan", { inputs: a.items.map((i) => i.grant) });
    const op = nextOp();
    const bar = el("progress", { max: 100, value: 0, "aria-label": t("progress.label") });
    const line = el("p", { role: "status", "aria-live": "polite", text: t("create.running", { name }) });
    const started = Date.now();
    const channel = new core.Channel();
    channel.onmessage = (ev) => {
      const elapsed = Math.max((Date.now() - started) / 1000, 0.001);
      if (ev.stage === "extract") {
        line.textContent = t("create.testing", { name });
        return;
      }
      const f = plan.bytes > 0 ? Math.min(ev.bytes_in / plan.bytes, 1) : 0;
      bar.value = Math.round(f * 100);
      line.textContent = t("create.progress", { files: count(ev.files_done), bytes: count(ev.bytes_in), rate: (ev.bytes_in / elapsed / MB).toFixed(1), eta: "" });
    };
    phase(t("create.title"), [bar, line], [
      [
        t("create.cancel"),
        () => {
          core.invoke("cancel", { op }).catch(showError);
          line.textContent = t("create.cancelling");
        },
        "cancel",
      ],
    ]);
    try {
      const request = {
        inputs: a.items.map((i) => i.grant),
        folder: a.folder.grant,
        name,
        tier: plan.tier,
        recovery: plan.recovery,
        encrypt: false,
        encryptNames: true,
        test: plan.test,
      };
      const r = await core.invoke("create_run", { request, op, progress: channel });
      const nodes = [el("p", { text: t("create.summary.done", { name: r.name, entries: count(r.entries), bytesIn: count(r.bytesIn), bytesOut: count(r.bytesOut), saved: r.bytesIn > 0 ? t("create.summary.ratio", { percent: Math.round((r.bytesOut / r.bytesIn) * 100) }) : "", time: span(r.seconds) }) })];
      nodes.push(el("p", { text: t("create.summary.path", { path: r.path }) }));
      nodes.push(el("p", { text: t("create.summary.recovery", { percent: r.recovery }) }));
      const k = r.test.kind;
      nodes.push(
        el("p", {
          class: k === "failed" ? "check bad" : "check",
          role: k === "failed" ? "alert" : null,
          text: k === "passed" ? t("create.test.passed", { entries: count(r.test.entries) }) : k === "failed" ? t("create.test.failed", { detail: r.test.detail }) : t("create.test.skipped"),
        }),
      );
      const command = el("input", { type: "text", readonly: true, class: "command", id: "create-command", value: r.command });
      nodes.push(el("label", { for: "create-command", text: t("create.summary.command") }), command, el("p", { class: "hint", text: t("create.summary.command.note") }));
      await new Promise((resolve) => {
        phase(t("create.title"), nodes, [
          [
            t("create.summary.open"),
            async () => {
              closeDialog();
              await openArchive(r.archive);
              resolve();
            },
            "primary",
          ],
          [
            t("create.close"),
            () => {
              closeDialog();
              resolve();
            },
            "cancel",
          ],
        ]);
      });
    } catch (e) {
      const text = e && e.class === "Cancelled" ? t("create.cancelled", { name }) : t("create.error", { name, message: message(e) });
      await new Promise((resolve) => {
        phase(t("create.title"), [el("p", { role: "alert", text })], [
          [
            t("create.close"),
            () => {
              closeDialog();
              resolve();
            },
            "primary cancel",
          ],
        ]);
      });
    }
  }

  return { open, runNamed };
}
