//! Input through Apple's voice-processing unit (macOS): echo cancellation,
//! noise suppression and gain control, and with them the system's Mic Mode
//! menu (Standard, Voice Isolation, Wide Spectrum) under the orange
//! microphone indicator. The signal is processed, so levels are not the
//! microphone's own.

use crate::source::Control;
use anyhow::{bail, Context, Result};
use coreaudio::audio_unit::render_callback::{data, Args};
use coreaudio::audio_unit::{AudioUnit, Element, IOType, Scope, StreamFormat};
use coreaudio_sys as sys;
use std::sync::atomic::Ordering;
use std::sync::mpsc::SyncSender;
use std::sync::Arc;

/// A running voice-processing input; stops when dropped.
pub struct VoiceInput {
    _unit: AudioUnit,
    pub sample_rate: u32,
    pub channels: usize,
}

/// Start capturing from the named device (the default input if `None`)
/// through voice processing, sending interleaved chunks to `tx`.
pub fn open(
    device: Option<&str>,
    tx: SyncSender<Vec<f32>>,
    ctl: Arc<Control>,
) -> Result<VoiceInput> {
    let mut unit = AudioUnit::new(IOType::VoiceProcessingIO).context("voice processing unit")?;
    // Configured uninitialized: the device cannot change after.
    unit.uninitialize()?;
    if let Some(name) = device {
        let id = crate::device_mac::find(name).with_context(|| format!("no device '{name}'"))?;
        unit.set_property(
            sys::kAudioOutputUnitProperty_CurrentDevice,
            Scope::Global,
            Element::Input,
            Some(&id),
        )
        .context("selecting the device for voice processing")?;
    }
    // Nothing is played, so no output; and other apps' sound is kept as loud
    // as the system allows (voice processing ducks it otherwise). Both are
    // best effort: older systems lack them.
    let _ = unit.set_property(
        sys::kAudioOutputUnitProperty_EnableIO,
        Scope::Output,
        Element::Output,
        Some(&0u32),
    );
    let _ = unit.set_property(
        sys::kAUVoiceIOProperty_OtherAudioDuckingConfiguration,
        Scope::Global,
        Element::Output,
        Some(&sys::AUVoiceIOOtherAudioDuckingConfiguration {
            mEnableAdvancedDucking: 1,
            mDuckingLevel: sys::kAUVoiceIOOtherAudioDuckingLevelMin,
        }),
    );
    unit.initialize().context("starting voice processing")?;
    // The unit picks the format it delivers in.
    let asbd = unit.get_property(
        sys::kAudioUnitProperty_StreamFormat,
        Scope::Output,
        Element::Input,
    )?;
    let format = StreamFormat::from_asbd(asbd)?;
    if format.channels != 1
        && format
            .flags
            .contains(coreaudio::audio_unit::audio_format::LinearPcmFlags::IS_NON_INTERLEAVED)
    {
        bail!(
            "voice processing delivers {} separate channels",
            format.channels
        );
    }
    unit.set_input_callback(move |args: Args<data::Interleaved<f32>>| {
        if tx.try_send(args.data.buffer.to_vec()).is_err() {
            ctl.dropped.fetch_add(1, Ordering::Relaxed);
        }
        Ok(())
    })
    .context("voice processing input")?;
    unit.start().context("starting voice processing")?;
    Ok(VoiceInput {
        _unit: unit,
        sample_rate: format.sample_rate as u32,
        channels: format.channels as usize,
    })
}
