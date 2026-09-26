//! Not an assertion — a stopwatch, for when an attach feels slow.
//!
//! `SHOT=/path/to/screenshot.png cargo test --release -p eidolon-tools \
//!    --test attach_timing -- --nocapture`
//!
//! Silent and passing with no `SHOT`, so it costs a normal test run
//! nothing. The interesting number is the one that dominates: on a
//! screenshot from a large display it is the resize and the PNG re-encode,
//! not the decode.
use std::time::Instant;

#[test]
fn where_the_time_goes() {
    let Ok(bytes) = std::fs::read(std::env::var("SHOT").unwrap_or_default()) else {
        return;
    };
    let ms = |t: Instant| t.elapsed().as_secs_f64() * 1000.0;
    println!("input {} KB", bytes.len() / 1024);

    let t = Instant::now();
    let a =
        eidolon_tools::image::Attachment::from_bytes("pasted.png".into(), bytes.clone()).unwrap();
    println!("from_bytes total   {:>8.1} ms   -> {}", ms(t), a.label());

    let t = Instant::now();
    let img = image::load_from_memory(&bytes).unwrap();
    println!("  decode           {:>8.1} ms", ms(t));
    for (n, f) in [
        ("Lanczos3", image::imageops::FilterType::Lanczos3),
        ("CatmullRom", image::imageops::FilterType::CatmullRom),
        ("Triangle", image::imageops::FilterType::Triangle),
    ] {
        let t = Instant::now();
        let r = img.resize(1568, 1568, f);
        println!(
            "  resize {n:<11}{:>8.1} ms  -> {}x{}",
            ms(t),
            r.width(),
            r.height()
        );
    }
    let small = img.resize(1568, 1568, image::imageops::FilterType::Triangle);

    let t = Instant::now();
    let mut out = Vec::new();
    small
        .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
        .unwrap();
    println!(
        "  png default      {:>8.1} ms  -> {} KB",
        ms(t),
        out.len() / 1024
    );

    let t = Instant::now();
    let mut fast = Vec::new();
    let enc = image::codecs::png::PngEncoder::new_with_quality(
        &mut fast,
        image::codecs::png::CompressionType::Fast,
        image::codecs::png::FilterType::Adaptive,
    );
    small.write_with_encoder(enc).unwrap();
    println!(
        "  png fast         {:>8.1} ms  -> {} KB",
        ms(t),
        fast.len() / 1024
    );
}
