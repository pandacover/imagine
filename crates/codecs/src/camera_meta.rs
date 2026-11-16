//! Camera colour metadata that the video stream itself does not carry.
//!
//! Sony cameras flag S-Log3 footage as BT.709 in the H.264 / HEVC VUI. The log curve and gamut
//! they recorded are only in the clip's Non-Real-Time metadata XML (`<NonRealTimeMeta>`), which the
//! camera writes inside the MP4 and as a sidecar next to it (`C0001.MP4` → `C0001M01.XML`). Its
//! `CaptureGammaEquation` and `CaptureColorPrimaries` items say what was shot. [`camera_color_space`]
//! reads them so S-Log3 clips import already interpreted as S-Log3, and the colour pipeline turns
//! them into normal-looking Rec. 709 without a LUT.

use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use filmcraft_color::ColorSpace;

/// How much of each end of a movie file is searched for the metadata XML (cameras write it next
/// to the `moov` box, at the start or the end of the file).
const SCAN: u64 = 4 << 20;
/// The largest sidecar XML read.
const MAX_SIDECAR: u64 = 1 << 20;

/// The colour space a camera recorded `path` in, from its Non-Real-Time metadata (embedded or
/// sidecar), when that says something the stream's own metadata cannot (a log curve). `None` when
/// there is no such metadata or it names a display colour space.
pub fn camera_color_space(path: &Path) -> Option<ColorSpace> {
    if let Some(cs) = sidecar(path).and_then(|x| std::fs::read(x).ok()).and_then(|b| from_xml(&b)) {
        return Some(cs);
    }
    embedded(path).and_then(|b| from_xml(&b))
}

/// `C0001.MP4` → an existing `C0001M01.XML` (any case) in the same folder.
fn sidecar(path: &Path) -> Option<PathBuf> {
    let stem = path.file_stem()?.to_str()?;
    let dir = path.parent()?;
    let want = format!("{stem}M01.XML").to_ascii_lowercase();
    std::fs::read_dir(dir).ok()?.filter_map(|e| e.ok()).map(|e| e.path()).find(|p| {
        p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.to_ascii_lowercase() == want)
            && p.metadata().is_ok_and(|m| m.is_file() && m.len() <= MAX_SIDECAR)
    })
}

/// The `<NonRealTimeMeta>…</NonRealTimeMeta>` XML inside a movie file, searched for in its first
/// and last [`SCAN`] bytes.
fn embedded(path: &Path) -> Option<Vec<u8>> {
    let mut f = std::fs::File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    let mut head = Vec::new();
    (&mut f).take(SCAN.min(len)).read_to_end(&mut head).ok()?;
    if let Some(x) = nrt_xml(&head) {
        return Some(x.to_vec());
    }
    if len <= SCAN {
        return None;
    }
    let start = len.saturating_sub(SCAN).max(SCAN.min(len));
    f.seek(SeekFrom::Start(start)).ok()?;
    let mut tail = Vec::new();
    f.take(len - start).read_to_end(&mut tail).ok()?;
    nrt_xml(&tail).map(<[u8]>::to_vec)
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

fn nrt_xml(b: &[u8]) -> Option<&[u8]> {
    let start = find(b, b"<NonRealTimeMeta")?;
    let rest = b.get(start..)?;
    let end = find(rest, b"</NonRealTimeMeta>")?;
    rest.get(..end.saturating_add(b"</NonRealTimeMeta>".len()))
}

/// The `value` of the `<Item name="NAME" value="…"/>` element.
fn item_value<'a>(xml: &'a str, name: &str) -> Option<&'a str> {
    let at = xml.find(&format!("name=\"{name}\""))?;
    // the element containing it
    let open = xml.get(..at)?.rfind('<')?;
    let close = open.saturating_add(xml.get(open..)?.find('>')?);
    let el = xml.get(open..close)?;
    let v = el.find("value=\"")?.saturating_add("value=\"".len());
    let rest = el.get(v..)?;
    rest.get(..rest.find('"')?)
}

/// The log colour space a Non-Real-Time metadata XML records (S-Log3 with S-Gamut3.Cine or
/// S-Gamut3).
pub fn from_xml(bytes: &[u8]) -> Option<ColorSpace> {
    let xml = std::str::from_utf8(nrt_xml(bytes).unwrap_or(bytes)).ok()?;
    let gamma = item_value(xml, "CaptureGammaEquation")?.to_ascii_lowercase();
    let primaries = item_value(xml, "CaptureColorPrimaries").unwrap_or_default().to_ascii_lowercase();
    if !gamma.contains("s-log3") {
        return None;
    }
    // "s-log3-cine" names the gamut too; otherwise the primaries item does
    let plain_gamut = !gamma.contains("cine") && primaries.contains("s-gamut3") && !primaries.contains("cine");
    Some(if plain_gamut { ColorSpace::SLog3SGamut3 } else { ColorSpace::SLog3SGamut3Cine })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nrt(gamma: &str, primaries: &str) -> String {
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<NonRealTimeMeta xmlns="urn:schemas-professionalDisc:nonRealTimeMeta:ver.2.20">
  <AcquisitionRecord>
    <Group name="CameraUnitMetadataSet">
      <Item name="CaptureGammaEquation" value="{gamma}"/>
      <Item name="CaptureColorPrimaries" value="{primaries}"/>
    </Group>
  </AcquisitionRecord>
</NonRealTimeMeta>"#
        )
    }

    #[test]
    fn reads_the_log_curve_and_gamut() {
        assert_eq!(from_xml(nrt("s-log3-cine", "S-Gamut3.Cine").as_bytes()), Some(ColorSpace::SLog3SGamut3Cine));
        assert_eq!(from_xml(nrt("s-log3", "S-Gamut3").as_bytes()), Some(ColorSpace::SLog3SGamut3));
        assert_eq!(from_xml(nrt("s-log3", "S-Gamut3.Cine").as_bytes()), Some(ColorSpace::SLog3SGamut3Cine));
        // display gammas are what the VUI already says
        assert_eq!(from_xml(nrt("rec709", "rec709").as_bytes()), None);
        assert_eq!(from_xml(nrt("hlg", "rec2020").as_bytes()), None);
        assert_eq!(from_xml(b"<NonRealTimeMeta></NonRealTimeMeta>"), None);
        assert_eq!(from_xml(b"\xff\xfe garbage"), None);
        assert_eq!(from_xml(br#"<Item name="CaptureGammaEquation" value="s-log3-cine"#), None, "unterminated");
    }

    #[test]
    fn finds_the_sidecar_and_embedded_xml() {
        let dir = std::env::temp_dir().join(format!("fc-camera-meta-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // sidecar, upper-case as cameras write it
        let clip = dir.join("C0001.MP4");
        std::fs::write(&clip, b"\0\0\0\x08free").unwrap();
        assert_eq!(camera_color_space(&clip), None);
        std::fs::write(dir.join("C0001M01.XML"), nrt("s-log3-cine", "S-Gamut3.Cine")).unwrap();
        assert_eq!(camera_color_space(&clip), Some(ColorSpace::SLog3SGamut3Cine));
        // embedded in a uuid box after a large payload (searched at the end)
        let big = dir.join("C0002.MP4");
        let xml = nrt("s-log3", "S-Gamut3");
        let mut b = vec![0u8; (SCAN as usize) + 1000];
        let size = (24 + xml.len()) as u32;
        b.extend_from_slice(&size.to_be_bytes());
        b.extend_from_slice(b"uuid");
        b.extend_from_slice(&[0x5f; 16]);
        b.extend_from_slice(xml.as_bytes());
        std::fs::write(&big, &b).unwrap();
        assert_eq!(camera_color_space(&big), Some(ColorSpace::SLog3SGamut3));
        assert_eq!(camera_color_space(&dir.join("missing.mp4")), None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
