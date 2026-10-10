# Lane environment

This CPU-only lane has an immutable operating-system root filesystem.
Do not use sudo, apt or modify system files; install tools in user space.
Work in `/home/dev` and follow the applicable project's instructions.
Preserve existing authored changes, untracked inputs and other agents' work.
Check Git status before editing; follow the project's tests before publishing.

## Tools and files

Put local executables in `$HOME/.local/bin`, already on PATH.
Use project virtual environments and pinned project dependencies.
Use SDKMAN under `$HOME/.sdkman` for Java/Kotlin and supported JVM SDKs.
Do not overwrite shell startup files; verify installed tools in a fresh shell.
Do not use `/tmp` for anything; use `$HOME/.tmp` or project-local temporary files.
Keep credentials private and out of source, reports and notifications.

## Pi

Use normal `pi`; log in with `/login openai-codex` using your own subscription.
Default model is GPT-6 Sol with medium thinking; deliberate subscription choices persist.
Use `questionnaire` whenever clarification is needed, not only while planning.
When asked to propose or revise a plan, use `plan_propose` or `plan_update`.
No manual planning-mode entry/exit is needed; plan management does not restrict tools.
Plan revisions need human review; track implementation steps with evidence.
Use `lane_complain` for local environment feedback; it needs no approval.

## Coordination

Herdr is the lane session manager. Keep the human's workspace and focus undisturbed.
Agents may delegate concrete, bounded subtasks when useful and remain responsible for results.
Use `task_delegate`: helpers get a separate workspace, one agent per tab.
Use side-channel messages; agree ownership before overlapping edits.
No per-turn claim-file ceremony is required. Do not stop unrelated sessions.
Verify combined results; idle or a finished response alone is not task success.

## Optional capabilities

Check `p0_status`; mail, Matrix, memory and notifications require provisioned helpers.
External service changes retain their permissions independently of plan management.
Showing images to the human: `worklane-show-image` uses connected Ghostty graphics.
Read detailed automation, P0, collaboration or migration guides only when needed.
Report actual results and blockers concisely; do not invent success or ETAs.
