use ffmpeg::format::Pixel;
use ffmpeg::media::Type;
use ffmpeg::software::scaling::{context::Context, flag::Flags};
use ffmpeg_next as ffmpeg;
use minifb::{Key, Window, WindowOptions};
use std::thread;
use std::time::{Duration, Instant};

pub fn play_custom_video(file_path: &str) -> Result<(), Box<dyn std::error::Error>> {
    println!("🎬 Ініціалізую рушій FFmpeg...");
    ffmpeg::init()?;

    let mut ictx = ffmpeg::format::input(&file_path)?;
    let input = ictx
        .streams()
        .best(Type::Video)
        .ok_or(ffmpeg::Error::StreamNotFound)?;
    let video_stream_index = input.index();

    let context_decoder = ffmpeg::codec::context::Context::from_parameters(input.parameters())?;
    let mut decoder = context_decoder.decoder().video()?;

    let width = decoder.width() as usize;
    let height = decoder.height() as usize;

    let mut scaler = Context::get(
        decoder.format(),
        width as u32,
        height as u32,
        Pixel::RGB24,
        width as u32,
        height as u32,
        Flags::BILINEAR,
    )?;

    // ==========================================
    // 1. СТВОРЮЄМО ВІКНО MINIFB
    // ==========================================
    let mut window = Window::new(
        "Відеоплеєр",
        width,
        height,
        WindowOptions::default(),
    )?;

    // Це наш буфер екрана (одне число u32 = один піксель)
    let mut window_buffer: Vec<u32> = vec![0; width * height];

    let mut decoded_frame = ffmpeg::frame::Video::empty();
    let mut rgb_frame = ffmpeg::frame::Video::empty();

    println!("▶ Відтворюємо: {} ({}x{})", file_path, width, height);

    // ==========================================
    // 2. ГОЛОВНИЙ ЦИКЛ ВІДТВОРЕННЯ
    // ==========================================
    for (stream, packet) in ictx.packets() {
        // Якщо користувач закрив вікно або натиснув ESC - виходимо
        if !window.is_open() || window.is_key_down(Key::Escape) {
            break;
        }

        if stream.index() == video_stream_index {
            decoder.send_packet(&packet)?;

            while decoder.receive_frame(&mut decoded_frame).is_ok() {
                // Засікаємо час початку обробки кадру
                let start_time = Instant::now();

                // Конвертуємо формат FFmpeg (зазвичай YUV) у масив RGB-байтів
                scaler.run(&decoded_frame, &mut rgb_frame)?;
                let data = rgb_frame.data(0); // Отримуємо сирі байти

                // Магія бітових зсувів: RGB (3 байти) -> ARGB u32 (1 число)
                for (i, pixel) in window_buffer.iter_mut().enumerate() {
                    let r = data[i * 3] as u32;
                    let g = data[i * 3 + 1] as u32;
                    let b = data[i * 3 + 2] as u32;

                    // Пакуємо: 0xFF (Alpha) | Червоний | Зелений | Синій
                    *pixel = (255 << 24) | (r << 16) | (g << 8) | b;
                }

                // Виводимо наш масив пікселів у вікно
                window.update_with_buffer(&window_buffer, width, height)?;

                // ==========================================
                // 3. НАЇВНИЙ КОНТРОЛЬ ШВИДКОСТІ (FRAMELIMIT)
                // ==========================================
                // Якщо ми відмалювали кадр швидше ніж за 33мс (~30 FPS),
                // присипляємо потік, щоб відео не летіло на швидкості 1000 FPS
                let elapsed = start_time.elapsed();
                let frame_target = Duration::from_millis(33);
                if elapsed < frame_target {
                    thread::sleep(frame_target - elapsed);
                }
            }
        }
    }

    println!("✅ Відтворення завершено.");
    Ok(())
}

fn main() {
    let video_path = "/home/valentinef/my_home/v/348548_h"; // Вкажіть шлях до вашого відеофайлу
    if let Err(e) = play_custom_video(video_path) {
        eprintln!("Помилка при відтворенні відео: {}", e);
    }
    println!("Hello, world!");
}
