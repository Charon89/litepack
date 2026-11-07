// Keyboard helpers shared by the page and its dialogs (E5-9; docs/notes/e5-design.md section 5):
// F6 between the panes, Tab wrapping inside a modal dialog, and a roving tab stop for a radio
// group made of buttons. No strings here: the page's text lives in strings/en.json.

const FOCUSABLE = 'a[href], button, input, select, textarea, summary, [tabindex]';

function shown(e) {
  return !e.hidden && e.getClientRects().length > 0 && getComputedStyle(e).visibility !== "hidden";
}

/** The elements Tab visits inside `root`, in order (a radio group counts once). */
export function tabStops(root) {
  const stops = [];
  for (const e of root.querySelectorAll(FOCUSABLE)) {
    if (e.disabled || e.tabIndex < 0 || !shown(e)) {
      continue;
    }
    if (e.tagName === "INPUT" && e.type === "radio" && e.name) {
      const group = root.querySelectorAll(`input[type=radio][name="${CSS.escape(e.name)}"]`);
      const checked = [...group].find((r) => r.checked);
      if (checked ? checked !== e : group[0] !== e) {
        continue;
      }
    }
    stops.push(e);
  }
  return stops;
}

/** Keep Tab and Shift+Tab inside the modal `dialog`: from the last stop to the first and back. */
export function trapTab(dialog) {
  dialog.addEventListener("keydown", (e) => {
    if (e.key !== "Tab" || e.ctrlKey || e.altKey || e.metaKey) {
      return;
    }
    const stops = tabStops(dialog);
    if (stops.length === 0) {
      e.preventDefault();
      return;
    }
    const at = stops.indexOf(document.activeElement);
    const last = stops.length - 1;
    if (at === -1 || (e.shiftKey && at === 0) || (!e.shiftKey && at === last)) {
      e.preventDefault();
      stops[e.shiftKey ? last : 0].focus();
    }
  });
}

/**
 * F6 and Shift+F6 cycle the panes: each element with `data-pane` that is shown, in document
 * order; `data-pane-focus` (a selector inside it) says what takes the focus, else the pane itself.
 */
export function wirePanes() {
  document.addEventListener("keydown", (e) => {
    if (e.key !== "F6" || e.ctrlKey || e.altKey || e.metaKey || document.querySelector("dialog[open]")) {
      return;
    }
    const panes = [...document.querySelectorAll("[data-pane]")].filter(shown);
    if (panes.length === 0) {
      return;
    }
    e.preventDefault();
    const at = panes.findIndex((p) => p.contains(document.activeElement));
    const next = at === -1 ? (e.shiftKey ? panes.length - 1 : 0) : (at + (e.shiftKey ? -1 : 1) + panes.length) % panes.length;
    const pane = panes[next];
    const selector = pane.dataset.paneFocus;
    const target = (selector && [...pane.querySelectorAll(selector)].find((x) => !x.disabled && shown(x))) || pane;
    target.focus();
  });
}

/** The radio buttons of `group` (role=radio): one tab stop, the checked one. */
export function syncRadios(group) {
  const radios = [...group.querySelectorAll("[role=radio]")];
  const checked = radios.find((r) => r.getAttribute("aria-checked") === "true") ?? radios[0];
  for (const r of radios) {
    r.tabIndex = r === checked ? 0 : -1;
  }
}

/** Arrow keys move inside the group, checking the one reached (a click). */
export function wireRadios(group) {
  group.addEventListener("keydown", (e) => {
    const radios = [...group.querySelectorAll("[role=radio]")];
    const at = radios.indexOf(document.activeElement);
    if (at === -1) {
      return;
    }
    let to = at;
    if (e.key === "ArrowRight" || e.key === "ArrowDown") {
      to = (at + 1) % radios.length;
    } else if (e.key === "ArrowLeft" || e.key === "ArrowUp") {
      to = (at - 1 + radios.length) % radios.length;
    } else {
      return;
    }
    e.preventDefault();
    radios[to].click();
    radios[to].focus();
  });
}

/** `Control+N` as the tooltip shows it. */
export function keyLabel(shortcuts) {
  return shortcuts.replaceAll("Control", "Ctrl");
}

/** Show a phase: every child of the dialog but the live region is replaced by `nodes`, which
 *  go before it, so the live region is never detached (a region that leaves the tree and comes
 *  back is a new one and announces nothing). */
export function setPhase(dialog, live, ...nodes) {
  for (const child of [...dialog.children]) {
    if (child !== live) {
      child.remove();
    }
  }
  for (const node of nodes) {
    dialog.insertBefore(node, live);
  }
}

/** Say the first line of the dialog's body through its live region: cleared now, set in a later
 *  task (an update to a region already in the tree is what is announced). A body that starts
 *  with an alert says itself, so it is not said twice. */
export function announce(dialog, live) {
  const body = dialog.querySelector(".wizard-body");
  const first = body ? body.firstElementChild : null;
  live.textContent = "";
  if (!first || first.getAttribute("role") === "alert") {
    return;
  }
  const text = first.tagName === "P" ? first.textContent : (body.querySelector("p")?.textContent ?? "");
  setTimeout(() => {
    live.textContent = text;
  }, 80);
}

/** The letter a key stands for: the character when it is a Latin letter (Dvorak, Colemak), else
 *  the physical key's (a Cyrillic layout), upper case; otherwise the key itself. */
export function keyLetter(e) {
  if (/^[a-zA-Z]$/.test(e.key)) {
    return e.key.toUpperCase();
  }
  if (/^Key[A-Z]$/.test(e.code)) {
    return e.code.slice(3);
  }
  return e.key.toUpperCase();
}
