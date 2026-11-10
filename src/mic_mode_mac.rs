//! The Mic Mode the user chose in Control Center (Standard, Voice Isolation,
//! Wide Spectrum). Only a voice-processing capture applies a mode, so a live
//! input goes through voice processing when, and only when, the user picked
//! one other than Standard; with Standard it is the microphone's own signal.

use objc2::runtime::AnyClass;
use objc2::{msg_send, sel};

// AVCaptureDevice, which reports the choice.
#[link(name = "AVFoundation", kind = "framework")]
extern "C" {}

/// AVCaptureMicrophoneMode values.
const STANDARD: isize = 0;
const WIDE_SPECTRUM: isize = 1;
const VOICE_ISOLATION: isize = 2;

/// AVCaptureDevice, if this system reports Mic Modes (macOS 12 and later).
fn device_class() -> Option<&'static AnyClass> {
    let class = AnyClass::get(c"AVCaptureDevice")?;
    class.class_method(sel!(preferredMicrophoneMode))?;
    Some(class)
}

/// Whether the user chose a mode that needs voice processing.
pub fn wants_voice() -> bool {
    // SAFETY: a class method returning an NSInteger.
    device_class().is_some_and(|c| {
        let mode: isize = unsafe { msg_send![c, preferredMicrophoneMode] };
        mode != STANDARD
    })
}

/// The mode in effect for this app's capture, by name.
pub fn active() -> Option<&'static str> {
    let c = device_class()?;
    // SAFETY: a class method returning an NSInteger.
    let mode: isize = unsafe { msg_send![c, activeMicrophoneMode] };
    Some(match mode {
        STANDARD => "Standard",
        WIDE_SPECTRUM => "Wide Spectrum",
        VOICE_ISOLATION => "Voice Isolation",
        _ => "unknown",
    })
}
