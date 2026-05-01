use ffmpeg::format::{Pixel, Sample};
use ffmpeg::media::Type;
use ffmpeg::software::resampling::Context as AudioResampler;
use ffmpeg::software::scaling::{context::Context as VideoScaler, flag::Flags};
use ffmpeg_next as ffmpeg;
use minifb::{Key, Window, WindowOptions};
use rodio::{MixerDeviceSink, Player, buffer::SamplesBuffer};
use std::error::Error;
use std::io;
use std::num::{NonZeroU16, NonZeroU32};
use std::thread;
use std::time::{Duration, Instant};

pub struct MediaStreams {
    file_path: String,
}

struct VideoPlayback {
    stream_index: usize,
    time_base: ffmpeg::Rational,
    decoder: ffmpeg::decoder::Video,
    scaler: VideoScaler,
    window: Window,
    window_buffer: Vec<u32>,
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
    pub fn new(file_path: impl Into<String>) -> Self {
        Self {
            file_path: file_path.into(),
        }
    }

    pub fn play(&self) -> Result<(), Box<dyn Error>> {
        println!("🎬🔊 Ініціалізую медіапотік...");
        ffmpeg::init()?;

        let mut ictx = ffmpeg::format::input(&self.file_path)?;
        let mut video = Self::create_video_playback(&ictx)?;
        let mut audio = Self::create_audio_playback(&ictx)?;

        if video.is_none() && audio.is_none() {
            return Err(Box::new(ffmpeg::Error::StreamNotFound));
        }

        if let Some(video) = video.as_ref() {
            println!(
                "▶ Відтворюємо: {} ({}x{})",
                self.file_path,
                video.window_buffer.len() / video.window.get_size().1,
                video.window.get_size().1
            );
        } else {
            println!("▶ Відтворюємо аудіо: {}", self.file_path);
        }

        let mut interrupted = false;
        let mut video_clock = None;

        for (stream, packet) in ictx.packets() {
            if let Some(video) = video.as_mut() {
                if !video.window.is_open() || video.window.is_key_down(Key::Escape) {
                    interrupted = true;
                    break;
                }
            }

            if let Some(video) = video.as_mut()
                && stream.index() == video.stream_index
            {
                video.decoder.send_packet(&packet)?;

                while video
                    .decoder
                    .receive_frame(&mut video.decoded_frame)
                    .is_ok()
                {
                    Self::render_video_frame(video, &mut video_clock)?;
                }
            }

            if let Some(audio) = audio.as_mut()
                && stream.index() == audio.stream_index
            {
                audio.decoder.send_packet(&packet)?;

                while audio
                    .decoder
                    .receive_frame(&mut audio.decoded_frame)
                    .is_ok()
                {
                    Self::append_audio_frame(
                        &audio.player,
                        &mut audio.resampler,
                        &audio.decoded_frame,
                    )?;
                }
            }
        }

        if !interrupted {
            if let Some(video) = video.as_mut() {
                video.decoder.send_eof()?;
                while video
                    .decoder
                    .receive_frame(&mut video.decoded_frame)
                    .is_ok()
                {
                    Self::render_video_frame(video, &mut video_clock)?;
                }
            }

            if let Some(audio) = audio.as_mut() {
                audio.decoder.send_eof()?;
                while audio
                    .decoder
                    .receive_frame(&mut audio.decoded_frame)
                    .is_ok()
                {
                    Self::append_audio_frame(
                        &audio.player,
                        &mut audio.resampler,
                        &audio.decoded_frame,
                    )?;
                }

                Self::flush_audio_resampler(&audio.player, &mut audio.resampler)?;
                audio.player.sleep_until_end();
            }
        }

        println!("✅ Відтворення завершено.");
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

        let window = Window::new(
            "Mедіа плеер Валентина",
            width,
            height,
            WindowOptions::default(),
        )?;

        Ok(Some(VideoPlayback {
            stream_index,
            time_base,
            decoder,
            scaler,
            window,
            window_buffer: vec![0; width * height],
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

    fn render_video_frame(
        video: &mut VideoPlayback,
        video_clock: &mut Option<(f64, Instant)>,
    ) -> Result<(), Box<dyn Error>> {
        Self::sync_video(
            video.decoded_frame.timestamp(),
            video.time_base,
            video_clock,
        );

        video
            .scaler
            .run(&video.decoded_frame, &mut video.rgb_frame)?;
        let data = video.rgb_frame.data(0);

        for (i, pixel) in video.window_buffer.iter_mut().enumerate() {
            let r = data[i * 3] as u32;
            let g = data[i * 3 + 1] as u32;
            let b = data[i * 3 + 2] as u32;
            *pixel = (255 << 24) | (r << 16) | (g << 8) | b;
        }

        let (width, height) = video.window.get_size();
        video
            .window
            .update_with_buffer(&video.window_buffer, width, height)?;

        Ok(())
    }

    fn sync_video(
        timestamp: Option<i64>,
        time_base: ffmpeg::Rational,
        video_clock: &mut Option<(f64, Instant)>,
    ) {
        let Some(timestamp) = timestamp else {
            thread::sleep(Duration::from_millis(33));
            return;
        };

        let seconds = timestamp as f64 * f64::from(time_base);
        let (origin_seconds, playback_start) = video_clock.get_or_insert((seconds, Instant::now()));
        let target = Duration::from_secs_f64((seconds - *origin_seconds).max(0.0));
        let elapsed = playback_start.elapsed();

        if target > elapsed {
            thread::sleep(target - elapsed);
        }
    }

    fn append_audio_frame(
        player: &Player,
        resampler: &mut AudioResampler,
        decoded_frame: &ffmpeg::frame::Audio,
    ) -> Result<(), Box<dyn Error>> {
        let mut resampled_frame = ffmpeg::frame::Audio::empty();
        resampler.run(decoded_frame, &mut resampled_frame)?;
        Self::queue_audio_buffer(player, &resampled_frame)
    }

    fn queue_audio_buffer(
        player: &Player,
        audio_frame: &ffmpeg::frame::Audio,
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
            .map(|chunk| f32::from_ne_bytes(chunk.try_into().expect("f32 chunks are sized")))
            .collect::<Vec<_>>();

        if samples.is_empty() {
            return Ok(());
        }

        player.append(SamplesBuffer::new(channels, sample_rate, samples));
        Ok(())
    }

    fn flush_audio_resampler(
        player: &Player,
        resampler: &mut AudioResampler,
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

            Self::queue_audio_buffer(player, &delayed_frame)?;
        }

        Ok(())
    }
}
