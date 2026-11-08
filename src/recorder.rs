//! WAV recording of the incoming stream.

use anyhow::{Context, Result};
use clap::ValueEnum;
use hound::{SampleFormat, WavSpec, WavWriter};
use std::fs::File;
use std::io::BufWriter;
use std::path::{Path, PathBuf};

#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
pub enum RecordFormat {
    Int16,
    Int24,
    Float32,
}

pub struct Recorder {
    writer: Option<WavWriter<BufWriter<File>>>,
    format: RecordFormat,
    pub path: PathBuf,
    pub samples: u64,
}

#[inline]
pub fn to_i16(s: f32) -> i16 {
    (s.clamp(-1.0, 1.0) * 32767.0).round() as i16
}

#[inline]
pub fn to_i24(s: f32) -> i32 {
    (s.clamp(-1.0, 1.0) * 8_388_607.0).round() as i32
}

/// Path for the `n`th recording this session. With a base path, later
/// recordings get a `-n` suffix; without one a timestamped name is used.
pub fn record_path(base: Option<&Path>, n: usize, unix_secs: u64) -> PathBuf {
    match base {
        Some(b) if n == 0 => b.to_path_buf(),
        Some(b) => {
            let stem = b
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "vu".into());
            let ext = b
                .extension()
                .map(|e| e.to_string_lossy().into_owned())
                .unwrap_or_else(|| "wav".into());
            b.with_file_name(format!("{stem}-{n}.{ext}"))
        }
        None if n == 0 => PathBuf::from(format!("vu-{unix_secs}.wav")),
        None => PathBuf::from(format!("vu-{unix_secs}-{n}.wav")),
    }
}

impl Recorder {
    pub fn create(
        path: &Path,
        channels: usize,
        sample_rate: u32,
        format: RecordFormat,
    ) -> Result<Self> {
        let (bits, sample_format) = match format {
            RecordFormat::Int16 => (16, SampleFormat::Int),
            RecordFormat::Int24 => (24, SampleFormat::Int),
            RecordFormat::Float32 => (32, SampleFormat::Float),
        };
        let spec = WavSpec {
            channels: channels as u16,
            sample_rate,
            bits_per_sample: bits,
            sample_format,
        };
        let writer = WavWriter::create(path, spec)
            .with_context(|| format!("creating {}", path.display()))?;
        Ok(Recorder {
            writer: Some(writer),
            format,
            path: path.to_path_buf(),
            samples: 0,
        })
    }

    pub fn write(&mut self, data: &[f32]) -> Result<()> {
        let w = self.writer.as_mut().context("recorder already finished")?;
        match self.format {
            RecordFormat::Int16 => {
                let mut i16w = w.get_i16_writer(data.len() as u32);
                for &s in data {
                    i16w.write_sample(to_i16(s));
                }
                i16w.flush()?;
            }
            RecordFormat::Int24 => {
                for &s in data {
                    w.write_sample(to_i24(s))?;
                }
            }
            RecordFormat::Float32 => {
                for &s in data {
                    w.write_sample(s)?;
                }
            }
        }
        self.samples += data.len() as u64;
        Ok(())
    }

    pub fn finish(&mut self) -> Result<()> {
        if let Some(w) = self.writer.take() {
            w.finalize()
                .with_context(|| format!("finalizing {}", self.path.display()))?;
        }
        Ok(())
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        let _ = self.finish();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::read_wav_chunk;

    fn tmp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("vu-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join(name)
    }

    #[test]
    fn conversions() {
        assert_eq!(to_i16(0.0), 0);
        assert_eq!(to_i16(1.0), 32767);
        assert_eq!(to_i16(-2.0), -32767);
        assert_eq!(to_i16(0.5), 16384);
        assert_eq!(to_i24(1.0), 8_388_607);
        assert_eq!(to_i24(-1.0), -8_388_607);
    }

    #[test]
    fn paths() {
        assert_eq!(
            record_path(Some(Path::new("out.wav")), 0, 5),
            PathBuf::from("out.wav")
        );
        assert_eq!(
            record_path(Some(Path::new("dir/out.wav")), 2, 5),
            PathBuf::from("dir/out-2.wav")
        );
        assert_eq!(record_path(None, 0, 123), PathBuf::from("vu-123.wav"));
        assert_eq!(record_path(None, 1, 123), PathBuf::from("vu-123-1.wav"));
    }

    #[test]
    fn roundtrip_all_formats() {
        let data: Vec<f32> = (0..2000).map(|i| ((i as f32) * 0.01).sin() * 0.9).collect();
        for (fmt, tol) in [
            (RecordFormat::Int16, 1e-4),
            (RecordFormat::Int24, 1e-6),
            (RecordFormat::Float32, 0.0),
        ] {
            let p = tmp(&format!("rt-{fmt:?}.wav"));
            let mut r = Recorder::create(&p, 2, 44100, fmt).unwrap();
            r.write(&data[..1000]).unwrap();
            r.write(&data[1000..]).unwrap();
            assert_eq!(r.samples, 2000);
            r.finish().unwrap();
            assert!(r.write(&data).is_err());
            let mut rd = hound::WavReader::open(&p).unwrap();
            assert_eq!(rd.spec().channels, 2);
            assert_eq!(rd.spec().sample_rate, 44100);
            let mut back = Vec::new();
            read_wav_chunk(&mut rd, 10_000, &mut back).unwrap();
            assert_eq!(back.len(), 2000);
            for (a, b) in data.iter().zip(&back) {
                assert!((a - b).abs() <= tol as f32 + 1e-7, "{fmt:?}: {a} vs {b}");
            }
            std::fs::remove_file(&p).ok();
        }
    }
}
