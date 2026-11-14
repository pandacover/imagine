# filmcraft-ffmpeg

Last-resort video decoding through an external `ffmpeg` program, for camera files our own decoders
do not read yet: 10-bit 4:2:2 H.264 (Sony XAVC S 4:2:2, XAVC S-I, Canon XF-AVC) and HEVC 4:2:2
(XAVC HS 4:2:2) when no hardware decoder takes them. See
[ADR 0002](../../docs/adr/0002-ffmpeg-fallback-decoder.md).

- `filmcraft_ffmpeg::register()` registers the fallback factory when an ffmpeg program is found
  (`FILMCRAFT_FFMPEG`, next to the executable, `PATH`, Homebrew and system folders). The desktop
  app and `filmcraft-cli` call it at start-up.
- It only takes streams that every other decoder reports as unsupported.
- FFmpeg is never linked: it runs as a separate process (ffmpeg 5.1 or later, for `-fps_mode`).

On macOS: `brew install ffmpeg`.
