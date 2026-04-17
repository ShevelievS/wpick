---
name: wpick-scaffold
description: "Use this skill when setting up the wpick workspace from scratch, initializing the Cargo workspace, creating the three-crate structure (wpick-core, wpick-tui, wpick-daemon), or configuring shared dependencies. Triggers: 'create workspace', 'scaffold project', 'initialize wpick', 'setup Cargo.toml', 'create crate structure'. Read PROJECT.md before using this skill."
---

# wpick Workspace Scaffold

## Reference Files
Always read before starting:
- `PROJECT.md` — architecture rules, dependency table, strict crate separation rules

## Step 1 — Root Cargo.toml

Create `wpick/Cargo.toml`:

```toml
[workspace]
members = ["wpick-core", "wpick-tui", "wpick-daemon"]
resolver = "2"

[workspace.dependencies]
serde       = { version = "1", features = ["derive"] }
serde_json  = "1"
thiserror   = "2"
anyhow      = "1"
tokio       = { version = "1", features = ["full"] }
tracing     = "0.1"
```

## Step 2 — wpick-core

```bash
cargo new --lib wpick-core
```

`wpick-core/Cargo.toml`:
```toml
[package]
name    = "wpick-core"
version = "0.1.0"
edition = "2021"

[dependencies]
serde           = { workspace = true }
serde_json      = { workspace = true }
thiserror       = { workspace = true }
tracing         = { workspace = true }
toml            = "0.8"
dirs            = "5"
rusqlite        = { version = "0.32", features = ["bundled"] }
keyvalues-serde = "0.2"
depkg           = "0.1"
walkdir         = "2"

[dev-dependencies]
tempfile = "3"
```

`wpick-core/src/lib.rs` — only re-exports, no logic:
```rust
pub mod cache;
pub mod config;
pub mod discovery;
pub mod error;
pub mod ipc;
pub mod model;
pub mod pkg;

pub use config::{AppDirs, WpickConfig};
pub use error::{Result, WpickError};
pub use ipc::{ClientCommand, DaemonResponse};
pub use model::{WallpaperInfo, WallpaperType};
```

Create empty module files:
```bash
touch wpick-core/src/{cache,config,discovery,error,ipc,model,pkg}.rs
```

## Step 3 — wpick-tui

```bash
cargo new wpick-tui
```

`wpick-tui/Cargo.toml`:
```toml
[package]
name    = "wpick-tui"
version = "0.1.0"
edition = "2021"

[[bin]]
name = "wpick"           # binary called 'wpick', not 'wpick-tui'
path = "src/main.rs"

[dependencies]
wpick-core         = { path = "../wpick-core" }
serde              = { workspace = true }
serde_json         = { workspace = true }
anyhow             = { workspace = true }
tokio              = { workspace = true }
tracing            = { workspace = true }
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
ratatui            = "0.29"
crossterm          = "0.28"
```

Create source files:
```bash
touch wpick-tui/src/{app,ui,client}.rs
```

## Step 4 — wpick-daemon

```bash
cargo new wpick-daemon
```

`wpick-daemon/Cargo.toml`:
```toml
[package]
name    = "wpick-daemon"
version = "0.1.0"
edition = "2021"

[[bin]]
name = "wpick-daemon"
path = "src/main.rs"

[dependencies]
wpick-core              = { path = "../wpick-core" }
serde                   = { workspace = true }
serde_json              = { workspace = true }
anyhow                  = { workspace = true }
tokio                   = { workspace = true }
tracing                 = { workspace = true }
tracing-subscriber      = { version = "0.3", features = ["env-filter"] }
tracing-appender        = "0.2"
wayland-client          = "0.31"
wayland-protocols-wlr   = { version = "0.3", features = ["client"] }
wgpu                    = "22"
bytemuck                = { version = "1", features = ["derive"] }
ffmpeg-next             = "7"
rodio                   = "0.19"
parking_lot             = "0.12"
raw-window-handle       = "0.6"
```

Create source files:
```bash
touch wpick-daemon/src/{state,ipc_server,renderer,video,audio}.rs
```

## Step 5 — Verification

```bash
cargo check --workspace
```

Expected: compiles with zero errors. Warnings for empty modules are acceptable.

## Step 6 — .gitignore

```
/target
**/*.lock
.env
*.log
```

Keep `Cargo.lock` in version control (workspace binary projects should pin it).
Remove `*.lock` from gitignore and add `Cargo.lock` explicitly:
```
/target
.env
```

## Invariants to Check After Scaffold

- [ ] `wpick-core/Cargo.toml` has NO `anyhow`, NO `wayland-client`, NO `wgpu`, NO `ratatui`
- [ ] `wpick-tui/Cargo.toml` has NO `thiserror`, NO `wayland-client`, NO `wgpu`
- [ ] `wpick-daemon/Cargo.toml` has NO `thiserror`, NO `ratatui`, NO `crossterm`
- [ ] `wpick-tui` and `wpick-daemon` do NOT depend on each other
- [ ] `cargo check --workspace` passes
