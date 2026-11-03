// Keyboard helpers shared by the page and its dialogs (E5-9; docs/notes/e5-design.md section 5):
// F6 between the panes, Tab wrapping inside a modal dialog, and a roving tab stop for a radio
// group made of buttons. No strings here: the page's text lives in strings/en.json.

const FOCUSABLE = 'a[href], button, input, select, textarea, [tabindex]';

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
