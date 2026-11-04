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

## E6-4 "Open with" setting and Default apps button

Both controls sit in the command bar, left of "Check for updates", and exist only in the copy the installer registered
(the registration record names this exe). Do these on a machine you may change, with the installed copy; never on a
development build, which must show neither control.

| # | Item | Expected result | Result |
|---|---|---|---|
| 10.1 | A portable copy, a development build and a copy that is not the registered one | Neither the "Offer LitePack in "Open with"..." box nor "Default apps..." is on the page (absent, not greyed) | |
| 10.2 | The installed copy, installed without the opt-in | Both show; the box is unticked and names the formats (for example ".zip, .7z, ..."); Tab reaches the box then the button with a visible ring | |
| 10.3 | Tick the box | A moment later it stays ticked; right-click a `.zip` > Open with lists LitePack (as an option, never the default); a `.zip` default someone else chose (for example 7-Zip's) is untouched | |
| 10.4 | Untick the box | The Open with entry for `.zip` is gone again; `.lpk` still opens in LitePack | |
| 10.5 | Tick it, close and reopen the window | The box is still ticked (the page reads the record) | |
| 10.6 | "Default apps..." on Windows 11 | Settings opens on LitePack's own default-apps page (the `registeredAppUser=LitePack` address) | |
| 10.7 | "Default apps..." on Windows 10 22H2 | Settings opens; it may ignore the parameter and show the general Default apps page. Record which in the Result column (E6-7) | |
| 10.8 | Windows high contrast, Narrator | The box and button are named, the ring and the box state are visible; a failure (in regedit, deny your user "Set Value" on `HKCU\Software\Classes\.zip\OpenWithProgids`, then tick the box) shows the alert "LitePack's Explorer integration may be incomplete ... Reinstall LitePack, or run lpk-gui.exe --register-shell, to repair it." under the command bar, it stays even if the controls vanish, and Narrator reads it; restore the permission and repair afterwards | |

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

## E5-5 Create wizard

Press `New archive...` or Ctrl+N, or start `lpk-gui.exe --create <paths>`. Esc cancels, Enter creates when
the form is valid, focus returns to where it was when the dialog closes.

| # | Item | Expected result | Result |
|---|---|---|---|
| 5.1 | Open the wizard from the bar's button and from Ctrl+N | One dialog, empty inputs, Create disabled | |
| 5.2 | Open it with `--create <folder> <file>` | Both inputs listed, totals shown, folder and name proposed from the first input | |
| 5.3 | Add files, add a folder, Remove one | The list and the totals follow; estimates start again | |
| 5.4 | Type an empty name, `a/b`, `con`, then a free name | Each bad name is refused in the dialog and Create is disabled; the free name enables it | |
| 5.5 | Choose... a folder; pick a recent folder | The folder line changes; a chosen folder joins the recent list after a create | |
| 5.6 | Each tier card (Fast, Balanced, Store) | Balanced is selected; each card says "estimating..." and then an estimate ("about N s, about N MB") | |
| 5.7 | Set password...: matching entries, then a mismatch | The native dialog asks twice; a mismatch is refused there; the page says only that a password is set | |
| 5.8 | With a password: "Encrypt file names" on (default) and off | On: the archive does not list without the password; off: it lists | |
| 5.9 | Recovery 5 (proposed) and 0; 21 | 5 and 0 create; 21 disables Create | |
| 5.10 | "Test when done" on and off | On: the summary says it passed (or names the failure); off: "Not tested." | |
| 5.11 | Create a large input and press Cancel | The run stops; neither the archive nor a `.partial` file remains | |
| 5.12 | The summary and Open | Counts, ratio, time, recovery, encryption and path are shown; Open shows the archive in the browser | |
| 5.13 | Copy as command | A read-only field holds an `lpk a` line with the tier, recovery and inputs, and the note that the password is not included | |
| 5.14 | Create over an existing `<name>.lpk` | The dialog names it and Create stays disabled; nothing is overwritten | |
| 5.15 | `--create <paths> --name x.lpk` | Created at once with the wizard's defaults (Balanced, recovery 5, test when done), through the progress dialog and summary | |

## E5-8 Maintenance: test, check and repair, history, update

The command bar's Maintenance group (Test, Check and repair..., History..., Update from folder...) works on
the open archive; `lpk-gui.exe --test <archives>` runs Test for each. Esc cancels, Enter takes the primary
button, focus returns to the button that opened the dialog. Use a copy of an archive made with recovery
records (`lpk a --recovery 20`) and a copy with a byte flipped in the middle of its data.

| # | Item | Expected result | Result |
|---|---|---|---|
| 8.1 | Open an archive; look at the Maintenance group before and after | All four are disabled with no archive (the tooltip says why); with an `.lpk` all four are enabled | |
| 8.2 | Open a ZIP or 7z | Test is enabled; Check, History and Update are disabled and the tooltip says only `.lpk` has them | |
| 8.3 | Test an undamaged archive | Progress with Cancel, then "passed" with entries, chunks and blocks and the line "Every chunk was decoded and matched its hash" | |
| 8.4 | Test with Cancel pressed mid-run (a large archive) | "The test was cancelled"; nothing else changed; the window stays usable | |
| 8.5 | Test a damaged archive | "The test found a problem", the message names the block and the entry; "Check and repair..." is offered | |
| 8.6 | Test an encrypted archive with names in the clear (keyless) | "passed" with the line that only frame hashes and recovery records were checked; "Unlock..." is offered and there was no password prompt by itself | |
| 8.7 | Press Unlock, type a wrong password, then the right one | The native prompt asks again after the wrong one; the result then says every chunk was decoded; a second Test needs no password | |
| 8.8 | Press Unlock and Cancel the prompt | "The test was cancelled"; the archive is still open | |
| 8.9 | Check an archive without recovery records | "has no recovery records, so it cannot be repaired"; no repair button | |
| 8.10 | Check an undamaged archive with recovery records | Frame counts, "No damage found" | |
| 8.11 | Check the damaged copy | Counts show damaged shards; "Repair to a copy..." is offered | |
| 8.12 | Repair to a copy beside the archive | `<name> (repaired).lpk` appears beside it; the result says the original was not changed and the copy passed its test; Open the copy opens it | |
| 8.13 | Repair again, and again | `<name> (repaired 2).lpk`, then `(repaired 3)`; nothing is overwritten | |
| 8.14 | Repair, press Choose folder... and pick another folder | The copy goes into that folder | |
| 8.15 | Cancel a repair mid-run | "Cancelled. No copy was kept"; no `(repaired)` file is left | |
| 8.16 | History of an archive with several generations | Generations newest first with sizes, the latest and the one shown marked | |
| 8.17 | Select an earlier generation, press Browse | The window shows that generation with the banner "Generation N of M, shown read-only"; Update from folder is disabled (tooltip says why); Extract works and restores its files | |
| 8.18 | In the view, press "Back to the latest" | The latest generation opens and the banner goes | |
| 8.19 | Select an earlier generation, press Roll back | A native box names the generations removed and says it cannot be undone; No leaves the archive and returns to History | |
| 8.20 | Roll back and answer Yes | The archive ends at that generation (History lists no later one, the file is shorter); it reopens; the result states the generations removed | |
| 8.21 | Start a rollback while another operation runs | It waits for it, then asks | |
| 8.22 | Update from folder: choose a folder in which one earlier file is deleted and one is new | The dialog says the generation mirrors the folder; the result names the new generation and the test passes; the deleted path is gone from the listing, the new one is there; History shows the old generation still holds the deleted file | |
| 8.23 | Update from folder on an encrypted archive | The password is asked for before anything is written; Cancel at the prompt leaves the archive as it was; the right password appends and the archive reopens without a second prompt | |
| 8.24 | Cancel an update mid-run | "The archive is as it was"; the file is byte-identical to before; the archive reopens | |
| 8.25 | Update with Test when done off, with the Store tier and with recovery 0 | The run uses them (no test line; a larger archive without recovery records) | |
| 8.26 | `lpk-gui.exe --test a.lpk b.lpk c.lpk` | "Archive 1 of 3", "2 of 3", "3 of 3", one result each, one at a time; Close ends | |
| 8.27 | `--test` on a mix that includes a damaged archive and a sealed one | The damaged one fails with its message, the sealed one offers Unlock beside its result and Unlock re-runs only that archive | |
| 8.28 | Cancel the `--test` queue | The running archive ends cancelled and the rest do not run | |

## E5-9 Keyboard, names, high contrast

The automated pass (axe-core over every view and dialog the WebDriver reaches, and the keyboard test) is in CI's
`gui-e2e` job; these items are the manual ones, with Narrator, and the states the checker cannot reach.

| # | Step | Expect | Result |
|---|---|---|---|
| 9.1 | Empty window: Tab through it, then F6 and Shift+F6 | Every control takes focus once in a sensible order, with a visible ring; F6 goes command bar, recent archives and back | |
| 9.2 | Open an archive: F6 and Shift+F6 | The focus visits the command bar, the path bar and filter, the list and the preview pane (when shown) in that order and wraps | |
| 9.3 | In the list: arrows, Home, End, PageUp, PageDown, Shift+arrows, Ctrl+arrows with Space, Ctrl+A, Esc, Enter on a folder, Backspace and Alt+Up | Each does what a mouse does; the list is one Tab stop; the focused row has a visible ring | |
| 9.4 | Ctrl+O, Ctrl+N, Ctrl+E, Alt+P, Ctrl+F, Ctrl+Shift+T, Ctrl+Shift+K, Ctrl+Shift+H, Ctrl+Shift+U | Open, New, Extract, the preview toggle, the filter, Test, Check, History and Update; each button's tooltip names its key | |
| 9.5 | "Folders" and "All files": Tab to the group, then the arrow keys | One Tab stop for the group; the arrows switch the view | |
| 9.6 | Each dialog (extract, create, test, check, history, update): Tab and Shift+Tab around | The focus stays inside and wraps; Esc closes or cancels; Enter takes the primary action when allowed; the focus returns to the button that opened it | |
| 9.7 | Narrator on the window and on each dialog | The list is read as a table with its row and column counts, selection and level; every control has a name; each dialog is announced with its title and text | |
| 9.8 | Narrator: open an archive, extract, create, run Test | The status line, a result and an error are read once, not on every focus move | |
| 9.9 | The states the checker cannot reach: the password prompt, the rollback confirmation, the envelope refusal, a running progress view (extract, create, test) | Narrator reads each prompt and its buttons; a progress bar is named and its value changes are read | |
| 9.10 | Windows high contrast (Aquatic, Desert, Dusk, Night sky) with every view and dialog | Text, borders, the focus ring, the selected row, the checked "Folders"/"All files" button, disabled buttons and the dialogs' edges are all visible | |
| 9.11 | Light and dark themes, 100 % and 200 % scaling | Text is readable and the focus ring visible in each | |

## Notes

- Envelope refusal of a foreign archive (E4 follow-up): the GUI offers no "Allow X" for a ZIP or 7z memory limit yet, because that limit comes from the session's open options, not from the extraction's; such an error shows its message only.
