//! Last-resort video decoding through an external `ffmpeg` program (layer L5).
//!
//! Our own decoders do not read every camera format yet: 10-bit 4:2:2 H.264 (Sony XAVC S 4:2:2,
//! XAVC S-I, Canon XF-AVC) is the common one, and HEVC 4:2:2 outside macOS. [`register`] adds a
//! fallback factory ([`filmcraft_codecs::register_fallback_video_decoder`]) that takes exactly the
//! H.264 / HEVC streams every other decoder reports as unsupported, and decodes them by piping the
//! samples through an `ffmpeg` process ([`FfmpegDecoder`]).
//!
//! FFmpeg is never linked: it runs as a separate program, found at `FILMCRAFT_FFMPEG`, next to the
//! executable (or in the macOS bundle's `Resources`), on `PATH`, or in the usual Homebrew and
//! system folders ([`binary`]). Without one, registration does nothing and those files keep
//! showing the unreadable slate.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable))]

mod decoder;
mod locate;

pub use decoder::FfmpegDecoder;
pub use locate::binary;

use filmcraft_codecs::VideoDecoder;
use filmcraft_codecs::hw::NalStreamInfo;
use filmcraft_isobmff::SampleEntry;

/// Register the FFmpeg fallback decoder. Returns the ffmpeg program it will use, or `None` (and
/// registers nothing) when none was found.
pub fn register() -> Option<std::path::PathBuf> {
    let bin = binary()?;
    filmcraft_codecs::register_fallback_video_decoder(factory);
    log::info!("FFmpeg fallback decoder: {}", bin.display());
    Some(bin.to_path_buf())
}

/// Whether [`register`] registered the fallback decoder.
pub fn registered() -> bool {
    filmcraft_codecs::fallback_video_decoder_registered(factory)
}

/// The fallback factory: H.264 (`avcC`) and HEVC (`hvcC`) streams, when an ffmpeg program exists.
pub fn factory(e: &SampleEntry) -> Option<filmcraft_codecs::Result<Box<dyn VideoDecoder>>> {
    let info = NalStreamInfo::from_entry(e)?;
    let bin = binary()?;
    Some(info.and_then(|i| FfmpegDecoder::new(i, bin.to_path_buf())).map(|d| Box::new(d) as Box<dyn VideoDecoder>))
}
