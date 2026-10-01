---
name: review
description: Review the current branch's uncommitted or latest-commit changes with the reviewer subagent. Usage: /review [commit-range or task-id]
disable-model-invocation: true
---
Delegate to the **reviewer** subagent: "Review `$ARGUMENTS` (default: `git diff HEAD~1` plus working tree) against docs/PLAN.md acceptance criteria, CLAUDE.md rules and docs/LICENSING.md. Return verdict + findings."

Print the verdict and findings as returned. Do not fix anything yourself; if the user wants fixes, use /next-task with the task id.
