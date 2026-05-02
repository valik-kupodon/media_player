mod gui;
mod media_streams;

use eframe::egui;
use gui::player_app::PlayerApp;
use std::sync::mpsc;

fn main() -> Result<(), eframe::Error> {
    let playlist = vec![
        "/home/valentinef/my_home/v/347732_h".to_string(),
        "/home/valentinef/my_home/v/347779_h".to_string(),
    ];

    let (_, video_rx) = mpsc::sync_channel(3);
    let mut app = PlayerApp::new(video_rx, playlist);
    if !app.playlist.is_empty() {
        app.play_track(0);
    }
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([1280.0, 720.0]),
        ..Default::default()
    };

    eframe::run_native(
        "Valentine Media Player",
        options,
        Box::new(|_cc| Ok(Box::new(app))),
    )
}
