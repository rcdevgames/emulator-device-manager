#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod proc;
mod sdk;
mod settings;
mod tasks;

use eframe::egui;

fn main() -> eframe::Result {
    // Gate before any window exists: if the toolchain this app drives is missing,
    // say so in a dialog and exit. No half-dead window to click around in.
    if let Some(problem) = sdk::preflight() {
        sdk::report_and_exit(&problem);
    }

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1180.0, 800.0])
            .with_min_inner_size([880.0, 560.0])
            .with_title("ManDra")
            .with_icon(window_icon()),
        ..Default::default()
    };
    eframe::run_native(
        "mandra",
        options,
        Box::new(|cc| Ok(Box::new(app::App::new(cc)))),
    )
}

/// Same artwork as the embedded exe icon, for the title bar and taskbar.
fn window_icon() -> egui::IconData {
    let bytes = include_bytes!("../assets/icon.png");
    let mut decoder = png::Decoder::new(std::io::Cursor::new(&bytes[..]));
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let fallback = || egui::IconData {
        rgba: vec![0x16, 0x18, 0x1c, 0xff],
        width: 1,
        height: 1,
    };
    let Ok(mut reader) = decoder.read_info() else {
        return fallback();
    };
    let mut buf = vec![0u8; reader.output_buffer_size()];
    let Ok(info) = reader.next_frame(&mut buf) else {
        return fallback();
    };
    let (w, h) = (info.width as usize, info.height as usize);
    let data = &buf[..info.buffer_size()];
    let rgba = match info.color_type {
        png::ColorType::Rgba => data.to_vec(),
        png::ColorType::Rgb => {
            let mut out = Vec::with_capacity(w * h * 4);
            for px in data.chunks_exact(3) {
                out.extend_from_slice(&[px[0], px[1], px[2], 255]);
            }
            out
        }
        _ => return fallback(),
    };
    if rgba.len() < w * h * 4 {
        return fallback();
    }
    egui::IconData {
        rgba,
        width: w as u32,
        height: h as u32,
    }
}
