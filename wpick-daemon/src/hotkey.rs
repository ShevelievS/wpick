use evdev::{Device, EventType, InputEventKind, Key};
use std::collections::HashSet;
use tokio::sync::{broadcast, mpsc};
use wpick_core::config::HotkeyConfig;

// ─── Entry point ──────────────────────────────────────────────────────────────

pub async fn run(config: HotkeyConfig, mut shutdown: broadcast::Receiver<()>) {
    if !config.enabled || config.keys.is_empty() {
        return;
    }

    let Some((mods, trigger)) = parse_keys(&config.keys) else {
        tracing::warn!("hotkey: invalid key combination '{}' — example: 'super+w'", config.keys);
        return;
    };

    let keyboards = find_keyboards();
    if keyboards.is_empty() {
        tracing::warn!(
            "hotkey: no keyboard devices found — \
             ensure the daemon user is in the 'input' group: sudo usermod -aG input $USER"
        );
        return;
    }

    tracing::info!(
        "hotkey: watching {} keyboard device(s) for '{}'",
        keyboards.len(),
        config.keys,
    );

    let (tx, mut rx) = mpsc::channel::<()>(4);

    // Cap at 8 devices: prevents 100+ threads on machines with many input nodes
    // (KVM switches, uinput virtual devices, etc.), each of which would cost ~8 MB stack.
    const MAX_KEYBOARD_WATCHERS: usize = 8;
    if keyboards.len() > MAX_KEYBOARD_WATCHERS {
        tracing::warn!("hotkey: {} keyboard devices found, watching only first {}",
            keyboards.len(), MAX_KEYBOARD_WATCHERS);
    }
    for path in keyboards.into_iter().take(MAX_KEYBOARD_WATCHERS) {
        let tx    = tx.clone();
        let mods  = mods.clone();
        std::thread::Builder::new()
            .name(format!("hotkey:{}", path.display()))
            .spawn(move || watch_device(path, mods, trigger, tx))
            .ok();
    }

    // Debounce: ignore repeated fires within 800 ms — multiple evdev devices
    // watching the same physical keyboard each report the same keypress.
    let mut last_fired = std::time::Instant::now()
        .checked_sub(std::time::Duration::from_secs(10))
        .unwrap_or_else(std::time::Instant::now);

    loop {
        tokio::select! {
            Some(()) = rx.recv() => {
                // Drain any extra pending signals from the same keypress
                while rx.try_recv().is_ok() {}

                if last_fired.elapsed() < std::time::Duration::from_millis(800) {
                    continue;
                }
                last_fired = std::time::Instant::now();
                tracing::info!("hotkey: triggered — opening wpick popup");
                spawn_popup(&config);
            }
            _ = shutdown.recv() => break,
        }
    }
}

// ─── Device watcher (runs in a blocking thread per device) ───────────────────

fn watch_device(
    path: std::path::PathBuf,
    mods: HashSet<Key>,
    trigger: Key,
    tx: mpsc::Sender<()>,
) {
    let Ok(mut device) = Device::open(&path) else { return };
    let mut pressed: HashSet<Key> = HashSet::new();

    loop {
        let events = match device.fetch_events() {
            Ok(e)  => e,
            Err(_) => break,
        };
        for event in events {
            let InputEventKind::Key(key) = event.kind() else { continue };
            match event.value() {
                1 | 2 => { pressed.insert(key); }  // press / repeat
                0     => { pressed.remove(&key); }  // release
                _     => {}
            }
            // Fire only on initial press (value == 1), not repeat
            if event.value() == 1 && key == trigger {
                let all_held = mods.iter().all(|m| {
                    pressed.contains(m) || pressed.contains(&right_variant(*m))
                });
                if all_held {
                    let _ = tx.try_send(());
                }
            }
        }
    }
}

// ─── Popup spawner ────────────────────────────────────────────────────────────

fn spawn_popup(config: &HotkeyConfig) {
    // Reject terminal paths containing whitespace or shell metacharacters to
    // prevent accidental command injection via the config file.
    let terminal = if config.terminal.is_empty()
        || config.terminal.chars().any(|c| c.is_whitespace() || matches!(c, ';' | '&' | '|' | '$' | '`' | '\\' | '\'' | '"'))
    {
        if !config.terminal.is_empty() {
            tracing::warn!("hotkey: terminal '{}' contains unsafe characters — using auto-detection",
                config.terminal);
        }
        detect_terminal()
    } else {
        config.terminal.clone()
    };

    let w = config.width;
    let h = config.height;

    if std::env::var("HYPRLAND_INSTANCE_SIGNATURE").is_ok() {
        // Hyprland: if window exists → focus it; otherwise spawn with inline rules.
        let already_open = std::process::Command::new("hyprctl")
            .args(["clients", "-j"])
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.contains("wpick-popup"))
            .unwrap_or(false);

        if already_open {
            tracing::info!("hotkey: wpick-popup already open — focusing");
            let _ = std::process::Command::new("hyprctl")
                .args(["dispatch", "focuswindow", "class:wpick-popup"])
                .spawn();
        } else {
            let term_cmd = terminal_launch_cmd(&terminal, "wpick-popup");
            let dispatch = format!("[float;center;size {w} {h}] {term_cmd}");
            tracing::info!("hotkey: spawning — hyprctl dispatch exec {}", dispatch);
            match std::process::Command::new("hyprctl")
                .args(["dispatch", "exec", &dispatch])
                .output()
            {
                Ok(out) => tracing::info!("hotkey: hyprctl → {}", String::from_utf8_lossy(&out.stdout).trim()),
                Err(e)  => tracing::warn!("hotkey: hyprctl failed: {}", e),
            }
        }
    } else {
        // Generic compositor: spawn terminal, let WM handle placement.
        let cmd = terminal_launch_cmd(&terminal, "wpick-popup");
        let parts: Vec<&str> = cmd.split_whitespace().collect();
        if let Some((prog, args)) = parts.split_first() {
            let _ = std::process::Command::new(prog).args(args).spawn();
        }
    }
}

// ─── Terminal helpers ─────────────────────────────────────────────────────────

/// Returns the shell-ready launch command for the given terminal with app-id set.
fn terminal_launch_cmd(terminal: &str, app_id: &str) -> String {
    match terminal {
        "kitty"     => format!("kitty --class {app_id} -e wpick"),
        "alacritty" => format!("alacritty --class {app_id} -e wpick"),
        "wezterm"   => format!("wezterm start --class {app_id} -- wpick"),
        "xterm"     => format!("xterm -name {app_id} -e wpick"),
        t           => format!("{t} --app-id {app_id} -e wpick"), // foot + others
    }
}

fn detect_terminal() -> String {
    for t in &["foot", "kitty", "alacritty", "wezterm", "xterm"] {
        if which_exists(t) {
            return t.to_string();
        }
    }
    "xterm".to_string()
}

fn which_exists(prog: &str) -> bool {
    std::process::Command::new("which")
        .arg(prog)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

// ─── Key helpers ──────────────────────────────────────────────────────────────

/// Parse `"super+w"` → `({KEY_LEFTMETA}, KEY_W)`.
fn parse_keys(keys_str: &str) -> Option<(HashSet<Key>, Key)> {
    let mut mods    = HashSet::new();
    let mut trigger = None;
    for part in keys_str.split('+') {
        match part.trim().to_lowercase().as_str() {
            "super" | "meta" | "win" | "mod4" => { mods.insert(Key::KEY_LEFTMETA); }
            "ctrl"  | "control" | "mod1"      => { mods.insert(Key::KEY_LEFTCTRL); }
            "shift"                            => { mods.insert(Key::KEY_LEFTSHIFT); }
            "alt"                              => { mods.insert(Key::KEY_LEFTALT); }
            k => { trigger = str_to_key(k); }
        }
    }
    trigger.map(|k| (mods, k))
}

fn str_to_key(s: &str) -> Option<Key> {
    match s {
        "a" => Some(Key::KEY_A), "b" => Some(Key::KEY_B), "c" => Some(Key::KEY_C),
        "d" => Some(Key::KEY_D), "e" => Some(Key::KEY_E), "f" => Some(Key::KEY_F),
        "g" => Some(Key::KEY_G), "h" => Some(Key::KEY_H), "i" => Some(Key::KEY_I),
        "j" => Some(Key::KEY_J), "k" => Some(Key::KEY_K), "l" => Some(Key::KEY_L),
        "m" => Some(Key::KEY_M), "n" => Some(Key::KEY_N), "o" => Some(Key::KEY_O),
        "p" => Some(Key::KEY_P), "q" => Some(Key::KEY_Q), "r" => Some(Key::KEY_R),
        "s" => Some(Key::KEY_S), "t" => Some(Key::KEY_T), "u" => Some(Key::KEY_U),
        "v" => Some(Key::KEY_V), "w" => Some(Key::KEY_W), "x" => Some(Key::KEY_X),
        "y" => Some(Key::KEY_Y), "z" => Some(Key::KEY_Z),
        "space" | "spc"        => Some(Key::KEY_SPACE),
        "return" | "enter"     => Some(Key::KEY_ENTER),
        "tab"                  => Some(Key::KEY_TAB),
        "0" => Some(Key::KEY_0), "1" => Some(Key::KEY_1), "2" => Some(Key::KEY_2),
        "3" => Some(Key::KEY_3), "4" => Some(Key::KEY_4), "5" => Some(Key::KEY_5),
        "6" => Some(Key::KEY_6), "7" => Some(Key::KEY_7), "8" => Some(Key::KEY_8),
        "9" => Some(Key::KEY_9),
        _ => None,
    }
}

/// Returns the right-hand variant of a left modifier key.
fn right_variant(key: Key) -> Key {
    match key {
        Key::KEY_LEFTMETA  => Key::KEY_RIGHTMETA,
        Key::KEY_LEFTCTRL  => Key::KEY_RIGHTCTRL,
        Key::KEY_LEFTSHIFT => Key::KEY_RIGHTSHIFT,
        Key::KEY_LEFTALT   => Key::KEY_RIGHTALT,
        k => k,
    }
}

// ─── Keyboard device discovery ────────────────────────────────────────────────

fn find_keyboards() -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir("/dev/input") else { return out };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(dev) = Device::open(&path) else { continue };
        // Must support KEY events and have alphabetic keys (i.e. is a keyboard).
        if dev.supported_events().contains(EventType::KEY)
            && dev.supported_keys().map(|k| k.contains(Key::KEY_A)).unwrap_or(false)
        {
            out.push(path);
        }
    }
    out
}
