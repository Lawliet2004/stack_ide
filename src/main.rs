use blue_ide::app::BlueIdeApp;
use blue_ide::perf::startup::StartupTimer;

fn main() -> eframe::Result<()> {
    // StartupTimer must be the very first thing in main() — before eframe::run_native.
    // It measures total startup including window creation time.
    let mut timer = StartupTimer::new();
    timer.begin("Total startup");

    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1200.0, 800.0])
            .with_decorations(false),
        ..Default::default()
    };

    // Move `timer` into the creation closure so it's available when App is constructed.
    let mut timer_cell = Some(timer);

    eframe::run_native(
        "Blue IDE",
        native_options,
        Box::new(move |creation_context| {
            let mut timer = timer_cell.take().unwrap_or_else(StartupTimer::new);
            timer.begin("eframe init");
            let mut app = BlueIdeApp::new(creation_context);
            timer.end("eframe init");
            timer.end("Total startup");
            // Finalise and store on App.
            let data = timer.finish();
            // Save to history (non-blocking).
            blue_ide::perf::startup::save_startup_history(data.to_history_entry());
            app.startup_data = Some(data);
            Box::new(app)
        }),
    )
}
