---
name: wpick-tui
description: "Use this skill when implementing wpick-tui: the ratatui terminal interface, App state machine, key bindings, IPC client, wallpaper list layout, detail panel, status display, or reconnection logic. Triggers: 'implement TUI', 'ratatui layout', 'key bindings', 'wallpaper list', 'detail panel', 'IPC client', 'App struct', 'event loop', 'status message', 'crossterm'. Read PROJECT.md, TUI.md, and ERRORS.md before using this skill."
---

# wpick-tui Implementation Skill

## Reference Files
Read before every TUI session:
- `TUI.md` — complete layout spec, App state, all key bindings, ratatui widget code
- `PROJECT.md` — IPC protocol, how app flow works end-to-end
- `ERRORS.md` — user-facing error message text, display rules

## Implementation Order

```
1. client.rs  → IpcClient (connect + send + recv wrappers)
2. app.rs     → App struct + key handler (no rendering yet)
3. ui.rs      → render functions (header, list, detail, footer)
4. main.rs    → terminal init, run loop, cleanup
```

Test client.rs against a running daemon before building the UI.

## client.rs — Implementation Checklist

- [ ] `connect()` returns a helpful error: `"Cannot connect to daemon at {:?}: {}. Is wpick-daemon running?"`
- [ ] `send()` wraps the IPC helpers from `wpick_core::ipc`
- [ ] `send()` uses `BufReader`/`BufWriter` wrapping the `OwnedReadHalf`/`OwnedWriteHalf`
- [ ] `list_wallpapers()` convenience method returns `Vec<WallpaperInfo>` or `anyhow::Error`

Connection pattern:
```rust
pub async fn connect(socket_path: &std::path::Path) -> anyhow::Result<Self> {
    let stream = tokio::net::UnixStream::connect(socket_path)
        .await
        .with_context(|| format!(
            "Cannot connect to daemon at {:?}. Start wpick-daemon first.",
            socket_path
        ))?;
    let (r, w) = stream.into_split();
    Ok(Self {
        reader: tokio::io::BufReader::new(r),
        writer: tokio::io::BufWriter::new(w),
    })
}
```

## app.rs — App State Checklist

```rust
pub struct App {
    pub config:           WpickConfig,
    pub dirs:             AppDirs,
    pub client:           Option<IpcClient>,    // None = not connected
    pub wallpapers:       Vec<WallpaperInfo>,
    pub selected:         usize,
    pub mode:             AppMode,
    pub status_message:   Option<String>,
    pub status_is_error:  bool,
    pub daemon_connected: bool,
    pub loading:          bool,                 // show "Loading..." in list
    pub should_quit:      bool,
}
```

- [ ] `selected` never exceeds `wallpapers.len().saturating_sub(1)`
- [ ] `select_next()` and `select_prev()` clamp: `(self.selected + 1).min(len - 1)`
- [ ] `status_message` is cleared at the start of every `handle_key()` call
- [ ] `loading` is set `true` before refresh, `false` after (even on error)
- [ ] Commands that require connection check `self.client.is_some()` first

Volume step size: `0.05` (5% per keypress). Clamp with `.clamp(0.0, 1.0)`.

Preserve selection on refresh:
```rust
let prev_id = self.wallpapers.get(self.selected).map(|w| w.id);
self.wallpapers = new_list;
if let Some(id) = prev_id {
    if let Some(pos) = self.wallpapers.iter().position(|w| w.id == id) {
        self.selected = pos;
        return;
    }
}
self.selected = self.selected.min(self.wallpapers.len().saturating_sub(1));
```

Status message helpers:
```rust
pub fn set_status_ok(&mut self, msg: impl Into<String>) {
    self.status_message = Some(msg.into());
    self.status_is_error = false;
}
pub fn set_status_error(&mut self, msg: impl Into<String>) {
    self.status_message = Some(msg.into());
    self.status_is_error = true;
}
```

## ui.rs — Layout Rules

**Never put logic in ui.rs.** All decisions happen in app.rs. `ui.rs` only reads `&App`.

Layout hierarchy:
```rust
// 1. Vertical: [header=1, main=fill, footer=2]
let [header, main, footer] = Layout::vertical([
    Constraint::Length(1),
    Constraint::Min(0),
    Constraint::Length(2),
]).areas(frame.area());

// 2. Horizontal: [list=30%, detail=70%]
let [list_area, detail_area] = Layout::horizontal([
    Constraint::Percentage(30),
    Constraint::Percentage(70),
]).areas(main);
```

**Minimum terminal size check:**
```rust
pub fn render(frame: &mut ratatui::Frame, app: &App) {
    if frame.area().width < 80 || frame.area().height < 20 {
        let msg = ratatui::widgets::Paragraph::new("Terminal too small (min 80×20)");
        frame.render_widget(msg, frame.area());
        return;
    }
    // ... normal render
}
```

**List widget — ListState must be rendered as stateful:**
```rust
let mut list_state = ratatui::widgets::ListState::default();
list_state.select(
    if app.wallpapers.is_empty() { None } else { Some(app.selected) }
);
frame.render_stateful_widget(list_widget, list_area, &mut list_state);
// NOT frame.render_widget() — that doesn't show selection highlight
```

**Color scheme (crossterm Color):**
```
Header conn status:  Color::Green (connected) / Color::Red (disconnected)
Selected list item:  Color::Cyan + Modifier::BOLD
Unsupported item:    Color::DarkGray
Detail labels:       Color::Gray
Status OK:           Color::Green
Status error:        Color::Yellow  (not Red — red implies crash)
Key hint keys:       Color::Cyan
Key hint labels:     Color::DarkGray
Volume MUTED:        Color::Yellow
```

**Detail panel — empty state:**
```rust
let content = if app.wallpapers.is_empty() {
    Text::from(if app.loading {
        "Loading wallpapers..."
    } else {
        "No wallpapers found.\nCheck Steam installation or add paths in config."
    })
} else {
    // ... render selected wallpaper info
};
```

**Footer key hint format:**
```
 ↑↓/jk Navigate   Enter Apply   +/- Volume   a Mute   r Refresh
 i Info   p Pause   q Quit   Q Kill daemon
```

## main.rs — Terminal Lifecycle

**Always restore terminal, even on panic:**
```rust
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Setup tracing to STDERR (doesn't interfere with TUI)
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let config = WpickConfig::load().context("Failed to load config")?;
    let dirs   = config.app_dirs().context("Failed to resolve app dirs")?;

    // Enable raw mode
    crossterm::terminal::enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    crossterm::execute!(stdout, crossterm::terminal::EnterAlternateScreen)?;

    // Run app — catch all errors to ensure terminal is restored
    let result = run_app(config, dirs).await;

    // ALWAYS restore terminal
    crossterm::terminal::disable_raw_mode()?;
    crossterm::execute!(std::io::stdout(), crossterm::terminal::LeaveAlternateScreen)?;

    result
}

async fn run_app(config: WpickConfig, dirs: AppDirs) -> anyhow::Result<()> {
    let backend  = ratatui::backend::CrosstermBackend::new(std::io::stdout());
    let mut terminal = ratatui::Terminal::new(backend)?;
    let mut app  = App::new(config, dirs);
    app.run(&mut terminal).await
}
```

## Reconnection Logic

When `self.send()` fails (IPC error), set `client = None`, `daemon_connected = false`.

In the event loop, attempt reconnect on every tick while disconnected:
```rust
// In run() loop, before draw:
if self.client.is_none() {
    self.try_reconnect().await;
}
```

```rust
async fn try_reconnect(&mut self) {
    match tokio::time::timeout(
        std::time::Duration::from_millis(200),
        IpcClient::connect(&self.dirs.socket_path),
    ).await {
        Ok(Ok(client)) => {
            self.client = Some(client);
            self.daemon_connected = true;
            self.refresh_list().await;
        }
        _ => {
            // Still not connected — will retry next tick
        }
    }
}
```

## Common Mistakes to Avoid

| Mistake | Correct approach |
|---------|-----------------|
| `render_widget` for List | Use `render_stateful_widget` with `ListState` |
| Status shown in header | Status goes in detail panel bottom |
| `println!` in TUI code | All output via `tracing` to stderr |
| Hold `IpcClient` borrow across `.await` | Clone the data you need before awaiting |
| Crash on empty wallpaper list | Show empty state message, never index empty Vec |
| `crossterm::Event::Mouse` causes panic | Add `_ => {}` arm in match |
| Terminal not restored on error | Use the try/always-restore pattern in main.rs |

## Verification Steps

```bash
# 1. Check compiles
cargo check -p wpick-tui

# 2. Run against a live daemon
cargo run -p wpick-daemon &
sleep 1
cargo run -p wpick-tui

# 3. Test without daemon (should show disconnected state, not crash)
cargo run -p wpick-tui  # with no daemon running

# 4. Test reconnection
cargo run -p wpick-tui &
# Kill daemon, restart it → TUI should reconnect within ~1 second

# 5. Test minimum terminal size
# Resize terminal to very small → should show "Terminal too small" message
```

## Debug Output

The TUI writes to stderr (visible if you redirect):
```bash
RUST_LOG=wpick_tui=debug cargo run -p wpick-tui 2>tui-debug.log
# In another terminal:
tail -f tui-debug.log
```
