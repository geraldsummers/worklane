# Migrate a lane's agent instructions

Use this procedure when the user asks an agent to adopt the current Worklane guidance.
A new reference bundle alone does not authorize editing active instructions. Migration
updates documentation; it does not start services, launch jobs, install SDKs, or run a
repository release gate. Keep user rules and project-specific requirements intact.

## Locate the source and target

1. Resolve `$HOME/.local/share/worklane/agent-docs/current` once and retain that version's
   absolute path for this migration. Read its `REVISION`, `lane.md`, and `automation.md`.
   Do not edit the managed bundle. If it is absent, attach the lane using the updated
   Worklane binary on its owning host, then retry. No Worklane checkout is needed in the lane.
2. Record the exact Herdr identity and inspect live agents and claims before editing.
   Claim the instruction paths, backup directory, and migration record. Coordinate if
   another agent owns them; never overwrite another agent's claim.
3. Determine the Codex home used by the target agent (`CODEX_HOME`, otherwise `$HOME/.codex`).
   Inspect both `AGENTS.override.md` and `AGENTS.md`, including symlink targets and file
   contents. An override can shadow the base file; Codex ignores empty instruction files.
   Inspect applicable project instructions too, so repository policies are not mistaken for
   global lane defaults. Worklane bootstrap seeds only `$HOME/.codex`; a custom Codex home
   requires this explicit migration.
4. Locate any previous migration record under
   `$HOME/.local/state/worklane/agent-docs/migrations/`. Compare its source revision and
   post-migration checksums with the current files. The same revision and unchanged files
   mean no migration is needed. Changed files still need review; a revision marker does
   not authorize overwriting later customization.

See [Codex instruction discovery](https://learn.chatgpt.com/docs/agent-configuration/agents-md)
for override precedence and project scope. Do not remove an override merely to activate a
base file; merge the guidance into the effective file while preserving the override's purpose.
If the effective file contains no Worklane guidance, add the lane guidance alongside its
custom rules. If neither file exists, create `AGENTS.md` in the target Codex home.

## Back up and merge

1. Create a unique UTC-timestamped migration directory beneath
   `$HOME/.local/state/worklane/agent-docs/migrations/`, with directory mode 0700. Before
   editing, save the affected files' exact bytes with mode 0600 and record original paths,
   modes, symlink targets, and SHA-256 checksums. Record missing files as absent. For a
   symlink, back up both the link metadata and resolved file; preserve the link when editing.
2. Read the old instructions as a whole. Map each Worklane rule to the new lane guide,
   automation reference, or Worklane contributor guide. The new layout separates generic
   lane rules from release requirements for the Worklane repository. Preserve all local
   additions: SDK inventories, task preferences, project policies, and explicit exceptions.
3. Replace identifiable older Worklane boilerplate with the current `lane.md` content.
   Its installed automation path provides the detailed workflows. Keep unrelated custom
   text in clearly labeled sections. Do not replace the active file with a symlink to the
   managed bundle: later attaches must not change active instructions automatically.
4. Remove Worklane-specific release commands from global lane defaults only when they are
   identifiable stock boilerplate. They now belong in Worklane's repository `AGENTS.md`.
   Preserve deliberate local release rules in their applicable project. If a rule's origin
   or intended scope is ambiguous, retain it and ask the user to resolve that ambiguity.
5. Review the proposed diff against the backup. Every removed rule must be accounted for
   by an equivalent current rule, a relocated reference, or an explicit user decision.
   Verify source checksums again immediately before writing; concurrent changes require
   a fresh comparison. Write through a temporary file beside the target and rename it
   atomically, preserving permissions and any existing symlink indirection.

## Verify and record

- Confirm that custom rules, SDK inventories, and applicable project requirements remain.
  The lane guidance must not impose Worklane's lab or release toolchain on unrelated projects.
- Check that the installed automation and migration references exist and that the effective
  Codex instruction file contains the intended guidance. Review both files if an override
  exists; do not claim a base-file edit is effective when the override shadows it.
- Record the source revision, UTC time, affected paths, before/after SHA-256 checksums,
  backups, and any unresolved decisions in `migration.md` in the migration directory.
  Add `<!-- Worklane guidance bundle: REVISION -->` to the adopted instructions, replacing
  `REVISION` with the bundle's actual hash. Record an interrupted or partial migration
  honestly; do not mark it complete until all affected files are verified.
- Report the adopted revision, retained customizations, and backup location. Explain that
  Codex loads instructions on a new run/session; an already-running agent does not reload
  its initial instruction context automatically. Do not restart another pane or session.
- To roll back when requested, first compare current files with the recorded post-migration
  checksums. Restore backups only if doing so will not discard newer edits; otherwise merge
  the rollback. Remove a newly created file only when it still matches the recorded result.

## Review scenarios

| Starting state | Expected migration result |
| --- | --- |
| Old stock global instructions | Adopt lane guidance; repository release boilerplate is removed from global scope |
| Stock instructions plus custom rules/SDK inventory | Adopt current guidance and retain every custom addition |
| Non-empty override and base file | Update the effective override; preserve the base and explain precedence |
| Empty file, custom Codex home, or symlink | Inspect actual discovery and target; preserve file/link ownership |
| Same revision and unchanged checksums | Report already migrated; make no duplicate backup or content change |
| Changed instructions after migration | Review changes; never use the revision marker as overwrite permission |
| Interrupted migration or concurrent editor | Reconcile backups and current checksums before continuing |
