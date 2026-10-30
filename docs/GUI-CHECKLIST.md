# GUI checklist (E5-10)

E5-10 runs this list on each supported configuration (Windows 10 22H2 x64, Windows 11 x64,
Windows 11 ARM64) and commits one filled-in copy per configuration: OS build, date, and for each
item pass or fail with the reason. Each task from E5-4 on appends its dialogs under its own
heading; the keyboard and accessibility items of E5-9 apply to every dialog.

Configuration: OS build ______  Date ______  Tester ______

## E5-4 Extract wizard

Open an archive (File: Open, a drop, or `lpk-gui.exe <archive>`), press `Extract...` or Ctrl+E.
Every dialog must work by keyboard alone: Esc cancels, Enter takes the primary button, and focus
returns to where it was when the dialog closes.

| # | Item | Expected result | Result |
|---|---|---|---|
| 4.1 | Destination Smart (default) with a folder holding several top-level items | A new `<archive name>` folder beside the archive; nothing existing touched; no question asked | |
| 4.2 | Smart with an archive holding one top-level item whose name is free | That item lands directly beside the archive | |
| 4.3 | Destination "A new folder beside the archive", run twice | `<name>`, then `<name> (2)`; nothing replaced | |
| 4.4 | Destination "This folder" with files that already exist | The conflict question appears once (4.8) | |
| 4.5 | Destination Choose... | The native folder picker opens; after choosing, the folder is listed and selected; cancelling the picker changes nothing | |
| 4.6 | Recent destinations | A folder chosen in 4.5 is listed next time as "Recent: name"; one never chosen in the picker is not | |
| 4.7 | Everything versus Selected (N) | Selected is the default only when rows are selected; with a filter hiding some picks, the dialog (plan and result) says how many are left out; only the visible picks are extracted | |
| 4.8 | Conflict question: Skip, Keep both, Replace, Cancel | Skip leaves existing files; Keep both writes `name (1).ext`; Replace overwrites; Cancel writes nothing; the result counts skipped and renamed | |
| 4.9 | Password dialog, light theme (an archive with encrypted names, then one with encrypted content) | Native dialog owned by the window, themed light, over the main window; OK continues | |
| 4.10 | Password dialog, dark theme | Same dialog in dark colours (title bar and controls) | |
| 4.11 | Wrong password, then the right one | The dialog asks again saying the password was not right; the right one extracts; a second extraction of the same archive does not ask again | |
| 4.12 | Password dialog cancelled | The operation ends as cancelled; nothing is written | |
| 4.13 | Envelope refusal (an archive whose memory need exceeds this PC's allowance) | Reads "needs X; this PC allows Y" with `Allow X`; Allow re-runs that one extraction; the next extraction refuses again (nothing persisted) | |
| 4.14 | Not enough free space (a destination volume too small) | Refused before anything is written, naming both figures | |
| 4.15 | Progress and Cancel on a large archive | Files, bytes, MB/s and an ETA update; Cancel (and Esc) stops it; the result says any files already extracted were kept in the folder (the unfinished one is removed, D-87 (2)), also when none had finished yet; cancelling a password prompt, the conflict question or the run before it started says it was cancelled before anything was written | |
| 4.16 | Result with entry errors (a damaged archive) | Each failed entry is listed with its reason | |
| 4.17 | Result with symlinks (an archive with links, run without the symlink privilege) | Skipped and unprivileged links are listed | |
| 4.18 | Result check line | `.lpk`: "every file matched its BLAKE3 hash" (only when a file was written); ZIP and 7z: "each member matched its stored checksum", or "N members matched their stored checksum; K had none to check" when some member stored none; tar: "not verifiable"; no check line after a cancel; the Mark of the Web warning appears only when a file could not be marked | |
| 4.19 | Several archives queued (`lpk-gui.exe --extract-here a.lpk b.lpk c.lpk`) | One window, one extraction at a time, "Archive k of 3" while running, one result each; a password prompt per sealed archive | |
| 4.20 | `lpk-gui.exe --extract-here <archive>` from a command prompt | Smart beside the archive, no destination question | |
| 4.21 | `lpk-gui.exe --extract-to <archive>` from a command prompt, twice | `<name>\` beside the archive, then `<name> (2)\` | |
| 4.22 | A second `--extract-here` while the first window is working | Forwarded to the running window and queued behind the first | |
| 4.23 | Closing the window mid-extraction | LitePack asks; on yes the run is cancelled and cleaned up before the process ends | |
| 4.24 | Recent archives | The empty window lists archives opened through the Open dialog, a verb or a drop (newest first, at most 10); one that was deleted is gone from the list | |
| 4.25 | Clear recent lists | One button empties both the archives and the destinations; they stay empty after a restart | |

## E5-7 Verbs, single instance, settings

| # | Item | Expected result | Result |
|---|---|---|---|
| 7.1 | A second launch with a verb (`lpk-gui.exe --extract-here x.lpk` while a window is open) | The verb reaches the first window, which comes to the front; no second window | |
| 7.2 | The "Check for updates" switch | Off stays off after a restart; on stays on | |
| 7.3 | A portable copy (an empty `portable` file beside the executable) | Settings, recent lists and WebView data are kept in `data` beside the executable, nothing under the user profile | |

## E5-6 Preview pane

Open an archive; the pane is on the right of the list, shown at every start. Focus a file row (arrow keys or a click) and wait a moment: the pane previews it.

| # | Item | Expected result | Result |
|---|---|---|---|
| 6.1 | A text entry (`.txt`, `.json`, and an `.html` or `.svg` file) | Its text in a monospace block; an `.html` or `.svg` file shows its source, never a page or a picture; nothing runs | |
| 6.2 | An image entry (PNG, JPEG, GIF, WebP, BMP, ICO) | The picture, scaled to fit the pane; the same file renamed `.txt` still shows as an image (the content decides, not the name) | |
| 6.3 | An image in both themes (Windows light and dark) | A picture with transparency shows on a neutral checkered background in both, and the pane's text stays legible | |
| 6.4 | A binary entry (an `.exe`, a `.zip`, a file with NUL bytes) | A hex view: offset, 16 bytes in hex and their ASCII on each line; scrolling a 16 MiB file stays smooth | |
| 6.5 | An entry over 16 MiB | The pane says it cannot preview and names the 16 MiB cap; nothing else changes | |
| 6.6 | An entry of exactly 16 MiB | It is previewed (text or hex) | |
| 6.7 | A folder row | Its name and how many items it holds | |
| 6.8 | A file with bidirectional or hidden characters in its text (U+202E, U+200B, a BOM inside) and one in its name | In the text and in the pane's title they show as visible escapes (U+202E as `\u{202E}`), not as reordered or invisible characters | |
| 6.9 | An encrypted entry (archive made with `--password`, listable names) | Resting on it shows "Encrypted" and an Unlock button, with no prompt; Unlock asks once; a wrong password asks again; Cancel at the prompt returns the pane to "Encrypted" with its Unlock button (never "Reading..."); a double-click with the pane hidden shows the pane and asks; after the right one, other entries preview with no question | |
| 6.10 | Moving focus quickly down a long list | The pane follows the last row only; no flicker of the ones passed | |
| 6.11 | Alt+P and the Preview button in the bar | Each hides and shows the pane; hidden, nothing is read; shown again it previews the focused row; on again after a restart | |
| 6.12 | Double-click a file whose type is not opened directly (a `.cmd`, `.html`) | Nothing is extracted or launched; the status line says it is shown in the preview pane, and the pane shows it | |
| 6.13 | Tab through the window | Focus reaches the text or hex block and leaves it again with Tab; no keyboard trap | |
| 6.14 | A ZIP or 7z archive | Its entries preview like any other (or the pane gives the engine's message where reading them is not supported yet) | |

## E5-5 and E5-8

Append the create wizard (E5-5), the verbs and shell integration (E5-7) and
the maintenance dialogs (E5-8) here.

## Notes

- Envelope refusal of a foreign archive (E4 follow-up): the GUI offers no "Allow X" for a ZIP or 7z memory limit yet, because that limit comes from the session's open options, not from the extraction's; such an error shows its message only.
