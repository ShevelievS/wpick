# wpick — Exact Working Sequence

> **Головний операційний файл.** Відкривай першим у кожній сесії.  
> Тут немає "що реалізувати" — це є в .md файлах.  
> Тут є **коли**, **в якому порядку** і **як перевірити перед рухом далі**.

---

## Структура сесії (кожен раз)

```
1. Відкрий SEQUENCE.md (цей файл)  → знайди поточний блок
2. Відкрий PROJECT.md               → освіжи архітектурний контекст
3. Відкрий відповідний .md          → специфікація блоку
4. Відкрий відповідний skill        → процедура реалізації
5. Реалізуй
6. Пройди gate-перевірку            → тільки потім переходь далі
7. Постав ✅ в цьому файлі
```

---

## Карта прогресу

| # | Блок | Файли | Gate | Статус |
|---|------|-------|------|--------|
| 0 | Scaffold | `wpick-scaffold.md` | `cargo check --workspace` ✓ | ✅ |
| 1 | error.rs + model.rs | `CORE.md` §error §model | компілюється без warnings | ✅ |
| 2 | config.rs | `CORE.md` §config | unit test load/save | ✅ |
| 3 | discovery.rs | `CORE.md` §discovery | unit test VDF parse | ✅ |
| 4 | pkg.rs | `CORE.md` §pkg | mtime перевірка працює | ✅ |
| 5 | cache.rs | `CORE.md` §cache | upsert + get_all тест | ✅ |
| 6 | ipc.rs | `CORE.md` §ipc | round-trip тест всіх варіантів | ✅ |
| 7 | state.rs | `DAEMON.md` §state | компілюється | ✅ |
| 8 | ipc_server.rs | `DAEMON.md` §ipc_server | `echo '{"type":"List"}' \| nc -U ~/.wpick.sock` | ✅ |
| 9 | video.rs | `DAEMON.md` §video | decode 10 frames від тестового mp4 | ✅ |
| 10 | audio.rs | `DAEMON.md` §audio | AudioSamples::next() не паніки | ✅ |
| 11 | renderer.rs | `DAEMON.md` §renderer | layer surface з'являється на моніторі | ✅ |
| 12 | daemon main.rs | `DAEMON.md` §main | `wpick-daemon` стартує, сокет є, SIGTERM прибирає | ✅ |
| 13 | client.rs | `TUI.md` §client | connect + List command | ⬜ |
| 14 | app.rs | `TUI.md` §app | всі keys обробляються | ⬜ |
| 15 | ui.rs | `TUI.md` §ui | рендер без паніки | ⬜ |
| 16 | tui main.rs | `TUI.md` §main | термінал відновлюється після Ctrl+C | ⬜ |
| 17 | Інтеграція | усі файли | set wallpaper → відображається | ⬜ |

---

## Блок 0 — Scaffold

**Файли для сесії:**
```
skills/wpick-scaffold.md     ← основна інструкція
PROJECT.md                   ← dependency table (секція "Complete Dependency Table")
```

**Команди:**
```bash
mkdir wpick && cd wpick
git init
# Створюй файли за wpick-scaffold.md кроками 1–6
cargo check --workspace
```

**Gate (не рухайся далі поки):**
```
✓ cargo check --workspace — 0 errors
✓ grep -c "anyhow" wpick-core/Cargo.toml → 0
✓ grep -c "wpick-daemon" wpick-tui/Cargo.toml → 0
```

---

## Блоки 1–6 — wpick-core

**Файли для кожної сесії:**
```
PROJECT.md                 ← завжди
CORE.md                    ← основна специфікація
ERRORS.md                  ← правила помилок
skills/wpick-core.md       ← процедура
ERRORS_TO_AVOID.md         ← перед кожним блоком переглянь
```

**Порядок всередині кожної сесії:**
```bash
# Перед початком:
cargo check -p wpick-core        # зафіксуй baseline

# Після реалізації модуля:
cargo check -p wpick-core        # 0 errors
cargo test -p wpick-core         # всі тести зелені
cargo clippy -p wpick-core       # 0 warnings у новому коді
```

**Gate після блоку 6 (ipc.rs):**
```bash
cargo test -p wpick-core -- --nocapture
# Очікувано: мінімум 7 тестів пройшло
# Обов'язково: round-trip тест для КОЖНОГО варіанту ClientCommand і DaemonResponse
```

---

## Блоки 7–12 — wpick-daemon

**Файли для кожної сесії:**
```
PROJECT.md                 ← завжди
DAEMON.md                  ← основна специфікація
ERRORS.md                  ← log levels
skills/wpick-daemon.md     ← процедура + шейдери
ERRORS_TO_AVOID.md         ← ОБОВ'ЯЗКОВО перед renderer.rs
```

**Критично для Блоку 11 (renderer.rs):**
```
Перед кодом прочитай DAEMON.md §"Wayland Initialization (Critical Sequence)"
та DAEMON.md §"Wayland Threading Rule" повністю.
Це єдиний блок де порядок операцій абсолютно точний — будь-яке відхилення
дає чорний екран без помилок.
```

**Gates:**

Блок 8 (ipc_server):
```bash
cargo run -p wpick-daemon &
sleep 1
echo '{"type":"List"}' | nc -U ~/.wpick.sock
# Очікувано: {"type":"WallpaperList","items":[...]}
kill %1
```

Блок 9 (video.rs):
```bash
# Потрібен тестовий mp4. Скачай будь-яке відео або:
ffmpeg -f lavfi -i testsrc=duration=5:size=1280x720:rate=30 /tmp/test.mp4
# Напиши тест:
cargo test -p wpick-daemon test_decode_frames -- --nocapture
```

Блок 11 (renderer.rs):
```bash
# Запусти і подивись очима — layer surface має з'явитися
cargo run -p wpick-daemon
# Перевір лог:
tail -f ~/.local/share/wpick/wpick.log
# Очікувано: "Wayland surface configured: WxH"
```

Блок 12 (main.rs):
```bash
cargo run -p wpick-daemon &
ls -la ~/.wpick.sock          # сокет існує
kill %1
ls -la ~/.wpick.sock          # сокет видалено — це важливо
```

---

## Блоки 13–16 — wpick-tui

**Файли для кожної сесії:**
```
PROJECT.md                 ← завжди
TUI.md                     ← основна специфікація
ERRORS.md                  ← user-facing messages
skills/wpick-tui.md        ← процедура
```

**Тестуй client.rs до написання UI:**
```bash
cargo run -p wpick-daemon &
# Напиши тимчасовий main.rs що тільки підключається і робить List:
cargo run -p wpick-tui
# Переконайся що бачиш список обоїв у дебаг-виводі
```

**Gate Блок 16:**
```bash
cargo run -p wpick-tui
# 1. Ctrl+C → термінал нормальний (не залишився raw mode)
# 2. Запусти без daemon → бачиш "Disconnected" але не crash
# 3. Запусти daemon після tui → tui підключається автоматично
```

---

## Блок 17 — Інтеграція

```bash
# Повний smoke test:
cargo build --workspace --release

# Terminal 1:
./target/release/wpick-daemon

# Terminal 2:
./target/release/wpick

# Checklist:
# ✓ Список обоїв завантажується
# ✓ Enter → обої змінюються на моніторі
# ✓ +/- → гучність змінюється
# ✓ a → mute/unmute
# ✓ q → TUI виходить, daemon продовжує
# ✓ Q → daemon зупиняється, сокет видаляється
# ✓ r → оновлення списку
```

---

## Як зберігати прогрес між сесіями

Після кожного завершеного блоку:

1. Постав ✅ в таблиці вище
2. `git add -A && git commit -m "Block N: [назва]"`
3. Якщо з'явилася нова помилка — запиши в `ERRORS_TO_AVOID.md`
4. Якщо змінилося рішення — онови відповідний `.md` файл

---

## Якщо щось пішло не так

| Ситуація | Дія |
|----------|-----|
| Блок компілюється але не працює | Перечитай DAEMON.md §"Known Difficult Points" |
| Версії крейтів конфліктують | Відкрий `COMPAT.md` → перевір таблицю |
| Wayland чорний екран | Перечитай "Wayland Init Sequence" кроки 1–9 |
| Claude Code зробив не те | Зверни до точнішого промпту в `PROMPTS.md` |
| Нова помилка | Запиши в `ERRORS_TO_AVOID.md` ПЕРЕД виправленням |

---

## Файли проекту — повна карта

```
wpick/
├── SEQUENCE.md              ← ТИ ТУТ — відкривай першим
├── ERRORS_TO_AVOID.md       ← оновлюй при кожній новій помилці
├── PROMPTS.md               ← готові промпти для Claude Code
├── COMPAT.md                ← версії крейтів що точно працюють разом
│
├── docs/
│   ├── PROJECT.md           ← архітектура, flow, залежності
│   ├── CORE.md              ← специфікація wpick-core
│   ├── TUI.md               ← специфікація wpick-tui
│   ├── DAEMON.md            ← специфікація wpick-daemon
│   └── ERRORS.md            ← правила помилок
│
├── skills/
│   ├── wpick-index.md       ← карта скіллів
│   ├── wpick-scaffold.md
│   ├── wpick-core.md
│   ├── wpick-daemon.md
│   └── wpick-tui.md
│
├── assets/
│   ├── vertex.wgsl          ← готовий вертекс шейдер
│   └── fragment.wgsl        ← готовий фрагмент шейдер
│
├── Cargo.toml               ← workspace
├── wpick-core/
├── wpick-tui/
└── wpick-daemon/
```
