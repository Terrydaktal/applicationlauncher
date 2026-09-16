//! Fixed-memory stereo filter bank. Raw PCM is consumed and discarded on the
//! audio worker; neither raw samples nor a reconstructable waveform leave it.

use crate::models::AudioVisualization;

pub(super) const SAMPLE_RATE: u32 = 16_000;
pub(super) const FRAME_BYTES: usize = 8; // Two native-endian f32 channels.
const HOP_FRAMES: usize = SAMPLE_RATE as usize / 50;
const FREQUENCIES: [f32; 8] = [60.0, 120.0, 250.0, 500.0, 1000.0, 2000.0, 4000.0, 6500.0];
const MIN_REFERENCE_RMS: f32 = 0.04;

#[derive(Clone, Copy, Default)]
struct Band {
    b0: f32,
    a1: f32,
    a2: f32,
    z1: [f32; 2],
    z2: [f32; 2],
}

impl Band {
    fn new(frequency: f32) -> Self {
        // Constant-peak-gain biquad bandpass, Q=1.2. Separate channel state avoids
        // cancelling stereo signals whose left/right channels have opposite phase.
        let omega = std::f32::consts::TAU * frequency / SAMPLE_RATE as f32;
        let alpha = omega.sin() / 2.4;
        let denominator = 1.0 + alpha;
        Self {
            b0: alpha / denominator,
            a1: -2.0 * omega.cos() / denominator,
            a2: (1.0 - alpha) / denominator,
            ..Self::default()
        }
    }

    fn sample(&mut self, input: f32, channel: usize) -> f32 {
        let output = self.b0 * input + self.z1[channel];
        self.z1[channel] = self.z2[channel] - self.a1 * output;
        self.z2[channel] = -self.b0 * input - self.a2 * output;
        // Bound denormal work when an application retains a silent output stream.
        if self.z1[channel].abs() < 1e-20 {
            self.z1[channel] = 0.0;
        }
        if self.z2[channel].abs() < 1e-20 {
            self.z2[channel] = 0.0;
        }
        output
    }
}

pub(super) struct Analyzer {
    filters: [Band; 8],
    energy: [f32; 8],
    frames: usize,
    peak: f32,
    reference_rms: f32,
    baseline: [f32; 8],
    envelope: [f32; 8],
    visual: AudioVisualization,
}

impl Default for Analyzer {
    fn default() -> Self {
        Self {
            filters: FREQUENCIES.map(Band::new),
            energy: [0.0; 8],
            frames: 0,
            peak: 0.0,
            reference_rms: MIN_REFERENCE_RMS,
            baseline: [0.0; 8],
            envelope: [0.0; 8],
            visual: AudioVisualization::default(),
        }
    }
}

impl Analyzer {
    pub(super) fn push_pcm(&mut self, bytes: &[u8]) {
        for frame in bytes.chunks_exact(FRAME_BYTES) {
            let left = f32::from_ne_bytes(frame[..4].try_into().unwrap());
            let right = f32::from_ne_bytes(frame[4..8].try_into().unwrap());
            self.push_frame(left, right);
        }
    }

    fn push_frame(&mut self, left: f32, right: f32) {
        let samples = [left, right].map(|value| {
            if value.is_finite() {
                value.clamp(-4.0, 4.0)
            } else {
                0.0
            }
        });
        self.peak = self.peak.max(samples[0].abs()).max(samples[1].abs());
        for (band, energy) in self.filters.iter_mut().zip(&mut self.energy) {
            let left = band.sample(samples[0], 0);
            let right = band.sample(samples[1], 1);
            *energy += (left * left + right * right) * 0.5;
        }
        self.frames += 1;
        if self.frames == HOP_FRAMES {
            self.visual.peak = display_level(self.peak);
            let rms = self
                .energy
                .map(|energy| (energy / HOP_FRAMES as f32).sqrt());
            // Share a slowly decaying reference across bands so bass/treble retain
            // their relative strengths. The floor prevents boosting near-silence.
            self.reference_rms = rms.iter().copied().fold(
                (self.reference_rms * 0.995).max(MIN_REFERENCE_RMS),
                f32::max,
            );
            for index in 0..8 {
                // Positive change relative to ~250 ms of recent energy emphasises
                // real drum hits and note attacks, never a synthetic time oscillator.
                let onset = (rms[index] - self.baseline[index]).max(0.0);
                let level =
                    ((0.75 * rms[index] + 1.5 * onset) / self.reference_rms).clamp(0.0, 1.0);
                self.baseline[index] += (rms[index] - self.baseline[index]) * 0.08;
                self.envelope[index] = level.max(self.envelope[index] * 0.55);
                self.visual.bands[index] = (100.0 * self.envelope[index]).round() as u8;
            }
            // Silence must stop the animation, even if filter/envelope tails remain.
            if self.visual.peak == 0 {
                self.visual = AudioVisualization::default();
                self.envelope.fill(0.0);
            }
            self.frames = 0;
            self.peak = 0.0;
            self.energy.fill(0.0);
        }
    }

    pub(super) fn visual(&self) -> AudioVisualization {
        self.visual
    }

    pub(super) fn reset(&mut self) {
        for filter in &mut self.filters {
            filter.z1.fill(0.0);
            filter.z2.fill(0.0);
        }
        self.energy.fill(0.0);
        self.reference_rms = MIN_REFERENCE_RMS;
        self.baseline.fill(0.0);
        self.envelope.fill(0.0);
        self.frames = 0;
        self.peak = 0.0;
        self.visual = AudioVisualization::default();
    }
}

fn display_level(amplitude: f32) -> u8 {
    if amplitude <= 0.001 || !amplitude.is_finite() {
        return 0;
    }
    // Log amplitude spans -60 to 0 dBFS; do not invent motion for near-silence.
    ((20.0 * amplitude.log10() + 60.0) * (100.0 / 60.0))
        .clamp(0.0, 100.0)
        .round() as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(frequency: f32, amplitude: f32, opposite_phase: bool) -> AudioVisualization {
        let mut analyzer = Analyzer::default();
        for frame in 0..SAMPLE_RATE {
            let sample = (frame as f32 * std::f32::consts::TAU * frequency / SAMPLE_RATE as f32)
                .sin()
                * amplitude;
            analyzer.push_frame(sample, if opposite_phase { -sample } else { sample });
        }
        analyzer.visual()
    }

    #[test]
    fn bass_and_treble_land_in_their_measured_frequency_bands() {
        let bass = tone(120.0, 0.5, false);
        let treble = tone(4000.0, 0.5, false);
        assert_eq!(
            bass.bands
                .iter()
                .enumerate()
                .max_by_key(|(_, value)| *value)
                .unwrap()
                .0,
            1
        );
        assert_eq!(
            treble
                .bands
                .iter()
                .enumerate()
                .max_by_key(|(_, value)| *value)
                .unwrap()
                .0,
            6
        );
        assert!(bass.bands[1] > bass.bands[6] + 25);
        assert!(treble.bands[6] > treble.bands[1] + 25);
    }

    #[test]
    fn real_amplitude_changes_and_opposite_phase_stereo_are_preserved() {
        let quiet = tone(500.0, 0.02, false);
        let loud = tone(500.0, 0.5, false);
        assert!(loud.bands[3] > quiet.bands[3] + 30);
        assert!(loud.peak > quiet.peak);
        assert_eq!(loud, tone(500.0, 0.5, true));
    }

    #[test]
    fn pause_silence_and_invalid_samples_do_not_keep_the_ring_alive() {
        let mut analyzer = Analyzer::default();
        for _ in 0..HOP_FRAMES {
            analyzer.push_frame(0.5, 0.5);
        }
        assert!(analyzer.visual().peak > 0);
        for _ in 0..HOP_FRAMES {
            analyzer.push_frame(0.0, 0.0);
        }
        assert_eq!(analyzer.visual(), AudioVisualization::default());
        for _ in 0..HOP_FRAMES {
            analyzer.push_frame(f32::NAN, f32::INFINITY);
        }
        assert_eq!(analyzer.visual(), AudioVisualization::default());
        assert_eq!(tone(120.0, 0.00001, false), AudioVisualization::default());
    }

    #[test]
    fn pcm_chunking_is_invariant_and_reset_discards_old_audio() {
        let samples: Vec<u8> = (0..HOP_FRAMES * 8)
            .flat_map(|index| ((index / 2) as f32 * 0.1).sin().to_ne_bytes())
            .collect();
        let mut whole = Analyzer::default();
        let mut chunks = Analyzer::default();
        whole.push_pcm(&samples);
        for chunk in samples.chunks(FRAME_BYTES * 7) {
            chunks.push_pcm(chunk);
        }
        assert_eq!(whole.visual(), chunks.visual());
        chunks.reset();
        assert_eq!(chunks.visual(), AudioVisualization::default());
    }

    fn feed_tone_blocks(analyzer: &mut Analyzer, amplitude: f32, blocks: usize) -> u8 {
        for _ in 0..blocks {
            for frame in 0..HOP_FRAMES {
                let sample = (frame as f32 * std::f32::consts::TAU * 500.0 / SAMPLE_RATE as f32)
                    .sin()
                    * amplitude;
                analyzer.push_frame(sample, sample);
            }
        }
        analyzer.visual().bands[3]
    }

    #[test]
    fn six_db_musical_attacks_have_visible_travel_and_quick_release() {
        let mut analyzer = Analyzer::default();
        feed_tone_blocks(&mut analyzer, 0.2, 100);
        for _ in 0..4 {
            let quiet = feed_tone_blocks(&mut analyzer, 0.1, 16);
            let attack = feed_tone_blocks(&mut analyzer, 0.2, 2);
            assert!(
                attack > quiet + 35,
                "6 dB attack is visually compressed: {quiet} -> {attack}"
            );
            let released = feed_tone_blocks(&mut analyzer, 0.1, 5);
            assert!(released + 25 < attack, "ring did not recede within 100 ms");
        }
    }

    #[test]
    fn sustained_tones_do_not_invent_beats_or_motion() {
        let mut analyzer = Analyzer::default();
        let stable = feed_tone_blocks(&mut analyzer, 0.2, 200);
        for _ in 0..100 {
            let current = feed_tone_blocks(&mut analyzer, 0.2, 1);
            assert!(
                current.abs_diff(stable) <= 1,
                "steady tone generated motion"
            );
        }
        feed_tone_blocks(&mut analyzer, 0.0, 1);
        assert_eq!(analyzer.visual(), AudioVisualization::default());
    }

    #[test]
    fn reset_also_clears_the_adaptive_loudness_history() {
        let mut used = Analyzer::default();
        feed_tone_blocks(&mut used, 0.8, 30);
        used.reset();
        let mut fresh = Analyzer::default();
        feed_tone_blocks(&mut used, 0.03, 4);
        feed_tone_blocks(&mut fresh, 0.03, 4);
        assert_eq!(used.visual(), fresh.visual());
    }

    #[test]
    #[ignore = "release throughput measurement; run explicitly with --ignored --nocapture"]
    fn measured_filter_bank_throughput() {
        let samples: Vec<u8> = (0..HOP_FRAMES)
            .flat_map(|index| {
                let sample = (index as f32 * 0.1).sin() * 0.5;
                [sample, -sample].into_iter().flat_map(f32::to_ne_bytes)
            })
            .collect();
        let mut analyzers: Vec<_> = (0..32).map(|_| Analyzer::default()).collect();
        let started = std::time::Instant::now();
        for _ in 0..1000 {
            for analyzer in &mut analyzers {
                analyzer.push_pcm(std::hint::black_box(&samples));
                std::hint::black_box(analyzer.visual());
            }
        }
        let elapsed = started.elapsed().as_secs_f64();
        eprintln!(
            "32 streams x 20 seconds of PCM analysed in {elapsed:.3}s; {:.3}% of one core per real-time stream (analysis only)",
            elapsed / 640.0 * 100.0
        );
        assert!(analyzers.iter().all(|analyzer| analyzer.visual().peak > 0));
    }
}
