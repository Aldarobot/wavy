// Copyright Jeron Aldaron Lau 2019 - 2020.
// Distributed under either the Apache License, Version 2.0
//    (See accompanying file LICENSE_APACHE_2_0.txt or copy at
//          https://apache.org/licenses/LICENSE-2.0),
// or the Boost Software License, Version 1.0.
//    (See accompanying file LICENSE_BOOST_1_0.txt or copy at
//          https://www.boost.org/LICENSE_1_0.txt)
// at your option. This file may not be copied, modified, or distributed except
// according to those terms.

#![allow(unsafe_code)]

use std::{
    ffi::CStr,
    mem::MaybeUninit,
    os::raw::{c_char, c_void},
};

use super::{
    free, pcm, Alsa, SndPcmAccess, SndPcmFormat, SndPcmMode, SndPcmStream,
};

pub(crate) const DEFAULT: &[u8] = b"default\0";

/// Reset hardware parameters.
pub(crate) unsafe fn reset_hwp(
    pcm: *mut c_void,
    hwp: *mut c_void,
) -> Option<()> {
    let format = if cfg!(target_endian = "little") {
        SndPcmFormat::FloatLe
    } else if cfg!(target_endian = "big") {
        SndPcmFormat::FloatBe
    } else {
        unreachable!()
    };
    pcm::hw_params_any(pcm, hwp).ok()?;
    pcm::hw_params_set_access(pcm, hwp, SndPcmAccess::RwInterleaved).ok()?;
    pcm::hw_params_set_format(pcm, hwp, format).ok()?;
    Some(())
}

/// Open a PCM Device.
pub(crate) fn open(
    name: *const c_char,
    stream: SndPcmStream,
) -> Option<(*mut c_void, *mut c_void, u8)> {
    unsafe {
        let pcm = pcm::open(name, stream, SndPcmMode::Nonblock).ok()?;
        let hwp = pcm::hw_params_malloc().ok()?;
        let mut channels = 0;
        reset_hwp(pcm, hwp)?;
        for i in 1..=8 {
            if pcm::hw_test_channels(pcm, hwp, i).is_ok() {
                channels |= 1 << (i - 1);
            }
        }
        Some((pcm, hwp, channels))
    }
}

pub(crate) trait SoundDevice:
    std::fmt::Display + From<AudioDevice>
{
    const INPUT: bool;

    fn pcm(&self) -> *mut c_void;
    fn hwp(&self) -> *mut c_void;
}

/// An Audio Device (input or output).
#[derive(Debug)]
pub(crate) struct AudioDevice {
    /// Human-readable name for the device.
    pub(crate) name: String,
    /// PCM For Device.
    pub(crate) pcm: *mut c_void,
    /// Hardware parameters for device.
    pub(crate) hwp: *mut c_void,
    /// Bitflags for numbers of channels (which of 1-8 are supported)
    pub(crate) supported: u8,
    /// File descriptors associated with this device.
    pub(crate) fds: Vec<smelling_salts::Device>,
}

impl AudioDevice {
    /// Generate file descriptors.
    pub(crate) fn start(&mut self) -> Option<()> {
        assert!(self.fds.is_empty());
        // Get file descriptor.
        let fd_list = unsafe { pcm::poll_descriptors(self.pcm).ok()? };
        // Add to list.
        for fd in fd_list {
            self.fds.push(smelling_salts::Device::new(fd.fd, unsafe {
                smelling_salts::Watcher::from_raw(fd.events as u32)
            }));
        }
        Some(())
    }
}

impl Drop for AudioDevice {
    fn drop(&mut self) {
        // Unregister async file descriptors before closing the PCM.
        for fd in &mut self.fds {
            fd.old();
        }
        // Free hardware parameters and close PCM
        unsafe {
            pcm::hw_params_free(self.hwp);
            pcm::close(self.pcm).unwrap();
        }
    }
}

/// Return a list of available audio devices.
pub(crate) fn device_list<D: SoundDevice, F: Fn(D) -> T, T>(
    abstrakt: F,
) -> Vec<T> {
    super::ALSA.with(|alsa| {
        if let Some(alsa) = alsa {
            device_list_internal(&alsa, abstrakt)
        } else {
            Vec::new()
        }
    })
}

fn device_list_internal<D: SoundDevice, F: Fn(D) -> T, T>(
    alsa: &Alsa,
    abstrakt: F,
) -> Vec<T> {
    let tpcm = CStr::from_bytes_with_nul(b"pcm\0").unwrap();
    let tname = CStr::from_bytes_with_nul(b"NAME\0").unwrap();
    let tdesc = CStr::from_bytes_with_nul(b"DESC\0").unwrap();
    let tioid = CStr::from_bytes_with_nul(b"IOID\0").unwrap();

    let mut hints = MaybeUninit::uninit();
    let mut devices = Vec::new();
    unsafe {
        if (alsa.snd_device_name_hint)(-1, tpcm.as_ptr(), hints.as_mut_ptr())
            < 0
        {
            return Vec::new();
        }
        let hints = hints.assume_init();
        let mut n = hints;
        while !(*n).is_null() {
            // Allocate 3 C Strings describing device.
            let pcm_name = (alsa.snd_device_name_get_hint)(*n, tname.as_ptr());
            let io = (alsa.snd_device_name_get_hint)(*n, tioid.as_ptr());
            debug_assert_ne!(pcm_name, std::ptr::null_mut());

            // Convert description to Rust String
            let name = match CStr::from_ptr(pcm_name).to_str() {
                Ok(x) if x.starts_with("sysdefault") => {
                    n = n.offset(1);
                    continue;
                }
                Ok("null") => {
                    // Can't use epoll on null.
                    n = n.offset(1);
                    continue;
                }
                Ok("default") => "Default".to_string(),
                _a => {
                    let name =
                        (alsa.snd_device_name_get_hint)(*n, tdesc.as_ptr());
                    assert_ne!(name, std::ptr::null_mut());
                    let rust =
                        CStr::from_ptr(name).to_string_lossy().to_string();
                    free(name.cast());
                    rust.replace("\n", ": ")
                }
            };

            // Check device io direction.
            let is_input = io.is_null() || *(io.cast::<u8>()) == b'I';
            let is_output = io.is_null() || *(io.cast::<u8>()) == b'O';
            if !io.is_null() {
                free(io.cast());
            }

            // Right input type?
            if (D::INPUT && is_input) || (!D::INPUT && is_output) {
                // Try to connect to PCM.
                let dev = open(
                    pcm_name,
                    if D::INPUT {
                        SndPcmStream::Capture
                    } else {
                        SndPcmStream::Playback
                    },
                );

                if let Some((pcm, hwp, supported)) = dev {
                    // Add device to list of devices.
                    devices.push(abstrakt(D::from(AudioDevice {
                        name,
                        pcm,
                        hwp,
                        supported,
                        fds: Vec::new(),
                    })));
                }
            }
            free(pcm_name.cast());
            n = n.offset(1);
        }
        (alsa.snd_device_name_free_hint)(hints);
    }
    devices
}
