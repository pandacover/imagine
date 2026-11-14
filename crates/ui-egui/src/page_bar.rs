//! The page bar: one tab per stage of the work along the bottom of the window (Media, Edit, Color,
//! Effects, Audio, Deliver), so moving from cutting to grading to delivery is one click. Each page
//! is a mode plus, inside Edit mode, the workspace laid out for that job. Window ▸ Page Bar shows
//! or hides it.
//!
//! Also Window ▸ Simple Mode ([`hidden_in_simple_mode`]): hides menu commands and effects folders a
//! photographer / videographer delivering to clients and social media does not need (studio
//! interchange formats, immersive 360° video, obsolete effects). Everything stays reachable by
//! command id (CLI, MCP, control channel), and turning Simple Mode off shows it all again.

use egui::{Color32, Rect, Sense, Stroke, pos2, vec2};
use serde_json::{Value, json};

use crate::FilmcraftApp;
use crate::state::Mode;
use crate::theme::Tokens;

/// Height of the page bar.
pub const HEIGHT: f32 = 30.0;

/// A page: its id (automation `pagebar.<id>`, command `page.<id>`), label, mode and, in Edit mode,
/// workspace.
pub struct Page {
    pub id: &'static str,
    pub label: &'static str,
    pub mode: Mode,
    pub workspace: Option<&'static str>,
}

pub const PAGES: [Page; 6] = [
    Page { id: "media", label: "Media", mode: Mode::Import, workspace: None },
    Page { id: "edit", label: "Edit", mode: Mode::Edit, workspace: Some("Editing") },
    Page { id: "color", label: "Color", mode: Mode::Edit, workspace: Some("Color") },
    Page { id: "effects", label: "Effects", mode: Mode::Edit, workspace: Some("Effects") },
    Page { id: "audio", label: "Audio", mode: Mode::Edit, workspace: Some("Audio") },
    Page { id: "deliver", label: "Deliver", mode: Mode::Export, workspace: None },
];

/// The page showing now: the mode's page, and in Edit mode the page of the current workspace
/// (None for a workspace no page uses).
pub fn current(app: &FilmcraftApp) -> Option<&'static Page> {
    PAGES.iter().find(|p| p.mode == app.ui.mode && p.workspace.is_none_or(|w| w == app.ui.workspace))
}

/// The desktop app's default layout: the page bar shown and Simple Mode on.
pub fn creator_layout(app: &mut FilmcraftApp) {
    app.ui.show_page_bar = true;
    app.ui.simple_mode = true;
}

/// Switch to `page`.
pub fn open(app: &mut FilmcraftApp, page: &Page) {
    app.ui.mode = page.mode;
    if let Some(w) = page.workspace
        && app.ui.workspace != w
        && crate::dock::names(&app.workspaces).iter().any(|n| n == w)
    {
        app.set_workspace(w);
    }
}

/// Commands handled here: `page.<id>`, `window.pageBar`, `window.simpleMode` (`{"on":bool?}`,
/// toggled without it). None for other ids.
pub fn invoke(app: &mut FilmcraftApp, id: &str, params: &Value) -> Option<Result<Value, String>> {
    let toggled = |cur: bool| params.get("on").and_then(Value::as_bool).unwrap_or(!cur);
    if let Some(p) = id.strip_prefix("page.") {
        let Some(page) = PAGES.iter().find(|x| x.id == p) else { return Some(Err(format!("unknown page `{p}`"))) };
        open(app, page);
        return Some(Ok(json!({"page": page.id})));
    }
    match id {
        "window.pageBar" => {
            app.ui.show_page_bar = toggled(app.ui.show_page_bar);
            Some(Ok(json!({"on": app.ui.show_page_bar})))
        }
        "window.simpleMode" => {
            app.ui.simple_mode = toggled(app.ui.simple_mode);
            Some(Ok(json!({"on": app.ui.simple_mode})))
        }
        _ => None,
    }
}

/// Check marks of the commands above.
pub fn checked(app: &FilmcraftApp, id: &str) -> Option<bool> {
    match id {
        "window.pageBar" => Some(app.ui.show_page_bar),
        "window.simpleMode" => Some(app.ui.simple_mode),
        _ => id.strip_prefix("page.").map(|p| current(app).is_some_and(|c| c.id == p)),
    }
}

/// Menu commands Simple Mode hides: studio interchange (AAF / OMF audio-post hand-offs, CMX EDL).
/// XML, FCPXML and OpenTimelineIO stay: clients and other editors send those.
const HIDDEN_COMMANDS: &[&str] = &["file.exportAaf", "file.exportOmf", "file.exportEdl", "file.importAaf"];

/// Effects-browser folders Simple Mode hides (360° video, effects kept only for old projects).
const HIDDEN_FOLDERS: &[&str] = &["Immersive Video", "Obsolete", "Legacy"];

/// Whether Simple Mode hides the menu command `id`.
pub fn hidden_in_simple_mode(id: &str) -> bool {
    HIDDEN_COMMANDS.contains(&id)
}

/// Whether Simple Mode hides an effect or transition in `category` (its folder path).
pub fn folder_hidden_in_simple_mode(category: &[&str]) -> bool {
    category.iter().any(|c| HIDDEN_FOLDERS.contains(c))
}

/// Draws the page bar in `rect`: the pages centred, the active one highlighted.
pub fn show(app: &mut FilmcraftApp, ui: &mut egui::Ui, rect: Rect) {
    let t = app.tokens;
    let p = ui.painter().clone();
    p.rect_filled(rect, 0.0, t.header_bg);
    p.line_segment([rect.left_top(), rect.right_top()], Stroke::new(1.0, Color32::BLACK));
    let active = current(app).map(|c| c.id);
    let labels: Vec<(&'static Page, std::sync::Arc<egui::Galley>)> = PAGES
        .iter()
        .map(|pg| {
            let col = if active == Some(pg.id) { t.tab_text_active } else { t.tab_text };
            (pg, p.layout_no_wrap(crate::i18n::t(pg.label).to_string(), Tokens::ui(12.0), col))
        })
        .collect();
    const PAD: f32 = 18.0;
    let total: f32 = labels.iter().map(|(_, g)| g.size().x + 2.0 * PAD).sum();
    let mut x = rect.center().x - total / 2.0;
    let mut clicked = None;
    for (pg, galley) in labels {
        let w = galley.size().x + 2.0 * PAD;
        let r = Rect::from_min_size(pos2(x, rect.min.y + 3.0), vec2(w, rect.height() - 6.0));
        let resp = ui.interact(r, egui::Id::unique(("pagebar", pg.id)), Sense::click()).on_hover_text(crate::i18n::t(pg.label));
        app.auto.add(&format!("pagebar.{}", pg.id), r, pg.label);
        let on = active == Some(pg.id);
        if on {
            p.rect_filled(r, 4.0, t.hover);
            p.line_segment([pos2(r.min.x + PAD, r.max.y - 1.0), pos2(r.max.x - PAD, r.max.y - 1.0)], Stroke::new(2.0, t.accent));
        } else if resp.hovered() {
            p.rect_filled(r, 4.0, t.hover);
        }
        let col = if on || resp.hovered() { t.tab_text_active } else { t.tab_text };
        p.galley_with_override_text_color(pos2(r.min.x + PAD, r.center().y - galley.size().y / 2.0), galley, col);
        if resp.clicked() {
            clicked = Some(pg);
        }
        x += w;
    }
    if let Some(pg) = clicked {
        open(app, pg);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simple_mode_hides_studio_interchange_and_360_video_only() {
        assert!(hidden_in_simple_mode("file.exportAaf") && hidden_in_simple_mode("file.exportOmf") && hidden_in_simple_mode("file.exportEdl"));
        assert!(!hidden_in_simple_mode("file.exportFcp7Xml") && !hidden_in_simple_mode("file.exportOtio") && !hidden_in_simple_mode("file.exportMedia"));
        assert!(folder_hidden_in_simple_mode(&["Video Effects", "Immersive Video"]));
        assert!(!folder_hidden_in_simple_mode(&["Video Effects", "Blur & Sharpen"]));
        // the app: pages switch mode and workspace; Simple Mode filters the menus
        let mut app = FilmcraftApp::new(filmcraft_engine::Session::default());
        assert!(!app.ui.show_page_bar && !app.ui.simple_mode, "off in a bare app");
        creator_layout(&mut app);
        assert!(app.ui.show_page_bar && app.ui.simple_mode);
        assert!(invoke(&mut app, "page.color", &json!({})).unwrap().is_ok());
        assert_eq!((app.ui.mode, app.ui.workspace.as_str()), (Mode::Edit, "Color"));
        assert_eq!(checked(&app, "page.color"), Some(true));
        assert!(invoke(&mut app, "page.deliver", &json!({})).unwrap().is_ok());
        assert_eq!(app.ui.mode, Mode::Export);
        assert_eq!(current(&app).map(|p| p.id), Some("deliver"));
        assert!(invoke(&mut app, "page.nope", &json!({})).unwrap().is_err());
        let ids = |app: &FilmcraftApp| crate::menus::menu_items(app).into_iter().map(|m| m.id).collect::<Vec<_>>();
        assert!(!ids(&app).iter().any(|i| i == "file.exportAaf") && ids(&app).iter().any(|i| i == "file.exportMedia"));
        assert_eq!(invoke(&mut app, "window.simpleMode", &json!({})).unwrap().unwrap()["on"], false);
        assert!(ids(&app).iter().any(|i| i == "file.exportAaf"));
        assert_eq!(invoke(&mut app, "window.pageBar", &json!({"on": false})).unwrap().unwrap()["on"], false);
        assert!(invoke(&mut app, "file.save", &json!({})).is_none());
        // every page's workspace is a built-in one
        for pg in &PAGES {
            if let Some(w) = pg.workspace {
                assert!(crate::dock::WORKSPACES.contains(&w), "{w}");
            }
        }
    }
}
