# PROMPTS_V0.2.md — Claude Code prompts for v0.2.0

> Copy-paste prompts, one per block. Each prompt is self-contained and
> tells Claude Code exactly which files to read, what to change, and
> what the gate is. Blocks 18–26 in `SEQUENCE.md` map 1:1 to sections
> below.
>
> Use `PROMPTS.md` for v0.1 blocks (0–17); that file is unchanged.

---

## Block 18 — Extended config schema

```
Контекст: wpick v0.2.0, Block 18 — расширение config schema.
Читай целиком: docs/PROJECT.md, docs/CORE.md §config.rs,
skills/wpick-core.md, COMPAT.md, ERRORS_TO_AVOID.md, docs/CONFIG.md.

Цель: расширить WpickConfig до схемы, которая покроет multi-monitor,
pause triggers, streaming audio, autostart.

Реализуй в wpick-core/src/config.rs:

1. Новые структуры (все с #[serde(default)] на уровне struct):
   - FitMode { Fill, Fit, Stretch, Center } с
     #[serde(rename_all = "lowercase")], Default = Fill.
   - MonitorConfig { wallpaper_id: Option<u64>, fit: FitMode, mute: bool }
   - PauseConfig { on_fullscreen, on_battery, on_lid_close }
   - AudioConfig { chunk_frames, max_preload_mb, ducking_enabled }

2. Defaults:
   - PauseConfig::default() → on_fullscreen: true, остальное false.
   - AudioConfig::default() → chunk_frames: 2048, max_preload_mb: 50,
     ducking_enabled: true.
   - FitMode::default() → Fill.
   - autostart: false.

3. Добавь в WpickConfig:
   pub monitors:  HashMap<String, MonitorConfig>,
   pub pause:     PauseConfig,
   pub audio:     AudioConfig,
   pub autostart: bool,

   Все с #[serde(default)]. Это критично — без этого старые config.toml
   сломаются.

4. Тесты (в том же файле):
   - test_default_config_volume (уже есть — не трогай)
   - test_save_and_reload расширь: новые поля должны round-trip-нуться.
   - Новый test_v01_config_forward_compat: запиши минимальный TOML
     (только [general]), загрузи, проверь что monitors пуст,
     pause.on_fullscreen = true, audio.chunk_frames = 2048.
   - Новый test_v02_full_config_roundtrip: полный TOML со всеми
     секциями → save → load → identical.

5. Обнови skills/wpick-core.md §config.rs — добавь подсекцию
   "v0.2 extended schema" со всеми новыми структурами и defaults.

6. Обнови docs/CORE.md §config.rs если расхождение со спецификацией.

7. Обнови docs/CONFIG.md если где-то расходится с реализацией (должно
   уже совпадать — сверь).

Gate:
  cargo test -p wpick-core config::tests → все зелёные
  cargo test -p wpick-core → регрессии отсутствуют
  cargo clippy -p wpick-core -- -D warnings → 0

Новые ошибки → в ERRORS_TO_AVOID.md (E-28+) ДО исправления.
```

---

## Block 19 — Frame buffer reuse

```
Контекст: wpick v0.2.0, Block 19 — оптимизация video.rs (frame buffer reuse).
Читай: skills/wpick-daemon.md §video.rs, docs/DAEMON.md §video.rs,
ERRORS_TO_AVOID.md (E-10, E-11, E-15), wpick-daemon/src/video.rs,
wpick-daemon/src/renderer.rs.

Проблема: next_frame_rgba() делает rgba.data(0).to_vec() на каждый кадр.
При 1920×1080@30fps это ~240 МБ/с allocator churn.

Изменения в wpick-daemon/src/video.rs:

1. В struct VideoDecoder добавь: frame_buf: Vec<u8>.
2. В Self::open() инициализируй Vec::with_capacity(width*height*4).
3. Измени сигнатуру:
   pub fn next_frame_rgba(&mut self) -> anyhow::Result<Option<(&[u8], u32, u32)>>
4. В теле: self.frame_buf.clear(); затем extend_from_slice
   (учитывая padded stride — см. паттерн в docs/DAEMON.md §video.rs).
5. Возвращай &self.frame_buf[..].
6. Адаптируй tests — сигнатура сменилась, но логика test_decoder_reads_frame
   остаётся той же.

Изменения в wpick-daemon/src/renderer.rs:

7. В render_loop (или render-функции Output-рендерера) — upload_frame уже
   принимает &[u8], новая сигнатура совместима без правок на месте
   вызова.
8. Если borrow checker ругается на использование frame через .await —
   сначала extract (width, height), upload перед await, потом await.

Обнови skills/wpick-daemon.md §video.rs — новый checklist содержит правило
"frame_buf переиспользуется".

Обнови docs/DAEMON.md §video.rs — новая сигнатура next_frame_rgba
уже задокументирована; сверь что совпадает.

Если появится новая ошибка (например borrow conflict в renderer) —
добавь E-36 в ERRORS_TO_AVOID.md.

Gate:
  cargo test -p wpick-daemon video::tests → зелёные
  cargo clippy -p wpick-daemon -- -D warnings → 0 в video.rs/renderer.rs
  cargo run -p wpick-daemon + wpick set <id> → видео играет 60+ сек
  (опционально) heaptrack ./target/release/wpick-daemon → <1 аллокация/frame
```

---

## Block 20 — Streaming audio

```
Контекст: wpick v0.2.0, Block 20 — streaming audio decoder.
Читай: skills/wpick-daemon.md §audio.rs, docs/DAEMON.md §audio.rs,
ERRORS_TO_AVOID.md (E-16, E-17, E-28, E-29), wpick-daemon/src/audio.rs,
wpick-daemon/src/ducking.rs (не менять, только понять), docs/CONFIG.md §audio.

Цель: заменить pre-load полного трека на streaming source.
Архитектура: [ffmpeg thread] --(SyncSender cap=4)--> [StreamingSource iter] -> rodio::Sink.

Реализуй в wpick-daemon/src/audio.rs:

1. struct StreamingSource {
     rx:            std::sync::mpsc::Receiver<Vec<f32>>,
     current:       Vec<f32>,
     pos:           usize,
     sample_rate:   u32,
     channels:      u16,
     _shutdown_tx:  std::sync::mpsc::Sender<()>,
   }

2. impl Iterator for StreamingSource — см. docs/DAEMON.md §audio.rs.
   КРИТИЧНО: underrun возвращает Some(0.0), НЕ None (E-28).

3. impl rodio::Source — total_duration None, channels/sample_rate
   из поля.

4. fn spawn_audio_decoder(path, chunk_frames, shutdown_rx)
     -> anyhow::Result<(Receiver<Vec<f32>>, u32, u16)>
   — тред с именем "wpick-audio-dec-<id>". Внутри loop: read_packets
   → decode → resample → накопление → send по chunk_frames.
   EOF → seek_to_start + decoder.flush (E-15 аналог для audio).
   Shutdown: try_recv().is_err() OR tx.send failed → return.

5. Адаптируй build_audio_source() — возвращает StreamingSource.

6. Удали AudioSamples и decode_audio_to_f32 — больше не нужны.

7. Тесты:
   - test_streaming_source_plays_chunks: синтетический канал, 3 chunks,
     проверяем правильный порядок семплов.
   - test_streaming_source_silence_on_underrun: закрытый канал → next()
     возвращает Some(0.0) бесконечно.

8. В pub async fn run() добавь parameter paused_rx: watch::Receiver<bool>
   и audio_cfg: AudioConfig. На paused_rx.has_changed() → sink.pause()/play()
   с last_pause_sent guard.

9. main.rs: прочти config.audio.chunk_frames, передай в audio::run.
   ducking::start не меняется.

Обнови skills/wpick-daemon.md §audio.rs:
- Удали checklist про pre-load.
- Новая секция "Streaming decoder architecture" со схемой.
- Правило: "StreamingSource::next() НИКОГДА не возвращает None"

Обнови docs/DAEMON.md §audio.rs — сверь что реализация совпадает со
спецификацией; если расходится — обнови либо документ, либо код.

Добавь в ERRORS_TO_AVOID.md:
  E-28 уже описан (StreamingSource None → rodio стоп) — обнови если
    что-то прояснилось.
  E-29 уже описан (shutdown не наблюдается) — обнови по факту.

Gate:
  cargo test -p wpick-daemon audio::tests → зелёные
  cargo run -p wpick-daemon + wpick set <video_id_with_audio>
    → звук за <500ms
  ps -o rss= -p $(pidof wpick-daemon) до и после Set:
    ожидаемо ≤ 80 MB на длинных треках (v0.1: 150+).
```

---

## Block 21 — Multi-monitor support

```
Контекст: wpick v0.2.0, Block 21 — multi-monitor support.
Это самый большой блок. При возможности разделить на 21a (IPC+state)
и 21b (renderer refactor + hotplug).

Читай: docs/PROJECT.md, docs/DAEMON.md §renderer.rs + §state.rs,
skills/wpick-daemon.md, ERRORS_TO_AVOID.md §Wayland (E-05..E-08, E-30..E-32),
docs/CONFIG.md §monitors, docs/MULTIMONITOR.md ЦЕЛИКОМ,
wpick-daemon/src/renderer.rs, state.rs, ipc_server.rs,
wpick-core/src/ipc.rs, wpick-core/src/model.rs.

Этап 1 — IPC расширение (wpick-core):
1. В model.rs добавь OutputInfo { name, width, height, scale,
   current_wallpaper_id }.
2. В ipc.rs:
   - ClientCommand::Set { id, #[serde(default)] monitor: Option<String> }
   - ClientCommand::ListOutputs
   - ClientCommand::Status, Pause, Resume
   - DaemonResponse::OutputList { items }
   - DaemonResponse::Status { paused, reasons, monitors: Vec<MonitorStatus> }
   - struct MonitorStatus { name, current_wallpaper_id }
3. Round-trip тесты для всех новых вариантов.
4. Тест v01_set_forward_compat: {"type":"Set","id":1} без monitor
   десериализуется в Set { id: 1, monitor: None }.

Этап 2 — state.rs:
5. DaemonState: HashMap<String, Option<WallpaperInfo>> monitors_current
   вместо одиночного current_wallpaper.
6. monitors_tx: watch::Sender<HashMap<..>>.
7. pub fn set_wallpaper(&mut self, monitor: Option<String>, info, known_outputs: &[String]).
   monitor=None → apply to all known_outputs.
8. outputs_tx: watch::Sender<Vec<OutputInfo>> — renderer публикует, ipc_server читает.
9. pause_override_tx: watch::Sender<Option<bool>>.

Этап 3 — renderer.rs (ядро блока):
10. RendererManager { Connection, EventQueue, WaylandState, shared wgpu,
    HashMap<OutputKey, OutputRenderer> }.
11. OutputRenderer — см. docs/MULTIMONITOR.md §Renderer structure.
12. Общие wgpu::Instance/Adapter/Device/Queue создаются один раз.
13. Pipeline общий когда SurfaceFormat совпадает.
14. Per-output: Surface, bind_group, video_texture, VideoDecoder, frame clock.
15. Hotplug:
    - wl_registry::Event::Global interface=="wl_output" → bind + subscribe.
    - wl_output events Name/Mode/Scale/Done → populate OutputBinding.
    - Done первый раз → promote to OutputRenderer.
    - GlobalRemove → drop OutputRenderer (идемпотентно, E-32).
16. Фильтрация Mode: только с CURRENT flag (E-31).
17. render_loop: diff monitors_rx против renderers map; for each output
    try_render_frame; dispatch_pending для Wayland.
18. FitMode uniform buffer обновляется только при изменении размеров.
19. Publish outputs_tx при hotplug/resize.

Этап 4 — ipc_server.rs:
20. Dispatch Set { id, monitor } → state.set_wallpaper с known из outputs_rx.
21. Dispatch ListOutputs → outputs_rx.borrow().clone().
22. Dispatch Status → собрать paused (через pause::current_reasons()
    — будет в Block 22; пока возвращать empty), monitors из state.
23. Dispatch Pause/Resume → state.set_pause_override.

Этап 5 — wpick-tui:
24. В CLI добавь subcommands outputs, status. В set добавь --monitor.
25. app.rs: outputs: Vec<OutputInfo>, refresh_outputs() после connect + на 'r'.
26. ui.rs: footer помечает multi-monitor (например "2 monitors").
    TUI monitor selector — отложен в v0.3.

Обнови skills/wpick-daemon.md §renderer — новая секция "Multi-monitor
rules" с правилами 10-19.

Обнови skills/wpick-tui.md — новые команды в cli.rs.

Обнови docs/DAEMON.md §renderer.rs если реализация расходится со
спецификацией.

Добавь в ERRORS_TO_AVOID.md всё новое по мере встречи (E-36+).
E-30, E-31, E-32 уже описаны — по факту работы сверь и обнови.

Gate:
  cargo build --workspace --release → без ошибок
  Подключи второй монитор (или nested sway/Hyprland).
  wpick-daemon → оба монитора показывают видео
  wpick outputs → список мониторов
  wpick set <id2> --monitor HDMI-A-1 → только этот
  wpick set <id1> → все (когда config пуст)
  Отключить HDMI → renderer дропает; daemon жив.
  Подключить назад → renderer поднимается; wallpaper из config применяется.
```

---

## Block 22 — Pause manager

```
Контекст: wpick v0.2.0, Block 22 — pause manager.
Читай: docs/PROJECT.md, docs/DAEMON.md §pause.rs + §renderer.rs + §audio.rs,
skills/wpick-daemon.md, docs/CONFIG.md §pause, docs/PAUSE.md ЦЕЛИКОМ,
wpick-daemon/src/{renderer.rs,audio.rs,state.rs}.

Цель: daemon автоматически паузит рендер+аудио на fullscreen / battery /
lid, с manual override через IPC Pause/Resume.

Реализуй wpick-daemon/src/pause.rs:

1. pub async fn run(
     config: PauseConfig,
     paused_tx: watch::Sender<bool>,
     override_rx: watch::Receiver<Option<bool>>,
     shutdown_rx: broadcast::Receiver<()>,
   ) -> anyhow::Result<()>

2. Внутри: Sources { fullscreen: AtomicBool, on_battery, lid_closed }
   в Arc. tokio::spawn три sub-tasks (только при соответствующем cfg).

3. Hyprland task:
   - $HYPRLAND_INSTANCE_SIGNATURE отсутствует → log info, не стартовать (E-33).
   - Подключись к $XDG_RUNTIME_DIR/hypr/$SIG/.socket2.sock.
   - На hyprctl activewindow (через тот же IPC), заполни initial state.
   - Loop: read_line, match "fullscreen>>0"/"fullscreen>>1" → обнови
     src.fullscreen.
   - EOF/Err → sleep 5s, reconnect (E-35).

4. Battery task:
   - Найди /sys/class/power_supply/*/online через glob. Пусто → log info,
     не стартовать (E-34).
   - Poll каждые 2s. Content "0" → on_battery=true.

5. Lid task:
   - /proc/acpi/button/lid/*/state. Пусто → log info, не стартовать.
   - Poll каждые 2s. "closed" → lid_closed=true.

6. Aggregator task: select! на re-eval signal + override_rx changes +
   shutdown. На каждое событие: compute auto_paused, final_paused через
   override.unwrap_or(auto). Publish в paused_tx ТОЛЬКО при изменении
   (E-13).

7. pub fn current_reasons() -> Vec<String> — читает из
   OnceLock<ArcSwap<Vec<String>>>, обновляется aggregator-ом.

Интеграция:

8. renderer.rs: в render_loop, перед декодированием:
   if *paused_rx.borrow() { dispatch_pending; flush; sleep 100ms; continue; }

9. audio.rs: уже добавлено в Block 20 — last_pause_sent guard.

10. main.rs:
    - watch::channel для paused, pause_override.
    - tokio::spawn(pause::run(...)).
    - Передай paused_rx в renderer и audio.

11. ipc_server.rs Status теперь возвращает реальные данные:
    paused = *paused_rx.borrow(),
    reasons = pause::current_reasons().

Обнови skills/wpick-daemon.md — новая секция "pause.rs — rules".

Обнови docs/DAEMON.md §pause.rs если реализация расходится.

Добавь новые записи в ERRORS_TO_AVOID.md (E-33..E-35 уже описаны;
обнови по факту).

Gate:
  cargo build --workspace → без ошибок
  cargo run -p wpick-daemon + wpick set <id> + fullscreen YouTube
    → pidstat 1 показывает <0.5% CPU daemon
  Закрытие fullscreen → рендеринг возобновляется.
  Отключение AC при on_battery=true → паузится за ≤2s.
  wpick status → показывает paused=true, reasons=["fullscreen"].
  wpick pause → manual pause; wpick resume → возврат к auto.
```

---

## Block 23 — CLI completions + man pages

```
Контекст: wpick v0.2.0, Block 23 — shell completions + man pages.
Читай: wpick-tui/src/main.rs, skills/wpick-tui.md, COMPAT.md.

Цель: completions и man генерируются из clap defs; zero duplication.

1. wpick-tui/Cargo.toml: добавь
     clap_complete = "4"
     clap_mangen   = "0.2"

2. Раздели CLI определение: создай wpick-tui/src/cli.rs с struct Cli
   и enum Commands. main.rs импортирует из crate::cli. Это нужно чтобы
   (в будущем) build.rs мог include!-нуть определение.

3. Добавь в Commands:
     #[command(hide = true)] Completions { shell: clap_complete::Shell },
     #[command(hide = true)] Man,

4. Добавь subcommand `wpick-daemon man` → создай в wpick-daemon минимальный
   clap::Parser (сейчас там нет clap) с --version, --help, hidden `man`.

5. Обработчики:
     Commands::Completions { shell } => {
         let mut cmd = Cli::command();
         let name    = cmd.get_name().to_string();
         clap_complete::generate(shell, &mut cmd, name, &mut std::io::stdout());
     }
     Commands::Man => {
         clap_mangen::Man::new(Cli::command()).render(&mut std::io::stdout())?;
     }

6. Long-about для Cli: добавь DESCRIPTION, FILES
   (~/.config/wpick/config.toml, ~/.wpick.sock, ~/.cache/wpick),
   EXAMPLES, SEE ALSO wpick-daemon(1).

7. Тесты (опционально, но полезно):
   - test_completions_bash_non_empty
   - test_man_contains_TH_header

Обнови skills/wpick-tui.md:
- Новая секция "Shell completions & man pages".
- Правило: "Cli struct — single source of truth, не дублировать".

Обнови COMPAT.md — clap_complete="4", clap_mangen="0.2" в таблице
зависимостей wpick-tui.

Gate:
  wpick completions bash | head             → bash completion script
  wpick completions zsh  > /tmp/_wpick; source /tmp/_wpick; wpick <TAB>
  wpick man > /tmp/wpick.1; man /tmp/wpick.1 → открывается
  wpick --help                              → формат не сломан
  wpick-daemon man > /tmp/wpick-daemon.1    → открывается
```

---

## Block 24 — Systemd user service

```
Контекст: wpick v0.2.0, Block 24 — systemd user service.
Читай: wpick-daemon/src/main.rs, docs/PROJECT.md, docs/SYSTEMD.md.

Задача: готовый user unit с harderning, логирование через journald.

1. Создай dist/systemd/wpick-daemon.service — точное содержимое
   в docs/SYSTEMD.md §Unit file.

2. В wpick-daemon/src/main.rs: init_tracing должен детектить
   $JOURNAL_STREAM и переключать writer:

   let under_systemd = std::env::var_os("JOURNAL_STREAM").is_some();
   if under_systemd {
       tracing_subscriber::fmt()
           .with_writer(std::io::stderr)
           .with_ansi(false)
           .with_env_filter(filter)
           .init();
   } else {
       // существующий tracing_appender daily rotation
   }

3. SIGTERM handler: убедись что remove_file(socket) + exit(0)
   — уже должно быть; просто проверь.

Обнови skills/wpick-daemon.md — секция "Running under systemd" с
правилом про JOURNAL_STREAM.

Обнови docs/SYSTEMD.md если содержимое unit-файла или detection
расходится с реализацией.

Gate:
  cp dist/systemd/wpick-daemon.service ~/.config/systemd/user/
  systemctl --user daemon-reload
  systemctl --user start wpick-daemon
  systemctl --user status wpick-daemon     → active (running)
  journalctl --user -u wpick-daemon -n 50  → логи видны
  systemctl --user stop wpick-daemon       → выход <2s, код 0
```

---

## Block 25 — Multi-distro packaging

```
Контекст: wpick v0.2.0, Block 25 — multi-distro packaging.
Читай: текущий PKGBUILD, .github/workflows/release.yml,
dist/systemd/wpick-daemon.service, docs/INSTALL.md.

1. Обнови PKGBUILD — установка completions, man, systemd unit:
   В package():
     install -Dm755 target/release/wpick $pkgdir/usr/bin/wpick
     install -Dm755 target/release/wpick-daemon $pkgdir/usr/bin/wpick-daemon
     "$pkgdir/usr/bin/wpick" completions bash | install -Dm644 /dev/stdin \
         $pkgdir/usr/share/bash-completion/completions/wpick
     "$pkgdir/usr/bin/wpick" completions zsh  | install -Dm644 /dev/stdin \
         $pkgdir/usr/share/zsh/site-functions/_wpick
     "$pkgdir/usr/bin/wpick" completions fish | install -Dm644 /dev/stdin \
         $pkgdir/usr/share/fish/vendor_completions.d/wpick.fish
     "$pkgdir/usr/bin/wpick" man | install -Dm644 /dev/stdin \
         $pkgdir/usr/share/man/man1/wpick.1
     install -Dm644 dist/systemd/wpick-daemon.service \
         $pkgdir/usr/lib/systemd/user/wpick-daemon.service
   
   depends+=(ffmpeg wayland systemd)

2. Создай второй PKGBUILD aur/wpick-bin/PKGBUILD, который pull-ит
   release tarball из GitHub Release (binary package).

3. Создай flake.nix — см. docs/INSTALL.md §Nix и roadmap Block 25
   в предыдущем плане. Используй rustPlatform.buildRustPackage,
   installShellCompletion, installManPage.

4. nix flake lock → закоммить flake.lock.

5. Обнови .github/workflows/release.yml:
   - cargo test на PR.
   - В release job:
     - apt install ffmpeg (убедись что версия совместима с pin;
       если нет — docker archlinux:latest).
     - Артефакты: wpick-x86_64-linux.tar.gz + checksums.txt.
     - nix flake check step.
     - (опционально) AUR auto-push через
       KSXGitHub/github-actions-deploy-aur@v2 для wpick-bin.

Обнови docs/INSTALL.md — синхронизируй любые расхождения.

Обнови skills/wpick-index.md — добавь в конце секцию "Release process".

Обнови README.md — install section ссылка на docs/INSTALL.md, badges:
AUR version, Nix flake.

Gate:
  makepkg -si (в чистом docker:archlinux) → собирается, устанавливается.
  pacman -Ql wpick → completions/man/service в правильных путях.
  nix build . → успех.
  nix run . -- list → работает (если daemon запущен).
  Запушить тестовый tag v0.2.0-rc1 → workflow зелёный, Release создан.
```

---

## Block 26 — Release sync

```
Контекст: wpick v0.2.0, Block 26 — release sync. Финальный блок.
Читай: CHANGELOG.md, README.md, все docs/*.md, все skills/*.md,
COMPAT.md, ERRORS_TO_AVOID.md, Cargo.toml всех трёх crates,
PKGBUILD, flake.nix, dist/systemd/wpick-daemon.service.

1. Version bump. В wpick-core/Cargo.toml, wpick-daemon/Cargo.toml,
   wpick-tui/Cargo.toml, PKGBUILD, flake.nix:
     version = "0.2.0"

2. CHANGELOG.md [0.2.0] — финализируй дату и проверь что все
   реальные изменения отражены:
   - Added: extended config, multi-monitor, streaming audio,
     frame reuse, auto-pause, CLI completions + man, systemd unit,
     AUR + Nix, новые CLI команды.
   - Changed: Set gains monitor (breaking для direct IPC),
     audio no longer pre-loads, VideoDecoder signature.
   - Fixed: реальные фиксы из процесса.
   - Known limitations: Scene/Web → v0.3, TUI preview → v0.3,
     TUI monitor selector → v0.3, Flatpak → v0.3, hardware decode → v0.3.

3. README.md:
   - Status table обнови (v0.2 фичи → ✅).
   - Feature list обнови.
   - Badges: AUR version, Nix flake.
   - Install section ссылки на docs/INSTALL.md.

4. SEQUENCE.md: под v0.2 блоками — "released as v0.2.0" маркер.

5. COMPAT.md:
   - Новые деп: clap_complete, clap_mangen.
   - Verified combinations — обнови datestamp, добавь Nix row.

6. ERRORS_TO_AVOID.md:
   - Проверь что E-28..E-35 все на месте.
   - Отсортируй индекс по категориям (должно быть уже).
   - Добавь любые новые E-36+ полученные за v0.2.

7. Full regression:
     cargo test --workspace
     cargo clippy --workspace -- -D warnings
     cargo build --workspace --release
   
   Manual smoke:
     1. systemctl --user start wpick-daemon
     2. wpick list, set, volume, mute
     3. wpick set <id2> --monitor HDMI-A-1 (если есть два монитора)
     4. Fullscreen → daemon CPU падает
     5. wpick status → корректно
     6. htop RSS ≤ 100MB
     7. man wpick, wpick completions zsh
     8. systemctl --user status wpick-daemon → active

8. Tag:
     git add -A
     git commit -m "Release v0.2.0"
     git tag -a v0.2.0 -m "v0.2.0 — multi-monitor, auto-pause, streaming audio, distro packaging"
     git push origin main v0.2.0
   
   CI подхватит тег, release workflow создаст GitHub Release.

Gate:
  Tag v0.2.0 на GitHub существует, Release с артефактами создан.
  AUR: paru -Ss wpick показывает 0.2.0.
  Nix: nix run github:ederadar/wpick — работает с чистого клона.
  Журнал: journalctl --user -u wpick-daemon -g "0.2.0" показывает
    правильную version string.
```