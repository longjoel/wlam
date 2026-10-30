//! The host's heap region and the emitter's memory size must agree.
//!
//! `wlam-host` does not depend on `wlam-emit` (the host is linked *into* the
//! module, so the dependency points the other way), which means
//! `HEAP_REGION_BASE`/`SIZE` exist twice. If they drift, `HeapAlloc` hands the
//! guest an address outside its memory and the guest traps on its first store
//! through it -- a failure that looks like a wild pointer in guest code rather
//! than a host/emitter disagreement.
//!
//! This test is the thing that makes the duplication safe.

#[test]
fn the_heap_region_is_inside_the_memory_the_emitter_declares() {
    let base = wlam_host::heap::HEAP_REGION_BASE;
    let size = wlam_host::heap::HEAP_REGION_SIZE;
    assert_eq!(
        base,
        wlam_emit::chain::HEAP_REGION_BASE,
        "heap region base must match the emitter's"
    );
    assert_eq!(
        size,
        wlam_emit::chain::HEAP_REGION_SIZE,
        "heap region size must match the emitter's"
    );

    // A typical image: Uxn32 loads at 0x400000 and is 0x18000 bytes.
    let image_end = 0x400000u64 + 0x18000;
    let pages = wlam_emit::chain::pages_for_image(image_end);
    let memory_end = pages as u64 * 0x10000;
    assert!(
        memory_end >= (base as u64 + size as u64),
        "an image at 0x400000 gets {memory_end:#x} bytes of memory, but the heap \
         region ends at {:#x}: HeapAlloc would return addresses the guest cannot \
         dereference",
        base as u64 + size as u64
    );
}

#[test]
fn the_heap_region_does_not_overlap_the_image() {
    // The image must be mapped where its absolute addresses say it is, and the
    // heap must not be carved out of it: an allocation would overwrite the
    // guest's own code or data.
    let base = wlam_host::heap::HEAP_REGION_BASE;
    let image_base = 0x400000u32;
    let image_end = image_base + 0x18000;
    assert!(
        base >= image_end,
        "heap region at {base:#x} overlaps the image ending at {image_end:#x}"
    );
}

#[test]
fn the_stack_seed_does_not_land_in_the_heap() {
    // The harness seeds ESP at 0x70000 and the stack grows down from there.
    let stack = 0x70000u32;
    let base = wlam_host::heap::HEAP_REGION_BASE;
    assert!(
        stack < base,
        "the stack seed {stack:#x} is at or above the heap region {base:#x}"
    );
}
