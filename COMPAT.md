# COMPAT.md — Версії крейтів що точно працюють разом

> Це найважливіший файл для уникнення втрат часу на версійні конфлікти.  
> raw-window-handle — найчастіша причина проблем: він повинен бути однаковим  
> у всіх крейтах що торкаються Wayland/wgpu.

---

## Перевірена комбінація (MVP)

```toml
# Workspace root Cargo.toml
[workspace.dependencies]
serde       = { version = "1",      features = ["derive"] }
serde_json  = "1"
thiserror   = "2"
anyhow      = "1"
tokio       = { version = "1",      features = ["full"] }
tracing     = "0.1"

# wpick-core
toml              = "0.8"
dirs              = "5"
rusqlite          = { version = "0.32", features = ["bundled"] }
keyvalues-serde   = "0.2"
walkdir           = "2"
# depkg: перевір актуальну версію на crates.io (може бути під іншою назвою)
# Fallback: реалізувати парсер вручну за форматом PKGV0001/PKGV0005

# wpick-daemon
wayland-client          = "0.31"
wayland-protocols-wlr   = { version = "0.3", features = ["client"] }
wgpu                    = "22"
raw-window-handle       = "0.6"      # ← МАЄ збігатися з тим що використовує wgpu 22
bytemuck                = { version = "1", features = ["derive"] }
ffmpeg-next             = "8"        # потребує system ffmpeg 8.x (Arch 2025+); для ffmpeg 7.x використовуй "7"
rodio                   = "0.19"
parking_lot             = "0.12"
tracing-subscriber      = { version = "0.3", features = ["env-filter"] }
tracing-appender        = "0.2"

# wpick-tui
ratatui     = "0.29"
crossterm   = "0.28"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
```

---

## Критичні залежності версій

### raw-window-handle ↔ wgpu

| wgpu | raw-window-handle | Статус |
|------|------------------|--------|
| 22.x | 0.6 | ✅ Перевірено |
| 21.x | 0.6 | ✅ |
| 20.x | 0.6 | ✅ |
| 19.x | 0.5 | ⚠️ Старе API |
| 23.x | 0.6 | 🔍 Ймовірно ОК — перевір при появі |

**Правило**: Перевіряй `wgpu/Cargo.toml` на GitHub щоб знати яку версію rwh очікує.

### wayland-client ↔ wayland-protocols-wlr

| wayland-client | wayland-protocols-wlr | Статус |
|----------------|----------------------|--------|
| 0.31 | 0.3 | ✅ |
| 0.30 | 0.2 | ✅ але застарілі |
| 0.31 | 0.2 | ❌ не сумісні |

### ratatui ↔ crossterm

| ratatui | crossterm | Статус |
|---------|-----------|--------|
| 0.29 | 0.28 | ✅ |
| 0.28 | 0.27 | ✅ |
| 0.29 | 0.27 | ❌ |

---

## Як перевірити конфлікти до build

```bash
# Перевірка дерева залежностей:
cargo tree -p wpick-daemon | grep raw-window-handle
# Всі рядки мають показувати ОДНАКОВУ версію rwh

# Перевірка дублікатів:
cargo tree --duplicates
# Ідеально: порожній вивід
# Прийнятно: дублікати тільки по patch версіях (0.6.0 vs 0.6.1)
# Проблема: дублікати по minor версіях (0.5.x vs 0.6.x)
```

---

## Проблема з depkg

На момент написання специфікації, `depkg` може бути:
- Назва крейту відрізняється
- Не опублікований на crates.io
- Тільки як git dependency

**Пошук:**
```bash
cargo search wallpaper
cargo search pkgv
```

**Fallback — самостійна реалізація PKG парсера:**

Формат PKGV0001 (спрощено):
```
Magic: "PKGV0001" або "PKGV0005" (8 bytes)
Header: кількість файлів (u32 little-endian)
Для кожного файлу:
  - filename length (u32 le)
  - filename (UTF-8 bytes)
  - file data length (u32 le)
  - file data (bytes)
```

Мінімальна реалізація:
```rust
pub fn extract_pkg(pkg_path: &Path, out_dir: &Path) -> anyhow::Result<()> {
    let data = std::fs::read(pkg_path)?;
    let magic = &data[0..8];
    
    anyhow::ensure!(
        magic == b"PKGV0001" || magic == b"PKGV0005",
        "Not a valid PKG file: {:?}", pkg_path
    );
    
    let mut pos = 8usize;
    let file_count = u32::from_le_bytes(data[pos..pos+4].try_into()?) as usize;
    pos += 4;
    
    for _ in 0..file_count {
        let name_len = u32::from_le_bytes(data[pos..pos+4].try_into()?) as usize;
        pos += 4;
        let name = std::str::from_utf8(&data[pos..pos+name_len])?;
        pos += name_len;
        
        let data_len = u32::from_le_bytes(data[pos..pos+4].try_into()?) as usize;
        pos += 4;
        let file_data = &data[pos..pos+data_len];
        pos += data_len;
        
        let out_path = out_dir.join(name);
        if let Some(parent) = out_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&out_path, file_data)?;
    }
    
    Ok(())
}
```

**Увага**: Реальний формат складніший — textures (.tex) потребують конвертації.  
Якщо `depkg` не знайдено, шукай `wpengine` або `we_pkg` на crates.io.

---

## System ffmpeg версії

| ffmpeg-next | System ffmpeg | Статус |
|-------------|---------------|--------|
| 8.x | 8.x | ✅ Перевірено (Arch 2025) |
| 7.x | 7.x, 6.x | ✅ |
| 6.x | 6.x, 5.x | ✅ |
| 7.x | 8.x | ❌ avfft.h removed, struct size mismatch |
| 7.x | 4.x | ❌ |

```bash
# Перевірка версії system ffmpeg:
ffmpeg -version | head -1
pkg-config --modversion libavcodec
```

Arch Linux завжди має останній ffmpeg — зазвичай 7.x.  
Ubuntu LTS може мати старіший — використовуй PPA або збирай вручну.

---

## Оновлення версій

При оновленні будь-якого крейту з цієї таблиці:
1. Запусти `cargo tree --duplicates` — перевір нові конфлікти
2. Оновлюй версії в парі (наприклад, wgpu + raw-window-handle разом)
3. Зафіксуй нову перевірену комбінацію в цьому файлі
