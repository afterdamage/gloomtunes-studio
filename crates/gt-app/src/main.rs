//! GloomTunes Studio desktop application.

#![forbid(unsafe_code)]
// Hide the console window on Windows release builds.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod app;
mod audio_io;

fn main() -> eframe::Result {
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
            .with_inner_size([560.0, 300.0])
            .with_min_inner_size([480.0, 260.0]),
        ..Default::default()
    };
    eframe::run_native(
        "GloomTunes Studio",
        options,
        Box::new(|cc| Ok(Box::new(app::GloomApp::new(&cc.egui_ctx)))),
    )
}
