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
}

impl PlayerApp {
    pub fn new(video_rx: Receiver<VideoFrame>, playlist: Vec<String>) -> Self {
        Self {
            video_rx,
            texture: None,
            is_playing: true,
            volume: 0.5,
            playlist,
            current_index: 0,
            shared_volume: Arc::new(AtomicU32::new(0.5_f32.to_bits())),
            shared_paused: Arc::new(AtomicBool::new(false)),
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
}

impl eframe::App for PlayerApp {
    fn ui(&mut self, ui: &mut eframe::egui::Ui, _frame: &mut eframe::Frame) {
        if !ui.ctx().egui_wants_keyboard_input() {
            if ui.ctx().input(|i| i.key_pressed(eframe::egui::Key::Space)) {
                self.is_playing = !self.is_playing;
                self.shared_paused
                    .store(!self.is_playing, Ordering::Relaxed);
            }
        }
        // Завжди дренуємо канал кадрів, інакше decoder thread може заблокуватися
        // на повному sync_channel і разом з відео зупинить аудіо.
        eframe::egui::Panel::right("playlist_panel")
            .default_size(200.0)
            .show_inside(ui, |ui| {
                ui.heading("Плейлист");
                ui.separator();

                // Кнопка відкриття діалогу вибору файлів
                if ui.button("📂 Додати відео...").clicked() {
                    // Викликаємо нативне вікно Linux
                    if let Some(files) = rfd::FileDialog::new()
                        .set_title("Виберіть файли для плейлиста")
                        .pick_files()
                    // Дозволяє вибрати кілька файлів (Shift/Ctrl)
                    {
                        // Додаємо вибрані файли в наш вектор
                        for path in files {
                            self.playlist.push(path.to_string_lossy().to_string());
                        }

                        // Якщо це перше відео, яке ми додали - одразу вмикаємо його
                        if self.playlist.len() > 0 && !self.is_playing && self.texture.is_none() {
                            self.play_track(0);
                        }
                    }
                }

                ui.separator();
                eframe::egui::ScrollArea::vertical().show(ui, |ui| {
                    let mut track_to_play = None;

                    for (index, file_path) in self.playlist.iter().enumerate() {
                        // Витягуємо тільки назву файлу з довгого шляху для краси
                        let file_name = std::path::Path::new(file_path)
                            .file_name()
                            .unwrap_or_default()
                            .to_string_lossy();

                        // Виділяємо поточний трек іншим кольором
                        let is_current = index == self.current_index;
                        let text = eframe::egui::RichText::new(file_name).color(if is_current {
                            eframe::egui::Color32::GREEN
                        } else {
                            eframe::egui::Color32::LIGHT_GRAY
                        });

                        // Якщо клікнули по назві в плейлисті - запам'ятовуємо індекс
                        if ui.selectable_label(is_current, text).clicked() {
                            track_to_play = Some(index);
                        }
                    }

                    // Якщо користувач клікнув на трек - перемикаємо (робимо це поза циклом)
                    if let Some(index) = track_to_play {
                        self.play_track(index);
                    }
                });
            });
        eframe::egui::CentralPanel::default().show_inside(ui, |ui| {
            let mut latest_frame = None;
            while let Ok(frame) = self.video_rx.try_recv() {
                latest_frame = Some(frame);
            }

            if self.is_playing
                && let Some(frame) = latest_frame
            {
                let image = eframe::egui::ColorImage::from_rgb(
                    [frame.width, frame.height],
                    &frame.rgb_data,
                );

                self.texture = Some(ui.ctx().load_texture(
                    "vid_frame",
                    image,
                    eframe::egui::TextureOptions::LINEAR,
                ));
            }

            // --- 2. МАЛЮВАННЯ ІНТЕРФЕЙСУ ---
            ui.ctx().set_visuals(eframe::egui::Visuals::dark());

            // Малюємо панель знизу вгору
            ui.with_layout(
                eframe::egui::Layout::bottom_up(eframe::egui::Align::Center),
                |ui| {
                    ui.add_space(10.0);

                    // КНОПКИ КЕРУВАННЯ
                    ui.horizontal(|ui| {
                        ui.add_space(20.0);
                        let button_text = if self.is_playing {
                            "⏸ Пауза"
                        } else {
                            "▶ Відтворити"
                        };
                        let styled_text = eframe::egui::RichText::new(button_text)
                            .size(18.0)
                            .color(if self.is_playing {
                                eframe::egui::Color32::LIGHT_RED
                            } else {
                                eframe::egui::Color32::GREEN
                            });

                        // Зміна стану по кліку
                        if ui.button(styled_text).clicked() {
                            self.is_playing = !self.is_playing;
                            self.shared_paused
                                .store(!self.is_playing, Ordering::Relaxed);
                        }

                        ui.add_space(15.0);
                        ui.label(
                            eframe::egui::RichText::new("00:00 / 00:00")
                                .size(16.0)
                                .color(eframe::egui::Color32::RED),
                        );

                        ui.separator();
                        ui.label("🔊");
                        let prev_button = eframe::egui::RichText::new("⏮ Prev")
                            .size(15.0)
                            .color(eframe::egui::Color32::LIGHT_BLUE);
                        if ui.button(prev_button).clicked() {
                            if self.current_index > 0 {
                                self.play_track(self.current_index - 1);
                            } else if self.current_index == 0 {
                                self.play_track(self.playlist.len() - 1); // Зациклюємо на останній трек
                            }
                        }
                        let next_button = eframe::egui::RichText::new("⏭ Next")
                            .size(15.0)
                            .color(eframe::egui::Color32::LIGHT_BLUE);
                        if ui.button(next_button).clicked() {
                            if self.current_index + 1 < self.playlist.len() {
                                self.play_track(self.current_index + 1);
                            } else if self.current_index + 1 == self.playlist.len() {
                                self.play_track(0); // Зациклюємо на перший трек
                            }
                        }
                        if ui
                            .add(
                                eframe::egui::Slider::new(&mut self.volume, 0.0..=3.0)
                                    .show_value(false),
                            )
                            .changed()
                        {
                            self.shared_volume
                                .store(self.volume.to_bits(), Ordering::Relaxed);
                        }
                    });

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

        // --- 3. ЗАПИТ НА НАСТУПНИЙ КАДР ---
        // Якщо на паузі - не перемальовуємо інтерфейс 60 разів на секунду, економимо CPU!
        if self.is_playing {
            ui.ctx().request_repaint();
        }
    }
}
