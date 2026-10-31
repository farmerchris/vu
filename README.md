# vu

A full-screen terminal sound meter written in Rust. It shows:

- **Levels**: per-channel RMS bars (300 ms average) with a decaying peak envelope, a peak-hold marker, a momentary-loudness bar, and readouts for peak, hold, RMS, EBU R128 loudness (momentary, short-term, integrated), the dominant frequency and its musical note (shown only when a tone stands at least 15 dB above nearby bins, so hiss reads `—`), and clipping.
- **Equalizer**: log-spaced graphic-EQ bands (31 by default) with peak caps.
- **Spectrogram**: a scrolling spectrogram on a log, mel or linear frequency axis. Each terminal cell holds two pixels.
- **Timeline**: peak and RMS levels over time.

By default it reads the default audio input. It can also read a WAV file or a built-in test signal.

## Build

```sh
cargo build --release      # binary: target/release/vu
cargo test                 # unit + integration tests
```

On macOS, the terminal needs microphone permission (System Settings → Privacy & Security → Microphone). Without it the input is silent.

If a live input starts delivering pure digital silence (every sample exactly zero, which a working microphone never does) for 1.5 s, `vu` reopens the device, up to three times, and shows a yellow warning in the status bar. If it stays silent, the warning says so: the microphone is probably muted or in use by another device.

## Usage

```sh
vu                                   # meter the default input
vu -l                                # list input devices
vu -d "USB"                          # pick a device by name substring (or index)
vu -f song.wav                       # play a WAV file through the meters in real time
vu -f song.wav --fast                # analyse the whole file at once, then browse it
vu -f song.wav --loop
vu -r take.wav                       # record the input while metering
vu -r take.wav -t 30                 # record 30 seconds, then stop
vu -g sweep:20:20000:10              # test signal
vu -p spectrogram,eq                 # only some panels
vu -f song.wav --report              # print statistics, no UI
vu -g sine:997:-20 -t 3 --report     # check the loudness meter
```

Test signals for `-g`: `sine:FREQ[:DB]`, `square:FREQ[:DB]`, `noise[:DB]`, `pink[:DB]`, `sweep:F0:F1:SECS[:DB]`, `silence`.

Other options (see `vu --help`): `--fft-size`, `--hop`, `--window`, `-w/--weighting a|c|z`, `-b/--bands`, `--min-freq`, `--max-freq`, `--freq-scale`, `--db-min`, `--db-max`, `--meter-floor`, `--rms-time`, `--peak-decay`, `--peak-hold`, `--colormap`, `--color auto|truecolor|256`, `--fps`, `-z/--span SECONDS` (time shown across the charts, default 10), `--history`, `--channel`, `--channels`, `--sample-rate`, `--buffer-size`, `--record-format int16|int24|float32`, `--no-mouse`.

## Keys

| Key | Action |
| --- | --- |
| ← / → (h / l) | move the cursor one column back or forward (Shift, Ctrl or H / L moves 10) |
| PgUp / PgDn (b / f) | page back or forward |
| Home / End (g / G) | jump to the oldest data / return to live |
| space | hold or release the display |
| mouse click / wheel | place the cursor / scroll |
| + / - (↑ / ↓) | zoom the time axis in or out (0.5 s, 1, 2, 5, 10, 20, 30 s, 1, 2, 5, 10 min across the chart) |
| p | pause or resume the input |
| r | start or stop recording |
| 1 2 3 4 | toggle the meter, equalizer, spectrogram and timeline |
| c / s | cycle the colour map / frequency scale |
| [ ] / { } | lower or raise the display floor / ceiling |
| ? | help |
| q / Esc | quit |

The charts always show a whole number of seconds (10 s by default), whatever the terminal width: resizing stretches the same time range rather than showing more of it. The timeline's bottom edge (the spectrogram's, when the timeline is hidden) carries a time axis with ticks at round times, and the timeline title shows the visible range, e.g. `0:03.845 – 0:13.845 (10 s)`.

The cursor (`▼` and a vertical line on the spectrogram and timeline) marks the analysis frame that the level meters and equalizer are showing. Scrolling back moves the cursor through the stored history, and End returns to live.

## Design

- The `source` module reads audio from a cpal input stream, a WAV reader thread, or a generator thread. The WAV and generator threads are paced to real time unless `--fast` is set.
- `analyzer` runs once per hop (`fft-size/4` by default). Each run computes a windowed real FFT (realfft) into a calibrated power spectrum, applies optional A/C weighting, sums the EQ bands, finds the dominant frequency by parabolic interpolation, measures per-channel peak and RMS with ballistics, and updates BS.1770-4 K-weighted gated loudness.
- Each frame stores its spectrum as one byte per bin (0.5 dB steps), so the default 10 minutes of history takes about 75 MB.
- The charts write directly into the ratatui buffer. Pixel-to-bin ranges and colour lookup tables are cached, and a frame is redrawn only when data or input changes.
