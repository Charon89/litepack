// The About box (docs/notes/e7-design.md section 5): opened from the settings dialog's "About
// LitePack" button. It shows the version, the licence, the portable or installed state and its
// folders, links to the project's pages and the sandbox profile's removal. The links go to Rust by
// name (`about_open`, a closed set): the page never holds an address it could open. Tab stays
// inside, Esc or Close shuts it, and the focus goes back to the gear (the settings dialog's
// opener). Text goes in through textContent only.
import { trapTab } from "./a11y.js";

/**
 * Wire the About box. `core` is Tauri's core API, `t` the string lookup, `opener` the control
 * the focus returns to, `settings` the settings dialog the box is opened from.
 */
export function wireAbout(core, t, opener, settings) {
  const $ = (id) => document.getElementById(id);
  const dialog = $("about");
  const result = $("about-result");
  trapTab(dialog);

  // Each link's words, by the name Rust gives it.
  const linkText = {
    docs: () => t("about.link.docs"),
    privacy: () => t("about.link.privacy"),
    security: () => t("about.link.security"),
    notices: () => t("about.link.notices"),
    repository: () => t("about.link.repository"),
  };

  async function fill() {
    const a = await core.invoke("about");
    $("about-version").textContent = t("about.version", { version: a.version });
    $("about-licence").textContent = t("about.licence", { licence: a.licence });
    $("about-state").textContent = a.portable ? t("about.portable") : t("about.installed");
    $("about-settings").textContent = a.settings ? t("about.settings", { path: a.settings }) : "";
    $("about-settings").hidden = !a.settings;
    $("about-install").textContent = a.install ? t("about.install", { path: a.install }) : "";
    $("about-install").hidden = !a.install;
    const list = $("about-links");
    list.replaceChildren();
    for (const l of a.links) {
      const li = document.createElement("li");
      const b = document.createElement("button");
      b.type = "button";
      b.className = "link";
      b.dataset.link = l.link;
      b.textContent = (linkText[l.link] ?? (() => l.link))();
      b.title = l.url;
      b.addEventListener("click", () => {
        core.invoke("about_open", { link: l.link }).catch((e) => {
          result.textContent = t("about.link.failed", { cause: e?.detail ?? e?.message ?? String(e) });
        });
      });
      li.append(b);
      list.append(li);
    }
  }

  function outcomeText(r) {
    switch (r.outcome) {
      case "removed":
        return t("about.profile.removed");
      case "absent":
        return t("about.profile.absent");
      case "busy":
        return t("about.profile.busy");
      default:
        return t("about.profile.failed", { reason: r.reason ?? "" });
    }
  }

  $("about-remove").addEventListener("click", async () => {
    const button = $("about-remove");
    button.disabled = true;
    result.textContent = "";
    try {
      result.textContent = outcomeText(await core.invoke("remove_profile"));
    } catch (e) {
      result.textContent = t("about.profile.failed", { reason: e?.detail ?? e?.message ?? String(e) });
    } finally {
      button.disabled = false;
      button.focus();
    }
  });

  // The box opens even when its facts cannot be read, and says so.
  async function open() {
    result.textContent = "";
    try {
      await fill();
    } catch (e) {
      result.textContent = t("about.failed", { cause: e?.detail ?? e?.message ?? String(e) });
    }
    dialog.showModal();
  }

  // From the settings dialog: it closes first (its own handler moves the focus to the gear),
  // then the box opens over the page.
  $("about-open").addEventListener("click", () => {
    settings.addEventListener("close", () => open(), { once: true });
    settings.close();
  });
  $("about-close").addEventListener("click", () => dialog.close());
  dialog.addEventListener("close", () => {
    result.textContent = "";
    opener.focus();
  });
}
