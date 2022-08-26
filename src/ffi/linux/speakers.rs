// Copyright © 2019-2022 The Wavy Contributors.
//
// Licensed under any of:
// - Apache License, Version 2.0 (https://www.apache.org/licenses/LICENSE-2.0)
// - Boost Software License, Version 1.0 (https://www.boost.org/LICENSE_1_0.txt)
// - MIT License (https://mit-license.org/)
// At your choosing (See accompanying files LICENSE_APACHE_2_0.txt,
// LICENSE_MIT.txt and LICENSE_BOOST_1_0.txt).

#![allow(unsafe_code)]

use std::{
    fmt::{Display, Error, Formatter},
    future::Future,
    marker::PhantomData,
    os::raw::c_void,
    pin::Pin,
    sync::atomic::{AtomicBool, Ordering::SeqCst},
    task::{Context, Poll},
};

use fon::{
    chan::{Ch32, Channel},
    surround::Surround32,
    Frame, Resampler, Sink,
};

use super::{
    asound, pcm_hw_params, AudioDevice, SndPcmState, SndPcmStream, SoundDevice,
    DEFAULT,
};

/// ALSA Speakers connection.
pub(crate) struct Speakers {
    /// ALSA PCM type for both speakers and microphones.
    device: AudioDevice,
    /// Index into audio frames to start writing.
    starti: usize,
    /// Raw buffer of audio yet to be played.
    buffer: Vec<Ch32>,
    /// Resampler context for speakers sink.
    resampler: ([Ch32; 6], f64),
    /// The number of frames in the buffer.
    period: u16,
    /// Number of available channels
    pub(crate) channels: u8,
    /// The sample rate of the speakers.
    pub(crate) sample_rate: Option<f64>,
    /// Speakers are locked
    locked: AtomicBool,
}

impl SoundDevice for Speakers {
    const INPUT: bool = false;

    fn pcm(&self) -> *mut c_void {
        self.device.pcm
    }

    fn hwp(&self) -> *mut c_void {
        self.device.pcm
    }
}

impl Display for Speakers {
    fn fmt(&self, f: &mut Formatter<'_>) -> Result<(), Error> {
        f.write_str(self.device.name.as_str())
    }
}

impl From<AudioDevice> for Speakers {
    fn from(device: AudioDevice) -> Self {
        Self {
            device,
            starti: 0,
            buffer: Vec::new(),
            sample_rate: None,
            channels: 0,
            resampler: ([Ch32::MID; 6], 0.0),
            period: 0,
            locked: AtomicBool::new(false),
        }
    }
}

impl Default for Speakers {
    fn default() -> Self {
        let (pcm, hwp, supported) =
            super::open(DEFAULT.as_ptr().cast(), SndPcmStream::Playback)
                .unwrap();
        Self::from(AudioDevice {
            name: "Default".to_string(),
            pcm,
            hwp,
            supported,
            fds: Vec::new(),
        })
    }
}

impl Speakers {
    /// Attempt to configure the speaker for a specific number of channels.
    fn set_channels<F>(&mut self) -> Option<bool>
    where
        F: Frame<Chan = Ch32>,
    {
        if F::CHAN_COUNT != self.channels.into() {
            if !matches!(F::CHAN_COUNT, 1 | 2 | 6) {
                panic!("Unknown speaker configuration")
            }
            self.channels = F::CHAN_COUNT as u8;
            // Configure Hardware Parameters
            pcm_hw_params(
                &self.device,
                self.channels,
                &mut self.buffer,
                &mut self.sample_rate,
                &mut self.period,
            )?;
            Some(true)
        } else {
            Some(false)
        }
    }

    /// Generate an audio sink for the user to fill.
    pub(crate) fn play<F>(&mut self) -> SpeakersSink<F>
    where
        F: Frame<Chan = Ch32>,
    {
        // Change number of channels, if different than last call.
        self.set_channels::<F>()
            .expect("Speaker::play() called with invalid configuration");
        // Convert the resampler to the target speaker configuration.
        let resampler = Resampler::<F>::new(
            Surround32::from_channels(&self.resampler.0[..]).convert(),
            self.resampler.1,
        );
        // Create a sink that borrows this speaker's buffer mutably.
        SpeakersSink(self, resampler, PhantomData)
    }

    pub(crate) fn channels(&self) -> u8 {
        self.device.supported
    }
}

impl Future for Speakers {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        // Get mutable reference to speakers.
        let this = self.get_mut();

        // Safety
        if this.locked.load(SeqCst) {
            eprintln!("Tried to poll speakers before dropping sink");
            std::process::exit(1);
        }

        // If speaker is unconfigured, return Ready to configure and play.
        if this.channels == 0 {
            let _ = this.device.start();
            this.locked.store(true, SeqCst);
            return Poll::Ready(());
        }

        // Check if not woken, then yield.
        let mut pending = true;
        for fd in &this.device.fds {
            if !fd.should_yield() {
                pending = false;
                break;
            }
        }

        if pending {
            return Poll::Pending;
        }

        // Attempt to write remaining internal speaker buffer to the speakers.
        let result = unsafe {
            asound::pcm::writei(
                this.device.pcm,
                this.buffer.as_ptr(),
                this.period.into(),
            )
        };

        // Check if it succeeds, then return Ready.
        let len = match result {
            Ok(len) => len,
            Err(error) => {
                match error {
                    // Edge-triggered epoll should only go into pending mode if
                    // read/write call results in EAGAIN (according to epoll man
                    // page)
                    -11 => {
                        /* Pending */
                        for fd in &this.device.fds {
                            // Register waker, and then return not ready.
                            fd.register_waker(cx.waker());
                        }
                        return Poll::Pending;
                    }
                    -32 => {
                        match unsafe { asound::pcm::state(this.device.pcm) } {
                            SndPcmState::Xrun => {
                                // Player samples are not generated fast enough
                                unsafe {
                                    asound::pcm::prepare(this.device.pcm)
                                        .unwrap();
                                    asound::pcm::writei(
                                        this.device.pcm,
                                        this.buffer.as_ptr(),
                                        this.period.into(),
                                    )
                                    .unwrap()
                                }
                            }
                            st => {
                                eprintln!(
                            "Incorrect state = {:?} (XRUN): Report Bug to \
                             https://github.com/libcala/wavy/issues/new",
                            st
                        );
                                unreachable!()
                            }
                        }
                    }
                    -77 => {
                        eprintln!(
                            "Incorrect state (-EBADFD): Report Bug to \
                         https://github.com/libcala/wavy/issues/new"
                        );
                        unreachable!()
                    }
                    -86 => {
                        eprintln!(
                            "Stream got suspended, trying to recover… \
                         (-ESTRPIPE)"
                        );

                        // Prepare, so we keep getting samples.
                        unsafe {
                            // Whether this works or not, we want to prepare.
                            let _ = asound::pcm::resume(this.device.pcm);
                            // Prepare
                            asound::pcm::prepare(this.device.pcm).unwrap();
                            asound::pcm::writei(
                                this.device.pcm,
                                this.buffer.as_ptr(),
                                this.period.into(),
                            )
                            .unwrap()
                        }
                    }
                    _ => unreachable!(),
                }
            }
        };

        // Shift buffer.
        this.buffer.drain(..len * this.channels as usize);
        this.starti = this.buffer.len() / this.channels as usize;
        this.buffer
            .resize(this.period as usize * this.channels as usize, Ch32::MID);
        // Ready for more samples.
        this.locked.store(true, SeqCst);
        Poll::Ready(())
    }
}

pub(crate) struct SpeakersSink<F: Frame<Chan = Ch32>>(
    *mut Speakers,
    Resampler<F>,
    PhantomData<F>,
);

impl<F: Frame<Chan = Ch32>> Sink<F> for SpeakersSink<F> {
    fn sample_rate(&self) -> f64 {
        let speakers = unsafe { self.0.as_mut().unwrap() };
        speakers.sample_rate.unwrap()
    }

    fn resampler(&mut self) -> &mut Resampler<F> {
        &mut self.1
    }

    fn buffer(&mut self) -> &mut [F] {
        let speakers = unsafe { self.0.as_mut().unwrap() };
        let data = speakers.buffer.as_mut_ptr().cast();
        let count = speakers.period.into();
        unsafe {
            &mut std::slice::from_raw_parts_mut(data, count)[speakers.starti..]
        }
    }
}

impl<F: Frame<Chan = Ch32>> Drop for SpeakersSink<F> {
    fn drop(&mut self) {
        //
        let speakers = unsafe { self.0.as_mut().unwrap() };
        // Store 5.1 surround sample to resampler.
        let frame: Surround32 = self.1.frame().convert();
        speakers.resampler.0 = [
            frame.channels()[0],
            frame.channels()[1],
            frame.channels()[2],
            frame.channels()[3],
            frame.channels()[4],
            frame.channels()[5],
        ];
        // Store partial index from resampler.
        speakers.resampler.1 = self.1.index() % 1.0;
        // Unlock
        speakers.locked.store(false, SeqCst);
    }
}
