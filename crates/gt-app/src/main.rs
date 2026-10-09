//! GloomTunes Studio desktop application.

#![forbid(unsafe_code)]
// Hide the console window on Windows release builds.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod app;
mod audio_io;
mod files;
mod library;
mod live;
mod midi_io;

fn main() -> eframe::Result {
    // Started by the plugin scanner to look inside one plugin file, out of harm's way.
    let mut args = std::env::args_os().skip(1);
    if args.next().as_deref() == Some(std::ffi::OsStr::new(gt_plugin_host::SCAN_SWITCH)) {
        let file = args
            .next()
            .map(std::path::PathBuf::from)
            .unwrap_or_default();
        std::process::exit(gt_plugin_host::catalog::scan_child_main(&file));
    }

    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    // wgpu (Vulkan/DX12) is the default renderer. If it cannot start, e.g. no Vulkan driver,
    // fall back to OpenGL (glow). `GT_RENDERER=glow` forces OpenGL from the start.
    let forced_glow = std::env::var("GT_RENDERER").as_deref() == Ok("glow");
    if !forced_glow {
        match run(eframe::Renderer::Wgpu) {
            Err(eframe::Error::Wgpu(e)) => {
                log::warn!("wgpu renderer unavailable ({e}); falling back to OpenGL");
            }
            other => return other,
        }
    }
    run(eframe::Renderer::Glow)
}

fn run(renderer: eframe::Renderer) -> eframe::Result {
    let options = eframe::NativeOptions {
        renderer,
        viewport: egui::ViewportBuilder::default()
            .with_title("GloomTunes Studio")
            .with_inner_size([1240.0, 720.0])
            .with_min_inner_size([960.0, 520.0]),
        ..Default::default()
    };
    eframe::run_native(
        "GloomTunes Studio",
        options,
        Box::new(|cc| {
            // A project file given on the command line opens at start.
            let open = std::env::args_os().nth(1).map(std::path::PathBuf::from);
            Ok(Box::new(app::GloomApp::new(&cc.egui_ctx, open)))
        }),
    )
}
