//! Audio sources: the default/selected input device, an audio file (or
//! standard input), or a generator. Each delivers interleaved f32 chunks
//! over a channel.

use crate::cli::Cli;
use crate::generator::{parse_spec, Generator};
use anyhow::{bail, Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::io::Read;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender, TryRecvError};
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
    /// Another kind of running input, kept alive while the source is.
    _input: Option<Box<dyn std::any::Any>>,
    device: Option<DeviceParams>,
    /// A live input not yet claimed, and why: taking it now would take it
    /// from another app (see [`blocked`]). [`Source::claim`] tries again.
    pub waiting: Option<String>,
    /// A Bluetooth input, which another device (a phone) can take over.
    pub bluetooth: bool,
    /// A live input that follows the user's Mic Mode: whether it was opened
    /// through voice processing. `None` for other inputs.
    voice: Option<bool>,
    /// The channel count the input delivers, when it is shown as another
    /// (see [`Source::keep_format`]).
    delivered: Option<usize>,
}

/// What is needed to reopen a device stream into the same channel.
struct DeviceParams {
    device: cpal::Device,
    config: cpal::StreamConfig,
    format: cpal::SampleFormat,
    tx: SyncSender<Vec<f32>>,
    /// The rate was asked for (`--sample-rate`), so it may be set on the
    /// device; otherwise the device's own rate is left alone.
    fixed_rate: bool,
}

/// A live input's sample rate changed under it (a Bluetooth headset
/// switching profiles, say): the analysis must start again at the new rate.
#[derive(Debug)]
pub struct RateChanged {
    pub from: u32,
    pub to: u32,
}

impl std::fmt::Display for RateChanged {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(
            f,
            "the input's sample rate changed from {} to {} Hz",
            self.from, self.to
        )
    }
}

impl std::error::Error for RateChanged {}

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
    /// Show this input in the format of `old`, the one it replaces, so an
    /// analysis can carry on across the change: its channels are spread
    /// over (or folded into) the old count. Returns false if the sample
    /// rates differ, which this cannot bridge.
    pub fn keep_format(&mut self, old: &SourceInfo) -> bool {
        if self.info.sample_rate != old.sample_rate {
            return false;
        }
        if self.info.channels != old.channels {
            self.delivered = Some(self.info.channels);
            self.info.channels = old.channels;
        }
        true
    }

    /// The next chunk of audio if one is waiting, in the channel layout
    /// shown.
    pub fn try_recv(&self) -> Result<Vec<f32>, TryRecvError> {
        let chunk = self.rx.try_recv()?;
        Ok(match self.delivered {
            Some(from) => remap(&chunk, from, self.info.channels),
            None => chunk,
        })
    }

    /// The user's Mic Mode has changed so that this live input should be
    /// opened again, through voice processing or without it.
    pub fn mic_mode_changed(&self) -> bool {
        self.voice.is_some_and(|v| v != mic_mode_wants_voice())
    }

    /// Name a voice-processing input after the Mic Mode now in effect, which
    /// the user can change between Voice Isolation and Wide Spectrum while it
    /// runs. Returns whether the name changed.
    pub fn name_mic_mode(&mut self) -> bool {
        #[cfg(target_os = "macos")]
        if self.voice == Some(true) && self._input.is_some() {
            if let (Some((device, _)), Some(mode)) = (
                self.info.name.rsplit_once(" ("),
                crate::mic_mode_mac::active(),
            ) {
                let name = format!("{device} ({mode})");
                if name != self.info.name {
                    self.info.name = name;
                    return true;
                }
            }
        }
        false
    }

    /// Whether this source can be reopened (live devices only).
    pub fn can_reopen(&self) -> bool {
        self.device.is_some()
    }

    /// Claim a waiting input if it can now be shared; returns whether it is
    /// running. See [`Source::waiting`].
    pub fn claim(&mut self) -> Result<bool> {
        if self.waiting.is_some() {
            self.reopen()?;
        }
        Ok(self.waiting.is_none())
    }

    /// The live device's sample rate now, if it can be read.
    pub fn device_rate(&self) -> Option<u32> {
        let p = self.device.as_ref()?;
        p.device
            .default_input_config()
            .ok()
            .map(|c| c.sample_rate().0)
    }

    /// Close and reopen the device stream, looking the device up again by name
    /// in case it reconnected. Data keeps arriving on the same channel. Fails
    /// with [`RateChanged`] if the device now runs at another rate, rather
    /// than forcing the old rate back on it (and on every other app using it).
    pub fn reopen(&mut self) -> Result<()> {
        let Some(p) = &mut self.device else {
            bail!("not a live input")
        };
        self._stream = None;
        let rate = p.fixed_rate.then_some(p.config.sample_rate.0);
        if let Some(why) = blocked(&self.info.name, rate) {
            // Release it and wait, rather than take it.
            self.waiting = Some(why);
            return Ok(());
        }
        self.waiting = None;
        let host = cpal::default_host();
        if let Some(d) = host.input_devices().ok().and_then(|mut it| {
            it.find(|d| d.name().ok().as_deref() == Some(self.info.name.as_str()))
        }) {
            p.device = d;
        }
        if !p.fixed_rate {
            let from = p.config.sample_rate.0;
            let now = p.device.default_input_config().map(|c| c.sample_rate().0);
            if let Some(to) = now.ok().filter(|&r| r != from) {
                return Err(RateChanged { from, to }.into());
            }
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

/// Why the named input cannot be opened now without taking it from another
/// app, if so: another app holds it exclusively, or `rate` (asked for with
/// `--sample-rate`) would change the rate another app is capturing at.
/// Plain sharing is fine: macOS mixes any number of apps on one input.
pub fn blocked(name: &str, rate: Option<u32>) -> Option<String> {
    #[cfg(target_os = "macos")]
    {
        use crate::device_mac::{process_name, usage};
        let u = usage(name)?;
        if let Some(pid) = u.hog {
            return Some(format!("in exclusive use by {}", process_name(pid)));
        }
        if !u.alive {
            return Some("not available".into());
        }
        if let (Some(want), Some(now), Some(&pid)) = (rate, u.rate, u.capturing.first()) {
            if want != now {
                return Some(format!("{} is using it at {now} Hz", process_name(pid)));
            }
        }
        None
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (name, rate);
        None
    }
}

/// The rate the named device runs at now, where it can be read.
fn nominal_rate(name: &str) -> Option<u32> {
    #[cfg(target_os = "macos")]
    return crate::device_mac::usage(name).and_then(|u| u.rate);
    #[cfg(not(target_os = "macos"))]
    {
        let _ = name;
        None
    }
}

fn is_bluetooth(name: &str) -> bool {
    #[cfg(target_os = "macos")]
    return crate::device_mac::usage(name).is_some_and(|u| u.bluetooth);
    #[cfg(not(target_os = "macos"))]
    {
        let _ = name;
        false
    }
}

/// Open the source selected on the command line. `fast` disables real-time pacing.
pub fn open(cli: &Cli, fast: bool, looping: bool) -> Result<Source> {
    if let Some(path) = cli.inputs().first() {
        open_file(path, fast, looping)
    } else if let Some(spec) = &cli.generate {
        open_generator(
            spec,
            cli.sample_rate.unwrap_or(48000),
            cli.channels.unwrap_or(2),
            fast,
        )
    } else {
        open_live(cli)
    }
}

/// Interleaved audio from `from` channels to `to`: output channel `i` takes
/// input channel `i`, or the last one if there are fewer (so mono fills all).
fn remap(chunk: &[f32], from: usize, to: usize) -> Vec<f32> {
    chunk
        .chunks_exact(from)
        .flat_map(|frame| (0..to).map(move |i| frame[i.min(from - 1)]))
        .collect()
}

/// Whether the user's Mic Mode (macOS) needs voice processing to apply.
fn mic_mode_wants_voice() -> bool {
    #[cfg(target_os = "macos")]
    return crate::mic_mode_mac::wants_voice();
    #[cfg(not(target_os = "macos"))]
    false
}

/// A live input. With the user's Mic Mode set to Voice Isolation or Wide
/// Spectrum it goes through voice processing, as the mode applies only there;
/// otherwise (and if voice processing fails) it is the device's own signal.
/// A rate, channel count or buffer size asked for needs the device itself.
fn open_live(cli: &Cli) -> Result<Source> {
    if cli.sample_rate.is_some() || cli.channels.is_some() || cli.buffer_size.is_some() {
        return open_device(cli);
    }
    let voice = mic_mode_wants_voice();
    #[cfg(target_os = "macos")]
    let opened = if voice { open_voice(cli).ok() } else { None };
    #[cfg(not(target_os = "macos"))]
    let opened = None;
    let mut source = match opened {
        Some(s) => s,
        None => open_device(cli)?,
    };
    source.voice = Some(voice);
    Ok(source)
}

/// A live input through voice processing, so the user's Mic Mode applies.
#[cfg(target_os = "macos")]
fn open_voice(cli: &Cli) -> Result<Source> {
    let host = cpal::default_host();
    let device = match &cli.device {
        Some(name) => Some(find_device(&host, name)?.name()?),
        None => None,
    };
    let shown = match &device {
        Some(n) => n.clone(),
        None => host
            .default_input_device()
            .and_then(|d| d.name().ok())
            .unwrap_or_else(|| "input".into()),
    };
    let (tx, rx) = sync_channel(1024);
    let control = Arc::new(Control::default());
    let input = crate::voice_mac::open(device.as_deref(), tx, control.clone())?;
    Ok(Source {
        info: SourceInfo {
            name: format!(
                "{shown} ({})",
                crate::mic_mode_mac::active().unwrap_or("voice processing")
            ),
            sample_rate: input.sample_rate,
            channels: input.channels,
            total_frames: None,
            kind: SourceKind::Device,
        },
        rx,
        control,
        _stream: None,
        _input: Some(Box::new(input)),
        device: None,
        waiting: None,
        bluetooth: false,
        voice: None,
        delivered: None,
    })
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

/// Open an audio file. WAV goes through hound; other formats, WAV files
/// hound cannot read, and standard input (`-`) through symphonia's decoders.
fn open_file(path: &Path, fast: bool, looping: bool) -> Result<Source> {
    if Cli::is_stdin(path) {
        let input = symphonia::core::io::ReadOnlySource::new(std::io::stdin());
        return open_decoded(Box::new(input), None, "stdin".into(), fast, false)
            .context("reading standard input");
    }
    let opening = || format!("opening {}", path.display());
    let file = std::fs::File::open(path).with_context(opening)?;
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase());
    if matches!(ext.as_deref(), Some("wav" | "wave")) {
        if let Ok(reader) = hound::WavReader::open(path) {
            return open_wav(reader, path, fast, looping);
        }
    }
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    open_decoded(Box::new(file), ext.as_deref(), name, fast, looping).with_context(opening)
}

fn open_wav(
    reader: hound::WavReader<std::io::BufReader<std::fs::File>>,
    path: &Path,
    fast: bool,
    looping: bool,
) -> Result<Source> {
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
        _input: None,
        device: None,
        waiting: None,
        bluetooth: false,
        voice: None,
        delivered: None,
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

/// The formats files can be in, for error messages.
const FORMATS: &str = "it reads WAV, AIFF, FLAC, MP3, Ogg Vorbis, AAC and ALAC";

/// A compressed (or otherwise non-WAV) stream being decoded.
struct Decoded {
    format: Box<dyn symphonia::core::formats::FormatReader>,
    decoder: Box<dyn symphonia::core::codecs::Decoder>,
    track_id: u32,
    sample_rate: u32,
    channels: usize,
}

impl Decoded {
    /// Probe `input` for a format (`ext` is a hint) and set up its first
    /// audio track. Returns the decoder, the track's length in sample
    /// frames if known, and its first decoded audio, which fixes the format.
    fn open(
        input: Box<dyn symphonia::core::io::MediaSource>,
        ext: Option<&str>,
    ) -> Result<(Decoded, Option<u64>, Vec<f32>)> {
        use symphonia::core::codecs::CODEC_TYPE_NULL;
        let stream = symphonia::core::io::MediaSourceStream::new(input, Default::default());
        let mut hint = symphonia::core::probe::Hint::new();
        if let Some(ext) = ext {
            hint.with_extension(ext);
        }
        // Gapless: leave out the padding MP3 and AAC encoders add.
        let options = symphonia::core::formats::FormatOptions {
            enable_gapless: true,
            ..Default::default()
        };
        let format = symphonia::default::get_probe()
            .format(&hint, stream, &options, &Default::default())
            .map_err(|_| anyhow::anyhow!("not an audio file vu-meter can read ({FORMATS})"))?
            .format;
        let track = format
            .tracks()
            .iter()
            .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
            .context("no audio track")?;
        let decoder = symphonia::default::get_codecs()
            .make(&track.codec_params, &Default::default())
            .map_err(|_| anyhow::anyhow!("its audio codec is not supported ({FORMATS})"))?;
        let total = track.codec_params.n_frames;
        let mut d = Decoded {
            track_id: track.id,
            format,
            decoder,
            sample_rate: 0,
            channels: 0,
        };
        let first = d.next()?.context("no audio in it")?;
        Ok((d, total, first))
    }

    /// The next packet's audio, interleaved; `None` at the end.
    fn next(&mut self) -> Result<Option<Vec<f32>>> {
        use symphonia::core::errors::Error as E;
        loop {
            let packet = match self.format.next_packet() {
                Ok(p) => p,
                Err(E::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                    return Ok(None)
                }
                Err(E::ResetRequired) => return Ok(None),
                Err(e) => return Err(e.into()),
            };
            if packet.track_id() != self.track_id {
                continue;
            }
            let decoded = match self.decoder.decode(&packet) {
                Ok(d) => d,
                // A damaged packet is skipped, as players do.
                Err(E::DecodeError(_)) => continue,
                Err(e) => return Err(e.into()),
            };
            if decoded.frames() == 0 {
                continue;
            }
            let spec = *decoded.spec();
            let (rate, channels) = (spec.rate, spec.channels.count());
            if self.channels == 0 {
                (self.sample_rate, self.channels) = (rate, channels);
            } else if (rate, channels) != (self.sample_rate, self.channels) {
                bail!("the audio format changes part way through");
            }
            let mut buf =
                symphonia::core::audio::SampleBuffer::<f32>::new(decoded.capacity() as u64, spec);
            buf.copy_interleaved_ref(decoded);
            return Ok(Some(buf.samples().to_vec()));
        }
    }

    /// Go back to the start, for looping; false if the input cannot seek.
    fn rewind(&mut self) -> bool {
        use symphonia::core::formats::{SeekMode, SeekTo};
        let to = SeekTo::Time {
            time: Default::default(),
            track_id: Some(self.track_id),
        };
        let ok = self.format.seek(SeekMode::Accurate, to).is_ok();
        self.decoder.reset();
        ok
    }
}

fn open_decoded(
    input: Box<dyn symphonia::core::io::MediaSource>,
    ext: Option<&str>,
    name: String,
    fast: bool,
    looping: bool,
) -> Result<Source> {
    let (decoded, total_frames, first) = Decoded::open(input, ext)?;
    if decoded.channels == 0 || decoded.sample_rate == 0 {
        bail!("no audio in it");
    }
    let info = SourceInfo {
        name,
        sample_rate: decoded.sample_rate,
        channels: decoded.channels,
        total_frames,
        kind: SourceKind::File,
    };
    let (tx, rx) = sync_channel(64);
    let control = Arc::new(Control::default());
    let ctl = control.clone();
    thread::Builder::new()
        .name("vu-file".into())
        .spawn(move || {
            if let Err(e) = decoded_thread(decoded, first, tx, &ctl, fast, looping) {
                ctl.set_error(format!("file read error: {e:#}"));
            }
        })?;
    Ok(Source {
        info,
        rx,
        control,
        _stream: None,
        _input: None,
        device: None,
        waiting: None,
        bluetooth: false,
        voice: None,
        delivered: None,
    })
}

fn decoded_thread(
    mut decoded: Decoded,
    first: Vec<f32>,
    tx: SyncSender<Vec<f32>>,
    ctl: &Control,
    fast: bool,
    looping: bool,
) -> Result<()> {
    // Packets can hold a tenth of a second; smaller pieces keep the meters
    // moving smoothly.
    const CHUNK: usize = 1024;
    let channels = decoded.channels;
    let mut pacer = Pacer::new(decoded.sample_rate);
    let mut next = Some(first);
    loop {
        let buf = match next.take() {
            Some(b) => b,
            None => match decoded.next()? {
                Some(b) => b,
                None if looping && decoded.rewind() => continue,
                None => return Ok(()),
            },
        };
        for piece in buf.chunks(CHUNK * channels) {
            if !wait_unpaused(ctl, &mut pacer) {
                return Ok(());
            }
            if !fast {
                pacer.wait(piece.len() / channels);
            }
            if tx.send(piece.to_vec()).is_err() {
                return Ok(());
            }
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
        _input: None,
        device: None,
        waiting: None,
        bluetooth: false,
        voice: None,
        delivered: None,
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
        // The device's own rate: opening at any other would change it for
        // every app using the device (cpal sets it to match).
        None => {
            let def = device.default_input_config()?;
            let rate = nominal_rate(&name).map_or(def.sample_rate(), cpal::SampleRate);
            let ch = cli.channels.unwrap_or(def.channels() as usize);
            if rate == def.sample_rate() && ch == def.channels() as usize {
                def
            } else {
                device
                    .supported_input_configs()?
                    .filter(|c| c.channels() as usize == ch)
                    .filter(|c| c.min_sample_rate() <= rate && rate <= c.max_sample_rate())
                    .max_by_key(|c| c.sample_format() == def.sample_format())
                    .map(|c| c.with_sample_rate(rate))
                    .with_context(|| {
                        format!("'{name}' does not support {ch} channels at {} Hz", rate.0)
                    })?
            }
        }
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
    let waiting = blocked(&info.name, cli.sample_rate);
    let stream = match waiting {
        Some(_) => None,
        None => {
            let stream = build_any(&device, &config, format, tx.clone(), control.clone())?;
            stream.play().context("starting input stream")?;
            Some(stream)
        }
    };
    let bluetooth = is_bluetooth(&info.name);
    let device = Some(DeviceParams {
        device,
        config,
        format,
        tx,
        fixed_rate: cli.sample_rate.is_some(),
    });
    Ok(Source {
        info,
        rx,
        control,
        _stream: stream,
        _input: None,
        device,
        waiting,
        bluetooth,
        voice: None,
        delivered: None,
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
