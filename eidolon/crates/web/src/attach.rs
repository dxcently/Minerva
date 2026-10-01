//! Images a page attaches to `POST /api/say`, checked before the session sees
//! any of them.
//!
//! **What an attachment is here.** `{name?, media_type, data}`, with `data`
//! standard base64 (the alphabet with `+` and `/`, padded, and no whitespace),
//! which is what a browser's `FileReader.readAsDataURL` gives once the
//! `data:...;base64,` prefix is removed. One becomes one
//! [`ContentBlock::image`] with its name as the `alt`, which is the block the
//! TUI's `:attach` makes (`eidolon_tools::image::Attachment::block`).
//!
//! Every image is checked before anything is said. The first failure refuses
//! the whole say with a one-line reason, which the route answers with 422.
//!
//! **What is not done.** Nothing is re-encoded or resized. The TUI brings an
//! image within the wire's limits through `eidolon_tools::image`, and this
//! crate does not depend on it. The door refuses anything over that same
//! limit ([`MAX_BYTES`]) instead, under [`crate::serve::MAX_BODY`] for the
//! whole say. Documents are refused by name ([`NO_DOCUMENTS`]) because
//! harnox's `ContentBlock` has no document block to carry one.

use eidolon_core::message::ContentBlock;

/// Bounds how many blocks one request can make.
pub const MAX_IMAGES: usize = 8;

/// The longest name an image may carry, in bytes: the usual filename limit.
pub const MAX_NAME: usize = 255;

/// The core has no document block. Accepting the field and dropping it would
/// answer 202 for a file the model never sees.
pub const NO_DOCUMENTS: &str = "this door takes images only: the session has no document block to carry one";

pub const SHAPE: &str =
    "a say with images is {\"text\"?: string, \"mode\"?, \"images\": [{\"name\"?: string, \"media_type\": string, \"data\": base64}]}";

/// Decoded bytes. The TUI's `MAX_BYTES` (`crates/tools/src/image.rs`), copied:
/// it is private there, and this crate does not depend on `eidolon-tools`. The
/// TUI shrinks an image to it; the door refuses one past it.
pub const MAX_BYTES: usize = 4 * 1024 * 1024;

/// One image as `POST /api/say` receives it.
#[derive(serde::Deserialize)]
pub struct ImageIn {
    #[serde(default)]
    pub name: Option<String>,
    pub media_type: String,
    pub data: String,
}

/// The images as content blocks in the order given, or why they are refused.
pub fn blocks(images: Vec<ImageIn>) -> Result<Vec<ContentBlock>, String> {
    if images.len() > MAX_IMAGES {
        return Err(format!("at most {MAX_IMAGES} images may go with one message, not {}", images.len()));
    }
    images
        .into_iter()
        .enumerate()
        .map(|(i, image)| one(image).map_err(|why| format!("image {i}: {why}")))
        .collect()
}

fn one(ImageIn { name, media_type, data }: ImageIn) -> Result<ContentBlock, String> {
    if let Some(name) = &name
        && (name.is_empty() || name.len() > MAX_NAME || name.contains(['\0', '/', '\\']))
    {
        return Err(format!("a name is 1 to {MAX_NAME} bytes with no NUL, / or \\"));
    }
    if !TYPES.contains(&media_type.as_str()) {
        return Err(format!("{media_type:?} is not one of {}", TYPES.join(", ")));
    }
    let bytes = decode(&data).ok_or("data is not standard padded base64")?;
    if bytes.is_empty() {
        return Err("data is empty".to_string());
    }
    if bytes.len() > MAX_BYTES {
        return Err(format!("{} bytes, over the {MAX_BYTES} limit; shrink it first", bytes.len()));
    }
    match sniff(&bytes) {
        Some(found) if found == media_type => Ok(ContentBlock::image(media_type, data, name)),
        Some(found) => Err(format!("declared {media_type} but the bytes are {found}")),
        None => Err(format!("declared {media_type} but the bytes are not an image")),
    }
}

const TYPES: [&str; 4] = ["image/png", "image/jpeg", "image/gif", "image/webp"];

fn sniff(b: &[u8]) -> Option<&'static str> {
    if b.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if b.starts_with(b"\xff\xd8\xff") {
        Some("image/jpeg")
    } else if b.starts_with(b"GIF87a") || b.starts_with(b"GIF89a") {
        Some("image/gif")
    } else if b.len() >= 12 && b.starts_with(b"RIFF") && &b[8..12] == b"WEBP" {
        Some("image/webp")
    } else {
        None
    }
}

/// Strict standard padded base64. Hand-written because `eidolon-web` has no
/// base64 dependency; the input is bounded by `MAX_BODY`.
fn decode(s: &str) -> Option<Vec<u8>> {
    let s = s.as_bytes();
    if !s.len().is_multiple_of(4) {
        return None;
    }
    let pad = s.iter().rev().take_while(|&&c| c == b'=').count();
    if pad > 2 {
        return None;
    }
    let (quads, _) = s.as_chunks::<4>();
    let mut out = Vec::with_capacity(quads.len() * 3);
    for (i, quad) in quads.iter().enumerate() {
        let last = i + 1 == quads.len();
        let mut n: u32 = 0;
        let mut have = 0;
        for &c in quad {
            let v = match c {
                b'A'..=b'Z' => c - b'A',
                b'a'..=b'z' => c - b'a' + 26,
                b'0'..=b'9' => c - b'0' + 52,
                b'+' => 62,
                b'/' => 63,
                // Padding only in the last quad, and only at its end: the
                // count above said how many, and `have` stops short of them.
                b'=' if last && have >= 4 - pad => break,
                _ => return None,
            };
            n = n << 6 | u32::from(v);
            have += 1;
        }
        n <<= 6 * (4 - have);
        let [_, a, b, c] = n.to_be_bytes();
        out.extend_from_slice(&[a, b, c][..have - 1]);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const PNG: &str = "iVBORw0KGgo="; // the eight-byte PNG signature
    const JPEG: &str = "/9j/4A=="; // FF D8 FF E0
    const GIF: &str = "R0lGODlh"; // GIF89a
    const WEBP: &str = "UklGRgAAAABXRUJQ"; // RIFF, 4 bytes, WEBP

    fn img(media_type: &str, data: &str) -> ImageIn {
        ImageIn { name: Some("shot.png".into()), media_type: media_type.into(), data: data.into() }
    }

    #[test]
    fn decode_takes_padded_standard_base64_and_nothing_else() {
        assert_eq!(decode("").unwrap(), b"");
        assert_eq!(decode("Zg==").unwrap(), b"f");
        assert_eq!(decode("Zm8=").unwrap(), b"fo");
        assert_eq!(decode("Zm9v").unwrap(), b"foo");
        assert_eq!(decode("+/+/").unwrap(), [0xfb, 0xff, 0xbf]);
        assert!(decode("Zg").is_none(), "unpadded");
        assert!(decode("Zg=a").is_none(), "padding in the middle");
        assert!(decode("Z===").is_none(), "three pads");
        assert!(decode("Zg==Zm9v").is_none(), "padding before the last quad");
        assert!(decode("Zm9v\n").is_none(), "whitespace");
        assert!(decode("-_-_").is_none(), "the url-safe alphabet");
    }

    #[test]
    fn each_of_the_four_types_passes_with_its_own_bytes() {
        for (t, d) in [("image/png", PNG), ("image/jpeg", JPEG), ("image/gif", GIF), ("image/webp", WEBP)] {
            let out = blocks(vec![img(t, d)]).unwrap_or_else(|e| panic!("{t}: {e}"));
            assert_eq!(out, vec![ContentBlock::image(t, d, Some("shot.png".into()))]);
        }
    }

    #[test]
    fn a_type_that_lies_about_its_bytes_is_refused() {
        let e = blocks(vec![img("image/png", JPEG)]).err().unwrap();
        assert!(e.contains("declared image/png but the bytes are image/jpeg"), "{e}");
        let e = blocks(vec![img("image/png", "Zm9v")]).err().unwrap();
        assert!(e.contains("not an image"), "{e}");
    }

    #[test]
    fn every_type_that_may_be_declared_is_one_sniff_recognises() {
        let fixtures = [("image/png", PNG), ("image/jpeg", JPEG), ("image/gif", GIF), ("image/webp", WEBP)];
        assert_eq!(TYPES.len(), fixtures.len());
        for (t, d) in fixtures {
            assert!(TYPES.contains(&t), "{t}");
            assert_eq!(sniff(&decode(d).unwrap()), Some(t));
        }
    }

    #[test]
    fn only_the_four_types_are_taken() {
        for t in ["image/bmp", "image/svg+xml", "IMAGE/PNG", "application/pdf", ""] {
            assert!(blocks(vec![img(t, PNG)]).is_err(), "{t}");
        }
    }

    #[test]
    fn empty_bad_or_too_many_is_refused() {
        assert!(blocks(vec![img("image/png", "")]).err().unwrap().contains("empty"));
        assert!(blocks(vec![img("image/png", "not base64!")]).err().unwrap().contains("base64"));
        let nine = (0..9).map(|_| img("image/png", PNG)).collect();
        assert!(blocks(nine).err().unwrap().contains("at most 8"));
        let eight = (0..8).map(|_| img("image/png", PNG)).collect();
        assert_eq!(blocks(eight).unwrap().len(), 8);
    }

    #[test]
    fn a_name_is_short_and_no_path() {
        let named = |n: &str| ImageIn { name: Some(n.into()), media_type: "image/png".into(), data: PNG.into() };
        for bad in ["", "a/b.png", "a\\b.png", "a\0.png", &"x".repeat(256)] {
            assert!(blocks(vec![named(bad)]).is_err(), "{bad:?}");
        }
        assert!(blocks(vec![named(&"x".repeat(255))]).is_ok());
        let nameless = ImageIn { name: None, media_type: "image/png".into(), data: PNG.into() };
        assert_eq!(blocks(vec![nameless]).unwrap(), vec![ContentBlock::image("image/png", PNG, None)]);
    }

    fn encode(bytes: &[u8]) -> String {
        const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
        for chunk in bytes.chunks(3) {
            let n = chunk.iter().enumerate().fold(0u32, |n, (i, &b)| n | u32::from(b) << (16 - 8 * i));
            for i in 0..4 {
                out.push(if i <= chunk.len() { A[(n >> (18 - 6 * i) & 63) as usize] as char } else { '=' });
            }
        }
        out
    }

    #[test]
    fn an_image_over_the_tui_s_limit_is_refused_and_one_at_it_is_not() {
        let png = |len: usize| {
            let mut b = b"\x89PNG\r\n\x1a\n".to_vec();
            b.resize(len, 0);
            encode(&b)
        };
        assert_eq!(encode(b"\x89PNG\r\n\x1a\n"), PNG, "the test encoder agrees with the fixture");
        assert!(blocks(vec![img("image/png", &png(MAX_BYTES))]).is_ok());
        let e = blocks(vec![img("image/png", PNG), img("image/png", &png(MAX_BYTES + 1))]).err().unwrap();
        assert_eq!(e, format!("image 1: {} bytes, over the {MAX_BYTES} limit; shrink it first", MAX_BYTES + 1));
    }

    #[test]
    fn the_refusal_names_which_image() {
        let e = blocks(vec![img("image/png", PNG), img("image/gif", PNG)]).err().unwrap();
        assert!(e.starts_with("image 1: "), "{e}");
    }
}
