# ADR 0002: an external `ffmpeg` program as the last-resort video decoder

- **Status:** accepted (2026-11-14, approved by the project owner)
- **Issue:** #626 (10-bit 4:2:2 camera media), [gaps.md G3](../gaps.md#g3-10-bit-and-422-camera-media-needs-hardware)

## Context

Our H.264 decoder reads 8-bit 4:2:0 only, and our HEVC decoder Main and Main 10 4:2:0. Most
current mirrorless cameras record 10-bit 4:2:2 (Sony XAVC S 4:2:2 and XAVC S-I, XAVC HS 4:2:2,
Canon XF-AVC, Panasonic). Hardware decoders take some of these on some machines, and the rest fail
with the unreadable slate. Files from a Sony a7 IV did not open on an Apple silicon Mac. Extending
our decoders (High 10, High 4:2:2, HEVC RExt) is planned but takes time. Editors need their camera
files to open now.

## Decision

Add `crates/ffmpeg` (`filmcraft-ffmpeg`, layer L5): a `VideoDecoder` that runs an `ffmpeg`
program as a separate process. It writes a stream's samples to the process as an Annex B byte
stream and reads raw planar pictures back. It is registered with
`filmcraft_codecs::register_fallback_video_decoder`, so it is tried only for H.264 / HEVC streams
that every other factory declines or reports as `CodecError::Unsupported`. Our decoders and the
hardware decoders keep every stream they support. To make that decision before decoding,
`h264_factory` and `hevc_factory` now report the bit depths and chroma formats they do not decode
as unsupported at creation, instead of failing on the first picture.

Rules:

1. FFmpeg is never linked. No FFmpeg code is copied or read. The program is found at
   `FILMCRAFT_FFMPEG`, next to the executable (or in the macOS bundle's `Resources`), on `PATH`,
   or in `/opt/homebrew/bin`, `/usr/local/bin` and `/usr/bin`. Without one, nothing is registered
   and behaviour is unchanged.
2. The decoder follows the never-crash rules: ffmpeg failing to start, dying, or rejecting a
   damaged stream is a `CodecError`, frame sizes are capped, and waits are bounded.
3. Output must match ffmpeg's own decode of the file exactly (tested in
   `crates/ffmpeg/src/decoder/tests.rs` for 10-bit 4:2:2 H.264 with B-frames, all-intra, and
   HEVC).
4. Bundling an ffmpeg build with releases is a separate decision. An LGPL build (no `--enable-gpl`)
   can ship next to the app as a separate program with its licence and source offer. A GPL build
   cannot. Until that is decided, users install ffmpeg themselves (`brew install ffmpeg`).

## Consequences

- Sony a7 IV / XAVC files open on every platform with ffmpeg installed.
- The ffmpeg path is slower than our frame-threaded decoders and the hardware path (one process
  per open decoder; pictures cross a pipe). It is a fallback, not the fast path.
- When our own decoders gain High 10 / 4:2:2, their factories stop reporting those streams as
  unsupported and the fallback stops being used for them, with no other change.
