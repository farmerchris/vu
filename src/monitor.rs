//! `--monitor`: play what is being metered through the default output, so a
//! file or test signal can be heard as well as seen. The output device is
//! opened at its own rate and channel count (never changed, as other apps
//! share it) and the audio is converted to match.

use anyhow::{bail, Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

/// Audio queued before playback starts (and starts again after running
/// dry), seconds: enough to ride out the meter's frame-by-frame delivery.
const PREBUFFER_SECS: f64 = 0.12;
/// Most audio queued, seconds; more than this is dropped, which keeps the
/// sound in step with the meters if the output runs slow.
const MAX_QUEUE_SECS: f64 = 0.75;

/// Converts interleaved input to the output's rate (linear interpolation,
/// plenty for listening along) and channel count.
pub struct Converter {
    in_ch: usize,
    out_ch: usize,
    /// Input frames per output frame.
    step: f64,
    /// Position of the next output frame, in input frames after `prev`.
    pos: f64,
    /// The last input frame of the previous chunk.
    prev: Vec<f32>,
}

impl Converter {
    pub fn new(in_rate: u32, in_ch: usize, out_rate: u32, out_ch: usize) -> Self {
        Converter {
            in_ch: in_ch.max(1),
            out_ch: out_ch.max(1),
            step: in_rate as f64 / out_rate.max(1) as f64,
            pos: 0.0,
            prev: vec![0.0; in_ch.max(1)],
        }
    }

    /// Input channel feeding output channel `o`: mono goes to the first two
    /// outputs, anything else channel for channel; outputs past the input's
    /// channels stay silent.
    fn source(&self, o: usize) -> Option<usize> {
        match self.in_ch {
            1 => (o < 2).then_some(0),
            n => (o < n).then_some(o),
        }
    }

    /// Convert `chunk`, appending interleaved output frames to `out`.
    pub fn push(&mut self, chunk: &[f32], mut out: impl FnMut(f32)) {
        let n = chunk.len() / self.in_ch;
        if n == 0 {
            return;
        }
        // Frame `i` of the sequence: `prev`, then the chunk's frames.
        let at = |i: usize, c: usize, prev: &[f32]| match i {
            0 => prev[c],
            i => chunk[(i - 1) * self.in_ch + c],
        };
        while self.pos < n as f64 {
            let i = self.pos.floor() as usize;
            let t = (self.pos - i as f64) as f32;
            for o in 0..self.out_ch {
                let v = match self.source(o) {
                    Some(c) => {
                        let (a, b) = (at(i, c, &self.prev), at(i + 1, c, &self.prev));
                        a + (b - a) * t
                    }
                    None => 0.0,
                };
                out(v.clamp(-1.0, 1.0));
            }
            self.pos += self.step;
        }
        self.pos -= n as f64;
        self.prev
            .copy_from_slice(&chunk[(n - 1) * self.in_ch..n * self.in_ch]);
    }
}

/// Samples waiting to be played, shared with the output callback.
#[derive(Default)]
struct Queue {
    samples: VecDeque<f32>,
    /// Playing; false while filling up to the prebuffer.
    playing: bool,
}

impl Queue {
    /// Fill `data` from the queue, with silence while it is (re)filling.
    fn fill<T: cpal::Sample + cpal::FromSample<f32>>(&mut self, data: &mut [T], prebuffer: usize) {
        if !self.playing && self.samples.len() >= prebuffer {
            self.playing = true;
        }
        for s in data.iter_mut() {
            let v = if self.playing {
                self.samples.pop_front().unwrap_or(0.0)
            } else {
                0.0
            };
            *s = T::from_sample(v);
        }
        if self.samples.is_empty() {
            self.playing = false;
        }
    }
}

pub struct Monitor {
    _stream: cpal::Stream,
    queue: Arc<Mutex<Queue>>,
    converter: Converter,
    /// Most samples the queue may hold.
    cap: usize,
    /// The output device's name.
    pub name: String,
}

impl Monitor {
    /// Start playing to the default output; audio comes from [`Monitor::push`].
    pub fn open(sample_rate: u32, channels: usize) -> Result<Self> {
        let device = cpal::default_host()
            .default_output_device()
            .context("no default output device")?;
        let name = device.name().unwrap_or_else(|_| "output".into());
        let supported = device
            .default_output_config()
            .with_context(|| format!("'{name}' has no output format"))?;
        let config: cpal::StreamConfig = supported.config();
        let (rate, out_ch) = (config.sample_rate.0, config.channels as usize);
        let queue = Arc::new(Mutex::new(Queue::default()));
        let prebuffer = (PREBUFFER_SECS * rate as f64) as usize * out_ch;
        use cpal::SampleFormat as F;
        let stream = match supported.sample_format() {
            F::F32 => build::<f32>(&device, &config, queue.clone(), prebuffer),
            F::I16 => build::<i16>(&device, &config, queue.clone(), prebuffer),
            F::U16 => build::<u16>(&device, &config, queue.clone(), prebuffer),
            F::I32 => build::<i32>(&device, &config, queue.clone(), prebuffer),
            other => bail!("'{name}': unsupported output format {other:?}"),
        }
        .with_context(|| format!("opening '{name}'"))?;
        stream
            .play()
            .with_context(|| format!("starting '{name}'"))?;
        Ok(Monitor {
            _stream: stream,
            queue,
            converter: Converter::new(sample_rate, channels, rate, out_ch),
            cap: (MAX_QUEUE_SECS * rate as f64) as usize * out_ch,
            name,
        })
    }

    /// Queue interleaved input for playing. If the queue is full (the output
    /// is behind) the chunk is dropped.
    pub fn push(&mut self, chunk: &[f32]) {
        let Ok(mut q) = self.queue.lock() else { return };
        if q.samples.len() >= self.cap {
            return;
        }
        self.converter.push(chunk, |v| q.samples.push_back(v));
    }
}

fn build<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    queue: Arc<Mutex<Queue>>,
    prebuffer: usize,
) -> Result<cpal::Stream>
where
    T: cpal::SizedSample + cpal::FromSample<f32>,
{
    Ok(device.build_output_stream(
        config,
        move |data: &mut [T], _: &cpal::OutputCallbackInfo| {
            // Never wait in the audio thread: a busy queue is one buffer of silence.
            match queue.try_lock() {
                Ok(mut q) => q.fill(data, prebuffer),
                Err(_) => data.iter_mut().for_each(|s| *s = T::from_sample(0.0f32)),
            }
        },
        |_| {},
        None,
    )?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn convert(c: &mut Converter, chunk: &[f32]) -> Vec<f32> {
        let mut out = Vec::new();
        c.push(chunk, |v| out.push(v));
        out
    }

    #[test]
    fn same_rate_passes_through_one_frame_late() {
        let mut c = Converter::new(48000, 2, 48000, 2);
        let out = convert(&mut c, &[0.1, -0.1, 0.2, -0.2, 0.3, -0.3]);
        assert_eq!(out, [0.0, 0.0, 0.1, -0.1, 0.2, -0.2]);
        // The held frame leads the next chunk: nothing lost across chunks.
        assert_eq!(convert(&mut c, &[0.4, -0.4]), [0.3, -0.3]);
        assert!(convert(&mut c, &[]).is_empty());
    }

    #[test]
    fn channels_are_mapped() {
        // Mono to both of a stereo output, and only the first two of more.
        let mut c = Converter::new(48000, 1, 48000, 4);
        assert_eq!(convert(&mut c, &[0.5, 0.25])[4..], [0.5, 0.5, 0.0, 0.0]);
        // Surround to stereo keeps left and right.
        let mut c = Converter::new(48000, 6, 48000, 2);
        let out = convert(&mut c, &[[0.1, 0.2, 0.3, 0.4, 0.5, 0.6]; 2].concat());
        assert_eq!(out[2..], [0.1, 0.2]);
        // Stereo to mono-only output plays the left.
        let mut c = Converter::new(48000, 2, 48000, 1);
        assert_eq!(convert(&mut c, &[0.1, 0.9, 0.2, 0.8]), [0.0, 0.1]);
        // Out-of-range input is limited rather than wrapped.
        let mut c = Converter::new(48000, 1, 48000, 1);
        assert_eq!(convert(&mut c, &[2.0, -3.0, 0.0])[1..], [1.0, -1.0]);
    }

    #[test]
    fn resampling_keeps_the_length_and_the_tone() {
        for (from, to) in [
            (44100u32, 48000u32),
            (48000, 44100),
            (96000, 48000),
            (22050, 48000),
        ] {
            let mut c = Converter::new(from, 1, to, 1);
            let tone: Vec<f32> = (0..from)
                .map(|i| 0.5 * (2.0 * std::f32::consts::PI * 440.0 * i as f32 / from as f32).sin())
                .collect();
            // In uneven chunks, as a decoder delivers them.
            let mut out = Vec::new();
            for chunk in tone.chunks(1117) {
                c.push(chunk, |v| out.push(v));
            }
            assert!(
                (out.len() as i64 - to as i64).abs() <= 2,
                "{from}->{to}: {} samples",
                out.len()
            );
            let crossings = out.windows(2).filter(|w| w[0] < 0.0 && w[1] >= 0.0).count();
            assert!(
                (crossings as i32 - 440).abs() <= 1,
                "{from}->{to}: {crossings}"
            );
            let peak = out.iter().fold(0.0f32, |m, v| m.max(v.abs()));
            assert!((peak - 0.5).abs() < 0.01, "{from}->{to}: {peak}");
        }
    }

    #[test]
    fn queue_prebuffers_and_recovers() {
        let mut q = Queue::default();
        let mut buf = [1.0f32; 4];
        q.samples.extend([0.5; 6]);
        q.fill(&mut buf, 8);
        assert_eq!(buf, [0.0; 4], "silent until the prebuffer is in");
        q.samples.extend([0.5; 2]);
        q.fill(&mut buf, 8);
        assert_eq!(buf, [0.5; 4]);
        // Running dry plays silence and waits for the prebuffer again.
        let mut buf = [1.0f32; 6];
        q.fill(&mut buf, 8);
        assert_eq!(buf, [0.5, 0.5, 0.5, 0.5, 0.0, 0.0]);
        assert!(!q.playing);
        q.samples.extend([0.25; 3]);
        q.fill(&mut buf, 8);
        assert_eq!(buf, [0.0; 6]);
    }
}
