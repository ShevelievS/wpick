use ratatui::Frame;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};
use wpick_core::model::WallpaperInfo;

use crate::app::{App, AppMode};

pub fn render(frame: &mut Frame, app: &App) {
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
    render_footer(frame, app, footer);

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
        ("\u{25cf} Connected", Color::Green)
    } else {
        ("\u{25cb} Disconnected", Color::Red)
    };

    let filled = (app.config.general.volume * 10.0) as usize;
    let empty  = 10_usize.saturating_sub(filled);
    let vol_bar = format!(
        "  Vol {}{} {:.0}%",
        "\u{2588}".repeat(filled),
        "\u{2591}".repeat(empty),
        app.config.general.volume * 100.0
    );

    let mute_span = if app.config.general.muted {
        Span::styled(
            " [MUTED] ",
            Style::default()
                .fg(Color::Black)
                .bg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )
    } else {
        Span::styled(" [sound] ", Style::default().fg(Color::DarkGray))
    };

    let line = Line::from(vec![
        Span::styled("wpick", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw("  "),
        Span::styled(conn_text, Style::default().fg(conn_color)),
        Span::raw(vol_bar),
        mute_span,
    ]);

    frame.render_widget(Paragraph::new(line), area);
}

// ── List (left panel) ─────────────────────────────────────────────────────────

fn render_list(frame: &mut Frame, app: &App, area: ratatui::layout::Rect) {
    let items: Vec<ListItem> = if app.loading {
        vec![ListItem::new("Loading wallpapers...")]
    } else if app.wallpapers.is_empty() {
        vec![ListItem::new("No wallpapers found.")]
    } else {
        app.wallpapers
            .iter()
            .map(|w| {
                let is_current = Some(w.id) == app.current_wallpaper_id;
                let prefix = if is_current { "\u{25b6} " } else { "  " };
                let color   = if w.is_supported() { Color::Reset } else { Color::DarkGray };
                ListItem::new(format!("{}{} {}", prefix, w.type_icon(), w.title))
                    .style(Style::default().fg(color))
            })
            .collect()
    };

    let title = format!("Wallpapers ({})", app.wallpapers.len());
    let list  = List::new(items)
        .block(Block::default().borders(Borders::ALL).title(title))
        .highlight_style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("> ");

    let mut list_state = ListState::default();
    list_state.select(if app.wallpapers.is_empty() || app.loading {
        None
    } else {
        Some(app.selected)
    });

    frame.render_stateful_widget(list, area, &mut list_state);
}

// ── Detail (right panel or full screen) ──────────────────────────────────────

fn render_detail(frame: &mut Frame, app: &App, area: ratatui::layout::Rect) {
    if app.wallpapers.is_empty() {
        let msg = if app.loading {
            "Loading wallpapers..."
        } else {
            "No wallpapers found.\nCheck Steam installation or add paths in config."
        };
        let block = Block::default().borders(Borders::ALL).title(" Detail ");
        frame.render_widget(Paragraph::new(msg).block(block), area);
        return;
    }

    let [preview_area, info_area] = Layout::vertical([
        Constraint::Percentage(40),
        Constraint::Percentage(60),
    ])
    .areas(area);

    render_preview(frame, app, preview_area);
    render_info(frame, app, info_area);
}

fn render_preview(frame: &mut Frame, app: &App, area: ratatui::layout::Rect) {
    let block = Block::default().borders(Borders::ALL).title(" Preview ");

    let content = match app.wallpapers.get(app.selected) {
        None    => String::new(),
        Some(w) => match &w.preview_path {
            Some(path) => format!("[ {} ]\n\n(image rendering in v0.2)", path),
            None       => "[ preview not available in v0.1 ]".to_string(),
        },
    };

    frame.render_widget(Paragraph::new(content).block(block), area);
}

fn render_info(frame: &mut Frame, app: &App, area: ratatui::layout::Rect) {
    let w: &WallpaperInfo = match app.wallpapers.get(app.selected) {
        Some(w) => w,
        None    => return,
    };

    let label     = Style::default().fg(Color::Gray);
    let audio_str = if w.has_audio { "\u{266a} Yes" } else { "No" };
    let file_size = format_bytes(w.file_size_bytes);

    let mut lines: Vec<Line> = vec![
        Line::from(vec![
            Span::styled("Title:  ", label),
            Span::raw(w.title.clone()),
        ]),
        Line::from(vec![
            Span::styled("Type:   ", label),
            Span::raw(format!("{}  {}", w.wallpaper_type, w.type_icon())),
        ]),
        Line::from(vec![
            Span::styled("Audio:  ", label),
            Span::raw(audio_str),
        ]),
        Line::from(vec![
            Span::styled("Size:   ", label),
            Span::raw(file_size),
        ]),
        Line::from(vec![
            Span::styled("ID:     ", label),
            Span::raw(w.id.to_string()),
        ]),
    ];

    if !w.is_supported() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "\u{26a0} Not supported (scene/web)",
            Style::default().fg(Color::Yellow),
        )));
    }

    if let Some(ref msg) = app.status_message {
        let color = if app.status_is_error { Color::Yellow } else { Color::Green };
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            format!("[ {} ]", msg),
            Style::default().fg(color),
        )));
    }

    let block = Block::default().borders(Borders::ALL).title(" Details ");
    frame.render_widget(Paragraph::new(Text::from(lines)).block(block), area);
}

// ── Footer ────────────────────────────────────────────────────────────────────

fn render_footer(frame: &mut Frame, _app: &App, area: ratatui::layout::Rect) {
    let key  = |s: &'static str| Span::styled(s, Style::default().fg(Color::Cyan));
    let lbl  = |s: &'static str| Span::styled(s, Style::default().fg(Color::DarkGray));
    let mute = Span::styled(
        "[A]",
        Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
    );

    let line1 = Line::from(vec![
        key("\u{2191}\u{2193}/jk"), lbl(" Nav  "),
        key("Enter"), lbl(" Apply  "),
        key("+/-"), lbl(" Vol  "),
        mute, lbl(" Mute  "),
        key("r"), lbl(" Refresh"),
    ]);
    let line2 = Line::from(vec![
        key("i"), lbl(" Detail  "),
        key("q/Esc"), lbl(" Quit (daemon runs)  "),
        key("Q"), lbl(" Kill daemon"),
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