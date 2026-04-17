---
name: wpick-core
description: "Use this skill when implementing any module inside wpick-core: error types, config loading, Steam library discovery, PKG extraction, SQLite cache, IPC protocol types, or shared data models. Triggers: 'implement core', 'write WpickError', 'parse VDF', 'extract PKG', 'cache module', 'IPC types', 'WallpaperInfo', 'config.rs', 'discovery.rs', 'cache.rs', 'pkg.rs', 'ipc.rs'. Read PROJECT.md and CORE.md before using this skill."
---

# wpick-core Implementation Skill

## Reference Files
Read before every session:
- `PROJECT.md` — architecture, IPC protocol spec, file paths
- `CORE.md` — complete module API, all type definitions, implementation algorithms
- `ERRORS.md` — WpickError variants, construction patterns

## Module Implementation Order

Follow this order to avoid missing dependencies:

```
1. error.rs    → no deps inside core
2. model.rs    → depends on error.rs
3. config.rs   → depends on error.rs, model.rs
4. ipc.rs      → depends on error.rs, model.rs
5. discovery.rs → depends on error.rs, config.rs
6. pkg.rs      → depends on error.rs, model.rs, discovery.rs
7. cache.rs    → depends on error.rs, model.rs
```

## error.rs — Rules

- Use `thiserror` exclusively. Zero `anyhow` imports.
- Every variant has a human-readable `#[error("...")]` message.
- Use `#[from]` only for: `std::io::Error`, `rusqlite::Error`, `serde_json::Error`, `toml::de::Error`.
- String-carrying variants always include context (ID, file path).
- Expose `pub type Result<T> = std::result::Result<T, WpickError>;`

Verify after writing:
```bash
cargo test -p wpick-core -- --nocapture 2>&1 | head -20
```

## model.rs — Rules

- `WallpaperInfo` and `WallpaperType` must be `#[derive(Debug, Clone, Serialize, Deserialize)]`
- `WallpaperType` uses `#[serde(rename_all = "lowercase")]`
- `WallpaperInfo.type_icon()` returns `&'static str` — no allocation
- `WallpaperInfo.is_supported()` returns `bool` — only `WallpaperType::Video` is true in MVP

## config.rs — Implementation Checklist

- [ ] `WpickConfig` fields all have `#[serde(default)]` at struct level
- [ ] `GeneralConfig::default()` returns `volume: 0.8, muted: false`
- [ ] `load()` uses `dirs::config_dir()` → panics if XDG broken (acceptable)
- [ ] `load()` creates parent directories via `std::fs::create_dir_all()`
- [ ] `load()` returns `Default::default()` when file does not exist (not an error)
- [ ] `save()` writes to `.tmp` file first, then `std::fs::rename()` (atomic write)
- [ ] `app_dirs()` calls `create_dir_all` for all dirs it returns
- [ ] Socket path: `dirs::home_dir() / ".wpick.sock"` — NOT inside a subdirectory

Paths computed by `app_dirs()`:
```rust
let home   = dirs::home_dir().ok_or_else(|| WpickError::Config("No home dir".into()))?;
let config = dirs::config_dir().ok_or_else(|| WpickError::Config("No config dir".into()))?;
let cache  = if self.paths.cache_dir.is_empty() {
    dirs::cache_dir().ok_or_else(|| WpickError::Config("No cache dir".into()))? .join("wpick")
} else {
    PathBuf::from(&self.paths.cache_dir)
};
// ...
AppDirs {
    config_file:    config.join("wpick").join("config.toml"),
    cache_dir:      cache.clone(),
    wallpapers_dir: cache.join("wallpapers"),
    db_path:        cache.join("wpick.db"),
    socket_path:    home.join(".wpick.sock"),
    log_dir:        dirs::data_local_dir().unwrap_or(home.join(".local/share")).join("wpick"),
}
```

## discovery.rs — Implementation Checklist

Steam root candidates to search (in order):
```rust
let candidates = vec![
    home.join(".steam/steam"),
    home.join(".steam/root"),
    home.join(".local/share/Steam"),
    home.join("snap/steam/common/.steam/steam"),
    PathBuf::from("/home").join(&username).join(".var/app/com.valvesoftware.Steam/data/Steam"),
];
```

For each valid Steam root:
1. Read `{root}/steamapps/libraryfolders.vdf`
2. Parse with `keyvalues_serde::from_str::<LibraryFolders>(&content)`
3. Extract all `path` strings
4. Check `{path}/steamapps/workshop/content/431960/` exists
5. List numeric subdirs → `WallpaperDir { id, path }`

VDF struct pattern:
```rust
#[derive(serde::Deserialize)]
struct LibraryFolders {
    #[serde(flatten)]
    entries: std::collections::BTreeMap<String, LibraryEntry>,
}
#[derive(serde::Deserialize)]
struct LibraryEntry {
    path: String,
    #[serde(flatten)]
    _rest: std::collections::BTreeMap<String, serde_json::Value>,
}
```

Edge cases — handle without returning error, use `tracing::warn!`:
- VDF file missing → skip that Steam root
- VDF parse error → `WpickError::VdfParse { path, reason }` — let caller decide
- Dir entry not parseable as u64 → `tracing::debug!("Skipping non-numeric dir")`, continue
- Symlinks → use `std::fs::metadata()` (follows symlinks), not `symlink_metadata()`

## pkg.rs — Implementation Checklist

mtime check pattern:
```rust
let mtime_secs = std::fs::metadata(&pkg_path)?
    .modified()?
    .duration_since(std::time::UNIX_EPOCH)
    .unwrap_or_default()
    .as_secs();

let stamp_path = out_dir.join(".pkg_mtime");
let cached_mtime = std::fs::read_to_string(&stamp_path)
    .ok()
    .and_then(|s| s.trim().parse::<u64>().ok());

let needs_extract = cached_mtime != Some(mtime_secs);
```

After extraction:
```rust
std::fs::write(&stamp_path, mtime_secs.to_string())?;
```

`project.json` field names from Wallpaper Engine (actual casing):
```rust
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]  // covers soundEnabled, contentrating etc.
pub struct ProjectJson {
    pub title: String,
    #[serde(rename = "type")]
    pub wallpaper_type: String,
    pub file: Option<String>,
    pub preview: Option<String>,
    #[serde(default)]
    pub sound_enabled: bool,
    #[serde(default = "default_volume")]
    pub volume: f32,
}
fn default_volume() -> f32 { 1.0 }
```

Return `Ok(None)` (not an error) when:
- `scene.pkg` doesn't exist in the wallpaper dir
- `wallpaper_type` is not "video"
- `project.json` is valid but file path inside doesn't exist on disk

Return `Err(WpickError::PkgExtract { id, reason })` when:
- `depkg::extract()` fails

## cache.rs — Implementation Checklist

- [ ] Use `PRAGMA journal_mode=WAL` and `PRAGMA synchronous=NORMAL` on open
- [ ] `CREATE TABLE IF NOT EXISTS` — idempotent, safe to call every open
- [ ] `STRICT` table mode for type safety
- [ ] `upsert()` uses `INSERT OR REPLACE` (not INSERT OR IGNORE)
- [ ] `get_all()` sorts by `title ASC`
- [ ] `prune()` deletes IDs not in the provided slice
- [ ] `count()` used by daemon to decide if full scan is needed

SQL schema (embed as a constant):
```rust
const SCHEMA_SQL: &str = "
    PRAGMA journal_mode=WAL;
    PRAGMA synchronous=NORMAL;
    CREATE TABLE IF NOT EXISTS wallpapers (
        id               INTEGER PRIMARY KEY,
        title            TEXT NOT NULL,
        wallpaper_type   TEXT NOT NULL,
        file_path        TEXT NOT NULL,
        preview_path     TEXT,
        has_audio        INTEGER NOT NULL DEFAULT 0,
        file_size_bytes  INTEGER NOT NULL DEFAULT 0,
        pkg_mtime_secs   INTEGER NOT NULL
    ) STRICT;
    CREATE TABLE IF NOT EXISTS meta (
        key   TEXT PRIMARY KEY,
        value TEXT NOT NULL
    ) STRICT;
";
```

Row mapping from SQLite to `WallpaperInfo`:
```rust
fn row_to_info(row: &rusqlite::Row) -> rusqlite::Result<WallpaperInfo> {
    let type_str: String = row.get(1)?;
    let wallpaper_type = match type_str.as_str() {
        "video" => WallpaperType::Video,
        "web"   => WallpaperType::Web,
        _       => WallpaperType::Scene,
    };
    Ok(WallpaperInfo {
        id:              row.get::<_, i64>(0)? as u64,
        title:           row.get(1)?,
        wallpaper_type,
        file_path:       row.get(3)?,
        preview_path:    row.get(4)?,
        has_audio:       row.get::<_, bool>(5)?,
        file_size_bytes: row.get::<_, i64>(6)? as u64,
    })
}
```

## ipc.rs — Implementation Checklist

- [ ] Both enums use `#[serde(tag = "type")]` — produces `{"type":"List"}` format
- [ ] `send_command` and `send_response` flush after write: call `writer.flush().await?`
- [ ] `recv_*` functions check for empty line (EOF): if `line.is_empty()` → `WpickError::IpcClosed`
- [ ] All four functions are generic over `AsyncWrite`/`AsyncBufRead` — not bound to `UnixStream`

Send pattern:
```rust
pub async fn send_command<W: tokio::io::AsyncWrite + Unpin>(
    writer: &mut W,
    cmd: &ClientCommand,
) -> crate::Result<()> {
    use tokio::io::AsyncWriteExt;
    let mut json = serde_json::to_string(cmd)?;
    json.push('\n');
    writer.write_all(json.as_bytes()).await?;
    writer.flush().await?;
    Ok(())
}
```

Recv pattern:
```rust
pub async fn recv_response<R: tokio::io::AsyncBufRead + Unpin>(
    reader: &mut R,
) -> crate::Result<DaemonResponse> {
    use tokio::io::AsyncBufReadExt;
    let mut line = String::new();
    reader.read_line(&mut line).await?;
    if line.is_empty() {
        return Err(WpickError::IpcClosed);
    }
    Ok(serde_json::from_str(line.trim())?)
}
```

## Testing — Required Tests

```bash
# Run all core tests:
cargo test -p wpick-core

# With output:
cargo test -p wpick-core -- --nocapture
```

Minimum required tests per module:

**discovery.rs:**
- `test_parse_vdf_single_library()` — parse VDF with one library
- `test_parse_vdf_multiple_libraries()` — parse VDF with 2+ libraries
- `test_skip_non_numeric_dir()` — directory named "thumbnails" doesn't panic

**cache.rs:**
- `test_open_creates_schema()` — open on new file, check tables exist
- `test_upsert_and_retrieve()` — upsert + get_by_id returns same data
- `test_prune_removes_stale()` — prune removes IDs not in active list

**ipc.rs:**
- `test_command_round_trip()` — serialize → deserialize each ClientCommand variant
- `test_response_round_trip()` — serialize → deserialize each DaemonResponse variant

Use `tempfile::TempDir` for all tests that need disk access.
