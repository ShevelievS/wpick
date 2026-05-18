use chrono::Local;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, BorderType, Borders, Clear, List, ListItem, Paragraph};
use ratatui_image::{Resize, StatefulImage, protocol::StatefulProtocol};
use wpick_core::model::{WallpaperSource};

use crate::app::{App, AppMode, COLOR_PRESETS, FpHint, NavItem, Panel, PALETTE, SORT_OPTIONS, SortMode, TIMER_OPTIONS, fit_label, fp_is_system};

// ─── Helpers ──────────────────────────────────────────────────────────────────

/// Truncate `s` to at most `max_chars` Unicode scalar values, appending `…` if truncated.
fn ellipsize(s: &str, max_chars: usize) -> String {
    if max_chars == 0 { return String::new(); }
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= max_chars {
        s.to_owned()
    } else {
        chars[..max_chars - 1].iter().collect::<String>() + "…"
    }
}

// ─── Entry point ──────────────────────────────────────────────────────────────

pub fn render(frame: &mut Frame, app: &mut App) {
    let full = frame.area();

    if full.width < 60 || full.height < 16 {
        frame.render_widget(
            Paragraph::new("Terminal too small (min 60×16)"),
            full,
        );
        return;
    }

    // Windowed mode: render in a centered sub-area.
    let area = if app.config.tui.windowed {
        let w = (full.width  as f32 * 0.82) as u16;
        let h = (full.height as f32 * 0.82) as u16;
        let x = full.x + (full.width  - w) / 2;
        let y = full.y + (full.height - h) / 2;
        let r = Rect::new(x, y, w, h);
        frame.render_widget(Clear, r);
        r
    } else {
        full
    };

    let [header, main, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .areas(area);

    render_header(frame, app, header);
    render_footer(frame, app, footer);

    match app.mode {
        AppMode::FolderPicker => {
            render_main(frame, app, main);
            render_folder_picker(frame, app, area);
        }
        AppMode::Detail => {
            render_detail_fullscreen(frame, app, main);
        }
        AppMode::TimerDialog => {
            render_main(frame, app, main);
            render_timer_dialog(frame, app, area);
        }
        AppMode::PackNameInput => {
            render_main(frame, app, main);
            render_pack_name_dialog(frame, app, area);
        }
        AppMode::Browse => {
            render_main(frame, app, main);
        }
        AppMode::Settings => {
            render_main(frame, app, main);
            render_settings_dialog(frame, app, area);
        }
        AppMode::Help => {
            render_main(frame, app, main);
            render_help_overlay(frame, app, area);
        }
        AppMode::PackPicker => {
            render_main(frame, app, main);
            render_pack_picker(frame, app, area);
        }
        AppMode::SortDialog => {
            render_main(frame, app, main);
            render_sort_dialog(frame, app, area);
        }
    }

    if app.monitor_select_mode {
        render_monitor_overlay(frame, app, area);
    }
}

// ─── Main 3-panel layout ──────────────────────────────────────────────────────

fn render_main(frame: &mut Frame, app: &mut App, area: Rect) {
    let constraints = match (app.show_nav, app.show_preview) {
        (true,  true)  => vec![Constraint::Length(20), Constraint::Fill(1), Constraint::Percentage(38)],
        (true,  false) => vec![Constraint::Length(20), Constraint::Fill(1)],
        (false, true)  => vec![Constraint::Fill(1), Constraint::Percentage(38)],
        (false, false) => vec![Constraint::Fill(1)],
    };

    let areas: Vec<Rect> = Layout::horizontal(constraints).split(area).to_vec();

    match (app.show_nav, app.show_preview) {
        (true, true) => {
            render_nav(frame, app, areas[0]);
            render_list(frame, app, areas[1]);
            render_detail(frame, app, areas[2]);
        }
        (true, false) => {
            render_nav(frame, app, areas[0]);
            render_list(frame, app, areas[1]);
        }
        (false, true) => {
            render_list(frame, app, areas[0]);
            render_detail(frame, app, areas[1]);
        }
        (false, false) => {
            render_list(frame, app, areas[0]);
        }
    }
}

// ─── Header ───────────────────────────────────────────────────────────────────

fn render_header(frame: &mut Frame, app: &App, area: Rect) {
    let t = app.theme();

    let (conn_txt, conn_col) = if app.daemon_connected {
        ("\u{25cf} connected", t.text_dim)
    } else {
        ("\u{25cb} disconnected", Color::Red)
    };

    let status_span = match &app.status_message {
        Some(msg) => Span::styled(
            format!("  {msg}"),
            Style::default().fg(if app.status_is_error { Color::Red } else { t.color_hint }),
        ),
        None => Span::raw(""),
    };

    let now_playing = app.current_wallpaper_id.and_then(|id| {
        app.wallpapers.iter().find(|w| w.id == id).map(|w| {
            if w.title.chars().count() > 40 {
                format!("\u{25ba} {}…", w.title.chars().take(39).collect::<String>())
            } else {
                format!("\u{25ba} {}", w.title)
            }
        })
    });

    let left_spans = vec![
        Span::styled("wpick ", Style::default().add_modifier(Modifier::BOLD)),
        Span::styled(conn_txt, Style::default().fg(conn_col)),
        status_span,
    ];

    let pos = app.config.tui.now_playing_pos.as_str();

    match pos {
        "top-left" => {
            if let Some(np) = &now_playing {
                let mut all = vec![
                    Span::styled(np.as_str(), Style::default().fg(t.text_dim)),
                    Span::raw("  "),
                ];
                all.extend(left_spans);
                frame.render_widget(Paragraph::new(Line::from(all)), area);
            } else {
                frame.render_widget(Paragraph::new(Line::from(left_spans)), area);
            }
        }
        "top-center" => {
            let center_txt = now_playing.as_deref().unwrap_or("").to_owned();
            let center_len = (center_txt.chars().count() as u16).max(1);
            let [l, c, _] = Layout::horizontal([
                Constraint::Fill(1),
                Constraint::Length(center_len),
                Constraint::Fill(1),
            ]).areas(area);
            frame.render_widget(Paragraph::new(Line::from(left_spans)), l);
            frame.render_widget(Paragraph::new(Span::styled(center_txt, Style::default().fg(t.text_dim))), c);
        }
        "none" => {
            frame.render_widget(Paragraph::new(Line::from(left_spans)), area);
        }
        _ => { // "top-right" (default)
            let np_str = now_playing.as_deref().unwrap_or("").to_owned();
            let right_len = np_str.chars().count() as u16;
            if right_len > 0 {
                let [l, r] = Layout::horizontal([Constraint::Fill(1), Constraint::Length(right_len)]).areas(area);
                frame.render_widget(Paragraph::new(Line::from(left_spans)), l);
                frame.render_widget(Paragraph::new(Span::styled(np_str, Style::default().fg(t.text_dim))), r);
            } else {
                frame.render_widget(Paragraph::new(Line::from(left_spans)), area);
            }
        }
    }
}

// ─── Footer ───────────────────────────────────────────────────────────────────

fn render_footer(frame: &mut Frame, app: &App, area: Rect) {
    let t = app.theme();
    let hint = match &app.mode {
        AppMode::Settings    => "↑↓ nav  Space toggle  Esc close",
        AppMode::TimerDialog => "↑↓ select  Enter start  s stop  Esc cancel",
        AppMode::PackNameInput => "type name  Enter confirm  Esc cancel",
        AppMode::Help        => "Esc / ? close",
        AppMode::PackPicker  => "↑↓ move  Space toggle  Esc close",
        AppMode::SortDialog  => "↑↓ select  d asc/desc  Enter apply  Esc cancel",
        _ => match app.active_panel {
            Panel::Nav  => "→ focus list  n new pack  d del pack  [ hide nav  S settings",
            Panel::List => "Enter apply  / search  p★ fav  o sort  t timer  q quit  +/- vol  ? help  Alt+K kill",
        },
    };
    frame.render_widget(
        Paragraph::new(Span::styled(hint, Style::default().fg(t.color_hint))),
        area,
    );
}

// ─── Nav panel ────────────────────────────────────────────────────────────────

const NAV_STATS_H: u16 = 5; // 2 border + 3 content lines

fn render_nav(frame: &mut Frame, app: &App, area: Rect) {
    if area.height > NAV_STATS_H + 8 {
        let [nav_area, stats_area] = Layout::vertical([
            Constraint::Min(8),
            Constraint::Length(NAV_STATS_H),
        ]).areas(area);
        render_nav_list(frame, app, nav_area);
        render_nav_stats(frame, app, stats_area);
    } else {
        render_nav_list(frame, app, area);
    }
}

fn render_nav_list(frame: &mut Frame, app: &App, area: Rect) {
    let t = app.theme();
    let focused = app.active_panel == Panel::Nav;
    let border_style = if focused {
        Style::default().fg(t.border_active)
    } else {
        Style::default().fg(t.border_idle)
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(border_style)
        .title(Span::styled(" Library ", Style::default().fg(t.color_col_title).add_modifier(Modifier::BOLD)));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    let nav_items = app.nav_items();
    let fav_count  = app.wallpapers.iter().filter(|w| app.favorites.contains(&w.id)).count();
    let freq_count = app.frequent.len();
    let all_count  = app.wallpapers.len();
    let ws_count   = app.wallpapers.iter().filter(|w| w.source == WallpaperSource::Workshop).count();

    let mut items: Vec<ListItem> = Vec::new();
    let mut last_section = "";

    for (i, nav) in nav_items.iter().enumerate() {
        let section = match nav {
            NavItem::Favorites | NavItem::Frequent => "top",
            NavItem::Pack(_) => "packs",
            NavItem::Source(_) => "sources",
        };
        if section != last_section && !last_section.is_empty() {
            items.push(ListItem::new(Span::styled(
                "\u{2500}".repeat(inner.width.saturating_sub(2) as usize),
                Style::default().fg(t.border_idle),
            )));
        }
        last_section = section;

        let selected = i == app.nav_selected;
        let base_style = if selected && focused {
            Style::default().fg(t.sel_fg).bg(t.sel_bg).add_modifier(Modifier::BOLD)
        } else if selected {
            Style::default().fg(t.color_hint)
        } else {
            Style::default()
        };

        let (icon, label, count) = match nav {
            NavItem::Favorites  => ("\u{2605}", "Favorites".to_owned(), fav_count),
            NavItem::Frequent   => ("\u{21ba}", "Frequent".to_owned(), freq_count),
            NavItem::Pack(i)    => ("\u{25b8}", app.packs[*i].name.clone(), app.packs[*i].ids.len()),
            NavItem::Source(sf) => match sf {
                crate::app::SourceFilter::All          => ("\u{2261}", "All".to_owned(), all_count),
                crate::app::SourceFilter::Workshop     => ("\u{229e}", "Workshop".to_owned(), ws_count),
                crate::app::SourceFilter::Local(label) => {
                    let cnt = app.wallpapers.iter().filter(|w| {
                        matches!(&w.source, WallpaperSource::Local { label: l } if l == label)
                    }).count();
                    ("\u{25a1}", label.clone(), cnt)
                }
            },
        };

        let text      = format!("{icon} {label}");
        let count_str = format!("{count:>3}");
        let avail_w   = inner.width.saturating_sub(2) as usize;
        let pad       = avail_w.saturating_sub(text.chars().count() + count_str.len());
        let line      = format!("{text}{}{count_str}", " ".repeat(pad));

        items.push(ListItem::new(Span::styled(line, base_style)));
    }

    frame.render_widget(List::new(items), inner);
}

fn render_nav_stats(frame: &mut Frame, app: &App, area: Rect) {
    let t   = app.theme();
    let dim = Style::default().fg(t.text_dim);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(t.border_idle));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    // Timer
    let timer_line = if app.timer_active {
        let m = app.timer_remaining_secs / 60;
        let s = app.timer_remaining_secs % 60;
        Line::from(vec![
            Span::styled(" \u{23f1} ", dim),
            Span::styled(format!("{m}:{s:02}"), Style::default().fg(t.color_hint)),
        ])
    } else {
        Line::from(Span::styled(" \u{23f1}  \u{2014}", dim))
    };

    // Volume
    let vol_line = if app.config.general.muted {
        Line::from(Span::styled(" \u{25a0} MUTED", Style::default().fg(t.color_fav)))
    } else {
        let pct = (app.config.general.volume * 100.0).round() as u32;
        match app.config.tui.vol_style.as_str() {
            "number" => Line::from(vec![
                Span::styled(" \u{266a} ", dim),
                Span::styled(format!("{pct}%"), Style::default().fg(t.vol_bar)),
            ]),
            "bar" => {
                // Wider bar (10 segments)
                let filled = (app.config.general.volume * 10.0).round() as usize;
                let empty  = 10usize.saturating_sub(filled);
                Line::from(vec![
                    Span::styled(" \u{266a} ", dim),
                    Span::styled("\u{25b0}".repeat(filled), Style::default().fg(t.vol_bar)),
                    Span::styled("\u{25b1}".repeat(empty),  dim),
                ])
            }
            _ => {
                // "slim" (default): 6 segments + percentage
                let filled = (app.config.general.volume * 6.0).round() as usize;
                let empty  = 6usize.saturating_sub(filled);
                Line::from(vec![
                    Span::styled(" \u{266a} ", dim),
                    Span::styled("\u{25b0}".repeat(filled), Style::default().fg(t.vol_bar)),
                    Span::styled("\u{25b1}".repeat(empty),  dim),
                    Span::styled(format!(" {pct}%"), dim),
                ])
            }
        }
    };

    // Clock
    let clock = Local::now().format(" %H:%M").to_string();
    let clock_line = Line::from(Span::styled(clock, dim));

    frame.render_widget(
        Paragraph::new(Text::from(vec![timer_line, vol_line, clock_line])),
        inner,
    );
}

// ─── List panel ───────────────────────────────────────────────────────────────

fn render_list(frame: &mut Frame, app: &mut App, area: Rect) {
    let t = app.theme();
    let focused = app.active_panel == Panel::List;
    let border_style = if focused {
        Style::default().fg(t.border_active)
    } else {
        Style::default().fg(t.border_idle)
    };

    // Search bar above the block when active.
    let (list_area, maybe_search) = if app.search_active {
        let [search, list] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Min(0),
        ]).areas(area);
        (list, Some(search))
    } else {
        (area, None)
    };

    if let Some(sa) = maybe_search {
        frame.render_widget(
            Paragraph::new(Span::styled(
                format!("/ {}\u{2588}", app.search_query),
                Style::default().fg(t.color_fav),
            )),
            sa,
        );
    }

    // Title from current nav item.
    let nav_label = match app.current_nav() {
        NavItem::Favorites  => "Favorites".to_owned(),
        NavItem::Frequent   => "Frequent".to_owned(),
        NavItem::Pack(i)    => app.packs.get(i).map(|p| p.name.clone()).unwrap_or_default(),
        NavItem::Source(sf) => match sf {
            crate::app::SourceFilter::All          => "All".to_owned(),
            crate::app::SourceFilter::Workshop     => "Workshop".to_owned(),
            crate::app::SourceFilter::Local(label) => label,
        },
    };
    let view   = app.current_view();
    let total  = app.wallpapers.len();
    let shown  = view.len();
    let dir    = if app.sort_desc { "↓" } else { "↑" };
    let sort_tag = match app.sort_mode {
        SortMode::Default    => String::new(),
        SortMode::Name       => format!(" ↕name{dir}"),
        SortMode::Size       => format!(" ↕size{dir}"),
        SortMode::Resolution => format!(" ↕res{dir}"),
    };
    let title  = if app.search_query.is_empty() {
        format!(" {nav_label} ({shown}/{total}){sort_tag} ")
    } else {
        format!(" {nav_label}: \"{}\" ({shown}){sort_tag} ", app.search_query)
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(border_style)
        .title(Span::styled(title, Style::default().fg(t.color_col_title).add_modifier(Modifier::BOLD)));

    let inner_full = block.inner(list_area);
    frame.render_widget(&block, list_area);

    // Pack subtitle bar: show pack wallpaper count + hint when viewing a pack.
    let (inner, pack_hint_area) = if matches!(app.current_nav(), NavItem::Pack(_)) {
        let [hint, rest] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Min(0),
        ]).areas(inner_full);
        (rest, Some(hint))
    } else {
        (inner_full, None)
    };

    if let Some(hint_area) = pack_hint_area {
        let pack_idx = match app.current_nav() { NavItem::Pack(i) => i, _ => 0 };
        let pack = app.packs.get(pack_idx);
        let ids_count = pack.map(|p| p.ids.len()).unwrap_or(0);
        let hint_txt = if ids_count == 0 {
            "  \u{1f4e6} Empty pack  \u{2014}  press [a] to add current wallpaper".to_string()
        } else {
            format!("  \u{1f4e6} {ids_count} wallpaper{}  \u{2014}  [a] add / remove", if ids_count == 1 { "" } else { "s" })
        };
        frame.render_widget(
            Paragraph::new(Span::styled(hint_txt, Style::default().fg(t.color_hint))),
            hint_area,
        );
    }

    if app.loading {
        frame.render_widget(
            Paragraph::new(app.status_message.clone().unwrap_or_else(|| "Loading…".into()))
                .block(Block::default()),
            inner,
        );
        return;
    }

    if view.is_empty() {
        let t = app.theme();
        let dim = Style::default().fg(t.text_dim);
        let msg: ratatui::text::Text = match app.current_nav() {
            NavItem::Favorites => ratatui::text::Text::from(vec![
                Line::from(Span::styled("No favorites yet", dim)),
                Line::from(Span::styled("Press [p] on a wallpaper to add it", dim)),
            ]),
            NavItem::Frequent => ratatui::text::Text::from(
                Line::from(Span::styled("No wallpapers played yet", dim))
            ),
            _ => {
                if app.wallpapers.is_empty() {
                    ratatui::text::Text::raw("No wallpapers. Press 'r' to scan.")
                } else {
                    ratatui::text::Text::raw("No matches.")
                }
            }
        };
        frame.render_widget(Paragraph::new(msg), inner);
        return;
    }

    let idx = app.selected.min(view.len().saturating_sub(1));

    let items: Vec<ListItem> = view.iter().enumerate().map(|(i, w)| {
        let is_selected = i == idx;
        let is_playing  = Some(w.id) == app.current_wallpaper_id;
        let is_fav      = app.favorites.contains(&w.id);

        let fav_icon  = if is_fav       { "\u{2605}" } else { " " };
        let play_icon = if is_playing   { "\u{25cf}" } else { "\u{25b6}" };
        let audio_ico = if w.has_audio  { " \u{266a}" } else { "" };

        let style = if is_selected && focused {
            Style::default().fg(t.sel_fg).bg(t.sel_bg).add_modifier(Modifier::BOLD)
        } else if is_selected {
            Style::default().fg(t.sel_fg)
        } else if is_playing {
            Style::default().fg(t.color_playing)
        } else if is_fav {
            Style::default().fg(t.color_fav)
        } else {
            Style::default()
        };

        // Reserve: 1 fav + 1 space + 1 play + 1 space + audio_ico (0 or 2) + some padding.
        let prefix_len = 4 + audio_ico.chars().count();
        let avail = (inner.width as usize).saturating_sub(prefix_len);
        let title = ellipsize(&w.title, avail);
        let label = format!("{fav_icon} {play_icon} {title}{audio_ico}");
        ListItem::new(Span::styled(label, style))
    }).collect();

    let mut list_state = app.list_state;
    frame.render_stateful_widget(
        List::new(items).highlight_symbol("").highlight_style(Style::default()),
        inner,
        &mut list_state,
    );
    app.list_state = list_state;
}

// ─── Detail / Preview panel ───────────────────────────────────────────────────

fn render_detail(frame: &mut Frame, app: &mut App, area: Rect) {
    let t = app.theme();
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(t.border_idle));
    let inner = block.inner(area);
    frame.render_widget(&block, area);

    // Correct preview height for terminal cell aspect ratio.
    // Cells are typically taller than wide (e.g. 10×20px), so a naive 9/16
    // character-height produces a portrait-looking image. We multiply by
    // cell_w/cell_h so the rendered image appears as true 16:9 on screen.
    let (cell_w, cell_h) = app.picker.as_ref()
        .map(|p| { let fs = p.font_size(); (fs.width as f32, fs.height as f32) })
        .unwrap_or((10.0, 20.0));
    let preview_h = ((inner.width as f32) * (9.0 / 16.0) * (cell_w / cell_h)) as u16;
    let preview_h = preview_h
        .min((inner.height as f32 * 0.55) as u16)
        .min(inner.height.saturating_sub(8))
        .max(4);

    let [preview_area, sep_area, info_area] = Layout::vertical([
        Constraint::Length(preview_h),
        Constraint::Length(1),
        Constraint::Fill(1),
    ]).areas(inner);

    // Render image preview.
    if let Some(proto) = app.preview.as_mut() {
        render_preview_proto(frame, proto, preview_area);
    } else {
        frame.render_widget(
            Paragraph::new(Span::styled(
                "\n  No preview",
                Style::default().fg(t.text_dim),
            )),
            preview_area,
        );
    }

    // Separator between preview and info.
    frame.render_widget(
        Paragraph::new(Span::styled(
            "\u{2500}".repeat(inner.width as usize),
            Style::default().fg(t.border_idle),
        )),
        sep_area,
    );

    // Details below the preview.
    let info_lines = build_detail_lines(app);
    frame.render_widget(Paragraph::new(Text::from(info_lines)), info_area);
}

fn build_detail_lines(app: &App) -> Vec<Line<'static>> {
    let t   = app.theme();
    let dim = Style::default().fg(t.text_dim);
    let sep = Style::default().fg(t.border_idle);
    let mut lines: Vec<Line<'static>> = Vec::new();

    let view = app.current_view();
    let Some(w) = view.get(app.selected) else {
        return vec![Line::from(Span::styled(" Nothing selected", dim))];
    };

    let fav_str   = if app.favorites.contains(&w.id) { " \u{2605}" } else { "" };
    let audio_str = if w.has_audio { " \u{266a}" } else { "" };
    let size_mb   = w.file_size_bytes as f64 / 1_048_576.0;
    let res_str   = if w.width > 0 { format!("  {}×{}", w.width, w.height) } else { String::new() };
    let src_str   = match &w.source {
        WallpaperSource::Workshop        => "Workshop".to_owned(),
        WallpaperSource::Local { label } => label.clone(),
    };

    // Title
    lines.push(Line::from(Span::styled(
        format!(" {}{fav_str}", w.title),
        Style::default().fg(Color::White).add_modifier(Modifier::BOLD),
    )));

    // Type + audio + size + resolution on one line
    lines.push(Line::from(Span::styled(
        format!(" video{audio_str}  {size_mb:.1} MB{res_str}"),
        dim,
    )));

    // Source + fit
    lines.push(Line::from(Span::styled(
        format!(" {}  \u{b7}  {}", src_str, fit_label(app.current_fit)),
        dim,
    )));

    // File path — last 2 components, truncated
    let path_str = {
        let p      = std::path::Path::new(&w.file_path);
        let file   = p.file_name().and_then(|f| f.to_str()).unwrap_or("");
        let parent = p.parent().and_then(|d| d.file_name()).and_then(|f| f.to_str()).unwrap_or("");
        if parent.is_empty() { file.to_owned() } else { format!("\u{2026}/{parent}/{file}") }
    };
    let path_str = if path_str.chars().count() > 30 {
        let suffix: String = path_str.char_indices()
            .rev()
            .take(30)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .map(|(_, c)| c)
            .collect();
        format!("\u{2026}{}", suffix)
    } else { path_str };
    lines.push(Line::from(Span::styled(format!(" {path_str}"), dim)));

    // Separator
    lines.push(Line::from(Span::styled("\u{2500}".repeat(30), sep)));

    if app.timer_active {
        // Header: countdown + interval hint
        let mins = app.timer_remaining_secs / 60;
        let secs = app.timer_remaining_secs % 60;
        lines.push(Line::from(vec![
            Span::styled(" \u{21ba} ", Style::default().fg(t.color_hint)),
            Span::styled(
                format!("{mins}:{secs:02}"),
                Style::default().fg(t.color_hint),
            ),
        ]));
        // Mini list: up to 4 wallpapers in rotation
        for &id in app.timer_ids.iter().take(4) {
            let title = app.wallpapers.iter()
                .find(|w| w.id == id)
                .map(|w| w.title.as_str())
                .unwrap_or("—");
            let truncated = if title.chars().count() > 28 {
                format!("{}…", title.chars().take(27).collect::<String>())
            } else {
                title.to_owned()
            };
            let is_playing = app.current_wallpaper_id == Some(id);
            let (icon, style) = if is_playing {
                ("\u{25cf} ", Style::default().fg(t.color_playing))
            } else {
                ("\u{25cb} ", dim)
            };
            lines.push(Line::from(vec![
                Span::styled(format!("  {icon}"), style),
                Span::styled(truncated, style),
            ]));
        }
        if app.timer_ids.len() > 4 {
            lines.push(Line::from(Span::styled(
                format!("  +{} more", app.timer_ids.len() - 4),
                dim,
            )));
        }
    } else {
        lines.push(Line::from(Span::styled(" \u{21ba}  \u{2014}", dim)));
    }

    lines
}

// ─── Help overlay ─────────────────────────────────────────────────────────────

fn render_help_overlay(frame: &mut Frame, app: &App, area: Rect) {
    let t = app.theme();
    let w = 52u16;
    let h = 28u16;
    let x = area.x + area.width.saturating_sub(w) / 2;
    let y = area.y + area.height.saturating_sub(h) / 2;
    let dialog = Rect::new(x, y, w.min(area.width), h.min(area.height));

    frame.render_widget(Clear, dialog);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(t.border_overlay))
        .title(Span::styled(" ? Keybindings  (TUI only) ", Style::default().add_modifier(Modifier::BOLD)));
    let inner = block.inner(dialog);
    frame.render_widget(block, dialog);

    let sec_col  = t.color_col_title;
    let key_col  = t.color_hint;
    let desc_col = t.text_dim;

    let section = move |s: &'static str| -> Line<'static> {
        Line::from(Span::styled(s, Style::default().fg(sec_col).add_modifier(Modifier::BOLD)))
    };
    let key = move |k: &'static str, desc: &'static str| -> Line<'static> {
        Line::from(vec![
            Span::styled(format!("  {k:<12}"), Style::default().fg(key_col)),
            Span::styled(desc, Style::default().fg(desc_col)),
        ])
    };

    let lines = vec![
        section("Navigation"),
        key("↑↓ / jk",      "move selection"),
        key("←/h  →/l",     "switch panels"),
        key("Tab",           "switch panels"),
        key("/",             "search"),
        section("Wallpaper"),
        key("Enter",         "apply wallpaper"),
        key("p",             "toggle favorite ★"),
        key("a",             "add to pack"),
        key("i",             "detail fullscreen"),
        key("f",             "cycle fit mode"),
        section("Timer"),
        key("t",             "open timer dialog"),
        key("o",             "sort order"),
        section("Audio"),
        key("+/-",           "volume up/down"),
        key("m",             "mute/unmute"),
        section("Panels"),
        key("[",             "toggle nav panel"),
        key("]",             "toggle preview"),
        section("Other"),
        key("s",             "folders / extra dirs"),
        key("n",             "new pack"),
        key("d / Del",       "delete pack"),
        key("M",             "select monitor"),
        key("r",             "refresh / scan"),
        key("S",             "settings"),
        key("?",             "this help"),
        key("q",             "quit (daemon runs)"),
        key("Alt+K",         "quit & kill daemon"),
    ];

    frame.render_widget(Paragraph::new(Text::from(lines)), inner);
}

fn render_preview_proto(frame: &mut Frame, proto: &mut StatefulProtocol, area: Rect) {
    let img = StatefulImage::new().resize(Resize::Fit(None));
    frame.render_stateful_widget(img, area, proto);
}

// ─── Detail fullscreen (i key) ────────────────────────────────────────────────

fn render_detail_fullscreen(frame: &mut Frame, app: &mut App, area: Rect) {
    let t = app.theme();

    // Centered card: 82% width, 88% height
    let card_w = (area.width as f32 * 0.82) as u16;
    let card_h = (area.height as f32 * 0.88) as u16;
    let card_x = area.x + (area.width.saturating_sub(card_w)) / 2;
    let card_y = area.y + (area.height.saturating_sub(card_h)) / 2;
    let card   = Rect::new(card_x, card_y, card_w, card_h);

    frame.render_widget(Clear, card);

    // Collect wallpaper data before any mutable borrows.
    struct WInfo {
        title:   String,
        id:      u64,
        is_fav:  bool,
        audio:   bool,
        size_mb: f64,
        src_str: String,
        res_str: String,
    }
    let w_info: Option<WInfo> = {
        let view = app.current_view();
        view.get(app.selected).map(|w| WInfo {
            title:   w.title.clone(),
            id:      w.id,
            is_fav:  app.favorites.contains(&w.id),
            audio:   w.has_audio,
            size_mb: w.file_size_bytes as f64 / 1_048_576.0,
            src_str: match &w.source {
                wpick_core::model::WallpaperSource::Workshop        => "Steam Workshop".to_owned(),
                wpick_core::model::WallpaperSource::Local { label } => format!("Local: {label}"),
            },
            res_str: if w.width > 0 { format!("{}×{}", w.width, w.height) } else { "unknown".to_owned() },
        })
    };
    let title_str = w_info.as_ref().map(|w| w.title.clone()).unwrap_or_default();

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(t.border_overlay))
        .title(Span::styled(
            format!(" {} ", title_str),
            Style::default().fg(t.color_col_title).add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(card);
    frame.render_widget(block, card);

    // Preview takes top 62%, info bottom 38%; floor at 4 to avoid StatefulImage panic.
    let preview_h = (((inner.height as f32) * 0.62) as u16).max(4);
    let [preview_area, sep_area, info_area] = Layout::vertical([
        Constraint::Length(preview_h),
        Constraint::Length(1),
        Constraint::Fill(1),
    ]).areas(inner);

    // Render preview
    if let Some(proto) = app.preview.as_mut() {
        render_preview_proto(frame, proto, preview_area);
    } else {
        frame.render_widget(
            Paragraph::new(Span::styled(" No preview", Style::default().fg(t.text_dim))),
            preview_area,
        );
    }

    // Separator
    frame.render_widget(
        Paragraph::new(Span::styled(
            "\u{2500}".repeat(inner.width as usize),
            Style::default().fg(t.border_idle),
        )),
        sep_area,
    );

    // Info section — rich details
    if let Some(w) = w_info {
        let fav_str   = if w.is_fav { " \u{2605} Favorite" } else { "" };
        let audio_str = if w.audio { "yes" } else { "no" };

        let [left_col, right_col] = Layout::horizontal([
            Constraint::Fill(1),
            Constraint::Fill(1),
        ]).areas(info_area);

        let dim = Style::default().fg(t.text_dim);
        let val = Style::default().fg(t.color_col_title);

        let left_lines = vec![
            Line::from(vec![Span::styled(" Source   ", dim), Span::styled(w.src_str, val)]),
            Line::from(vec![Span::styled(" Size     ", dim), Span::styled(format!("{:.1} MB", w.size_mb), val)]),
            Line::from(vec![Span::styled(" Res      ", dim), Span::styled(w.res_str, val)]),
            Line::from(vec![Span::styled(" Audio    ", dim), Span::styled(audio_str, val)]),
            Line::from(vec![Span::styled(" Fit      ", dim), Span::styled(fit_label(app.current_fit), val)]),
        ];

        let uptime   = app.start_time.elapsed().as_secs();
        let (uh, um) = (uptime / 3600, (uptime % 3600) / 60);
        let uptime_s = if uh > 0 { format!("{}h {}m", uh, um) } else { format!("{}m", um) };

        let playing_s = app.wallpaper_applied_at.map(|t| {
            let e = t.elapsed().as_secs();
            let (eh, em) = (e / 3600, (e % 3600) / 60);
            if eh > 0 { format!("{}h {}m", eh, em) } else { format!("{}m", em) }
        });

        let clock = Local::now().format("%H:%M:%S").to_string();

        let mut right_lines = vec![
            Line::from(Span::styled(format!(" #{}", w.id), dim)),
            Line::from(Span::styled(fav_str, Style::default().fg(t.color_fav))),
            Line::from(""),
            Line::from(Span::styled(format!(" Session  {uptime_s}"), dim)),
        ];
        if let Some(ps) = playing_s {
            right_lines.push(Line::from(Span::styled(format!(" Playing  {ps}"), dim)));
        }
        right_lines.push(Line::from(Span::styled(format!(" {clock}"), dim)));
        right_lines.push(Line::from(""));
        right_lines.push(Line::from(Span::styled(" [i] close  [p] fav  [Enter] apply", dim)));

        frame.render_widget(Paragraph::new(Text::from(left_lines)), left_col);
        frame.render_widget(Paragraph::new(Text::from(right_lines)), right_col);
    }
}

// ─── Timer dialog ─────────────────────────────────────────────────────────────

fn render_timer_dialog(frame: &mut Frame, app: &App, area: Rect) {
    let t = app.theme();
    let w = 36u16;
    let h = (TIMER_OPTIONS.len() as u16) + 6;
    let x = area.x + area.width.saturating_sub(w) / 2;
    let y = area.y + area.height.saturating_sub(h) / 2;
    let dialog = Rect::new(x, y, w.min(area.width), h.min(area.height));

    frame.render_widget(Clear, dialog);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(t.border_overlay))
        .title(Span::styled(" \u{23f1} Timer ", Style::default().add_modifier(Modifier::BOLD)));
    let inner = block.inner(dialog);
    frame.render_widget(block, dialog);

    let mut lines = vec![
        Line::from(Span::styled(" Select interval:", Style::default().fg(t.text_dim))),
        Line::from(""),
    ];
    for (i, (label, _)) in TIMER_OPTIONS.iter().enumerate() {
        let selected = i == app.timer_dialog_idx;
        let style = if selected {
            Style::default().fg(t.sel_fg).bg(t.sel_bg).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(t.text_dim)
        };
        lines.push(Line::from(Span::styled(
            format!("  {}  {label}", if selected { "\u{25b6}" } else { " " }),
            style,
        )));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        if app.timer_active { "  [s] stop  [Enter] restart  [Esc] cancel" }
        else                { "  [Enter] start  [Esc] cancel" },
        Style::default().fg(t.color_hint),
    )));

    frame.render_widget(Paragraph::new(Text::from(lines)), inner);
}

// ─── Settings dialog ──────────────────────────────────────────────────────────

fn active_preset_idx(app: &App) -> Option<usize> {
    let c = &app.config.tui.colors;
    let current = [c.border_active, c.border_idle, c.border_overlay, c.sel_bg, c.sel_fg,
                   c.color_hint, c.color_playing, c.color_fav, c.col_title, c.text_dim, c.vol_bar];
    COLOR_PRESETS.iter().position(|(_, p)| *p == current)
}

fn render_settings_dialog(frame: &mut Frame, app: &App, area: Rect) {
    let t = app.theme();
    let dim       = Style::default().fg(t.text_dim);
    let hint      = Style::default().fg(t.color_hint);
    let sel_bg    = Style::default().fg(t.sel_fg).bg(t.sel_bg).add_modifier(Modifier::BOLD);
    let hdr_style = Style::default().fg(t.color_col_title).add_modifier(Modifier::BOLD);
    let sep       = Style::default().fg(t.border_idle);

    if app.settings_level == 0 {
        // ── Compact category list ─────────────────────────────────────────────
        let cats = App::SETTINGS_CATS;
        let w = 32u16;
        let h = (cats.len() as u16 + 4).min(area.height.saturating_sub(2));
        let x = area.x + area.width.saturating_sub(w) / 2;
        let y = area.y + area.height.saturating_sub(h) / 2;
        let dialog = Rect::new(x, y, w.min(area.width), h);

        frame.render_widget(Clear, dialog);
        let block = Block::default()
            .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(t.border_overlay))
            .title(Span::styled(" \u{2699} Settings ", Style::default().add_modifier(Modifier::BOLD)));
        let inner = block.inner(dialog);
        frame.render_widget(block, dialog);

        let mut lines: Vec<Line> = Vec::new();
        for (i, &cat) in cats.iter().enumerate() {
            let selected = i == app.settings_cat;
            let (arrow, row_style) = if selected {
                ("\u{25b6}", Style::default().fg(t.sel_fg).bg(t.sel_bg).add_modifier(Modifier::BOLD))
            } else {
                (" ", dim)
            };
            lines.push(Line::from(Span::styled(format!(" {arrow} {cat}"), row_style)));
        }
        lines.push(Line::from(Span::styled("\u{2500}".repeat(inner.width as usize), sep)));
        lines.push(Line::from(Span::styled(" Enter/\u{2192} open  Esc close", hint)));
        frame.render_widget(Paragraph::new(Text::from(lines)), inner);

    } else {
        // ── Inside a category — compact, no wasted space ──────────────────────
        let cat_name = App::SETTINGS_CATS[app.settings_cat];
        let items    = App::settings_cat_items(app.settings_cat);

        // Compute compact height based on actual content
        let content_lines = if app.settings_cat == 1 {
            COLOR_PRESETS.len()
        } else {
            items.len()
        };
        let w = if app.settings_cat == 1 { 46u16 } else { 52u16 };
        let h = (content_lines as u16 + 4).min(area.height.saturating_sub(2));
        let x = area.x + area.width.saturating_sub(w) / 2;
        let y = area.y + area.height.saturating_sub(h) / 2;
        let dialog = Rect::new(x, y, w.min(area.width), h);

        frame.render_widget(Clear, dialog);
        let block = Block::default()
            .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(t.border_overlay))
            .title(Span::styled(
                format!(" \u{2699} {cat_name} "),
                Style::default().add_modifier(Modifier::BOLD),
            ));
        let inner = block.inner(dialog);
        frame.render_widget(block, dialog);

        let mut lines: Vec<Line> = Vec::new();

        if app.settings_cat == 1 {
            // ── Visual: 13 presets with swatches ─────────────────────────────
            let active_idx = active_preset_idx(app);
            for (i, &(name, indices)) in COLOR_PRESETS.iter().enumerate() {
                let selected  = i == app.settings_selected;
                let is_active = active_idx == Some(i);
                let bullet    = if is_active { "\u{25cf}" } else { "\u{25cb}" };

                let swatch = |slot: usize| -> Color {
                    let (_, (r, g, b)) = PALETTE[indices[slot].min(PALETTE.len() - 1)];
                    Color::Rgb(r, g, b)
                };
                let row_style = if selected {
                    Style::default().fg(t.sel_fg).bg(t.sel_bg).add_modifier(Modifier::BOLD)
                } else { dim };
                let bg = if selected { t.sel_bg } else { Color::Reset };

                lines.push(Line::from(vec![
                    Span::styled(format!(" {bullet} {name:<14}"), row_style),
                    Span::styled("\u{25a0} ", Style::default().fg(swatch(0)).bg(bg)),
                    Span::styled("\u{25a0} ", Style::default().fg(swatch(3)).bg(bg)),
                    Span::styled("\u{25a0} ", Style::default().fg(swatch(5)).bg(bg)),
                    Span::styled("\u{25a0} ", Style::default().fg(swatch(6)).bg(bg)),
                    Span::styled("\u{25a0}", Style::default().fg(swatch(7)).bg(bg)),
                    Span::styled("", row_style),
                ]));
            }
        } else {
            // ── TUI (0) and Keys (2) ──────────────────────────────────────────
            for (i, &(label, is_header)) in items.iter().enumerate() {
                if is_header {
                    lines.push(Line::from(Span::styled(format!(" {label}"), hdr_style)));
                    continue;
                }
                let selected = i == app.settings_selected;
                let prefix = if app.settings_cat == 0 {
                    match i {
                        0 => if app.show_nav     { "\u{25cf} " } else { "\u{25cb} " },
                        1 => if app.show_preview { "\u{25cf} " } else { "\u{25cb} " },
                        _ => "  ",
                    }
                } else { "  " };
                let suffix: String = if app.settings_cat == 0 {
                    match i {
                        2 => format!("  [{}]", app.config.tui.now_playing_pos),
                        3 => format!("  [{}]", app.config.tui.vol_style),
                        _ => String::new(),
                    }
                } else { String::new() };
                let (row_style, tag) = if selected && app.settings_cat == 0 {
                    (sel_bg, "  Space")
                } else if selected {
                    (sel_bg, "")
                } else {
                    (dim, "")
                };
                lines.push(Line::from(Span::styled(
                    format!(" {prefix}{label}{suffix}{tag}"),
                    row_style,
                )));
            }
        }

        lines.push(Line::from(Span::styled("\u{2500}".repeat(inner.width as usize), sep)));
        let action_hint = match app.settings_cat {
            0 => " \u{2191}\u{2193} move  Space toggle  \u{2190} back  Esc close",
            1 => " \u{2191}\u{2193} move  Enter apply  \u{2190} back  Esc close",
            _ => " \u{2191}\u{2193} scroll  \u{2190} back  Esc close",
        };
        lines.push(Line::from(Span::styled(action_hint, hint)));

        frame.render_widget(Paragraph::new(Text::from(lines)), inner);
    }
}

// ─── Pack name dialog ─────────────────────────────────────────────────────────

fn render_pack_name_dialog(frame: &mut Frame, app: &App, area: Rect) {
    let t  = app.theme();
    let w  = 40u16;
    let h  = 5u16;
    let x  = area.x + area.width.saturating_sub(w) / 2;
    let y  = area.y + area.height.saturating_sub(h) / 2;
    let dialog = Rect::new(x, y, w.min(area.width), h.min(area.height));

    frame.render_widget(Clear, dialog);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(t.border_overlay))
        .title(Span::styled(" New Pack ", Style::default().add_modifier(Modifier::BOLD)));
    let inner = block.inner(dialog);
    frame.render_widget(block, dialog);

    let t = app.theme();
    let lines = vec![
        Line::from(""),
        Line::from(Span::styled(
            format!(" {}\u{2588}", app.pack_name_buf),
            Style::default().fg(t.color_col_title),
        )),
        Line::from(Span::styled("  [Enter] create  [Esc] cancel", Style::default().fg(t.color_hint))),
    ];
    frame.render_widget(Paragraph::new(Text::from(lines)), inner);
}

// ─── Monitor overlay ──────────────────────────────────────────────────────────

fn render_monitor_overlay(frame: &mut Frame, app: &App, area: Rect) {
    let t = app.theme();
    let w = 36u16;
    let h = (app.monitors.len() as u16 + 4).min(area.height);
    let x = area.x + area.width.saturating_sub(w) / 2;
    let y = area.y + area.height.saturating_sub(h) / 2;
    let dialog = Rect::new(x, y, w.min(area.width), h);

    frame.render_widget(Clear, dialog);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(t.border_overlay))
        .title(Span::styled(" Select Monitor ", Style::default().add_modifier(Modifier::BOLD)));
    let inner = block.inner(dialog);
    frame.render_widget(block, dialog);

    let sel_style  = Style::default().fg(t.sel_fg).bg(t.sel_bg).add_modifier(Modifier::BOLD);
    let idle_style = Style::default().fg(t.text_dim);
    let mut items: Vec<ListItem> = vec![ListItem::new(Span::styled(
        if app.monitor_selected == 0 { "\u{25b6} All monitors" } else { "  All monitors" },
        if app.monitor_selected == 0 { sel_style } else { idle_style },
    ))];
    for (i, (name, w, h)) in app.monitors.iter().enumerate() {
        let sel = i + 1 == app.monitor_selected;
        let label = if *w > 0 { format!("{name} ({w}x{h})") } else { name.clone() };
        items.push(ListItem::new(Span::styled(
            if sel { format!("\u{25b6} {label}") } else { format!("  {label}") },
            if sel { sel_style } else { idle_style },
        )));
    }
    items.push(ListItem::new(Span::styled(
        "  [Enter] apply  [Esc] cancel",
        Style::default().fg(t.color_hint),
    )));
    frame.render_widget(List::new(items), inner);
}

// ─── Folder picker ────────────────────────────────────────────────────────────

fn render_folder_picker(frame: &mut Frame, app: &App, area: Rect) {
    let t = app.theme();
    let w = (area.width * 2 / 3).max(50).min(area.width);
    let h = (area.height * 3 / 4).max(10).min(area.height);
    let x = area.x + (area.width - w) / 2;
    let y = area.y + (area.height - h) / 2;
    let dialog = Rect::new(x, y, w, h);

    frame.render_widget(Clear, dialog);

    let is_added = app.config.paths.extra_dirs
        .contains(&app.fp_path.to_string_lossy().into_owned());

    let title = if is_added {
        Span::styled(" Folder Picker [added] ", Style::default().fg(t.color_playing).add_modifier(Modifier::BOLD))
    } else {
        Span::styled(" Folder Picker ", Style::default().add_modifier(Modifier::BOLD))
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(t.border_overlay))
        .title(title);
    let inner = block.inner(dialog);
    frame.render_widget(block, dialog);

    let [path_row, list_area, help_row] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Fill(1),
        Constraint::Length(1),
    ]).areas(inner);

    let t = app.theme();
    frame.render_widget(
        Paragraph::new(Span::styled(
            format!(" \u{25a1} {}", app.fp_path.display()),
            Style::default().fg(t.color_hint),
        )),
        path_row,
    );

    let items: Vec<ListItem> = app.fp_entries.iter().enumerate().map(|(i, name)| {
        let selected = i == app.fp_selected;
        let full     = if name == ".." {
            app.fp_path.parent().map(|p| p.to_path_buf()).unwrap_or_else(|| app.fp_path.clone())
        } else {
            app.fp_path.join(name)
        };

        let (badge, badge_style) = if name == ".." {
            ("[^]", Style::default().fg(t.text_dim))
        } else {
            match app.fp_hints.get(&full) {
                Some(FpHint::HasVideos)  => ("[V]", Style::default().fg(t.color_playing)),
                Some(FpHint::HasSubdirs) => ("[·]", Style::default().fg(t.color_fav)),
                Some(FpHint::System)     => ("[!]", Style::default().fg(Color::Red)),
                Some(FpHint::Unreadable) => ("[?]", Style::default().fg(t.text_dim)),
                _                        => ("[-]", Style::default().fg(t.text_dim)),
            }
        };

        let is_sys = fp_is_system(&full);
        let name_style = if is_sys {
            Style::default().fg(t.text_dim)
        } else if selected {
            Style::default().fg(t.sel_fg).bg(t.sel_bg).add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };

        ListItem::new(Line::from(vec![
            Span::styled(badge, badge_style),
            Span::raw(" "),
            Span::styled(name.as_str(), name_style),
        ]))
    }).collect();

    frame.render_widget(List::new(items), list_area);
    frame.render_widget(
        Paragraph::new(Span::styled(
            " [a] add  [d] remove  [Enter/\u{2192}] enter  [\u{2190}/Bksp] up  [Esc] close",
            Style::default().fg(t.color_hint),
        )),
        help_row,
    );
}

// ─── Sort dialog ──────────────────────────────────────────────────────────────

fn render_sort_dialog(frame: &mut Frame, app: &App, area: Rect) {
    let t = app.theme();
    let w = 38u16;
    let h = (SORT_OPTIONS.len() as u16) + 8;
    let x = area.x + area.width.saturating_sub(w) / 2;
    let y = area.y + area.height.saturating_sub(h) / 2;
    let dialog = Rect::new(x, y, w.min(area.width), h.min(area.height));

    frame.render_widget(Clear, dialog);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(t.border_overlay))
        .title(Span::styled(" \u{21d5} Sort ", Style::default().add_modifier(Modifier::BOLD)));
    let inner = block.inner(dialog);
    frame.render_widget(block, dialog);

    let dim  = Style::default().fg(t.text_dim);
    let hint = Style::default().fg(t.color_hint);

    let mut lines = vec![
        Line::from(Span::styled(" Sort by:", dim)),
        Line::from(""),
    ];

    for (i, &(label, _)) in SORT_OPTIONS.iter().enumerate() {
        let selected = i == app.sort_dialog_sel;
        let style = if selected {
            Style::default().fg(t.sel_fg).bg(t.sel_bg).add_modifier(Modifier::BOLD)
        } else {
            dim
        };
        lines.push(Line::from(Span::styled(
            format!("  {}  {label}", if selected { "\u{25b6}" } else { " " }),
            style,
        )));
    }

    lines.push(Line::from(""));

    let dir_label = if app.sort_desc { "Descending ↓" } else { "Ascending  ↑" };
    lines.push(Line::from(vec![
        Span::styled(" Direction:  ", dim),
        Span::styled(dir_label, Style::default().fg(t.color_col_title).add_modifier(Modifier::BOLD)),
        Span::styled("  [d]", hint),
    ]));

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "  [Enter] apply  [Esc] cancel",
        hint,
    )));

    frame.render_widget(Paragraph::new(Text::from(lines)), inner);
}

// ─── Pack picker ──────────────────────────────────────────────────────────────

fn render_pack_picker(frame: &mut Frame, app: &mut App, area: Rect) {
    let w = (area.width * 2 / 3).max(50).min(area.width);
    let h = (area.height * 3 / 4).max(10).min(area.height);
    let x = area.x + (area.width - w) / 2;
    let y = area.y + (area.height - h) / 2;
    let dialog = Rect::new(x, y, w, h);

    frame.render_widget(Clear, dialog);

    let t = app.theme();
    let pack_name = app.packs.get(app.pack_picker_pack)
        .map(|p| p.name.clone())
        .unwrap_or_default();
    let in_pack_count = app.packs.get(app.pack_picker_pack)
        .map(|p| p.ids.len())
        .unwrap_or(0);

    let title = Span::styled(
        format!(" \u{1f4e6} {} ({} wallpapers) ", pack_name, in_pack_count),
        Style::default().add_modifier(Modifier::BOLD),
    );
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(t.border_overlay))
        .title(title);
    let inner = block.inner(dialog);
    frame.render_widget(block, dialog);

    let [hint_row, list_area] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Fill(1),
    ]).areas(inner);

    frame.render_widget(
        Paragraph::new(Span::styled(
            " \u{25cf} in pack  \u{25cb} not in pack   Space toggle   Esc close",
            Style::default().fg(t.color_hint),
        )),
        hint_row,
    );

    let pack_ids: Vec<u64> = app.packs.get(app.pack_picker_pack)
        .map(|p| p.ids.clone())
        .unwrap_or_default();

    let sel = app.pack_picker_sel;
    let items: Vec<ListItem> = app.wallpapers.iter().enumerate().map(|(i, w)| {
        let in_pack = pack_ids.contains(&w.id);
        let is_sel  = i == sel;
        let check   = if in_pack { "\u{25cf} " } else { "\u{25cb} " };
        let audio   = if w.has_audio { " \u{266a}" } else { "" };

        let style = if is_sel {
            Style::default().fg(t.sel_fg).bg(t.sel_bg).add_modifier(Modifier::BOLD)
        } else if in_pack {
            Style::default().fg(t.color_playing)
        } else {
            Style::default().fg(t.text_dim)
        };

        ListItem::new(Span::styled(format!(" {check}{}{audio}", w.title), style))
    }).collect();

    use ratatui::widgets::ListState;
    let mut list_state = ListState::default().with_selected(Some(sel));
    frame.render_stateful_widget(
        List::new(items).highlight_symbol(""),
        list_area,
        &mut list_state,
    );
}
