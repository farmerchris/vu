# Changelog

All notable changes to vu-meter. Versions follow [semantic versioning](https://semver.org).

## 2.0.0 — 2026-11-10

The 2.0 release turns the meter into a tool for files as much as for live inputs: it reads the common audio formats, lets you hear what it is metering, shows stereo phase, and reports and checks loudness for scripts.

### Added

- **Audio formats.** Files can be AIFF, FLAC, MP3, Ogg Vorbis, AAC or ALAC (`.m4a`) as well as WAV, decoded in pure Rust. The format is recognised from the content. WAV files that the WAV reader cannot handle (ADPCM, for one) fall back to the decoders.
- **Standard input.** `-f -` meters another program's output: `ffmpeg -i in.mkv -f wav - | vu-meter -f -`.
- **File names without `-f`.** `vu-meter song.flac` is the same as `vu-meter -f song.flac`, and `--report` takes several files.
- **Monitoring.** `--monitor` (`-m`, key `o`, Input → Monitor Output) plays a file or test signal through the default output while it is metered. The output device keeps its own sample rate and channel count; the audio is converted to match. A live input is never monitored, since it would feed back.
- **Scope panel** (`--scope`, `-p …,scope`, key `6`), in the terminal and the window: a vectorscope with a phase correlation meter for stereo, an oscilloscope for mono. It follows the cursor through the history like the other panels.
- **Stereo readouts.** The level meters show the phase correlation and left/right balance of a two-channel input; correlation below zero is red.
- **Report formats.** `--format json` and `--format csv`, one object or row per input.
- **More in the report.** DC offset per channel, the highest true peak, peak to loudness ratio, stereo correlation, and the gain needed to reach the loudness target (with the true peak that would result).
- **Compliance check.** `--report --check` tests each input against `--lufs-target` (within `--tolerance` LU) and `--tp-limit` dBTP, prints PASS or FAIL with the reason, and exits with status 3 if anything fails.
- **Level log.** `--log FILE.csv` writes peak, true peak, RMS, loudness, correlation and clipping at a fixed interval (`--log-interval`), from the terminal, the window or a report.
- **Markers.** `m` marks the cursor's place, `n` / `N` jump between markers, `M` clears them; they show on the spectrogram and timeline, and their times are printed on exit.
- **Surround loudness.** Files with five or six channels are measured with the BS.1770 channel weights (surrounds +1.5 dB, LFE left out); `--surround on|off|auto` chooses.
- **Shell completions.** `--completions bash|zsh|fish|elvish|powershell`.
- **Window:** drop an audio file on the window to meter it; the Input menu gains Monitor Output and the marker commands, the View menu the scope.
- **Status bar:** a file's length beside the position, and a MON badge while monitoring.

### Changed

- **Loudness is measured faster.** Integrated loudness and loudness range are gated over histograms instead of rescanning every block ten times a second, so the cost no longer grows with the length of the measurement. A one-hour analysis takes 8.9 s where 0.2.0 took 12.5 s on the same machine, with the same results, and that includes the new stereo measurements.
- Keys are read from the terminal device rather than standard input, which is what lets `-f -` take audio on a pipe while the meters stay interactive.
- `--report` reads its inputs from the same options as the meters, so `-t`, `--channel`, `--fft-size` and the rest apply to each file.
- The default history uses about 85 MB rather than 75 MB, for the scope's samples.
- The strongest-tone search starts at the first FFT bin at or above `--min-freq`, rather than the one below it.
- Exit status 3 means a failed `--check`; errors are still 1 and command-line mistakes 2.
- The help text, `--help` examples and the in-app key list cover the new keys.

### Fixed

- `History::range` overflowed when asked for a range ending at `usize::MAX`, returning nothing.
- The `--record` help named `r` as the key that toggles recording; it is `R` (`r` resets loudness).
- The report's true peak for channels beyond the eighth read −inf; it is now at least the sample peak.
- `vu-meter-gui --report` opened a window and ignored the option; it now says to use `vu-meter --report`.

### Compatibility

- Saved window settings from 0.2.0 are read as before.
- Every 0.2.0 option keeps its meaning. `-m` is new (monitor); positional arguments, previously an error, are now file names.
- The text report keeps its existing lines in their existing order and adds new ones after the loudness range, so scripts that pick lines by label still work.
- New dependencies: `symphonia` (decoding) and `clap_complete`.

## 0.2.0 and earlier

Not recorded release by release; in the order the work landed:

- Terminal sound meter with levels, loudness, spectrogram, equalizer and timeline.
- Time axis, zoom by round time spans, peak hold, session maximum and clip count.
- VU and LUFS meter modes, loudness targets, the piano panel and the loudness timeline.
- The window front end (`--gui`, `vu-meter-gui`) with native macOS menus and a live Dock icon.
- macOS Mic Mode is followed for live inputs.
