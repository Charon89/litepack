# Privacy

LitePack collects nothing. It has no telemetry, no analytics, no crash reports and no accounts. This page says what
the code does today; it describes nothing that is planned.

## Your files

Files stay on your computer. Archives are read and written locally, and nothing is uploaded.

The GUI keeps some things on the computer:

- your settings file;
- the lists of recent archives and destinations, which hold their paths (the button **Clear recent lists** empties
  them);
- the journal of interrupted writes;
- the data folder of WebView2 (see below);
- for an entry you open from an archive, a copy in a folder of its own, which the program tries to remove when it
  exits (it cannot while another program still holds the copy open) and, after an earlier run that did not exit
  cleanly, removes when it next starts;
- short-lived files that Explorer's menu writes for the GUI to read, which hold the paths you selected; the GUI
  deletes each one after use, and deletes any left over after 24 hours.

All of it stays on the computer and is sent nowhere.

## Network requests

- The command line (`lpk`), the shell extension and the sandboxed worker make no network requests. They contain no
  network code. On Windows the worker also runs in an AppContainer that is granted no capability, the network one included (the
  [security model](security.md) says what that does and does not prove).
- The graphical program (`lpk-gui`) has one network request: the update check.

### The update check

- It is an HTTPS GET of one file, `https://github.com/Charon89/litepack/releases/latest/download/latest.json`, which
  is the address set in the GUI's configuration. The program makes it in the background when it starts, and it
  gives up after 30 seconds.
- It sends nothing beyond what any HTTPS request carries. GitHub, which serves the file, sees your IP address and
  the request headers, such as `Host`, `Accept` and the User-Agent `tauri-plugin-updater/` with that library's version. GitHub redirects the request to a second GitHub host, so GitHub's servers answer it. The address carries no LitePack version, no system and no identifier.
- It is on by default. It can be turned off: in the GUI's **Settings**, clear the box **Check for updates**. The setting is saved on your computer and read when the program starts, so clearing it takes effect from the next start; with it cleared, the program makes no request at all.
- The check also makes no request unless the program holds the public key that verifies releases. No release exists
  yet, so no build holds one, and no build makes the request today.
- A failed check is written to the program's error output only. It is sent nowhere.

## The window

The GUI shows its interface through Microsoft Edge WebView2, a Windows component. It falls under Microsoft's own
privacy statement, and this page makes no claim about what WebView2 does on its own. The interface itself loads
only the program's own files: its content security policy allows no other source and no connection to any web
address.

## This website

The website is hosted on GitHub Pages, and GitHub's privacy statement covers that hosting. The site sets no
cookies and loads no script, style or font from a third party. Its search runs in your browser. Its page template
(mdBook) keeps your choice of theme and the sidebar's state in the browser's local storage, and the sidebar's scroll
position in its session storage, both on your computer; the site sends neither anywhere.

## Questions

For a privacy question that is not a security report, open an issue in the
[repository](https://github.com/Charon89/litepack/issues). Report a vulnerability privately, as the
[security policy](https://github.com/Charon89/litepack/security/policy) describes.

{{#include footer.md}}
