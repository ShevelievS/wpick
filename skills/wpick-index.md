---
name: wpick-index
description: "Use this skill when starting a new chat about wpick to understand which files to read, what order to implement things, and how the skill files relate to each other. Triggers: 'start working on wpick', 'new chat wpick', 'where to begin wpick', 'wpick project overview'."
---

# wpick Skills Index

## What These Files Are

```
skills/
├── wpick-index.md     ← this file — read first in every new chat
├── wpick-scaffold.md  ← Block 0: workspace setup, Cargo.toml
├── wpick-core.md      ← Blocks 1–5: library crate implementation
├── wpick-daemon.md    ← Blocks 7–8: daemon, Wayland, wgpu, audio
└── wpick-tui.md       ← Block 6: TUI interface
```

```
docs/
├── PROJECT.md   ← architecture, full app flow, all dependencies, rules
├── CORE.md      ← complete API spec for wpick-core
├── TUI.md       ← layout spec, state machine, key bindings
├── DAEMON.md    ← Wayland init, wgpu pipeline, ffmpeg decode
└── ERRORS.md    ← error types, propagation rules, log levels
```

**Skills** = procedural how-to (steps, patterns, checklists)  
**Docs** = specifications (types, algorithms, examples)

Always read the relevant **doc** file alongside the **skill** file.

---

## What to Load Per Task

| Task | Load these files |
|------|-----------------|
| Fresh start, new chat | `PROJECT.md` + relevant skill |
| Workspace scaffold | `skills/wpick-scaffold.md` |
| Any wpick-core module | `PROJECT.md` + `CORE.md` + `ERRORS.md` + `skills/wpick-core.md` |
| Daemon (any module) | `PROJECT.md` + `DAEMON.md` + `ERRORS.md` + `skills/wpick-daemon.md` |
| TUI (any module) | `PROJECT.md` + `TUI.md` + `ERRORS.md` + `skills/wpick-tui.md` |
| Error handling only | `ERRORS.md` |
| Debugging IPC | `PROJECT.md` (IPC protocol section) + `CORE.md` (ipc.rs section) |

---

## Implementation Sequence (Full Project)

```
Phase 1 — Foundation
  ├── Block 0: Workspace scaffold       → skills/wpick-scaffold.md
  ├── Block 1: error.rs + model.rs      → skills/wpick-core.md
  └── Block 2: config.rs                → skills/wpick-core.md

Phase 2 — Data Pipeline
  ├── Block 3: discovery.rs             → skills/wpick-core.md
  ├── Block 4: pkg.rs                   → skills/wpick-core.md
  └── Block 5: cache.rs                 → skills/wpick-core.md

Phase 3 — IPC Contract
  └── Block 6: ipc.rs                   → skills/wpick-core.md
      (write tests: all variants round-trip)

Phase 4 — Daemon
  ├── Block 7: state.rs + ipc_server.rs → skills/wpick-daemon.md
  ├── Block 8: video.rs                 → skills/wpick-daemon.md
  ├── Block 9: audio.rs                 → skills/wpick-daemon.md
  └── Block 10: renderer.rs + main.rs   → skills/wpick-daemon.md

Phase 5 — TUI
  ├── Block 11: client.rs               → skills/wpick-tui.md
  ├── Block 12: app.rs                  → skills/wpick-tui.md
  └── Block 13: ui.rs + main.rs         → skills/wpick-tui.md

Phase 6 — Integration
  └── End-to-end test: start daemon → open TUI → set wallpaper
```

---

## Project Invariants (Check Before Every PR/Commit)

```bash
# Must always pass:
cargo check --workspace
cargo test -p wpick-core

# Dependency rules:
grep -n "anyhow" wpick-core/Cargo.toml        # must return nothing
grep -n "thiserror" wpick-tui/Cargo.toml      # must return nothing
grep -n "thiserror" wpick-daemon/Cargo.toml   # must return nothing
grep -n "wpick-daemon" wpick-tui/Cargo.toml   # must return nothing
grep -n "wpick-tui" wpick-daemon/Cargo.toml   # must return nothing
```

---

## When Something Is Unclear

1. Check `PROJECT.md` first — it has the full flow and all design decisions with rationale.
2. Check the specific `.md` doc (CORE/TUI/DAEMON) for implementation details.
3. Check `ERRORS.md` for any error handling question.
4. If still unclear, the answer is: match the pattern already used elsewhere in the same crate.

---

## How to Start a New Chat (Template)

Paste this at the start:

```
I'm working on wpick — a Rust Wayland live wallpaper manager.
Context files attached: PROJECT.md + [CORE.md | TUI.md | DAEMON.md] + ERRORS.md

Skill file attached: skills/[wpick-core | wpick-tui | wpick-daemon].md

Today's task: [describe specific module or feature]
```
