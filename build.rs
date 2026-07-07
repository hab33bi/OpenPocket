fn main() {
    generate_sin_lut();
    generate_inter_font();
    linker_be_nice();
    // make sure linkall.x is the last linker script (otherwise might cause problems with flip-link)
    println!("cargo:rustc-link-arg=-Tlinkall.x");
}

/// 512-entry sin LUT — f32 (init) + i16 Q14 (hot eval path).
fn generate_sin_lut() {
    let out_dir = std::env::var("OUT_DIR").expect("OUT_DIR");
    let mut body = String::from("pub static SIN_LUT: [f32; 512] = [\n");
    let mut body_i16 = String::from("pub static SIN_LUT_I16: [i16; 512] = [\n");
    for i in 0..512 {
        let angle = (i as f64 / 512.0) * std::f64::consts::TAU;
        let s = angle.sin();
        body.push_str(&format!("{:.8}_f32,\n", s));
        let q14 = (s * 16384.0).round() as i32;
        let q14 = q14.clamp(-32768, 32767);
        body_i16.push_str(&format!("{q14}_i16,\n"));
    }
    body.push_str("];\n");
    body_i16.push_str("];\n");
    body.push_str(&body_i16);
    let path = std::path::Path::new(&out_dir).join("sin_lut.rs");
    std::fs::write(path, body).expect("write sin_lut.rs");
    println!("cargo:rerun-if-changed=build.rs");
}

fn linker_be_nice() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() > 1 {
        let kind = &args[1];
        let what = &args[2];

        match kind.as_str() {
            "undefined-symbol" => match what.as_str() {
                what if what.starts_with("_defmt_") => {
                    eprintln!();
                    eprintln!(
                        "💡 `defmt` not found - make sure `defmt.x` is added as a linker script and you have included `use defmt_rtt as _;`"
                    );
                    eprintln!();
                }
                "_stack_start" => {
                    eprintln!();
                    eprintln!("💡 Is the linker script `linkall.x` missing?");
                    eprintln!();
                }
                what if what.starts_with("esp_rtos_") => {
                    eprintln!();
                    eprintln!(
                        "💡 `esp-radio` has no scheduler enabled. Make sure you have initialized `esp-rtos` or provided an external scheduler."
                    );
                    eprintln!();
                }
                "embedded_test_linker_file_not_added_to_rustflags" => {
                    eprintln!();
                    eprintln!(
                        "💡 `embedded-test` not found - make sure `embedded-test.x` is added as a linker script for tests"
                    );
                    eprintln!();
                }
                "free"
                | "malloc"
                | "calloc"
                | "get_free_internal_heap_size"
                | "malloc_internal"
                | "realloc_internal"
                | "calloc_internal"
                | "free_internal" => {
                    eprintln!();
                    eprintln!(
                        "💡 Did you forget the `esp-alloc` dependency or didn't enable the `compat` feature on it?"
                    );
                    eprintln!();
                }
                _ => (),
            },
            // we don't have anything helpful for "missing-lib" yet
            _ => {
                std::process::exit(1);
            }
        }

        std::process::exit(0);
    }

    println!(
        "cargo:rustc-link-arg=-Wl,--error-handling-script={}",
        std::env::current_exe().unwrap().display()
    );
}

fn generate_inter_font() {
    let out_dir = std::env::var("OUT_DIR").expect("OUT_DIR");
    let font_path = std::path::Path::new("assets/InterDisplay-Bold.ttf");

    let mut body = String::from("/// Auto-generated from InterDisplay-Bold.ttf via fontdue in build.rs\n\n");

    if !font_path.exists() {
        // Stub so builds don't hard-fail before user places the font.
        body.push_str("pub const INTER_FONT_PRESENT: bool = false;\n");
        body.push_str("#[derive(Copy, Clone)]\npub struct Glyph { pub width: u8, pub height: u8, pub advance: u8, pub ymin: i16, pub data: &'static [u8] }\n");
        body.push_str("pub static GLYPHS: [Option<Glyph>; 128] = [None; 128];\n");
        let path = std::path::Path::new(&out_dir).join("inter_font.rs");
        std::fs::write(&path, body).expect("write stub inter_font.rs");
        println!("cargo:warning=assets/InterDisplay-Bold.ttf not found. Place your Inter Display Bold .ttf there. Using stub.");
        println!("cargo:rerun-if-changed=assets/InterDisplay-Bold.ttf");
        return;
    }

    let font_data = std::fs::read(font_path).expect("read Inter font");
    let font = fontdue::Font::from_bytes(font_data.as_slice(), fontdue::FontSettings::default())
        .expect("parse Inter font");

    // Chars we need for time (HH:MM) and date "July 7th 2026"
    let needed: Vec<char> = "0123456789:July 7th 2026".chars().collect();

    body.push_str("pub const INTER_FONT_PRESENT: bool = true;\n");
    body.push_str("#[derive(Copy, Clone)]\npub struct Glyph { pub width: u8, pub height: u8, pub advance: u8, pub ymin: i16, pub data: &'static [u8] }\n");
    body.push_str("pub static GLYPHS: [Option<Glyph>; 128] = [\n");

    for c in 0u8..128 {
        let ch = c as char;
        if needed.contains(&ch) {
            let (metrics, bitmap) = font.rasterize(ch, 72.0); // 2x raster for "scale to gray" AA (see quote in clock.rs)
            let w = metrics.width as u8;
            let h = metrics.height as u8;
            let adv = metrics.advance_width.round() as u8;
            let ymin = metrics.ymin as i16;  // from font metrics - negative for descenders like 'y' to position lower relative to baseline

            if std::env::var("FONT_DEBUG").is_ok()
                && (ch.is_alphabetic() || ch == ':' || ch.is_digit(10))
            {
                println!(
                    "cargo:warning=FONT_METRICS {}: xmin={} ymin={} w={} h={} adv={}",
                    ch, metrics.xmin, metrics.ymin, metrics.width, metrics.height, metrics.advance_width
                );
            }

            // Pack to 1bpp, row-major, bits MSB first in each byte
            let mut packed: Vec<u8> = vec![0; ((w as usize + 7) / 8) * h as usize];
            for y in 0..h as usize {
                for x in 0..w as usize {
                    let val = bitmap[y * w as usize + x];
                    if val > 127 {
                        let byte_idx = y * ((w as usize + 7) / 8) + (x / 8);
                        let bit = 7 - (x % 8);
                        packed[byte_idx] |= 1 << bit;
                    }
                }
            }

            body.push_str(&format!("    Some(Glyph {{ width: {}, height: {}, advance: {}, ymin: {}, data: &[", w, h, adv, ymin));
            for (i, b) in packed.iter().enumerate() {
                if i > 0 { body.push(','); }
                body.push_str(&format!("{}", b));
            }
            body.push_str("] }),\n");
        } else {
            body.push_str("    None,\n");
        }
    }
    body.push_str("];\n");

    let path = std::path::Path::new(&out_dir).join("inter_font.rs");
    std::fs::write(&path, body).expect("write inter_font.rs");
    println!("cargo:rerun-if-changed=assets/InterDisplay-Bold.ttf");
    println!("cargo:rerun-if-changed=build.rs");
}
