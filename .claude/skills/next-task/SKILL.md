---
name: next-task
description: Pick the next unchecked task in docs/PLAN.md, implement it with the implementer subagent, review it with the reviewer subagent, and report. Usage: /next-task [task-id]
disable-model-invocation: true
---
Run the LitePack task loop for one task.

1. If `$ARGUMENTS` names a task id (e.g., `P0-3`), use it; otherwise take the first task in `docs/PLAN.md` whose checkbox is `[ ]` and whose dependencies (earlier tasks in the same phase) are `[x]`.
2. Mark it `[~]` in `docs/PLAN.md`.
3. Delegate to the **implementer** subagent with: the task id, its full text and acceptance criteria, and the instruction to follow `CLAUDE.md`. Wait for its summary.
4. Delegate to the **reviewer** subagent with the task id and the implementer's summary. Wait for the verdict.
5. If REQUEST CHANGES: send the findings back to the same implementer (continue it) once; then re-review once. If still failing, stop and report to the user.
6. If APPROVE: ensure the checkbox is `[x]` with evidence, and print a 5-line summary: task, files changed, evidence commands, decisions appended, next task id.

Keep the main conversation free of file contents; relay only summaries.
