# vu-meter

A sound meter written in Rust, full-screen in the terminal or in a window (`vu-meter --gui`), for a live input, an audio file or a test signal. It can also print loudness reports for scripts and check files against a delivery target. See [CHANGELOG.md](CHANGELOG.md) for what is new in 2.0.

It shows:

- **Levels**, in one of three meter modes (`v` cycles RMS → VU → LUFS; `--vu` or `--lufs` picks the starting one). Each mode shows only its own measure, in both the meters and the timeline:
  - **RMS** (default): per-channel RMS bars (300 ms sliding window) with a peak envelope falling at 20 dB per 1.7 s (IEC 60268-18) and a peak-hold marker (held 1.5 s, then falling at 20 dB/s), on a dBFS scale. Readouts: `peak hold rms avg`.
  - **VU**: per-channel VU needles, simulated on every sample as a damped spring driven by the rectified signal (IEC 60268-17: 99% of a step in 300 ms, about 1% overshoot, falling the same way). Each frame shows the highest point the needle reached, so no moment is skipped, and the marker holds the highest reading (1.5 s, then 20 dB/s). The scale is the standard VU scale, −20 to +3 VU and linear in amplitude like a real meter's needle, in the traditional two zones: green below 0 VU, red from 0 VU up. 0 VU is −18 dBFS (EBU R68; `--vu-ref -20` for SMPTE), so a −18 dBFS alignment tone reads 0 VU. Readouts: `vu hold`, in VU.
  - **LUFS**: momentary (M) and short-term (S) loudness bars on a scale from −36 to +9 LU, linear in dB (the top of EBU Tech 3341's "+9" scale with the floor of its "+18" one, deep enough for speech into a mic), with 0 LU at the target loudness (`--lufs-target`, see below; −14 LUFS by default, the level streaming services normalize to); M is soft blue and S sage green up to the target, and yellow above it; a faint olive line marks the target across the empty part of the bars and the timeline. The readouts match: `mom` in the M bar's blue, `short` in the S bar's green, anything at or over the target in yellow. The M bar's grey mark is the highest momentary loudness, and a `◀` beside the S bar points at the integrated loudness, so you can watch it settle against the target line. Readouts: `mom short integ max M` in LUFS; `tgt`, integrated loudness relative to the target (the number delivery specs and streaming normalization use; it needs a whole song or programme to settle); `LRA`, loudness range (EBU Tech 3342); and the max **true peak** in dBTP (the `TP` line) (BS.1770-4, 4× oversampled), since loudness specs limit true peak rather than sample peak. `r` starts a new measurement: integrated, LRA, max momentary and max true peak. Reset at the top of each take or song.
  - In every mode: the session's highest peak (sample peak in dBFS; true peak in dBTP in LUFS mode) and the clip count (`x` resets the sample peak and clips), a CLIP badge lit for 2 s after a clip, and the strongest tone with its musical note (shown only when a tone is louder than −50 dB and stands at least 15 dB above nearby bins, so hiss and fan hum read `—`). The VU and loudness scales top out well below full scale, so this is what shows an overload there. A two-channel input also gets a `corr  bal` line: the phase correlation of left and right over the last 300 ms (+1 is mono, around 0 wide or unrelated, below 0 out of phase, shown in red because it would cancel when played in mono) and which side is louder, in dB. The numbers refresh 10 times a second.
- **Equalizer**: the standard 1/3-octave bands (20 Hz – 20 kHz, octave centres labelled) with fast-rise, slow-fall smoothing per band and peak caps (held 1 s, then falling at 30 dB/s; hidden within 6 dB of the floor, where they would only track noise); `-b N` switches to N log-spaced bands.
- **Spectrogram**: a scrolling spectrogram on a log, mel or linear frequency axis. Each terminal cell holds two pixels.
- **Timeline**: levels over time in the meter mode: peak and RMS (RMS), the VU needle with no peak layer, since peaks mostly sit above +3 VU (VU), or momentary loudness (LUFS), in the M bar's colours: soft blue below the target, yellow above it, with the faint target line. Each column holds the highest reading of the frames it covers.
- **Piano** (off by default; `-p …,piano`, `--piano`, `-p all` or `5`): an 88-key keyboard, A0–C8, lighting the key nearest the strongest tone. When the panel is too narrow for all 88 keys, a window of keys follows the tone.

- **Scope** (off by default; `-p …,scope`, `--scope`, `-p all` or `6`): for two channels or more, a vectorscope (goniometer) of the first two, with mid (L+R) up and side (R−L) across, so mono draws a vertical line, a wide mix a round cloud and out-of-phase audio a horizontal one, over a phase correlation meter from −1 to +1. For one channel, an oscilloscope triggered on a rising zero crossing. Both scale themselves to the signal, so quiet inputs still fill the panel, and both show the frame under the cursor like every other panel.

By default it reads the default audio input. It can also read an audio file (WAV, AIFF, FLAC, MP3, Ogg Vorbis, AAC or ALAC in `.m4a`), another program's output on standard input, or a built-in test signal.

## Build

```sh
cargo build --release      # binaries: target/release/vu-meter and target/release/vu-meter-gui (cargo run starts vu-meter)
cargo test                 # unit + integration tests
cargo build --release --no-default-features   # terminal only, without the window's dependencies
```

On macOS, the terminal needs microphone permission (System Settings → Privacy & Security → Microphone). Without it the input is silent.

If a live input starts delivering pure digital silence (every sample exactly zero, which a working microphone never does) for 1.5 s, `vu-meter` reopens the device, up to three times, and shows a yellow warning in the status bar. If it stays silent, the warning says so: the microphone is probably muted or in use by another device. If a live input's sample rate changes under it (a Bluetooth headset switching profiles, say), it is never forced back: the window starts the input again at the new rate, and the terminal shows a warning. Audio arriving faster than real time is dropped, so the timeline never runs ahead of the clock.

The input is shared, never taken: it is opened the ordinary way, alongside any other apps using it (never in exclusive "hog" mode), and its sample rate and buffer size are left alone unless `--sample-rate` or `--buffer-size` ask for them. On macOS, if another app holds the device exclusively, or `--sample-rate` would change the rate another app is recording at, the status bar says so and the meter waits, starting as soon as the device can be shared. A silent Bluetooth input (AirPods taken over by a phone, say) is not reopened, as that would take it back; choose it again from the Input menu, or restart, to do that.

## Usage

```sh
vu-meter                                # meter the default input
vu-meter -l                             # list input devices
vu-meter -d "USB"                       # pick a device by name substring (or index)
vu-meter song.flac                      # play a file through the meters in real time (or: -f song.flac)
vu-meter song.mp3 --monitor             # ...and listen to it
vu-meter song.wav --fast                # analyse the whole file at once, then browse it
vu-meter song.wav --loop
ffmpeg -i film.mkv -f wav - | vu-meter -f -   # meter another program's output
vu-meter -r take.wav                    # record the input while metering
vu-meter -r take.wav -t 30              # record 30 seconds, then stop
vu-meter -g sweep:20:20000:10           # test signal
vu-meter -p spectrogram,eq              # only some panels
vu-meter -p all --cycle-views           # demo: every panel, meter modes stepping every 3 s
vu-meter --report song.wav              # print statistics, no UI
vu-meter --report *.flac --format csv   # one row per file (or --format json)
vu-meter --report --check --lufs-target podcast mix.wav   # exit 3 unless it meets the target
vu-meter --log levels.csv               # log levels and loudness once a second while metering
vu-meter -g sine:997:-20 -t 3 --report  # check the loudness meter
vu-meter --gui                          # the same meters in a window (or: vu-meter-gui)
```

Test signals for `-g`: `sine[:FREQ[:DB]]`, `square[:FREQ[:DB]]`, `noise[:DB]`, `pink[:DB]`, `sweep[:F0[:F1[:SECS[:DB[:END_DB]]]]]` (with `END_DB` the level also ramps from `DB` to `END_DB` across each sweep, e.g. `sweep:100:800:5:-20:-10`), `silence`. Every field is optional, and an empty one also takes its default, so you can set a later field alone:

| Field | Default |
|---|---|
| `FREQ` (sine, square) | 1000 Hz |
| `F0`, `F1`, `SECS` (sweep) | 20 Hz, 20000 Hz, 10 s, repeating |
| `DB` (peak level) | −6 dBFS; −12 dBFS for noise and pink |
| `END_DB` (sweep) | none: constant level |

```sh
vu-meter -g                  # 1 kHz sine at -6 dBFS
vu-meter -g sweep            # 20 Hz - 20 kHz over 10 s
vu-meter -g sine::-20        # 1 kHz at -20 dBFS
vu-meter -g sweep:100::5     # 100 Hz - 20 kHz over 5 s
vu-meter -g sweep::::-30:-6  # full sweep, level rising from -30 to -6 dBFS
```

Other options (see `vu-meter --help`): `--fft-size`, `--hop`, `--window`, `-w/--weighting a|c|z`, `-b/--bands`, `--min-freq`, `--max-freq`, `--freq-scale`, `--db-min`, `--db-max`, `--spec-floor` (spectrogram colour floor, default −90 dB: each FFT bin holds only a small share of broadband energy, so a −70 floor would hide quiet high-frequency content), `--meter-floor`, `--rms-time`, `--rms-aes17` (show RMS 3 dB higher, so a sine's RMS reads its peak, the AES17 convention VU mode already uses), `--vu`, `--lufs`, `--lufs-target`, `--vu-ref`, `--cycle-views[=SECS]` (step through the meter modes every SECS seconds, default 3), `--piano`, `--scope`, `--monitor`, `--format text|json|csv`, `--check`, `--tolerance`, `--tp-limit`, `--log`, `--log-interval`, `--surround auto|on|off`, `--completions SHELL`, `--gui`, `--foreground`, `--no-dock-meter`, `--dock-style bars|needle`, `--peak-decay`, `--peak-hold`, `--colormap`, `--color auto|truecolor|256`, `--fps`, `-z/--span SECONDS` (time shown across the charts, default 10), `--history`, `--channel`, `--channels`, `--sample-rate`, `--buffer-size`, `--record-format int16|int24|float32`, `--no-mouse`.

### Files

`-f FILE`, or just the file name, reads WAV, AIFF, FLAC, MP3, Ogg Vorbis, AAC and ALAC (`.m4a`, `.mp4`); the format is recognised from the content, with the extension as a hint. `-f -` reads any of these from standard input, so anything ffmpeg can decode can be metered: `ffmpeg -i in.opus -f wav - | vu-meter -f -`. Files play in real time (`--fast` scans them at once, `--loop` repeats them), and the status bar shows the position and the length, `t 0:42.130 / 3:51.200`.

`--monitor` (`-m`, or `o` while running) plays the file or test signal through the default output as it is metered. The output device is used at its own sample rate and channel count, which are never changed (other apps share it): the audio is converted to match, mono to both speakers and surround as its front left and right. A live input is never monitored, as a microphone next to the speakers would feed back.

A file with five or six channels is taken to be 5.0 or 5.1 in the usual order (L R C LFE Ls Rs) and its loudness measured with the BS.1770 weights: the surrounds count 1.5 dB more and the LFE is left out. `--surround off` weighs every channel the same, as a live multichannel interface always is unless `--surround on` says otherwise.

### Reports

`--report` analyses each input as fast as it can be read and prints its statistics: per-channel peak, true peak, RMS, crest factor, DC offset and clipped samples; integrated, maximum momentary and short-term loudness and loudness range; the highest true peak, the peak to loudness ratio (PLR), the phase correlation of a stereo file, the gain that would bring it to `--lufs-target` (and where that would put the true peak), and the dominant frequency.

```sh
vu-meter --report a.wav b.flac c.mp3          # a block per file
vu-meter --report *.wav --format csv > levels.csv
vu-meter --report mix.wav --format json | jq '.[0].integrated_lufs'
```

`--format json` prints an array with one object per input (values that cannot be measured, such as the loudness range of a two-second file, are `null`); `--format csv` a header and a row per input. A file that cannot be read is reported on stderr and the others carry on.

`--check` turns the report into a pass/fail test, for a build script or a batch of deliveries: integrated loudness must be within `--tolerance` LU (default 1) of `--lufs-target`, and the true peak at or under `--tp-limit` dBTP (default −1). Each report gains a `Check` line saying PASS, or FAIL and why.

| Exit status | Meaning |
| --- | --- |
| 0 | every input was read (and passed, with `--check`) |
| 1 | an error, such as a file that could not be read |
| 2 | the command line could not be understood |
| 3 | `--check` failed for at least one input |

### Logging

`--log FILE.csv` writes a row every `--log-interval` seconds (default 1) of the stream while metering, in the terminal, the window or a report: the highest sample peak, true peak and momentary loudness of the interval, the RMS of its loudest channel, the short-term, integrated and range readings at its end, the phase correlation, and whether anything clipped. Rows are flushed as they are written, so `tail -f` follows a live log; cells that have no reading yet (the loudness range in the first three seconds, correlation for mono) are empty.

```sh
vu-meter --log room.csv --log-interval 60     # a day of room noise, minute by minute
vu-meter --report album.flac --log curve.csv  # a file's loudness curve, at once
```

### Shell completions

```sh
vu-meter --completions zsh > ~/.zfunc/_vu-meter     # also bash, fish, elvish, powershell
```

### Loudness targets

`--lufs-target` takes a value in LUFS (`--lufs-target -18`) or one of these names (any case). It sets the loudness that reads 0 LU in LUFS mode; the scale runs from 36 LU below it to 9 LU above (−50 to −5 LUFS with the default).

| Name | LUFS | Target of |
| --- | --- | --- |
| `streaming` (default), `spotify`, `youtube` | −14 | streaming services' playback normalization |
| `apple` | −16 | Apple Music |
| `podcast` | −16 | podcasts, stereo |
| `podcast-mono` | −19 | podcasts, mono (BS.1770 measures one channel 3 dB below the same signal on both channels of a stereo file) |
| `broadcast`, `ebu` | −23 | European broadcast, EBU R128 |
| `atsc` | −24 | US broadcast, ATSC A/85 |
| `netflix` | −27 | Netflix (dialogue-gated) |

Broadcast targets are delivery specs, measured as integrated loudness. Streaming targets are where playback is turned down to, so louder masters are allowed but get quieter on playback. Services change these from time to time; check the current spec before delivering.

## Window

`vu-meter --gui` (or `vu-meter-gui`, which takes the same options) shows the same panels in a resizable window, drawn on the GPU with [egui](https://github.com/emilk/egui). It starts in the background and gives the terminal back (errors on startup are still shown there); `--foreground` keeps it attached, so Ctrl-C closes it and its messages, such as where a recording was saved, print in the terminal. Both front ends share the analysis, the app state and the key bindings below, so every key works in both.

On macOS the menus are in the system menu bar; elsewhere they are a menu bar in the window. Both are built from one description and set the same state as the keys:

- **File**: start / stop recording; close (⌘W) and quit (⌘Q) finish a recording properly.
- **View**: show / hide each panel (the scope included), colour map, frequency scale, zoom, chart floor and ceiling, full screen.
- **Input**: **Source** (the default input, every input device, and test signals; the list follows devices as they are plugged in and out, and choosing one starts a fresh measurement with the same display settings), pause the input, monitor the output, hold the display, back to live, oldest data, and the markers.
- **Meter**: meter mode, demo cycling, loudness target presets, VU reference, RMS as AES17, reset loudness, reset max and clips.
- **Help**: the keys and readout legend.

On macOS the Dock icon is a live level meter, as Activity Monitor's is, mirroring the meter bars of the current mode in their colours and behaviour, a few times a second:

- **RMS**: a column per channel in the green / yellow / red zones, with the dim peak envelope above the RMS level and the white hold cap.
- **VU**: a column per channel, green below 0 VU and red above, with the hold cap.
- **LUFS**: M in soft blue with its max-M cap, and S in sage green with the integrated-loudness pointer, both yellow above the target.

View → Live Dock Icon (or `--no-dock-meter`) keeps the icon still instead. View → Dock Icon as Needle Meter (or `--dock-style needle`) draws it as an analogue meter instead: a cream face with the scale arc in the mode's colours, the needle on the loudest channel (or momentary loudness in LUFS mode), a hold mark, and in LUFS mode the integrated-loudness pointer.

vu-meter follows the Mic Mode chosen under the orange microphone indicator in the menu bar (macOS). With Standard, the default, the input is the microphone's own signal. Voice Isolation and Wide Spectrum apply only to audio that goes through Apple's voice processing, so with either one chosen the input is captured through it, and its levels are the processed signal's (echo cancellation, noise suppression, automatic gain), not the microphone's. The input's name (and the window title) shows the mode in effect. Changing the mode switches the capture within a second, and the charts, statistics and any recording carry on through the switch (voice processing's mono signal is spread over the microphone's channels); only if the sample rate differs does the input start afresh. `--sample-rate`, `--channels` and `--buffer-size` need the device itself, so with any of them the mode is not followed. The menu is attributed to the app vu-meter was started from (Terminal, iTerm…).

View → Reset to Defaults puts every setting, the zoom, the Dock icon and the window size back to the built-in defaults (ignoring the command line); the input and any recording carry on.

The window remembers its settings between sessions: the panels, meter mode, colour map, frequency scale, chart floors and ceiling, loudness target, VU reference, RMS as AES17, zoom, and the Dock icon setting, as well as its position and size. The input is not remembered: it starts on the default input unless the command line names one (`-d`, `-g` or `-f`). Options given on the command line override the saved values for that run. They are kept in the app's data directory (`~/Library/Application Support/vu-meter/` on macOS).

Click or drag on the spectrogram or timeline to place the cursor; scroll to move it. Drop an audio file on the window to meter it.

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
| o | listen to a file or test signal through the default output (`--monitor`) |
| m / M | set a marker at the cursor, or remove the one there / clear every marker |
| n / N | jump to the next / previous marker |
| R | start or stop recording |
| x | reset the session max level and clip count |
| r | reset the loudness measurement: integrated, LRA, max momentary and max true peak |
| 1 2 3 4 5 6 | toggle the meter, equalizer, spectrogram, timeline, piano and scope |
| c / s | cycle the colour map / frequency scale |
| v | cycle the meter mode: RMS, VU, LUFS (meters and timeline) |
| [ ] / { } | lower or raise the chart floors (equalizer, timeline and spectrogram together) / ceiling |
| ? | help |
| q / Esc | quit |

The charts always show a whole number of seconds (10 s by default), whatever the terminal width: resizing stretches the same time range rather than showing more of it. The timeline's bottom edge (the spectrogram's, when the timeline is hidden) carries a time axis with ticks at round times, and the timeline title shows the visible range, e.g. `0:03.845 – 0:13.845 (10 s)`.

The cursor (`▼` and a vertical line on the spectrogram and timeline) marks the analysis frame that the level meters, equalizer and scope are showing. Scrolling back moves the cursor through the stored history, and End returns to live.

Markers (`◆` on the spectrogram and timeline) are places to come back to: `m` sets one at the cursor (live, that is now), `n` and `N` step through them, and their times are printed when vu-meter exits, e.g. `vu-meter: marker 2 at 1:12.405`.

## Design

- The `source` module reads audio from a cpal input stream, a file reader thread, or a generator thread. WAV files are read with hound and every other format (and standard input) decoded with [symphonia](https://github.com/pdeljanov/Symphonia), in pure Rust. The file and generator threads are paced to real time unless `--fast` is set.
- `analyzer` runs once per hop (`fft-size/4` by default; 4096 / 1024 samples, about 21 ms at 48 kHz). Each run computes a windowed real FFT (realfft) into a calibrated power spectrum, applies optional A/C weighting, sums the EQ bands, finds the dominant frequency by parabolic interpolation, measures per-channel peak and RMS with ballistics, updates BS.1770-4 K-weighted gated loudness, and for the scope keeps the phase correlation and 128 sample pairs of the hop.
- Integrated loudness and loudness range are gated over histograms of block loudness in 0.1 LU bins (each bin keeps its blocks' exact energy), so they cost the same after a day as after a minute. An hour of audio is analysed in about 9 seconds.
- All meter smoothing happens in the analyzer, per hop, and is stored in each frame, so scrolling back shows exactly what was shown live. The equalizer bands close 50% of the gap to a new reading per 1/30 s when rising and 12% when falling, clamped to the display floor and snapping the last 0.1 dB. The dominant frequency stays on the previous tone while that tone is within 3 dB of the strongest, so it does not flip between harmonics.
- The UI redraws at 30 fps by default. Each frame is wrapped in synchronized output (`ESC[?2026h … ESC[?2026l`), so terminals that support it show only whole frames.
- Each frame stores its spectrum as one byte per bin (0.5 dB steps), so the default 10 minutes of history takes about 85 MB.
- The charts write directly into the ratatui buffer. Pixel-to-bin ranges and colour lookup tables are cached, and a frame is redrawn only when data or input changes.
