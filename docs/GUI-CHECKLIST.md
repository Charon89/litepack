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
| 4.15 | Progress and Cancel on a large archive | Files, bytes, MB/s and an ETA update; Cancel (and Esc) stops it; the result says nothing was left behind and the folder it created is gone | |
| 4.16 | Result with entry errors (a damaged archive) | Each failed entry is listed with its reason | |
| 4.17 | Result with symlinks (an archive with links, run without the symlink privilege) | Skipped and unprivileged links are listed | |
| 4.18 | Result check line | `.lpk`: "every file matched its BLAKE3 hash"; ZIP and 7z: "each member matched its stored checksum"; tar: "not verifiable"; the Mark of the Web warning appears only when a file could not be marked | |
| 4.19 | Several archives queued (`lpk-gui.exe --extract-here a.lpk b.lpk c.lpk`) | One window, one extraction at a time, "Archive k of 3" while running, one result each; a password prompt per sealed archive | |
| 4.20 | `lpk-gui.exe --extract-here <archive>` from a command prompt | Smart beside the archive, no destination question | |
| 4.21 | `lpk-gui.exe --extract-to <archive>` from a command prompt, twice | `<name>\` beside the archive, then `<name> (2)\` | |
| 4.22 | A second `--extract-here` while the first window is working | Forwarded to the running window and queued behind the first | |
| 4.23 | Closing the window mid-extraction | LitePack asks; on yes the run is cancelled and cleaned up before the process ends | |
| 4.24 | Recent archives | The empty window lists archives opened through the Open dialog, a verb or a drop (newest first, at most 10); one that was deleted is gone from the list | |
| 4.25 | Clear recent lists | One button empties both the archives and the destinations; they stay empty after a restart | |

## E5-5 to E5-8

Append the create wizard (E5-5), the preview (E5-6), the verbs and shell integration (E5-7) and
the maintenance dialogs (E5-8) here.
