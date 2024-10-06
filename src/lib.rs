//! The sound waves are _so_ wavy!
//!
//! # About
//!
//! Wavy is a library for asynchronous cross-platform real-time audio recording
//! & playback.  This library is great for if you need low-latency sound effects
//! in video games, if you're making a multi-media player, Digital Audio
//! Workstation, or building a synthesizer; anything that needs access to
//! speakers or microphones.
//!
//! ## How it works
//!
//! Wavy starts up an dedicated single-threaded async executor for audio, where
//! you can run futures dealing directly with recording or playing audio.
//! Depending on the platform, it may run on a separate thread.  When dealing
//! with real-time audio, it is important to make your code real-time safe
//! (avoid unbounded-time operations, such as syscalls).  Communicating between
//! threads is often not real-time safe, but can be using [`DualQueue`].
//!
//! # Getting Started

use event_iterator::EventIterator;
use fon::{Audio, Frame};

/// Default preferred sample rate for audio devices
pub const DEFAULT_SAMPLE_RATE: u32 = 48_000;
/// Default preferred number of chunks in the ring buffer
pub const DEFAULT_CHUNKS: usize = 8;
/// Default preferred number of frames in a chunk
pub const DEFAULT_FRAMES: usize = 32;

/// Default preferred audio device configuration
pub type DefaultAudioConfig = AudioConfig<
    DEFAULT_SAMPLE_RATE,
    DEFAULT_CHUNKS,
    DEFAULT_FRAMES,
>;

/// Configuration for an audio device
pub struct AudioConfig<
    const SAMPLE_RATE: u32,
    const CHUNKS: usize,
    const FRAMES: usize,
>;

/// [`EventIterator`] of [`MicrophoneStream`]
pub struct Microphone {}

/// [`EventIterator`] of [`SpeakersSink`]
pub struct Speakers {}

/// Chunked stream of recorded audio
pub struct MicrophoneStream {}

/// Chunked sink for audio playback
pub struct SpeakersSink {}

/// [`EventIterator`] of [`Microphone`]
pub struct MicrophoneSearcher<T = DefaultAudioConfig> {
    audio_config: T,
}

/// [`EventIterator`] of [`Speakers`]
pub struct SpeakersSearcher<T = DefaultAudioConfig> {
    audio_config: T,
}

/// [`EventIterator`] to real-time share data between async executors
pub struct DualQueue<T> {
    t: T
}
