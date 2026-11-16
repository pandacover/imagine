//! Video Noise Reduction: temporal then spatial noise reduction for low-light footage (food and
//! interiors shot at high ISO).
//!
//! - **Temporal:** up to four frames on each side are averaged into the current one, each pixel
//!   weighted by how alike the neighbour is there (a 3×3 box of perceptual luma), so static detail
//!   gets the full averaging and moving things keep their own frame (no ghosting). Motion
//!   Sensitivity sets how small a difference already counts as motion.
//! - **Spatial:** an edge-preserving (bilateral) filter, separately for luma and for chroma:
//!   chroma noise (the coloured blotches of high ISO) gets a wider, stronger filter than luma, so
//!   texture stays while colour speckle goes. Detail Preservation narrows the luma filter's range.
//!
//! Distances are measured on √luma (close to perceptual lightness), so shadows, where the noise
//! is, are not under-filtered. All in linear light on premultiplied pixels; alpha is kept.

use filmcraft_project::EffectInstance;
use rayon::prelude::*;

use super::{FxCtx, Image, fv, luma};

/// Pixel weight from a distance `d` at scale `s`: a cheap Cauchy-like falloff (no `exp`).
#[inline]
fn falloff(d: f32, s: f32) -> f32 {
    let q = d / s.max(1e-6);
    let r = 1.0 / (1.0 + q * q);
    r * r
}

/// √luma of every pixel.
fn perceptual(img: &Image) -> Vec<f32> {
    img.px.par_chunks(4).map(|p| luma([p[0], p[1], p[2]]).max(0.0).sqrt()).collect()
}

/// 3×3 box average of `g` (edges clamped).
fn box3(g: &[f32], w: usize, h: usize) -> Vec<f32> {
    let mut out = vec![0.0; g.len()];
    out.par_chunks_mut(w.max(1)).enumerate().for_each(|(y, row)| {
        for (x, o) in row.iter_mut().enumerate() {
            let mut s = 0.0;
            for dy in -1i64..=1 {
                let yy = (y as i64 + dy).clamp(0, h as i64 - 1) as usize;
                for dx in -1i64..=1 {
                    let xx = (x as i64 + dx).clamp(0, w as i64 - 1) as usize;
                    s += g.get(yy * w + xx).copied().unwrap_or(0.0);
                }
            }
            *o = s / 9.0;
        }
    });
    out
}

/// Temporal averaging of `img` with `neighbours` (same size; others are ignored).
pub(crate) fn temporal(img: &mut Image, neighbours: &[Image], strength: f32, sensitivity: f32) {
    let (w, h) = (img.w, img.h);
    let used: Vec<&Image> = neighbours.iter().filter(|n| n.w == w && n.h == h && n.px.len() == img.px.len()).collect();
    if used.is_empty() || strength <= 0.0 || w == 0 || h == 0 {
        return;
    }
    // a difference of `sigma` in √luma halves… quarters the weight; high sensitivity = small sigma
    let sigma = 0.012 + (1.0 - sensitivity) * 0.06;
    let cur = box3(&perceptual(img), w, h);
    let others: Vec<Vec<f32>> = used.iter().map(|n| box3(&perceptual(n), w, h)).collect();
    let src = img.px.clone();
    img.px.par_chunks_mut(4).enumerate().for_each(|(i, o)| {
        let c = cur.get(i).copied().unwrap_or(0.0);
        let mut acc = [src[i * 4], src[i * 4 + 1], src[i * 4 + 2], src[i * 4 + 3]];
        let mut wsum = 1.0;
        for (n, g) in used.iter().zip(&others) {
            let d = (g.get(i).copied().unwrap_or(0.0) - c).abs();
            let wt = strength * falloff(d, sigma);
            if wt < 1e-4 {
                continue;
            }
            for k in 0..4 {
                acc[k] += wt * n.px[i * 4 + k];
            }
            wsum += wt;
        }
        for k in 0..4 {
            o[k] = acc[k] / wsum;
        }
    });
}

/// Edge-preserving spatial filtering: luma within `luma_radius` at range `luma_sigma`, chroma within
/// `chroma_radius` at range `chroma_sigma` (√luma units; 0 = off).
pub(crate) fn spatial(img: &mut Image, luma_radius: usize, luma_sigma: f32, chroma_radius: usize, chroma_sigma: f32) {
    let (w, h) = (img.w, img.h);
    let lr = if luma_sigma > 0.0 { luma_radius } else { 0 };
    let cr = if chroma_sigma > 0.0 { chroma_radius } else { 0 };
    let r = lr.max(cr);
    if r == 0 || w == 0 || h == 0 {
        return;
    }
    let g = perceptual(img);
    let y: Vec<f32> = img.px.par_chunks(4).map(|p| luma([p[0], p[1], p[2]])).collect();
    let src = img.px.clone();
    // spatial weights: Gaussian-like falloff at a third of each radius
    let sw = |dx: i64, dy: i64, rad: usize| -> f32 {
        if rad == 0 || dx.unsigned_abs() as usize > rad || dy.unsigned_abs() as usize > rad {
            return 0.0;
        }
        let s = (rad as f32 / 2.0).max(0.5);
        falloff(((dx * dx + dy * dy) as f32).sqrt(), s * 1.6)
    };
    let ri = r as i64;
    img.px.par_chunks_mut(w * 4).enumerate().for_each(|(yy, row)| {
        for x in 0..w {
            let i = yy * w + x;
            let gc = g[i];
            let (mut ly, mut lw) = (0.0f32, 0.0f32);
            let (mut cc, mut cw) = ([0.0f32; 3], 0.0f32);
            for dy in -ri..=ri {
                let ny = (yy as i64 + dy).clamp(0, h as i64 - 1) as usize;
                for dx in -ri..=ri {
                    let nx = (x as i64 + dx).clamp(0, w as i64 - 1) as usize;
                    let j = ny * w + nx;
                    let d = (g[j] - gc).abs();
                    let wl = sw(dx, dy, lr) * if lr > 0 { falloff(d, luma_sigma) } else { 0.0 };
                    if wl > 0.0 {
                        ly += wl * y[j];
                        lw += wl;
                    }
                    let wc = sw(dx, dy, cr) * if cr > 0 { falloff(d, chroma_sigma) } else { 0.0 };
                    if wc > 0.0 {
                        for k in 0..3 {
                            cc[k] += wc * (src[j * 4 + k] - y[j]);
                        }
                        cw += wc;
                    }
                }
            }
            let yc = y[i];
            let yo = if lw > 0.0 { ly / lw } else { yc };
            let o = &mut row[x * 4..x * 4 + 3];
            for k in 0..3 {
                let chroma = if cw > 0.0 { cc[k] / cw } else { src[i * 4 + k] - yc };
                o[k] = yo + chroma;
            }
        }
    });
}

/// The Video Noise Reduction effect.
pub fn video_denoise(img: &mut Image, e: &EffectInstance, cx: &FxCtx) {
    let frames = fv(e, "temporal_frames", cx).clamp(0.0, 4.0).round() as usize;
    let strength = fv(e, "temporal", cx).clamp(0.0, 100.0) / 100.0;
    let sensitivity = fv(e, "motion", cx).clamp(0.0, 100.0) / 100.0;
    let luma_amt = fv(e, "luma", cx).clamp(0.0, 100.0) / 100.0;
    let chroma_amt = fv(e, "chroma", cx).clamp(0.0, 100.0) / 100.0;
    let detail = fv(e, "detail", cx).clamp(0.0, 100.0) / 100.0;
    if frames > 0
        && strength > 0.0
        && let Some(env) = cx.env
    {
        let fps = if cx.frame_rate > 0.0 { cx.frame_rate } else { env.frame_rate().max(1.0) };
        let clip = env.clip_seconds();
        let now = env.clip_offset();
        let neighbours: Vec<Image> = (1..=frames)
            .flat_map(|k| [-(k as f64), k as f64])
            .filter(|k| (0.0..clip).contains(&(now + k / fps)))
            .filter_map(|k| env.frame(k / fps, e))
            .collect();
        temporal(img, &neighbours, strength, sensitivity);
    }
    // radii scale with the working size (previews at reduced resolution filter less far)
    let scale = cx.px_scale.clamp(0.05, 4.0);
    let rad = |base: f32| ((base * scale).round() as usize).clamp(1, 8);
    let luma_sigma = luma_amt * 0.045 * (1.0 - 0.7 * detail);
    let chroma_sigma = chroma_amt * 0.09;
    spatial(img, rad(2.0), luma_sigma, rad(4.0), chroma_sigma);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn noise(seed: &mut u64) -> f32 {
        // xorshift, roughly Gaussian (sum of uniforms)
        let mut s = 0.0;
        for _ in 0..4 {
            *seed ^= *seed << 13;
            *seed ^= *seed >> 7;
            *seed ^= *seed << 17;
            s += (*seed % 10_000) as f32 / 10_000.0;
        }
        s / 4.0 - 0.5
    }

    /// A dark grey picture with a bright right half, plus luma and chroma noise.
    fn noisy(seed: u64, amp: f32) -> Image {
        let (w, h) = (64, 48);
        let mut img = Image::new(w, h);
        let mut s = seed;
        for y in 0..h {
            for x in 0..w {
                let base = if x >= w / 2 { 0.6 } else { 0.05 };
                let n = noise(&mut s) * amp;
                let c = [noise(&mut s) * amp, noise(&mut s) * amp];
                let i = (y * w + x) * 4;
                img.px[i] = (base + n + c[0]).max(0.0);
                img.px[i + 1] = (base + n - c[0] - c[1]).max(0.0);
                img.px[i + 2] = (base + n + c[1]).max(0.0);
                img.px[i + 3] = 1.0;
            }
        }
        img
    }

    /// Standard deviation of channel `k` over the left (dark) half, away from the edge.
    fn spread(img: &Image, k: usize) -> f32 {
        let v: Vec<f32> = (0..img.h).flat_map(|y| (2..img.w / 2 - 6).map(move |x| (y, x))).map(|(y, x)| img.px[(y * img.w + x) * 4 + k]).collect();
        let m = v.iter().sum::<f32>() / v.len() as f32;
        (v.iter().map(|a| (a - m) * (a - m)).sum::<f32>() / v.len() as f32).sqrt()
    }

    fn edge_step(img: &Image) -> f32 {
        let row = img.h / 2;
        let at = |x: usize| img.px[(row * img.w + x) * 4 + 1];
        at(img.w / 2 + 1) - at(img.w / 2 - 2)
    }

    #[test]
    fn spatial_removes_noise_and_keeps_the_edge() {
        let src = noisy(7, 0.04);
        let mut img = src.clone();
        spatial(&mut img, 2, 0.045, 4, 0.09);
        for k in 0..3 {
            assert!(spread(&img, k) < 0.5 * spread(&src, k), "channel {k}: {} vs {}", spread(&img, k), spread(&src, k));
        }
        assert!(edge_step(&img) > 0.9 * edge_step(&src), "edge {} vs {}", edge_step(&img), edge_step(&src));
        assert!(img.px.iter().all(|v| v.is_finite()));
    }

    #[test]
    fn temporal_averages_static_pixels_and_leaves_motion() {
        let frames: Vec<Image> = (1..=4).map(|s| noisy(100 + s, 0.04)).collect();
        let src = noisy(1, 0.04);
        let mut img = src.clone();
        temporal(&mut img, &frames, 1.0, 0.5);
        assert!(spread(&img, 1) < 0.65 * spread(&src, 1), "{} vs {}", spread(&img, 1), spread(&src, 1));
        // a neighbour where something bright moved into the dark half: not averaged in there
        let mut moved = noisy(200, 0.0);
        for y in 0..moved.h {
            for x in 0..8 {
                let i = (y * moved.w + x) * 4;
                moved.px[i..i + 3].copy_from_slice(&[0.9, 0.9, 0.9]);
            }
        }
        let mut img = noisy(1, 0.0);
        temporal(&mut img, &[moved], 1.0, 0.5);
        assert!(img.px[(10 * img.w + 3) * 4] < 0.1, "ghost: {}", img.px[(10 * img.w + 3) * 4]);
        // wrong sizes and zero strength change nothing
        let mut img = src.clone();
        temporal(&mut img, &[Image::new(3, 3)], 1.0, 0.5);
        temporal(&mut img, &frames, 0.0, 0.5);
        assert_eq!(img.px, src.px);
    }
}
