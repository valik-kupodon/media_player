use crate::media_streams::{MediaStreams, VideoFrame};
use eframe::egui;
use std::sync::mpsc::{Receiver, SyncSender};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU32, Ordering},
};
use std::thread;

pub struct PlayerApp {
    video_rx: Receiver<VideoFrame>,
    texture: Option<egui::TextureHandle>,
    is_playing: bool,
    volume: f32,
    pub playlist: Vec<String>,
    current_index: usize,
    shared_volume: Arc<AtomicU32>,
    shared_paused: Arc<AtomicBool>,
    pub is_hide_playlist: bool,
}

impl PlayerApp {
    pub fn new(video_rx: Receiver<VideoFrame>) -> Self {
        Self {
            video_rx,
            texture: None,
            is_playing: true,
            volume: 0.5,
            playlist: Vec::new(),
            current_index: 0,
            shared_volume: Arc::new(AtomicU32::new(0.5_f32.to_bits())),
            shared_paused: Arc::new(AtomicBool::new(false)),
            is_hide_playlist: false,
        }
    }

    pub fn start_video_decoder(
        file_path: String,
        video_tx: SyncSender<VideoFrame>,
        shared_volume: Arc<AtomicU32>,
        shared_paused: Arc<AtomicBool>,
    ) {
        thread::spawn(move || {
            let media_streams = MediaStreams::new(file_path, shared_volume, shared_paused);

            if let Err(e) = media_streams.play_with_video_tx(video_tx) {
                eprintln!("Помилка при відтворенні медіа: {}", e);
            }
        });
    }

    pub fn play_track(&mut self, index: usize) {
        if index >= self.playlist.len() {
            eprintln!("Індекс поза межами плейлиста: {}", index);
            return;
        }
        self.current_index = index;
        let new_file = self.playlist[self.current_index].clone();

        // Створюємо абсолютно нову трубу
        let (new_tx, new_rx) = std::sync::mpsc::sync_channel(3);

        // Запускаємо новий потік декодера
        // (старий потік тихо помре, щойно ми перезапишемо self.video_rx)
        self.shared_paused.store(false, Ordering::Relaxed);
        Self::start_video_decoder(
            new_file,
            new_tx,
            Arc::clone(&self.shared_volume),
            Arc::clone(&self.shared_paused),
        );

        // Підміняємо трубу та очищаємо старий кадр
        self.video_rx = new_rx;
        self.texture = None; // Щоб на мить з'явився напис "Завантаження..."
        self.is_playing = true;
    }
    fn handle_input(&mut self, ctx: &eframe::egui::Context) {
        if !ctx.egui_wants_keyboard_input() {
            if ctx.input(|i| i.key_pressed(eframe::egui::Key::Space)) {
                self.toggle_play();
            }
        }
    }

    /// Перемикання паузи (щоб не дублювати логіку в кнопці та пробілі)
    fn toggle_play(&mut self) {
        self.is_playing = !self.is_playing;
        self.shared_paused
            .store(!self.is_playing, Ordering::Relaxed);
    }

    /// Права панель - Плейлист
    fn draw_playlist_panel(&mut self, ui: &mut eframe::egui::Ui) {
        eframe::egui::Panel::right("playlist_panel")
            .default_size(200.0)
            .show_inside(ui, |ui| {
                ui.heading("Плейлист");
                ui.separator();

                if ui.button("📂 Додати відео...").clicked() {
                    if let Some(files) = rfd::FileDialog::new().pick_files() {
                        for path in files {
                            self.playlist.push(path.to_string_lossy().to_string());
                        }
                        if self.playlist.len() > 0 && !self.is_playing && self.texture.is_none() {
                            self.play_track(0);
                        }
                    }
                }

                ui.separator();
                self.draw_tracks_list(ui);
            });
    }

    fn draw_tracks_list(&mut self, ui: &mut eframe::egui::Ui) {
        eframe::egui::ScrollArea::vertical().show(ui, |ui| {
            let mut track_to_play = None;
            for (index, file_path) in self.playlist.iter().enumerate() {
                let file_name = std::path::Path::new(file_path)
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy();

                let is_current = index == self.current_index;
                let text = eframe::egui::RichText::new(file_name).color(if is_current {
                    eframe::egui::Color32::GREEN
                } else {
                    eframe::egui::Color32::LIGHT_GRAY
                });

                if ui.selectable_label(is_current, text).clicked() {
                    track_to_play = Some(index);
                }
            }
            if let Some(index) = track_to_play {
                self.play_track(index);
            }
        });
    }

    /// Нижня панель керування
    fn draw_controls(&mut self, ui: &mut eframe::egui::Ui) {
        ui.horizontal(|ui| {
            ui.add_space(20.0);

            // Кнопка Play/Pause
            let play_text = if self.is_playing {
                "⏸ Пауза"
            } else {
                "▶ Відтворити"
            };
            let color = if self.is_playing {
                eframe::egui::Color32::LIGHT_RED
            } else {
                eframe::egui::Color32::GREEN
            };

            if ui
                .button(
                    eframe::egui::RichText::new(play_text)
                        .size(18.0)
                        .color(color),
                )
                .clicked()
            {
                self.toggle_play();
            }

            ui.add_space(15.0);
            ui.label(
                eframe::egui::RichText::new("00:00 / 00:00")
                    .size(16.0)
                    .color(eframe::egui::Color32::RED),
            );

            ui.separator();
            self.draw_nav_buttons(ui);
            self.draw_volume_slider(ui);
            let toggle_text = if self.is_hide_playlist {
                "📂 Показати плейлист"
            } else {
                "📁 Приховати плейлист"
            };
            if ui
                .selectable_label(!self.is_hide_playlist, toggle_text)
                .clicked()
            {
                self.is_hide_playlist = !self.is_hide_playlist;
            }
        });
    }

    fn draw_nav_buttons(&mut self, ui: &mut eframe::egui::Ui) {
        if ui
            .button(
                eframe::egui::RichText::new("⏮ Prev")
                    .size(15.0)
                    .color(eframe::egui::Color32::LIGHT_BLUE),
            )
            .clicked()
        {
            let idx = if self.current_index > 0 {
                self.current_index - 1
            } else {
                self.playlist.len() - 1
            };
            self.play_track(idx);
        }
        if ui
            .button(
                eframe::egui::RichText::new("⏭ Next")
                    .size(15.0)
                    .color(eframe::egui::Color32::LIGHT_BLUE),
            )
            .clicked()
        {
            let idx = (self.current_index + 1) % self.playlist.len();
            self.play_track(idx);
        }
    }

    fn draw_volume_slider(&mut self, ui: &mut eframe::egui::Ui) {
        ui.label("🔊");
        if ui
            .add(eframe::egui::Slider::new(&mut self.volume, 0.0..=3.0).show_value(false))
            .changed()
        {
            self.shared_volume
                .store(self.volume.to_bits(), Ordering::Relaxed);
        }
    }
    fn process_video_frames(&mut self, ctx: &eframe::egui::Context) {
        let mut latest_frame = None;

        // Дренуємо канал, щоб отримати найсвіжіший кадр
        while let Ok(frame) = self.video_rx.try_recv() {
            latest_frame = Some(frame);
        }

        // Оновлюємо текстуру тільки якщо ми граємо і прийшов новий кадр
        if self.is_playing {
            if let Some(frame) = latest_frame {
                let image = eframe::egui::ColorImage::from_rgb(
                    [frame.width, frame.height],
                    &frame.rgb_data,
                );

                self.texture = Some(ctx.load_texture(
                    "vid_frame",
                    image,
                    eframe::egui::TextureOptions::LINEAR,
                ));
            }
        }
    }
}

impl eframe::App for PlayerApp {
    fn ui(&mut self, ui: &mut eframe::egui::Ui, _frame: &mut eframe::Frame) {
        // 1. Логіка оновлення стану
        self.handle_input(ui.ctx());
        self.process_video_frames(ui.ctx()); // Винесли отримання кадрів

        // 2. Рендеринг панелей
        if !self.is_hide_playlist {
            self.draw_playlist_panel(ui);
        }

        eframe::egui::CentralPanel::default().show_inside(ui, |ui| {
            ui.ctx().set_visuals(eframe::egui::Visuals::dark());

            ui.with_layout(
                eframe::egui::Layout::bottom_up(eframe::egui::Align::Center),
                |ui| {
                    ui.add_space(10.0);

                    // Малюємо керування
                    self.draw_controls(ui);

                    ui.add_space(10.0);
                    ui.separator();

                    // МАЛЮВАННЯ САМОГО ВІДЕО
                    if let Some(texture) = &self.texture {
                        ui.image((texture.id(), ui.available_size()));
                    } else {
                        ui.centered_and_justified(|ui| {
                            ui.label(
                                eframe::egui::RichText::new("Завантаження відео...")
                                    .size(30.0)
                                    .strong(),
                            );
                        });
                    }
                },
            );
        });

        if self.is_playing {
            ui.ctx().request_repaint();
        }
    }
}
