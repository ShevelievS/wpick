use std::path::Path;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph};
use wpick_core::model::{WallpaperInfo, WallpaperType};

use crate::app::{App, AppMode};

pub fn render(frame: &mut Frame, app: &mut App) {
    if frame.area().width < 80 || frame.area().height < 20 {
        frame.render_widget(
            Paragraph::new("Terminal too small (min 80\u{00d7}20)"),
            frame.area(),
        );
        return;
    }

    let [header, main, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(2),
    ])
    .areas(frame.area());

    render_header(frame, app, header);
    render_footer(frame, footer);

    match app.mode {
        AppMode::Browse => {
            let [list_area, detail_area] = Layout::horizontal([
                Constraint::Percentage(30),
                Constraint::Percentage(70),
            ])
            .areas(main);
            render_list(frame, app, list_area);
            render_detail(frame, app, detail_area);
        }
        AppMode::Detail => {
            render_detail(frame, app, main);
        }
    }
}

// ── Header ────────────────────────────────────────────────────────────────────

fn render_header(frame: &mut Frame, app: &App, area: ratatui::layout::Rect) {
    let (conn_text, conn_color) = if app.daemon_connected {
        ("\u{25cf} connected", Color::Green)
    } else {
        ("\u{25cb} disconnected", Color::Red)
    };

    let vol_spans: Vec<Span> = if app.config.general.muted {
        vec![Span::styled(
            "  [MUTED] ",
            Style::default()
                .fg(Color::Black)
                .bg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )]
    } else {
        let filled = (app.config.general.volume * 10.0) as usize;
        let empty  = 10_usize.saturating_sub(filled);
        vec![
            Span::raw("  vol "),
            Span::styled("\u{2588}".repeat(filled), Style::default().fg(Color::Blue)),
            Span::styled("\u{2591}".repeat(empty),  Style::default().fg(Color::DarkGray)),
            Span::raw(format!(" {:.0}%", app.config.general.volume * 100.0)),
        ]
    };

    let mut left_spans = vec![
        Span::styled("wpick", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw("  "),
        Span::styled(conn_text, Style::default().fg(conn_color)),
    ];
    left_spans.extend(vol_spans);

    let now_playing = app.current_wallpaper_id
        .and_then(|id| app.wallpapers.iter().find(|w| w.id == id))
        .map(|w| format!("\u{25b6} {}", w.title));

    if let Some(np) = now_playing {
        let np_len = np.chars().count() as u16;
        let [left_area, right_area] = Layout::horizontal([
            Constraint::Fill(1),
            Constraint::Length(np_len),
        ])
        .areas(area);
        frame.render_widget(Paragraph::new(Line::from(left_spans)), left_area);
        frame.render_widget(
            Paragraph::new(Span::styled(np, Style::default().fg(Color::DarkGray))),
            right_area,
        );
    } else {
        frame.render_widget(Paragraph::new(Line::from(left_spans)), area);
    }
}

// ── List (left panel) ─────────────────────────────────────────────────────────

fn render_list(frame: &mut Frame, app: &mut App, area: ratatui::layout::Rect) {
    let title = format!("Wallpapers ({})", app.wallpapers.len());

    let items: Vec<ListItem> = if app.loading {
        app.list_state.select(None);
        vec![ListItem::new(" Loading\u{2026}")]
    } else if app.wallpapers.is_empty() {
        app.list_state.select(None);
        vec![ListItem::new(" No wallpapers found.")]
    } else {
        let list_idx = wallpaper_idx_to_list_idx(&app.wallpapers, app.selected);
        app.list_state.select(Some(list_idx));

        let mut v: Vec<ListItem> = Vec::new();

        let videos: Vec<&WallpaperInfo> = app.wallpapers.iter()
            .filter(|w| matches!(w.wallpaper_type, WallpaperType::Video))
            .collect();
        let others: Vec<&WallpaperInfo> = app.wallpapers.iter()
            .filter(|w| !matches!(w.wallpaper_type, WallpaperType::Video))
            .collect();

        if !videos.is_empty() {
            v.push(separator("  \u{2500}\u{2500} video \u{2500}\u{2500}"));
            for w in &videos {
                v.push(make_wallpaper_item(w, app.current_wallpaper_id));
            }
        }
        if !others.is_empty() {
            v.push(separator("  \u{2500}\u{2500} scene \u{00b7} web \u{2500}\u{2500}"));
            for w in &others {
                v.push(make_wallpaper_item(w, app.current_wallpaper_id));
            }
        }

        v
    };

    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title(title))
        .highlight_style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))
        .highlight_symbol("> ");

    frame.render_stateful_widget(list, area, &mut app.list_state);
}

fn wallpaper_idx_to_list_idx(wallpapers: &[WallpaperInfo], wp_idx: usize) -> usize {
    let n_videos = wallpapers.iter()
        .filter(|w| matches!(w.wallpaper_type, WallpaperType::Video))
        .count();

    let w = &wallpapers[wp_idx];
    if matches!(w.wallpaper_type, WallpaperType::Video) {
        let video_rank = wallpapers[..wp_idx].iter()
            .filter(|w| matches!(w.wallpaper_type, WallpaperType::Video))
            .count();
        // +1 for the "── video ──" separator at index 0
        1 + video_rank
    } else {
        let non_video_rank = wallpapers[..wp_idx].iter()
            .filter(|w| !matches!(w.wallpaper_type, WallpaperType::Video))
            .count();
        let video_sep = if n_videos > 0 { 1 } else { 0 };
        // video_sep + all_videos + 1 (scene·web separator) + rank
        video_sep + n_videos + 1 + non_video_rank
    }
}

fn separator(text: &'static str) -> ListItem<'static> {
    ListItem::new(Line::from(Span::styled(
        text,
        Style::default().fg(Color::DarkGray),
    )))
}

fn make_wallpaper_item(w: &WallpaperInfo, current_id: Option<u64>) -> ListItem<'static> {
    let (icon, icon_color) = type_icon_and_color(&w.wallpaper_type);

    let mut spans: Vec<Span<'static>> = vec![
        Span::raw(" "),
        Span::styled(icon, Style::default().fg(icon_color)),
        Span::raw(" "),
        Span::raw(w.title.clone()),
    ];

    if w.has_audio {
        spans.push(Span::styled(" \u{266a}", Style::default().fg(Color::Yellow)));
    }

    if Some(w.id) == current_id {
        spans.push(Span::styled(" \u{25cf}", Style::default().fg(Color::Green)));
    }

    let item = ListItem::new(Line::from(spans));
    if !w.is_supported() {
        item.style(Style::default().fg(Color::DarkGray))
    } else {
        item
    }
}

fn type_icon_and_color(t: &WallpaperType) -> (&'static str, Color) {
    match t {
        WallpaperType::Video => ("\u{25b6}", Color::Blue),    // ▶
        WallpaperType::Scene => ("\u{25c8}", Color::Magenta), // ◈
        WallpaperType::Web   => ("\u{2295}", Color::Green),   // ⊕
    }
}

// ── Detail (right panel or full screen) ──────────────────────────────────────

fn render_detail(frame: &mut Frame, app: &App, area: ratatui::layout::Rect) {
    // Always reserve 1 line at bottom for status bar
    let [content_area, status_area] = Layout::vertical([
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .areas(area);

    if app.wallpapers.is_empty() {
        let msg = if app.loading {
            "Loading wallpapers\u{2026}"
        } else {
            "No wallpapers found.\nCheck Steam installation or add paths in config."
        };
        frame.render_widget(
            Paragraph::new(msg)
                .block(Block::default().borders(Borders::ALL).title(" Detail ")),
            content_area,
        );
    } else {
        let [preview_area, info_area] = Layout::vertical([
            Constraint::Percentage(40),
            Constraint::Min(0),
        ])
        .areas(content_area);
        render_preview(frame, app, preview_area);
        render_info(frame, app, info_area);
    }

    render_status(frame, app, status_area);
}

fn render_preview(frame: &mut Frame, app: &App, area: ratatui::layout::Rect) {
    let content = match app.wallpapers.get(app.selected) {
        None => Text::raw(""),
        Some(w) => match &w.preview_path {
            Some(path) => {
                let filename = Path::new(path)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or(path.as_str());
                Text::from(vec![
                    Line::from(format!("[ {} ]", filename)),
                    Line::from(""),
                    Line::from("(image preview in v0.2)"),
                ])
            }
            None => Text::raw("preview not available"),
        },
    };

    frame.render_widget(
        Paragraph::new(content)
            .block(Block::default().borders(Borders::ALL).title(" Preview ")),
        area,
    );
}

fn render_info(frame: &mut Frame, app: &App, area: ratatui::layout::Rect) {
    let w = match app.wallpapers.get(app.selected) {
        Some(w) => w,
        None    => return,
    };

    let label = Style::default().fg(Color::Gray);
    let (icon, _) = type_icon_and_color(&w.wallpaper_type);
    let file_size  = format_bytes(w.file_size_bytes);

    let audio_span: Span = if w.has_audio {
        Span::styled("\u{266a} yes", Style::default().fg(Color::Yellow))
    } else {
        Span::styled("\u{2014}", Style::default().fg(Color::DarkGray))
    };

    let mut lines: Vec<Line> = vec![
        Line::from(vec![Span::styled("title  ", label), Span::raw(w.title.clone())]),
        Line::from(vec![
            Span::styled("type   ", label),
            Span::raw(format!("{} {}", icon, w.wallpaper_type)),
        ]),
        Line::from(vec![Span::styled("audio  ", label), audio_span]),
        Line::from(vec![Span::styled("size   ", label), Span::raw(file_size)]),
        Line::from(vec![
            Span::styled("id     ", label),
            Span::styled(w.id.to_string(), Style::default().fg(Color::DarkGray)),
        ]),
    ];

    if !w.is_supported() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "\u{26a0} not supported in v0.1",
            Style::default().fg(Color::Yellow),
        )));
    }

    frame.render_widget(
        Paragraph::new(Text::from(lines))
            .block(Block::default().borders(Borders::ALL).title(" Details ")),
        area,
    );
}

fn render_status(frame: &mut Frame, app: &App, area: ratatui::layout::Rect) {
    let line = match &app.status_message {
        None => Line::from(""),
        Some(msg) => {
            let color = if app.status_is_error { Color::Yellow } else { Color::Green };
            Line::from(Span::styled(msg.clone(), Style::default().fg(color)))
        }
    };
    frame.render_widget(Paragraph::new(line), area);
}

// ── Footer ────────────────────────────────────────────────────────────────────

fn render_footer(frame: &mut Frame, area: ratatui::layout::Rect) {
    let key  = |s: &'static str| Span::styled(s, Style::default().fg(Color::Cyan));
    let lbl  = |s: &'static str| Span::styled(s, Style::default().fg(Color::DarkGray));
    let mkey = Span::styled("m", Style::default().fg(Color::Yellow));

    let line1 = Line::from(vec![
        key("\u{2191}\u{2193}/jk"), lbl(" nav  "),
        key("Enter"),               lbl(" apply  "),
        key("+/-"),                 lbl(" vol  "),
        mkey,                       lbl(" mute  "),
        key("r"),                   lbl(" refresh  "),
        key("/"),                   lbl(" search"),
    ]);
    let line2 = Line::from(vec![
        key("i"),     lbl(" detail  "),
        key("q"),     lbl(" quit (daemon runs)  "),
        key("Q"),     lbl(" kill daemon"),
    ]);

    frame.render_widget(Paragraph::new(vec![line1, line2]), area);
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn format_bytes(bytes: u64) -> String {
    if bytes >= 1_073_741_824 {
        format!("{:.1} GB", bytes as f64 / 1_073_741_824.0)
    } else if bytes >= 1_048_576 {
        format!("{:.1} MB", bytes as f64 / 1_048_576.0)
    } else if bytes >= 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{bytes} B")
    }
}
