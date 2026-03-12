//! Read-only access to data stored in dedicated flash partitions.
//!
//! The ESP32-S3 MMU can map regions of external SPI flash into the CPU's
//! data-bus address space (`0x3C00_0000 …`).  The bootloader only maps the
//! application's DROM/IROM segments; additional data partitions must be
//! mapped explicitly at runtime.
//!
//! This module provides [`map_flash_region`] which configures the MMU to
//! map an arbitrary flash region and returns a `&'static [u8]` slice backed
//! by the memory-mapped flash — zero-copy, no heap, random-access.

use defmt::info;

// ---------------------------------------------------------------------------
// ROM function declarations (same ones used by esp-hal's PSRAM init)
// ---------------------------------------------------------------------------

unsafe extern "C" {
    fn Cache_Suspend_DCache();
    fn Cache_Resume_DCache(param: u32);

    /// Configure a DCache MMU mapping.
    ///
    /// - `ext_ram`: 0 for flash, `1 << 15` for SPIRAM.
    /// - `vaddr`:   Virtual address (must be 64 KB aligned).
    /// - `paddr`:   Physical flash address (must be 64 KB aligned).
    /// - `psize`:   Page size in KB — always 64 on ESP32-S3.
    /// - `num`:     Number of 64 KB pages to map.
    /// - `fixed`:   0 = pages grow with virtual addresses.
    fn cache_dbus_mmu_set(
        ext_ram: u32,
        vaddr: u32,
        paddr: u32,
        psize: u32,
        num: u32,
        fixed: u32,
    ) -> i32;
}

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Base of the data-bus external memory window on ESP32-S3.
const EXTMEM_ORIGIN: u32 = 0x3C00_0000;

/// MMU page size: 64 KB.
const MMU_PAGE_SIZE: u32 = 0x1_0000;

/// Hardware register base of the MMU table.
const DR_REG_MMU_TABLE: u32 = 0x600C_5000;

/// Size of the DCache MMU table in 32-bit entries.
///
/// ESP32-S3: ICACHE_MMU_SIZE = 0x800 bytes → 512 entries.
const MMU_TABLE_ENTRIES: usize = 0x800 / core::mem::size_of::<u32>();

/// Marker value for an unmapped MMU slot.
const MMU_INVALID: u32 = 1 << 14;

/// MMU ext_ram value for flash (not SPIRAM).
const MMU_ACCESS_FLASH: u32 = 0;

/// EXTMEM peripheral base address (ESP32-S3).
const EXTMEM_BASE: u32 = 0x600C_4000;

/// DCACHE_CTRL1 register: offset 0x04 from EXTMEM base.
/// Bits 0 and 1 are `dcache_shut_core0_bus` and `dcache_shut_core1_bus`.
const DCACHE_CTRL1: *mut u32 = (EXTMEM_BASE + 0x04) as *mut u32;

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Map a region of external flash into the CPU address space and return a
/// `&'static [u8]` view.
///
/// # Arguments
///
/// - `flash_offset` — Byte offset of the data in flash.  Will be rounded
///   **down** to a 64 KB page boundary; the returned slice's start is
///   adjusted so it points at exactly `flash_offset` within the mapping.
/// - `len` — Number of bytes to make accessible (from `flash_offset`).
///
/// # Panics
///
/// Panics if:
/// - Not enough free MMU entries exist for the requested mapping.
/// - The ROM `cache_dbus_mmu_set` call fails.
///
/// # Safety note
///
/// This function uses `unsafe` internally to call ROM cache-management
/// functions and to construct the returned slice.  It is safe to call from
/// application code as long as:
///
/// 1. The flash region actually contains valid data (garbage in → garbage
///    out, but no UB).
/// 2. Nothing else writes to the same flash region concurrently (external
///    flash is read-only from the CPU's perspective, so this is satisfied
///    by construction).
/// 3. The function is called **after** `esp_hal::init()` (so PSRAM
///    mappings are already in place and won't be clobbered).
pub fn map_flash_region(flash_offset: u32, len: usize) -> &'static [u8] {
    assert!(len > 0, "cannot map zero-length flash region");

    // Round down to page boundary; compute intra-page offset.
    let page_aligned_offset = flash_offset & !(MMU_PAGE_SIZE - 1);
    let intra_page = (flash_offset - page_aligned_offset) as usize;
    let total_bytes = intra_page + len;
    let num_pages = (total_bytes as u32).div_ceil(MMU_PAGE_SIZE);

    // Find the first contiguous run of `num_pages` free MMU slots.
    // We scan forward from the beginning, skipping all entries that are
    // already mapped (by the bootloader for DROM/IROM, or by PSRAM init).
    let first_free = find_first_free_run(num_pages as usize);

    let vaddr = EXTMEM_ORIGIN + (first_free as u32) * MMU_PAGE_SIZE;

    info!(
        "Mapping {} flash pages: flash 0x{:X}..+0x{:X} → vaddr 0x{:X} (MMU slots {}..{})",
        num_pages,
        page_aligned_offset,
        num_pages * MMU_PAGE_SIZE,
        vaddr,
        first_free,
        first_free + num_pages as usize,
    );

    unsafe {
        Cache_Suspend_DCache();

        let res = cache_dbus_mmu_set(
            MMU_ACCESS_FLASH,
            vaddr,
            page_aligned_offset,
            64, // page size in KB
            num_pages,
            0, // pages grow with virtual addresses
        );

        // Re-enable the data-bus cache for both cores by clearing bits 0–1
        // of DCACHE_CTRL1 (dcache_shut_core0_bus, dcache_shut_core1_bus).
        let ctrl1 = DCACHE_CTRL1.read_volatile();
        DCACHE_CTRL1.write_volatile(ctrl1 & !0b11);

        Cache_Resume_DCache(0);

        if res != 0 {
            panic!(
                "cache_dbus_mmu_set failed ({}): flash 0x{:X} → vaddr 0x{:X}, {} pages",
                res, page_aligned_offset, vaddr, num_pages,
            );
        }

        // Return a slice starting at the exact requested flash_offset.
        let ptr = (vaddr as usize + intra_page) as *const u8;
        core::slice::from_raw_parts(ptr, len)
    }
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Scan the MMU table for the first contiguous run of `count` free
/// (invalid) entries.  Panics if no such run exists.
fn find_first_free_run(count: usize) -> usize {
    let mmu = DR_REG_MMU_TABLE as *const u32;

    // The bootloader reserves the very last MMU entry for its own flash
    // access, so we exclude it from the search (same as PSRAM init does).
    let usable = MMU_TABLE_ENTRIES - 1;

    let mut run_start = 0usize;
    let mut run_len = 0usize;

    for i in 0..usable {
        let entry = unsafe { mmu.add(i).read_volatile() };
        if entry & MMU_INVALID != 0 {
            // Free slot.
            if run_len == 0 {
                run_start = i;
            }
            run_len += 1;
            if run_len >= count {
                return run_start;
            }
        } else {
            // Occupied — reset the run.
            run_len = 0;
        }
    }

    panic!(
        "Not enough free MMU entries: need {} contiguous, best run was {}",
        count, run_len
    );
}
