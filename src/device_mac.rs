//! What Core Audio knows about who else is using an input device, so a live
//! input can be shared with other apps instead of taken from them.

use coreaudio_sys::*;
use std::mem::size_of;
use std::ptr::null;

/// An input device's state, as far as sharing it goes.
#[derive(Debug, Default)]
pub struct Usage {
    /// Process holding the device exclusively (hog mode), if not this one.
    pub hog: Option<i32>,
    /// Other processes capturing from it (macOS 14 and later).
    pub capturing: Vec<i32>,
    /// Its sample rate now.
    pub rate: Option<u32>,
    pub bluetooth: bool,
    pub alive: bool,
}

fn address(selector: u32, scope: u32) -> AudioObjectPropertyAddress {
    AudioObjectPropertyAddress {
        mSelector: selector,
        mScope: scope,
        mElement: kAudioObjectPropertyElementMain,
    }
}

/// A fixed-size property value.
fn get<T: Copy>(obj: AudioObjectID, selector: u32, scope: u32) -> Option<T> {
    let addr = address(selector, scope);
    let mut value = std::mem::MaybeUninit::<T>::uninit();
    let mut size = size_of::<T>() as u32;
    // SAFETY: `value` has room for `size` bytes, which Core Audio fills.
    let status = unsafe {
        AudioObjectGetPropertyData(obj, &addr, 0, null(), &mut size, value.as_mut_ptr().cast())
    };
    (status == 0 && size as usize == size_of::<T>()).then(|| unsafe { value.assume_init() })
}

/// An array property of object ids.
fn ids(obj: AudioObjectID, selector: u32, scope: u32) -> Vec<AudioObjectID> {
    let addr = address(selector, scope);
    let mut size = 0u32;
    // SAFETY: plain property queries into buffers of the reported size.
    unsafe {
        if AudioObjectGetPropertyDataSize(obj, &addr, 0, null(), &mut size) != 0 {
            return Vec::new();
        }
        let mut v = vec![0 as AudioObjectID; size as usize / size_of::<AudioObjectID>()];
        if AudioObjectGetPropertyData(obj, &addr, 0, null(), &mut size, v.as_mut_ptr().cast()) != 0
        {
            return Vec::new();
        }
        v.truncate(size as usize / size_of::<AudioObjectID>());
        v
    }
}

fn name(obj: AudioObjectID) -> Option<String> {
    let s: CFStringRef = get(
        obj,
        kAudioDevicePropertyDeviceNameCFString,
        kAudioObjectPropertyScopeGlobal,
    )?;
    if s.is_null() {
        return None;
    }
    let mut buf = [0 as std::os::raw::c_char; 512];
    // SAFETY: `s` is a CFString we own (released below); `buf` is writable.
    let ok = unsafe {
        let ok = CFStringGetCString(s, buf.as_mut_ptr(), buf.len() as _, kCFStringEncodingUTF8);
        CFRelease(s.cast());
        ok
    };
    // SAFETY: on success the buffer holds a NUL-terminated string.
    (ok != 0).then(|| {
        unsafe { std::ffi::CStr::from_ptr(buf.as_ptr()) }
            .to_string_lossy()
            .into_owned()
    })
}

/// The device with this name (as cpal reports it).
pub(crate) fn find(device: &str) -> Option<AudioObjectID> {
    ids(
        kAudioObjectSystemObject,
        kAudioHardwarePropertyDevices,
        kAudioObjectPropertyScopeGlobal,
    )
    .into_iter()
    .find(|&d| name(d).as_deref() == Some(device))
}

/// Who is using the named input device, or `None` if it is not found.
pub fn usage(device: &str) -> Option<Usage> {
    let id = find(device)?;
    let me = std::process::id() as i32;
    let global = kAudioObjectPropertyScopeGlobal;
    let hog = get::<i32>(id, kAudioDevicePropertyHogMode, global).filter(|&p| p != -1 && p != me);
    let capturing = ids(
        kAudioObjectSystemObject,
        kAudioHardwarePropertyProcessObjectList,
        global,
    )
    .into_iter()
    .filter(|&p| get::<u32>(p, kAudioProcessPropertyIsRunningInput, global) == Some(1))
    .filter(|&p| {
        ids(
            p,
            kAudioProcessPropertyDevices,
            kAudioObjectPropertyScopeInput,
        )
        .contains(&id)
    })
    .filter_map(|p| get::<i32>(p, kAudioProcessPropertyPID, global))
    .filter(|&pid| pid != me)
    .collect();
    Some(Usage {
        hog,
        capturing,
        rate: get::<f64>(id, kAudioDevicePropertyNominalSampleRate, global).map(|r| r as u32),
        bluetooth: matches!(
            get::<u32>(id, kAudioDevicePropertyTransportType, global),
            Some(t) if t == kAudioDeviceTransportTypeBluetooth
                || t == kAudioDeviceTransportTypeBluetoothLE
        ),
        alive: get::<u32>(id, kAudioDevicePropertyDeviceIsAlive, global) != Some(0),
    })
}

// CFStringGetCString and CFRelease, used above.
#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {}

extern "C" {
    fn proc_name(pid: i32, buffer: *mut std::os::raw::c_char, size: u32) -> i32;
}

/// A process's name, or its id if that cannot be read.
pub fn process_name(pid: i32) -> String {
    let mut buf = [0 as std::os::raw::c_char; 256];
    // SAFETY: libproc writes at most `size` bytes, NUL-terminated.
    let n = unsafe { proc_name(pid, buf.as_mut_ptr(), buf.len() as u32) };
    if n <= 0 {
        return format!("process {pid}");
    }
    unsafe { std::ffi::CStr::from_ptr(buf.as_ptr()) }
        .to_string_lossy()
        .into_owned()
}
