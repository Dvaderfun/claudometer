# AGENTS.md

Claudometer is a native Win32 Rust tray app (Claude + Codex usage limits). Agent rules live in two files; read both before changing anything:

1. `CLAUDE.md` — commands, verification, and the binding "Hard-won gotchas".
2. `plans/state-of-the-art-v1.md` — start with §0 (executor brief) and §2 (status board and execution queue).

Non-negotiable: never write, refresh, or exchange provider credentials; never call live provider endpoints from tests; never push tags, publish releases, or change repository settings.
