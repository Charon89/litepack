"""E7-6's residue check (R291; docs/notes/e7-design.md sections 3 and 5; E7-5 extends it).

    python -I scripts/e7-residue.py snapshot <out.json> [--on-ci]
    python -I scripts/e7-residue.py diff <before.json> <after.json>
        [--allow-worker-profile | --require-worker-profile] [--allow-settings] [--verdict all|litepack]
    python -I scripts/e7-residue.py --self-test

--verdict litepack (R408; release.yml's verify-arm64 alone): an entry not allowed is a verdict only when it is
LitePack's own (`litepack_own`: `litepack`, `lpk`, LitePack's CLSID or its jump list's a233dcd0c3d36a7d in its path,
value name or data; in or naming the run folder; under Run, RunOnce, HKCU\\Environment, RegisteredApplications,
Classes\\.lpk or Uninstall; the worker profile's SID). Any other is printed as `OTHER:` and counted, not a verdict.

`snapshot` refuses unless GITHUB_ACTIONS=true or --on-ci is given: use it only on a disposable
machine. Every path it cannot read is printed and recorded; `diff` reports such a path as
residue (`?`) even when both snapshots record it alike. --require-worker-profile allows the
profile and fails unless both its folder and its Mappings key were added. --allow-settings
(E7-5, after an uninstall without the "delete the application data" box, as
docs/notes/e7-design.md section 3 allows) allows %APPDATA%\\LitePack\\settings.json and its folder
when that folder holds nothing else, and nothing more.

`snapshot` records every file and folder under %APPDATA% (the Start menu included),
%LOCALAPPDATA% and the desktop, %USERPROFILE%\\Desktop (E7-5: shortcuts; recorded as `%DESKTOP%`) (path, size and
modification time; a folder by its path alone, a link or junction by its path, never followed)
and the keys and values of HKCU\\Software (with Classes) and HKCU\\Environment, through
`reg.exe export` into the output's folder, parsed from UTF-16. Windows only; it holds the
profile's registry, so it is run on CI's runner alone (R294), never on a developer's PC.

`diff` prints every added (+), removed (-) or changed (~) entry. An entry is allowed when the
allow-list below names it (Windows' own bookkeeping, one folder, key or value per entry, each
with its reason), or, with --allow-worker-profile, when it belongs to the LitePack.Worker
AppContainer profile (matched by its moniker and the SID its Mappings key names, never by a
pattern over every AppContainer). Allowed entries are printed with their reason; the exit code
is 1 if any entry is not allowed.

R380 (CI runs 43683451272 and 43703543786, the installer's and the updater's uninstall diffs): on
CI's runner Explorer runs its one-time "post app install tasks" for the user when something asks
for them (an install's new Start menu shortcut does), and that registers Microsoft's own packages
for the runner's user in the middle of the test (Feedback Hub, Narrator QuickStart, WSL and the
.NET Native framework and runtime there: 671 records, the same in both runs but one GUID). `diff`
recognises such a registration from the diff's own entries (`packages()`) and excuses the records
that belong to it, nothing else:
  - the packages F: each family `<name>_<publisher id>` with an added folder
    %LOCALAPPDATA%\\Packages\\<family>, publisher id 8wekyb3d8bbwe or cw5n1h2txyewy (Microsoft's),
    name not containing LitePack; with no F the rule excuses nothing;
  - from the added entries the last point below does not bar: their full names N
    (`<name>_<version>_<arch>_<resource>_<id>` as a key path component, family in F), package SIDs
    S (Mappings keys whose added Moniker is in F), AppX ProgIDs P (HKCU\\Software\\Classes\\AppX<32>
    whose added Application [AppUserModelID] is `<family>!...`), and app execution aliases A (added
    reparse points %LOCALAPPDATA%\\Microsoft\\WindowsApps\\<family>\\<alias>);
  - excused, added entries only: a path with a whole component in F, N, S or P, or an app's
    AppUserModelID `<family>!<app>` (PushNotifications\\Backup); a value named by a P; under
    RegisteredApplications only a value whose data is the path of a package's capabilities,
    ...\\Repository\\Packages\\<full name in N>\\... (Windows' Default Programs entries of the
    packages' apps; LitePack's own `LitePack` value stays residue); an alias's reparse point
    WindowsApps\\<alias> and its CurrentVersion\\App Paths\\<alias> key and values; a URL protocol
    Classes\\<scheme> holding only `@ = "URL:<scheme>"` and `URL Protocol`, with
    UrlAssociations\\<scheme> added too (and that partner key, when it holds nothing), when a
    package's own capabilities name the scheme (an added value [<scheme>] under a key with a
    component in F or N whose path ends in Capabilities\\URLAssociations); an added key every added
    descendant of which is excused (one at least by this rule) and that has no unexcused value; and
    Explorer's marker [PostAppInstallTasksCompleted];
  - excused, changed entries, only with F: Explorer\\Package Installation [PackageListVersion],
    Search [InstalledPackagedAppsRevision], and the notification database's write-ahead log and the
    Start menu's and Search's tile model caches;
  - never: a removal, anything naming LitePack, org.litepack.gui, `.lpk` or LitePack's CLSID, the
    worker's AppContainer profile, RegisteredApplications, Run, RunOnce and HKCU\\Environment.
"""

import argparse
import codecs
import io
import json
import os
import re
import stat
import subprocess
import sys
import tempfile
import unittest
import unittest.mock
from pathlib import Path

# The worker's AppContainer profile (crates/lpk-sandbox-sys: APPCONTAINER_NAME).
WORKER_MONIKER = "LitePack.Worker"

REPARSE_POINT = stat.FILE_ATTRIBUTE_REPARSE_POINT

FILE_ROOTS = ("APPDATA", "LOCALAPPDATA", "DESKTOP")

# --allow-settings: the one file an uninstall without the box leaves (e7-design section 3).
SETTINGS_DIR = r"%APPDATA%\LitePack"
SETTINGS_FILE = SETTINGS_DIR + r"\settings.json"
SETTINGS_REASON = ("the GUI's settings, kept by an uninstall without the \"delete the application "
                   "data\" box (docs/notes/e7-design.md section 3)")


def root_dir(token):
    """The folder a file root's token names; DESKTOP is %USERPROFILE%\\Desktop."""
    if token == "DESKTOP":
        profile = os.environ.get("USERPROFILE")
        return os.path.join(profile, "Desktop") if profile else None
    return os.environ.get(token)
# HKCU\Software\Classes is the user's classes hive (UsrClass.dat) linked into HKCU\Software: it is
# exported on its own as well, and `snapshot` refuses unless its `Local Settings` key (where the
# AppContainer profiles are mapped) is in the result.
REG_ROOTS = (r"HKCU\Software", r"HKCU\Software\Classes", r"HKCU\Environment")
LOCAL_SETTINGS = r"HKCU\Software\Classes\Local Settings"
APPCONTAINER = LOCAL_SETTINGS + r"\Software\Microsoft\Windows\CurrentVersion\AppContainer"
MAPPINGS = APPCONTAINER + r"\Mappings"
STORAGE = APPCONTAINER + r"\Storage"

# Windows' own AppContainer bookkeeping (R301): after `DeleteAppContainerProfile`, Windows may keep
# these parent keys; Windows decides their lifetime, not LitePack. Allowed only when empty in the
# later snapshot (no value, no subkey); any value or subkey left under one is residue.
APPCONTAINER_REASON = ("Windows' own AppContainer bookkeeping: an empty parent key whose lifetime "
                       "DeleteAppContainerProfile decides (R301)")

# Windows' own bookkeeping, which running any program changes. One entry per folder (`file`: the
# folder or file and everything under it), key (`key`: the key and everything under it), value
# (`value`: a key and one value name) or empty key (`empty-key`: that key alone, added and holding
# nothing). Nothing LitePack writes may be listed here: that is fixed instead. Each entry's reason
# is printed beside every entry it allows.
AAD_LU = (LOCAL_SETTINGS + r"\Software\Microsoft\Windows\CurrentVersion\AppModel\SystemAppData"
          r"\Microsoft.AAD.BrokerPlugin_cw5n1h2txyewy\HAM\AUI\App\V1\LU")
SHELL_MUICACHE = LOCAL_SETTINGS + r"\Software\Microsoft\Windows\Shell\MuiCache"
SPOTLIGHT = r"HKCU\Software\Microsoft\Windows\CurrentVersion\DesktopSpotlight"
SPOTLIGHT_LU = (LOCAL_SETTINGS + r"\Software\Microsoft\Windows\CurrentVersion\AppModel\SystemAppData"
                r"\MicrosoftWindows.Client.CBS_cw5n1h2txyewy\HAM\AUI\Global.DesktopSpotlight\V1\LU")
SPOT = "Windows Spotlight's own state, which Windows updates in the background (CI run 42944220039, R343)"
USERASSIST_COUNT = (r"HKCU\Software\Microsoft\Windows\CurrentVersion\Explorer\UserAssist"
                    r"\{CEBFF5CD-ACE2-4F4F-9178-9926F41749EA}\Count")
WAM = ("written while the GUI starts, by Windows' account broker (WAM, "
       "Microsoft.AAD.BrokerPlugin), most likely for the WebView2 runtime (R315)")
MUI = "Windows' shell cache of a program's display strings (MuiCache), filled when a program runs"
START = "Windows' Start menu and search bookkeeping, updated when a shortcut is added or removed"
WEBVIEW2 = ("the WebView2 runtime's own per-user records, which it keeps for every WebView2 app "
            "(R313); LitePack's WebView2 data stays in data\\")

# Kinds: `file` (the folder or file and everything under it), `file-pattern` (files directly in
# one folder whose names match one shape), `key` (the key and everything under it), `key-only`
# (that one key's existence, not its values or subkeys), `value` (one key and one value name),
# `value-pattern` (one key, or with a `key` regular expression the keys it matches, value names of one
# shape), `file-changed-pattern` (files of one folder, names of one shape, changed only), `key-pattern-value` (one value name in keys
# of one shape, a regular expression over the whole key path), `empty-key` (that key alone, added and
# holding nothing), `file-changed` (that one file, changed only: never added or removed), `key-pattern`
# (the keys whose whole path matches a regular expression, and their values; not their subkeys),
# `key-pattern-only` (those keys' existence alone, not their values or subkeys; R373), `dir-pattern` (folders
# directly in one folder whose names match one shape, a folder in the later snapshot; R401),
# `file-in-dir-pattern` (files of one name shape directly inside such a folder, `dir` its shape, a file in the
# later snapshot; R401). Any
# entry may carry `signs`, the signs it may excuse ("+" added, "-" removed, "~" changed); without it,
# every sign its kind allows. A value-pattern's name may hold `{temp}`: the user's temp folder in its
# long form (`temp_folder()`).
USRCLASS = ("the storage files of HKCU\\Software\\Classes (its registry hive and transaction logs), which Windows "
            "rewrites whenever that hive is written; the diff already examines its keys one by one (R339)")
# R365 (CI run 43676006117): the first residue-checked GUI that ran long enough (the GUI the update
# restarted) to reach the WebView2 runtime's deferred start-up work.
R365 = "R365, CI run 43676006117"
# Windows names a jump list file by the CRC-64 of the upper-cased UTF-16LE AppUserModelID
# (register_tests.rs). Chromium's AUMID for the WebView2 browser (model ID MSEdgeWebView) plus the
# profile id it builds from the folder names EBWebView\Default: the same for every WebView2 app with
# that layout, so the runtime's, not LitePack's (whose own jump list the uninstaller removes).
WEBVIEW2_AUMID = "MSEDGEWEBVIEW.EBWEBVIEW.DEFAULT"
WEBVIEW2_JUMP_LIST = "2a2e0412b8ad04a2"
# Windows Search's record of each jump list's last update, one value per AppUserModelID; LitePack's own
# (org.litepack.gui) is deleted by the uninstaller (installer.nsh), so it stays residue.
SEARCH_JUMPLIST_DATA = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Search\JumplistData"
R367 = "R367, CI run 43683451272"
# R372 (CI run 43689320509's installer job, both attempts): records Windows wrote on the runner during the job's long
# step, while only the running lpk.exe compressed; nothing in LitePack names either key.
R372 = "R372, CI run 43689320509"
WIDGETS = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Dsh"
# R380 (CI run 43699094131's portable job): Storage Sense setting itself up (Parameters\CachedSizes,
# Parameters\StoragePolicy, SuggestedFolders); nothing in LitePack names it.
STORAGE_SENSE = r"HKCU\Software\Microsoft\Windows\CurrentVersion\StorageSense"
SHELL_REFRESH = (re.escape(LOCAL_SETTINGS + r"\Software\Microsoft\Windows\CurrentVersion\AppModel\ShellRefresh")
                 + r"\\[0-9]+")
CONSENT_STORE = r"HKCU\Software\Microsoft\Windows\CurrentVersion\CapabilityAccessManager\ConsentStore"
CONSENT = ("Windows' privacy consent defaults, which its capability access manager writes the first "
           "time a desktop program asks for camera, microphone or location state; LitePack asks for "
           "none (no getUserMedia, mediaDevices, geolocation, enumerateDevices or PermissionRequested "
           "in apps/ or crates/); written 3 s after the restarted GUI's WebView2 started, most likely "
           "by it; added only, a changed or removed privacy setting stays residue (" + R365 + ")")
CONSENT_KEYS = [CONSENT_STORE + "\\" + c + s
                for c in ("location", "microphone", "webcam") for s in ("", r"\NonPackaged")]
CONSENT_PARENTS = ("Windows' Capability Access Manager setting up the user's consent store (the two keys alone, "
                   "empty; CI run 43718620349's portable job, R388); LitePack writes nothing under it")
IDENTITY_CRL = r"HKCU\Software\Microsoft\IdentityCRL\Immersive\production"
# R401 (release.yml run 43738180914's verify jobs: x64 job 132893507301, ARM64 job 132893507494).
R401 = "R401, CI run 43738180914"
SECURITY_MAINTENANCE = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Security and Maintenance"
HELIUM = (r"%LOCALAPPDATA%\Packages\MicrosoftWindows.Client.WebExperience_cw5n1h2txyewy\SystemAppData"
          r"\Helium")
EXPLORER_STARTUP_LOG = r"%LOCALAPPDATA%\Microsoft\Windows\Explorer\ExplorerStartupLog.etl"
APPLICATION_VIEW = (r"HKCU\\Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\SessionInfo\\[0-9]+"
                    r"\\ApplicationViewManagement\\W32:[0-9A-F]{16}")
USER_TEMP = r"%LOCALAPPDATA%\Temp"
# NSIS's plugin folder ($PLUGINSDIR), named by GetTempFileName: "ns", a letter, and its unique part, one to four
# hexadecimal digits (nsrE5B7 and nsf1311 in CI run 43738180914, nsi2CA in run 43742607535; R404).
NSIS_PLUGINS_DIR = r"ns[a-z][0-9A-F]{1,4}\.tmp"
NSIS_PLUGINS = ("NSIS's plugin folder of the x86 installer or uninstaller, which NSIS deletes at exit but cannot "
                "under Windows on ARM's emulation while System.dll is still mapped (R401, windows-11-arm, CI run "
                "43738180914)")
# R406 (release.yml run 43746466112's verify-arm64, job 132919011093): two programs of the windows-11-arm image that
# write on their own schedule, in folders of their own.
CBS_WEBVIEW = r"%LOCALAPPDATA%\Packages\MicrosoftWindows.Client.CBS_cw5n1h2txyewy\LocalState\EBWebView"
FIREFOX_BACKGROUND_TASKS = r"%APPDATA%\Mozilla\Firefox\Background Tasks Profiles"
# R407 (CI run 43750018894's portable job, windows-2025; release.yml run 43750018868's verify-arm64): Windows' device
# census recording its scheduled run for the user, and the Widgets package's settings hive log beside its Helium ones.
CENSUS = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Census"
WEBEXPERIENCE_SETTINGS = r"%LOCALAPPDATA%\Packages\MicrosoftWindows.Client.WebExperience_cw5n1h2txyewy\Settings"
ALLOW = [
    # R407.
    {"kind": "key", "path": CENSUS,
     "reason": "Windows' device census (its scheduled Census task) recording its last run for the user; LitePack "
               "writes nothing under it (R407, CI run 43750018894)"},
    {"kind": "file-changed-pattern", "path": WEBEXPERIENCE_SETTINGS, "name": r"settings\.dat\.LOG[12]",
     "reason": "the Widgets package's own settings hive log, which Windows rewrites on its own (R407)"},
    # R406.
    {"kind": "file", "path": CBS_WEBVIEW,
     "reason": "the WebView2 data of Windows' own Start and Search client package (Client.CBS), which its "
               "component updater fills and rewrites on its own schedule; LitePack's WebView2 data is in its own "
               "folder (R406, CI run 43746466112)"},
    {"kind": "file", "path": FIREFOX_BACKGROUND_TASKS,
     "reason": "Firefox's background tasks (its default-browser agent, a scheduled task of the runner image's "
               "Firefox), which rewrite their own profiles; LitePack writes nothing there (R406, CI run "
               "43746466112)"},
    # R401.
    {"kind": "key", "path": SECURITY_MAINTENANCE,
     "reason": "Windows' Security and Maintenance setting up its checks for a user on its own schedule; LitePack "
               "writes nothing under it (" + R401 + ")"},
    {"kind": "file-changed-pattern", "path": HELIUM, "name": r"(User|UserClasses)\.dat\.LOG[12]",
     "reason": "the Widgets package's own registry hive logs, which Windows rewrites on its own (R401)"},
    {"kind": "file-changed", "path": EXPLORER_STARTUP_LOG,
     "reason": "Explorer's own start-up trace log (R401)"},
    # R404 (CI run 43742607535): removed too, when a window that existed at the baseline closes later.
    {"kind": "key-pattern", "path": APPLICATION_VIEW, "signs": "+-",
     "reason": "Explorer's per-session record of a window it showed (a session's volatile key, cleared at "
               "logoff) (R401, R404)"},
    {"kind": "dir-pattern", "path": USER_TEMP, "name": NSIS_PLUGINS_DIR, "signs": "+", "reason": NSIS_PLUGINS},
    {"kind": "file-in-dir-pattern", "path": USER_TEMP, "dir": NSIS_PLUGINS_DIR, "name": r"System\.dll",
     "signs": "+", "reason": NSIS_PLUGINS + ": its System.dll"},
    # R365.
    {"kind": "file",
     "path": r"%APPDATA%\Microsoft\Windows\Recent\CustomDestinations"
             "\\" + WEBVIEW2_JUMP_LIST + ".customDestinations-ms",
     "signs": "+~",
     "reason": "the WebView2 runtime's jump list, named by the CRC-64 of its AppUserModelID "
               + WEBVIEW2_AUMID + ", the same for every WebView2 app with the folders EBWebView\\Default; "
               "the runtime's, not LitePack's (" + R365 + ")"},
    {"kind": "value", "path": SEARCH_JUMPLIST_DATA, "name": "MSEdgeWebView.EBWebView.Default",
     "signs": "+~",
     "reason": "Windows Search's record of the WebView2 runtime's jump list (the file above), a value "
               "named by the same AppUserModelID and written some time after the list; the runtime's, "
               "not LitePack's (" + R367 + ")"},
    # R372.
    {"kind": "key-only", "path": WIDGETS, "signs": "+",
     "reason": "the Widgets board's key (Dsh), which Windows writes on its own schedule; LitePack writes nothing "
               "under it (" + R372 + ")"},
    {"kind": "value", "path": WIDGETS, "name": "WidgetsStateLastRun", "signs": "+~",
     "reason": "the Widgets board's last-run time, written by Windows while the job's CLI ran (" + R372 + ")"},
    {"kind": "key", "path": STORAGE_SENSE,
     "reason": "Windows' Storage Sense, which sets itself up for a user on its own schedule; LitePack writes "
               "nothing under it (R380, CI run 43699094131)"},
    # The numbered keys themselves (`key-pattern-only`; added or removed: a key has no content to
    # change), and their default value alone (its name is the empty string, printed `@`); any other
    # value under them stays residue (R373). Both assume LitePack installs no package: revisit them with
    # the sparse package (E7-5 part 2, E6-3).
    {"kind": "key-pattern-only", "path": SHELL_REFRESH, "signs": "+-",
     "reason": "Windows' package model asking the shell to refresh after a package change (AppModel\\ShellRefresh), "
               "a numbered key Windows adds and removes itself; LitePack installs no package (" + R372 + ")"},
    {"kind": "key-pattern-value", "path": SHELL_REFRESH, "name": "", "signs": "+-~",
     "reason": "Windows' package model asking the shell to refresh after a package change (AppModel\\ShellRefresh), "
               "a numbered key Windows adds and removes itself; LitePack installs no package (" + R372 + ")"},
    {"kind": "file", "path": r"%LOCALAPPDATA%\D3DSCache",
     "reason": "Direct3D's shader cache, which Windows' graphics runtime keeps for any program that "
               "draws with Direct3D; most likely written by WebView2's GPU process, about 120 s after "
               "the GUI started (" + R365 + ")"},
    *[{"kind": "key-only", "path": k, "signs": "+", "reason": CONSENT + ": the key"}
      for k in CONSENT_KEYS],
    # R388: the two parent keys alone (key-only: never a value or a subkey under them, never removed).
    *[{"kind": "key-only", "path": k, "signs": "+", "reason": CONSENT_PARENTS}
      for k in (CONSENT_STORE.rsplit("\\", 1)[0], CONSENT_STORE)],
    *[{"kind": "value", "path": k, "name": n, "signs": "+", "reason": CONSENT + ": its " + n}
      for k in CONSENT_KEYS for n in ("Value", "LastSetTime")],
    {"kind": "key-pattern",
     "path": re.escape(IDENTITY_CRL) + r"\\Token\\\{[0-9A-F]{8}(-[0-9A-F]{4}){3}-[0-9A-F]{12}\}",
     "reason": WAM + ": the Microsoft account device ticket, the registry side of its token cache "
                     "(" + R365 + ")"},
    {"kind": "value-pattern", "path": IDENTITY_CRL + r"\Property", "name": r"[0-9A-F]{16}",
     "reason": WAM + ": the device's property, named by its device id (" + R365 + ")"},
    {"kind": "file-changed", "path": r"%LOCALAPPDATA%\Microsoft\PowerShell\StartupProfileData-NonInteractive",
     "reason": "PowerShell's startup profile, rewritten by the CI job's own pwsh calls after the first "
               "snapshot; LitePack runs no PowerShell (" + R365 + ")"},
    {"kind": "value-pattern", "path": SHELL_MUICACHE,
     "name": (r"{temp}\\LitePack-[0-9]+(\.[0-9]+)*-updater-[A-Za-z0-9]{6}"
              r"\\LitePack-[0-9]+(\.[0-9]+)*-installer\.exe\.(ApplicationCompany|FriendlyAppName)"),
     "reason": MUI + ": the update's installer, written by the shell when the updater plugin's "
                     "ShellExecuteW started it from the temp folder (" + R365 + ")"},

    # CI run 42933006998's portable job, ruling R339.
    {"kind": "file-changed", "path": r"%LOCALAPPDATA%\Microsoft\Windows\UsrClass.dat", "reason": USRCLASS},
    {"kind": "file-changed", "path": r"%LOCALAPPDATA%\Microsoft\Windows\UsrClass.dat.LOG1", "reason": USRCLASS},
    {"kind": "file-changed", "path": r"%LOCALAPPDATA%\Microsoft\Windows\UsrClass.dat.LOG2", "reason": USRCLASS},
    # E7-6's first full diff (CI run 42483731626), ruling R313.
    {"kind": "file-pattern", "path": r"%LOCALAPPDATA%\Microsoft\TokenBroker\Cache",
     "name": r"[0-9a-f]{40}\.tbres", "reason": WAM + ": its token cache, one file per token"},
    # Any credential LitePack ever stores must use a target named after LitePack: the CI job's
    # `cmdkey /list` check after each run fails on a target containing "LitePack" or "lpk", so a
    # LitePack credential cannot hide behind this entry.
    {"kind": "file-pattern", "path": r"%LOCALAPPDATA%\Microsoft\Credentials",
     "name": r"[0-9A-F]{32}", "reason": WAM + ": the credential it stores through DPAPI"},
    {"kind": "value", "path": AAD_LU, "name": "PCT",
     "reason": WAM + ": the broker app's last-use time"},
    {"kind": "value", "path": AAD_LU, "name": "PTT",
     "reason": WAM + ": the broker app's last-use time"},
    {"kind": "value", "path": SPOTLIGHT, "name": "Maintenance", "reason": SPOT + ": its maintenance record"},
    {"kind": "value", "path": SPOTLIGHT, "name": "State", "reason": SPOT + ": its state"},
    {"kind": "value", "path": SPOTLIGHT_LU, "name": "PCT", "reason": SPOT + ": its last-use time"},
    {"kind": "value", "path": SPOTLIGHT_LU, "name": "PTT", "reason": SPOT + ": its last-use time"},
    {"kind": "value", "path": r"HKCU\Software\Microsoft\AuthCookies\Live\Default\DIDC",
     "name": "Data", "reason": WAM + ": the Microsoft account device cookie"},
    # The hex subkey differs between runs (1a4, 1a7); the names must point at a DLL of Windows' own
    # system folder (no backslash in the DLL's name), never at the install or run folder (R352); or a bare DLL
    # name whose name holds neither lpk nor litepack (R401: @windows.storage.dll,-21791 on windows-11-arm).
    {"kind": "value-pattern", "key": re.escape(LOCAL_SETTINGS) + r"\\MuiCache\\[0-9a-f]+\\52C64B7E",
     "name": (r"(@%SystemRoot%\\System32\\[\w.-]+\.dll,-[0-9]+"
              r"|C:\\Windows\\system32,@[\w.-]+\.dll,-[0-9]+"
              r"|@(?![\w.-]*(lpk|litepack))[\w.-]+\.dll,-[0-9]+)"),
     "reason": "Windows' cache of the display strings it loads from its own DLLs (MuiCache), "
               "filled when a program starts"},
    {"kind": "file-changed-pattern", "path": r"%LOCALAPPDATA%\Microsoft\Windows\Explorer",
     "name": r"(iconcache|thumbcache)_[\w]+\.db",
     "reason": "Explorer's own icon and thumbnail caches, which it rewrites when it refreshes icons"},
    {"kind": "key-only", "path": SHELL_MUICACHE, "reason": MUI + ": the cache's own key"},
    {"kind": "value-pattern", "path": SHELL_MUICACHE,
     # {run} is the escaped run folder (--run-folder, or RUNNER_TEMP\portable-run\LitePack); with
     # none known, the entry matches nothing.
     "name": r"{run}\\lpk-gui\.exe\.(ApplicationCompany|FriendlyAppName)",
     "reason": MUI + ": lpk-gui.exe's company and friendly name, keyed by its path in the run "
                     "folder"},
    {"kind": "value", "path": SHELL_MUICACHE, "name": "LangID",
     "reason": MUI + ": the cache's language"},
    {"kind": "value", "path": USERASSIST_COUNT, "name": "HRZR_PGYFRFFVBA",
     "reason": "Explorer's usage record (UserAssist): its session counter, UEME_CTLSESSION"},
    {"kind": "value", "path": USERASSIST_COUNT,
     "name": "Zvpebfbsg.JvaqbjfGrezvany_8jrxlo3q8oojr!Ncc",
     "reason": "Explorer's usage record (UserAssist) of Windows Terminal "
               "(Microsoft.WindowsTerminal_8wekyb3d8bbwe!App), not a LitePack program"},
    {"kind": "key", "path": r"HKCU\Software\Microsoft\EdgeWebView", "reason": WEBVIEW2},
    {"kind": "key",
     "path": r"HKCU\Software\Microsoft\EdgeUpdate\ClientState\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}",
     "reason": WEBVIEW2 + ": the runtime's update state under its own GUID"},
    # E7-5's first installer diff (CI run 42486016300): what Windows' shell and Start menu record
    # when an installer adds and its uninstaller removes a Start menu and a desktop shortcut, and
    # what Windows records when the installed GUI runs. The jump list Windows keeps for LitePack's
    # AppUserModelID is LitePack's and is removed by the uninstaller instead (installer.nsh).
    {"kind": "file-pattern", "path": r"%LOCALAPPDATA%\Microsoft\Windows\Caches",
     "name": r"\{3DA71D5A-20CC-432F-A115-DFE92379E91F\}\.3\.ver0x[0-9a-f]{16}\.db",
     "reason": START + ": the shell's cache of the installed programs, one file per version"},
    {"kind": "file-pattern",
     "path": r"%LOCALAPPDATA%\Packages\MicrosoftWindows.Client.CBS_cw5n1h2txyewy\LocalState\Search",
     "name": r"AppsIndex\.db(-wal|-shm)?", "reason": START + ": Windows Search's index of the apps"},
    {"kind": "file-pattern",
     "path": r"%LOCALAPPDATA%\Packages\Microsoft.Windows.StartMenuExperienceHost_cw5n1h2txyewy\LocalState",
     "name": r"start2\.bin", "reason": START + ": the Start menu's layout state"},
    {"kind": "value", "path": r"HKCU\Software\Microsoft\Windows\CurrentVersion\Start", "name": "Config",
     "reason": START + ": the Start menu's configuration"},
    {"kind": "value", "path": r"HKCU\Software\Microsoft\Windows\CurrentVersion\Search",
     "name": "InstalledWin32AppsRevision",
     "reason": START + ": Windows Search's revision of the installed desktop programs"},
    # The GUID in this key's name differs between runners (56dd80ad-... in run 42486016300,
    # e7cbac1d-... in run 42489790951): the key is matched by its shape, the value by its name.
    {"kind": "key-pattern-value",
     "path": (r"HKCU\\Software\\Microsoft\\Windows\\CurrentVersion\\CloudStore\\Store\\Cache"
              r"\\DefaultAccount\\\$de\$\{[0-9a-f-]{36}\}\$\$windows\.data\.unifiedtile"
              r"\.localstarttilepropertiesmap\\Current"),
     "name": "Data", "reason": START + ": the Start menu's tile properties (CloudStore)"},
    {"kind": "file-pattern",
     "path": r"%LOCALAPPDATA%\Packages\Microsoft.Windows.StartMenuExperienceHost_cw5n1h2txyewy\TempState",
     "name": r"prep_Activatable Class_Hermes_perf\.cache",
     "reason": START + ": a performance cache of the Start menu's host (run 42489790951)"},
    {"kind": "value", "path": r"HKCU\Software\Microsoft\Windows\Shell\Bags\1\Desktop",
     "name": "IconLayouts",
     "reason": "Explorer's desktop icon positions, rewritten when a desktop shortcut comes and goes"},
    {"kind": "file", "path": r"%LOCALAPPDATA%\Microsoft\Windows\WebCache",
     "reason": "WinINet's shared cache database (ESE), updated by any program that uses WinINet or "
               "urlmon (Windows' own, shared by every user program)"},
    {"kind": "value-pattern", "path": SHELL_MUICACHE,
     "name": r"C:\\Windows\\System32\\conhost\.exe\.(ApplicationCompany|FriendlyAppName)",
     "reason": MUI + ": Windows' Console Window Host (conhost.exe)"},
    {"kind": "value-pattern", "path": USERASSIST_COUNT, "name": r"{run13}\\ycx-thv\.rkr",
     "reason": "Explorer's usage record (UserAssist) of lpk-gui.exe, keyed by its path in the run "
               "folder (ROT13, as Explorer stores it)"},
    {"kind": "value", "path": r"HKCU\Software\Microsoft\Windows NT\CurrentVersion\Winlogon",
     "name": "ParseAutoexec",
     "reason": "R324: Windows' own legacy logon setting (Winlogon), most likely rewritten when "
               "Windows rebuilds the environment after the Path broadcast; nothing in LitePack's "
               "repository names Winlogon but this entry (grep, 2026-11-08)"},
    # R301.
    {"kind": "empty-key", "path": APPCONTAINER, "reason": APPCONTAINER_REASON},
    {"kind": "empty-key", "path": MAPPINGS, "reason": APPCONTAINER_REASON},
    {"kind": "empty-key", "path": STORAGE, "reason": APPCONTAINER_REASON},
]

# The environment variable that marks a disposable CI runner (GitHub Actions sets it).
CI_ENV = "GITHUB_ACTIONS"


def norm(s):
    """Windows' comparison of names: case-insensitive."""
    return s.casefold()


def under(path, root):
    """Whether `path` is `root` or lies under it (backslash-separated, case-insensitive)."""
    p, r = norm(path), norm(root).rstrip("\\")
    return p == r or p.startswith(r + "\\")


# --- snapshot --------------------------------------------------------------------------------


def long_path(p):
    """`p` with the `\\\\?\\` prefix, so that no path length limit applies to the walk."""
    p = os.path.abspath(p)
    if p.startswith("\\\\?\\"):
        return p
    if p.startswith("\\\\"):
        return "\\\\?\\UNC\\" + p[2:]
    return "\\\\?\\" + p


def record(path):
    """What the walk records for `path`, never following it: (record, descend)."""
    st = os.lstat(path)
    if getattr(st, "st_file_attributes", 0) & REPARSE_POINT or stat.S_ISLNK(st.st_mode):
        # A junction or a symbolic link (any reparse point): recorded, never followed.
        return {"kind": "reparse", "mtime": st.st_mtime_ns}, False
    if stat.S_ISDIR(st.st_mode):
        return {"kind": "dir"}, True
    return {"kind": "file", "size": st.st_size, "mtime": st.st_mtime_ns}, False


def walk(root_dir, token, out):
    """Record `root_dir`'s tree in `out` under names that start with %token%; every path that
    cannot be read is recorded as `unreadable` and printed."""
    stack = [(long_path(root_dir), f"%{token}%")]
    while stack:
        d, name = stack.pop()
        try:
            names = sorted(os.listdir(d))
        except OSError as e:
            out[name] = {"kind": "unreadable", "error": e.__class__.__name__}
            print(f"unreadable: {name} ({e.__class__.__name__})")
            continue
        for child in names:
            n = name + "\\" + child
            p = d + "\\" + child
            try:
                rec, descend = record(p)
            except OSError as err:
                out[n] = {"kind": "unreadable", "error": err.__class__.__name__}
                print(f"unreadable: {n} ({err.__class__.__name__})")
                continue
            out[n] = rec
            if descend:
                stack.append((p, n))


def parse_reg(text):
    """A `reg export` file's text: {key: {value name ('' for the default): data as written}}.
    Key names are given as HKCU\\...; values keep their exported form (`"text"`, `dword:...`,
    `hex(n):...`), continuation lines joined."""
    keys = {}
    current = None
    last = None  # (key, name) of the last value, for its continuation lines
    pending = None  # a value whose line ended in a backslash
    for raw in text.splitlines():
        line = raw.rstrip("\r")
        if pending is not None:
            k, name, data = pending
            part = line.strip()
            if part.endswith("\\"):
                pending = (k, name, data + part[:-1])
            else:
                keys[k][name] = data + part
                pending = None
            continue
        if not line.strip() or line.startswith("Windows Registry Editor"):
            continue
        if line.startswith("[") and line.endswith("]"):
            current = line[1:-1]
            for long, short in (("HKEY_CURRENT_USER", "HKCU"),):
                if current.upper().startswith(long):
                    current = short + current[len(long):]
            keys.setdefault(current, {})
            last = None
            continue
        if current is not None and (line.startswith('"') or line.startswith("@=")):
            if line.startswith("@="):
                name, rest = "", line[2:]
            else:
                i, chars = 1, []
                while i < len(line) and line[i] != '"':
                    if line[i] == "\\" and i + 1 < len(line):
                        i += 1
                    chars.append(line[i])
                    i += 1
                name = "".join(chars)
                rest = line[i + 1:]
                if not rest.startswith("="):
                    raise ValueError(f"unparsed .reg line: {line!r}")
                rest = rest[1:]
            if rest.endswith("\\") and not rest.startswith('"'):
                pending = (current, name, rest[:-1])
            else:
                keys[current][name] = rest
            last = (current, name)
            continue
        if last is not None:
            # A string value with a line break inside it.
            keys[last[0]][last[1]] += "\n" + line
            continue
        raise ValueError(f"unparsed .reg line: {line!r}")
    return keys


def read_reg_file(path):
    """A `reg export` file (UTF-16 with its byte order mark)."""
    data = Path(path).read_bytes()
    enc = "utf-16" if data[:2] in (b"\xff\xfe", b"\xfe\xff") else "utf-8-sig"
    return parse_reg(data.decode(enc))


def snapshot(out, on_ci):
    if os.name != "nt":
        raise SystemExit("e7-residue: snapshot runs on Windows only")
    if not on_ci and os.environ.get(CI_ENV) != "true":
        raise SystemExit(f"e7-residue: snapshot records this profile's registry; it runs only on a "
                         f"disposable machine ({CI_ENV}=true, or --on-ci)")
    out = Path(out)
    files = {}
    for token in FILE_ROOTS:
        root = root_dir(token)
        if not root:
            raise SystemExit(f"e7-residue: the folder of %{token}% is not set")
        if not os.path.isdir(root):
            # Recorded, so that a root created later shows up as a change.
            files[f"%{token}%"] = {"kind": "absent"}
            continue
        walk(root, token, files)
    registry = {}
    with tempfile.TemporaryDirectory(dir=out.parent) as tmp:
        for i, root in enumerate(REG_ROOTS):
            reg = Path(tmp) / f"{i}.reg"
            subprocess.run(["reg.exe", "export", root, str(reg), "/y"], check=True,
                           stdout=subprocess.DEVNULL)
            registry.update(read_reg_file(reg))
    if not any(under(k, LOCAL_SETTINGS) for k in registry):
        raise SystemExit(f"e7-residue: the export holds no key under {LOCAL_SETTINGS}")
    out.write_text(json.dumps({"files": files, "registry": registry}, sort_keys=True),
                   encoding="utf-8")
    unreadable = sum(1 for r in files.values() if r["kind"] == "unreadable")
    print(f"snapshot {out}: {len(files)} files and folders ({unreadable} unreadable), "
          f"{len(registry)} keys")


# --- diff ------------------------------------------------------------------------------------


def entries(snap):
    """A snapshot as {(kind, path, value name) compared case-insensitively: (the path and name as
    recorded, what it records)}."""
    e = {}
    for path, rec in snap["files"].items():
        e[("file", norm(path), None)] = (path, None, rec)
    for key, values in snap["registry"].items():
        e[("key", norm(key), None)] = (key, None, True)
        for name, data in values.items():
            e[("value", norm(key), norm(name))] = (key, name, data)
    return e


def profile_sids(*snaps):
    """The SIDs whose Mappings key names the worker's moniker, in any of the snapshots."""
    sids = set()
    for s in snaps:
        for key, values in s["registry"].items():
            parent, _, sid = key.rpartition("\\")
            if norm(parent) != norm(MAPPINGS):
                continue
            moniker = [v for n, v in values.items() if norm(n) == "moniker"]
            if any(norm(v) == norm(f'"{WORKER_MONIKER}"') for v in moniker):
                sids.add(norm(sid))
    return sids


def is_profile(kind, path, sids):
    """Whether an entry belongs to the worker's profile: a path component is its moniker (its
    folder under %LOCALAPPDATA%\\Packages, its AppContainer Storage key) or its SID."""
    parts = [norm(p) for p in path.split("\\")]
    if kind == "file":
        return under(path, f"%LOCALAPPDATA%\\Packages\\{WORKER_MONIKER}") or \
            any(p in sids for p in parts)
    return norm(WORKER_MONIKER) in parts or any(p in sids for p in parts)


def empty_key(snap, key):
    """Whether `key` is in `snap` holding nothing: no value, and no subkey but the R301 parents,
    each holding nothing in turn."""
    parents = {norm(a["path"]) for a in ALLOW if a["kind"] == "empty-key"}
    k = norm(key)
    found = False
    for path, values in snap["registry"].items():
        p = norm(path)
        if p == k:
            found = True
            if values:
                return False
        elif p.startswith(k + "\\"):
            if p not in parents or "\\" in p[len(k) + 1:] or not empty_key(snap, path):
                return False
    return found


def rot13(text):
    """Explorer's UserAssist value names: ROT13 over the ASCII letters."""
    return codecs.encode(str(text), "rot13")


def uninstaller_copy(path, after):
    """R312: NSIS's uninstaller self-copy, exactly %LOCALAPPDATA%\\Temp\\~nsu*.tmp holding one
    Un.exe or Un_<letter>.exe and nothing else (in `after`): the folder or that file."""
    def is_dir(p):
        folder, _, leaf = p.rpartition("\\")
        return norm(folder) == norm(r"%LOCALAPPDATA%\Temp") and \
            re.fullmatch(r"~nsu[0-9a-z]*\.tmp", leaf, re.IGNORECASE)
    folder = path if is_dir(path) else path.rpartition("\\")[0]
    if not is_dir(folder):
        return False
    inside = [p for p in after["files"] if under(p, folder) and norm(p) != norm(folder)]
    return len(inside) == 1 and re.fullmatch(r"Un(_[A-Z])?\.exe", inside[0].rpartition("\\")[2],
                                              re.IGNORECASE) is not None and \
        after["files"][inside[0]].get("kind") == "file"


UNINSTALLER_REASON = ("R312: NSIS's uninstaller copy; NSIS runs the uninstaller from a copy so that it "
                      "can remove the install folder, and only a reboot could delete that copy, which "
                      "a per-user uninstaller cannot schedule")


def run_pattern(run):
    """The escaped run folder for the `{run}` placeholder; None when no run folder is known."""
    if not run:
        return None
    return re.escape(str(run).replace("/", "\\").rstrip("\\"))


def temp_folder():
    """The user's temp folder in its long form (R365: Windows' MuiCache names the update's installer
    under the profile folder's long name, while the process ran from its short 8.3 form); None if
    unknown."""
    try:
        return os.path.realpath(tempfile.gettempdir())
    except OSError:
        return None


def allowed(kind, path, name, sign, after, run=None, unreadable=False):
    """The allow-list's reason for an entry, or None. Nothing excuses a path that either snapshot
    could not read."""
    if unreadable:
        return None
    for a in ALLOW:
        if "signs" in a and sign not in a["signs"]:
            continue
        if a["kind"] == "file" and kind == "file" and under(path, a["path"]):
            return a["reason"]
        if a["kind"] == "key" and kind in ("key", "value") and under(path, a["path"]):
            return a["reason"]
        if a["kind"] == "value" and kind == "value" and norm(path) == norm(a["path"]) and \
                norm(name) == norm(a["name"]):
            return a["reason"]
        if a["kind"] == "file-changed-pattern" and kind == "file" and sign == "~":
            folder, _, leaf = path.rpartition("\\")
            if norm(folder) == norm(a["path"]) and re.fullmatch(a["name"], leaf, re.IGNORECASE):
                return a["reason"]
        if a["kind"] == "value-pattern" and kind == "value" and (
                re.fullmatch(a["key"], path, re.IGNORECASE) if "key" in a
                else norm(path) == norm(a["path"])):
            pattern = a["name"]
            for token, folder in (("{run}", run), ("{run13}", rot13(run) if run else None),
                                  ("{temp}", temp_folder() if "{temp}" in pattern else None)):
                if token in pattern:
                    escaped = run_pattern(folder)
                    if escaped is None:
                        pattern = None
                        break
                    pattern = pattern.replace(token, escaped)
            if pattern is None:
                continue
            if re.fullmatch(pattern, name, re.IGNORECASE | re.DOTALL):
                return a["reason"]
        if a["kind"] == "key-pattern-value" and kind == "value" and \
                re.fullmatch(a["path"], path, re.IGNORECASE) and norm(name) == norm(a["name"]):
            return a["reason"]
        if a["kind"] == "key-pattern" and kind in ("key", "value") and \
                re.fullmatch(a["path"], path, re.IGNORECASE):
            return a["reason"]
        if a["kind"] == "key-pattern-only" and kind == "key" and \
                re.fullmatch(a["path"], path, re.IGNORECASE):
            return a["reason"]
        if a["kind"] == "key-only" and kind == "key" and norm(path) == norm(a["path"]):
            return a["reason"]
        if a["kind"] == "file-pattern" and kind == "file":
            folder, _, leaf = path.rpartition("\\")
            if norm(folder) == norm(a["path"]) and re.fullmatch(a["name"], leaf, re.IGNORECASE):
                return a["reason"]
        if a["kind"] == "dir-pattern" and kind == "file":
            folder, _, leaf = path.rpartition("\\")
            if norm(folder) == norm(a["path"]) and re.fullmatch(a["name"], leaf, re.IGNORECASE) and \
                    after["files"].get(path, {}).get("kind") == "dir":
                return a["reason"]
        if a["kind"] == "file-in-dir-pattern" and kind == "file":
            folder, _, leaf = path.rpartition("\\")
            parent, _, sub = folder.rpartition("\\")
            if norm(parent) == norm(a["path"]) and re.fullmatch(a["dir"], sub, re.IGNORECASE) and \
                    re.fullmatch(a["name"], leaf, re.IGNORECASE) and \
                    after["files"].get(path, {}).get("kind") == "file":
                return a["reason"]
        if a["kind"] == "file-changed" and kind == "file" and sign == "~" and \
                norm(path) == norm(a["path"]):
            return a["reason"]
        if a["kind"] == "empty-key" and kind == "key" and sign == "+" and \
                norm(path) == norm(a["path"]) and empty_key(after, path):
            return a["reason"]
    return None


# --- R380: Windows registering its own packages for the runner's user (see the header) ---------

R380_RUNS = "R380, CI runs 43683451272 and 43703543786"
MS_PUBLISHERS = ("8wekyb3d8bbwe", "cw5n1h2txyewy")
PACKAGES_DIR = r"%LOCALAPPDATA%\Packages"
WINDOWS_APPS = r"%LOCALAPPDATA%\Microsoft\WindowsApps"
CLASSES = r"HKCU\Software\Classes"
CURRENT_VERSION = r"HKCU\Software\Microsoft\Windows\CurrentVersion"
APP_PATHS = CURRENT_VERSION + r"\App Paths"
EXPLORER = CURRENT_VERSION + r"\Explorer"
URL_ASSOCIATIONS = r"HKCU\Software\Microsoft\Windows\Shell\Associations\UrlAssociations"
CAPABILITY_ACCESS = CURRENT_VERSION + r"\CapabilityAccessManager"
LITEPACK_CLSID = "{01E42821-7D51-4B9B-AD9F-FFCA769A6025}"
POST_INSTALL_MARKER = (EXPLORER, "PostAppInstallTasksCompleted")
# Rule 4: what a package registration changes in Windows' own bookkeeping (changed entries only).
R380_CHANGED_VALUES = [(EXPLORER + r"\Package Installation", "PackageListVersion"),
                       (CURRENT_VERSION + r"\Search", "InstalledPackagedAppsRevision")]
R380_CHANGED_FILES = [
    r"%LOCALAPPDATA%\Microsoft\Windows\Notifications\wpndatabase.db-wal",
    r"%LOCALAPPDATA%\Packages\Microsoft.Windows.StartMenuExperienceHost_cw5n1h2txyewy\TempState"
    r"\StartUnifiedTileModelCache.dat",
    r"%LOCALAPPDATA%\Packages\MicrosoftWindows.Client.CBS_cw5n1h2txyewy\TempState\SearchUnifiedTileModelCache.dat",
]
# Rule 5: keys this rule never excuses anything under. RegisteredApplications (where LitePack registers
# itself, as `LitePack`) yields only its values whose data is the path of a package's capabilities,
# Repository\Packages\<full name>\...: Windows' Default Programs entries of the packages' apps.
REGISTERED_APPS = r"HKCU\Software\RegisteredApplications"
R380_NEVER_KEYS = [CURRENT_VERSION + r"\Run",
                   CURRENT_VERSION + r"\RunOnce", r"HKCU\Environment"]
FULL_NAME = re.compile(r"(?P<name>[^_\\]+)_(?P<version>[0-9]+(\.[0-9]+){3})_(?P<arch>[0-9a-z]+)_"
                       r"(?P<resource>[^_\\]*)_(?P<id>[0-9a-z]{13})", re.IGNORECASE)
APPX_PROGID = re.compile(r"AppX[0-9a-z]{32}", re.IGNORECASE)


def r380_reason(families, what):
    """The printed reason of an entry R380 excuses, naming its package (or packages)."""
    fams = sorted(set(families), key=norm)
    which = (f"its own package {fams[0]}" if len(fams) == 1
             else "its own packages " + ", ".join(fams))
    return f"Windows registering {which} for the runner's user during the run ({R380_RUNS}): {what}"


def unquote(data):
    """A string value's text as `reg export` writes it, without its quotes."""
    if isinstance(data, str) and len(data) >= 2 and data.startswith('"') and data.endswith('"'):
        return data[1:-1]
    return data if isinstance(data, str) else ""


def r380_never(kind, path, name, rec, worker_sids):
    """Rule 5: whether R380 must never excuse this entry, whatever else it names."""
    texts = [path, name or ""] + ([rec] if kind == "value" and isinstance(rec, str) else [])
    for t in texts:
        t = norm(t)
        if "litepack" in t or norm(LITEPACK_CLSID) in t:  # org.litepack.gui and LitePack.Worker too
            return True
    parts = [norm(p) for p in path.split("\\")]
    if ".lpk" in parts or norm(name or "") == ".lpk":
        return True
    if any(p in worker_sids for p in parts):
        return True
    if kind != "file" and any(under(path, k) for k in R380_NEVER_KEYS):
        return True
    if "registeredapplications" in parts:
        # Only a value naming a package's full name: what packages() checks for itself (see its end).
        return kind != "value" or not under(path, REGISTERED_APPS)
    return False


def package_family(leaf):
    """The family name `leaf` is when it names a Microsoft package that is not LitePack's, or None."""
    name, sep, publisher = leaf.rpartition("_")
    if not sep or not name or "_" in name or norm(publisher) not in MS_PUBLISHERS:
        return None
    if "litepack" in norm(name):
        return None
    return leaf


def packages(changes, reasons, skip, worker_sids):
    """R380 over all of a diff's entries at once: the reason for each entry it excuses, or None.
    `changes` are diff()'s (sign, path, name, rec, kind); `reasons` the reasons they already have
    (the allow-list's), used only to see a parent key's descendants as excused; `skip` the indexes
    nothing may excuse (an unreadable path)."""
    out = [None] * len(changes)
    added = [(i, c) for i, c in enumerate(changes) if c[0] == "+"]
    # The entries the sets F, N, S, P and A are built from: rule 5's feed none of them.
    sources = [(i, c) for i, c in added if not r380_never(c[4], c[1], c[2], c[3], worker_sids)]
    # F: the families with an added folder under %LOCALAPPDATA%\Packages.
    fam = {}
    for _, (sign, path, name, rec, kind) in sources:
        folder, _, leaf = path.rpartition("\\")
        if kind == "file" and rec.get("kind") == "dir" and norm(folder) == norm(PACKAGES_DIR):
            f = package_family(leaf)
            if f:
                fam[norm(f)] = f
    if not fam:
        return out
    # N, S, P and A, each mapped to its family.
    full, sids, progids, aliases = {}, {}, {}, {}
    for _, (sign, path, name, rec, kind) in sources:
        parts = path.split("\\")
        if kind != "file":
            for p in parts:
                m = FULL_NAME.fullmatch(p)
                if m and norm(m["name"] + "_" + m["id"]) in fam:
                    full[norm(p)] = fam[norm(m["name"] + "_" + m["id"])]
        if kind == "value":
            parent, _, leaf = path.rpartition("\\")
            if norm(parent) == norm(MAPPINGS) and norm(leaf).startswith("s-1-15-2-") and \
                    norm(name or "") == "moniker" and norm(unquote(rec)) in fam and \
                    norm(leaf) not in worker_sids:
                sids[norm(leaf)] = fam[norm(unquote(rec))]
            if norm(parent).rpartition("\\")[0] == norm(CLASSES) and \
                    APPX_PROGID.fullmatch(parent.rpartition("\\")[2]) and norm(leaf) == "application" and \
                    norm(name or "") == "appusermodelid":
                f = norm(unquote(rec)).partition("!")
                if f[1] and f[0] in fam:
                    progids[norm(parent.rpartition("\\")[2])] = fam[f[0]]
        if kind == "file" and rec.get("kind") == "reparse":
            folder, _, leaf = path.rpartition("\\")
            outer, _, f = folder.rpartition("\\")
            if norm(outer) == norm(WINDOWS_APPS) and norm(f) in fam:
                aliases[norm(leaf)] = fam[norm(f)]
    named = {**{k: (v, "a record under its family name") for k, v in fam.items()},
             **{k: (v, "a record under its full name") for k, v in full.items()},
             **{k: (v, "a record under its package SID") for k, v in sids.items()},
             **{k: (v, "a record under its AppX ProgID") for k, v in progids.items()}}
    everyone = list(fam.values())
    # The URL schemes a package's own capabilities name: an added value [<scheme>] under a key with a
    # component in F or N whose path ends in Capabilities\URLAssociations.
    declared = {}
    for _, (sign, path, name, rec, kind) in sources:
        if kind == "value" and name and norm(path).endswith("\\capabilities\\urlassociations"):
            f = next((fam.get(norm(p)) or full.get(norm(p)) for p in path.split("\\")
                      if norm(p) in fam or norm(p) in full), None)
            if f:
                declared[norm(name)] = f
    keys_added ={norm(c[1]) for _, c in added if c[4] == "key"}

    def contents(key):
        """The indexes of the added entries strictly inside an added key: its values and subkeys."""
        k = norm(key)
        return [i for i, c in added if c[4] != "file" and (norm(c[1]).startswith(k + "\\") or
                                                           (c[4] == "value" and norm(c[1]) == k))]

    for i, (sign, path, name, rec, kind) in enumerate(changes):
        if i in skip or r380_never(kind, path, name, rec, worker_sids):
            continue
        if sign == "~":
            if (kind == "value" and any(norm(path) == norm(k) and norm(name or "") == norm(n)
                                        for k, n in R380_CHANGED_VALUES)) or \
                    (kind == "file" and any(norm(path) == norm(p) for p in R380_CHANGED_FILES)):
                out[i] = r380_reason(everyone, "Windows' package bookkeeping, updated by the registration")
            continue
        if sign != "+":
            continue
        if under(path, REGISTERED_APPS):  # a value (r380_never), excused by this test alone
            # The path of a package's capabilities, Repository\Packages\<full name>\...; `reg export`
            # writes each backslash of the text doubled.
            data = norm(rec) if isinstance(rec, str) else ""
            f = next((v for k, v in full.items() if f"\\\\packages\\\\{k}\\\\" in data), None)
            if f:
                out[i] = r380_reason([f], "Default Programs' entry of one of its apps (its capabilities' path)")
            continue
        hit = next((named[norm(p)] for p in path.split("\\") if norm(p) in named), None)
        # An app's AppUserModelID `<family>!<app>` as a whole component (PushNotifications\Backup).
        hit = hit or next(((fam[norm(p).partition("!")[0]], "a record under the AppUserModelID of one of its apps")
                           for p in path.split("\\") if "!" in p and norm(p).partition("!")[0] in fam), None)
        if hit:
            out[i] = r380_reason([hit[0]], hit[1])
            continue
        if kind == "value":
            if norm(name or "") in progids:
                out[i] = r380_reason([progids[norm(name)]], "a value named by its AppX ProgID")
                continue
            if (norm(path), norm(name or "")) == tuple(norm(x) for x in POST_INSTALL_MARKER):
                out[i] = r380_reason(everyone, "Explorer's marker of its post app install tasks")
                continue
        folder, _, leaf = path.rpartition("\\")
        if kind == "file" and rec.get("kind") == "reparse" and norm(folder) == norm(WINDOWS_APPS) and \
                norm(leaf) in aliases:
            out[i] = r380_reason([aliases[norm(leaf)]], f"its app execution alias {leaf}")
            continue
        if kind != "file" and norm(folder) == norm(APP_PATHS) and norm(leaf) in aliases:
            # The key App Paths\<alias> and its values (a value's path is its key).
            out[i] = r380_reason([aliases[norm(leaf)]], f"the App Paths entry of its alias {leaf}")
    # URL protocols: Classes\<scheme> holding only its two values, its UrlAssociations key added too.
    for i, (sign, path, name, rec, kind) in added:
        folder, _, scheme = path.rpartition("\\")
        if kind != "key" or norm(folder) != norm(CLASSES) or i in skip or \
                r380_never(kind, path, name, rec, worker_sids) or \
                norm(URL_ASSOCIATIONS + "\\" + scheme) not in keys_added or norm(scheme) not in declared:
            continue
        inside = contents(path)
        values = {norm(changes[j][2] or ""): changes[j][3] for j in inside if changes[j][4] == "value"
                  and norm(changes[j][1]) == norm(path)}
        if len(inside) != 2 or set(values) != {"", "url protocol"} or \
                not isinstance(values[""], str) or norm(unquote(values[""])) != norm("URL:" + scheme) or \
                any(j in skip or r380_never(*(changes[j][k] for k in (4, 1, 2, 3)), worker_sids)
                    for j in inside):
            continue
        what = f"the URL protocol {scheme} its capabilities declare"
        for j in [i] + inside:
            out[j] = r380_reason([declared[norm(scheme)]], what)
        partner = next((j for j, c in added if c[4] == "key" and
                        norm(c[1]) == norm(URL_ASSOCIATIONS + "\\" + scheme)), None)
        if partner is not None and partner not in skip and not contents(changes[partner][1]) and \
                not r380_never("key", changes[partner][1], None, True, worker_sids):
            out[partner] = r380_reason([declared[norm(scheme)]], what + ": its association key")
    # Parents: an added key all of whose added contents are excused, one at least by this rule (the
    # deepest first, so that a parent of parents sees its subkeys' verdicts).
    parents = sorted((i for i, c in added if c[4] == "key" and out[i] is None and i not in skip and
                      not r380_never(c[4], c[1], c[2], c[3], worker_sids)),
                     key=lambda i: -changes[i][1].count("\\"))
    for i in parents:
        inside = contents(changes[i][1])
        mine = [j for j in inside if out[j] is not None]
        if mine and all(out[j] is not None or reasons[j] for j in inside):
            fams = sorted({f for j in mine for f in everyone if f in out[j]}, key=norm) or everyone
            out[i] = r380_reason(fams, "a parent key holding only its records")
    return out


def show(kind, path, name, rec):
    if kind == "value":
        return f"value {path} [{name or '@'}] = {rec}"
    if kind == "key":
        return f"key {path}"
    if rec.get("kind") == "file":
        return f"file {path} ({rec['size']} bytes, mtime {rec['mtime']})"
    if rec.get("kind") == "unreadable":
        return f"unreadable {path} ({rec.get('error')})"
    return f"{rec.get('kind')} {path}"


def settings_only(after):
    """Whether %APPDATA%\\LitePack holds nothing but settings.json in `after`."""
    inside = [p for p in after["files"] if under(p, SETTINGS_DIR) and norm(p) != norm(SETTINGS_DIR)]
    return all(norm(p) == norm(SETTINGS_FILE) for p in inside)


# R408: `diff --verdict litepack` (verify-arm64 alone): a residue line is a verdict only when it is LitePack's own.
# The names that make it so (case-blind, in its path, value name or data): LitePack itself (org.litepack.gui and
# LitePack.Worker too), `lpk` (lpk.exe, .lpk, lpk_shell.dll), its shell CLSID, and the jump list's file name for its
# AppUserModelID (installer.nsh).
LITEPACK_MARKS = ("litepack", "lpk", norm(LITEPACK_CLSID), "a233dcd0c3d36a7d")
VERDICT_REASON = "no verdict with --verdict litepack"


def verdict_keys():
    """The keys LitePack must never leave anything under, whatever its name (R408): R380's rule 5 list
    (R380_NEVER_KEYS: Run, RunOnce, HKCU\\Environment), RegisteredApplications, Classes\\.lpk and Uninstall."""
    return R380_NEVER_KEYS + [REGISTERED_APPS, CLASSES + r"\.lpk", CURRENT_VERSION + r"\Uninstall"]


def expand_root(path):
    """A snapshot path with its %ROOT% token replaced by the folder it names (None if unknown)."""
    m = re.match(r"%([A-Z]+)%(.*)", path)
    if not m:
        return path
    folder = root_dir(m.group(1))
    return folder.rstrip("\\") + m.group(2) if folder else None


def litepack_own(kind, path, name, rec, run, worker_sids=()):
    """R408: whether a residue line is LitePack's own: a LitePack mark in its path, value name or data; or it lies
    in (a file) or names (any text) the run folder; or it lies under a key of `verdict_keys()`; or it is the worker
    profile's (its SID)."""
    texts = [path, name or ""] + ([rec] if kind == "value" and isinstance(rec, str) else [])
    for t in texts:
        t = norm(t)
        if any(m in t for m in LITEPACK_MARKS):
            return True
    if run:
        r = norm(str(run).replace("/", "\\").rstrip("\\"))
        # A .reg export doubles the backslashes inside string data.
        if any(r in norm(t) or r.replace("\\", "\\\\") in norm(t) for t in texts):
            return True
        if kind == "file":
            real = expand_root(path)
            if real is not None and under(real, r):
                return True
    if kind != "file" and any(under(path, k) for k in verdict_keys()):
        return True
    parts = [norm(p) for p in path.split("\\")]
    return any(p in worker_sids for p in parts)


def diff(before, after, allow_profile, run=None, allow_settings=False, allow_uninstaller=False, verdict="all"):
    """The diff's lines and the number of entries not allowed. A path unreadable in either
    snapshot is listed (and counted unless allowed) even when both record it alike: what lies
    inside it was not seen. With verdict="litepack" (R408), a line not allowed that is not LitePack's own
    (`litepack_own`) is printed as OTHER and not counted."""
    b, a = entries(before), entries(after)
    sids = profile_sids(before, after)
    changes = []
    for k in sorted(set(b) | set(a), key=lambda k: (k[0], k[1], k[2] or "")):
        if k not in b:
            changes.append(("+",) + a[k] + (k[0],))
        elif k not in a:
            changes.append(("-",) + b[k] + (k[0],))
        elif a[k][2] != b[k][2]:
            changes.append(("~",) + a[k] + (k[0],))
        elif k[0] == "file" and a[k][2].get("kind") == "unreadable":
            changes.append(("?",) + a[k] + (k[0],))
    # Keys that exist only as the profile's ancestors (created for it, holding no value of their
    # own that changed) belong to it too.
    profile_keys = [c[1] for c in changes if c[4] != "file" and is_profile(c[4], c[1], sids)]
    valued = {norm(c[1]) for c in changes if c[4] == "value"}
    unreadable = {norm(p) for s in (before, after) for p, r in s["files"].items()
                  if r.get("kind") == "unreadable"}
    reasons, unreads = [], set()
    for i, (sign, path, name, rec, kind) in enumerate(changes):
        unread = kind == "file" and norm(path) in unreadable
        if unread:
            unreads.add(i)
        reason = allowed(kind, path, name, sign, after, run, unread)
        if not reason and allow_settings and not unread and kind == "file" and sign == "+" and \
                norm(path) in (norm(SETTINGS_DIR), norm(SETTINGS_FILE)) and settings_only(after):
            reason = SETTINGS_REASON
        if not reason and allow_uninstaller and not unread and kind == "file" and sign == "+" and \
                uninstaller_copy(path, after):
            reason = UNINSTALLER_REASON
        reasons.append(reason)
    # R380 sees every entry at once: its sets of names depend on each other.
    package_reasons = packages(changes, reasons, unreads, sids)
    lines, bad = [], 0
    for i, (sign, path, name, rec, kind) in enumerate(changes):
        text = f"{sign} {show(kind, path, name, rec)}"
        unread = i in unreads
        reason = reasons[i] or package_reasons[i]
        ancestor = kind == "key" and sign in "+-" and norm(path) not in valued and \
            any(under(p, path) and norm(p) != norm(path) for p in profile_keys)
        if reason:
            lines.append(f"allowed: {text}  -- {reason}")
        elif allow_profile and (is_profile(kind, path, sids) or ancestor) and \
                not unread:
            lines.append(f"profile: {text}")
        elif verdict == "litepack" and not litepack_own(kind, path, name, rec, run, sids):
            lines.append(f"OTHER: {text}")
        else:
            lines.append(f"RESIDUE: {text}")
            bad += 1
    return lines, bad


def profile_present(lines):
    """What the worker's profile lacks in a diff's lines: both its folder under
    %LOCALAPPDATA%\\Packages and its Mappings key must be added. Empty when both are."""
    folder = norm(f"profile: + dir %LOCALAPPDATA%\\Packages\\{WORKER_MONIKER}")
    mapping = norm(f"profile: + key {MAPPINGS}\\S-1-15-2-")
    missing = []
    if not any(norm(line) == folder for line in lines):
        missing.append(f"no added folder %LOCALAPPDATA%\\Packages\\{WORKER_MONIKER}")
    if not any(norm(line).startswith(mapping) and "\\" not in line[len(mapping):]
               for line in lines):
        missing.append(f"no added key {MAPPINGS}\\S-1-15-2-...")
    return missing


def main(argv):
    if argv[:1] == ["--self-test"]:
        r = unittest.main(argv=[sys.argv[0]], module=__name__, exit=False, verbosity=2).result
        return 0 if r.wasSuccessful() else 1
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    sub = ap.add_subparsers(dest="cmd", required=True)
    s = sub.add_parser("snapshot")
    s.add_argument("out")
    s.add_argument("--on-ci", action="store_true",
                   help="this is a disposable machine (implied by GITHUB_ACTIONS=true)")
    d = sub.add_parser("diff")
    d.add_argument("before")
    d.add_argument("after")
    d.add_argument("--allow-worker-profile", action="store_true")
    d.add_argument("--run-folder",
                   help="the folder the run's lpk-gui.exe lies in (default: "
                        "RUNNER_TEMP\\portable-run\\LitePack when RUNNER_TEMP is set)")
    d.add_argument("--require-worker-profile", action="store_true",
                   help="allow the profile, and fail unless its folder and Mappings key were added")
    d.add_argument("--allow-settings", action="store_true",
                   help=r"allow %%APPDATA%%\LitePack\settings.json alone (an uninstall without the box)")
    d.add_argument("--allow-uninstaller-copy", action="store_true",
                   help=r"R312: allow %%LOCALAPPDATA%%\Temp\~nsu*.tmp holding one Un.exe alone")
    d.add_argument("--verdict", choices=("all", "litepack"), default="all",
                   help="R408: `litepack` makes only LitePack's own residue a verdict (release.yml's verify-arm64); "
                        "the rest is printed as OTHER")
    a = ap.parse_args(argv)
    if a.cmd == "snapshot":
        snapshot(a.out, a.on_ci)
        return 0
    load = lambda p: json.loads(Path(p).read_text(encoding="utf-8"))  # noqa: E731
    allow = a.allow_worker_profile or a.require_worker_profile
    run = a.run_folder
    if run is None and os.environ.get("RUNNER_TEMP"):
        run = os.path.join(os.environ["RUNNER_TEMP"], "portable-run", "LitePack")
    lines, bad = diff(load(a.before), load(a.after), allow, run, a.allow_settings,
                      a.allow_uninstaller_copy, a.verdict)
    for line in lines:
        print(line)
    flag = " (worker profile allowed)" if allow else ""
    if a.verdict == "litepack":
        other = sum(1 for line in lines if line.startswith("OTHER: "))
        print(f"{len(lines)} differences{flag}, {bad + other} not allowed: {bad} LitePack's, {other} other "
              f"({VERDICT_REASON})")
    else:
        print(f"{len(lines)} differences{flag}, {bad} not allowed")
    missing = profile_present(lines) if a.require_worker_profile else []
    for m in missing:
        print(f"PROFILE MISSING: {m}")
    return 1 if bad or missing else 0


# --- tests (synthetic snapshots only, R294) --------------------------------------------------


SID = "S-1-15-2-1-2-3-4-5-6-7"


def snap(files=None, registry=None):
    return {"files": files or {}, "registry": registry or {}}


def f(size=1, mtime=1):
    return {"kind": "file", "size": size, "mtime": mtime}


def with_profile(s):
    files = dict(s["files"])
    files[r"%LOCALAPPDATA%\Packages\litepack.worker"] = {"kind": "dir"}
    files[r"%LOCALAPPDATA%\Packages\litepack.worker\AC\Temp"] = {"kind": "dir"}
    reg = dict(s["registry"])
    reg[MAPPINGS + "\\" + SID] = {"DisplayName": '"LitePack.Worker"',
                                   "Moniker": '"LitePack.Worker"'}
    reg[MAPPINGS + "\\" + SID + r"\Children"] = {}
    # A parent created for the profile alone, with no value of its own.
    reg[STORAGE] = {}
    reg[STORAGE + r"\litepack.worker"] = {"x": "dword:00000001"}
    return snap(files, reg)


class Tests(unittest.TestCase):
    base = snap({r"%APPDATA%\a.txt": f()}, {r"HKCU\Software\X": {"v": '"1"'}, MAPPINGS: {}})

    def test_no_change_is_clean(self):
        self.assertEqual(diff(self.base, self.base, False), ([], 0))

    def test_an_added_file_is_residue(self):
        after = snap(dict(self.base["files"], **{r"%APPDATA%\LitePack\settings.json": f()}),
                     self.base["registry"])
        lines, bad = diff(self.base, after, True)
        self.assertEqual(bad, 1)
        self.assertEqual(lines, [r"RESIDUE: + file %APPDATA%\LitePack\settings.json "
                                 "(1 bytes, mtime 1)"])

    def test_the_settings_file_alone_is_allowed_with_the_flag(self):
        files = dict(self.base["files"], **{SETTINGS_DIR: {"kind": "dir"}, SETTINGS_FILE: f()})
        after = snap(files, self.base["registry"])
        lines, bad = diff(self.base, after, False, allow_settings=True)
        self.assertEqual(bad, 0, lines)
        self.assertEqual(len(lines), 2)
        self.assertTrue(all(line.startswith("allowed: + ") for line in lines), lines)
        self.assertEqual(diff(self.base, after, False)[1], 2)
        # Anything else in the folder, and the folder then, is residue; so is a changed file.
        more = dict(files, **{SETTINGS_DIR + r"\recent.json": f()})
        lines, bad = diff(self.base, snap(more, self.base["registry"]), False, allow_settings=True)
        self.assertEqual(bad, 3, lines)
        before = snap(files, self.base["registry"])
        changed = snap(dict(files, **{SETTINGS_FILE: f(size=2)}), self.base["registry"])
        self.assertEqual(diff(before, changed, False, allow_settings=True)[1], 1)

    def test_a_profile_path_unreadable_before_is_never_excused(self):
        # R317: a change inside the profile whose earlier side was unreadable is residue, even
        # with the profile allowed, since what lay inside it was not seen.
        ac = r"%LOCALAPPDATA%\Packages\litepack.worker\AC"
        before = with_profile(self.base)
        before["files"][ac] = {"kind": "unreadable", "error": "PermissionError"}
        after = with_profile(self.base)
        after["files"][ac] = {"kind": "dir"}
        lines, bad = diff(before, after, True)
        self.assertEqual(bad, 1, lines)
        self.assertIn("RESIDUE: ~ dir " + ac, lines)

    def test_the_uninstaller_copy_alone_is_allowed_with_the_flag(self):
        folder = r"%LOCALAPPDATA%\Temp\~nsu1.tmp"
        files = dict(self.base["files"], **{folder: {"kind": "dir"}, folder + r"\Un.exe": f()})
        after = snap(files, self.base["registry"])
        lines, bad = diff(self.base, after, False, allow_uninstaller=True)
        self.assertEqual(bad, 0, lines)
        self.assertEqual(diff(self.base, after, False)[1], 2)
        # Another file beside it, or another name, is residue.
        more = snap(dict(files, **{folder + r"\x.dll": f()}), self.base["registry"])
        self.assertEqual(diff(self.base, more, False, allow_uninstaller=True)[1], 3)
        other = snap(dict(self.base["files"], **{r"%LOCALAPPDATA%\Temp\other.tmp": {"kind": "dir"},
                                                 r"%LOCALAPPDATA%\Temp\other.tmp\Un.exe": f()}),
                     self.base["registry"])
        self.assertEqual(diff(self.base, other, False, allow_uninstaller=True)[1], 2)

    def test_userassist_of_the_run_folder_is_matched_in_rot13(self):
        run = r"C:\Users\u\AppData\Local\LitePack"
        name = rot13(run) + r"\ycx-thv.rkr"
        before = snap(self.base["files"], dict(self.base["registry"], **{USERASSIST_COUNT: {}}))
        after = snap(self.base["files"],
                     dict(self.base["registry"], **{USERASSIST_COUNT: {name: "hex:00"}}))
        self.assertEqual(diff(before, after, False, run)[1], 0)
        self.assertEqual(diff(before, after, False)[1], 1)
        self.assertEqual(diff(before, after, False, r"C:\elsewhere")[1], 1)

    def test_the_start_tile_cache_is_matched_by_its_shape(self):
        key = (r"HKCU\Software\Microsoft\Windows\CurrentVersion\CloudStore\Store\Cache\DefaultAccount"
               r"\$de${e7cbac1d-30b4-408f-a41d-5714e42e257a}$$windows.data.unifiedtile"
               r".localstarttilepropertiesmap\Current")
        before = snap(self.base["files"], dict(self.base["registry"], **{key: {"Data": "hex:01"}}))
        after = snap(self.base["files"], dict(self.base["registry"], **{key: {"Data": "hex:02"}}))
        self.assertEqual(diff(before, after, False)[1], 0)
        other = snap(self.base["files"], dict(self.base["registry"], **{key: {"Data": "hex:01", "X": "1"}}))
        self.assertEqual(diff(before, other, False)[1], 1)
        bent = key.replace("unifiedtile", "unifiedtileX")
        moved = snap(self.base["files"], dict(self.base["registry"], **{bent: {"Data": "hex:02"}}))
        self.assertEqual(diff(snap(self.base["files"], dict(self.base["registry"], **{bent: {"Data": "1"}})),
                              moved, False)[1], 1)

    def test_a_desktop_shortcut_is_residue(self):
        before = snap(dict(self.base["files"], **{"%DESKTOP%": {"kind": "absent"}}),
                      self.base["registry"])
        after = snap(dict(self.base["files"], **{r"%DESKTOP%\LitePack.lnk": f()}),
                     self.base["registry"])
        lines, bad = diff(before, after, True, allow_settings=True)
        self.assertEqual(bad, 2, lines)

    def test_a_removed_and_a_changed_file(self):
        after = snap({r"%LOCALAPPDATA%\b": f()}, self.base["registry"])
        before = snap(dict(self.base["files"], **{r"%LOCALAPPDATA%\b": f(mtime=0)}),
                      self.base["registry"])
        lines, bad = diff(before, after, False)
        self.assertEqual(bad, 2)
        self.assertTrue(lines[0].startswith(r"RESIDUE: - file %APPDATA%\a.txt"))
        self.assertTrue(lines[1].startswith(r"RESIDUE: ~ file %LOCALAPPDATA%\b"))

    def test_a_changed_value_is_residue(self):
        after = snap(self.base["files"], dict(self.base["registry"],
                                              **{r"HKCU\Software\X": {"v": '"2"'}}))
        lines, bad = diff(self.base, after, True)
        self.assertEqual((lines, bad), ([r'RESIDUE: ~ value HKCU\Software\X [v] = "2"'], 1))

    def test_an_allow_listed_entry_is_allowed_with_its_reason(self):
        global ALLOW
        saved = ALLOW
        ALLOW = [{"kind": "value", "path": r"HKCU\Software\X", "name": "V",
                  "reason": "a test's reason"},
                 {"kind": "file", "path": r"%LOCALAPPDATA%\Cache", "reason": "cache"}]
        try:
            after = snap(dict(self.base["files"], **{r"%LOCALAPPDATA%\cache\c\d": f()}),
                         {r"HKCU\Software\x": {"v": '"2"'}, MAPPINGS: {}})
            # The key's and the value's names differ in case only: the same key and value.
            before = snap(self.base["files"], {r"HKCU\Software\X": {"V": '"1"'}, MAPPINGS: {}})
            lines, bad = diff(before, after, False)
            self.assertEqual(bad, 0, lines)
            self.assertEqual(len(lines), 2, lines)
            self.assertIn("-- a test's reason", lines[1])
            self.assertIn("-- cache", lines[0])
            # A sibling of the allowed folder is not allowed.
            after["files"][r"%LOCALAPPDATA%\Cache2"] = f()
            self.assertEqual(diff(before, after, False)[1], 1)
        finally:
            ALLOW = saved

    def added_keys(self, keys, run=None):
        after = snap(self.base["files"], dict(self.base["registry"], **{k: v for k, v in keys}))
        return diff(self.base, after, False, run)

    def test_spotlight_allows_its_four_values_only(self):
        # As on CI: the keys exist; Maintenance is added, State, PCT and PTT change.
        other_lu = SPOTLIGHT_LU.replace("Global.DesktopSpotlight", "Other")
        reg = dict(self.base["registry"], **{SPOTLIGHT: {"State": '"s0"'},
                                             SPOTLIGHT_LU: {"PCT": "hex(b):00", "PTT": "hex(b):00"},
                                             other_lu: {"PCT": "hex(b):00"}})
        before = snap(self.base["files"], reg)

        def after(spot, lu, other=None):
            return snap(self.base["files"], dict(reg, **{SPOTLIGHT: spot, SPOTLIGHT_LU: lu,
                                                         other_lu: other or {"PCT": "hex(b):00"}}))
        changed = {"PCT": "hex(b):01", "PTT": "hex(b):02"}
        lines, bad = diff(before, after({"Maintenance": '"m"', "State": '"s1"'}, changed), False)
        self.assertEqual(bad, 0, lines)
        # Another value there, or the same name in another key, is residue.
        for case in (after({"State": '"s0"', "Other": '"x"'}, {"PCT": "hex(b):00", "PTT": "hex(b):00"}),
                     after({"State": '"s0"'}, {"PCT": "hex(b):00", "PTT": "hex(b):00"}, {"PCT": "hex(b):01"})):
            lines, bad = diff(before, case, False)
            self.assertEqual(bad, 1, lines)

    def test_a_webview2_key_below_its_root_is_allowed_and_a_sibling_is_not(self):
        root = r"HKCU\Software\Microsoft\EdgeWebView"
        lines, bad = self.added_keys([(root + r"\BLBeacon", {"version": '"1"'})])
        self.assertEqual(bad, 0, lines)
        for sibling in (r"HKCU\Software\Microsoft\EdgeWebViewX", r"HKCU\Software\Microsoft\Edge",
                        r"HKCU\Software\Microsoft\EdgeUpdate\ClientState\{0000}"):
            lines, bad = self.added_keys([(sibling, {"v": '"1"'})])
            self.assertEqual(bad, 2, lines)

    def test_file_patterns_hold_to_their_folder_and_shape(self):
        cache = r"%LOCALAPPDATA%\Microsoft\TokenBroker\Cache"
        ok = cache + "\\" + "a" * 40 + ".tbres"
        after = snap(dict(self.base["files"], **{ok: f()}), self.base["registry"])
        self.assertEqual(diff(self.base, after, False)[1], 0)
        for bad_path in (cache + r"\settings.json", cache + "\\sub\\" + "a" * 40 + ".tbres",
                         r"%LOCALAPPDATA%\Microsoft\Credentials\litepack"):
            after = snap(dict(self.base["files"], **{bad_path: f()}), self.base["registry"])
            self.assertEqual(diff(self.base, after, False)[1], 1, bad_path)

    def test_the_classes_hive_files_are_allowed_changed_only(self):
        win = r"%LOCALAPPDATA%\Microsoft\Windows"
        hive = [win + "\\" + n for n in ("UsrClass.dat", "UsrClass.dat.LOG1", "UsrClass.dat.LOG2")]
        before = snap(dict(self.base["files"], **{h: f() for h in hive}), self.base["registry"])
        changed = snap(dict(self.base["files"], **{h: f(size=2) for h in hive}), self.base["registry"])
        self.assertEqual(diff(before, changed, False)[1], 0)
        # Added or removed, they are residue; so is a sibling.
        self.assertEqual(diff(self.base, before, False)[1], 3)
        self.assertEqual(diff(before, self.base, False)[1], 3)
        sib = win + r"\UsrClass2.dat"
        before = snap(dict(self.base["files"], **{sib: f()}), self.base["registry"])
        changed = snap(dict(self.base["files"], **{sib: f(size=2)}), self.base["registry"])
        self.assertEqual(diff(before, changed, False)[1], 1)

    def test_the_muicache_allows_windows_dll_strings_in_any_hex_subkey_only(self):
        key = LOCAL_SETTINGS + r"\MuiCache\1a7\52C64B7E"
        before = snap(self.base["files"], dict(self.base["registry"], **{key: {"seed": '"s"'}}))

        def bad_of(k, names):
            reg = dict(self.base["registry"], **{key: {"seed": '"s"'}})
            reg[k] = dict(reg.get(k, {}), **names)
            return diff(before, snap(self.base["files"], reg), False, "D:/a/_temp/portable-run/LitePack")
        urlmon = {r"@%SystemRoot%\System32\urlmon.dll,-4200": '"Open File - Security Warning"',
                  r"C:\Windows\system32,@elscore.dll,-7": '"x"'}
        lines, bad = bad_of(key, urlmon)
        self.assertEqual(bad, 0, lines)
        # The same name under an unrelated key, or naming another folder's DLL, is residue.
        other = LOCAL_SETTINGS + r"\Other\1a7\52C64B7E"
        self.assertEqual(bad_of(other, urlmon)[1], 2 + 1)
        for name in (r"@D:\a\_temp\portable-run\LitePack\x.dll,-1", r"C:\Windows\system32\sub,@x.dll,-1",
                     r"@%SystemRoot%\System32\sub\x.dll,-1", r"C:\a\LitePack,@lpk.dll,-1"):
            lines, bad = bad_of(key, {name: '"x"'})
            self.assertEqual(bad, 1, lines)

    def test_explorers_icon_cache_is_allowed_changed_only(self):
        ex = r"%LOCALAPPDATA%\Microsoft\Windows\Explorer"
        idx, other = ex + r"\iconcache_idx.db", ex + r"\other.db"
        before = snap(dict(self.base["files"], **{idx: f(), other: f()}), self.base["registry"])
        changed = snap(dict(self.base["files"], **{idx: f(size=2), other: f()}), self.base["registry"])
        self.assertEqual(diff(before, changed, False)[1], 0)
        added = snap(dict(before["files"], **{ex + r"\iconcache_99.db": f()}), self.base["registry"])
        self.assertEqual(diff(before, added, False)[1], 1)
        changed = snap(dict(self.base["files"], **{idx: f(), other: f(size=2)}), self.base["registry"])
        self.assertEqual(diff(before, changed, False)[1], 1)

    def test_the_muicache_allows_lpk_gui_names_in_the_run_folder_only(self):
        mui = SHELL_MUICACHE
        run = "D:/a/_temp/portable-run/LitePack"
        names = {r"D:\a\_temp\portable-run\LitePack\lpk-gui.exe.FriendlyAppName": '"L"',
                 r"D:\a\_temp\portable-run\LitePack\lpk-gui.exe.ApplicationCompany": '"C"',
                 "LangID": "hex:09,04"}
        lines, bad = self.added_keys([(mui, names)], run)
        self.assertEqual(bad, 0, lines)
        # Without a run folder, or under another folder, the same names are residue.
        self.assertEqual(self.added_keys([(mui, names)])[1], 2)
        for other in (r"D:\x\LitePack", r"D:\a\_temp\portable-run\LitePack\sub",
                      r"D:\a\_temp\portable-run\LitePackX"):
            lines, bad = self.added_keys([(mui, {other + r"\lpk-gui.exe.FriendlyAppName": '"L"'})],
                                         run)
            self.assertEqual(bad, 1, lines)
        lines, bad = self.added_keys([(mui, {r"D:\x\other.exe.FriendlyAppName": '"O"'})], run)
        self.assertEqual(bad, 1, lines)

    def test_no_allow_list_entry_excuses_an_unreadable_path(self):
        cache = r"%LOCALAPPDATA%\Microsoft\TokenBroker\Cache"
        locked = {cache + "\\" + "a" * 40 + ".tbres": {"kind": "unreadable", "error": "OSError"}}
        after = snap(dict(self.base["files"], **locked), self.base["registry"])
        lines, bad = diff(self.base, after, False)
        self.assertEqual(bad, 1, lines)
        self.assertTrue(lines[0].startswith("RESIDUE: + unreadable "), lines)
        # Readable before, unreadable after: still residue.
        before = snap(dict(self.base["files"], **{p: f() for p in locked}), self.base["registry"])
        self.assertEqual(diff(before, after, False)[1], 1)

    def test_the_profile_needs_the_flag(self):
        after = with_profile(self.base)
        lines, bad = diff(self.base, after, False)
        self.assertEqual(bad, len(lines))
        self.assertEqual(bad, 9)
        lines, bad = diff(self.base, after, True)
        self.assertEqual(bad, 0, lines)
        self.assertTrue(all(line.startswith("profile: ") for line in lines))
        self.assertEqual(profile_present(lines), [])

    def test_a_parent_key_made_for_the_profile_needs_the_flag(self):
        after = with_profile(self.base)
        parent = f"+ key {STORAGE}"
        self.assertIn(f"RESIDUE: {parent}", diff(self.base, after, False)[0])
        self.assertIn(f"profile: {parent}", diff(self.base, after, True)[0])

    def test_a_parent_key_with_a_value_of_its_own_is_residue(self):
        after = with_profile(self.base)
        after["registry"][STORAGE] = {"own": '"x"'}
        lines, bad = diff(self.base, after, True)
        self.assertEqual(bad, 2, lines)
        self.assertIn(f"RESIDUE: + key {STORAGE}", lines)
        self.assertIn(f'RESIDUE: + value {STORAGE} [own] = "x"', lines)

    def test_empty_appcontainer_parents_left_by_windows_are_allowed(self):
        # R301: after --remove-profile, the parents alone, empty.
        after = snap(self.base["files"], dict(self.base["registry"], **{APPCONTAINER: {},
                                                                        STORAGE: {}}))
        lines, bad = diff(self.base, after, False)
        self.assertEqual(bad, 0, lines)
        self.assertTrue(all(line.startswith("allowed: + key ") for line in lines), lines)
        # Anything left under one is residue, and the parent is no longer empty.
        after["registry"][STORAGE + r"\litepack.worker"] = {}
        lines, bad = diff(self.base, after, False)
        self.assertIn(f"RESIDUE: + key {STORAGE}", lines)
        self.assertIn(f"RESIDUE: + key {STORAGE}\\litepack.worker", lines)
        # Its parent AppContainer holds it, so it is not empty either.
        self.assertEqual(bad, 3, lines)
        # A value in the parent itself too.
        after = snap(self.base["files"], dict(self.base["registry"], **{STORAGE: {"v": '"1"'}}))
        self.assertEqual(diff(self.base, after, False)[1], 2)

    def test_the_profile_requirement_needs_its_folder_and_its_mapping(self):
        after = with_profile(self.base)
        del after["registry"][MAPPINGS + "\\" + SID]
        del after["registry"][MAPPINGS + "\\" + SID + r"\Children"]
        missing = profile_present(diff(self.base, after, True)[0])
        self.assertEqual(len(missing), 1, missing)
        self.assertIn("Mappings", missing[0])
        files_gone = with_profile(self.base)
        files_gone["files"] = dict(self.base["files"])
        missing = profile_present(diff(self.base, files_gone, True)[0])
        self.assertEqual(len(missing), 1, missing)
        self.assertIn("Packages", missing[0])

    def test_the_moniker_value_is_matched_without_case(self):
        after = with_profile(self.base)
        after["registry"][MAPPINGS + "\\" + SID] = {"moniker": '"litepack.WORKER"'}
        lines, bad = diff(self.base, after, True)
        self.assertEqual(bad, 0, lines)

    def test_an_unreadable_path_is_reported_even_unchanged(self):
        s = snap(dict(self.base["files"], **{r"%LOCALAPPDATA%\locked": {
            "kind": "unreadable", "error": "PermissionError"}}), self.base["registry"])
        lines, bad = diff(s, s, False)
        self.assertEqual(lines, [r"RESIDUE: ? unreadable %LOCALAPPDATA%\locked (PermissionError)"])
        self.assertEqual(bad, 1)
        # Inside the profile's folder too, with the flag.
        s["files"] = {r"%LOCALAPPDATA%\Packages\litepack.worker\x": {"kind": "unreadable",
                                                                    "error": "OSError"}}
        self.assertEqual(diff(snap(registry=s["registry"]), s, True)[1], 1)

    def test_paths_get_the_long_path_prefix(self):
        if os.name != "nt":
            self.skipTest("Windows paths")
        self.assertEqual(long_path("C:\\a\\b"), "\\\\?\\C:\\a\\b")
        self.assertEqual(long_path("\\\\?\\C:\\a"), "\\\\?\\C:\\a")
        self.assertEqual(long_path("\\\\srv\\share\\a"), "\\\\?\\UNC\\srv\\share\\a")

    def test_a_reparse_point_is_recorded_and_not_followed(self):
        if os.name != "nt":
            self.skipTest("Windows reparse points")
        with tempfile.TemporaryDirectory() as d:
            target = Path(d) / "target"
            target.mkdir()
            (target / "inside").write_bytes(b"x")
            link = Path(d) / "link"
            try:
                os.symlink(target, link, target_is_directory=True)
            except OSError:
                self.skipTest("no symlink privilege")
            out = {}
            walk(d, "T", out)
        self.assertEqual(out["%T%\\link"]["kind"], "reparse")
        self.assertNotIn("%T%\\link\\inside", out)
        self.assertEqual(out["%T%\\target\\inside"]["kind"], "file")

    def test_snapshot_refuses_off_ci(self):
        env = {k: v for k, v in os.environ.items() if k != CI_ENV}
        with tempfile.TemporaryDirectory() as d, \
                unittest.mock.patch.dict(os.environ, env, clear=True):
            with self.assertRaises(SystemExit):
                snapshot(Path(d) / "s.json", on_ci=False)
            self.assertEqual(list(Path(d).iterdir()), [])

    def test_the_removed_profile_is_matched_too(self):
        lines, bad = diff(with_profile(self.base), self.base, True)
        self.assertEqual(bad, 0, lines)
        self.assertTrue(all(line.startswith("profile: - ") for line in lines))

    def test_another_appcontainer_is_never_the_profile(self):
        after = snap(dict(self.base["files"], **{r"%LOCALAPPDATA%\Packages\other.app": {"kind": "dir"}}),
                     dict(self.base["registry"], **{MAPPINGS + r"\S-1-15-2-9": {"Moniker": '"other.app"'}}))
        lines, bad = diff(self.base, after, True)
        self.assertEqual(bad, 3, lines)

    def test_a_reg_export_in_utf16_is_parsed(self):
        text = ("Windows Registry Editor Version 5.00\r\n\r\n"
                "[HKEY_CURRENT_USER\\Software]\r\n\r\n"
                "[HKEY_CURRENT_USER\\Software\\A b]\r\n"
                '@="default"\r\n'
                '"Na\\"me"="va\\\\l"\r\n'
                '"D"=dword:0000002a\r\n'
                '"Bin"=hex:00,01,\\\r\n  02,03\r\n'
                '"Multi"="line one\r\nline two"\r\n'
                "\r\n[HKEY_CURRENT_USER\\Environment]\r\n"
                '"Path"=hex(2):25,00,\\\r\n  00,00\r\n')
        with tempfile.TemporaryDirectory() as d:
            p = Path(d) / "x.reg"
            p.write_bytes(b"\xff\xfe" + text.encode("utf-16-le"))
            keys = read_reg_file(p)
        self.assertEqual(keys[r"HKCU\Software"], {})
        self.assertEqual(keys[r"HKCU\Software\A b"], {
            "": '"default"', 'Na"me': '"va\\\\l"', "D": "dword:0000002a",
            "Bin": "hex:00,01,02,03", "Multi": '"line one\nline two"'})
        self.assertEqual(keys[r"HKCU\Environment"], {"Path": "hex(2):25,00,00,00"})

    def test_a_snapshot_round_trips_through_json(self):
        s = with_profile(self.base)
        self.assertEqual(diff(s, json.loads(json.dumps(s)), False), ([], 0))

    # R365: CI run 43676006117's 26 lines, as (sign, kind, path, value name).
    R365_TEMP = r"C:\Users\runneradmin\AppData\Local\Temp"
    R365_JUMP = (r"%APPDATA%\Microsoft\Windows\Recent\CustomDestinations"
                 r"\2a2e0412b8ad04a2.customDestinations-ms")
    R365_PS = r"%LOCALAPPDATA%\Microsoft\PowerShell\StartupProfileData-NonInteractive"
    R365_TOKEN = IDENTITY_CRL + r"\Token\{67082621-8D18-4333-9C64-10DE93676363}"
    R365_DOWNLOAD = r"%LOCALAPPDATA%\Temp\LitePack-0.0.2-updater-5EsJTL"

    def r365(self, sign, kind, path, name=None):
        with unittest.mock.patch.object(sys.modules[__name__], "temp_folder", lambda: self.R365_TEMP):
            return allowed(kind, path, name, sign, snap())

    def test_r365_the_webview2_jump_list_is_the_crc_of_its_app_id(self):
        def crc64(data):
            c = (1 << 64) - 1
            for b in data:
                c ^= b
                for _ in range(8):
                    c = (c >> 1) ^ 0x92C64265D32139A4 if c & 1 else c >> 1
            return c
        self.assertEqual(f"{crc64('MICROSOFT.WINDOWS.EXPLORER'.encode('utf-16-le')):016x}",
                         "f01b4d95cf55d32a")
        self.assertEqual(f"{crc64(WEBVIEW2_AUMID.upper().encode('utf-16-le')):016x}",
                         WEBVIEW2_JUMP_LIST)

    def test_r365_the_runs_windows_and_webview2_lines_are_allowed(self):
        mui = self.R365_TEMP + (r"\LitePack-0.0.2-updater-5EsJTL\LitePack-0.0.2-installer.exe"
                                ".FriendlyAppName")
        lines = [
            ("+", "file", self.R365_JUMP),
            ("+", "file", r"%LOCALAPPDATA%\D3DSCache\af4dc2d2daeb611a"),
            ("+", "file", r"%LOCALAPPDATA%\D3DSCache\af4dc2d2daeb611a"
                          r"\F4EB2D6C-ED2B-4BDD-AD9D-F913287E6768.dxcache"),
            ("~", "file", self.R365_PS),
            ("+", "key", self.R365_TOKEN),
            ("+", "key", CONSENT_STORE + r"\location"),
            ("+", "key", CONSENT_STORE + r"\location\NonPackaged"),
            ("+", "key", CONSENT_STORE + r"\microphone\NonPackaged"),
            ("+", "key", CONSENT_STORE + r"\webcam\NonPackaged"),
            ("+", "value", SHELL_MUICACHE, mui),
            ("~", "value", IDENTITY_CRL + r"\Property", "0018801AC06FBBF3"),
            ("+", "value", self.R365_TOKEN, "ApplicationFlags"),
            ("+", "value", self.R365_TOKEN, "DeviceId"),
            ("+", "value", self.R365_TOKEN, "DeviceTicket"),
        ] + [("+", "value", CONSENT_STORE + "\\" + k, n)
             for k in (r"location\NonPackaged", "microphone", r"microphone\NonPackaged", "webcam",
                       r"webcam\NonPackaged")
             for n in ("LastSetTime", "Value")]
        self.assertEqual(len(lines), 24)
        for line in lines:
            self.assertIsNotNone(self.r365(*line), line)
        # The other two are LitePack's own download, which the product removes: still residue.
        self.assertIsNone(self.r365("+", "file", self.R365_DOWNLOAD))
        self.assertIsNone(self.r365("+", "file", self.R365_DOWNLOAD + r"\LitePack-0.0.2-installer.exe"))

    def test_r365_near_misses_stay_residue(self):
        custom = r"%APPDATA%\Microsoft\Windows\Recent\CustomDestinations"
        mui = r"\LitePack-0.0.2-updater-5EsJTL\LitePack-0.0.2-installer.exe.FriendlyAppName"
        misses = [
            ("+", "file", custom + r"\a233dcd0c3d36a7d.customDestinations-ms"),
            ("-", "file", self.R365_JUMP),
            ("+", "file", r"%LOCALAPPDATA%\D3DSCacheX\a"),
            ("~", "value", CONSENT_STORE + r"\location\NonPackaged", "Value"),
            ("-", "value", CONSENT_STORE + r"\webcam", "Value"),
            ("-", "key", CONSENT_STORE + r"\webcam\NonPackaged"),
            ("+", "value", CONSENT_STORE + r"\location\NonPackaged", "Other"),
            ("+", "value", CONSENT_STORE + r"\contacts", "Value"),
            ("+", "value", CONSENT_STORE + r"\contacts\NonPackaged", "LastSetTime"),
            ("+", "key", CONSENT_STORE + r"\webcam\NonPackaged\app"),
            ("~", "value", IDENTITY_CRL + r"\Property", "0018801AC06FBBF"),
            ("~", "value", IDENTITY_CRL + r"\Property", "DeviceId"),
            ("+", "key", self.R365_TOKEN + r"\Sub"),
            ("+", "value", IDENTITY_CRL + r"\Token\not-a-guid", "DeviceId"),
            ("+", "value", SHELL_MUICACHE, self.R365_TEMP + r"\other.exe.FriendlyAppName"),
            ("+", "value", SHELL_MUICACHE,
             self.R365_TEMP + r"\LitePack-0.0.2-updater-5EsJTL\other.exe.FriendlyAppName"),
            ("+", "value", SHELL_MUICACHE, r"D:\x" + mui),
            ("+", "value", SHELL_MUICACHE, self.R365_TEMP + r"\sub" + mui),
            ("+", "file", self.R365_PS),
            ("-", "file", self.R365_PS),
        ]
        for line in misses:
            self.assertIsNone(self.r365(*line), line)
        # With no temp folder known, the MuiCache entry matches nothing.
        with unittest.mock.patch.object(sys.modules[__name__], "temp_folder", lambda: None):
            self.assertIsNone(allowed("value", SHELL_MUICACHE, self.R365_TEMP + mui, "+", snap()))

    def test_r388_the_two_empty_capability_keys_alone_are_allowed(self):
        cap = CONSENT_STORE.rsplit("\\", 1)[0]
        self.assertEqual(cap, r"HKCU\Software\Microsoft\Windows\CurrentVersion\CapabilityAccessManager")
        for key in (cap, CONSENT_STORE):
            self.assertIn("R388", self.r365("+", "key", key) or "", key)
        misses = [
            ("+", "value", cap, "Value"),
            ("+", "value", CONSENT_STORE, "Value"),
            ("+", "value", CONSENT_STORE, ""),
            ("~", "value", CONSENT_STORE, "LastSetTime"),
            ("+", "key", CONSENT_STORE + r"\broadFileSystemAccess"),
            ("+", "key", cap + r"\Other"),
            ("-", "key", cap),
            ("-", "key", CONSENT_STORE),
        ]
        for line in misses:
            self.assertIsNone(self.r365(*line), line)

    def test_r367_windows_search_record_of_the_webview2_jump_list(self):
        # CI run 43683451272's one line, as printed, and the same value changed.
        for sign in "+~":
            self.assertIsNotNone(allowed("value", SEARCH_JUMPLIST_DATA, "MSEdgeWebView.EBWebView.Default",
                                         sign, snap()))
        self.assertEqual(WEBVIEW2_AUMID, "MSEdgeWebView.EBWebView.Default".upper())
        misses = [
            ("-", SEARCH_JUMPLIST_DATA, "MSEdgeWebView.EBWebView.Default"),
            # LitePack's own record, which the uninstaller deletes.
            ("+", SEARCH_JUMPLIST_DATA, "org.litepack.gui"),
            ("~", SEARCH_JUMPLIST_DATA, "org.litepack.gui"),
            ("+", SEARCH_JUMPLIST_DATA, "MSEdgeWebView.EBWebView.Profile 1"),
            ("+", SEARCH_JUMPLIST_DATA, "MSEdgeWebView.EBWebView.Default.x"),
            ("+", SEARCH_JUMPLIST_DATA + r"\Sub", "MSEdgeWebView.EBWebView.Default"),
            ("+", r"HKCU\Software\Microsoft\Windows\CurrentVersion\Search", "MSEdgeWebView.EBWebView.Default"),
        ]
        for sign, key, name in misses:
            self.assertIsNone(allowed("value", key, name, sign, snap()), (sign, key, name))
        # The key itself is not excused: only that one value in it.
        self.assertIsNone(allowed("key", SEARCH_JUMPLIST_DATA, None, "+", snap()))
        # Registry names compare case-insensitively.
        self.assertIsNotNone(allowed("value", SEARCH_JUMPLIST_DATA.lower(), "msedgewebview.ebwebview.default", "+",
                                     snap()))

    def test_r372_the_widgets_time_and_the_shell_refresh_keys(self):
        refresh = LOCAL_SETTINGS + r"\Software\Microsoft\Windows\CurrentVersion\AppModel\ShellRefresh"
        # CI run 43689320509's installer job, both attempts, as printed (the default value's name is empty).
        lines = [
            ("+", "key", WIDGETS, None),
            ("+", "value", WIDGETS, "WidgetsStateLastRun"),
            ("~", "value", WIDGETS, "WidgetsStateLastRun"),
            ("-", "key", refresh + r"\2", None),
            ("-", "value", refresh + r"\2", ""),
            ("+", "key", refresh + r"\17", None),
            ("~", "value", refresh + r"\17", ""),
            # R373: the default value, for each sign.
            ("+", "value", refresh + r"\17", ""),
            ("-", "value", refresh + r"\17", ""),
        ]
        for line in lines:
            self.assertIsNotNone(allowed(line[1], line[2], line[3], line[0], snap()), line)
        misses = [
            ("-", "key", WIDGETS, None),
            ("-", "value", WIDGETS, "WidgetsStateLastRun"),
            ("+", "value", WIDGETS, "Other"),
            ("+", "key", WIDGETS + r"\Sub", None),
            ("+", "value", WIDGETS + r"\Sub", "WidgetsStateLastRun"),
            ("+", "key", refresh, None),
            ("+", "value", refresh, ""),
            ("+", "key", refresh + r"\x", None),
            ("+", "key", refresh + r"\2\Sub", None),
            ("+", "value", refresh + r"\2\Sub", ""),
            ("+", "key", refresh + r"X\2", None),
            # R373: another value name under a numbered key is residue, whatever its sign.
            ("+", "value", refresh + r"\17", "Other"),
            ("-", "value", refresh + r"\17", "Other"),
            ("~", "value", refresh + r"\17", "Other"),
            ("+", "value", refresh + r"\17", "@"),
            # A key has no content to change: `~` on a numbered key is never excused.
            ("~", "key", refresh + r"\17", None),
        ]
        for line in misses:
            self.assertIsNone(allowed(line[1], line[2], line[3], line[0], snap()), line)

    # R380: the printed residue of CI runs 43703543786 and 43683451272 (the `RESIDUE: ` prefix removed).
    R380_FIXTURES = ("packages-43703543786.txt", "packages-43683451272.txt")
    FB = "Microsoft.WindowsFeedbackHub_8wekyb3d8bbwe"
    FB_PROGID = "AppXtmyvd154r75whynphdjyax5d711yvn0s"

    def r380_lines(self, fixture):
        text = (TESTDATA / fixture).read_text(encoding="utf-8")
        return [line for line in text.split("\n") if line]

    def r380_residue(self, lines):
        before, after = snapshots_of(lines)
        out, bad = diff(before, after, False)
        residue = {line[len("RESIDUE: "):] for line in out if line.startswith("RESIDUE: ")}
        self.assertEqual(len(residue), bad, out)
        return residue

    def test_r380_the_two_runs_package_registrations_are_excused(self):
        for fixture in self.R380_FIXTURES:
            lines = self.r380_lines(fixture)
            self.assertEqual(len(lines), 671, fixture)
            before, after = snapshots_of(lines)
            out, bad = diff(before, after, False)
            self.assertEqual(bad, 0, [line for line in out if line.startswith("RESIDUE")][:5])
            # Each line parsed back prints as it was printed, and each is excused by R380.
            shown = sorted(line[len("allowed: "):].partition("  -- ")[0] for line in out)
            self.assertEqual(shown, sorted(lines), fixture)
            # Each by R380, but for R388's two empty capability keys, which ALLOW's key-only entries excuse first.
            r388 = [line for line in out if "R388" in line]
            self.assertEqual(len(r388), 2, fixture)
            self.assertTrue(all(R380_RUNS in line for line in out if line not in r388), fixture)
            # The reason names the package.
            for line in out:
                if self.FB_PROGID in line.partition("  -- ")[0] and line.startswith("allowed: + key"):
                    self.assertIn("its own package " + self.FB + " ", line)

    def test_r380_a_litepack_or_foreign_publisher_family_excuses_nothing(self):
        lines = self.r380_lines(self.R380_FIXTURES[0])
        families = (r"Microsoft\.WindowsFeedbackHub|Microsoft\.Windows\.NarratorQuickStart|"
                    r"MicrosoftCorporationII\.WindowsSubsystemForLinux|"
                    r"Microsoft\.NET\.Native\.(Framework|Runtime)\.2\.2")
        # Every line stays residue but R388's two empty capability keys (ALLOW's key-only entries, whatever R380 does).
        cap_keys = {rf"+ key {CONSENT_STORE.rsplit(chr(92), 1)[0]}", rf"+ key {CONSENT_STORE}"}
        self.assertTrue(cap_keys <= set(lines))
        for prefix in ("LitePack", "litepack.", "LITEPACKX"):
            renamed = [re.sub(families, lambda m: prefix + m[0], line, flags=re.IGNORECASE) for line in lines]
            residue = self.r380_residue(renamed)
            self.assertEqual(len(residue), 669, prefix)
            self.assertEqual(set(renamed) - residue, cap_keys, prefix)
        other = [line.replace("8wekyb3d8bbwe", "0123456789abc") for line in lines]
        self.assertEqual(set(other) - self.r380_residue(other), cap_keys)
        # One family renamed to the worker's moniker: its lines stay residue, the others' do not.
        worker = [re.sub(r"Microsoft\.WindowsFeedbackHub", "LitePack.Worker", line, flags=re.IGNORECASE)
                  for line in lines]
        residue = self.r380_residue(worker)
        named = {line for line in worker if "litepack" in line.lower()}
        self.assertTrue(named and named <= residue, sorted(named - residue)[:3])
        self.assertIn(rf"+ value {CLASSES}\.fh\OpenWithProgids [{self.FB_PROGID}] = hex(0):", residue)
        self.assertTrue(any("NarratorQuickStart" in line for line in set(worker) - residue))

    def test_r380_removals_and_unnamed_records_stay_residue(self):
        lines = self.r380_lines(self.R380_FIXTURES[1])
        removed = [rf"+ reparse {WINDOWS_APPS}\bash.exe",
                   rf"+ dir {PACKAGES_DIR}\{self.FB}\AC",
                   rf"+ key {CLASSES}\AppXm4fjth7rfv9b2vppk3w1gcpsf4gy9r3n\Shell"]
        for r in removed:
            self.assertIn(r, lines)
        changed = ["-" + line[1:] if line in removed else line for line in lines]
        self.assertEqual(self.r380_residue(changed), {"-" + r[1:] for r in removed})
        bg = CURRENT_VERSION + r"\Authentication\LogonUI\Notifications\BackgroundCapability"
        cap = CAPABILITY_ACCESS
        extra = [
            # A SID that no added Moniker names, under a key where the packages' SIDs are excused.
            rf"+ value {bg}\S-1-15-2-1-2-3\App.AppXabc.mca [Capabilities] = dword:00000004",
            rf"+ key {MAPPINGS}\S-1-15-2-9-9-9",
            rf'+ value {MAPPINGS}\S-1-15-2-9-9-9 [DisplayName] = "x"',
            # LitePack's own profile, never the packages'.
            rf"+ dir {PACKAGES_DIR}\LitePack.Worker",
            rf"+ dir {PACKAGES_DIR}\LitePack.Worker_8wekyb3d8bbwe",
            rf"+ key {MAPPINGS}\S-1-15-2-5-5-5",
            rf'+ value {MAPPINGS}\S-1-15-2-5-5-5 [Moniker] = "LitePack.Worker"',
            # .lpk's OpenWithProgids, with an AppX ProgID not in P and with one in P.
            rf"+ key {CLASSES}\.lpk",
            rf"+ key {CLASSES}\.lpk\OpenWithProgids",
            rf"+ value {CLASSES}\.lpk\OpenWithProgids [AppX0000000000000000000000000000abcd] = hex(0):",
            rf"+ value {CLASSES}\.lpk\OpenWithProgids [{self.FB_PROGID}] = hex(0):",
            rf"+ value {CLASSES}\.fh\OpenWithProgids [AppX0000000000000000000000000000abcd] = hex(0):",
            # URL protocols: an extra value; no UrlAssociations partner; the wrong default.
            rf"+ key {CLASSES}\examplescheme",
            rf'+ value {CLASSES}\examplescheme [@] = "URL:examplescheme"',
            rf'+ value {CLASSES}\examplescheme [URL Protocol] = ""',
            rf'+ value {CLASSES}\examplescheme [Extra] = "1"',
            rf"+ key {URL_ASSOCIATIONS}\examplescheme",
            rf"+ key {CLASSES}\otherscheme",
            rf'+ value {CLASSES}\otherscheme [@] = "URL:otherscheme"',
            rf'+ value {CLASSES}\otherscheme [URL Protocol] = ""',
            rf"+ key {CLASSES}\thirdscheme",
            rf'+ value {CLASSES}\thirdscheme [@] = "URL:other"',
            rf'+ value {CLASSES}\thirdscheme [URL Protocol] = ""',
            rf"+ key {URL_ASSOCIATIONS}\thirdscheme",
            # A capability key with a value not excused (and its parents then).
            rf"+ key {CONSENT_STORE}\broadFileSystemAccess",
            rf'+ value {CONSENT_STORE}\broadFileSystemAccess [Value] = "Deny"',
            # Rule 5: a family named where this rule never looks; LitePack's names; a bare empty key.
            rf'+ value {CURRENT_VERSION}\Run [x] = "{self.FB}"',
            rf'+ value {CURRENT_VERSION}\RunOnce [x] = "{self.FB}"',
            rf'+ value HKCU\Environment [P] = "{self.FB}"',
            rf'+ value HKCU\Software\RegisteredApplications [y] = "{self.FB_PROGID}"',
            rf'+ value HKCU\Software\RegisteredApplications [{self.FB}!App] = "{self.FB}"',
            r'+ value HKCU\Software\RegisteredApplications [z] = "Software\\Classes\\Local Settings\\Software'
            r'\\Microsoft\\Windows\\CurrentVersion\\AppModel\\Repository\\Packages'
            r'\\Other.App_1.0.0.0_x64__8wekyb3d8bbwe\\App\\Capabilities"',
            r'+ value HKCU\Software\RegisteredApplications [LitePack] = "Software\\Classes\\Local Settings\\Software'
            r'\\Microsoft\\Windows\\CurrentVersion\\AppModel\\Repository\\Packages'
            r'\\Microsoft.WindowsFeedbackHub_1.2512.16303.0_x64__8wekyb3d8bbwe\\App\\Capabilities"',
            rf"+ key HKCU\Software\RegisteredApplications\{self.FB}",
            rf"+ key {CURRENT_VERSION}\PushNotifications\Backup\Other.App_8wekyb3d8bbwe!App",
            rf"+ key {CURRENT_VERSION}\PushNotifications\Backup\LitePack.Worker_8wekyb3d8bbwe!App",
            rf"+ key {CURRENT_VERSION}\PushNotifications\Backup\{self.FB}X!App",
            rf"+ key {CLASSES}\CLSID\{LITEPACK_CLSID}\{self.FB}",
            rf'+ value {CLASSES}\org.litepack.gui [x] = "{self.FB}"',
            rf"+ key HKCU\Software\LitePack\{self.FB}",
            r"+ key HKCU\Software\Foo",
            rf"+ reparse {WINDOWS_APPS}\notanalias.exe",
            rf"+ key {APP_PATHS}\notanalias.exe",
        ]
        # The parents of a record not excused are not excused either. (R388: the two empty capability keys,
        # CapabilityAccessManager and its ConsentStore, are allowed alone by ALLOW's key-only entries; a value
        # or a subkey under them, as broadFileSystemAccess above, stays residue.)
        for line in (rf"+ key {cap}", rf"+ key {CONSENT_STORE}"):
            self.assertIn(line, lines)
            self.assertNotIn(line, self.r380_residue(lines + extra))
        parents = [rf"+ key {CLASSES}\.fh", rf"+ key {CLASSES}\.fh\OpenWithProgids"]
        for line in parents:
            self.assertIn(line, lines)
        wanted = set(extra) | set(parents)
        residue = self.r380_residue(lines + extra)
        self.assertTrue(wanted <= residue, sorted(wanted - residue))
        # Everything else of the run stays excused.
        self.assertEqual(residue - wanted, set(), sorted(residue - wanted)[:5])

    def test_r380_a_family_naming_litepack_anywhere_is_refused(self):
        # R382: not only at the start of the name.
        for leaf in ("LitePack.App_8wekyb3d8bbwe", "Contoso.LitePack_8wekyb3d8bbwe",
                     "Contoso.AppLITEPACKx_cw5n1h2txyewy"):
            self.assertIsNone(package_family(leaf), leaf)
        self.assertEqual(package_family(self.FB), self.FB)
        # Such a family alone makes no F: the changes it would excuse stay residue.
        lines = self.r380_lines(self.R380_FIXTURES[0])
        alone = [line for line in lines if line.startswith("~ ")]
        family = [rf"+ dir {PACKAGES_DIR}\Contoso.LitePack_8wekyb3d8bbwe"]
        self.assertEqual(self.r380_residue(alone + family), set(alone + family))

    def test_r380_barred_entries_feed_no_set(self):
        # R382: an entry rule 5 bars names a full name or an AppX ProgID; the records that only those
        # names would excuse stay residue, alongside a registration.
        lines = self.r380_lines(self.R380_FIXTURES[0])
        full = "Microsoft.WindowsFeedbackHub_9.9.9.9_x64__8wekyb3d8bbwe"
        progid = "AppX11111111111111111111111111111111"
        extra = [
            # N from a key under HKCU\Software\LitePack; a folder under that full name.
            rf"+ key HKCU\Software\LitePack\{full}",
            rf"+ dir %LOCALAPPDATA%\Elsewhere\{full}",
            # P from an AppUserModelID naming LitePack; the ProgID's key and a value it names.
            rf"+ key {CLASSES}\{progid}",
            rf"+ key {CLASSES}\{progid}\Application",
            rf'+ value {CLASSES}\{progid}\Application [AppUserModelID] = "{self.FB}!LitePack"',
            rf"+ value {CLASSES}\.fh\OpenWithProgids [{progid}] = hex(0):",
        ]
        residue = self.r380_residue(lines + extra)
        wanted = set(extra) | {rf"+ key {CLASSES}\.fh", rf"+ key {CLASSES}\.fh\OpenWithProgids"}
        self.assertTrue(wanted <= residue, sorted(wanted - residue))
        self.assertEqual(residue - wanted, set(), sorted(residue - wanted)[:5])

    def test_r380_a_url_protocol_needs_its_package_capabilities(self):
        # R382: Classes\<scheme> and UrlAssociations\<scheme> with no package's capabilities naming the
        # scheme stay residue; the fixtures' own (feedback-hub and others) do not.
        lines = self.r380_lines(self.R380_FIXTURES[1])
        caps = (r"HKCU\Software\Classes\Local Settings\Software\Microsoft\Windows\CurrentVersion\AppModel"
                r"\Repository\Packages\Microsoft.WindowsFeedbackHub_1.2512.16303.0_x64__8wekyb3d8bbwe\App"
                r"\Capabilities\URLAssociations")
        self.assertIn(f'+ value {caps} [feedback-hub] = "AppXsbz2fdbdmn6pz7xyd3zc95j9syhcnyc2"', lines)
        extra = [
            rf"+ key {CLASSES}\undeclared",
            rf'+ value {CLASSES}\undeclared [@] = "URL:undeclared"',
            rf'+ value {CLASSES}\undeclared [URL Protocol] = ""',
            rf"+ key {URL_ASSOCIATIONS}\undeclared",
            # Named under a key that is not a package's capabilities.
            rf"+ key {CLASSES}\elsewhere",
            rf'+ value {CLASSES}\elsewhere [@] = "URL:elsewhere"',
            rf'+ value {CLASSES}\elsewhere [URL Protocol] = ""',
            rf"+ key {URL_ASSOCIATIONS}\elsewhere",
            r"+ key HKCU\Software\Other\Capabilities\URLAssociations",
            r'+ value HKCU\Software\Other\Capabilities\URLAssociations [elsewhere] = "x"',
        ]
        residue = self.r380_residue(lines + extra)
        self.assertEqual(residue, set(extra))
        # Without the package's capabilities value, its own scheme stays residue.
        mine = [rf"+ key {CLASSES}\feedback-hub", rf"+ key {URL_ASSOCIATIONS}\feedback-hub"]
        for line in mine:
            self.assertIn(line, lines)
        bare = [line for line in lines if not line.startswith(f"+ value {caps} [feedback-hub]")]
        self.assertTrue(set(mine) <= self.r380_residue(bare))

    def test_r380_near_misses_stay_residue(self):
        lines = self.r380_lines(self.R380_FIXTURES[0])
        other = "Contoso.Other_8wekyb3d8bbwe"
        extra = [
            # The RegisteredApplications key itself, not only a value.
            r"+ key HKCU\Software\RegisteredApplications",
            # A Mappings SID key whose Moniker names LitePack's worker profile, and its values.
            rf"+ key {MAPPINGS}\S-1-15-2-7-7-7",
            rf'+ value {MAPPINGS}\S-1-15-2-7-7-7 [Moniker] = "LitePack.Worker"',
            rf'+ value {MAPPINGS}\S-1-15-2-7-7-7 [DisplayName] = "{self.FB}"',
            rf"+ key {MAPPINGS}\S-1-15-2-7-7-7\Children",
            # An alias under a family not in F, and what it would name.
            rf"+ reparse {WINDOWS_APPS}\{other}\other.exe",
            rf"+ reparse {WINDOWS_APPS}\other.exe",
            rf"+ key {APP_PATHS}\other.exe",
            rf'+ value {APP_PATHS}\other.exe [@] = "x"',
        ]
        residue = self.r380_residue(lines + extra)
        self.assertTrue(set(extra) <= residue, sorted(set(extra) - residue))
        self.assertEqual(residue - set(extra), set(), sorted(residue - set(extra))[:5])

    def test_r380_changes_and_the_marker_need_a_package(self):
        lines = self.r380_lines(self.R380_FIXTURES[0])
        alone = [line for line in lines if line.startswith("~ ") or "PostAppInstallTasksCompleted" in line]
        self.assertEqual(len(alone), 6)
        self.assertEqual(self.r380_residue(alone), set(alone))
        # With the packages, the same records are excused; never removed.
        self.assertEqual(self.r380_residue(lines), set())
        marker = next(line for line in alone if line.startswith("+ "))
        gone = [("-" + line[1:]) if line == marker else line for line in lines]
        self.assertEqual(self.r380_residue(gone), {"-" + marker[1:]})

    def test_r380_storage_sense_is_allowed_and_a_sibling_is_not(self):
        guid = "{0E5DF9A8-0B4F-4C0A-9E4E-0C2C3B1A2D3F}"
        for sign in "+~-":
            self.assertIsNotNone(allowed("key", STORAGE_SENSE + r"\Parameters\CachedSizes\\" + guid, None, sign,
                                         snap()))
            self.assertIsNotNone(allowed("value", STORAGE_SENSE + r"\Parameters\StoragePolicy", "2048", sign,
                                         snap()))
            self.assertIsNotNone(allowed("value", STORAGE_SENSE + r"\SuggestedFolders\Music\Suggestions\\" + guid,
                                         "timeStamp", sign, snap()))
        for kind, path in (("key", STORAGE_SENSE + "X"), ("value", STORAGE_SENSE + r"X\Parameters"),
                           ("key", CURRENT_VERSION + r"\Storage")):
            self.assertIsNone(allowed(kind, path, "01", "+", snap()), path)

    def test_r401_security_and_maintenance_is_allowed_and_a_sibling_is_not(self):
        guid = "{01979C6A-42FA-414C-B8AA-EEE2C8202018}"
        for sign in "+~-":
            self.assertIn("R401", allowed("key", SECURITY_MAINTENANCE + r"\Checks\\" + guid + ".check.100", None,
                                          sign, snap()) or "")
            self.assertIn("R401", allowed("value", SECURITY_MAINTENANCE + r"\Providers\EventLog\\" + guid,
                                          "Enabled", sign, snap()) or "")
        for kind, path in (("key", SECURITY_MAINTENANCE + "X"), ("value", CURRENT_VERSION + r"\Security"),
                           ("key", CURRENT_VERSION + r"\Action Center")):
            self.assertIsNone(allowed(kind, path, "x", "+", snap()), path)

    def test_r407_the_device_census_and_the_widgets_settings_log_and_their_siblings(self):
        for sign in "+~-":
            self.assertIn("R407", allowed("key", CENSUS, None, sign, snap()) or "")
            for name in ("ReturnCode", "StartTime", "StartTimeBin"):
                self.assertIn("R407", allowed("value", CENSUS, name, sign, snap()) or "", (sign, name))
        for leaf in ("settings.dat.LOG2", "settings.dat.LOG1", "Settings.dat.log2"):
            self.assertIn("R407", allowed("file", WEBEXPERIENCE_SETTINGS + "\\" + leaf, None, "~", snap()) or "", leaf)
        current = CENSUS.rsplit("\\", 1)[0]
        for kind, path, name in (("key", current + r"\CensusX", None), ("value", current + r"\Census2", "StartTime"),
                                 ("value", current, "Census"), ("key", r"HKCU\Software\Census", None)):
            self.assertIsNone(allowed(kind, path, name, "+", snap()), path)
        ws = WEBEXPERIENCE_SETTINGS
        misses = [("+", ws + r"\settings.dat.LOG2"), ("-", ws + r"\settings.dat.LOG1"),
                  ("~", ws + r"\settings.dat"), ("~", ws + r"\settings.dat.LOG3"),
                  ("~", ws + r"\Sub\settings.dat.LOG1"), ("~", ws.rsplit("\\", 1)[0] + r"\settings.dat.LOG1")]
        for sign, path in misses:
            self.assertIsNone(allowed("file", path, None, sign, snap()), (sign, path))

    def test_r406_windows_shell_webview_data_and_firefox_background_tasks_and_their_siblings(self):
        for sign in "+~-":
            for path in (CBS_WEBVIEW + r"\hyphen-data\120.0.6050.0\hyph-en-us.hyb",
                         CBS_WEBVIEW + r"\component_crx_cache\metadata.json", CBS_WEBVIEW + r"\Local State",
                         FIREFOX_BACKGROUND_TASKS + r"\qh3smhym.MozillaBackgroundTask-308046B0AF4A39CB-defaultagent"
                         r"\prefs.js"):
                self.assertIn("R406", allowed("file", path, None, sign, snap()) or "", (sign, path))
        cbs = CBS_WEBVIEW.rsplit("\\", 1)[0]
        misses = [cbs + r"\Other\x.json", cbs + r"\EBWebViewX\x.json",
                  CBS_WEBVIEW.replace("Client.CBS", "Client.CBSX") + r"\x.json",
                  r"%APPDATA%\Mozilla\Firefox\Profiles\abc.default\prefs.js",
                  FIREFOX_BACKGROUND_TASKS + "X" + r"\prefs.js",
                  r"%LOCALAPPDATA%\org.litepack.gui\EBWebView\Local State"]
        for path in misses:
            self.assertIsNone(allowed("file", path, None, "+", snap()), path)
        # A registry path under the same names is not a file: nothing excuses it.
        self.assertIsNone(allowed("key", r"HKCU\Software\Mozilla\Firefox\Background Tasks Profiles", None, "+",
                                  snap()))

    def test_r401_the_widgets_hive_logs_and_explorers_startup_log_changed_only(self):
        for leaf in ("UserClasses.dat.LOG2", "User.dat.LOG2", "User.dat.LOG1", "userclasses.dat.log1"):
            self.assertIn("R401", allowed("file", HELIUM + "\\" + leaf, None, "~", snap()) or "", leaf)
        self.assertIn("R401", allowed("file", EXPLORER_STARTUP_LOG, None, "~", snap()) or "")
        misses = [("+", HELIUM + r"\User.dat.LOG2"), ("-", HELIUM + r"\UserClasses.dat.LOG1"),
                  ("~", HELIUM + r"\User.dat"), ("~", HELIUM + r"\User.dat.LOG3"),
                  ("~", HELIUM + r"\Sub\User.dat.LOG1"), ("~", HELIUM.rsplit("\\", 1)[0] + r"\User.dat.LOG1"),
                  ("+", EXPLORER_STARTUP_LOG), ("-", EXPLORER_STARTUP_LOG),
                  ("~", EXPLORER_STARTUP_LOG + ".old")]
        for sign, path in misses:
            self.assertIsNone(allowed("file", path, None, sign, snap()), (sign, path))

    def test_r401_r404_explorers_window_record_is_allowed_added_or_removed(self):
        si = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Explorer\SessionInfo"
        key = si + r"\2\ApplicationViewManagement\W32:0000000000030378"
        gone = si + r"\2\ApplicationViewManagement\W32:0000000000080016"  # R404: CI run 43742607535
        for sign, k in (("+", key), ("-", gone)):
            self.assertIn("R404", allowed("key", k, None, sign, snap()) or "", sign)
            self.assertIn("R404", allowed("value", k, "VirtualDesktop", sign, snap()) or "", sign)
        misses = [("~", "value", key, "VirtualDesktop"), ("~", "value", gone, "VirtualDesktop"),
                  ("-", "key", si + r"\2\Other\W32:0000000000080016", None),
                  ("-", "value", si + r"\2\Other\W32:0000000000080016", "VirtualDesktop"),
                  ("-", "key", gone + r"\Sub", None),
                  ("+", "key", key + r"\Sub", None),
                  ("+", "key", si + r"\2\ApplicationViewManagement\W32:00000000000303", None),
                  ("+", "key", si + r"\x\ApplicationViewManagement\W32:0000000000030378", None),
                  ("+", "value", si + r"\2\ApplicationViewManagement", "x"),
                  ("+", "key", si + r"\2\Other\W32:0000000000030378", None)]
        for sign, kind, path, name in misses:
            self.assertIsNone(allowed(kind, path, name, sign, snap()), (sign, kind, path, name))

    def test_r401_the_muicache_allows_a_bare_windows_dll_name_never_lpks(self):
        key = LOCAL_SETTINGS + r"\MuiCache\1d8\52C64B7E"
        self.assertIsNotNone(allowed("value", key, "@windows.storage.dll,-21791", "+", snap()))
        for name in ("@lpk_shell.dll,-1", "@LPK_SHELL.DLL,-1", "@LitePack.dll,-2", "@my-litepack-x.dll,-3",
                     "@windows.storage.dll", "@sub\\windows.storage.dll,-1", "windows.storage.dll,-1"):
            self.assertIsNone(allowed("value", key, name, "+", snap()), name)

    def test_r401_nsis_plugin_folders_with_system_dll_alone_are_allowed_added_only(self):
        t = r"%LOCALAPPDATA%\Temp"
        files = {t + r"\nsrE5B7.tmp": {"kind": "dir"}, t + r"\nsrE5B7.tmp\System.dll": f(12288),
                 t + r"\nsf1311.tmp": {"kind": "dir"}, t + r"\nsf1311.tmp\System.dll": f(12288)}
        lines, bad = diff(self.base, snap(dict(self.base["files"], **files), self.base["registry"]), False)
        self.assertEqual(bad, 0, lines)
        self.assertTrue(all("R401" in line for line in lines), lines)
        # Any other file in such a folder stays residue; so does a folder of another shape or place, a file
        # of the folder's name, and a removal.
        extra = dict(files, **{t + r"\nsrE5B7.tmp\nsExec.dll": f()})
        lines, bad = diff(self.base, snap(dict(self.base["files"], **extra), self.base["registry"]), False)
        self.assertEqual(bad, 1, lines)
        self.assertIn(r"RESIDUE: + file %LOCALAPPDATA%\Temp\nsrE5B7.tmp\nsExec.dll", "\n".join(lines))
        # R404: one to four hex digits (nsi2CA in CI run 43742607535), any case.
        for leaf in ("nsa1.tmp", "nsi2CA.tmp", "nsb2ca.tmp", "nsf1311.tmp", "NSRE5B7.TMP"):
            two = {t + "\\" + leaf: {"kind": "dir"}, t + "\\" + leaf + r"\System.dll": f(12288)}
            lines, bad = diff(self.base, snap(dict(self.base["files"], **two), self.base["registry"]), False)
            self.assertEqual(bad, 0, (leaf, lines))
        for leaf in ("nsiE5B71.tmp", "nsi.tmp", "nsi2CG.tmp", "ns12CA.tmp", "nsi2C-.tmp"):
            for extra in ({t + "\\" + leaf: {"kind": "dir"}}, {t + "\\" + leaf + r"\System.dll": f(12288)}):
                after = snap(dict(self.base["files"], **extra), self.base["registry"])
                self.assertEqual(diff(self.base, after, False)[1], 1, (leaf, extra))
        misses = {t + r"\nsrE5B7X.tmp": {"kind": "dir"}, t + r"\abcE5B7.tmp": {"kind": "dir"},
                  t + r"\sub\nsrE5B7.tmp": {"kind": "dir"}, t + r"\nsq0001.tmp": f(),
                  t + r"\nsq0002.tmp\sub": {"kind": "dir"}, t + r"\nsq0002.tmp\sub\System.dll": f(),
                  t + r"\nsq0003.tmp\System.dll.bak": f()}
        for path, rec in misses.items():
            after = snap(dict(self.base["files"], **{path: rec}), self.base["registry"])
            self.assertEqual(diff(self.base, after, False)[1], 1, path)
        before = snap(dict(self.base["files"], **files), self.base["registry"])
        self.assertEqual(diff(before, self.base, False)[1], 4)

    def test_r401_the_start_menus_tile_record_of_litepack_stays_residue(self):
        key = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Start\TileProperties\W~org.litepack.gui"
        self.assertIsNone(allowed("key", key, None, "+", snap()))
        self.assertIsNone(allowed("value", key, "Category", "+", snap()))

    # R408 (CI run 43753831748's verify-arm64): with --verdict litepack, only LitePack's own residue is a verdict.
    R408_CLOUD = (r"HKCU\Software\Microsoft\Windows\CurrentVersion\CloudStore\Store\Cache\DefaultAccount"
                  r"\$$windows.data.placeholdertilecollection\Current")
    R408_CDM = (r"%LOCALAPPDATA%\Packages\Microsoft.Windows.ContentDeliveryManager_cw5n1h2txyewy\Settings"
                r"\settings.dat")

    def r408(self, files=None, registry=None, before_files=None, before_registry=None, run=None, verdict="litepack"):
        before = snap(dict(self.base["files"], **(before_files or {})),
                      dict(self.base["registry"], **(before_registry or {})))
        after = snap(dict(before["files"], **(files or {})), dict(before["registry"], **(registry or {})))
        return diff(before, after, False, run, verdict=verdict)

    def test_r408_litepacks_own_residue_stays_a_verdict(self):
        run_key = CURRENT_VERSION + r"\Run"
        cases = [
            {"registry": {r"HKCU\Software\Vendor\Thing": {"Target": r'"C:\\Apps\\LitePack\\lpk-gui.exe"'}}},
            {"registry": {r"HKCU\Software\Org.LitePack.Gui": {}}},
            {"registry": {CLASSES + r"\CLSID\\" + LITEPACK_CLSID: {}}},
            {"registry": {run_key: {"SomeUpdater": '"C:\\\\x\\\\u.exe"'}}},
            {"before_registry": {r"HKCU\Environment": {"Path": '"C:\\\\a"'}},
             "registry": {r"HKCU\Environment": {"Path": '"C:\\\\a;C:\\\\b"'}}},
            {"registry": {REGISTERED_APPS: {"Other": '"x"'}}},
            {"registry": {CURRENT_VERSION + r"\Uninstall\Other": {}}},
            {"registry": {CLASSES + r"\.lpk\OpenWithProgids": {}}},
            {"files": {r"%APPDATA%\Microsoft\Windows\Recent\AutomaticDestinations"
                       r"\a233dcd0c3d36a7d.automaticDestinations-ms": f()}},
        ]
        for c in cases:
            lines, bad = self.r408(**c)
            # (An added key of another name that holds LitePack's value is OTHER itself; its value is the verdict.)
            self.assertGreaterEqual(bad, 1, (c, lines))
            self.assertEqual(bad, sum(1 for line in lines if line.startswith("RESIDUE: ")), (c, lines))
        with unittest.mock.patch.dict(os.environ, {"LOCALAPPDATA": r"C:\U\AppData\Local"}):
            run = "C:/U/AppData/Local/Apps/Run"
            lines, bad = self.r408(files={r"%LOCALAPPDATA%\Apps\Run\x.txt": f()}, run=run)
            self.assertEqual(bad, 1, lines)
            # A value whose data names the run folder (doubled backslashes, as reg export writes it).
            lines, bad = self.r408(registry={r"HKCU\Software\Vendor": {"p": r'"C:\\U\\AppData\\Local\\Apps\\Run\\a"'}},
                                   run=run)
            self.assertEqual(bad, 1, lines)
            lines, bad = self.r408(files={r"%LOCALAPPDATA%\Apps\Runner\x.txt": f()}, run=run)
            self.assertEqual(bad, 0, lines)

    def test_r408_other_residue_is_printed_and_no_verdict_but_the_default_still_fails(self):
        for c in ({"registry": {self.R408_CLOUD: {"Data": "hex:01,02"}}}, {"files": {self.R408_CDM: f()}},
                  {"before_files": {self.R408_CDM: f()}, "files": {self.R408_CDM: f(size=2)}}):
            lines, bad = self.r408(**c)
            self.assertEqual(bad, 0, (c, lines))
            self.assertTrue(lines and all(line.startswith("OTHER: ") for line in lines), (c, lines))
        lines, bad = self.r408(registry={self.R408_CLOUD: {"Data": "hex:01,02"}}, verdict="all")
        self.assertEqual(bad, 2, lines)
        self.assertTrue(all(line.startswith("RESIDUE: ") for line in lines), lines)

    def test_r408_the_cli_counts_both_and_fails_only_on_litepacks(self):
        with tempfile.TemporaryDirectory() as d:
            b, a = Path(d) / "b.json", Path(d) / "a.json"
            b.write_text(json.dumps(self.base), encoding="utf-8")
            reg = dict(self.base["registry"], **{self.R408_CLOUD: {}})
            a.write_text(json.dumps(snap(self.base["files"], reg)), encoding="utf-8")
            out = io.StringIO()
            with unittest.mock.patch("sys.stdout", out):
                rc = main(["diff", str(b), str(a), "--verdict", "litepack"])
                rc_all = main(["diff", str(b), str(a)])
            self.assertEqual((rc, rc_all), (0, 1), out.getvalue())
            self.assertIn("1 differences, 1 not allowed: 0 LitePack's, 1 other (no verdict with --verdict "
                          "litepack)", out.getvalue())
            reg[r"HKCU\Software\LitePack"] = {}
            a.write_text(json.dumps(snap(self.base["files"], reg)), encoding="utf-8")
            with unittest.mock.patch("sys.stdout", io.StringIO()) as out2:
                self.assertEqual(main(["diff", str(b), str(a), "--verdict", "litepack"]), 1)
            self.assertIn("2 not allowed: 1 LitePack's, 1 other", out2.getvalue())

    def test_r380_the_parser_reads_shows_format(self):
        self.assertEqual(parse_shown(r'+ value HKCU\A [@] = "x"'), ("+", "value", r"HKCU\A", "", '"x"'))
        self.assertEqual(parse_shown(r"~ file %APPDATA%\a b (3 bytes, mtime 7)"),
                         ("~", "file", r"%APPDATA%\a b", None, {"kind": "file", "size": 3, "mtime": 7}))
        self.assertEqual(parse_shown(r"+ reparse %X%\r")[4]["kind"], "reparse")
        self.assertEqual(parse_shown(r"- key HKCU\B"), ("-", "key", r"HKCU\B", None, True))


TESTDATA = Path(__file__).resolve().parent / "e7-residue-testdata"


def parse_shown(line):
    """A diff line in show()'s format, with its sign: (sign, kind, path, value name, record)."""
    sign, _, rest = line.partition(" ")
    word, _, rest = rest.partition(" ")
    if word == "value":
        m = re.fullmatch(r"(.*?) \[(.*?)\] = (.*)", rest, re.DOTALL)
        return sign, "value", m[1], "" if m[2] == "@" else m[2], m[3]
    if word == "key":
        return sign, "key", rest, None, True
    if word == "file":
        m = re.fullmatch(r"(.*) \(([0-9]+) bytes, mtime ([0-9]+)\)", rest)
        return sign, "file", m[1], None, {"kind": "file", "size": int(m[2]), "mtime": int(m[3])}
    if word in ("dir", "reparse"):
        return sign, "file", rest, None, {"kind": word}
    raise ValueError(f"unparsed diff line: {line!r}")


def snapshots_of(lines):
    """The two snapshots whose diff is exactly `lines` (show()'s format, each with its sign)."""
    before, after = snap(), snap()
    parsed = [parse_shown(line) for line in lines]
    keys = {norm(p) for _, kind, p, _, _ in parsed if kind == "key"}
    for sign, kind, path, name, rec in parsed:
        if kind == "file":
            if sign in "+~":
                after["files"][path] = rec
            if sign in "-~":
                before["files"][path] = dict(rec, mtime=rec.get("mtime", 0) + 1) if sign == "~" else rec
        elif kind == "key":
            (after if sign == "+" else before)["registry"].setdefault(path, {})
        else:
            if norm(path) not in keys:  # the value's key itself is unchanged: on both sides
                before["registry"].setdefault(path, {})
                after["registry"].setdefault(path, {})
            if sign in "+~":
                after["registry"].setdefault(path, {})[name] = rec
            if sign in "-~":
                before["registry"].setdefault(path, {})[name] = rec + " (before)" if sign == "~" else rec
    return before, after


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
