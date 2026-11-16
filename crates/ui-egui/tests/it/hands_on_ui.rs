//! Fixes from a hands-on test of the editor (the demo project: clips dragged in, a title, a
//! transition, Export opened), driven headless through the control channel.

use std::collections::BTreeSet;
use std::sync::mpsc::{Sender, channel};

use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_project::{BinEntry, ClipId, ItemId, ParamValue};
use filmcraft_time::Tick;
use filmcraft_ui_egui::FilmcraftApp;
use filmcraft_ui_egui::control::ControlRequest;
use serde_json::{Value, json};

struct Driver {
    harness: Harness<'static, FilmcraftApp>,
    tx: Sender<ControlRequest>,
}

impl Driver {
    fn with(session: Session) -> Self {
        let (tx, rx) = channel();
        let app = FilmcraftApp::new(session).with_control(rx);
        let harness = Harness::builder().with_size(egui::vec2(1600.0, 980.0)).with_max_steps(10_000).build_eframe(move |_cc| app);
        let mut d = Driver { harness, tx };
        d.frames(4);
        d
    }
    fn demo() -> Self {
        let mut session = Session::default();
        session.execute("file.openDemoProject", json!({})).expect("demo project");
        Self::with(session)
    }
    fn frames(&mut self, n: usize) {
        for _ in 0..n {
            let ctx = self.harness.ctx.clone();
            let mut raw = std::mem::take(self.harness.input_mut());
            eframe::App::raw_input_hook(self.harness.state_mut(), &ctx, &mut raw);
            *self.harness.input_mut() = raw;
            self.harness.step();
        }
    }
    fn ok(&mut self, method: &str, params: Value) -> Value {
        let (req, reply) = ControlRequest::new(method, params.clone());
        self.tx.send(req).unwrap();
        for _ in 0..600 {
            self.frames(1);
            if let Ok(r) = reply.try_recv() {
                assert_eq!(r["ok"], true, "{method} {params}: {r}");
                self.frames(2);
                return r["result"].clone();
            }
        }
        panic!("no reply to {method} {params}");
    }
    fn exec(&mut self, command: &str, params: Value) -> Value {
        self.ok("engine.execute", json!({"command": command, "params": params}))
    }
    fn app(&mut self) -> &mut FilmcraftApp {
        self.harness.state_mut()
    }
    fn ids(&mut self, prefix: &str) -> BTreeSet<String> {
        let v = self.ok("ui.elements", json!({"prefix": prefix}));
        v.as_array().unwrap().iter().filter_map(|e| e["id"].as_str().map(str::to_string)).collect()
    }
    fn rect(&mut self, id: &str) -> egui::Rect {
        let v = self.ok("ui.elements", json!({"prefix": id}));
        let e = v.as_array().unwrap().iter().find(|e| e["id"] == id).unwrap_or_else(|| panic!("no element {id}")).clone();
        let r: Vec<f32> = e["rect"].as_array().unwrap().iter().map(|x| x.as_f64().unwrap() as f32).collect();
        egui::Rect::from_min_size(egui::pos2(r[0], r[1]), egui::vec2(r[2], r[3]))
    }
    fn send(&mut self, e: egui::Event) {
        self.harness.input_mut().events.push(e);
        self.frames(1);
    }
    /// Press at `from`, move to `to` over several frames, and return what `during` reads while the
    /// button is still down; then release.
    fn drag<T>(&mut self, from: egui::Pos2, to: egui::Pos2, during: impl FnOnce(&mut Self) -> T) -> T {
        let button = |pos, pressed| egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() };
        self.send(egui::Event::PointerMoved(from));
        self.send(button(from, true));
        for k in 1..=12 {
            self.send(egui::Event::PointerMoved(from.lerp(to, k as f32 / 12.0)));
        }
        self.frames(2);
        let out = during(self);
        self.send(button(to, false));
        self.frames(4);
        out
    }
    fn first_video_clip(&mut self) -> ClipId {
        let q = self.app().session.active_sequence().expect("a sequence");
        q.video_tracks.iter().flat_map(|t| &t.items).next().expect("a video clip").id
    }
    fn param(&mut self, clip: ClipId, effect: &str, param: &str) -> ParamValue {
        let s = &self.app().session;
        let (_, it) = s.active_sequence().unwrap().find_item(clip).unwrap();
        it.effects.iter().find(|e| e.effect == effect).unwrap().param(param).unwrap().value_at(Tick::ZERO)
    }
}

/// Effect Controls: a number typed into a property and Enter commits it.
#[test]
fn enter_commits_a_number_typed_in_effect_controls() {
    let mut d = Driver::demo();
    let clip = d.first_video_clip();
    d.exec("timeline.select", json!({"clips": [clip.0]}));
    d.ok("ui.panel.show", json!({"panel": "EffectControls"}));
    d.ok("ui.set", json!({"focused": "EffectControls"}));
    d.ok("ui.menu.invoke", json!({"id": "window.maximizeFrame"}));
    d.frames(4);
    let ids = d.ids("effectControls.motion.");
    assert!(ids.contains("effectControls.motion.scale.value"), "{ids:?}");
    d.ok("ui.click", json!({"id": "effectControls.motion.scale.value"}));
    d.ok("ui.type", json!({"text": "50"}));
    d.ok("ui.key", json!({"key": "Enter"}));
    d.frames(3);
    assert_eq!(d.param(clip, "motion", "scale"), ParamValue::Float(50.0));
}

/// The Program monitor's play button stops playback too.
#[test]
fn the_program_play_button_pauses() {
    let mut d = Driver::demo();
    d.frames(4);
    d.ok("ui.click", json!({"id": "program.transport.playback.toggle"}));
    d.frames(6);
    assert!(d.app().playback.playing, "playing after the first click");
    d.ok("ui.click", json!({"id": "program.transport.playback.toggle"}));
    d.frames(3);
    assert!(!d.app().playback.playing, "stopped after the second click");
}

/// A template card dragged from Essential Graphics ▸ Browse onto footage on the Timeline adds a
/// graphic clip above it (it was refused, with only a status-bar note) and leaves the panel
/// layout as it was.
#[test]
fn a_template_dragged_onto_the_timeline_adds_a_title_and_keeps_the_layout() {
    let mut d = Driver::demo();
    d.ok("ui.menu.invoke", json!({"id": "window.panel.EssentialGraphics"}));
    d.ok("ui.set", json!({"gfxTemplates": {"tab": "browse", "query": ""}}));
    d.frames(4);
    let cards: Vec<String> = d.ids("gfxTemplates.item.").into_iter().collect();
    let card = cards.first().expect("a template card").clone();
    let clips = |d: &mut Driver| d.app().session.active_sequence().unwrap().video_tracks.iter().map(|t| t.items.len()).sum::<usize>();
    let before = clips(&mut d);
    let (timeline, program, egfx) = (d.rect("panel.Timeline"), d.rect("panel.Program"), d.rect("panel.EssentialGraphics"));
    let from = d.rect(&card).center();
    let v1 = d.rect("timeline.track.V1.target");
    let tracks = d.rect("timeline.tracks");
    let to = egui::pos2(tracks.min.x + tracks.width() * 0.6, v1.center().y);
    // V1 holds footage there: the preview and the title go on the first free track above it
    let shown = d.drag(from, to, |d| d.ids("timeline.dropPreview."));
    assert_eq!(shown.into_iter().collect::<Vec<_>>(), ["timeline.dropPreview.V2"]);
    d.frames(4);
    let status = d.app().ui.status.clone();
    assert_eq!(clips(&mut d), before + 1, "a graphic clip went in ({status})");
    assert_eq!((d.rect("panel.Timeline"), d.rect("panel.Program"), d.rect("panel.EssentialGraphics")), (timeline, program, egfx), "the layout stayed");
}

/// A Project item dragged onto Video 1 of an empty sequence previews V1 / A1 and lands there,
/// with the Clip Mismatch Warning answered.
#[test]
fn a_drop_on_video_1_previews_and_lands_on_video_1() {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    s.execute("file.newSequence", json!({"name": "Empty", "width": 1280, "height": 720, "fps": 25})).unwrap();
    let mut d = Driver::with(s);
    let p = d.app().session.project.clone();
    let bin = p.root.children.iter().find_map(|e| if let BinEntry::Bin(b) = e { (b.name == "Footage").then(|| b.clone()) } else { None }).unwrap();
    let item: ItemId =
        bin.children.iter().find_map(|e| if let BinEntry::Item(i) = e { (p.source_streams(*i) == (true, true)).then_some(*i) } else { None }).unwrap();
    d.ok("ui.panel.show", json!({"panel": "Project"}));
    d.exec("project.view.set", json!({"view": "list"}));
    d.ok("ui.menu.invoke", json!({"id": "projectPanel.openBin", "params": {"bin": bin.id.0, "how": "inPlace"}}));
    d.frames(4);
    let from = d.rect(&format!("project.item.{}", item.0)).center();
    let v1 = d.rect("timeline.track.V1.target");
    let tracks = d.rect("timeline.tracks");
    let to = egui::pos2(tracks.min.x + 60.0, v1.center().y);
    let shown = d.drag(from, to, |d| d.ids("timeline.dropPreview."));
    let expect: BTreeSet<String> = ["timeline.dropPreview.V1", "timeline.dropPreview.A1"].iter().map(|s| s.to_string()).collect();
    assert_eq!(shown, expect);
    d.ok("ui.click", json!({"id": "clipMismatch.keep"}));
    let q = d.app().session.active_sequence().unwrap().clone();
    assert_eq!(q.video_tracks[0].items.len(), 1, "on V1");
    assert_eq!(q.audio_tracks[0].items.len(), 1, "on A1");
}

/// Typing in Essential Graphics' Source Text is one undo step: Undo while still typing takes the
/// whole edit back, not a character at a time ("Tomo Test" went to "Tomo Tes").
#[test]
fn undo_takes_back_a_whole_source_text_edit() {
    let mut d = Driver::demo();
    let clip = ClipId(d.exec("graphics.newText", json!({"text": "Hi"}))["clip"].as_u64().unwrap());
    d.exec("timeline.select", json!({"clips": [clip.0]}));
    d.ok("ui.panel.show", json!({"panel": "Properties"}));
    d.ok("ui.set", json!({"focused": "Properties"}));
    d.ok("ui.menu.invoke", json!({"id": "window.maximizeFrame"}));
    d.frames(4);
    let undo = d.app().session.history.undo.len();
    d.ok("ui.click", json!({"id": "graphics.sourceText"}));
    d.ok("ui.key", json!({"key": "Cmd+A"}));
    for word in ["T", "o", "m", "o", " ", "T", "e", "s", "t"] {
        d.ok("ui.type", json!({"text": word}));
        d.frames(2);
    }
    assert_eq!(d.param(clip, "graphic_text", "text"), ParamValue::Text("Tomo Test".into()));
    assert_eq!(d.app().session.history.undo.len(), undo + 1, "one step for the whole edit");
    d.ok("ui.key", json!({"key": "Cmd+Z"}));
    d.frames(3);
    assert_eq!(d.param(clip, "graphic_text", "text"), ParamValue::Text("Hi".into()), "Undo in the field took the whole edit back");
    d.ok("ui.key", json!({"key": "Cmd+Shift+Z"}));
    d.frames(3);
    assert_eq!(d.param(clip, "graphic_text", "text"), ParamValue::Text("Tomo Test".into()), "and Redo puts it back");
}
