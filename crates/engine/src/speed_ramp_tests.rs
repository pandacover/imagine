//! Clip ▸ Speed Ramp: the clip keeps its media, gets longer where it is slow, later clips move,
//! the razor keeps the ramp continuous, and Clear goes back to the constant speed.

use filmcraft_media::DemoScene;
use filmcraft_time::{TICKS_PER_SECOND as S, Tick};
use serde_json::json;

use crate::media_test_util::{make_movie, session_with, tmp_dir};

fn secs(t: Tick) -> f64 {
    t.0 as f64 / S as f64
}

#[test]
fn hero_ramp_keeps_the_media_slows_the_middle_and_moves_later_clips() {
    let root = tmp_dir("speed-ramp");
    let (a, b) = (root.join("a.mov"), root.join("b.mov"));
    make_movie(&a, DemoScene::OceanSunset, 64, 36, 96); // 4 s at 24 fps
    make_movie(&b, DemoScene::Dunes, 64, 36, 48);
    let (mut s, _, seq) = session_with(&[&a, &b]);
    let clip = |s: &crate::Session, k: usize| s.project.sequence(seq).unwrap().video_tracks[0].items[k].clone();
    let (first, second) = (clip(&s, 0), clip(&s, 1));
    let r = s.execute("clip.speedRamp", json!({"clips": [first.id.0], "preset": "hero"})).unwrap();
    assert_eq!(r["clips"].as_array().unwrap().len(), 1, "{r}");
    let ramped = clip(&s, 0);
    // same media, longer on the timeline (the slow middle), smooth slow motion on
    // (rounded up to whole frames: at 200 % at the end that is at most two media frames more)
    let extra = secs(ramped.source_out()) - secs(first.source_out());
    assert!((0.0..=2.0 / 24.0 + 1e-6).contains(&extra), "{extra}");
    assert!(ramped.duration > first.duration, "{} s", secs(ramped.duration));
    assert_eq!(ramped.time_interpolation, filmcraft_project::TimeInterpolation::OpticalFlow);
    // fast at the start (200 %), slow in the middle (25 %)
    assert!((ramped.speed_at(ramped.start + Tick(S / 4)) - 2.0).abs() < 1e-6);
    let mid = ramped.start + Tick(ramped.duration.0 / 2);
    assert!((ramped.speed_at(mid) - 0.25).abs() < 0.01, "{}", ramped.speed_at(mid));
    // the next clip moved by the change, and the linked sound is no longer than the picture
    let moved = clip(&s, 1);
    assert_eq!(moved.start, second.start + (ramped.duration - first.duration));
    let q = s.project.sequence(seq).unwrap();
    assert!(q.audio_tracks[0].items.iter().all(|i| i.duration <= ramped.duration.max(second.duration)));
    q.check().unwrap();

    // razor in the slow part: the two pieces show the same frames as before
    let cut = mid;
    let before: Vec<Tick> = (0..8).map(|k| ramped.moving_source_time_at(cut + Tick(k * S / 8))).collect();
    s.execute("timeline.razor", json!({"time": cut.0, "track": "V1"})).unwrap();
    let right = clip(&s, 1);
    assert_eq!(right.start, cut);
    let after: Vec<Tick> = (0..8).map(|k| right.moving_source_time_at(cut + Tick(k * S / 8))).collect();
    for (x, y) in before.iter().zip(&after) {
        assert!((x.0 - y.0).abs() < S / 1000, "{} vs {}", secs(*x), secs(*y));
    }
    s.execute("edit.undo", json!({})).unwrap();

    // clear: back to 100 %, the original length
    s.execute("clip.speedRamp", json!({"clips": [first.id.0], "clear": true})).unwrap();
    let plain = clip(&s, 0);
    assert!(plain.speed_ramp().is_none());
    assert!((secs(plain.duration) - secs(first.duration)).abs() <= 1.0 / 24.0 + 1e-9, "within a frame: {}", secs(plain.duration));
    // hostile points are refused, not a crash
    for bad in [json!([]), json!([{"at": -1, "speed": 50}]), json!([{"at": 1, "speed": 0}]), json!([{"at": 1}])] {
        assert!(s.execute("clip.speedRamp", json!({"clips": [first.id.0], "points": bad})).is_err());
    }
    let _ = std::fs::remove_dir_all(&root);
}
