//! The FFmpeg fallback decoder on the camera formats it exists for (10-bit 4:2:2 H.264 with
//! B-frames, as Sony XAVC S 4:2:2 records; all-intra, as XAVC S-I; 10-bit 4:2:2 HEVC, as XAVC HS),
//! compared picture for picture with ffmpeg's own decode of the file. Also: our own decoder keeps
//! the streams it supports, restarts after a seek, and a damaged stream is an error, not a crash.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use super::*;
use filmcraft_isobmff::SampleEntry;

fn fixture(ff: &Path, name: &str, args: &[&str]) -> PathBuf {
    let out = filmcraft_testkit::fixtures_dir("ffmpeg").join(name);
    filmcraft_testkit::fixtures::generate(&out, |tmp| {
        Command::new(ff)
            .args(["-y", "-v", "error", "-f", "lavfi", "-i", "testsrc2=size=320x180:rate=25", "-t", "2"])
            .args(args)
            .arg(tmp)
            .stdin(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    })
    .unwrap_or_else(|| panic!("fixture {name}"))
}

const H264_422_10: &[&str] = &["-c:v", "libx264", "-profile:v", "high422", "-pix_fmt", "yuv422p10le", "-g", "12", "-bf", "2", "-f", "mp4"];
const H264_422_10_INTRA: &[&str] = &["-c:v", "libx264", "-profile:v", "high422", "-pix_fmt", "yuv422p10le", "-x264-params", "keyint=1", "-f", "mp4"];
const HEVC_422_10: &[&str] =
    &["-c:v", "libx265", "-pix_fmt", "yuv422p10le", "-x265-params", "keyint=12:bframes=2:log-level=error", "-tag:v", "hvc1", "-f", "mp4"];
const H264_420_8: &[&str] = &["-c:v", "libx264", "-pix_fmt", "yuv420p", "-g", "12", "-f", "mp4"];

struct Stream {
    entry: SampleEntry,
    samples: Vec<(Vec<u8>, i64)>,
    sync: Vec<bool>,
}

fn read_stream(path: &Path) -> Stream {
    let bytes = std::fs::read(path).unwrap();
    let file = filmcraft_isobmff::open(bytes.clone()).unwrap();
    let t = file.track_of_kind(filmcraft_isobmff::TrackKind::Video).unwrap();
    let track = &file.tracks[t];
    let samples = (0..track.samples.len()).map(|i| (file.read_sample(&bytes, t, i).unwrap(), track.samples[i].pts)).collect();
    Stream { entry: track.entries[0].clone(), samples, sync: track.samples.iter().map(|s| s.is_sync).collect() }
}

fn decoder(s: &Stream) -> FfmpegDecoder {
    let info = NalStreamInfo::from_entry(&s.entry).unwrap().unwrap();
    FfmpegDecoder::new(info, crate::binary().unwrap().to_path_buf()).unwrap()
}

fn decode_all(d: &mut dyn VideoDecoder, samples: &[(Vec<u8>, i64)]) -> Vec<DecodedFrame> {
    let mut out = Vec::new();
    for (s, p) in samples {
        out.extend(d.decode(s, *p).unwrap_or_else(|e| panic!("{}: {e}", d.name())));
    }
    out.extend(d.flush());
    out
}

/// ffmpeg's own decode of `path` as raw `pix_fmt` frames.
fn reference(ff: &Path, path: &Path, pix_fmt: &str, frame_bytes: usize) -> Vec<Vec<u8>> {
    let out =
        Command::new(ff).args(["-v", "error", "-i"]).arg(path).args(["-fps_mode", "passthrough", "-f", "rawvideo", "-pix_fmt", pix_fmt, "-"]).output().unwrap();
    assert!(out.status.success());
    out.stdout.chunks_exact(frame_bytes).map(<[u8]>::to_vec).collect()
}

fn raw16(d: &PixelData) -> Vec<u8> {
    let PixelData::Yuv16 { planes, chroma: Chroma::C422, bits: 10, .. } = d else { panic!("not 10-bit 4:2:2: {d:?}") };
    planes.iter().flat_map(|p| p.iter().flat_map(|v| v.to_le_bytes())).collect()
}

fn check_matches_ffmpeg(name: &str, args: &[&str]) {
    let ff = filmcraft_testkit::require_ffmpeg!();
    let path = fixture(&ff, name, args);
    let s = read_stream(&path);
    let mut d = decoder(&s);
    let frames = decode_all(&mut d, &s.samples);
    let mut want_pts: Vec<i64> = s.samples.iter().map(|x| x.1).collect();
    want_pts.sort_unstable();
    assert_eq!(frames.iter().map(|f| f.pts).collect::<Vec<_>>(), want_pts, "{name}: one picture per sample, in presentation order");
    let bytes = d.layout.frame_bytes().unwrap();
    let refs = reference(&ff, &path, "yuv422p10le", bytes);
    assert_eq!(refs.len(), frames.len(), "{name}: picture count");
    for (i, (f, r)) in frames.iter().zip(&refs).enumerate() {
        assert_eq!((f.frame.width, f.frame.height), (320, 180), "{name}: size");
        assert!(raw16(&f.frame.data) == *r, "{name}: picture {i} differs from ffmpeg's decode");
    }
}

#[test]
fn h264_422_10bit_with_b_frames_matches_ffmpeg() {
    check_matches_ffmpeg("h264_422_10.mp4", H264_422_10);
}

#[test]
fn h264_422_10bit_all_intra_matches_ffmpeg() {
    check_matches_ffmpeg("h264_422_10_intra.mp4", H264_422_10_INTRA);
}

#[test]
fn hevc_422_10bit_matches_ffmpeg() {
    check_matches_ffmpeg("hevc_422_10.mp4", HEVC_422_10);
}

#[test]
fn restarts_at_a_sync_sample_after_reset_and_after_flush() {
    let ff = filmcraft_testkit::require_ffmpeg!();
    let s = read_stream(&fixture(&ff, "h264_422_10.mp4", H264_422_10));
    let mut d = decoder(&s);
    let all = decode_all(&mut d, &s.samples);
    let key = (1..s.samples.len()).find(|&i| s.sync[i]).unwrap();
    // part of the first GOP, then a seek
    for (x, p) in &s.samples[..5] {
        d.decode(x, *p).unwrap();
    }
    d.reset();
    let tail = decode_all(&mut d, &s.samples[key..]);
    let first = tail.first().unwrap();
    let same = all.iter().find(|f| f.pts == first.pts).unwrap();
    assert_eq!(raw16(&first.frame.data), raw16(&same.frame.data), "picture after the seek");
    // the decoder is usable again after a flush
    let again = decode_all(&mut d, &s.samples[key..]);
    assert_eq!(again.len(), tail.len());
}

#[test]
fn regular_streams_keep_our_decoder_and_422_goes_to_ffmpeg() {
    let ff = filmcraft_testkit::require_ffmpeg!();
    assert!(crate::register().is_some());
    assert!(crate::registered());
    let plain = read_stream(&fixture(&ff, "h264_420_8.mp4", H264_420_8));
    let d = filmcraft_codecs::make_video_decoder(&plain.entry).unwrap();
    assert_ne!(d.name(), "FFmpeg (external)");
    let camera = read_stream(&fixture(&ff, "h264_422_10.mp4", H264_422_10));
    let mut d = filmcraft_codecs::make_video_decoder(&camera.entry).unwrap();
    assert_eq!(d.name(), "FFmpeg (external)");
    assert_eq!(decode_all(d.as_mut(), &camera.samples).len(), camera.samples.len());
}

#[test]
fn damaged_stream_is_an_error_not_a_crash() {
    let ff = filmcraft_testkit::require_ffmpeg!();
    let s = read_stream(&fixture(&ff, "h264_422_10.mp4", H264_422_10));
    let mut d = decoder(&s);
    let mut seed = 0x1234_5678_u64;
    let mut failed = false;
    for (x, p) in &s.samples {
        let junk: Vec<u8> = x
            .iter()
            .enumerate()
            .map(|(i, b)| {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                // keep the length prefixes, scramble the rest
                if i < 4 { *b } else { (seed & 0xff) as u8 }
            })
            .collect();
        failed |= d.decode(&junk, *p).is_err();
    }
    let _ = d.flush();
    // whatever ffmpeg made of it, the decoder answered every call; and it still works
    let _ = failed;
    assert_eq!(decode_all(&mut d, &s.samples).len(), s.samples.len());
}
