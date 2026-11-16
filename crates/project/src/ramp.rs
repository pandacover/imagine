//! Speed ramps: a clip whose speed changes while it plays (fast, then slow as the sauce pours,
//! then fast again), from keyframes on its Time Remapping ▸ Speed.
//!
//! Speed keyframes sit on moments of the footage (media time, the time every other effect
//! keyframe of the clip uses), so a marker stays on "the moment the sauce pours" whatever the speed
//! around it. The speed at media time `m` is `rate(m)` (Speed/Duration × the keyframed percentage,
//! kept within 1 % … 10 000 %), and the timeline time it takes to play the media from the clip's
//! in point to `m` is `T(m) = ∫ dm / rate(m)`. Between keyframes `T` is integrated with Simpson's
//! rule; before the first and after the last keyframe the speed is constant, so `T` is exact there.
//! The frame shown at timeline offset `t` is the `m` with `T(m) = t` (found by bisection: `T`
//! increases strictly because the speed is always positive).
//!
//! Without speed keyframes nothing here applies and a clip plays at its constant Speed/Duration.

use filmcraft_time::Tick;

use crate::keyframe::Param;
use crate::track::TrackItem;

/// Lowest and highest speed of a ramp, as a factor (1 % and 10 000 %).
const MIN_RATE: f64 = 0.01;
const MAX_RATE: f64 = 100.0;
/// Simpson subintervals per segment between two keyframes (even).
const STEPS: usize = 16;
/// At most this many keyframe segments are integrated one by one (a hostile project can hold
/// millions of keyframes); beyond it the ramp's keyframes past the limit are ignored.
const MAX_SEGMENTS: usize = 4096;
/// Bisection rounds when inverting `T` inside a segment (2⁻⁴⁸ of the segment: well under a tick).
const BISECT: usize = 48;

/// The ramp of a clip: its keyframed Time Remapping speed and constant Speed/Duration factor.
#[derive(Clone, Copy)]
pub struct SpeedRamp<'a> {
    speed: &'a Param,
    base: f64,
}

impl<'a> SpeedRamp<'a> {
    /// The clip's ramp, when its Time Remapping ▸ Speed has keyframes.
    pub fn of(item: &'a TrackItem) -> Option<Self> {
        let speed = item.effect("time_remap")?.params.get("speed").filter(|p| p.is_animated())?;
        let base = if item.speed.is_finite() && item.speed.abs() > 0.0 { item.speed.abs() } else { 1.0 };
        Some(SpeedRamp { speed, base })
    }

    /// Playback speed (media seconds per timeline second) at media time `m`.
    pub fn rate(&self, m: f64) -> f64 {
        let pct = self.speed.f64_at(Tick(m.round() as i64));
        let r = self.base * if pct.is_finite() { pct / 100.0 } else { 1.0 };
        r.clamp(MIN_RATE, MAX_RATE)
    }

    /// Keyframe media times after `from`, in order (at most [`MAX_SEGMENTS`]).
    fn breaks(&self, from: f64) -> impl Iterator<Item = f64> + '_ {
        self.speed.keyframes.iter().map(|k| k.time.0 as f64).filter(move |&k| k > from).take(MAX_SEGMENTS)
    }

    /// `∫ dm / rate(m)` from `a` to `b` (`a ≤ b`) inside one segment, by Simpson's rule.
    fn segment(&self, a: f64, b: f64) -> f64 {
        if b <= a {
            return 0.0;
        }
        let h = (b - a) / STEPS as f64;
        let f = |m: f64| 1.0 / self.rate(m);
        // the end is sampled a tick inside: a Hold keyframe at `b` starts the next speed there
        let mut s = f(a) + f((b - 1.0).max(a));
        for i in 1..STEPS {
            s += f(a + h * i as f64) * if i % 2 == 1 { 4.0 } else { 2.0 };
        }
        s * h / 3.0
    }

    /// Timeline ticks it takes to play from media time `from` to `to` (`to ≥ from`).
    pub fn timeline_span(&self, from: Tick, to: Tick) -> Tick {
        let (a, b) = (from.0 as f64, to.0 as f64);
        if b <= a {
            return Tick::ZERO;
        }
        let mut t = 0.0;
        let mut at = a;
        for k in self.breaks(a) {
            if k >= b {
                break;
            }
            t += self.segment(at, k);
            at = k;
        }
        t += self.tail(at, b);
        Tick(t.round().clamp(0.0, Tick::MAX.0 as f64) as i64)
    }

    /// The last stretch `at … b`: Simpson when a keyframe still lies ahead (the speed changes),
    /// exact when `at` is past the last keyframe (constant speed).
    fn tail(&self, at: f64, b: f64) -> f64 {
        if self.breaks(at).next().is_some() { self.segment(at, b) } else { (b - at) / self.rate(at) }
    }

    /// The media time reached `t` timeline ticks after media time `from` (`t ≥ 0`).
    pub fn media_after(&self, from: Tick, t: Tick) -> Tick {
        let want = t.0.max(0) as f64;
        let mut at = from.0 as f64;
        let mut done = 0.0;
        for k in self.breaks(at) {
            let seg = self.segment(at, k);
            if done + seg >= want {
                return Tick(self.invert(at, k, want - done).round() as i64);
            }
            done += seg;
            at = k;
        }
        // past the last keyframe: constant speed
        let m = at + (want - done) * self.rate(at);
        Tick(m.clamp(i64::MIN as f64 / 4.0, Tick::MAX.0 as f64).round() as i64)
    }

    /// The `m` in `a … b` with `segment(a, m) = need`.
    fn invert(&self, a: f64, b: f64, need: f64) -> f64 {
        let (mut lo, mut hi) = (a, b);
        for _ in 0..BISECT {
            let mid = (lo + hi) / 2.0;
            if self.segment(a, mid) < need {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        (lo + hi) / 2.0
    }
}

impl TrackItem {
    /// The clip's speed ramp, when it has speed keyframes ([`SpeedRamp`]).
    pub fn speed_ramp(&self) -> Option<SpeedRamp<'_>> {
        SpeedRamp::of(self)
    }

    /// Playback speed at timeline time `t`: the ramp's speed there, else the constant speed.
    pub fn speed_at(&self, t: Tick) -> f64 {
        match self.speed_ramp() {
            Some(r) => r.rate(self.moving_source_time_at(t).0 as f64),
            None => self.speed.abs(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keyframe::{Interpolation, Keyframe};
    use crate::{ItemId, ParamValue, find_effect};

    const S: i64 = filmcraft_time::TICKS_PER_SECOND;

    fn clip(keys: &[(f64, f64)], interp: Interpolation) -> TrackItem {
        let mut it = TrackItem {
            id: crate::ClipId(1),
            item: ItemId(1),
            name: "c".into(),
            label: crate::Label::Iris,
            start: Tick::ZERO,
            duration: Tick(10 * S),
            source_in: Tick::ZERO,
            speed: 1.0,
            reverse: false,
            enabled: true,
            link: None,
            group: None,
            effects: crate::effect::intrinsic_video(),
            markers: vec![],
            gain_db: 0.0,
            frame_hold: None,
            scale_to_frame: false,
            essential: None,
            multicam: None,
            time_interpolation: Default::default(),
            hold_filters: false,
            field_options: None,
            source_channels: Vec::new(),
            audio_stream: 0,
            graphic: None,
        };
        let mut tr = find_effect("time_remap").unwrap().instance();
        let p = tr.params.get_mut("speed").unwrap();
        p.keyframes = keys.iter().map(|&(sec, pct)| Keyframe { interp, ..Keyframe::new(Tick((sec * S as f64) as i64), ParamValue::Float(pct)) }).collect();
        it.effects.retain(|e| e.effect != "time_remap");
        it.effects.push(tr);
        it
    }

    fn secs(t: Tick) -> f64 {
        t.0 as f64 / S as f64
    }

    #[test]
    fn hold_keyframes_play_each_stretch_at_its_speed() {
        // media 0–2 s at 100 %, 2–4 s at 25 % (8 s of timeline), then 200 %
        let it = clip(&[(0.0, 100.0), (2.0, 25.0), (4.0, 200.0)], Interpolation::Hold);
        let r = it.speed_ramp().unwrap();
        assert!((secs(r.timeline_span(Tick(0), Tick(2 * S))) - 2.0).abs() < 1e-6);
        assert!((secs(r.timeline_span(Tick(2 * S), Tick(4 * S))) - 8.0).abs() < 1e-6);
        assert!((secs(r.timeline_span(Tick(4 * S), Tick(8 * S))) - 2.0).abs() < 1e-6);
        // the clip shows media 3 s at timeline 2 + 4 = 6 s, and 6 s at 10 + 1 = 11 s
        let at = |sec: f64| secs(it.moving_source_time_at(Tick((sec * S as f64) as i64)));
        assert!((at(1.0) - 1.0).abs() < 1e-6);
        assert!((at(6.0) - 3.0).abs() < 1e-6, "{}", at(6.0));
        assert!((at(11.0) - 6.0).abs() < 1e-6, "{}", at(11.0));
        assert!((it.speed_at(Tick(6 * S)) - 0.25).abs() < 1e-9);
    }

    #[test]
    fn eased_ramps_are_smooth_and_invert_exactly() {
        let it = clip(&[(0.0, 200.0), (3.0, 25.0), (6.0, 25.0), (9.0, 200.0)], Interpolation::Bezier);
        let r = it.speed_ramp().unwrap();
        let mut prev = -1.0;
        for k in 0..400 {
            let t = Tick(k * S / 20);
            let m = it.moving_source_time_at(t);
            assert!(secs(m) > prev, "media time always moves forward");
            prev = secs(m);
            // round trip: the timeline time of that media time is t
            assert!((r.timeline_span(Tick::ZERO, m) - t).0.abs() < S / 10_000, "{k}");
        }
    }

    #[test]
    fn source_out_and_without_keyframes_constant_speed() {
        let it = clip(&[(0.0, 50.0)], Interpolation::Linear);
        // one keyframe: 50 % throughout, so 10 s of timeline show 5 s of media
        assert!((secs(it.source_out()) - 5.0).abs() < 1e-6);
        let mut plain = it.clone();
        plain.effects.retain(|e| e.effect != "time_remap");
        assert!(plain.speed_ramp().is_none());
        assert_eq!(plain.moving_source_time_at(Tick(4 * S)), Tick(4 * S));
    }

    #[test]
    fn hostile_speeds_stay_finite() {
        for v in [f64::NAN, f64::INFINITY, -500.0, 0.0, 1e300] {
            let it = clip(&[(0.0, 100.0), (1.0, v), (2.0, 100.0)], Interpolation::Linear);
            let m = it.moving_source_time_at(Tick(9 * S));
            assert!(m.0 >= 0, "{v}");
            assert!(it.source_out().0 >= 0);
        }
    }
}
