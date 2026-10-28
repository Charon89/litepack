# E7 design: release, signing, installers, updater, distribution

2026-10-27; for E7-1 to E7-10 (`docs/PLAN.md:241–260`) and the release halves of E8-6 and E8-8 (`:276`, `:278`), on
`origin/main` 52f6220. Numbers are design defaults or the vendors' own statements, never measurements (D-07); a price,
duration or size not confirmed on the vendor's own page is marked *unverified*. Each choice becomes the `D-NN` named in
§11; the owner's questions are §10. Web pages read on 2026-10-27; nothing was downloaded.

## 1. The release workflow (E7-2, E7-3, E8-6, E8-8)

**Trigger.** A tag `v<semver>` on a commit of `main` runs `.github/workflows/release.yml`, and its `package` job enters
the `release` environment, which admits `v*` tags only [GH-ENV]. A `workflow_dispatch` on a branch is a rehearsal: its
`package` job enters a `rehearsal` environment instead, whose federated credential holds the signer role on the Public
Trust Test profile alone (roles are assigned per certificate profile [AS-RR]), so a branch run cannot reach the
production profile even with an edited workflow file. Test-profile certificates are not trusted by default and carry
the lifetime EKU [AS-RR][AS-TM], so in a rehearsal `assemble` and `verify` first import the test chain's root into the
runner's machine root store, which `/pa` and the sparse package's registration need, and finish within the
certificates' 72 hours [AS-CM]. Microsoft publishes no test root, so the first rehearsal resolves it from a test
signature's chain, the owner commits its SHA-256, and later rehearsals import that root only (*unverified* that the
chain resolves). A rehearsal makes no attestation and no draft. One run at a time.

| Job | Runner; permissions; secrets | Does |
|---|---|---|
| `preflight` | Linux; `contents: read`; none | tag = workspace version = `tauri.conf.json`'s = each manifest template's; the tagged commit is on `main`; `scripts/third-party-notices.sh --check`, `check-unsafe.sh`, `check-release-hooks.sh` widened to `lpk-gui` (no E5-11 driver switch in a release build, the review's condition in `docs/STATUS.md`) |
| `tools` | `windows-2025`; `contents: read`; none | `cargo install tauri-cli --version 2.12.4 --locked`; NSIS 3.13 and `nsis_tauri_utils` 0.5.3 fetched from the URLs the bundler pins and checked against its own SHA-1 values [T-NSIS-RS]; the Artifact Signing dlib package and the SDK build tools' `signtool` from NuGet [AS-INT]; uploads them with `tools.sha256`, the SHA-256 of every file |
| `build` ×2 (x64, ARM64) | `windows-2025`; `contents: read`; none | `scripts/repro-build.sh` with `cargo auditable` for `lpk`, `lpk-check`, `lpk-worker` (and `lpk-worker-libarchive` after E4-4) and `lpk_shell.dll`; `cargo tauri build --no-bundle` for `lpk-gui.exe` twice (with the updater, and without it for the MSIX, §4); ARM64 cross-built as `release-checks.yml` does (D-107 (3)); `check-worker-symbols.sh`; one CycloneDX SBOM per binary (`cargo cyclonedx --describe binaries --target …` [CDX]); uploads `unsigned-<arch>` with `unsigned.sha256` |
| `package` | `windows-2025`; environment `release` (owner's approval) or `rehearsal`; `id-token: write`, `contents: read`; OIDC, no stored secret | checks every downloaded file against `tools.sha256` and `unsigned.sha256`; signs the listed PEs; builds and signs the sparse package (§4); puts the checked NSIS where the bundler looks; runs the checked `cargo-tauri.exe bundle --no-binary-patching` per architecture (§3); uploads the signed files, the installers and `signed.sha256` |
| `assemble` | `windows-2025`; `contents: read`; none | the zips (§5) and the unsigned Store bundle (§4) from the signed files; unpacks every installer and zip; the verification below; `SHA256SUMS` |
| `verify` ×2 | `windows-2025`, `windows-11-arm`; `contents: read`; none | silent install, `lpk` from PATH, a round trip, the GUI smoke (`crates/lpk-gui/smoke.sh`), a foreign archive through the installed worker, upgrade from the previous public release when one exists, silent uninstall and the residue diff (§3); every installed PE on `signed.sha256` and the installed uninstaller signed; the portable snapshot (§5); the ARM64 machine check of `release-checks.yml` |
| `attest` (tags only) | Linux; `id-token: write`, `attestations: write`, `contents: read` | `actions/attest@v4` provenance over every signed binary and every asset, SBOM attestations per binary [GH-ATT] (E8-6) |
| `draft` (tags only; after `verify`, `attest`) | Linux; `contents: write`, `actions: read` | `scripts/memory-safety-statement.sh --since <previous tag>` into the notes (E8-7); `gh release create --draft --verify-tag` with the assets; `--prerelease` for a pre-release version |

**What runs beside the credential.** Only `package` holds the signing identity, and it compiles nothing and downloads
no tool: `cargo-tauri.exe`, `signtool` and the dlib come from `tools` and run only if their SHA-256 match, and the
bundler reuses a complete NSIS cache, re-checking `NSIS.exe` and the plugin hashes it pins [T-NSIS-RS]. The one signer
is `scripts/release-sign.ps1`, both for `package`'s own step and as Tauri's `signCommand`: a file whose SHA-256 is on
`unsigned.sha256` or `tools.sha256` (the NSIS plugin DLLs the bundler signs) is signed, one already on `signed.sha256`
is left alone, and anything else is refused, which fails the bundle. The uninstaller and the installer that makensis
writes during the bundle cannot be on a list beforehand; the script accepts them only while that step runs, and the
credential-free jobs check them: `assemble` requires every PE inside each installer to be on `signed.sha256`, and
`verify` checks the uninstaller it installs. What stays trusted inside `package` is the pinned tauri-cli, NSIS and
Microsoft's signing tools; its only action is `azure/login`, pinned by commit SHA like every other.

**Order of signatures.** Inner files before their containers, hashes after signatures, the updater's signature last:
every executable and DLL is signed with an RFC 3161 timestamp before it is packaged; NSIS's uninstaller is signed while
the installer is compiled (`!uninstfinalize` with the sign command [T-NSI]), the installer after; then `SHA256SUMS` and
the attestations over the final bytes ("Do not modify signed files" [SSR]); then minisign over the signed installer
(§6). The bundler signs copies of the NSIS plugin DLLs, the resources it judges unsigned, the uninstaller (through the
command it passes to the template) and the installer [T-NSIS-RS], and sidecars it finds unsigned [T-BUN14]. Binary
patching writes the bundle type into the main exe and re-signs it, and is needed only when one platform ships several
bundle types that each update through their own installer [T-BUN210]; the updater picks the installer by sniffing the
downloaded bytes, never by bundle type [UPD-SRC], so `--no-binary-patching` (tauri-cli 2.12.0 and later [T-CLI]; the
pin is 2.12.4) keeps the signed `lpk-gui.exe` byte-identical to the built one.

**Signing calls.** `azure/login@v3` over OpenID Connect, no client secret, as Microsoft recommends [AS-ACT]; then
`release-sign.ps1` runs `signtool sign /fd SHA256 /tr http://timestamp.acs.microsoft.com /td SHA256 /dlib
Azure.CodeSigning.Dlib.dll /dmdf metadata.json <file>`, the dlib authenticating through the CLI session that
`azure/login` opened [AS-INT]; Tauri's `signCommand` takes a string or `{cmd, args}` with `%1` [T-CFG]. The dlib needs
.NET 8 [AS-INT], taken from the runner image (*unverified*). Microsoft's `azure/artifact-signing-action` is not used: it
would sign whatever the job hands it, outside the allow-list. It "can only be executed on Windows runners" and "We
currently do not support Windows Arm runners" [AS-ACT], and the dlib ships in x64 and x86 builds [AS-INT], so ARM64
binaries are signed on the x64 job either way.
Certificates are renewed daily and valid 72 hours, so a signature without a timestamp stops validating when its
certificate expires [AS-CM][AS-INT]; on the OV route (§10) the CA's timestamp server replaces Microsoft's.

**Verification (E7-3, in `assemble` and again in `verify`).** `signtool verify /pa /v /tw` on every PE and package, exit
2 (warnings, such as no timestamp) counted as a failure [ST]; installers and zips are unpacked first (7-Zip unpacks NSIS
[7Z]), so the plugin DLLs NSIS extracts at run time are checked too; the signer's subject is compared with the sparse
package's `Publisher` (the exact DN), `bundle.publisher` (Add/Remove Programs) and winget's `Publisher` (§8). Smart App
Control blocks unsigned files without reputation and applies to all executable files, not only downloads [SSR]: an
unsigned PE anywhere is a release blocker.

**What ships**, per architecture (x64, ARM64); file names follow the product name (pending):

| Asset | Contents | Signature |
|---|---|---|
| NSIS installer | `lpk-gui.exe`, `uninstall.exe`, `bin\` (`lpk.exe`, `lpk-check.exe`, the workers), `shell\<version>\lpk_shell.dll`, the signed sparse package, notices (§3) | every PE inside, and the installer |
| Portable zip | `lpk-gui.exe`, `lpk.exe`, `lpk-check.exe`, the workers, the `portable` marker, notices (§5) | every PE |
| CLI zip | `lpk.exe`, `lpk-check.exe`, the workers, notices | every PE |
| `latest.json`, the installers' `.sig` | the updater manifest, added to the draft later by the owner (§6) | minisign |
| `SHA256SUMS`, `sbom-<version>.zip` | hashes of the assets the workflow makes; one CycloneDX SBOM per shipped binary | attested |

`SHA256SUMS` cannot list `latest.json` and the `.sig` files, which arrive after it; what protects them is the updater's
own check of each installer against its minisign signature and of the version against the signature's trusted comment
(§6). The Store's MSIX bundle is a workflow artifact the owner uploads to Partner Center, never a release asset (§4).

**Reproducibility and provenance.** Building through `scripts/repro-build.sh` keeps what ships equal to what E8-5
compares; `cargo auditable` stores the dependency list in a `.dep-v0` section, sorted and without timestamps so that
reproducible builds hold [AUD]; the compare job switches to it in the same PR and adds both `lpk-gui.exe` variants and
`lpk_shell.dll` to its set (today `lpk`, `lpk-check`, `lpk-worker`, D-107 (2)). A signature changes the bytes: the run
records and attests the unsigned binaries' SHA-256 beside the signed ones; proving that a signed file's Authenticode
digest equals a rebuild's is a later E8 item. Immutable releases lock a published release's assets and tag and add a
release attestation [GH-IMM]: the workflow only drafts, and the owner publishes after E8-4's round trip and the updater
signature (§6).

**Tag and version (E8-8).** One source, the workspace `Cargo.toml`, checked equal in `tauri.conf.json`, the sparse and
Store manifests (`MAJOR.MINOR.PATCH.0`) and the tag. The Store reserves the fourth MSIX field and refuses 0 as the first
[MSIX-REQ], so the first Store version is 1.0.0 or later; a sparse package version registers once (0x80073CF9 [GI]), so
every release bumps at least PATCH; pre-releases (`-rc.N`) are GitHub pre-releases, never "latest" [GH-LATEST], so the
updater never offers one.

## 2. Identity, accounts and keys (E7-1)

**One name, many places.** The publisher's legal name is the certificate subject (an individual's validated name or an
organisation's; no custom CN or O [AS-FAQ]), the sparse package's `Publisher` (equal to the subject [GI]),
`bundle.publisher` in Add/Remove Programs (derived from the identifier when unset, and it may not equal the product name
[T-STORE]), winget's `Publisher` (matching ARP [WG-MAN]) and the name SmartScreen shows for a signed file [SSR]. The
Store shows the Partner Center account's name: the individual's own, or a registered business's [ACCT]. The product
name sets the install folder, the settings folder (E5 §5), ARP and Start menu, the Store reservation and winget's
`PackageName`; the identifier (`org.litepack.gui`, `crates/lpk-gui/tauri.conf.json:5`) names the local state folder (§3)
and presumes a domain. All three are fixed before the first public release.

**Azure Artifact Signing** [AS-QS][AS-FAQ][AS-INT]. Public Trust is open to organisations in the US, Canada, the EU,
the UK, Australia, New Zealand, Japan, South Korea, Singapore, Switzerland, Norway and Israel; "Individual developers
must be located in the United States or Canada" [AS-QS]. An individual's identity is read from the Azure billing
account, which must be of type Individual, its legal name and sold-to address matching the government ID; validation
runs in the portal only, through Microsoft Verified ID with AU10TIX (a government ID and a face check, on a phone), and
the status turns Completed minutes after a successful check [AS-QS][AS-FAQ]. The certificate shows the name with city,
province and country, never the e-mail, and the street address and postal code only if the certificate profile opts in
[AS-QS]: public in every signed file. Organisations: 1 to 20 business days, possibly longer [AS-QS]. A paid
subscription is required, free, trial and sponsored ones refused; billing is not pro-rated [AS-FAQ]; Basic costs about
US$9.99 a month per Microsoft Learn [CSO][SSR] (the pricing page showed no figure: *unverified*), with 5,000 signatures
a month and one profile of each type [AS-PRICE]. Certificates are renewed daily and valid 72 hours; the certificate is
never handed out (FIPS 140-3 Level 3); no EV [AS-CM][AS-FAQ]. A profile's EKU is its durable identity across
renewals, and a deleted and recreated profile gets a new one [AS-CM], so the production profile is never recreated. A
validation expires, with reminders from 60 days before (its term is not stated: *unverified*); changed details need a
new validation, which changes the certificates [AS-FAQ][AS-QS]. Its CAs left Microsoft's Trusted Root Program and stay
on the Windows Platform Trust List, so Windows still trusts them [AS-CM]; what that rules out is in §4.

**OV certificate** (the fallback, D-89 (6)): US$150–300 a year and "several business days" per Microsoft (*unverified*
against CA price lists); since 2023-06-01 the key is generated and used in a hardware crypto module (CSBR §6.2.7.4.2), a
USB token or the CA's cloud signing; since 2026-03-01 at most 460 days' validity (§6.3.2) [CSO][CSBR]. SmartScreen
treats it like Artifact Signing [CSO]; EV no longer helps [SSR].

**Store account.** Individual (published under your own name) or company (a registered legal entity; D-U-N-S or
documents; manual review typically 2–5 business days); no fee in the new flow, which starts only at
storedeveloper.microsoft.com; an individual account cannot become a company account [ACCT]. Policy 10.14 wants a
company account from "any person acting in relation to their trade or profession", or when consumers would read the app
or publisher name as a business [POL].

**Who holds what.**

| Item | Lives in | Held by | Used by |
|---|---|---|---|
| Identity validation, certificate profile | the owner's Azure subscription | the owner (MFA) | — |
| Permission to sign | an Entra app registration whose federated credential names this repository's `release` environment and whose only role is Artifact Signing Certificate Profile Signer on the production profile [AS-RR] | the owner | `package` on a tag, over OIDC; GitHub stores only the client, tenant and subscription IDs |
| Permission to rehearse | a second app registration, federated with the `rehearsal` environment, the signer role on the Public Trust Test profile only [AS-RR] | the owner | `package` on a branch run |
| `release` environment | repository settings: reviewer the owner (self-review allowed), deployment tags `v*` [GH-ENV] | the owner | `package` on a tag |
| `rehearsal` environment | repository settings: branches allowed, no tags; no reviewer, since its signatures are untrusted off the runner and lapse with their 72-hour certificates [AS-TM][AS-RR][AS-CM] | the owner | `package` on a branch run |
| Updater (minisign) private key | removable media, encrypted with the key's password, two copies; plugged in only to sign, in a session with no agent running; never in git or GitHub (decision (f); §10 Q7) | the owner | `scripts/updater-sign.sh` (§6) |
| Agents' GitHub token | a fine-grained token for this repository without the Deployments permission, which approving a pending deployment needs [GH-PAT] (a classic token with `repo` can approve [GH-REV]); the owner's own credential never reaches an agent session | the owner | agents and build scripts on the owner's machine |
| Updater public key | `plugins.updater.pubkey` in `tauri.conf.json` (the content, not a path [T-UPD]) | git | the GUI |
| OV route: the private key | the CA's HSM or the owner's token | the owner | `signtool` |
| Partner Center | the owner's Microsoft account | the owner | Store submissions |
| CI test certificates | made per run, trusted only on that runner | CI | the sparse and MSIX install tests (§4) |

Repositories created after 2026-07-15 get an immutable OIDC subject with owner and repository IDs [GH-OIDC][GH-OIDCR],
so each federated credential is written from the `sub` claim a test run prints (the claim, never the token), not from
the documented examples. The owner's approval is what stands between a pushed tag and a production signature, and only
the owner's own credential can give it; a tag ruleset limits who pushes `v*` (the owner's setting).

**E7-1's other items.** The export-control note (mass-market encryption, a note and not legal advice) follows
`docs/LICENSING.md` "Other compliance". The committed `signtool verify` output names the subject, which every signed
file publishes anyway. The `git log --all -S` search covers the minisign header in plain and base64 form.

## 3. The NSIS installer (E7-5)

**Per user, no UAC.** `installMode: "currentUser"` is Tauri's default: `RequestExecutionLevel user` and `$INSTDIR` =
`$LOCALAPPDATA\<productName>` [T-NSI][T-CFG]; `both` needs an elevated installer [T-WIN], so per-machine waits for an
enterprise demand.

**A collision to fix first, in the local state only.** That folder, `%LOCALAPPDATA%\LitePack`, is also where E5-2 keeps
the GUI's journal and WebView2 data (`crates/lpk-gui/src/paths.rs:11`) and where E5-3 and E6-2 put `Open\` and
`Verbs\`; the settings, `%APPDATA%\LitePack\settings.json` (E5 §5 chose `config_dir` over `app_config_dir`), live
elsewhere and do not collide. Tauri's uninstaller deletes its own files and then `RMDir "$INSTDIR"` without `/r`; its
"delete the application data" box removes `$APPDATA\<identifier>` and `$LOCALAPPDATA\<identifier>` [T-NSI]. Proposed:
the local state moves to Tauri's `app_local_data_dir` (`%LOCALAPPDATA%\<identifier>`, also the webview's default data
folder [T-PATH]), so program and state never share a folder and the portable override covers it (§5). The settings stay
where E5 §5 put them; with the box ticked, the POSTUNINSTALL hook removes them too, reading the template's
`$DeleteAppDataCheckboxState` [T-NSI], a coupling E7-5's test guards. Moving the settings to `app_config_dir` as well,
so that Tauri's own branch removes them, would reverse E5 §5's choice: a separate, optional decision (§10 Q9). Rejected
unless the owner prefers that folder: forking `installer.nsi` (`nsis.template` [T-CFG]) to install under
`%LOCALAPPDATA%\Programs`, a three-way merge at every Tauri bump. `lpk-shell` takes the identifier as a constant,
compared with `tauri.conf.json` by a test.

**Layout.** `$INSTDIR\lpk-gui.exe` (Tauri keeps the main binary at the root) and `uninstall.exe`; `bin\` with
`lpk.exe`, `lpk-check.exe` and the workers, placed by `resources`' source-to-target map [T-CFG];
`shell\<version>\lpk_shell.dll` (a loaded DLL cannot be replaced, E6 risk (2)); `identity\` with the sparse package; the
notices and licences. The broker starts the worker beside its host (`crates/lpk-core/src/foreign.rs:203–211`), so the
GUI sets `Options::worker` to `bin\lpk-worker.exe` when that file exists: only `bin\` goes on PATH, and `uninstall`
never becomes a command.

**Hooks** (`NSIS_HOOK_PREINSTALL`, `…_POSTINSTALL`, `…_PREUNINSTALL`, `…_POSTUNINSTALL` in `installerHooks` [T-WIN]):
- PREINSTALL: below build 19045, a message naming Windows 10 22H2 and an exit before any file is written
  (`${AtLeastBuild} 19045` from `WinVer.nsh`, which needs the installer's `ManifestSupportedOS` to cover Windows 10
  [WINVER]; E7-5 checks it on a VM); the x64 installer on an ARM64 machine points to the ARM64 build
  (`${IsNativeARM64}`, `x64.nsh` [X64NSH]), because an x64 `lpk_shell.dll` cannot load into ARM64 Explorer (E6 §1).
- POSTINSTALL: `lpk-gui.exe --register-shell` (E6 §3: the sparse package with `$INSTDIR` as its external location, or
  route C; the `.lpk` association; `GuiPath`), and `$INSTDIR\bin` appended to `HKCU\Environment`'s `Path` by the same
  Rust code (no NSIS string-length limit), recorded in E6's "as found" list; earlier `shell\<version>` folders no longer
  loaded are deleted.
- PREUNINSTALL, skipped under `/UPDATE`: `lpk-gui.exe --unregister-shell` (package, keys, the PATH entry by value), and
  `bin\lpk-worker.exe --remove-profile`, the AppContainer profile E4-1 leaves to the uninstaller (D-97 (3);
  `workers/lpk-worker/src/main.rs:12`).
- POSTUNINSTALL, skipped under `/UPDATE`: `%LOCALAPPDATA%\<identifier>` (journal, WebView2 data, opened entries) removed
  always; `%APPDATA%\LitePack` (settings) removed only with the box; the bookkeeping Tauri keeps without the box,
  `HKCU\Software\<publisher>\<product>` and its `Installer Language` value [T-NSI], removed always; a `shell\` DLL that
  Explorer still holds is removed at the next logon by a per-user `RunOnce` command.

**Switches** [T-NSI][NSIS-CLI]: `/S` silent (installer and uninstaller), `/P` passive (a progress bar), `/NS` no
shortcuts, `/D=<dir>` last and unquoted, `/R` relaunch after a silent or passive install; `/UPDATE` and `/ARGS` are the
updater's. `docs/RELEASING.md` lists them; winget's `nullsoft` type sets the silent switches itself [WG-MAN].

**WebView2.** `downloadBootstrapper`, silent (Tauri's default [T-CFG], decision E5 (f)): the installer reads `pv` under
`EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}` (HKLM, then HKCU) and runs Microsoft's bootstrapper only
when it is missing [T-NSI][WV-DIST]. Windows 11 ships the runtime and "the vast majority" of Windows 10 devices have it;
Edge never stands in [WV-DIST], so `docs/RELEASING.md:19` and E7-5's "through Edge" are reworded (§11). A non-elevated
bootstrapper installs per user unless Edge's machine-wide updater takes over [WV-DIST].

**Compression.** NSIS's LZMA module is CPL-1.0 (with a linking exception), its zlib module zlib/libpng [NSIS-LIC], and
Tauri defaults to LZMA [T-CFG]. CPL-1.0 is not on `docs/LICENSING.md`'s list: `compression: "zlib"` unless the owner
approves CPL-1.0 for the installer stub (§10 Q6).

**Upgrade.** Tauri's installer closes a running app through the Restart Manager, which sends `WM_ENDSESSION`
[T-BUN210], E5's exit path. Through the updater (`/UPDATE`) the old uninstaller does not run at all and the new files
overwrite the old [T-NSI]; POSTINSTALL registers the higher sparse package version, an in-place package update. A
manual upgrade through Tauri's reinstall page runs the old uninstaller without `/UPDATE` [T-NSI], so the hooks see an
uninstall and then an install: registration redone, the AppContainer profile and the caches made again on use,
settings kept unless the user ticks the box. Upgrading while `lpk.exe` runs is in E7-5's test, its outcome recorded.

**Residue check** (`scripts/e7-residue.ps1`, E7-5's acceptance, `windows-2025`): snapshots of `%LOCALAPPDATA%`,
`%APPDATA%`, the Start menu, the desktop, `HKCU\Software` (with `Classes`), `HKCU\Environment`, `Get-AppxPackage` and
the AppContainer `Mappings` key, taken before install and after a silent uninstall that followed a GUI smoke run and a
foreign archive. Allowed differences: `%APPDATA%\LitePack\settings.json`, and Windows' own bookkeeping, which running
any program changes — Explorer's and the shell's caches and usage records under `HKCU\Software\Microsoft\Windows` and
`HKCU\Software\Classes\Local Settings` (MUI cache, UserAssist, recent documents, AppCompat flags, and the like) — named
one by one in the script's allow-list, each entry justified there, and everything else fails.

## 4. MSIX for the Store, and the sparse package (E7-4, E6-3)

Two packages with two identities, both fixed by E7-1's answers.

**The Store bundle.** Tauri makes EXE and MSI only [T-STORE], so E7-4 builds the package with `MakeAppx` from the
release binaries and bundles x64 and ARM64 [MSIX-REQ]. `Name`, `Publisher` and `PublisherDisplayName` are Partner
Center's values for the reserved name, case-sensitive [MSIX-REQ]; the Store replaces any signature on an MSIX with its
own, so no trusted certificate, token or HSM is involved [MSIX-REQ][CSO]. The manifest: `runFullTrust`; E6-2's
`com:SurrogateServer` and `desktop4:FileExplorerContextMenus` fragment (E6 §3); `uap5:AppExecutionAlias` `lpk` with the
console subsystem (Windows 10 1709 and later [ALIAS]); `.lpk` as its only file type (E6 §3); `MinVersion` 10.0.19045.0
(decision E5 (b)); version `MAJOR.MINOR.PATCH.0` (§1). Its `lpk-gui.exe` is the build without the updater (§6): the
Store updates it. For E7-4's install test, CI signs a copy with a throwaway certificate whose subject is the Store
`Publisher`, trusted only on the runner, and runs WACK on it [MSIX-REQ].

**WebView2 in the MSIX.** PLAN's "offline-installer variant for the Store package" cannot apply to an MSIX: the offline
installer is a requirement of the Store's EXE/MSI route (policy 10.2.9: a standalone installer, no downloader stub
[POL]; Tauri's Store guide [T-STORE]), and for MSIX Microsoft documents only App Installer's dependency element
[WV-DIST] (whether the Store honours it is *unverified*). Proposed: the MSIX relies on the Evergreen runtime, checks it at
launch (`pv`, or `GetAvailableCoreWebView2BrowserVersionString` [WV-DIST]) and names Microsoft's download page in a
native message, the detection and message policy 10.4.1 asks for [POL]; the listing discloses the dependency at the top
of its description (10.2.4 [POL]). Rejected: the Fixed Version runtime, "over 250 MB" and never updated by Microsoft
[WV-DIST]. Fallback if the MSIX fails certification: Tauri's documented route, the NSIS installer with `offlineInstaller`
submitted by URL (versioned and never changed, silent, every PE signed under a CA of the Microsoft Trusted Root Program
[POL][MSI-REQ]). Artifact Signing cannot serve it: its CAs are no longer in the Trusted Root Program, though Windows
still trusts them through the Platform Trust List [AS-CM]. That fallback needs an OV certificate (US$150–300 a year per
Microsoft, *unverified* [CSO]) with its token or cloud HSM (§2), on top of Artifact Signing (§10 Q2).

**Open under MSIX** (E7-4 checks each): the broker grants the AppContainer SID read and execute on the worker image at
every launch (`crates/lpk-core/src/foreign.rs:636–658`), a change the user is unlikely to be allowed in `WindowsApps`
(*unverified*), so an LPAC worker starting from the package folder is unproven; MSIX has no uninstall action, so the
`LitePack.Worker` profile would outlive a Store uninstall, against policy 10.2.7's clean uninstall [POL]; HKCU writes
may be redirected (E6 §3).

**The sparse package (E6-3).** `Publisher` equal to the certificate's subject DN as issued (shaped like `CN=…, O=…, L=…,
S=…, C=…` [AS-RR]), `uap10:AllowExternalContent`, `ProcessorArchitecture="neutral"`, `runFullTrust` and
`unvirtualizedResources`, `AppListEntry="none"` [GI]; `MinVersion` 10.0.19045.0, the product's minimum (19041 is the
feature's floor [GI], which E6 §3 used); the product's version, so reinstalling one version unregisters first [GI];
signed in `package` with the production profile, since users' machines need a production certificate [GI] (with the
test profile in a rehearsal, whose runners trust the test root, §1); development and CI tests keep E6 §3's
`CN=LitePack Test`. A later change of publisher changes the package family: Windows 11 21H2 and
later can bridge it with an artifact signed by the old certificate, Windows 10 cannot [MSIX-PI]; the installer
re-registers, and SmartScreen reputation starts over for a new publisher [SSR].

## 5. The portable zip and the CLI zip (E7-6)

**Portable.** An empty `portable` marker beside `lpk-gui.exe` makes the GUI set Tauri's app-directories override to
`<folder>\data` at start, and point its own settings path (E5 §5's `config_dir`, which the override does not cover) at
the same folder; Tauri documents the override for portable apps, relative paths resolving next to the executable,
which must be writable, with a dedicated subfolder rather than `./` [T-CFG] (a runtime form is shown in its docs; E7-6
confirms it in 2.12.5). If `data\` cannot be written the GUI says so and exits, never falling back to `%APPDATA%`. No
shell registration (that is the installer's); the WebView2 check of §4 at start (E5 §5); the update check only
notifies with a link to the release page (§6), since its installer would turn a portable copy into an installed one.
The one footprint outside the folder is the `LitePack.Worker` AppContainer profile, created when a foreign archive is
first opened (D-97 (3)); a button in the about box and `lpk-worker --remove-profile` remove it. PLAN's acceptance
looks only at `%APPDATA%`; proposed: the snapshot covers `%APPDATA%`, `%LOCALAPPDATA%` and `HKCU`, with the profile
after a foreign archive and §3's allow-list of Windows' own entries as the only exceptions.

**CLI zip.** `lpk.exe`, `lpk-check.exe`, the workers and the notices; no settings, no network; the same profile note.

## 6. The updater (E7-7)

**Plugin and endpoint.** `tauri-plugin-updater` 2.14.1 (the current release [UPD-SRC]; the `D-NN` pins it), called
from Rust alone (E5 §4). One endpoint, `https://github.com/<owner>/<repo>/releases/latest/download/latest.json`
[GH-LINK]; "latest" skips drafts and pre-releases [GH-LATEST]. Release builds refuse a non-HTTPS endpoint, but a
download URL only draws a warning [UPD-CFG][UPD-SRC], so the GUI refuses a `url` outside
`https://github.com/<owner>/<repo>/releases/download/` before downloading. Timeouts are set on the check and the
download, which have none by default [UPD-SRC]. The request carries the plugin's own User-Agent and no URL variable
(E5 §4).

**`latest.json`.** `version`, `notes`, `pub_date`, and `platforms` with `windows-x86_64` and `windows-aarch64`, each the
`url` of that release's NSIS installer and its `signature` (the `.sig` content) [T-UPD]; no installer-suffixed keys,
since no bundle type is patched in (§1) and `{os}-{arch}` is always looked up [UPD-SRC].

**Signatures.** minisign over the signed installer, made last (§1). `requireSignedVersion: true` from the first release:
the endpoint's response travels over TLS but is unsigned, and without this option a crafted manifest can install any
older signed release (CVE-2026-95625, published by the JFrog CNA, counts every version from 2.0.0 as affected because
the 2.12.0 fix is opt-in [CVE-95625][UPD-CFG]); tauri-cli 2.12.0 and later record the version in the signature's
trusted comment [T-CLI], and no earlier release exists to re-sign.
`allowDowngrades` stays false [UPD-CFG]. Proposed procedure (§10 Q7): the owner runs `scripts/updater-sign.sh <tag>` in
a session with no agent running, the key's removable medium plugged in for that step alone; it downloads the draft's
installers, checks `SHA256SUMS` and `gh attestation verify`, runs `tauri signer sign --app-version <version>` [T-CLI],
writes `latest.json` and uploads both to the draft; a dispatched check verifies them against the committed public key
before the owner publishes.

**Flow.** With the check on: at most once a day, after the window opens; a newer version shows its notes with "Install
and restart", "Later" and "Skip this version", and nothing installs without that click. Before installing, the GUI runs
its quit path (E5 §1: ask while an operation runs, cancel, close the sessions); `install` then starts the installer with
`/P /UPDATE` (`passive` [UPD-CFG]) and exits the process [UPD-SRC][T-UPD]; per user, no UAC.

**Default and silence** (decision E5 (d), §10 Q4). On by default and switchable off; off means no request at all, the
check never called. Portable: notify only. MSIX: compiled out by a cargo feature of `lpk-gui` that the Store build turns
off, with a manifest test that the Store build's tree has no updater plugin; the Store updates it. The CLI makes no
network call.

**Keys.** Each release compiles the public key in; losing the private key ends updates for every installed copy [T-UPD];
rotating it takes one release, signed with the old key, that carries the new public key. A leaked key cannot be revoked
in installed copies, which is why it stays on removable media, off GitHub and out of agent sessions (§2).

**Tests** (E7-7's acceptance; a local server stands in for GitHub, plain HTTP on localhost allowed by the test
configuration alone, and a config test asserts the shipped configuration sets no `dangerous*` option [UPD-CFG]): N
updates to N+1; a bad signature in the manifest, a tampered payload, and a validly signed older release announced as a
newer version are each refused; a `url` off the release prefix is refused; with the check off the server sees no
request.

## 7. Third-party notices in every artefact (E7-8)

**Coverage today.** `THIRD-PARTY-NOTICES` covers `lpk`, `lpk-check` and `lpk-gui`
(`scripts/third-party-notices.sh:12–15`). Missing from what ships: `lpk-worker` and the coming `lpk-worker-libarchive`
(the `workers/` workspace and its own lockfile), `lpk_shell.dll` (the `windows` crates), libarchive's `COPYING` (E4-4),
the NSIS stub and plugins inside each installer, and the WebView2 loader's licence text (`about-gui.hbs:12–14`, the E7
blocker of `docs/LICENSING.md`).

**The WebView2 text, from the package itself at build time.** `webview2-com-sys` 0.39.1 vendors
`WebView2LoaderStatic.lib` per architecture and names no SDK version [WV2SYS][WV2CL] (0.38.2 named 1.0.3650.58; 0.39.0
says only that it updated the SDK). E7-8 finds the source once: the `Microsoft.Web.WebView2` NuGet version whose `x64`
and `arm64` `WebView2LoaderStatic.lib` equal the vendored files byte for byte. A committed `notices/webview2.toml`
records that version, the package's hash and both library hashes, beside the `LICENSE.txt` extracted from that package.
CI downloads that exact package from nuget.org, checks its hash, extracts `LICENSE.txt` again and compares it with the
committed copy, and checks the library hashes against the crate in the cargo registry, so a `webview2-com-sys` bump with
other libraries fails until the pin moves. The licence published with the package (read for 1.0.4314.35) is BSD-style
and asks binary redistributions to reproduce the notice, conditions and disclaimer in their documentation [WV2LIC];
`about-gui.hbs` renders the pinned package's text verbatim.

**The installer's own code.** The NSIS stub, its compression module (zlib, §3) and the plugins Tauri's template uses,
their licence texts taken from the NSIS distribution the bundler fetches [NSIS-LIC], in a static section.

**Shipped and checked.** `THIRD-PARTY-NOTICES.txt`, `LICENSE-APACHE` and `LICENSE-MIT` at the root of each zip, in
`$INSTDIR` and in the MSIX; the GUI's about box opens the first. A script unpacks every artefact and compares the file
with the repository's. LIC-2's check already fails when `Cargo.lock` changes without the regenerated file; it gains
`workers/Cargo.lock`.

## 8. winget and the Microsoft Store (E7-9)

**winget first** [WG-SUB][WG-MAN]. After publishing, a workflow writes the version, default-locale and installer
manifests (schema 1.12.0) from the release: `PackageIdentifier` `<Publisher>.<Product>` (pending), `InstallerType:
nullsoft`, `Scope: user`, the x64 and ARM64 installers with their SHA-256, `UpgradeBehavior: install`, `Publisher` and
`PackageName` matching ARP, `License: Apache-2.0 OR MIT`, `PrivacyUrl` from E9-7, `ReleaseNotesUrl`; `winget validate`
runs where the runner has winget, else on the owner's machine. GitHub release URLs are what Microsoft's own manifest
example uses [WG-MAN]. The pull request to `microsoft/winget-pkgs` (one package version per PR) is the owner's step; the
pipeline installs and uninstalls for administrators and standard users, checks that the uninstall cleans up
(`Validation-Uninstall-Error`), scans with antivirus engines and checks URL reputation, then a moderator approves
[WG-SUB]. Later versions go through `wingetcreate` too [WG-MAN].

**Then the Store** (decision (b)): the MSIX route of §4, under an account whose type matches §10 Q1. The listing needs
the product name, the privacy policy URL (E9-7; Win32 products always need one, 10.5.1 [POL]), the final icons (decision
E5 (e)), the WebView2 disclosure (§4) and the age-rating answers; the certification report is committed.

## 9. The E7 gate on fresh VMs (E7-10)

**Machines.** Windows 10 22H2 x64; Windows 11 x64 installed clean with Smart App Control on [SSR]; Windows 11 ARM64 when
the owner has a device (decision (h)). A local account `tester`, screenshots cropped to the window (as E6-7).

**Checklist per machine** (OS build, date, pass or fail with the reason; committed):
1. Release N downloaded in Edge: the SmartScreen prompt shows the publisher's name, or there is no prompt and the file's
   Digital Signatures tab shows it (reputation may already exist [SSR]); a screenshot.
2. Install with the defaults: no UAC prompt; the Start menu entry; `lpk --version` in a new terminal; an archive opens
   by double-click; the Explorer submenu (E6-7's record); whether the WebView2 runtime was present before (`pv`).
3. Silent install `/S` on a fresh snapshot.
4. N to N+1 through the in-app updater (prompt, passive progress, relaunch, settings kept), and through winget.
5. Uninstall from Settings, and with `uninstall.exe /S`: the residue script finds only the settings file and its
   allow-listed Windows entries (§3); the sparse package and the AppContainer profile are gone; after the next logon
   nothing is left under `$INSTDIR`.
6. The portable zip from Downloads: §5's snapshot.
7. Once listed: the Store's install, launch and uninstall.

If no VM lacks the WebView2 runtime, the record says so: Windows 11 includes it in the OS [WV-DIST], so only a Windows
10 22H2 image without it exercises the installer's bootstrapper path, which CI cannot reach either.

## 10. The owner's decisions

1. **Publisher legal name, product name, identifier, trademark search** (D-89 (6), E7 (c)). An individual's name
   appears with city, province and country in every signature, and on SmartScreen, ARP, winget and the Store
   [AS-QS][SSR]. A company needs a registered entity (Artifact Signing: a business identifier, a domain e-mail, current
   public records, 1–20 business days; its organisation route also takes "an Organization or a DBA", proven by
   documents such as a business registration form [AS-QS]; the Store: D-U-N-S or documents [ACCT]). D-01 keeps a
   closed GUI Pro edition open, and selling it would be acting "in relation to their trade or profession", for which
   Store policy 10.14 requires a company account [POL]; an individual account cannot become one [ACCT]. Switching later
   means a new identity validation, a new sparse-package family (no bridge on Windows 10 [MSIX-PI]), SmartScreen
   reputation from zero [SSR] and a new Store account [ACCT]. Recommendation: decide before E7-1 starts; if the Pro
   edition is meant to be sold, form and validate the company first; otherwise the individual's legal name.
2. **The certificate route.** Artifact Signing (Basic about US$9.99/month per Microsoft Learn, *unverified* on the
   pricing page; individuals Completed minutes after Verified ID succeeds, organisations 1–20 business days; OIDC, no
   key to hold) or OV (US$150–300/year per Microsoft, *unverified*; several business days; a token or cloud HSM; at most
   460 days) [AS-QS][CSO][CSBR]. The Store's EXE fallback (§4) takes only a Trusted Root Program CA, which Artifact
   Signing's are no longer [AS-CM][POL]: if the MSIX route fails, an OV certificate is needed as well, at that cost.
   Recommendation: Artifact Signing if Q3's answer is yes, OV otherwise; an OV certificate only if the fallback is used.
3. **Can an individual use Artifact Signing?** Yes, when located in the US or Canada, with a pay-as-you-go subscription
   and an Individual-type billing account matching the ID [AS-QS][AS-FAQ]. Billing is not pro-rated and starts with the
   account, so a validation that fails still costs a full month of the SKU; Microsoft advises deleting the account after
   a failure to stop the billing [AS-FAQ]. Recommendation: open the subscription and start the validation now; its
   outcome is the answer.
4. **The update default** (E5 (d)): on, once a day, asking before it installs, switchable off; notify-only when
   portable; none in the MSIX. Archivers are a recurring exploit target (the CVEs of E6 §4), which favours on.
   Recommendation: on.
5. **Paid accounts.** The Azure subscription for Artifact Signing (or, instead, the OV certificate with its token or
   cloud HSM), plus an OV certificate only if the Store's EXE fallback is ever used (Q2). Free: the Store account in the
   new flow [ACCT], winget, GitHub for this public repository (environments on the Free plan need a public repository
   [GH-ENV]; the plan terms of attestations are not stated [GH-ATTC], *unverified*). The trademark search and any domain
   are outside this note. Recommendation: the Azure subscription alone.
6. **CPL-1.0 for the NSIS stub** (the licence list is the owner's). Recommendation: no; `compression: "zlib"`.
7. **Where the updater key signs.** The owner's machine (proposed: the key cannot be revoked in installed copies, so it
   stays where neither a GitHub compromise nor an agent can reach it) or a secret of the `release` environment (fully
   automatic, guarded only by the approval). The owner's machine also runs agents and build scripts with a `gh` token
   that, if it is a classic token with `repo`, can approve deployments [GH-REV]; so the proposal costs: the key on
   removable media behind its password, two copies kept apart; a signing step at every release done by hand in a
   session with no agent running; and a separate fine-grained token for agents, without the Deployments permission
   [GH-PAT], to create and renew. Recommendation: the owner's machine, with those three.
8. **Settings the owner applies:** the `release` and `rehearsal` environments (§2), a tag ruleset, immutable releases,
   the two Entra app registrations with their federated credentials, and the agents' token. Recommendation: all before
   E7-3.
9. **Optional: the settings folder** (§3). Keep E5 §5's `%APPDATA%\LitePack` (the uninstall hook honours Tauri's box)
   or move the settings to `app_config_dir` (`%APPDATA%\<identifier>`), which Tauri's own branch removes, reversing
   E5 §5's choice and tying the folder's name to the identifier. Recommendation: keep E5 §5's folder.

## 11. Order, acceptance, decisions, scope, risks

Order: E7-1 now (§10; its lead time is the schedule's) → E7-8 (needs no certificate) → E7-2 rehearsed with the test
profile → E7-3 → E7-5 with E6-3 → E7-6 → E7-7 → E7-4 → E7-9 → E7-10; E8-6 and E8-8 ride on E7-2; E5-2's state
folder moves, and §10 Q9 is answered, before E7-5.

| Task | Acceptance additions | `D-NN` records |
|---|---|---|
| E7-1 | §10's answers in `docs/RELEASING.md` "Accounts"; the OIDC subjects from test tokens | identity, route, accounts, key custody, agents' token |
| E7-2 | a rehearsal green through `rehearsal` with the test root; the release-hooks check covers `lpk-gui` | job graph, `tools` and `assemble`, the signer's allow-list, the `rehearsal` environment, `--no-binary-patching` |
| E7-3 | every PE inside every artefact, each on `signed.sha256`; `/tw` warnings fail; the subject compared | signing order, timestamps |
| E7-4 | the test-signed install, the alias, the WebView2 message, LPAC from `WindowsApps` | Store identity, runtime rule, updater-free build, the OV-only fallback |
| E7-5 | §3's layout, hooks and residue list with its Windows allow-list; the refusal below 19045 | state folder, compression, PATH |
| E7-6 | §5's wider snapshot | marker, notify-only |
| E7-7 | §6's six tests | plugin pin, `requireSignedVersion`, flow |
| E7-8 | workers, shell DLL, libarchive, NSIS, the WebView2 pin | the pin procedure |
| E7-9 | `winget validate`; the certification report | channels |
| E7-10 | §9's checklist | the gate record |

**PLAN edits proposed.**
- E7-2: the assets per architecture are the NSIS installer, the portable zip, the CLI zip, `latest.json` with the
  `.sig`s, `SHA256SUMS` and the SBOMs; the sparse package ships inside the installer; the Store bundle is a workflow
  artifact, not a release asset; the `tools` and `assemble` jobs keep tool builds, zips and verification away from the
  credential; the acceptance's test tag is preceded by a branch rehearsal through the `rehearsal` environment.
- E7-3: "every artefact" includes the PEs inside installers and zips; `/tw` warnings fail.
- E7-4: "with the offline-installer variant of the WebView2 runtime for the Store package" becomes "relying on the
  Evergreen runtime with a launch-time check and message (Store policy 10.4.1); the offline installer only on the EXE
  fallback (10.2.9), which needs an OV certificate"; plus the Store identity, the unsigned upload, the test-signed CI
  copy, no updater. It revises E5 (f)'s Store half, so it needs a `D-NN`.
- E7-5: "Windows 10 22H2 has it through Edge" becomes "most Windows 10 devices have the runtime; Edge does not count"
  (also `docs/RELEASING.md:19`); add the state folder, `compression: zlib`, the hooks, PATH and the residue list with
  its allow-list of Windows' own entries.
- E7-6: the snapshot covers `%APPDATA%`, `%LOCALAPPDATA%` and `HKCU`, the AppContainer profile listed.
- E7-7: `requireSignedVersion`, the download-URL check; the acceptance adds an older signed release announced as newer.
- E7-8: the notices cover the worker workspace, `lpk-shell`, libarchive, the NSIS stub and the WebView2 text pinned by
  library hash.
- E7-10: Smart App Control on the Windows 11 VM; "SmartScreen shows the publisher" becomes "the prompt, or the signature
  when no prompt appears, shows the publisher".
- E5-2 (follow-up) and E5-7: the local state moves to `app_local_data_dir` (`paths.rs`), which the collision needs (a
  `D-NN`); the settings stay in `%APPDATA%\LitePack` unless §10 Q9 moves them, which would change E5-7's text.
- E8-5: the compare set gains both `lpk-gui.exe` variants and `lpk_shell.dll`, built with `cargo auditable`.
- E8-6: `actions/attest@v4` over the final signed bytes; one CycloneDX SBOM per binary.
- E8-8: §1's versioning rule (first Store version 1.0.0 or later, a PATCH bump per release, pre-releases never latest).

**Out of Phase 1.** Per-machine installs and an MSI; delta updates; an ARM64X shell DLL; Intune; macOS and Linux
packages.

**Risks.**
1. E7-1's lead time; the term of a validation (*unverified*).
2. SmartScreen has no threshold: "several weeks and hundreds of clean installs" in Microsoft's words [SSR]. The durable
   identity across Artifact Signing's daily certificates is the profile's EKU [AS-CM]; whether SmartScreen keys
   reputation on it is not documented (*unverified*).
3. No NuGet release may match the vendored WebView2 libraries byte for byte; then the crate's maintainer is asked, and
   no GUI binary ships in the meantime.
4. Tauri: the template and bundler source read here are the `dev` branch's [T-NSI][T-NSIS-RS] (E7-5 checks the pinned
   version's); bundling the ARM64 target from prebuilt binaries on x64; the GUI's reproducibility (each *unverified*).
5. MSIX: LPAC from `WindowsApps`, the profile left behind, certification of a full-trust package with a COM surrogate
   (*unverified*). The EXE fallback stands but needs an OV certificate, US$150–300 a year per Microsoft (*unverified*)
   [CSO][AS-CM].
6. winget's uninstall check against kept settings; winget and WACK on `windows-2025` (*unverified*).
7. Supply chain: what stays trusted beside the credential is the pinned tauri-cli and NSIS, the bundler's own pins for
   NSIS being SHA-1 [T-NSIS-RS] (`tools.sha256` adds SHA-256); the `azure/*` and `actions/*` actions are pinned by
   commit SHA.
8. Windows 10 Enterprise LTSC 2021 reports build 19044 (*unverified*) and would be refused by D-89 (7)'s minimum.
9. `tauri-plugin-updater` 2.14.1 against `tauri` =2.12.5, and the trusted comment tauri-cli 2.12.4 writes for
   `requireSignedVersion` (*unverified*; E7-7's tests are the proof).
10. The rehearsal's test root: Microsoft publishes none, so resolving it from a test signature's chain is *unverified*;
    without it, rehearsals check signing and packaging but not `/pa` or the sparse package's registration.

[T-WIN]: https://v2.tauri.app/distribute/windows-installer/
[T-CFG]: https://v2.tauri.app/reference/config/
[T-UPD]: https://v2.tauri.app/plugin/updater/
[T-STORE]: https://v2.tauri.app/distribute/microsoft-store/
[T-NSI]: https://raw.githubusercontent.com/tauri-apps/tauri/dev/crates/tauri-bundler/src/bundle/windows/nsis/installer.nsi
[T-BUN210]: https://v2.tauri.app/release/tauri-bundler/v2.10.0/
[T-BUN14]: https://v2.tauri.app/release/tauri-bundler/v1.4.0/
[T-CLI]: https://v2.tauri.app/release/tauri-cli/all-versions/
[T-PATH]: https://docs.rs/tauri/2.12.5/tauri/path/struct.PathResolver.html
[UPD-SRC]: https://docs.rs/crate/tauri-plugin-updater/latest/source/src/updater.rs
[UPD-CFG]: https://docs.rs/crate/tauri-plugin-updater/latest/source/src/config.rs
[T-NSIS-RS]: https://raw.githubusercontent.com/tauri-apps/tauri/dev/crates/tauri-bundler/src/bundle/windows/nsis/mod.rs
[CVE-95625]: https://www.cve.org/CVERecord?id=CVE-2026-95625
[AS-QS]: https://learn.microsoft.com/en-us/azure/artifact-signing/quickstart
[AS-FAQ]: https://learn.microsoft.com/en-us/azure/artifact-signing/faq
[AS-INT]: https://learn.microsoft.com/en-us/azure/artifact-signing/how-to-signing-integrations
[AS-RR]: https://learn.microsoft.com/en-us/azure/artifact-signing/concept-resources-roles
[AS-CM]: https://learn.microsoft.com/en-us/azure/artifact-signing/concept-certificate-management
[AS-TM]: https://learn.microsoft.com/en-us/azure/artifact-signing/concept-trust-models
[AS-PRICE]: https://azure.microsoft.com/en-us/pricing/details/artifact-signing/
[AS-ACT]: https://github.com/Azure/artifact-signing-action
[CSO]: https://learn.microsoft.com/en-us/windows/apps/package-and-deploy/code-signing-options
[SSR]: https://learn.microsoft.com/en-us/windows/apps/package-and-deploy/smartscreen-reputation
[CSBR]: https://cabforum.org/working-groups/code-signing/requirements/
[ST]: https://learn.microsoft.com/en-us/windows/win32/seccrypto/signtool
[GI]: https://learn.microsoft.com/en-us/windows/apps/desktop/modernize/grant-identity-to-nonpackaged-apps
[MSIX-PI]: https://learn.microsoft.com/en-us/windows/msix/package/persistent-identity
[MSIX-REQ]: https://learn.microsoft.com/en-us/windows/apps/publish/publish-your-app/msix/app-package-requirements
[MSI-REQ]: https://learn.microsoft.com/en-us/windows/apps/publish/publish-your-app/msi/app-package-requirements
[ALIAS]: https://learn.microsoft.com/en-us/uwp/schemas/appxpackage/uapmanifestschema/element-uap5-appexecutionalias
[WV-DIST]: https://learn.microsoft.com/en-us/microsoft-edge/webview2/concepts/distribution
[WV2LIC]: https://www.nuget.org/packages/Microsoft.Web.WebView2/1.0.4314.35/License
[WV2SYS]: https://docs.rs/crate/webview2-com-sys/0.39.1/source/build.rs
[WV2CL]: https://docs.rs/crate/webview2-com-sys/0.39.1/source/CHANGELOG.md
[POL]: https://learn.microsoft.com/en-us/windows/apps/publish/store-policies
[ACCT]: https://learn.microsoft.com/en-us/windows/apps/publish/partner-center/open-a-developer-account
[WG-SUB]: https://learn.microsoft.com/en-us/windows/package-manager/package/repository
[WG-MAN]: https://learn.microsoft.com/en-us/windows/package-manager/package/manifest
[NSIS-LIC]: https://nsis.sourceforge.io/License
[NSIS-CLI]: https://nsis.sourceforge.io/Docs/Chapter3.html
[WINVER]: https://raw.githubusercontent.com/kichik/nsis/master/Include/WinVer.nsh
[X64NSH]: https://raw.githubusercontent.com/kichik/nsis/master/Include/x64.nsh
[7Z]: https://www.7-zip.org/
[GH-ATT]: https://docs.github.com/en/actions/how-tos/secure-your-work/use-artifact-attestations/use-artifact-attestations
[GH-ATTC]: https://docs.github.com/en/actions/concepts/security/artifact-attestations
[GH-IMM]: https://docs.github.com/en/code-security/supply-chain-security/understanding-your-software-supply-chain/immutable-releases
[GH-ENV]: https://docs.github.com/en/actions/how-tos/deploy/configure-and-manage-deployments/manage-environments
[GH-PAT]: https://docs.github.com/en/rest/authentication/permissions-required-for-fine-grained-personal-access-tokens
[GH-REV]: https://docs.github.com/en/rest/actions/workflow-runs#review-pending-deployments-for-a-workflow-run
[GH-OIDC]: https://docs.github.com/en/actions/concepts/security/openid-connect
[GH-OIDCR]: https://docs.github.com/en/actions/reference/security/oidc
[GH-LINK]: https://docs.github.com/en/repositories/releasing-projects-on-github/linking-to-releases
[GH-LATEST]: https://docs.github.com/en/rest/releases/releases#get-the-latest-release
[CDX]: https://docs.rs/crate/cargo-cyclonedx/latest
[AUD]: https://docs.rs/crate/cargo-auditable/latest
