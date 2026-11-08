//! Bounded history of analysis frames addressed by absolute index.

use crate::analyzer::Frame;
use std::collections::VecDeque;

pub struct History {
    frames: VecDeque<Frame>,
    cap: usize,
    base: usize,
}

impl History {
    pub fn new(cap: usize) -> Self {
        History { frames: VecDeque::with_capacity(cap.min(1 << 16)), cap: cap.max(1), base: 0 }
    }

    pub fn push(&mut self, f: Frame) {
        if self.frames.len() == self.cap {
            self.frames.pop_front();
            self.base += 1;
        }
        self.frames.push_back(f);
    }

    /// Absolute index of the oldest retained frame.
    pub fn first(&self) -> usize {
        self.base
    }

    /// Absolute index of the newest frame.
    pub fn last(&self) -> Option<usize> {
        if self.frames.is_empty() {
            None
        } else {
            Some(self.base + self.frames.len() - 1)
        }
    }

    pub fn len(&self) -> usize {
        self.frames.len()
    }

    pub fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }

    pub fn capacity(&self) -> usize {
        self.cap
    }

    pub fn get(&self, i: usize) -> Option<&Frame> {
        i.checked_sub(self.base).and_then(|k| self.frames.get(k))
    }

    /// Frames with absolute indices in `[a, b]`, clipped to what is retained.
    pub fn range(&self, a: usize, b: usize) -> impl Iterator<Item = &Frame> {
        let lo = a.saturating_sub(self.base).min(self.frames.len());
        let hi = (b + 1).saturating_sub(self.base).min(self.frames.len()).max(lo);
        self.frames.range(lo..hi)
    }

    /// Whether any frame in the `n` frames ending at `end` clipped.
    pub fn clipped(&self, end: usize, n: usize) -> bool {
        self.range(end.saturating_sub(n.saturating_sub(1)), end).any(|f| f.clipped)
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use crate::analyzer::MAX_CHANNELS;
    use crate::dsp::level::FLOOR_DB;

    pub fn frame(peak: f32, bands: &[u8]) -> Frame {
        let mut p = [FLOOR_DB; MAX_CHANNELS];
        p[0] = peak;
        p[1] = peak - 1.0;
        Frame {
            time: 0.0,
            channels: 2,
            peak: p,
            peak_decay: p,
            rms: p,
            rms_avg: p,
            hold: p,
            vu: p,
            vu_hold: p,
            lufs_m: f32::NEG_INFINITY,
            lufs_s: f32::NEG_INFINITY,
            lufs_i: f32::NEG_INFINITY,
            peak_freq: None,
            clipped: peak >= 0.0,
            spectrum: vec![0u8; 8].into_boxed_slice(),
            bands: bands.to_vec().into_boxed_slice(),
            band_caps: bands.to_vec().into_boxed_slice(),
        }
    }

    #[test]
    fn indices_survive_eviction() {
        let mut h = History::new(3);
        assert!(h.is_empty());
        assert_eq!(h.last(), None);
        for i in 0..5 {
            h.push(frame(-(i as f32), &[]));
        }
        assert_eq!(h.len(), 3);
        assert_eq!(h.capacity(), 3);
        assert_eq!(h.first(), 2);
        assert_eq!(h.last(), Some(4));
        assert!(h.get(1).is_none());
        assert_eq!(h.get(2).unwrap().peak[0], -2.0);
        assert_eq!(h.get(4).unwrap().peak[0], -4.0);
        assert!(h.get(5).is_none());
    }

    #[test]
    fn range_clips() {
        let mut h = History::new(10);
        for i in 0..10 {
            h.push(frame(i as f32, &[]));
        }
        assert_eq!(h.range(3, 5).count(), 3);
        assert_eq!(h.range(8, 100).count(), 2);
        assert_eq!(h.range(20, 30).count(), 0);
        assert_eq!(h.range(5, 2).count(), 0);
    }

    #[test]
    fn clip_window() {
        let mut h = History::new(10);
        h.push(frame(-10.0, &[1, 9]));
        h.push(frame(-3.0, &[5, 2]));
        h.push(frame(-20.0, &[3, 3]));
        assert!(!h.clipped(2, 3));
        h.push(frame(0.0, &[0, 0]));
        assert!(h.clipped(3, 1));
    }
}
