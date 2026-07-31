use crate::media_streams::{MediaStreams, VideoFrame};
use eframe::egui;
use keepawake::KeepAwake;
use std::sync::mpsc::{Receiver, SyncSender};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicI8, AtomicU32, AtomicU64, Ordering},
};
use std::thread;
use std::time::Duration;

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
    current_time: f64,
    total_time: f64,
    shared_seek: Arc<AtomicU64>,
    is_dragging: bool,
    drag_time: f64,
    visual_index: i8,
    shared_visual: Arc<AtomicI8>,
    keep_awake: Option<KeepAwake>,
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
            current_time: 0.0,
            total_time: 0.0,
            shared_seek: Arc::new(AtomicU64::new(0)),
            is_dragging: false,
            drag_time: 0.0,
            visual_index: 1,
            shared_visual: Arc::new(AtomicI8::new(1)),
            keep_awake: keepawake::Builder::default().display(true).create().ok(),
        }
    }

    pub fn start_video_decoder(
        file_path: String,
        video_tx: SyncSender<VideoFrame>,
        shared_volume: Arc<AtomicU32>,
        shared_paused: Arc<AtomicBool>,
        shader_seek: Arc<AtomicU64>,
        shared_visual: Arc<AtomicI8>,
    ) {
        thread::spawn(move || {
            let media_streams = MediaStreams::new(
                file_path,
                shared_volume,
                shared_paused,
                shader_seek,
                shared_visual,
            );

            if let Err(e) = media_streams.play_with_video_tx(video_tx) {
                eprintln!("Помилка при відтворенні медіа: {}", e);
            }
        });
    }

    #[inline]
    pub fn play_track(&mut self, index: usize) {
        if index >= self.playlist.len() {
            eprintln!("Індекс поза межами плейлиста: {}", index);
            self.is_playing = false;
            return;
        }
        self.current_index = index;
        let new_file = self.playlist[self.current_index].clone();

        self.keep_awake = keepawake::Builder::default().display(true).create().ok();

        // Створюємо абсолютно нову трубу
        let (new_tx, new_rx) = std::sync::mpsc::sync_channel(3);

        // Запускаємо новий потік декодера
        // (старий потік тихо помре, щойно ми перезапишемо self.video_rx)
        self.shared_paused.store(false, Ordering::SeqCst);
        Self::start_video_decoder(
            new_file,
            new_tx,
            Arc::clone(&self.shared_volume),
            Arc::clone(&self.shared_paused),
            Arc::clone(&self.shared_seek),
            Arc::clone(&self.shared_visual),
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
    #[inline]
    fn toggle_play(&mut self) {
        self.is_playing = !self.is_playing;
        self.shared_paused.store(!self.is_playing, Ordering::SeqCst);
        if self.is_playing {
            self.keep_awake = keepawake::Builder::default().display(true).create().ok();
        } else {
            self.keep_awake = None;
        }
    }

    /// Права панель - Плейлист
    fn draw_playlist_panel(&mut self, ui: &mut eframe::egui::Ui) {
        eframe::egui::Panel::right("playlist_panel")
            .default_size(200.0)
            .show(ui, |ui| {
                ui.heading("Плейлист");
                ui.separator();

                if ui.button("📂 Додати медіа файли...").clicked() {
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

    fn draw_time_seeker(&mut self, ui: &mut eframe::egui::Ui) {
        ui.scope(|ui| {
            ui.spacing_mut().slider_width = ui.available_width();

            let visuals = ui.visuals_mut();
            visuals.selection.bg_fill = eframe::egui::Color32::from_rgb(220, 20, 40);
            visuals.widgets.inactive.fg_stroke.color = eframe::egui::Color32::WHITE;
            visuals.widgets.inactive.bg_fill = eframe::egui::Color32::from_rgb(40, 40, 40);

            let mut display_time = if self.is_dragging {
                self.drag_time
            } else {
                self.current_time
            };

            let response = ui.add(
                eframe::egui::Slider::new(&mut display_time, 0.0..=self.total_time)
                    .show_value(false)
                    .trailing_fill(true),
            );

            if response.drag_started() {
                self.is_dragging = true;
            }
            if response.dragged() {
                self.drag_time = display_time;
            }
            if response.drag_stopped() {
                self.is_dragging = false;
                self.shared_seek
                    .store(display_time.to_bits(), std::sync::atomic::Ordering::Relaxed);
            }
        });

        ui.add_space(5.0);
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
            let timecode = format!(
                "{} / {}",
                Self::format_time(self.current_time),
                Self::format_time(self.total_time)
            );
            ui.label(
                eframe::egui::RichText::new(timecode)
                    .size(16.0)
                    .color(eframe::egui::Color32::RED),
            );

            ui.separator();
            self.draw_nav_buttons(ui);
            self.draw_volume_slider(ui);
            ui.separator();
            ui.label("✨ Візуал:");
            eframe::egui::ComboBox::from_id_salt("vis_combo")
                .selected_text(match self.visual_index {
                    0 => "Вимкнено",
                    1 => "Кроляча нора",
                    2 => "Еквалайзер",
                    _ => "Невідомо",
                })
                .show_ui(ui, |ui| {
                    if ui
                        .selectable_label(self.visual_index == 0, "Вимкнено")
                        .clicked()
                    {
                        self.visual_index = 0;
                        self.shared_visual.store(0, Ordering::Relaxed);
                    }
                    if ui
                        .selectable_label(self.visual_index == 1, "Кроляча нора")
                        .clicked()
                    {
                        self.visual_index = 1;
                        self.shared_visual.store(1, Ordering::Relaxed);
                    }
                    if ui
                        .selectable_label(self.visual_index == 2, "Еквалайзер")
                        .clicked()
                    {
                        self.visual_index = 2;
                        self.shared_visual.store(2, Ordering::Relaxed);
                    }
                });
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
        if !self.is_playing {
            return;
        }

        let mut latest_video_frame = None;
        let mut latest_pts = None;
        let mut latest_duration = None;

        // 1. Дренуємо канал
        loop {
            match self.video_rx.try_recv() {
                Ok(frame) => {
                    // Час оновлюємо ЗАВЖДИ (навіть від dummy-кадрів з аудіо)
                    latest_pts = Some(frame.pts);
                    latest_duration = Some(frame.duration);

                    // Зберігаємо картинку ТІЛЬКИ якщо вона реальна
                    // (так обкладинка альбому ніколи не зникне з екрану!)
                    if frame.width > 0 && frame.height > 0 && !frame.rgb_data.is_empty() {
                        latest_video_frame = Some(frame);
                    }
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    // АВТОВІДТВОРЕННЯ
                    if self.current_index + 1 < self.playlist.len() {
                        self.play_track(self.current_index + 1);
                    } else {
                        self.current_time = 0.0;
                        self.texture = None;
                        self.play_track(0); // Повертаємось на початок
                    }
                    return;
                }
            }
        }

        // 2. Оновлюємо стан UI
        if let Some(pts) = latest_pts {
            self.current_time = pts;
        }
        if let Some(dur) = latest_duration {
            self.total_time = dur;
        }

        // Оновлюємо текстуру тільки якщо прилетів новий РЕАЛЬНИЙ кадр
        if let Some(frame) = latest_video_frame {
            let image =
                eframe::egui::ColorImage::from_rgb([frame.width, frame.height], &frame.rgb_data);
            self.texture =
                Some(ctx.load_texture("vid_frame", image, eframe::egui::TextureOptions::LINEAR));
        }
    }

    #[inline]
    fn format_time(seconds: f64) -> String {
        if seconds.is_nan() || seconds < 0.0 {
            return "00:00".to_string();
        }

        let total_secs = seconds as u64;
        let hours = total_secs / 3600;
        let mins = (total_secs % 3600) / 60;
        let secs = total_secs % 60;
        if hours > 0 {
            format!("{:02}:{:02}:{:02}", hours, mins, secs)
        } else {
            format!("{:02}:{:02}", mins, secs)
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
        eframe::egui::Panel::bottom("controls_panel").show(ui, |ui| {
            ui.add_space(8.0);
            self.draw_time_seeker(ui);
            self.draw_controls(ui);
        });
        eframe::egui::CentralPanel::default().show(ui, |ui| {
            ui.ctx().set_visuals(eframe::egui::Visuals::dark());

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
        });

        if self.is_playing {
            ui.ctx().request_repaint();
        } else {
            ui.ctx().request_repaint_after(Duration::from_millis(50));
        }
    }
}
