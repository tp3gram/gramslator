fn main() {
    load_dotenv();
    linker_be_nice();
    // make sure linkall.x is the last linker script (otherwise might cause problems with flip-link)
    println!("cargo:rustc-link-arg=-Tlinkall.x");
}

/// Read a `.env` file (if present) and forward every `KEY=VALUE` pair to
/// rustc via `cargo:rustc-env` so that `env!()` picks them up at compile time.
fn load_dotenv() {
    let dotenv_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(".env");

    // Re-run this build script whenever .env changes.
    println!("cargo:rerun-if-changed={}", dotenv_path.display());

    let contents = match std::fs::read_to_string(&dotenv_path) {
        Ok(c) => c,
        Err(_) => return, // no .env file — rely on real env vars instead
    };

    for line in contents.lines() {
        let line = line.trim();
        // Skip blanks and comments
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((key, value)) = line.split_once('=') {
            println!("cargo:rustc-env={}={}", key.trim(), value.trim());
        }
    }
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

    // --error-handling-script is only supported by the RISC-V GNU ld, not the Xtensa one.
    let target = std::env::var("TARGET").unwrap_or_default();
    if target.starts_with("riscv") {
        println!(
            "cargo:rustc-link-arg=--error-handling-script={}",
            std::env::current_exe().unwrap().display()
        );
    }
}
