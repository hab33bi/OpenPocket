fn main() {
    generate_sin_lut();
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
