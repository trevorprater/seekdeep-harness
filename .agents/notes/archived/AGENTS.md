# AGENTS.md — Archived Agent Notes

Archived Agent Note triplets under the kind directories are frozen historical snapshots, not current authority. Never edit, reformat, translate, repair, delete, or move a sealed artifact; use an active Agent Note or current documentation for new decisions and facts.

The SeekDeep port preserves the pinned source archive byte-for-byte. Historical `DeepSeek Harness` / `dsh` identities and filenames are sealed history, not product-facing rename surfaces. Restore any mechanically renamed archive artifact to its source path and bytes; never replace a sidecar or manifest seal to accept the rename.

The archival change may only relocate a complete English/Chinese/sidecar triplet, insert the identical `Archived: YYYY-MM-DD` line below both `Status: implemented` lines, re-record the sidecar, and repair or delete inbound links. Do not inspect, verify, or repair links out of archived notes.

Run the [`seekdeep-archive-agent-notes`](../../skills/seekdeep-archive-agent-notes/SKILL.md) workflow and append new artifact hashes with `pnpm run verify-archived-agent-notes --write`. The normal verifier rejects changed or missing sealed artifacts, incomplete triplets, unknown kind folders, and invalid archive metadata.
