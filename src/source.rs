//! Audio sources: the default/selected input device, a WAV file, or a generator.
//! Each delivers interleaved f32 chunks over a channel.

use crate::cli::Cli;
use crate::generator::{parse_spec, Generator};
use anyhow::{bail, Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::io::Read;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum SourceKind {
    Device,
    File,
    Generator,
}

#[derive(Clone, Debug)]
pub struct SourceInfo {
    pub name: String,
    pub sample_rate: u32,
    pub channels: usize,
    /// Length in sample frames, when known.
    pub total_frames: Option<u64>,
    pub kind: SourceKind,
}

#[derive(Default)]
pub struct Control {
    pub paused: AtomicBool,
    pub dropped: AtomicU64,
    pub stop: AtomicBool,
    pub error: Mutex<Option<String>>,
}

impl Control {
    pub fn is_paused(&self) -> bool {
        self.paused.load(Ordering::Relaxed)
    }

    pub fn set_error(&self, e: String) {
        if let Ok(mut g) = self.error.lock() {
            *g = Some(e);
        }
    }

    pub fn take_error(&self) -> Option<String> {
        self.error.lock().ok().and_then(|mut g| g.take())
    }
}

pub struct Source {
    pub info: SourceInfo,
    pub rx: Receiver<Vec<f32>>,
    pub control: Arc<Control>,
    _stream: Option<cpal::Stream>,
    device: Option<DeviceParams>,
}

/// What is needed to reopen a device stream into the same channel.
struct DeviceParams {
    device: cpal::Device,
    config: cpal::StreamConfig,
    format: cpal::SampleFormat,
    tx: SyncSender<Vec<f32>>,
}

/// Watches a live input for digital silence: a run of samples that are all
/// exactly zero, which a real microphone never produces (even a quiet room has
/// hiss). It means the device has stopped delivering audio.
pub struct SilenceWatch {
    threshold: u64,
    run: u64,
    fired: bool,
}

impl SilenceWatch {
    /// Fire after `threshold` consecutive zero samples (per channel frame count
    /// times channels, i.e. interleaved samples).
    pub fn new(threshold: u64) -> Self {
        SilenceWatch {
            threshold: threshold.max(1),
            run: 0,
            fired: false,
        }
    }

    /// Feed a chunk; returns true once each time the zero run reaches the threshold.
    pub fn feed(&mut self, chunk: &[f32]) -> bool {
        match chunk.iter().rposition(|&x| x != 0.0) {
            Some(i) => {
                self.run = (chunk.len() - 1 - i) as u64;
                self.fired = false;
            }
            None => self.run += chunk.len() as u64,
        }
        if !self.fired && self.run >= self.threshold {
            self.fired = true;
            return true;
        }
        false
    }

    /// Start counting again (after reopening the device).
    pub fn reset(&mut self) {
        self.run = 0;
        self.fired = false;
    }

    /// Whether the input is currently silent past the threshold.
    pub fn silent(&self) -> bool {
        self.run >= self.threshold
    }
}

impl Source {
    /// Whether this source can be reopened (live devices only).
    pub fn can_reopen(&self) -> bool {
        self.device.is_some()
    }

    /// Close and reopen the device stream, looking the device up again by name
    /// in case it reconnected. Data keeps arriving on the same channel.
    pub fn reopen(&mut self) -> Result<()> {
        let Some(p) = &mut self.device else {
            bail!("not a live input")
        };
        self._stream = None;
        let host = cpal::default_host();
        if let Some(d) = host.input_devices().ok().and_then(|mut it| {
            it.find(|d| d.name().ok().as_deref() == Some(self.info.name.as_str()))
        }) {
            p.device = d;
        }
        let stream = build_any(
            &p.device,
            &p.config,
            p.format,
            p.tx.clone(),
            self.control.clone(),
        )?;
        stream.play().context("restarting input stream")?;
        self._stream = Some(stream);
        Ok(())
    }

    /// Number of device buffers dropped because the consumer fell behind, if any.
    pub fn dropped_chunks(&self) -> Option<u64> {
        let n = self.control.dropped.load(Ordering::Relaxed);
        (n > 0).then_some(n)
    }
}

impl Drop for Source {
    fn drop(&mut self) {
        self.control.stop.store(true, Ordering::Relaxed);
    }
}

/// Open the source selected on the command line. `fast` disables real-time pacing.
pub fn open(cli: &Cli, fast: bool, looping: bool) -> Result<Source> {
    if let Some(path) = &cli.file {
        open_file(path, fast, looping)
    } else if let Some(spec) = &cli.generate {
        open_generator(
            spec,
            cli.sample_rate.unwrap_or(48000),
            cli.channels.unwrap_or(2),
            fast,
        )
    } else {
        open_device(cli)
    }
}

/// Sends chunks timed to the real-time position of the stream.
pub struct Pacer {
    start: Instant,
    sample_rate: f64,
    sent: u64,
}

impl Pacer {
    pub fn new(sample_rate: u32) -> Self {
        Pacer {
            start: Instant::now(),
            sample_rate: sample_rate as f64,
            sent: 0,
        }
    }

    /// Offset from start at which a chunk ending at sample frame `end` is due.
    pub fn due(end: u64, sample_rate: f64) -> Duration {
        Duration::from_secs_f64(end as f64 / sample_rate)
    }

    pub fn wait(&mut self, frames: usize) {
        self.sent += frames as u64;
        let due = self.start + Self::due(self.sent, self.sample_rate);
        let now = Instant::now();
        if due > now {
            thread::sleep(due - now);
        }
    }

    /// Shift the timeline forward by time spent paused.
    pub fn shift(&mut self, d: Duration) {
        self.start += d;
    }
}

/// Block while paused, returning false when asked to stop.
fn wait_unpaused(ctl: &Control, pacer: &mut Pacer) -> bool {
    if ctl.is_paused() {
        let t = Instant::now();
        while ctl.is_paused() && !ctl.stop.load(Ordering::Relaxed) {
            thread::sleep(Duration::from_millis(10));
        }
        pacer.shift(t.elapsed());
    }
    !ctl.stop.load(Ordering::Relaxed)
}

/// Read up to `n` interleaved samples as f32 in [-1, 1].
pub fn read_wav_chunk<R: Read>(
    reader: &mut hound::WavReader<R>,
    n: usize,
    out: &mut Vec<f32>,
) -> Result<()> {
    let spec = reader.spec();
    match spec.sample_format {
        hound::SampleFormat::Float => {
            for s in reader.samples::<f32>().take(n) {
                out.push(s?);
            }
        }
        hound::SampleFormat::Int => {
            let scale = 1.0 / (1u64 << (spec.bits_per_sample - 1)) as f32;
            for s in reader.samples::<i32>().take(n) {
                out.push(s? as f32 * scale);
            }
        }
    }
    Ok(())
}

fn open_file(path: &Path, fast: bool, looping: bool) -> Result<Source> {
    let reader =
        hound::WavReader::open(path).with_context(|| format!("opening {}", path.display()))?;
    let spec = reader.spec();
    if spec.channels == 0 {
        bail!("{} has no channels", path.display());
    }
    let info = SourceInfo {
        name: path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
        sample_rate: spec.sample_rate,
        channels: spec.channels as usize,
        total_frames: Some(reader.duration() as u64),
        kind: SourceKind::File,
    };
    let (tx, rx) = sync_channel(64);
    let control = Arc::new(Control::default());
    let ctl = control.clone();
    let ch = info.channels;
    thread::Builder::new()
        .name("vu-file".into())
        .spawn(move || {
            if let Err(e) = file_thread(reader, tx, &ctl, ch, fast, looping) {
                ctl.set_error(format!("file read error: {e:#}"));
            }
        })?;
    Ok(Source {
        info,
        rx,
        control,
        _stream: None,
        device: None,
    })
}

fn file_thread(
    mut reader: hound::WavReader<std::io::BufReader<std::fs::File>>,
    tx: SyncSender<Vec<f32>>,
    ctl: &Control,
    channels: usize,
    fast: bool,
    looping: bool,
) -> Result<()> {
    const CHUNK: usize = 1024;
    let mut pacer = Pacer::new(reader.spec().sample_rate);
    let has_data = reader.duration() > 0;
    loop {
        if !wait_unpaused(ctl, &mut pacer) {
            return Ok(());
        }
        let mut buf = Vec::with_capacity(CHUNK * channels);
        read_wav_chunk(&mut reader, CHUNK * channels, &mut buf)?;
        buf.truncate(buf.len() - buf.len() % channels);
        if buf.is_empty() {
            if looping && has_data {
                reader.seek(0)?;
                continue;
            }
            return Ok(());
        }
        if !fast {
            pacer.wait(buf.len() / channels);
        }
        if tx.send(buf).is_err() {
            return Ok(());
        }
    }
}

fn open_generator(spec: &str, sample_rate: u32, channels: usize, fast: bool) -> Result<Source> {
    let gspec = parse_spec(spec)?;
    let info = SourceInfo {
        name: format!("gen {spec}"),
        sample_rate,
        channels,
        total_frames: None,
        kind: SourceKind::Generator,
    };
    let (tx, rx) = sync_channel(64);
    let control = Arc::new(Control::default());
    let ctl = control.clone();
    thread::Builder::new()
        .name("vu-gen".into())
        .spawn(move || {
            const CHUNK: usize = 512;
            let mut g = Generator::new(gspec, sample_rate);
            let mut pacer = Pacer::new(sample_rate);
            while wait_unpaused(&ctl, &mut pacer) {
                let mut buf = vec![0.0f32; CHUNK * channels];
                g.fill(&mut buf, channels);
                if !fast {
                    pacer.wait(CHUNK);
                }
                if tx.send(buf).is_err() {
                    break;
                }
            }
        })?;
    Ok(Source {
        info,
        rx,
        control,
        _stream: None,
        device: None,
    })
}

fn find_device(host: &cpal::Host, want: &str) -> Result<cpal::Device> {
    let devices: Vec<cpal::Device> = host.input_devices()?.collect();
    if let Ok(i) = want.parse::<usize>() {
        if let Some(d) = devices.get(i) {
            return Ok(d.clone());
        }
    }
    let needle = want.to_lowercase();
    // An exact name wins over a substring, so "Mic" never opens "Mic 2".
    let name = |d: &cpal::Device| d.name().map(|n| n.to_lowercase()).unwrap_or_default();
    if let Some(d) = devices.iter().find(|d| name(d) == needle) {
        return Ok(d.clone());
    }
    devices
        .into_iter()
        .find(|d| {
            d.name()
                .map(|n| n.to_lowercase().contains(&needle))
                .unwrap_or(false)
        })
        .with_context(|| format!("no input device matching '{want}' (see --list-devices)"))
}

/// Print a hint if opening the device stalls, which on macOS usually means the
/// system is waiting for microphone permission.
fn stall_hint() -> Arc<AtomicBool> {
    let done = Arc::new(AtomicBool::new(false));
    let d = done.clone();
    thread::spawn(move || {
        thread::sleep(Duration::from_secs(3));
        if !d.load(Ordering::Relaxed) {
            eprintln!(
                "vu: still waiting for the audio input... if this persists, allow microphone access \
                 for this terminal (macOS: System Settings > Privacy & Security > Microphone), \
                 or use -f FILE / -g SIGNAL"
            );
        }
    });
    done
}

fn open_device(cli: &Cli) -> Result<Source> {
    let hint = stall_hint();
    let res = open_device_inner(cli);
    hint.store(true, Ordering::Relaxed);
    res
}

fn open_device_inner(cli: &Cli) -> Result<Source> {
    let host = cpal::default_host();
    let device = match &cli.device {
        Some(name) => find_device(&host, name)?,
        None => host
            .default_input_device()
            .context("no default input device")?,
    };
    let name = device.name().unwrap_or_else(|_| "input".into());
    let supported = match cli.sample_rate {
        Some(sr) => device
            .supported_input_configs()?
            .filter(|c| c.min_sample_rate().0 <= sr && sr <= c.max_sample_rate().0)
            .filter(|c| cli.channels.is_none_or(|ch| c.channels() as usize == ch))
            .max_by_key(|c| c.sample_format() == cpal::SampleFormat::F32)
            .map(|c| c.with_sample_rate(cpal::SampleRate(sr)))
            .with_context(|| format!("'{name}' does not support {sr} Hz"))?,
        None => match cli.channels {
            Some(ch) => {
                let def = device.default_input_config()?;
                device
                    .supported_input_configs()?
                    .filter(|c| c.channels() as usize == ch)
                    .find(|c| {
                        c.min_sample_rate() <= def.sample_rate()
                            && def.sample_rate() <= c.max_sample_rate()
                    })
                    .map(|c| c.with_sample_rate(def.sample_rate()))
                    .with_context(|| format!("'{name}' does not support {ch} channels"))?
            }
            None => device.default_input_config()?,
        },
    };
    let format = supported.sample_format();
    let mut config: cpal::StreamConfig = supported.config();
    if let Some(bs) = cli.buffer_size {
        config.buffer_size = cpal::BufferSize::Fixed(bs);
    }
    let info = SourceInfo {
        name,
        sample_rate: config.sample_rate.0,
        channels: config.channels as usize,
        total_frames: None,
        kind: SourceKind::Device,
    };
    let (tx, rx) = sync_channel(1024);
    let control = Arc::new(Control::default());
    let stream = build_any(&device, &config, format, tx.clone(), control.clone())?;
    stream.play().context("starting input stream")?;
    let device = Some(DeviceParams {
        device,
        config,
        format,
        tx,
    });
    Ok(Source {
        info,
        rx,
        control,
        _stream: Some(stream),
        device,
    })
}

fn build_any(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    format: cpal::SampleFormat,
    tx: SyncSender<Vec<f32>>,
    control: Arc<Control>,
) -> Result<cpal::Stream> {
    use cpal::SampleFormat as F;
    Ok(match format {
        F::F32 => build_stream::<f32>(device, config, tx, control)?,
        F::F64 => build_stream::<f64>(device, config, tx, control)?,
        F::I16 => build_stream::<i16>(device, config, tx, control)?,
        F::I32 => build_stream::<i32>(device, config, tx, control)?,
        F::I8 => build_stream::<i8>(device, config, tx, control)?,
        F::U8 => build_stream::<u8>(device, config, tx, control)?,
        F::U16 => build_stream::<u16>(device, config, tx, control)?,
        F::U32 => build_stream::<u32>(device, config, tx, control)?,
        other => bail!("unsupported sample format {other:?}"),
    })
}

fn build_stream<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    tx: SyncSender<Vec<f32>>,
    ctl: Arc<Control>,
) -> Result<cpal::Stream>
where
    T: cpal::SizedSample,
    f32: cpal::FromSample<T>,
{
    let drop_ctl = ctl.clone();
    let stream = device.build_input_stream(
        config,
        move |data: &[T], _: &cpal::InputCallbackInfo| {
            let v: Vec<f32> = data.iter().map(|&s| s.to_sample::<f32>()).collect();
            // Never block the audio thread; drop (and count) data if the UI falls far behind.
            if tx.try_send(v).is_err() {
                drop_ctl.dropped.fetch_add(1, Ordering::Relaxed);
            }
        },
        move |e| ctl.set_error(format!("input stream error: {e}")),
        None,
    )?;
    Ok(stream)
}

/// Names of the input devices and of the default one.
pub fn input_device_names() -> (Vec<String>, Option<String>) {
    let host = cpal::default_host();
    let default = host.default_input_device().and_then(|d| d.name().ok());
    let names = host
        .input_devices()
        .map(|it| it.filter_map(|d| d.name().ok()).collect())
        .unwrap_or_default();
    (names, default)
}

pub fn list_devices() -> Result<()> {
    use std::io::Write;
    let hint = stall_hint();
    let host = cpal::default_host();
    let default = host.default_input_device().and_then(|d| d.name().ok());
    println!("Input devices ({}):", host.id().name());
    for (i, d) in host.input_devices()?.enumerate() {
        let name = d.name().unwrap_or_else(|_| "?".into());
        let mark = if Some(&name) == default.as_ref() {
            "*"
        } else {
            " "
        };
        print!(" {mark} {i:>2}: {name}  ");
        let _ = std::io::stdout().flush();
        let cfg = d
            .default_input_config()
            .map(|c| {
                format!(
                    "{} Hz, {} ch, {:?}",
                    c.sample_rate().0,
                    c.channels(),
                    c.sample_format()
                )
            })
            .unwrap_or_else(|e| format!("unavailable: {e}"));
        println!("[{cfg}]");
    }
    hint.store(true, Ordering::Relaxed);
    println!("\n* = default. Select with --device <index or name substring>.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn silence_watch_fires_once_per_run() {
        let mut w = SilenceWatch::new(100);
        assert!(!w.feed(&[0.0; 60]));
        assert!(!w.silent());
        assert!(w.feed(&[0.0; 60]), "120 zeros reach the threshold");
        assert!(w.silent());
        assert!(!w.feed(&[0.0; 500]), "fires only once per run");
        // Any non-zero sample ends the run; trailing zeros start a new one.
        assert!(!w.feed(&[0.0, 1e-6, 0.0, 0.0]));
        assert!(!w.silent());
        assert!(w.feed(&[0.0; 98]), "2 trailing + 98 = 100");
        w.reset();
        assert!(!w.silent());
        assert!(!w.feed(&[0.0; 99]));
        assert!(w.feed(&[0.0]));
        // Quiet but real signal never counts as silence.
        let mut w = SilenceWatch::new(10);
        assert!(!w.feed(&[1e-7; 1000]));
    }

    #[test]
    fn pacer_due_times() {
        assert_eq!(Pacer::due(48000, 48000.0), Duration::from_secs(1));
        assert_eq!(Pacer::due(0, 48000.0), Duration::ZERO);
        assert!((Pacer::due(24000, 48000.0).as_secs_f64() - 0.5).abs() < 1e-9);
    }

    #[test]
    fn pacer_waits_roughly_real_time() {
        let mut p = Pacer::new(10_000);
        let t = Instant::now();
        p.wait(500);
        let e = t.elapsed().as_secs_f64();
        assert!((0.045..0.5).contains(&e), "{e}");
    }

    #[test]
    fn file_source_reads_everything() {
        let dir = std::env::temp_dir().join(format!("vu-src-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("src.wav");
        let spec = hound::WavSpec {
            channels: 2,
            sample_rate: 8000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut w = hound::WavWriter::create(&path, spec).unwrap();
        for i in 0..5000 {
            w.write_sample((i % 100) as i16 * 100).unwrap();
        }
        w.finalize().unwrap();
        let src = open_file(&path, true, false).unwrap();
        assert_eq!(src.info.channels, 2);
        assert_eq!(src.info.total_frames, Some(2500));
        let total: usize = src.rx.iter().map(|c| c.len()).sum();
        assert_eq!(total, 5000);
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn generator_source_streams() {
        let src = open_generator("sine:1000", 8000, 2, true).unwrap();
        let c = src.rx.recv().unwrap();
        assert_eq!(c.len(), 1024);
        assert!(open_generator("bogus", 8000, 2, true).is_err());
    }

    #[test]
    fn pause_control() {
        let c = Control::default();
        assert!(!c.is_paused());
        c.paused.store(true, Ordering::Relaxed);
        assert!(c.is_paused());
        c.set_error("x".into());
        assert_eq!(c.take_error().as_deref(), Some("x"));
        assert_eq!(c.take_error(), None);
    }
}
