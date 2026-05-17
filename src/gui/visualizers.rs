// ==========================================
// ВІЗУАЛІЗАТОРИ (АБСТРАКЦІЯ ТА РЕАЛІЗАЦІЯ)
// ==========================================

pub trait AudioVisualizer: Send {
    /// Аналізує сирі аудіодані (наприклад, для розрахунку басу)
    fn analyze_audio(&mut self, samples: &[f32]);
    /// Генерує кадр на основі часу та проаналізованого звуку
    fn render(&self, pts: f64, width: usize, height: usize) -> Vec<u8>;
}

pub struct PlasmaVisualizer {
    smoothed_bass: f32,
}

impl PlasmaVisualizer {
    pub fn new() -> Self {
        Self { smoothed_bass: 0.0 }
    }
}

impl AudioVisualizer for PlasmaVisualizer {
    fn analyze_audio(&mut self, samples: &[f32]) {
        let mut lp = 0.0;
        let alpha = 0.05;
        let mut bass_energy = 0.0;

        for &sample in samples {
            lp += alpha * (sample - lp);
            bass_energy += lp * lp;
        }

        let bass_rms = if samples.is_empty() {
            0.0
        } else {
            (bass_energy / samples.len() as f32).sqrt()
        };

        // Згладжування басу (Attack / Release)
        if bass_rms > self.smoothed_bass {
            self.smoothed_bass = bass_rms; // Attack
        } else {
            self.smoothed_bass = self.smoothed_bass * 0.85 + bass_rms * 0.15; // Release
        }
    }

    #[inline]
    fn render(&self, pts: f64, width: usize, height: usize) -> Vec<u8> {
        let mut rgb_data = Vec::with_capacity(width * height * 3);
        let t = pts as f32;

        let pulse = (self.smoothed_bass * 40.0).min(1.5);

        // Коригуємо пропорції, щоб тунель був ідеально круглим, а не овальним
        let aspect = width as f32 / height as f32;

        for y in 0..height {
            // Нормалізуємо Y від -0.5 до 0.5
            let fy = (y as f32 / height as f32) - 0.5;

            for x in 0..width {
                // Нормалізуємо X з урахуванням пропорцій екрану
                let fx = ((x as f32 / width as f32) - 0.5) * aspect;

                // 1. ПОЛЯРНІ КООРДИНАТИ
                let dist = (fx * fx + fy * fy).sqrt();
                let angle = fy.atan2(fx);

                // 2. ІЛЮЗІЯ 3D-ГЛИБИНИ
                // Чим ближче до центру (dist ~ 0), тим далі "стіна" (z -> ∞)
                // Додаємо 0.01, щоб уникнути ділення на нуль в самому центрі
                let z = 1.0 / (dist + 0.01);

                // 3. РУХ У ТУНЕЛІ
                // u = рух вперед. Додаємо бас, щоб "пірнати" швидше під час удару!
                let u = z + t * 5.0 + pulse;
                // v = обертання стін (залежить від кута і плавно крутиться з часом)
                let v = (angle * 3.0) + t * 2.0;

                // 4. ВІЗЕРУНОК СТІН (Сітка або Фрактал)
                // Змішуємо синуси u та v, щоб створити абстрактні квадрати/ромби
                let pattern = ((u * 3.14).sin() * (v * 3.14).cos()).abs();

                // 5. ЗАТЕМНЕННЯ (Світло в кінці тунелю)
                // dist зменшується до центру, тому центр буде чорним
                let shade = (dist * 2.5).clamp(0.0, 1.0);

                // 6. ПЛАВНА ВЕСЕЛКА (Зсув фаз)
                // Колір залежить від часу та глибини (z)
                let color_phase = t * 1.0 + z * 0.2;

                // Зсуваємо синусоїду на 120 градусів для RGB, щоб отримати ідеальну веселку
                let r_base = (color_phase).sin();
                let g_base = (color_phase + 2.094).sin(); // 2PI/3
                let b_base = (color_phase + 4.188).sin(); // 4PI/3

                // 7. ЗБИРАЄМО ПІКСЕЛЬ
                let r = ((r_base * 0.5 + 0.5) * pattern * shade * 255.0) as u8;
                let g = ((g_base * 0.5 + 0.5) * pattern * shade * 255.0) as u8;
                let b = ((b_base * 0.5 + 0.5) * pattern * shade * 255.0) as u8;

                rgb_data.push(r);
                rgb_data.push(g);
                rgb_data.push(b);
            }
        }
        rgb_data
    }
}

pub struct EqualizerVisualizer {
    bars: [f32; 32], // 32 стовпчики частот
}

impl EqualizerVisualizer {
    pub fn new() -> Self {
        Self { bars: [0.0; 32] }
    }
}

impl AudioVisualizer for EqualizerVisualizer {
    fn analyze_audio(&mut self, samples: &[f32]) {
        let num_bars = self.bars.len();
        let window_size = samples.len().min(512);
        if window_size == 0 {
            return;
        }

        for k in 0..num_bars {
            let freq_bin = 1.0 + (k as f32).powf(1.4);

            let mut re = 0.0_f32;
            let mut im = 0.0_f32;

            for (n, &sample) in samples.iter().take(window_size).enumerate() {
                let window = 0.5
                    * (1.0
                        - (2.0 * std::f32::consts::PI * n as f32 / (window_size - 1) as f32).cos());
                let windowed_sample = sample * window;

                let angle =
                    2.0 * std::f32::consts::PI * freq_bin * (n as f32) / (window_size as f32);
                re += windowed_sample * angle.cos();
                im -= windowed_sample * angle.sin();
            }

            // 1. Нормалізуємо амплітуду (щоб вона не залежала від розміру вікна)
            let magnitude = (re * re + im * im).sqrt() / (window_size as f32 / 2.0);

            // 2. Переводимо в Логарифмічну шкалу (Децибели)
            // Додаємо 1e-6, щоб уникнути помилки log10(0) при абсолютній тиші
            let db = 20.0 * (magnitude + 1e-6).log10();

            // 3. Мапимо децибели у висоту 0.0 .. 1.0
            // Звук нижче -50 dB вважаємо тишею, а 0 dB - максимальним перевантаженням
            let min_db = -50.0;
            let max_db = 0.0;
            let mut target_height = ((db - min_db) / (max_db - min_db)).clamp(0.0, 1.0);

            // Трохи підсилюємо високі частоти (справа), бо в них від природи менше енергії, ніж у басах
            let eq_boost = 1.0 + (k as f32 / num_bars as f32) * 0.5;
            target_height = (target_height * eq_boost).clamp(0.0, 1.0);

            // 4. Фізика падіння (Гравітація)
            if target_height > self.bars[k] {
                // Швидка атака (Attack), але не миттєва, щоб уникнути стробоскопу
                self.bars[k] = self.bars[k] * 0.4 + target_height * 0.6;
            } else {
                // Плавне падіння (Release)
                self.bars[k] = self.bars[k] * 0.88;
            }
        }
    }

    #[inline]
    fn render(&self, _pts: f64, width: usize, height: usize) -> Vec<u8> {
        // Для еквалайзера краще малювати прямокутники, тому заливаємо все чорним фоном
        let mut rgb_data = vec![15; width * height * 3]; // Темно-сірий фон

        let num_bars = self.bars.len();
        let bar_width = width / num_bars;
        let gap = 2; // Відстань між стовпчиками у пікселях

        for (i, &val) in self.bars.iter().enumerate() {
            // Рахуємо висоту конкретного стовпчика у пікселях
            let bar_h = (val * height as f32) as usize;
            let bar_h = bar_h.clamp(0, height);

            let start_x = i * bar_width + gap;
            let end_x = (start_x + bar_width - gap).min(width);

            // Малюємо сам стовпчик знизу вгору
            for y in (height - bar_h)..height {
                // Робимо класичний градієнт: Зелений знизу -> Жовтий -> Червоний зверху
                let intensity = 1.0 - (y as f32 / height as f32);

                let r = (intensity * 255.0 * 2.0).clamp(0.0, 255.0) as u8;
                let g = ((1.0 - intensity) * 255.0 * 1.5).clamp(0.0, 255.0) as u8;
                let b = 30; // Легкий синуватий відтінок

                for x in start_x..end_x {
                    let idx = (y * width + x) * 3;
                    rgb_data[idx] = r;
                    rgb_data[idx + 1] = g;
                    rgb_data[idx + 2] = b;
                }
            }
        }
        rgb_data
    }
}
