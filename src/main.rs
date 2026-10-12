// Databaser — start, stop and restart local MySQL and PostgreSQL services.
//
// Two entry points share one binary:
//   * normal:   the GPUI window (unelevated)
//   * `--ctl`:  a short-lived elevated helper that performs one SCM action

mod app;
mod ctl;
mod elevation;
mod helper_task;
mod services;
mod settings;

use gpui_kit::*;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("--ctl") => std::process::exit(ctl::run(&args)),
        Some("--ctl-from-task") => std::process::exit(ctl::run_from_task()),
        _ => {}
    }

    gpui_kit::application()
        .with_assets(gpui_kit::assets::Assets)
        .run(|cx| {
            gpui_kit::init(cx);
            cx.set_app_identity("com.abraham.databaser", "Databaser");

            let settings = settings::Settings::load();
            app::apply_theme(settings.theme, None, cx);

            if let Err(error) = gpui_kit::open_window(window_options(), cx, move |_window, cx| {
                let settings = settings.clone();
                cx.new(move |cx| app::Databaser::new(settings, cx))
            }) {
                eprintln!("Databaser: failed to open window: {error}");
            }
        });
}

fn window_options() -> WindowOptions {
    let bounds = Bounds::new(point(px(220.), px(160.)), size(px(760.), px(600.)));
    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        titlebar: Some(TitlebarOptions {
            title: Some("Databaser".into()),
            ..Default::default()
        }),
        ..Default::default()
    }
}
