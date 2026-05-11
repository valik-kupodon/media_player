use ffmpeg::format::{Pixel, Sample};
use ffmpeg::media::Type;
use ffmpeg::software::resampling::Context as AudioResampler;
use ffmpeg::software::scaling::{context::Context as VideoScaler, flag::Flags};
use ffmpeg_next as ffmpeg;
use rodio::{MixerDeviceSink, Player, buffer::SamplesBuffer};
use std::error::Error;
use std::io;
use std::num::{NonZeroU16, NonZeroU32};
use std::sync::mpsc::{SyncSender, TrySendError};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
};
use std::thread;
use std::time::{Duration, Instant};

pub struct VideoFrame {
    pub width: usize,
    pub height: usize,
    pub rgb_data: Vec<u8>,
    pub pts: f64,
    pub duration: f64,
}

pub struct MediaStreams {
    file_path: String,
    shared_volume: Arc<AtomicU32>,
    shared_paused: Arc<AtomicBool>,
    shared_seek: Arc<AtomicU64>,
}

struct VideoPlayback {
    stream_index: usize,
    time_base: ffmpeg::Rational,
    width: usize,
    height: usize,
    decoder: ffmpeg::decoder::Video,
    scaler: VideoScaler,
    decoded_frame: ffmpeg::frame::Video,
    rgb_frame: ffmpeg::frame::Video,
}

struct AudioPlayback {
    stream_index: usize,
    time_base: ffmpeg::Rational,
    decoder: ffmpeg::decoder::Audio,
    resampler: AudioResampler,
    _sink_handle: MixerDeviceSink,
    player: Player,
    decoded_frame: ffmpeg::frame::Audio,
}

impl MediaStreams {
    pub fn new(
        file_path: impl Into<String>,
        shared_volume: Arc<AtomicU32>,
        shared_paused: Arc<AtomicBool>,
        shared_seek: Arc<AtomicU64>,
    ) -> Self {
        Self {
            file_path: file_path.into(),
            shared_volume,
            shared_paused,
            shared_seek,
        }
    }

    // Головна функція тепер виглядає максимально чисто і зрозуміло!
    pub fn play_with_video_tx(
        &self,
        video_tx: SyncSender<VideoFrame>,
    ) -> Result<(), Box<dyn Error>> {
        ffmpeg::init()?;

        let mut ictx = ffmpeg::format::input(&self.file_path)?;
        let duration = ictx.duration() as f64 / ffmpeg::ffi::AV_TIME_BASE as f64;
        let mut video = Self::create_video_playback(&ictx)?;
        let mut audio = Self::create_audio_playback(&ictx)?;

        let mut master_clock: Option<(f64, Instant)> = None;
        let mut last_dummy_send = Instant::now(); // Таймер для оновлення повзунка аудіо

        loop {
            Self::wait_if_paused(audio.as_ref(), &self.shared_paused, &mut master_clock);
            Self::handle_seek(
                &mut ictx,
                &mut video,
                &mut audio,
                &self.shared_seek,
                &mut master_clock,
            );

            let mut packet = ffmpeg::Packet::empty();
            if packet.read(&mut ictx).is_err() {
                break; // Пакети закінчилися (EOF)
            }

            if let Some(v) = video.as_mut() {
                if packet.stream() == v.stream_index {
                    v.decoder.send_packet(&packet)?;
                    if !Self::drain_video_decoder(
                        v,
                        audio.as_ref(),
                        &self.shared_paused,
                        &mut master_clock,
                        &video_tx,
                        duration,
                    )? {
                        return Ok(()); // Перемкнули трек
                    }
                }
            }

            if let Some(a) = audio.as_mut() {
                if packet.stream() == a.stream_index {
                    a.decoder.send_packet(&packet)?;
                    if !Self::drain_audio_decoder(
                        a,
                        video.is_none(),
                        &self.shared_paused,
                        &self.shared_volume,
                        &mut master_clock,
                        &video_tx,
                        duration,
                        &mut last_dummy_send,
                    )? {
                        return Ok(()); // Перемкнули трек
                    }
                }
            }
        }

        // --- БЛОК ОЧИЩЕННЯ ПІСЛЯ КІНЦЯ ФАЙЛУ (EOF) ---
        if let Some(v) = video.as_mut() {
            v.decoder.send_eof()?;
            if !Self::drain_video_decoder(
                v,
                audio.as_ref(),
                &self.shared_paused,
                &mut master_clock,
                &video_tx,
                duration,
            )? {
                return Ok(());
            }
        }

        if let Some(a) = audio.as_mut() {
            a.decoder.send_eof()?;
            if !Self::drain_audio_decoder(
                a,
                video.is_none(),
                &self.shared_paused,
                &self.shared_volume,
                &mut master_clock,
                &video_tx,
                duration,
                &mut last_dummy_send,
            )? {
                return Ok(());
            }

            Self::flush_audio_resampler(
                &a.player,
                &mut a.resampler,
                f32::from_bits(self.shared_volume.load(Ordering::Relaxed)),
            )?;
            a.player.sleep_until_end();
        }

        Ok(())
    }

    // ==========================================
    // НОВІ ВИДІЛЕНІ МЕТОДИ (РЕФАКТОРИНГ)
    // ==========================================

    /// Обробляє запити на перемотування
    fn handle_seek(
        ictx: &mut ffmpeg::format::context::Input,
        video: &mut Option<VideoPlayback>,
        audio: &mut Option<AudioPlayback>,
        shared_seek: &AtomicU64,
        master_clock: &mut Option<(f64, Instant)>,
    ) {
        let seek_val = f64::from_bits(shared_seek.load(Ordering::Relaxed));
        if !seek_val.is_nan() {
            let target_ts = (seek_val * 1_000_000.0) as i64;
            let _ = ictx.seek(target_ts, ..);

            if let Some(v) = video.as_mut() {
                v.decoder.flush();
            }
            if let Some(a) = audio.as_mut() {
                a.decoder.flush();
                a.player.clear();
                a.player.play();
            }

            *master_clock = None;
            shared_seek.store(f64::NAN.to_bits(), Ordering::Relaxed);
        }
    }

    /// Вичитує кадри з відеодекодера та відправляє їх в UI
    /// Повертає `false`, якщо потік треба завершити (UI відключився)
    fn drain_video_decoder(
        video: &mut VideoPlayback,
        audio: Option<&AudioPlayback>,
        shared_paused: &AtomicBool,
        master_clock: &mut Option<(f64, Instant)>,
        video_tx: &SyncSender<VideoFrame>,
        duration: f64,
    ) -> Result<bool, Box<dyn Error>> {
        while video
            .decoder
            .receive_frame(&mut video.decoded_frame)
            .is_ok()
        {
            Self::wait_if_paused(audio, shared_paused, master_clock);
            let frame = Self::decode_video_frame(video, master_clock, duration)?;

            match video_tx.try_send(frame) {
                Ok(()) => {}
                Err(TrySendError::Full(_)) => {} // Не встигаємо - дропаємо кадр
                Err(TrySendError::Disconnected(_)) => return Ok(false), // Зв'язок розірвано
            }
        }
        Ok(true)
    }

    /// Вичитує кадри з аудіодекодера, обробляє звук і надсилає "пульс часу" для UI
    fn drain_audio_decoder(
        audio: &mut AudioPlayback,
        video_is_none: bool,
        shared_paused: &AtomicBool,
        shared_volume: &AtomicU32,
        master_clock: &mut Option<(f64, Instant)>,
        video_tx: &SyncSender<VideoFrame>,
        duration: f64,
        last_dummy_send: &mut Instant,
    ) -> Result<bool, Box<dyn Error>> {
        while audio
            .decoder
            .receive_frame(&mut audio.decoded_frame)
            .is_ok()
        {
            Self::wait_if_paused(Some(audio), shared_paused, master_clock);

            let audio_pts = audio
                .decoded_frame
                .timestamp()
                .map(|ts| ts as f64 * f64::from(audio.time_base))
                .unwrap_or(0.0);

            if master_clock.is_none() {
                *master_clock = Some((audio_pts, Instant::now()));
            }

            // Відправляємо час в UI, щоб повзунок рухався (навіть якщо це обкладинка альбому)
            if video_is_none || last_dummy_send.elapsed() > Duration::from_millis(100) {
                let dummy_frame = VideoFrame {
                    width: 0,
                    height: 0,
                    rgb_data: vec![],
                    pts: audio_pts,
                    duration,
                };
                match video_tx.try_send(dummy_frame) {
                    Ok(()) | Err(TrySendError::Full(_)) => *last_dummy_send = Instant::now(),
                    Err(TrySendError::Disconnected(_)) => return Ok(false),
                }
            }

            let current_volume = f32::from_bits(shared_volume.load(Ordering::Relaxed));
            Self::append_audio_frame(
                &audio.player,
                &mut audio.resampler,
                &audio.decoded_frame,
                current_volume,
            )?;

            // Гальма для синхронізації (щоб не розкодувати весь файл за секунду)
            if let Some((start_pts, start_time)) = master_clock {
                let actual_elapsed = start_time.elapsed().as_secs_f64();
                let target_elapsed = audio_pts - *start_pts;

                if target_elapsed > actual_elapsed + 1.0 {
                    let delay = target_elapsed - actual_elapsed - 1.0;
                    thread::sleep(Duration::from_secs_f64(delay.min(0.05)));
                }
            }
        }
        Ok(true)
    }

    // ==========================================
    // СТАРІ МЕТОДИ (БЕЗ ЗМІН)
    // ==========================================

    fn create_video_playback(
        ictx: &ffmpeg::format::context::Input,
    ) -> Result<Option<VideoPlayback>, Box<dyn Error>> {
        let Some(input) = ictx.streams().best(Type::Video) else {
            return Ok(None);
        };

        let stream_index = input.index();
        let time_base = input.time_base();
        let context_decoder = ffmpeg::codec::context::Context::from_parameters(input.parameters())?;
        let decoder = context_decoder.decoder().video()?;
        let width = decoder.width() as usize;
        let height = decoder.height() as usize;

        let scaler = VideoScaler::get(
            decoder.format(),
            width as u32,
            height as u32,
            Pixel::RGB24,
            width as u32,
            height as u32,
            Flags::BILINEAR,
        )?;

        Ok(Some(VideoPlayback {
            stream_index,
            time_base,
            width,
            height,
            decoder,
            scaler,
            decoded_frame: ffmpeg::frame::Video::empty(),
            rgb_frame: ffmpeg::frame::Video::empty(),
        }))
    }

    fn create_audio_playback(
        ictx: &ffmpeg::format::context::Input,
    ) -> Result<Option<AudioPlayback>, Box<dyn Error>> {
        let Some(input) = ictx.streams().best(Type::Audio) else {
            return Ok(None);
        };

        let stream_index = input.index();
        let context_decoder = ffmpeg::codec::context::Context::from_parameters(input.parameters())?;
        let decoder = context_decoder.decoder().audio()?;

        let channel_layout = if decoder.channel_layout().is_empty() {
            ffmpeg::ChannelLayout::default(i32::from(decoder.channels()))
        } else {
            decoder.channel_layout()
        };

        let resampler = AudioResampler::get(
            decoder.format(),
            channel_layout,
            decoder.rate(),
            Sample::F32(ffmpeg::format::sample::Type::Packed),
            channel_layout,
            decoder.rate(),
        )?;

        let mut sink_handle = rodio::DeviceSinkBuilder::open_default_sink()?;
        sink_handle.log_on_drop(false);
        let player = Player::connect_new(sink_handle.mixer());

        Ok(Some(AudioPlayback {
            stream_index,
            time_base: input.time_base(),
            decoder,
            resampler,
            _sink_handle: sink_handle,
            player,
            decoded_frame: ffmpeg::frame::Audio::empty(),
        }))
    }

    fn decode_video_frame(
        video: &mut VideoPlayback,
        master_clock: &mut Option<(f64, Instant)>,
        duration: f64,
    ) -> Result<VideoFrame, Box<dyn Error>> {
        let pts = video
            .decoded_frame
            .timestamp()
            .map(|ts| ts as f64 * f64::from(video.time_base))
            .unwrap_or(0.0);

        Self::sync_video(pts, master_clock);

        video
            .scaler
            .run(&video.decoded_frame, &mut video.rgb_frame)?;

        Ok(Self::copy_rgb_frame(
            &video.rgb_frame,
            video.width,
            video.height,
            pts,
            duration,
        ))
    }

    fn copy_rgb_frame(
        frame: &ffmpeg::frame::Video,
        width: usize,
        height: usize,
        pts: f64,
        duration: f64,
    ) -> VideoFrame {
        let stride = frame.stride(0);
        let src = frame.data(0);
        let row_len = width * 3;

        let mut rgb_data = Vec::with_capacity(width * height * 3);
        for y in 0..height {
            let start = y * stride;
            rgb_data.extend_from_slice(&src[start..start + row_len]);
        }

        VideoFrame {
            width,
            height,
            rgb_data,
            pts,
            duration,
        }
    }

    fn sync_video(pts: f64, master_clock: &mut Option<(f64, Instant)>) {
        let (origin_pts, start) = master_clock.get_or_insert((pts, Instant::now()));
        let target = Duration::from_secs_f64((pts - *origin_pts).max(0.0));
        let elapsed = start.elapsed();

        if target > elapsed {
            thread::sleep(target - elapsed);
        }
    }

    fn wait_if_paused(
        audio: Option<&AudioPlayback>,
        shared_paused: &AtomicBool,
        master_clock: &mut Option<(f64, Instant)>,
    ) {
        let mut was_paused = false;
        let pause_start = Instant::now();
        while shared_paused.load(Ordering::SeqCst) {
            if !was_paused {
                if let Some(audio) = audio {
                    audio.player.pause();
                }
                was_paused = true;
            }
            thread::sleep(Duration::from_millis(10));
        }

        if was_paused {
            if let Some(audio) = audio {
                audio.player.play();
            }
            if let Some((_, clock_instant)) = master_clock {
                *clock_instant = *clock_instant + pause_start.elapsed();
            }
        }
    }

    fn append_audio_frame(
        player: &Player,
        resampler: &mut AudioResampler,
        decoded_frame: &ffmpeg::frame::Audio,
        currnet_volume: f32,
    ) -> Result<(), Box<dyn Error>> {
        let mut resampled_frame = ffmpeg::frame::Audio::empty();
        resampler.run(decoded_frame, &mut resampled_frame)?;
        Self::queue_audio_buffer(player, &resampled_frame, currnet_volume)
    }

    fn queue_audio_buffer(
        player: &Player,
        audio_frame: &ffmpeg::frame::Audio,
        current_volume: f32,
    ) -> Result<(), Box<dyn Error>> {
        let channels = NonZeroU16::new(audio_frame.channels()).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "audio frame has zero channels")
        })?;
        let sample_rate = NonZeroU32::new(audio_frame.rate()).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "audio frame has zero sample rate",
            )
        })?;

        let samples = audio_frame
            .data(0)
            .chunks_exact(std::mem::size_of::<f32>())
            .map(|chunk| {
                let raw_sample =
                    f32::from_ne_bytes(chunk.try_into().expect("f32 chunks are sized"));
                raw_sample * current_volume
            })
            .collect::<Vec<_>>();

        if !samples.is_empty() {
            player.append(SamplesBuffer::new(channels, sample_rate, samples));
        }

        Ok(())
    }

    fn flush_audio_resampler(
        player: &Player,
        resampler: &mut AudioResampler,
        current_volume: f32,
    ) -> Result<(), Box<dyn Error>> {
        while let Some(delay) = resampler.delay() {
            if delay.output <= 0 {
                break;
            }

            let output = resampler.output();
            let mut delayed_frame = ffmpeg::frame::Audio::new(
                output.format,
                delay.output as usize,
                output.channel_layout,
            );
            delayed_frame.set_rate(output.rate);

            resampler.flush(&mut delayed_frame)?;

            if delayed_frame.samples() == 0 {
                break;
            }

            Self::queue_audio_buffer(player, &delayed_frame, current_volume)?;
        }

        Ok(())
    }
}
