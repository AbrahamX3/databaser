// Databaser - start, stop and restart local MySQL and PostgreSQL services.
//
// Two entry points share one binary:
//   * normal:   the GPUI window (unelevated)
//   * `--ctl`:  a short-lived elevated helper that performs one SCM action

// Build as a GUI app rather than a console app, so Windows never allocates a
// console window: not for the main window, and not for the short-lived elevated
// helper processes (they report results through files and exit codes).
#![cfg_attr(windows, windows_subsystem = "windows")]

mod app;
mod assets;
mod autostart;
mod ctl;
mod elevation;
mod helper_task;
mod platform;
mod services;
mod settings;
mod tray;
mod update;

use gpui_kit::*;
use std::io::Write as _;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("--ctl") => std::process::exit(ctl::run(&args)),
        Some("--ctl-from-task") => std::process::exit(ctl::run_from_task()),
        _ => {}
    }

    // Starting with Windows makes a second launch likely (autostart plus a
    // manual open), so surface the window that already exists instead of
    // adding a duplicate that also polls. Held for the rest of `main`.
    let Some(_instance) = platform::SingleInstance::acquire("Databaser.single-instance") else {
        platform::focus_window(platform::WINDOW_TITLE);
        return;
    };

    // Launched by the Run entry rather than by the user: come up quietly.
    let from_run_entry = args.iter().any(|arg| arg == "--startup");

    gpui_kit::application()
        .with_assets(assets::AppAssets)
        .run(move |cx| {
            gpui_kit::init(cx);
            cx.set_app_identity("com.abraham.databaser", "Databaser");
            cx.set_global(tray::TrayState::default());

            // Tray actions are ordinary GPUI actions, dispatched to the app.
            let _ = cx.on_action(|_: &tray::OpenWindow, _cx: &mut App| {
                platform::show_window(platform::WINDOW_TITLE, true);
            });
            let _ = cx.on_action(|_: &tray::QuitApp, cx: &mut App| {
                tray::remove(cx);
                cx.quit();
            });

            let settings = settings::Settings::load();
            app::apply_theme(settings.theme, None, cx);

            if settings.background {
                if let Err(error) = tray::install(cx) {
                    let _ = writeln!(std::io::stderr(), "Databaser: no tray icon: {error}");
                }
            }

            if let Err(error) = gpui_kit::open_window(window_options(), cx, move |window, cx| {
                let settings = settings.clone();
                // Closing the window is how background mode is left running.
                window.on_window_should_close(cx, app::window_should_close);
                cx.new(move |cx| app::Databaser::new(settings, cx))
            }) {
                // Best-effort: a GUI-subsystem build may have no console at all,
                // and `eprintln!` would panic on an invalid handle.
                let _ = writeln!(std::io::stderr(), "Databaser: failed to open window: {error}");
            }

            // Start hidden in the tray when both the Run entry and background
            // mode are on; the tray icon is how it is brought back.
            if from_run_entry && tray::is_enabled(cx) {
                platform::show_window(platform::WINDOW_TITLE, false);
            }
        });
}

fn window_options() -> WindowOptions {
    let bounds = Bounds::new(point(px(220.), px(160.)), size(px(760.), px(600.)));
    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        // Keep the layout usable: below this the cards reflow rather than clip.
        window_min_size: Some(size(px(520.), px(360.))),
        titlebar: Some(TitlebarOptions {
            title: Some(platform::WINDOW_TITLE.into()),
            ..Default::default()
        }),
        ..Default::default()
    }
}
