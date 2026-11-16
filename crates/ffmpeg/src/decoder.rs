//! [`FfmpegDecoder`]: a [`VideoDecoder`] that pipes H.264 / HEVC samples through an `ffmpeg`
//! process and reads raw planar pictures back.
//!
//! One process runs per decoder from the first sample after a [`VideoDecoder::reset`] to the next
//! [`VideoDecoder::flush`] or reset. Samples go to its stdin as an Annex B byte stream (the sample
//! entry's parameter sets first), and a reader thread collects the pictures it writes to stdout.
//! ffmpeg outputs every picture once, in presentation order, without our timestamps, so the k-th
//! picture out gets the k-th smallest pts fed in since the restart.

use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use filmcraft_codecs::hw::{NalCodec, NalStreamInfo};
use filmcraft_codecs::{CodecError, DecodedFrame, Result, VideoDecoder};
use filmcraft_frame::{Chroma, PixelData, VideoFrame};

/// The largest picture accepted (bytes of one raw output frame): 8K 4:4:4 at 16 bits is ~600 MB.
const MAX_FRAME_BYTES: usize = 1 << 30;
/// How long to wait for the first picture of a run (process start-up and stream probing).
const FIRST_WAIT: Duration = Duration::from_secs(10);
/// How long to wait for a picture ffmpeg should already have produced.
const WAIT: Duration = Duration::from_millis(300);
/// How long [`VideoDecoder::flush`] waits for ffmpeg to finish.
const FLUSH_WAIT: Duration = Duration::from_secs(60);

/// Raw output layout: ffmpeg's `-pix_fmt` and the planes it implies.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Layout {
    width: usize,
    height: usize,
    chroma: Chroma,
    /// 8, 10, 12 or 16.
    bits: u32,
}

impl Layout {
    fn of(info: &NalStreamInfo) -> Result<Self> {
        let (w, h) = info.size();
        if w == 0 || h == 0 || w > 16384 || h > 16384 {
            return Err(CodecError::Unsupported(format!("picture size {w}x{h}")));
        }
        let chroma = match info.chroma_format_idc {
            2 => Chroma::C422,
            3 => Chroma::C444,
            // 4:2:0, and monochrome (ffmpeg adds neutral chroma)
            _ => Chroma::C420,
        };
        let bits = match info.bit_depth_luma.max(info.bit_depth_chroma) {
            0..=8 => 8,
            9..=10 => 10,
            11..=12 => 12,
            _ => 16,
        };
        let l = Self { width: w as usize, height: h as usize, chroma, bits };
        if l.frame_bytes().is_none() {
            return Err(CodecError::Unsupported(format!("picture size {w}x{h}")));
        }
        Ok(l)
    }

    fn pix_fmt(&self) -> String {
        let c = match self.chroma {
            Chroma::C420 => "420",
            Chroma::C422 => "422",
            Chroma::C444 => "444",
        };
        if self.bits == 8 { format!("yuv{c}p") } else { format!("yuv{c}p{}le", self.bits) }
    }

    fn chroma_size(&self) -> (usize, usize) {
        let (sx, sy) = self.chroma.shifts();
        (self.width.div_ceil(1 << sx), self.height.div_ceil(1 << sy))
    }

    /// Samples per plane: luma, then each chroma plane.
    fn samples(&self) -> Option<(usize, usize)> {
        let (cw, ch) = self.chroma_size();
        Some((self.width.checked_mul(self.height)?, cw.checked_mul(ch)?))
    }

    fn frame_bytes(&self) -> Option<usize> {
        let (y, c) = self.samples()?;
        let n = y.checked_add(c.checked_mul(2)?)?;
        let bytes = if self.bits == 8 { n } else { n.checked_mul(2)? };
        (bytes <= MAX_FRAME_BYTES).then_some(bytes)
    }

    /// One raw frame as planes.
    fn planes(&self, raw: &[u8]) -> Option<PixelData> {
        let (y, c) = self.samples()?;
        if self.bits == 8 {
            let (py, rest) = raw.split_at_checked(y)?;
            let (pu, pv) = rest.split_at_checked(c)?;
            let pv = pv.get(..c)?;
            return Some(PixelData::Yuv8 { planes: [Arc::new(py.to_vec()), Arc::new(pu.to_vec()), Arc::new(pv.to_vec())], chroma: self.chroma, alpha: None });
        }
        let wide = |b: &[u8]| -> Vec<u16> { b.chunks_exact(2).filter_map(|p| p.try_into().ok()).map(u16::from_le_bytes).collect() };
        let (py, rest) = raw.split_at_checked(y.checked_mul(2)?)?;
        let (pu, pv) = rest.split_at_checked(c.checked_mul(2)?)?;
        let pv = pv.get(..c.checked_mul(2)?)?;
        Some(PixelData::Yuv16 { planes: [Arc::new(wide(py)), Arc::new(wide(pu)), Arc::new(wide(pv))], chroma: self.chroma, bits: self.bits, alpha: None })
    }
}

/// What the reader thread sends.
enum Msg {
    Frame(Vec<u8>),
    /// stdout closed (ffmpeg finished or died).
    End,
}

/// A running ffmpeg process.
struct Run {
    child: Child,
    stdin: Option<ChildStdin>,
    rx: Receiver<Msg>,
    reader: Option<JoinHandle<()>>,
    stderr: Arc<Mutex<String>>,
    ended: bool,
    started: Instant,
}

impl Run {
    fn error(&self, what: &str) -> CodecError {
        let log = self.stderr.lock().unwrap_or_else(PoisonError::into_inner);
        let detail = log.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("no message").trim().to_string();
        CodecError::Decode(format!("ffmpeg {what}: {detail}"))
    }

    fn stop(&mut self) {
        self.stdin = None;
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(r) = self.reader.take() {
            let _ = r.join();
        }
    }
}

impl Drop for Run {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Keep ffmpeg from opening a console window on Windows.
pub(crate) fn no_window(c: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        c.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(not(windows))]
    let _ = c;
}

/// Decodes H.264 / HEVC samples with an external ffmpeg program (see the module docs).
pub struct FfmpegDecoder {
    info: NalStreamInfo,
    bin: PathBuf,
    layout: Layout,
    threads: usize,
    run: Option<Run>,
    /// Timestamps fed since the restart that have no picture yet (smallest first).
    pending: BinaryHeap<Reverse<i64>>,
    /// Samples fed and pictures received since the restart.
    fed: usize,
    got: usize,
    /// Pictures ffmpeg may hold back before it outputs one (reordering, frame threads and the
    /// parser's one access unit of look-ahead); grows when ffmpeg turns out to hold more.
    window: usize,
}

impl FfmpegDecoder {
    pub fn new(info: NalStreamInfo, bin: PathBuf) -> Result<Self> {
        let layout = Layout::of(&info)?;
        let threads = std::thread::available_parallelism().map_or(4, |n| n.get()).clamp(1, 8);
        let window = info.reorder.saturating_add(threads).saturating_add(2);
        Ok(Self { info, bin, layout, threads, run: None, pending: BinaryHeap::new(), fed: 0, got: 0, window })
    }

    fn spawn(&self) -> Result<Run> {
        let format = match self.info.codec {
            NalCodec::H264 => "h264",
            NalCodec::Hevc => "hevc",
        };
        let threads = self.threads.to_string();
        let pix_fmt = self.layout.pix_fmt();
        let mut c = Command::new(&self.bin);
        c.args(["-hide_banner", "-nostdin", "-loglevel", "error", "-probesize", "32", "-analyzeduration", "0", "-f", format, "-threads", &threads])
            .args(["-i", "pipe:0", "-map", "0:v:0", "-an", "-sn", "-fps_mode", "passthrough", "-f", "rawvideo", "-pix_fmt", &pix_fmt, "pipe:1"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        no_window(&mut c);
        let mut child = c.spawn().map_err(|e| CodecError::Decode(format!("cannot start ffmpeg ({}): {e}", self.bin.display())))?;
        let (Some(stdin), Some(mut stdout), Some(mut err_pipe)) = (child.stdin.take(), child.stdout.take(), child.stderr.take()) else {
            let _ = child.kill();
            let _ = child.wait();
            return Err(CodecError::Decode("ffmpeg pipes unavailable".into()));
        };
        let stderr = Arc::new(Mutex::new(String::new()));
        let log = Arc::clone(&stderr);
        std::thread::Builder::new()
            .name("ffmpeg-stderr".into())
            .spawn(move || {
                let mut s = String::new();
                let _ = err_pipe.read_to_string(&mut s);
                // keep the tail: ffmpeg can repeat an error for every picture
                let tail: String = s.chars().rev().take(4096).collect::<Vec<_>>().into_iter().rev().collect();
                *log.lock().unwrap_or_else(PoisonError::into_inner) = tail;
            })
            .map_err(|e| CodecError::Decode(format!("ffmpeg log thread: {e}")))?;
        let frame_bytes = self.layout.frame_bytes().ok_or_else(|| CodecError::Unsupported("picture too large".into()))?;
        let (tx, rx) = channel();
        let reader = std::thread::Builder::new()
            .name("ffmpeg-frames".into())
            .spawn(move || {
                loop {
                    let mut buf = vec![0u8; frame_bytes];
                    if stdout.read_exact(&mut buf).is_err() || tx.send(Msg::Frame(buf)).is_err() {
                        break;
                    }
                }
                let _ = tx.send(Msg::End);
            })
            .map_err(|e| CodecError::Decode(format!("ffmpeg reader thread: {e}")))?;
        let mut run = Run { child, stdin: Some(stdin), rx, reader: Some(reader), stderr, ended: false, started: Instant::now() };
        let mut head = Vec::new();
        for p in &self.info.parameter_sets {
            head.extend_from_slice(&[0, 0, 0, 1]);
            head.extend_from_slice(p);
        }
        if let Some(w) = run.stdin.as_mut()
            && w.write_all(&head).is_err()
        {
            return Err(run.error("did not start"));
        }
        Ok(run)
    }

    /// A length-prefixed sample as Annex B (start code before every NAL unit).
    fn annexb(&self, sample: &[u8]) -> Vec<u8> {
        let nals = self.info.nals(sample);
        let mut out = Vec::with_capacity(sample.len().saturating_add(nals.len().saturating_mul(4)));
        for n in nals {
            out.extend_from_slice(&[0, 0, 0, 1]);
            out.extend_from_slice(n);
        }
        out
    }

    fn picture(&mut self, raw: &[u8]) -> Option<DecodedFrame> {
        let Reverse(pts) = self.pending.pop()?;
        let data = self.layout.planes(raw)?;
        let frame = VideoFrame {
            width: self.layout.width as u32,
            height: self.layout.height as u32,
            data,
            color: self.info.color,
            par: self.info.par,
            pts: filmcraft_time::Tick::ZERO,
        };
        Some(DecodedFrame { pts, frame, draft: false })
    }

    /// Collect pictures: everything already received, then wait (briefly) until at most
    /// `window` fed samples are still without a picture, or for all of them when `all`.
    fn collect(&mut self, all: bool) -> Result<Vec<DecodedFrame>> {
        let mut raws = Vec::new();
        let Some(run) = self.run.as_mut() else { return Ok(Vec::new()) };
        let started = Instant::now();
        loop {
            let behind = self.fed.saturating_sub(self.got.saturating_add(raws.len()));
            let enough = if all { behind == 0 } else { behind <= self.window };
            let msg = match run.rx.try_recv() {
                Ok(m) => Some(m),
                Err(_) if enough || run.ended => None,
                Err(_) => {
                    // the first picture of a run may take a while (start-up, probing), but only
                    // once per run; later ones are owed after a short wait
                    let limit = if all {
                        FLUSH_WAIT.saturating_sub(started.elapsed())
                    } else if self.got.saturating_add(raws.len()) == 0 {
                        FIRST_WAIT.saturating_sub(run.started.elapsed()).max(WAIT.saturating_sub(started.elapsed()))
                    } else {
                        WAIT.saturating_sub(started.elapsed())
                    };
                    match run.rx.recv_timeout(limit) {
                        Ok(m) => Some(m),
                        Err(RecvTimeoutError::Timeout) => {
                            if !all {
                                // ffmpeg holds more pictures than expected: wait for fewer next time
                                self.window = self.window.saturating_add(1);
                            }
                            None
                        }
                        Err(RecvTimeoutError::Disconnected) => {
                            run.ended = true;
                            None
                        }
                    }
                }
            };
            match msg {
                Some(Msg::Frame(f)) => raws.push(f),
                Some(Msg::End) => run.ended = true,
                None => break,
            }
        }
        let ended = run.ended;
        let error = (ended && raws.is_empty() && self.got == 0 && self.fed > 0).then(|| run.error("could not decode the stream"));
        self.got = self.got.saturating_add(raws.len());
        let out: Vec<DecodedFrame> = raws.iter().filter_map(|r| self.picture(r)).collect();
        if let Some(e) = error {
            self.run = None;
            return Err(e);
        }
        Ok(out)
    }

    fn restart(&mut self) {
        self.run = None;
        self.pending.clear();
        self.fed = 0;
        self.got = 0;
    }
}

impl VideoDecoder for FfmpegDecoder {
    fn decode(&mut self, sample: &[u8], pts: i64) -> Result<Vec<DecodedFrame>> {
        if self.run.as_ref().is_some_and(|r| r.ended || r.stdin.is_none()) {
            // finished after a flush: start again
            self.restart();
        }
        if self.run.is_none() {
            self.run = Some(self.spawn()?);
        }
        let data = self.annexb(sample);
        let Some(run) = self.run.as_mut() else { return Err(CodecError::Decode("ffmpeg not running".into())) };
        let written = run.stdin.as_mut().is_some_and(|w| w.write_all(&data).and_then(|()| w.flush()).is_ok());
        if !written {
            let e = run.error("stopped");
            self.restart();
            return Err(e);
        }
        self.pending.push(Reverse(pts));
        self.fed = self.fed.saturating_add(1);
        self.collect(false)
    }

    fn flush(&mut self) -> Vec<DecodedFrame> {
        let Some(run) = self.run.as_mut() else { return Vec::new() };
        // closing stdin makes ffmpeg drain its pictures and exit
        run.stdin = None;
        let out = self.collect(true).unwrap_or_else(|e| {
            log::warn!("{e}");
            Vec::new()
        });
        self.restart();
        out
    }

    fn reset(&mut self) {
        self.restart();
    }

    fn name(&self) -> &str {
        "FFmpeg (external)"
    }

    fn is_disposable(&self, sample: &[u8]) -> bool {
        self.info.is_disposable(sample)
    }
}

#[cfg(test)]
mod tests;
