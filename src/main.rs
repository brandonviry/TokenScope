#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use tokenscope::{collectors::Collector, storage::Store, ui::TokenScope};

fn main() -> eframe::Result {
    let args: Vec<String> = std::env::args().collect();
    let data_dir = args
        .windows(2)
        .find(|a| a[0] == "--data-dir")
        .map(|a| a[1].clone());
    let store = Store::new(data_dir.map(Into::into));
    let (config, warning) = store.load_config();
    if args.iter().any(|a| a == "--check") {
        let mut collector = Collector::new(store.clone());
        let snapshot = collector.refresh(&config, true);
        let report = serde_json::json!({
            "events": snapshot.events.len(),
            "tokens": snapshot.events.iter().map(|e| e.total()).sum::<u64>(),
            "providers": snapshot.providers,
            "warnings": snapshot.warnings,
            "config_warning": warning,
        });
        let output = store.root.join("diagnostic.json");
        let _ = std::fs::write(
            &output,
            serde_json::to_vec_pretty(&report).unwrap_or_default(),
        );
        println!("{}", report);
        return Ok(());
    }
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("TokenScope · Consommation IA")
            .with_inner_size([1360.0, 960.0])
            .with_min_inner_size([960.0, 680.0])
            .with_icon(TokenScope::icon()),
        renderer: eframe::Renderer::Glow,
        ..Default::default()
    };
    eframe::run_native(
        "TokenScope",
        options,
        Box::new(move |cc| Ok(Box::new(TokenScope::new(cc, store, config, warning)))),
    )
}
