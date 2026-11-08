# vu

A full-screen terminal sound meter written in Rust. It shows:

- **Levels**, in one of three meter modes (`v` cycles RMS → VU → LUFS; `--vu` or `--lufs` picks the starting one). Each mode shows only its own measure, in both the meters and the timeline:
  - **RMS** (default): per-channel RMS bars (300 ms sliding window) with a peak envelope falling at 20 dB per 1.7 s (IEC 60268-18) and a peak-hold marker (held 1.5 s, then falling at 20 dB/s), on a dBFS scale. Readouts: `peak hold rms avg`.
  - **VU**: per-channel VU needles, simulated on every sample as a damped spring driven by the rectified signal (IEC 60268-17: 99% of a step in 300 ms, about 1% overshoot, falling the same way). Each frame shows the highest point the needle reached, so no moment is skipped, and the marker holds the highest reading (1.5 s, then 20 dB/s). The scale is the standard VU scale, −20 to +3 VU and linear in amplitude like a real meter's needle, in the traditional two zones: green below 0 VU, red from 0 VU up. 0 VU is −18 dBFS (EBU R68; `--vu-ref -20` for SMPTE), so a −18 dBFS alignment tone reads 0 VU. Readouts: `vu hold`, in VU.
  - **LUFS**: momentary (M) and short-term (S) loudness bars on the EBU +9 scale (EBU Tech 3341): −18 to +9 LU, linear in dB, with 0 LU at the −23 LUFS target; cyan below the target, yellow from it up. The M bar's marker is the highest momentary loudness. Readouts: `mom short integ max` in LUFS (`R` resets the max).
  - In every mode: the session's highest sample peak in dBFS and the clip count (`x` resets both), a CLIP badge lit for 2 s after a clip, and the strongest tone with its musical note (shown only when a tone stands at least 15 dB above nearby bins, so hiss reads `—`). The VU and loudness scales top out well below full scale, so this is what shows an overload there. The numbers refresh 10 times a second.
- **Equalizer**: the standard 1/3-octave bands (20 Hz – 20 kHz, octave centres labelled) with fast-rise, slow-fall smoothing per band and peak caps (held 1 s, then falling at 30 dB/s); `-b N` switches to N log-spaced bands.
- **Spectrogram**: a scrolling spectrogram on a log, mel or linear frequency axis. Each terminal cell holds two pixels.
- **Timeline**: levels over time in the meter mode: peak and RMS (RMS), the VU needle with no peak layer, since peaks mostly sit above +3 VU (VU), or short-term loudness with momentary behind it (LUFS). Each column holds the highest reading of the frames it covers.
- **Piano** (off by default; `-p …,piano`, `--piano`, `-p all` or `5`): an 88-key keyboard, A0–C8, lighting the key nearest the strongest tone. When the panel is too narrow for all 88 keys, a window of keys follows the tone.

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
vu -p all --cycle-views              # demo: every panel, meter modes stepping every 3 s
vu -f song.wav --report              # print statistics, no UI
vu -g sine:997:-20 -t 3 --report     # check the loudness meter
```

Test signals for `-g`: `sine:FREQ[:DB]`, `square:FREQ[:DB]`, `noise[:DB]`, `pink[:DB]`, `sweep:F0:F1:SECS[:DB[:END_DB]]` (with `END_DB` the level also ramps from `DB` to `END_DB` across each sweep, e.g. `sweep:100:800:5:-20:-10`), `silence`.

Other options (see `vu --help`): `--fft-size`, `--hop`, `--window`, `-w/--weighting a|c|z`, `-b/--bands`, `--min-freq`, `--max-freq`, `--freq-scale`, `--db-min`, `--db-max`, `--meter-floor`, `--rms-time`, `--vu`, `--lufs`, `--vu-ref`, `--cycle-views[=SECS]` (step through the meter modes every SECS seconds, default 3), `--piano`, `--peak-decay`, `--peak-hold`, `--colormap`, `--color auto|truecolor|256`, `--fps`, `-z/--span SECONDS` (time shown across the charts, default 10), `--history`, `--channel`, `--channels`, `--sample-rate`, `--buffer-size`, `--record-format int16|int24|float32`, `--no-mouse`.

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
| x | reset the session max level and clip count |
| R | reset the max loudness |
| 1 2 3 4 5 | toggle the meter, equalizer, spectrogram, timeline and piano |
| c / s | cycle the colour map / frequency scale |
| v | cycle the meter mode: RMS, VU, LUFS (meters and timeline) |
| [ ] / { } | lower or raise the display floor / ceiling |
| ? | help |
| q / Esc | quit |

The charts always show a whole number of seconds (10 s by default), whatever the terminal width: resizing stretches the same time range rather than showing more of it. The timeline's bottom edge (the spectrogram's, when the timeline is hidden) carries a time axis with ticks at round times, and the timeline title shows the visible range, e.g. `0:03.845 – 0:13.845 (10 s)`.

The cursor (`▼` and a vertical line on the spectrogram and timeline) marks the analysis frame that the level meters and equalizer are showing. Scrolling back moves the cursor through the stored history, and End returns to live.

## Design

- The `source` module reads audio from a cpal input stream, a WAV reader thread, or a generator thread. The WAV and generator threads are paced to real time unless `--fast` is set.
- `analyzer` runs once per hop (`fft-size/4` by default; 4096 / 1024 samples, about 21 ms at 48 kHz). Each run computes a windowed real FFT (realfft) into a calibrated power spectrum, applies optional A/C weighting, sums the EQ bands, finds the dominant frequency by parabolic interpolation, measures per-channel peak and RMS with ballistics, and updates BS.1770-4 K-weighted gated loudness.
- All meter smoothing happens in the analyzer, per hop, and is stored in each frame, so scrolling back shows exactly what was shown live. The equalizer bands close 50% of the gap to a new reading per 1/30 s when rising and 12% when falling, clamped to the display floor and snapping the last 0.1 dB. The dominant frequency stays on the previous tone while that tone is within 3 dB of the strongest, so it does not flip between harmonics.
- The UI redraws at 30 fps by default. Each frame is wrapped in synchronized output (`ESC[?2026h … ESC[?2026l`), so terminals that support it show only whole frames.
- Each frame stores its spectrum as one byte per bin (0.5 dB steps), so the default 10 minutes of history takes about 75 MB.
- The charts write directly into the ratatui buffer. Pixel-to-bin ranges and colour lookup tables are cached, and a frame is redrawn only when data or input changes.
