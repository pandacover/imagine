//! Clip ▸ Speed Ramp: put speed markers on a clip (fast, slow on the moment that matters, fast
//! again) as keyframes on its Time Remapping ▸ Speed ([`filmcraft_project::ramp`]).
//!
//! A ramp is given as a preset over the clip's media (`hero`: fast, slow in the middle, fast;
//! `slowEnd`; `slowStart`; `speedUp`) or as `points` (`at` seconds into the clip's media, `speed`
//! percent). The clip keeps the media it showed: its length on the timeline changes to fit the
//! new speeds, and with `ripple` (the default) later material on every unlocked track moves by
//! the change. Linked sound keeps its speed (sped-up sound is rarely wanted) and is shortened
//! when the picture gets shorter. Optical Flow is switched on so the slow parts are smooth.
//! `clear` removes the ramp, and the clip plays its constant speed again.

use filmcraft_project::{Interpolation, Keyframe, ParamValue, TimeInterpolation, TrackKind};
use filmcraft_time::Tick;
use serde_json::{Value, json};

use crate::commands::{CommandSpec, bad, bool_p, clips_p, has_selection, str_p};
use crate::{EngineError, Result, Session};

const ID: &str = "clip.speedRamp";
/// Most speed points a ramp may have.
const MAX_POINTS: usize = 64;

/// Presets: (fraction of the clip's media, speed %), eased between.
const PRESETS: &[(&str, &[(f64, f64)])] = &[
    ("hero", &[(0.0, 200.0), (0.3, 200.0), (0.42, 25.0), (0.58, 25.0), (0.7, 200.0), (1.0, 200.0)]),
    ("slowEnd", &[(0.0, 100.0), (0.55, 100.0), (0.7, 25.0), (1.0, 25.0)]),
    ("slowStart", &[(0.0, 25.0), (0.3, 25.0), (0.45, 100.0), (1.0, 100.0)]),
    ("speedUp", &[(0.0, 100.0), (0.3, 100.0), (0.4, 300.0), (0.6, 300.0), (0.7, 100.0), (1.0, 100.0)]),
];

/// The speed points of `p` for a clip showing `span` of media: (media offset, speed %).
fn points(p: &Value, span: Tick) -> Result<Vec<(Tick, f64)>> {
    if let Some(list) = p.get("points").and_then(Value::as_array) {
        if list.is_empty() || list.len() > MAX_POINTS {
            return Err(bad(ID, format!("give 1 to {MAX_POINTS} points")));
        }
        let mut out = Vec::with_capacity(list.len());
        for q in list {
            let at =
                q.get("at").and_then(Value::as_f64).filter(|v| v.is_finite() && *v >= 0.0).ok_or_else(|| bad(ID, "each point needs `at` (seconds, ≥ 0)"))?;
            let speed = q
                .get("speed")
                .and_then(Value::as_f64)
                .filter(|v| v.is_finite() && *v >= 1.0 && *v <= 10_000.0)
                .ok_or_else(|| bad(ID, "each point needs `speed` (1 … 10000 %)"))?;
            let at = Tick((at * filmcraft_time::TICKS_PER_SECOND as f64).round().min(span.0 as f64) as i64);
            out.push((at, speed));
        }
        out.sort_by_key(|x| x.0);
        out.dedup_by_key(|x| x.0);
        return Ok(out);
    }
    let name = str_p(p, "preset").unwrap_or("hero");
    let (_, preset) = PRESETS
        .iter()
        .find(|(n, _)| n.eq_ignore_ascii_case(name))
        .ok_or_else(|| bad(ID, format!("unknown preset `{name}` (hero, slowEnd, slowStart, speedUp)")))?;
    Ok(preset.iter().map(|&(f, s)| (Tick((span.0 as f64 * f).round() as i64), s)).collect())
}

fn run(s: &mut Session, p: &Value) -> Result<Value> {
    let clear = bool_p(p, "clear").unwrap_or(false);
    let ease = bool_p(p, "ease").unwrap_or(true);
    let ripple = bool_p(p, "ripple").unwrap_or(true);
    let clips = clips_p(s, p);
    if clips.is_empty() {
        return Err(bad(ID, "select a video clip"));
    }
    s.edit_sequence(if clear { "Remove Speed Ramp" } else { "Speed Ramp" }, |q, _, _| {
        let mut done = Vec::new();
        for c in clips {
            let Some((tid, it)) = q.find_item(c) else { continue };
            if q.track(tid).is_none_or(|t| t.kind != TrackKind::Video || t.locked) || it.frame_hold.is_some() {
                continue;
            }
            let (start, old_end, link) = (it.start, it.end(), it.link);
            let (src_in, src_out) = (it.source_in, it.source_out());
            let span = src_out - src_in;
            if span <= Tick::ZERO {
                continue;
            }
            let keys = if clear { Vec::new() } else { points(p, span)? };
            let mut work = it.clone();
            let speed = work.effect_mut("time_remap").and_then(|e| e.params.get_mut("speed")).ok_or_else(|| bad(ID, "clip has no Time Remapping"))?;
            let interp = if ease { Interpolation::Bezier } else { Interpolation::Linear };
            speed.keyframes = keys.iter().map(|&(at, v)| Keyframe { interp, ..Keyframe::new(src_in + at, ParamValue::Float(v)) }).collect();
            // the same media, at the new speeds
            let dur = match work.speed_ramp() {
                Some(r) => r.timeline_span(src_in, src_out),
                None => Tick((span.0 as f64 / work.speed.abs().max(1e-9)).round() as i64),
            };
            // whole sequence frames: a ramp rounds up so none of the clip's media is lost (it
            // may show part of a frame more); the constant speed rounds to the nearest frame
            let rate = q.settings.frame_rate;
            let exact = dur.0 as f64 / rate.frame_duration().0.max(1) as f64;
            let frames = if clear { exact.round() } else { (exact - 1e-6).ceil() }.max(1.0) as i64;
            let dur = rate.tick_of(frames);
            work.duration = dur;
            if !clear {
                work.time_interpolation = TimeInterpolation::OpticalFlow;
            }
            let delta = dur - (old_end - start);
            let mut next = q.clone();
            if let Some((_, slot)) = next.find_item_mut(c) {
                *slot = work;
            }
            // linked sound keeps its speed, and is no longer than the picture
            for tr in next.all_tracks_mut() {
                for i in &mut tr.items {
                    if i.id != c && link.is_some() && i.link == link && i.start == start && i.duration > dur {
                        i.duration = dur;
                    }
                }
            }
            if ripple && delta != Tick::ZERO {
                for tr in next.all_tracks_mut() {
                    if tr.locked {
                        continue;
                    }
                    for i in &mut tr.items {
                        if i.id != c && i.start >= old_end {
                            i.start += delta;
                        }
                    }
                    tr.sort();
                }
            } else if delta > Tick::ZERO {
                // no ripple: the clip may only grow into the gap after it
                let room = q.track(tid).and_then(|t| t.items.iter().filter(|i| i.start >= old_end).map(|i| i.start).min());
                if let (Some(room), Some((_, slot))) = (room, next.find_item_mut(c)) {
                    slot.duration = slot.duration.min(room - start);
                }
            }
            if let Err(e) = next.check() {
                return Err(bad(ID, format!("the new clip length doesn't fit ({e}): try again with ripple on")));
            }
            *q = next;
            done.push(json!({"clip": c.0, "durationSeconds": dur.0 as f64 / filmcraft_time::TICKS_PER_SECOND as f64}));
        }
        if done.is_empty() {
            return Err(EngineError::Other("no unlocked video clip selected".into()));
        }
        Ok(json!({"clips": done}))
    })
}

/// Clip ▸ Speed Ramp ▸ <preset>: the preset on the selected clips.
macro_rules! preset_cmd {
    ($id:literal, $label:literal, $preset:literal) => {
        CommandSpec {
            id: $id,
            label: $label,
            menu: &["Clip", "Speed Ramp"],
            shortcut: None,
            params: r#"{"clips":[id]?,"ripple":bool=true}"#,
            enabled: has_selection,
            run: |s, p| {
                let mut q = p.clone();
                if !q.is_object() {
                    q = json!({});
                }
                q["preset"] = json!($preset);
                run(s, &q)
            },
            journal: true,
        }
    };
}

pub fn commands() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            id: ID,
            label: "Speed Ramp…",
            menu: &[],
            shortcut: None,
            params: r#"{"clips":[id]?,"preset":"hero|slowEnd|slowStart|speedUp"?,"points":[{"at":seconds,"speed":percent}]?,"ease":bool=true,"ripple":bool=true,"clear":bool=false}"#,
            enabled: has_selection,
            run,
            journal: true,
        },
        preset_cmd!("clip.speedRamp.hero", "Fast, Slow in the Middle, Fast", "hero"),
        preset_cmd!("clip.speedRamp.slowEnd", "Slow Down at the End", "slowEnd"),
        preset_cmd!("clip.speedRamp.slowStart", "Slow Start, Then Normal", "slowStart"),
        preset_cmd!("clip.speedRamp.speedUp", "Speed Up the Middle", "speedUp"),
        CommandSpec {
            id: "clip.speedRamp.clear",
            label: "Remove Speed Ramp",
            menu: &["Clip", "Speed Ramp"],
            shortcut: None,
            params: r#"{"clips":[id]?}"#,
            enabled: has_selection,
            run: |s, p| {
                let mut q = if p.is_object() { p.clone() } else { json!({}) };
                q["clear"] = json!(true);
                run(s, &q)
            },
            journal: true,
        },
    ]
}
