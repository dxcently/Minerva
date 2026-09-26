//! Reading an image in, and saying what it is.
//!
//! An engine primitive, beside [`crate::fs`] and [`crate::shell`]: it
//! opens a file, decides what it is from its bytes, brings it within what
//! a model's wire will take, and hands back a content block. No manifest,
//! no policy, no terminal — the two consumers are the TUI's `:attach` and
//! the headless `run --image`, and neither should have to reach through
//! the other to get here.
//!
//! Drawing an image is a different question with a different answer, and
//! lives in `eidolon-tui`'s `image` module: what a *terminal* can do with
//! pixels has nothing to do with what a model can do with them.

use std::io::Cursor;
use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};
use base64::Engine;
use harnox::llm::message::{ContentBlock, ImageSource};

/// The most an attachment may be, in decoded bytes, before it is re-encoded
/// smaller. The Anthropic wire refuses a request whose images exceed about
/// 5 MB once base64 has grown them by a third; staying under 4 keeps the
/// margin for the prose around it.
const MAX_BYTES: usize = 4 * 1024 * 1024;

/// The longest edge an image is sent at. Anthropic's own guidance: beyond
/// roughly this, the model has already downscaled it and the extra pixels
/// are paid for and discarded.
const MAX_EDGE: u32 = 1568;

/// One image the operator has attached, ready to become a content block.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Attachment {
    /// What to call it — a filename, or `pasted.png` for the clipboard.
    pub name: String,
    pub media_type: String,
    /// The bytes, base64-encoded. Kept encoded because that is the form
    /// both wires want and the form the log stores; decoding is for
    /// drawing, which most sessions never do.
    pub data: String,
    /// Decoded length, for the placard.
    pub bytes: usize,
    /// Pixel dimensions, where the header gave them up.
    pub dims: Option<(u32, u32)>,
}

/// `~/x` as the operator means it.
///
/// Only where a *person* typed the path — the TUI's `:attach`, chat's, and
/// `run --image`. A path from anywhere else has already been resolved by
/// whatever produced it, and expanding it again would be guessing.
pub fn expand_tilde(arg: &str) -> std::path::PathBuf {
    match arg.trim().strip_prefix("~/") {
        Some(rest) => dirs::home_dir()
            .map(|h| h.join(rest))
            .unwrap_or_else(|| std::path::PathBuf::from(arg.trim())),
        None => std::path::PathBuf::from(arg.trim()),
    }
}

impl Attachment {
    /// Read an image off disk.
    pub fn load(path: &Path) -> Result<Attachment> {
        let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "image".into());
        Attachment::from_bytes(name, bytes)
    }

    /// Take bytes already in hand — a file just read, or the clipboard.
    pub fn from_bytes(name: String, bytes: Vec<u8>) -> Result<Attachment> {
        let Some(media_type) = sniff(&bytes) else {
            bail!("{name} is not an image this harness recognises (png, jpeg, gif, webp or bmp)");
        };
        let (bytes, media_type) = fit(bytes, media_type)?;
        let dims = image::ImageReader::new(Cursor::new(&bytes))
            .with_guessed_format()
            .ok()
            .and_then(|r| r.into_dimensions().ok());
        Ok(Attachment {
            name,
            media_type: media_type.to_string(),
            bytes: bytes.len(),
            data: base64::engine::general_purpose::STANDARD.encode(&bytes),
            dims,
        })
    }

    /// The line under the picture, and the whole of what a terminal that
    /// cannot draw shows: `screenshot.png · 1440×900 · 240 KB`.
    pub fn label(&self) -> String {
        let mut parts = vec![self.name.clone()];
        if let Some((w, h)) = self.dims {
            parts.push(format!("{w}×{h}"));
        }
        parts.push(human_bytes(self.bytes));
        parts.join(" · ")
    }

    pub fn block(&self) -> ContentBlock {
        ContentBlock::image(&self.media_type, &self.data, Some(self.name.clone()))
    }

    /// An attachment recovered from a journaled block — how a resumed
    /// session gets its pictures back.
    ///
    /// The log stores the bytes, the media type and the name, but not the
    /// dimensions, so they are read back out of the header here. That
    /// costs a base64 decode per image on the resume path, which is a
    /// bounded cost (an attachment is capped at four megabytes) paid on a
    /// path that is already reading the whole log — and the alternative,
    /// journaling the dimensions, would be writing a record field that can
    /// be derived, which is the thing the log is meant not to do.
    pub fn from_block(block: &ContentBlock) -> Option<Attachment> {
        let ContentBlock::Image { source, alt } = block else {
            return None;
        };
        let ImageSource::Base64 { media_type, data } = source else {
            return None;
        };
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(data)
            .ok()?;
        Some(Attachment {
            name: alt.clone().unwrap_or_else(|| "image".into()),
            media_type: media_type.clone(),
            bytes: bytes.len(),
            data: data.clone(),
            dims: image::ImageReader::new(Cursor::new(&bytes))
                .with_guessed_format()
                .ok()
                .and_then(|r| r.into_dimensions().ok()),
        })
    }

    /// The decoded bytes back again, for drawing.
    pub fn decoded(&self) -> Option<Vec<u8>> {
        base64::engine::general_purpose::STANDARD
            .decode(&self.data)
            .ok()
    }
}

/// Media type from the magic bytes, never from the extension: a screenshot
/// saved as `.jpg` by a tool that wrote a PNG is common enough, and the
/// wire believes the field rather than the name.
pub fn sniff(b: &[u8]) -> Option<&'static str> {
    if b.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if b.starts_with(&[0xff, 0xd8, 0xff]) {
        Some("image/jpeg")
    } else if b.starts_with(b"GIF87a") || b.starts_with(b"GIF89a") {
        Some("image/gif")
    } else if b.len() > 12 && b.starts_with(b"RIFF") && &b[8..12] == b"WEBP" {
        Some("image/webp")
    } else if b.starts_with(b"BM") {
        Some("image/bmp")
    } else {
        None
    }
}

/// Bring an image within what the wire will take, re-encoding only if it
/// has to. An image that is already small enough comes back untouched —
/// re-encoding a PNG the operator chose would cost quality for nothing,
/// and would change the bytes the log stores.
fn fit(bytes: Vec<u8>, media_type: &'static str) -> Result<(Vec<u8>, &'static str)> {
    let dims = image::ImageReader::new(Cursor::new(&bytes))
        .with_guessed_format()
        .ok()
        .and_then(|r| r.into_dimensions().ok());
    let too_wide = dims.is_some_and(|(w, h)| w.max(h) > MAX_EDGE);
    if bytes.len() <= MAX_BYTES && !too_wide {
        return Ok((bytes, media_type));
    }
    let img = image::load_from_memory(&bytes).context("decoding the image to resize it")?;
    // Into the box in one step. `resize` fits within it and keeps the
    // aspect ratio, so scaling by hand first and then resizing rounds
    // twice and lands a few pixels short of the edge it aimed at.
    let img = if too_wide {
        img.resize(MAX_EDGE, MAX_EDGE, image::imageops::FilterType::Lanczos3)
    } else {
        img
    };
    // PNG for something that was lossless, JPEG for something that was
    // already lossy — re-encoding a photograph as PNG is how a 300 KB
    // attachment becomes a 6 MB one and fails the size check it was
    // resized to pass.
    let mut out = Vec::new();
    let (fmt, media_type) = match media_type {
        "image/jpeg" => (image::ImageFormat::Jpeg, "image/jpeg"),
        _ => (image::ImageFormat::Png, "image/png"),
    };
    let img = if fmt == image::ImageFormat::Jpeg {
        image::DynamicImage::ImageRgb8(img.to_rgb8())
    } else {
        img
    };
    img.write_to(&mut Cursor::new(&mut out), fmt)
        .context("re-encoding the resized image")?;
    Ok((out, media_type))
}

/// `240 KB`.
pub fn human_bytes(n: usize) -> String {
    const K: usize = 1024;
    match n {
        0..K => format!("{n} B"),
        K..1_048_576 => format!("{} KB", n / K),
        _ => format!("{:.1} MB", n as f64 / 1_048_576.0),
    }
}

// ---------------------------------------------------------------------------
// The clipboard
// ---------------------------------------------------------------------------

/// The image on the system clipboard, if there is one.
///
/// The mirror of the TUI's own `clipboard::copy`, and asymmetric with
/// it on purpose: copying text can go out over OSC 52 because the terminal will
/// take it, but *reading* the clipboard has no escape that answers with
/// bytes, so this is helpers or nothing. `None` means no helper, no
/// clipboard, or nothing image-shaped on it — three cases the caller says
/// the same thing about, because "there is no picture to paste" is the
/// whole of what the operator needs to hear.
pub fn from_clipboard() -> Option<Attachment> {
    for (bin, list, read) in [
        (
            "wl-paste",
            &["--list-types"][..],
            &["--no-newline", "--type"][..],
        ),
        (
            "xclip",
            &["-selection", "clipboard", "-t", "TARGETS", "-o"][..],
            &["-selection", "clipboard", "-o", "-t"][..],
        ),
    ] {
        let usable = match bin {
            "wl-paste" => std::env::var_os("WAYLAND_DISPLAY").is_some(),
            _ => std::env::var_os("DISPLAY").is_some(),
        };
        if !usable {
            continue;
        }
        let Ok(types) = Command::new(bin).args(list).stderr(Stdio::null()).output() else {
            continue;
        };
        let types = String::from_utf8_lossy(&types.stdout);
        // PNG first wherever it is offered: it is lossless, every helper
        // can produce it, and a screenshot is exactly what it is for.
        let Some(t) = [
            "image/png",
            "image/jpeg",
            "image/webp",
            "image/gif",
            "image/bmp",
        ]
        .into_iter()
        .find(|t| types.contains(t)) else {
            continue;
        };
        let Ok(out) = Command::new(bin)
            .args(read)
            .arg(t)
            .stderr(Stdio::null())
            .output()
        else {
            continue;
        };
        if out.stdout.is_empty() {
            continue;
        }
        let ext = t.rsplit('/').next().unwrap_or("png");
        return Attachment::from_bytes(format!("pasted.{ext}"), out.stdout).ok();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(w: u32, h: u32) -> Vec<u8> {
        let img = image::RgbImage::from_fn(w, h, |x, y| {
            image::Rgb([(x % 256) as u8, (y % 256) as u8, 128])
        });
        let mut out = Vec::new();
        image::DynamicImage::ImageRgb8(img)
            .write_to(&mut Cursor::new(&mut out), image::ImageFormat::Png)
            .unwrap();
        out
    }

    #[test]
    fn the_media_type_comes_from_the_bytes_and_not_the_name() {
        let a = Attachment::from_bytes("screenshot.jpg".into(), png(4, 4)).unwrap();
        assert_eq!(a.media_type, "image/png");
        assert_eq!(a.name, "screenshot.jpg");
    }

    #[test]
    fn something_that_is_not_an_image_is_refused_by_name() {
        let e = Attachment::from_bytes("notes.txt".into(), b"hello there".to_vec()).unwrap_err();
        assert!(e.to_string().contains("notes.txt"), "{e}");
    }

    #[test]
    fn an_attachment_knows_its_own_size_and_says_so() {
        let a = Attachment::from_bytes("shot.png".into(), png(20, 10)).unwrap();
        assert_eq!(a.dims, Some((20, 10)));
        let l = a.label();
        assert!(l.contains("shot.png"), "{l}");
        assert!(l.contains("20×10"), "{l}");
    }

    /// An image inside the limits keeps the operator's own bytes. A
    /// re-encode would cost quality for nothing and change what the log
    /// stores.
    #[test]
    fn a_small_image_is_not_re_encoded() {
        let bytes = png(20, 10);
        let a = Attachment::from_bytes("s.png".into(), bytes.clone()).unwrap();
        assert_eq!(a.decoded().unwrap(), bytes);
    }

    #[test]
    fn an_oversized_image_is_brought_within_the_wires_limits() {
        let a = Attachment::from_bytes("huge.png".into(), png(MAX_EDGE + 600, 100)).unwrap();
        let (w, h) = a.dims.unwrap();
        assert_eq!(w, MAX_EDGE);
        assert!(h < 100, "the aspect ratio survives the resize: {w}×{h}");
    }

    /// A photograph resized as PNG is how a 300 KB attachment becomes a
    /// 6 MB one and fails the check that resized it.
    #[test]
    fn a_resized_jpeg_stays_a_jpeg() {
        let img = image::RgbImage::from_fn(MAX_EDGE + 100, 40, |x, y| {
            image::Rgb([(x % 251) as u8, (y % 253) as u8, ((x * y) % 249) as u8])
        });
        let mut jpg = Vec::new();
        image::DynamicImage::ImageRgb8(img)
            .write_to(&mut Cursor::new(&mut jpg), image::ImageFormat::Jpeg)
            .unwrap();
        let a = Attachment::from_bytes("photo.jpg".into(), jpg).unwrap();
        assert_eq!(a.media_type, "image/jpeg");
        assert_eq!(a.dims.unwrap().0, MAX_EDGE);
    }

    #[test]
    fn the_block_it_becomes_carries_the_name_as_its_alt() {
        let a = Attachment::from_bytes("shot.png".into(), png(4, 4)).unwrap();
        let ContentBlock::Image { source, alt } = a.block() else {
            panic!("not an image block")
        };
        assert_eq!(alt.as_deref(), Some("shot.png"));
        assert!(
            matches!(source, ImageSource::Base64 { ref media_type, .. } if media_type == "image/png")
        );
    }
}
