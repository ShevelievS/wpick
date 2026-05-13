/// wpick-webview — renders a Wallpaper Engine Web wallpaper (HTML/JS) as a
/// wlr-layer-shell Background surface using GTK3 + webkit2gtk + gtk-layer-shell.
///
/// Usage:
///   wpick-webview --file /path/to/index.html [--output DP-1]
use anyhow::Result;
use gtk::prelude::*;
use gtk_layer_shell::LayerShell;
use webkit2gtk::{
    UserContentInjectedFrames, UserContentManager, UserContentManagerExt,
    UserScript, UserScriptInjectionTime, WebView, WebViewExt,
};

// ─── CLI args ─────────────────────────────────────────────────────────────────

struct Args {
    file:   String,
    output: Option<String>,
}

fn parse_args() -> Result<Args> {
    let mut iter = std::env::args().skip(1);
    let mut file   = None::<String>;
    let mut output = None::<String>;

    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--file"   => { file   = iter.next(); }
            "--output" => { output = iter.next(); }
            other => anyhow::bail!("Unknown argument: {other}"),
        }
    }

    let file = file.ok_or_else(|| anyhow::anyhow!("--file <path> is required"))?;
    Ok(Args { file, output })
}

// ─── WE JS stubs ──────────────────────────────────────────────────────────────

const WE_JS_STUBS: &str = r#"
window.wallpaperRegisterAudioListener = function() {};
window.wallpaperRegisterAudioProcessingFunction = function() {};
window.wallpaperPlaylistTrackChanged  = function() {};
window.wallpaperPropertyListener      = {};
window.wallpaperRequestRandomSeed     = function(cb) { cb(Math.floor(Math.random()*65536)); };
"#;

// ─── Main ─────────────────────────────────────────────────────────────────────

fn main() -> Result<()> {
    let args = parse_args()?;

    gtk::init().map_err(|e| anyhow::anyhow!("gtk::init: {e}"))?;

    let window = gtk::Window::new(gtk::WindowType::Toplevel);
    window.set_title("wpick-webview");
    window.set_decorated(false);
    window.set_app_paintable(true);

    // Apply gtk-layer-shell via the LayerShell trait.
    window.init_layer_shell();
    window.set_layer(gtk_layer_shell::Layer::Background);
    window.set_anchor(gtk_layer_shell::Edge::Top,    true);
    window.set_anchor(gtk_layer_shell::Edge::Bottom, true);
    window.set_anchor(gtk_layer_shell::Edge::Left,   true);
    window.set_anchor(gtk_layer_shell::Edge::Right,  true);
    window.set_exclusive_zone(-1);
    window.set_keyboard_mode(gtk_layer_shell::KeyboardMode::None);

    // Target a specific monitor if requested.
    if let Some(ref output_name) = args.output {
        if let Some(display) = gdk::Display::default() {
            for i in 0..display.n_monitors() {
                if let Some(monitor) = display.monitor(i) {
                    if monitor.model().as_deref() == Some(output_name.as_str()) {
                        window.set_monitor(&monitor);
                        break;
                    }
                }
            }
        }
    }

    // UserContentManager with WE JS stubs injected at document start.
    let user_content = UserContentManager::new();
    let script = UserScript::new(
        WE_JS_STUBS,
        UserContentInjectedFrames::AllFrames,
        UserScriptInjectionTime::Start,
        &[] as &[&str],
        &[] as &[&str],
    );
    user_content.add_script(&script);

    let webview = WebView::with_user_content_manager(&user_content);
    webview.set_background_color(&gdk::RGBA::new(0.0, 0.0, 0.0, 0.0));
    webview.load_uri(&format!("file://{}", args.file));

    window.add(&webview);
    window.show_all();

    // Graceful shutdown on SIGTERM/SIGINT.
    let main_loop = glib::MainLoop::new(None, false);
    {
        let ml = main_loop.clone();
        window.connect_destroy(move |_| ml.quit());
    }
    {
        let ml = main_loop.clone();
        glib::unix_signal_add(libc::SIGTERM, move || { ml.quit(); glib::ControlFlow::Break });
    }
    {
        let ml = main_loop.clone();
        glib::unix_signal_add(libc::SIGINT, move || { ml.quit(); glib::ControlFlow::Break });
    }

    main_loop.run();
    Ok(())
}
