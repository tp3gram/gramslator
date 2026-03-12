fn main() {
    load_dotenv();
    linker_be_nice();
    override_memory_map();
    println!("cargo:rustc-link-arg=-Tdefmt.x");
    // make sure linkall.x is the last linker script (otherwise might cause problems with flip-link)
    println!("cargo:rustc-link-arg=-Tlinkall.x");
}

/// Override esp-hal's default `memory.x` to extend the flash-mapped segment
/// sizes from 4 MB to 16 MB, matching the physical flash capacity.
///
/// Large read-only assets (fonts, etc.) now live in dedicated flash
/// partitions mapped at runtime via [`gramslator::flash_data`], so the
/// application binary itself is small.  The 16 MB limit simply reflects
/// the hardware maximum; the linker only emits what the code actually
/// references.
///
/// We write our version to `OUT_DIR` and add it as a linker search path.
/// Cargo processes the current crate's search paths before dependencies, so
/// our `memory.x` takes precedence over esp-hal's.
fn override_memory_map() {
    use std::io::Write;

    let out = std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
    println!("cargo:rustc-link-search={}", out.display());

    // This is the stock esp-hal ESP32-S3 memory.x (after preprocessing) with
    // irom_seg and drom_seg bumped from 4M to 16M to match our 16 MB flash.
    let memory_x = r#"/* Overridden by gramslator build.rs — 16 MB flash */

/* reserved for ICACHE (32 KB) */
RESERVE_ICACHE = 0x8000;

VECTORS_SIZE = 0x400;

MEMORY
{
  vectors_seg ( RX )     : ORIGIN = 0x40370000 + RESERVE_ICACHE, len = VECTORS_SIZE
  iram_seg ( RX )        : ORIGIN = 0x40370000 + RESERVE_ICACHE + VECTORS_SIZE, len = 328k - VECTORS_SIZE - RESERVE_ICACHE

  dram2_seg ( RW )       : ORIGIN = 0x3FCDB700, len = 0x3FCED710 - 0x3FCDB700
  dram_seg ( RW )        : ORIGIN = 0x3FC88000 , len = ORIGIN(dram2_seg) - 0x3FC88000

  /* external flash — 16 MB */
  irom_seg ( RX )        : ORIGIN = 0x42000020, len = 16M - 0x20
  drom_seg ( R )         : ORIGIN = 0x3C000020, len = 16M - 0x20

  rtc_fast_seg(RWX) : ORIGIN = 0x600fe000, len = 8k
  rtc_slow_seg(RW)       : ORIGIN = 0x50000000, len = 8k
}
"#;

    let mut f = std::fs::File::create(out.join("memory.x")).expect("create memory.x");
    f.write_all(memory_x.as_bytes()).expect("write memory.x");
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
