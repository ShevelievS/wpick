# PROMPTS.md — Готові промпти для Claude Code

> Копіюй промпт для потрібного блоку, вставляй в Claude Code.  
> Кожен промпт вже включає всі необхідні обмеження.  
> НЕ додавай "будь ласка" або розширені пояснення — промпти вже оптимальні.

---

## Як використовувати

```
1. Відкрий потрібний промпт нижче
2. Перевір що відповідні .md файли відкриті в контексті
3. Вставляй ПОВНІСТЮ — не скорочуй
4. Якщо Claude Code зробив не те — використай промпт-корекцію внизу блоку
```

---

## Блок 0 — Scaffold

```
Контекст: wpick — Rust Wayland live wallpaper manager.

Прочитай skills/wpick-scaffold.md повністю.

Виконай наступне ТОЧНО за інструкцією в skills/wpick-scaffold.md:
1. Створи root Cargo.toml з workspace конфігурацією і [workspace.dependencies]
2. Створи wpick-core як lib crate з Cargo.toml і src/lib.rs (тільки pub mod + pub use)
3. Створи wpick-tui як bin crate, binary name "wpick", з Cargo.toml і порожніми src/{app,ui,client}.rs
4. Створи wpick-daemon як bin crate, binary name "wpick-daemon", з Cargo.toml і порожніми src/{state,ipc_server,renderer,video,audio}.rs

Після кожного файлу запускай: cargo check --workspace

Фінальна перевірка:
- grep "anyhow" wpick-core/Cargo.toml → має бути порожньо
- grep "wpick-daemon" wpick-tui/Cargo.toml → має бути порожньо
```

---

## Блок 1 — error.rs + model.rs

```
Контекст: wpick-core library crate.
Прочитай: CORE.md §error.rs, CORE.md §model.rs, ERRORS.md §"WpickError — Complete Definition"

Реалізуй wpick-core/src/error.rs:
- Повний WpickError enum з усіма варіантами за CORE.md
- #[from] тільки для: std::io::Error, rusqlite::Error, serde_json::Error, toml::de::Error
- pub type Result<T> = std::result::Result<T, WpickError>
- Жодного anyhow

Реалізуй wpick-core/src/model.rs:
- WallpaperType enum з #[serde(rename_all = "lowercase")]
- WallpaperInfo struct з усіма полями за CORE.md
- impl WallpaperType: Display
- impl WallpaperInfo: type_icon() -> &'static str, is_supported() -> bool

Запусти: cargo check -p wpick-core
Очікувано: 0 errors, допустимі warnings про unused
```

---

## Блок 2 — config.rs

```
Контекст: wpick-core library crate.
Прочитай: CORE.md §config.rs повністю, особливо "Implementation Notes"

Реалізуй wpick-core/src/config.rs:
- WpickConfig, GeneralConfig, PathsConfig, WaylandConfig з #[serde(default)]
- GeneralConfig::default() → volume: 0.8, muted: false
- AppDirs struct з усіма 6 полями
- WpickConfig::load() → читає ~/.config/wpick/config.toml, повертає Default якщо нема файлу
- WpickConfig::save() → атомарний запис через .tmp + rename
- WpickConfig::app_dirs() → обчислює всі шляхи через dirs crate, викликає create_dir_all

Важливо: socket_path = HOME/.wpick.sock (не в підпапці)

Напиши unit test:
#[test] fn test_config_default_volume() — перевіряє що default volume = 0.8

Запусти: cargo test -p wpick-core
```

---

## Блок 3 — discovery.rs

```
Контекст: wpick-core library crate.
Прочитай: CORE.md §discovery.rs повністю, включаючи "VDF Parsing" і "Edge cases"

Реалізуй wpick-core/src/discovery.rs:
- pub struct WallpaperDir { pub id: u64, pub path: PathBuf }
- pub fn find_wallpaper_dirs(config: &WpickConfig) -> crate::Result<Vec<WallpaperDir>>

Алгоритм пошуку Steam root: 5 стандартних шляхів + config.paths.extra_steam_libraries
Парсинг VDF: keyvalues_serde::from_str з LibraryFolders/LibraryEntry structs
Шлях до обоїв: {library_path}/steamapps/workshop/content/431960/

Edge cases (не panic, не error — просто skip + tracing::warn):
- VDF не існує → skip
- Dir name не є u64 → skip  
- 431960/ не існує → skip

Напиши 2 unit tests:
1. test_parse_vdf_single() — парсинг VDF з одним шляхом (використай тимчасовий файл)
2. test_skip_non_numeric_dir() — перевіряє що "thumbnails" папка ігнорується

Запусти: cargo test -p wpick-core
```

---

## Блок 4 — pkg.rs

```
Контекст: wpick-core library crate.
Прочитай: CORE.md §pkg.rs повністю, включаючи весь "Implementation" алгоритм 10 кроків

Реалізуй wpick-core/src/pkg.rs:
- pub struct ProjectJson з усіма полями, #[serde(rename_all = "camelCase")]
- pub fn extract_and_parse(wallpaper_dir: &WallpaperDir, wallpapers_cache: &Path) -> crate::Result<Option<WallpaperInfo>>

Логіка mtime invalidation:
- .pkg_mtime файл у out_dir зберігає u64 seconds
- Якщо mtime збігся → skip extraction
- Якщо ні → remove_dir_all + create_dir_all + depkg::extract + write new mtime

Повертай Ok(None) (не error) коли:
- scene.pkg відсутній
- wallpaper_type ≠ "video"
- file path всередині не існує

Запусти: cargo check -p wpick-core (тести для цього модуля потребують реального PKG)
```

---

## Блок 5 — cache.rs

```
Контекст: wpick-core library crate.
Прочитай: CORE.md §cache.rs, включаючи SQL schema і "Row mapping" приклад

Реалізуй wpick-core/src/cache.rs:
- pub struct Cache { conn: rusqlite::Connection }
- const SCHEMA_SQL: &str — CREATE TABLE IF NOT EXISTS wallpapers + meta, з STRICT
- Cache::open() → відкриває БД, execute_batch(SCHEMA_SQL), WAL mode
- Cache::get_all() → SELECT * ORDER BY title ASC, map rows до WallpaperInfo
- Cache::get_by_id() → SELECT WHERE id = ?
- Cache::upsert() → INSERT OR REPLACE з pkg_mtime_secs
- Cache::get_pkg_mtime() → SELECT pkg_mtime_secs WHERE id = ?
- Cache::prune() → DELETE WHERE id NOT IN (...)
- Cache::count() → SELECT COUNT(*)

Напиши 3 unit tests (використовуй tempfile::TempDir):
1. test_open_creates_schema() — після open, таблиця wallpapers існує
2. test_upsert_and_retrieve() — upsert + get_by_id повертає ті самі дані
3. test_prune() — після prune, видалений ID відсутній

Запусти: cargo test -p wpick-core
```

---

## Блок 6 — ipc.rs

```
Контекст: wpick-core library crate.
Прочитай: CORE.md §ipc.rs, PROJECT.md §"IPC Protocol Specification"

Реалізуй wpick-core/src/ipc.rs:
- ClientCommand enum з #[serde(tag = "type")] — всі 8 варіантів
- DaemonResponse enum з #[serde(tag = "type")] — всі 5 варіантів
- send_command<W: AsyncWrite + Unpin>() — to_string + \n + write_all + flush
- recv_response<R: AsyncBufRead + Unpin>() — read_line + перевірка EOF + from_str
- send_response і recv_command — аналогічно

Критично: 
- Після write_all ЗАВЖДИ flush
- При read_line: якщо n == 0 → WpickError::IpcClosed

Напиши round-trip тест для КОЖНОГО варіанту (в #[tokio::test]):
- Всі 8 ClientCommand варіантів: serialize → deserialize → рівні
- Всі 5 DaemonResponse варіантів: serialize → deserialize → рівні

Запусти: cargo test -p wpick-core -- --nocapture
Очікувано: мінімум 13 тестів зелені
```

---

## Блок 7+8 — state.rs + ipc_server.rs

```
Контекст: wpick-daemon binary crate.
Прочитай: DAEMON.md §state.rs, §ipc_server.rs, §main.rs — "Signal Handling"

Реалізуй wpick-daemon/src/state.rs:
- DaemonState struct з watch/broadcast Sender полями
- Методи: set_wallpaper(), stop(), set_volume(), toggle_mute(), set_paused()
- Правило: НІКОЛИ не тримати Mutex guard через .await

Реалізуй wpick-daemon/src/ipc_server.rs:
- pub async fn run(listener, state, cache, dirs) — loop accept + spawn
- handle_connection() — loop recv_command + dispatch + send_response
- dispatch() — match по всіх ClientCommand варіантах
- scan_and_populate_cache() — spawn_blocking, ітерує wallpaper_dirs, кешує

Реалізуй wpick-daemon/src/main.rs (тільки IPC частина — без renderer):
- Config::load + AppDirs
- Stale socket detection
- UnixListener::bind
- Signal handlers (SIGINT + SIGTERM) що видаляють сокет
- spawn ipc_server task

Перевірка:
cargo run -p wpick-daemon &
sleep 1
echo '{"type":"List"}' | nc -U ~/.wpick.sock
# Очікувано: {"type":"WallpaperList","items":[...]}
```

---

## Блок 9 — video.rs

```
Контекст: wpick-daemon binary crate.
Прочитай: DAEMON.md §video.rs повністю, включаючи "Handle non-contiguous frames"

Реалізуй wpick-daemon/src/video.rs:
- pub struct VideoDecoder { input_ctx, video_stream_idx, decoder, scaler, fps }
- VideoDecoder::open(path: &str) → ffmpeg::init() + best video stream + RGBA scaler
- next_frame_rgba() → Option<(Vec<u8>, u32, u32)> — None на EOF
- seek_to_start() → seek(0) + decoder.flush()
- frame_duration() → Duration від fps (guard: .max(1.0))
- dimensions() → (u32, u32)

Критично для scaler: output format = Pixel::RGBA (не RGB24)
Критично для frame data: перевіряй stride(0) == width*4, інакше strip padding

Напиши тест (потребує тестовий mp4):
#[test] fn test_decode_first_frame() {
    // Генеруй тест-відео: ffmpeg -f lavfi -i testsrc=duration=1:size=320x240:rate=30 /tmp/test.mp4
    let mut dec = VideoDecoder::open("/tmp/test.mp4").unwrap();
    let frame = dec.next_frame_rgba().unwrap();
    assert!(frame.is_some());
    let (data, w, h) = frame.unwrap();
    assert_eq!(w, 320);
    assert_eq!(h, 240);
    assert_eq!(data.len(), 320 * 240 * 4);
}
```

---

## Блок 10 — audio.rs

```
Контекст: wpick-daemon binary crate.
Прочитай: DAEMON.md §audio.rs повністю

Реалізуй wpick-daemon/src/audio.rs:
- AudioSamples struct: { samples: Vec<f32>, pos: usize, sample_rate: u32, channels: u16 }
- impl Iterator for AudioSamples — wrap-around loop (pos % len)
- impl rodio::Source for AudioSamples — channels, sample_rate, total_duration: None
- fn build_audio_source(path) → anyhow::Result<impl Source<Item=f32>>
  - ffmpeg decode + resample до stereo f32 48000Hz
  - повертає AudioSamples
- pub async fn run(wallpaper_rx, volume_rx, pause_rx) — watch polling loop

Критично:
- _output_stream живе весь час циклу
- sink.set_volume(if muted { 0.0 } else { vol })
- Якщо файл не має аудіо → tracing::info!, не panic

Тест:
#[test] fn test_audio_samples_loops() {
    let src = AudioSamples { samples: vec![1.0, 2.0, 3.0], pos: 0, sample_rate: 48000, channels: 2 };
    let samples: Vec<f32> = src.take(7).collect();
    assert_eq!(samples, vec![1.0, 2.0, 3.0, 1.0, 2.0, 3.0, 1.0]);
}
```

---

## Блок 11 — renderer.rs

```
Контекст: wpick-daemon binary crate.
Прочитай: DAEMON.md §renderer.rs ПОВНІСТЮ — кожен підрозділ.
Окремо: "Wayland Threading Rule" — обов'язково.
Окремо: ERRORS_TO_AVOID.md E-05, E-06, E-07, E-08, E-09, E-10, E-11

Реалізуй wpick-daemon/src/renderer.rs:
- RendererContext struct: wl_display, wl_surface, layer_surface, device, queue, surface, pipeline, video_texture, bind_group
- RendererContext::init(config) → повна Wayland + wgpu ініціалізація
- fn upload_frame(queue, texture, rgba_data, width, height)
- pub async fn run(wallpaper_rx, config, shutdown_rx) → render loop

Wayland init ТОЧНА послідовність (з DAEMON.md):
1. connect → 2. registry → 3. bind globals → 4. create_surface → 5. get_layer_surface
6. set_anchor(all) → 7. set_size(0,0) → 8. commit → 9. roundtrip
→ отримай configure(serial, w, h) → 10. ack_configure(serial) → 11. commit
→ тільки після цього ініціалізуй wgpu

wgpu: Backends::VULKAN, формат з surface.get_capabilities().formats[0]
Шейдери: читай з assets/vertex.wgsl і assets/fragment.wgsl

Перевірка: запусти daemon, встанови обої — layer surface має з'явитися на моніторі
```

---

## Блоки 13–16 — wpick-tui

```
Контекст: wpick-tui binary crate.
Прочитай: TUI.md повністю, PROJECT.md §IPC Protocol

Реалізуй в такому порядку:

1. wpick-tui/src/client.rs:
   IpcClient { reader: BufReader<OwnedReadHalf>, writer: BufWriter<OwnedWriteHalf> }
   connect(socket_path) → корисне повідомлення про помилку
   send(&cmd) → DaemonResponse
   list_wallpapers() → Vec<WallpaperInfo>

2. wpick-tui/src/app.rs:
   App struct з усіма полями (CORE.md §app.rs)
   App::run() → terminal setup + event loop
   handle_key() → всі клавіші з TUI.md §Key Handler
   Всі cmd_* методи
   try_reconnect() → timeout 200ms

3. wpick-tui/src/ui.rs:
   render(frame, app) → перевірка мін. розміру 80×20
   Vertical layout: [1, fill, 2]
   Horizontal: [30%, 70%]
   render_list() → render_stateful_widget (не render_widget!)
   render_detail() → empty state коли немає обоїв

4. wpick-tui/src/main.rs:
   enable_raw_mode → run_app → ЗАВЖДИ disable_raw_mode + LeaveAlternateScreen

Перевірки:
- Без daemon: показує "Disconnected", не крашиться
- Ctrl+C: термінал відновлений
- Q: daemon зупиняється, сокет видаляється
```

---

## Промпти-корекції (коли результат не той)

### Якщо Claude Code написав занадто складно
```
Спрости реалізацію. Видали всі абстракції що не потрібні для MVP.
Кожна функція має робити одну річ. Максимум 50 рядків на функцію.
Без trait objects де можна уникнути. Без зайвих generics.
```

### Якщо є unwrap() в продакшн коді
```
Замінь всі .unwrap() і .expect() в не-тестовому коді.
Використовуй ? для propagation, .ok_or(WpickError::...) для Option→Result,
.map_err(|e| WpickError::Config(e.to_string())) для зовнішніх помилок.
```

### Якщо порушено залежності між крейтами
```
Перевір: wpick-core не має імпортів з wayland-client, wgpu, ratatui, anyhow.
wpick-tui не має імпортів з wpick-daemon.
wpick-daemon не має імпортів з wpick-tui.
Виправ порушення — перенеси код у відповідний крейт або в wpick-core.
```

### Якщо blocking в async
```
Знайди всі виклики блокуючих функцій всередині async fn.
Перенеси їх у tokio::task::spawn_blocking(|| { ... }).await?
Блокуючі: будь-який файловий I/O (крім tokio::fs), rusqlite, depkg, serde на великих даних.
```
