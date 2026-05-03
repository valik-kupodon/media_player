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
    shared_seek: Arc<AtomicU64>, // Використовуємо AtomicU32 для зберігання бітів f64
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

    pub fn play_with_video_tx(
        &self,
        video_tx: SyncSender<VideoFrame>,
    ) -> Result<(), Box<dyn Error>> {
        ffmpeg::init()?;

        let mut ictx = ffmpeg::format::input(&self.file_path)?;
        let duration = ictx.duration() as f64 / ffmpeg::ffi::AV_TIME_BASE as f64;
        let mut video = Self::create_video_playback(&ictx)?;
        let mut audio = Self::create_audio_playback(&ictx)?;

        let mut video_clock = None;

        loop {
            Self::wait_if_paused(audio.as_ref(), &self.shared_paused, &mut video_clock);
            let seek_val = f64::from_bits(self.shared_seek.load(Ordering::Relaxed));
            if !seek_val.is_nan() {
                // FFmpeg оперує мікросекундами (AV_TIME_BASE = 1_000_000)
                let target_ts = (seek_val * 1_000_000.0) as i64;

                // Виконуємо стрибок до найближчого ключового кадру
                let _ = ictx.seek(target_ts, ..);

                // ОЧИЩАЄМО БУФЕРИ ДЕКОДЕРІВ (інакше на екран вилізуть старі кадри)
                if let Some(v) = video.as_mut() {
                    v.decoder.flush();
                }
                if let Some(a) = audio.as_mut() {
                    a.decoder.flush();
                }

                // КРИТИЧНО ВАЖЛИВО: скидаємо годинник, щоб відео не чекало старого часу
                video_clock = None;

                // Скидаємо прапорець назад у "Вимкнено"
                self.shared_seek
                    .store(f64::NAN.to_bits(), Ordering::Relaxed);
            }

            // --- 2. ВРУЧНУ ЧИТАЄМО ПАКЕТ ---
            let mut packet = ffmpeg::Packet::empty();
            if packet.read(&mut ictx).is_err() {
                break; // Пакети закінчилися (EOF)
            }
            if let Some(video) = video.as_mut()
                && packet.stream() == video.stream_index
            {
                video.decoder.send_packet(&packet)?;

                while video
                    .decoder
                    .receive_frame(&mut video.decoded_frame)
                    .is_ok()
                {
                    Self::wait_if_paused(audio.as_ref(), &self.shared_paused, &mut video_clock);
                    let frame = Self::decode_video_frame(video, &mut video_clock, duration)?;

                    match video_tx.try_send(frame) {
                        Ok(()) => {}
                        Err(TrySendError::Full(_)) => {}
                        Err(TrySendError::Disconnected(_)) => return Ok(()),
                    }
                }
            }

            if let Some(audio) = audio.as_mut()
                && packet.stream() == audio.stream_index
            {
                audio.decoder.send_packet(&packet)?;

                while audio
                    .decoder
                    .receive_frame(&mut audio.decoded_frame)
                    .is_ok()
                {
                    Self::wait_if_paused(Some(audio), &self.shared_paused, &mut video_clock);
                    let current_volume = f32::from_bits(self.shared_volume.load(Ordering::Relaxed));
                    Self::append_audio_frame(
                        &audio.player,
                        &mut audio.resampler,
                        &audio.decoded_frame,
                        current_volume,
                    )?;
                }
            }
        }

        if let Some(video) = video.as_mut() {
            video.decoder.send_eof()?;
            while video
                .decoder
                .receive_frame(&mut video.decoded_frame)
                .is_ok()
            {
                Self::wait_if_paused(audio.as_ref(), &self.shared_paused, &mut video_clock);
                let frame = Self::decode_video_frame(video, &mut video_clock, duration)?;
                match video_tx.try_send(frame) {
                    Ok(()) => {}
                    Err(TrySendError::Full(_)) => {}
                    Err(TrySendError::Disconnected(_)) => return Ok(()),
                }
            }
        }

        if let Some(audio) = audio.as_mut() {
            audio.decoder.send_eof()?;
            while audio
                .decoder
                .receive_frame(&mut audio.decoded_frame)
                .is_ok()
            {
                Self::wait_if_paused(Some(audio), &self.shared_paused, &mut video_clock);
                Self::append_audio_frame(
                    &audio.player,
                    &mut audio.resampler,
                    &audio.decoded_frame,
                    f32::from_bits(self.shared_volume.load(Ordering::Relaxed)),
                )?;
            }

            Self::flush_audio_resampler(
                &audio.player,
                &mut audio.resampler,
                f32::from_bits(self.shared_volume.load(Ordering::Relaxed)),
            )?;
            audio.player.sleep_until_end();
        }

        Ok(())
    }

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
            decoder,
            resampler,
            _sink_handle: sink_handle,
            player,
            decoded_frame: ffmpeg::frame::Audio::empty(),
        }))
    }

    fn decode_video_frame(
        video: &mut VideoPlayback,
        video_clock: &mut Option<(f64, Instant)>,
        duration: f64,
    ) -> Result<VideoFrame, Box<dyn Error>> {
        let pts = video
            .decoded_frame
            .timestamp()
            .map(|ts| ts as f64 * f64::from(video.time_base))
            .unwrap_or(0.0);

        Self::sync_video(pts, video_clock);

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

    fn sync_video(pts: f64, video_clock: &mut Option<(f64, Instant)>) {
        let (origin_pts, start) = video_clock.get_or_insert((pts, Instant::now()));
        let target = Duration::from_secs_f64((pts - *origin_pts).max(0.0));
        let elapsed = start.elapsed();

        if target > elapsed {
            thread::sleep(target - elapsed);
        }
    }

    fn wait_if_paused(
        audio: Option<&AudioPlayback>,
        shared_paused: &AtomicBool,
        video_clock: &mut Option<(f64, Instant)>,
    ) {
        let mut was_paused = false;

        while shared_paused.load(Ordering::Relaxed) {
            if let Some(audio) = audio
                && !audio.player.is_paused()
            {
                audio.player.pause();
            }

            was_paused = true;
            thread::sleep(Duration::from_millis(10));
        }

        if let Some(audio) = audio
            && audio.player.is_paused()
        {
            audio.player.play();
        }

        if was_paused {
            *video_clock = None;
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
