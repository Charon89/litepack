# LitePack

Open-core, Windows-first archiver. `.lpk` beats 7-Zip/WinRAR/WinZip on real user data by recompressing
already-compressed files bit-exactly (JPEG, Deflate inside PDF/Office/PNG/APK), deduplicating across files,
and sealing everything in a modern container (recovery records, authenticated encryption, verifiable
integrity, random access, incremental updates) — with zstd-class extraction speed.

Status: **Phase 0 — measure before building.** See `docs/PLAN.md`. Method, evidence and competitor
analysis: `docs/LitePack-Method-v2.md`. Decisions: `docs/DECISIONS.md`.

Licence: Apache-2.0 OR MIT (engine, CLI, format, benchmark harness).

---

## Working with Claude Code (the intended workflow)

### Setting up a machine (Windows) — the same steps on every computer you work from
1. Install Git for Windows (gives Claude Code a Bash tool) with `winget install Git.Git`, then
   `git clone https://github.com/Charon89/litepack` and `cd litepack`.
2. Run `powershell -ExecutionPolicy Bypass -File scripts\setup-windows.ps1`. It is idempotent and installs whatever is
   missing: GitHub CLI, Visual Studio 2022 Build Tools (C++ workload; one UAC prompt), rustup with the toolchain from
   `rust-toolchain.toml`, `cargo-nextest` and `cargo-deny`. Re-run it whenever local and CI disagree.
3. `gh auth login` once per machine (needed to push).
4. Install Claude Code: native installer from https://code.claude.com/docs/en/setup (or `winget install Anthropic.ClaudeCode`). It also runs inside the Claude desktop app and the VS Code / JetBrains extensions.
5. Run `claude` in the repo. It reads `CLAUDE.md` automatically.

Building on Linux (CI, WSL) additionally needs OpenSSL headers: `sudo apt-get install -y --no-install-recommends libssl-dev pkg-config` (the corpus downloader uses native TLS; Windows needs nothing extra).

Switching computers: `git pull --ff-only` before you start, `git push` when you stop. Everything Claude needs (rules,
plan, decisions, agents, skills) is in the repo; the benchmark corpus is rebuilt locally from `corpus.lock` (D-13).

### The loop
- `/next-task` — picks the next unchecked task in `docs/PLAN.md`, implements it with the **implementer** subagent (fresh context, own git worktree), reviews it with the **reviewer** subagent, ticks the box with evidence. Run it again for the next task.
- `/bench run --tools all --profile small` — long measurement runs go through the **bench-runner** subagent; only summary tables come back into your conversation.
- `/review P0-3` — independent read-only review of a task's changes.
- For design questions, press `Shift+Tab` to enter plan mode first (reads, no edits), approve the plan, then `/next-task`.

### Which model for what (speed + token efficiency)
| Role | Model (alias) | Why |
|---|---|---|
| Main session (you talking to Claude Code) | `opus` for planning sessions; `sonnet` for routine task loops | The orchestrator holds judgment, not bulk work. Start it with `claude --model sonnet` on routine days and `claude --model opus` when designing or deciding. If your plan includes `fable`, use it for the Phase 0 verdict and the format spec (E1). |
| implementer subagent | `sonnet`, effort medium | Bulk coding of well-specified tasks; fast and far cheaper than opus. Runs in an isolated worktree so several can work in parallel. |
| reviewer subagent | `opus`, effort high, read-only | Catches what sonnet misses; cheap because it reads diffs, never writes. |
| bench-runner subagent | `haiku`, read-only | Mechanical: runs commands, returns tables. |
| researcher subagent | `sonnet` with web tools | One question, cited answer, 300 words. |

Change a role's model by editing `model:` in `.claude/agents/<name>.md` (aliases: `haiku`, `sonnet`, `opus`, `fable`, `inherit`). Check what your subscription offers with `claude --model <alias>`; heavy subagent use is much more comfortable on a Max plan than Pro.

### Token-efficiency rules (already encoded in CLAUDE.md and the agents)
- Subagents keep the main context small: they return summaries, never file dumps or JSON.
- One task per `/next-task`; `/clear` between unrelated tasks; `/compact focus on open questions and next task` when a session gets long; `/context` shows what is loaded.
- `CLAUDE.md` stays under 200 lines; long material lives in `docs/` and is read on demand.
- Agents use `Grep`/`Glob` rather than reading whole files, and never read `bench/results/*.json`.
- `maxTurns` caps on every agent stop runaway loops; `effort` is medium for implementation, high only for review.
- Parallelism: P0-4's four probes are independent — ask for "run P0-4 probes jpeg, deflate, dedup and text as four parallel implementer subagents"; each gets its own worktree. Or open parallel terminals with `claude --worktree probe-jpeg` etc.
- Use `claude -p "..."` (headless) in CI or scripts for repeatable chores (e.g., regenerate the report).

### Repository layout
```
CLAUDE.md                 project rules Claude Code reads every session
docs/PLAN.md              ordered tasks + acceptance criteria (the backlog)
docs/DECISIONS.md         append-only decision log
docs/CORPUS.md            benchmark corpus spec
docs/LICENSING.md         licence policy + patent/FTO list
docs/LitePack-Method-v2.md research & architecture
.claude/agents/           implementer · reviewer · bench-runner · researcher
.claude/skills/           /next-task · /bench · /review
crates/lpk-bench/         Phase 0 tool (corpus, run, probe, report)
bench/results/            committed JSON measurements (source of truth for every number)
bench/reports/            generated Markdown reports
deny.toml                 cargo-deny licence policy
scripts/setup-windows.ps1 one-shot machine setup (build tools, Rust toolchain, cargo tools)
.github/workflows/ci.yml  fmt · clippy · nextest · deny on Ubuntu + Windows
```

### First session script (copy-paste)
```
claude --model opus
> Read CLAUDE.md, docs/PLAN.md and docs/DECISIONS.md. Summarise Phase 0 in 10 lines, then run /next-task P0-1.
```
