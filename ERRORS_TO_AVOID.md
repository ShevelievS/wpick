# ERRORS_TO_AVOID.md

> Оновлюй цей файл ОДРАЗУ як зустрів нову помилку — до того як виправив.  
> Формат: **Що зробив** → **Що сталось** → **Як правильно**  
> Переглядай перед кожним новим блоком.

---

## Async / Tokio

### E-01: Mutex через await
**Зробив:** Тримав `tokio::sync::Mutex` guard через `.await` точку  
**Сталось:** Дедлок або `MutexGuard is not Send` помилка компілятора  
**Правильно:**
```rust
// WRONG:
let guard = state.lock().await;
some_async_fn().await;

// CORRECT:
let value = state.lock().await.some_field.clone();
// guard дропнувся тут ^
some_async_fn(value).await;
```

### E-02: std::sync::Mutex в async контексті
**Зробив:** Використав `std::sync::Mutex` для стану що шариться між async задачами  
**Сталось:** `MutexGuard<T> cannot be held across an await point`  
**Правильно:** Тільки `tokio::sync::Mutex` для стану між async задачами

### E-03: Blocking I/O в async задачі
**Зробив:** Виклик `depkg::extract()` або `serde_json::from_str` на великих даних прямо в async  
**Сталось:** Блокував tokio worker thread — інші задачі зависали  
**Правильно:**
```rust
tokio::task::spawn_blocking(|| {
    depkg::extract(&pkg_path, &out_dir)
}).await??;
```

### E-04: sleep(0) замість yield_now()
**Зробив:** `tokio::time::sleep(Duration::from_millis(0)).await` для yield  
**Сталось:** Не гарантує yield, може не дати іншим задачам виконатись  
**Правильно:** `tokio::task::yield_now().await`

---

## Wayland

### E-05: Рендер до ack_configure
**Зробив:** Спробував рендерити на layer surface до отримання configure event  
**Сталось:** Чорний екран, жодних помилок  
**Правильно:** Суворий порядок:
```
commit() → roundtrip() → отримай configure(serial, w, h) → ack_configure(serial) → commit() → ТЕПЕР рендер
```

### E-06: wl_surface без commit після змін
**Зробив:** Змінив параметри layer_surface але не викликав `wl_surface.commit()`  
**Сталось:** Зміни ігноруються compositor'ом, поведінка непередбачувана  
**Правильно:** Після будь-яких змін surface → завжди `wl_surface.commit()`

### E-07: Wayland dispatch в tokio::spawn
**Зробив:** Запустив `EventQueue::dispatch()` в звичайному `tokio::spawn`  
**Сталось:** Паніка або UB — `wl_surface` не є `Send`  
**Правильно:** Renderer запускається або на main thread або в `tokio::task::spawn_local()` з `LocalSet`

### E-08: Не зареєстрував wl_output
**Зробив:** Пропустив bind `wl_output` при реєстрації глобалів  
**Сталось:** `configure` event приходить з розміром 0×0  
**Правильно:** Обов'язково bind `wl_output` → дочекайся `mode` event → тоді отримуєш реальні розміри

---

## wgpu

### E-09: Не той Backends на Linux
**Зробив:** `wgpu::Backends::all()` або `wgpu::Backends::METAL`  
**Сталось:** Fallback на GLES замість Vulkan, або взагалі немає адаптера  
**Правильно:**
```rust
wgpu::Instance::new(wgpu::InstanceDescriptor {
    backends: wgpu::Backends::VULKAN,
    ..Default::default()
})
```
Fallback: `VULKAN | GL` якщо треба підтримка старого заліза

### E-10: Texture upload з padding
**Зробив:** Передав `data(0)` від ffmpeg frame напряму до `write_texture`  
**Сталось:** Відео з'їхало горизонтально — ffmpeg stride ≠ width×4  
**Правильно:** Завжди перевіряй `stride(0)` і прибирай padding:
```rust
if rgba_frame.stride(0) == (width * 4) as usize {
    // можна напряму
} else {
    // треба strip padding — дивись DAEMON.md §video
}
```

### E-11: Surface format mismatch
**Зробив:** Hardcode `TextureFormat::Rgba8UnormSrgb` для swapchain  
**Сталось:** Panic або некоректні кольори — деякі Wayland compositors хочуть `Bgra8UnormSrgb`  
**Правильно:**
```rust
let format = surface.get_capabilities(&adapter).formats[0];
// використай цей format для SurfaceConfiguration
```

---

## ffmpeg-next

### E-12: ffmpeg::init() не викликаний
**Зробив:** Пропустив `ffmpeg::init()` на початку  
**Сталось:** Segfault або "No such decoder" при відкритті файлу  
**Правильно:** Виклик `ffmpeg::init()?` — один раз, на початку `VideoDecoder::open()`

### E-13: Не скинуто decoder buffer після seek
**Зробив:** `input_ctx.seek(0, ..)` без `decoder.flush()`  
**Сталось:** Перші кадри після seek — corrupted, "green garbage"  
**Правильно:**
```rust
self.input_ctx.seek(0, ..)?;
self.decoder.flush();  // обов'язково!
```

### E-14: Пакети від аудіо-стріму потрапляли у відео-декодер
**Зробив:** Передавав всі пакети в decoder без фільтрації по stream index  
**Сталось:** `decode error: invalid data found when processing input`  
**Правильно:**
```rust
for (stream, packet) in ctx.packets() {
    if stream.index() != self.video_stream_idx { continue; }
    self.decoder.send_packet(&packet)?;
}
```

### E-15: ffmpeg-next не бачить системний ffmpeg
**Зробив:** `cargo build` без встановлених dev libraries  
**Сталось:** `pkg-config: command not found` або `Could not find ffmpeg`  
**Правильно:**
```bash
# Arch:
sudo pacman -S ffmpeg pkgconf
# Перевірка:
pkg-config --libs libavcodec
```

---

## rodio / Audio

### E-16: OutputStream dropped раніше Sink
**Зробив:** `let sink = Sink::try_new(&stream_handle)?` але `_output_stream` дропнувся  
**Сталось:** Тиша — аудіо відтворюється в нікуди  
**Правильно:** `_output_stream` і `stream_handle` мають жити весь час відтворення
```rust
let (_output_stream, stream_handle) = OutputStream::try_default()?;
// обидва мають scope що включає весь audio loop
```

### E-17: AudioSamples повертає None
**Зробив:** Реалізував `Iterator::next()` без wrap-around при досягненні кінця  
**Сталось:** Аудіо грає один раз і зупиняється, rodio вважає Source вичерпаним  
**Правильно:**
```rust
fn next(&mut self) -> Option<f32> {
    if self.samples.is_empty() { return None; }
    let val = self.samples[self.pos];
    self.pos = (self.pos + 1) % self.samples.len();  // loop!
    Some(val)
}
```

---

## IPC / Serde

### E-18: Забутий flush після write
**Зробив:** `writer.write_all(json.as_bytes()).await?` без `writer.flush().await?`  
**Сталось:** Повідомлення застрягло в буфері `BufWriter`, інша сторона чекала нескінченно  
**Правильно:** Завжди після кожного запису:
```rust
writer.write_all(json.as_bytes()).await?;
writer.flush().await?;
```

### E-19: serde tag = "type" з одиночними варіантами
**Зробив:** `#[serde(tag = "type")]` на enum де деякі варіанти не мають полів  
**Сталось:** `{"type":"List"}` — OK, `{"type":"Ok"}` — OK  
**Правильно:** Це ПРАВИЛЬНА поведінка для `tag = "type"`. Не потрібні `{}` після варіантів без полів.

### E-20: EOF не перевірявся при read_line
**Зробив:** `reader.read_line(&mut line).await?` без перевірки результату  
**Сталось:** Нескінченний цикл при закритті з'єднання — `line` завжди порожня але помилки нема  
**Правильно:**
```rust
let n = reader.read_line(&mut line).await?;
if n == 0 { return Err(WpickError::IpcClosed); }
```

---

## ratatui / TUI

### E-21: render_widget замість render_stateful_widget для List
**Зробив:** `frame.render_widget(list, area)` для `List` віджету  
**Сталось:** Список рендериться але виділення (highlight) не відображається  
**Правильно:**
```rust
let mut state = ListState::default();
state.select(Some(app.selected));
frame.render_stateful_widget(list, area, &mut state);
```

### E-22: Термінал не відновлений після паніки
**Зробив:** `enable_raw_mode()` без гарантованого cleanup  
**Сталось:** Після паніки термінал залишився в raw mode — клавіші не відображались  
**Правильно:** Обгорни `run_app()` в catch_unwind або використай pattern з main.rs в TUI.md

### E-23: crossterm events блокують event loop
**Зробив:** `crossterm::event::read()` без попереднього `poll(timeout)`  
**Сталось:** UI зависав до натискання клавіші — не оновлювався стан з'єднання  
**Правильно:**
```rust
if crossterm::event::poll(Duration::from_millis(250))? {
    // тільки тоді read()
    let event = crossterm::event::read()?;
}
// інакше просто перемалюй
```

---

## Cargo / Залежності

### E-24: Версії raw-window-handle не збігаються
**Зробив:** `wayland-client` 0.31 + `raw-window-handle` 0.5  
**Сталось:** `expected RawWindowHandle from version 0.5, found 0.6` — тип помилка  
**Правильно:** Перевір `COMPAT.md` — версії мають бути з одного рядка таблиці

### E-25: depkg не знайдено на crates.io
**Зробив:** Спробував `cargo add depkg` — пакет не існує або інша назва  
**Сталось:** `error: package 'depkg' not found`  
**Правильно:** Перевір актуальну назву: `cargo search wallpaper engine` або шукай на crates.io.  
Fallback: реалізувати PKG парсер вручну (формат описаний в CORE.md)

### E-26: ffmpeg-next "7" несумісне з system FFmpeg 8.x (Arch 2025+)
**Зробив:** `ffmpeg-next = "7"` в wpick-daemon/Cargo.toml на Arch Linux з FFmpeg n8.1  
**Сталось:** `fatal error: '/usr/include/libavcodec/avfft.h' file not found` — avfft.h було видалено у FFmpeg 7.1, struct size mismatch у bindings  
**Правильно:** Використовуй `ffmpeg-next = "8"` для system FFmpeg 8.x.  
Перевірка: `ffmpeg -version | head -1` → якщо `n8.x` → потрібна `ffmpeg-next = "8"`

### E-27: Newer PKG format with length-prefixed magic (`\x08\0\0\0PKGV`)
**Зробив:** PKG extractor перевіряв тільки `PKGV0001` / `PKGV0005` magic  
**Сталось:** `Unknown magic: "\u{8}\0\0\0PKGV"` — деякі wallpapers мають формат з u32 length-prefix перед `PKGV`  
**Правильно:** При перевірці magic bytes також перевіряй байти 4..12 на `PKGV`:
```rust
let is_pkgv = magic == b"PKGV0001" || magic == b"PKGV0005"
    || (data.len() >= 12 && &data[4..8] == b"PKGV");
```
Якщо знайдено length-prefix варіант — offset `pos = 4` (skip u32), тоді читай версію.  
Або: gracefully skip як `Ok(None)` — ці wallpapers скоріш за все не video type.

---

## Шаблон для нового запису

```
### E-XX: [Коротка назва]
**Зробив:** [що саме зробив]
**Сталось:** [що пішло не так]
**Правильно:**
[код або опис правильного рішення]
```
