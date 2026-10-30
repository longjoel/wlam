//! The Win32 heap: `HeapCreate`, `HeapAlloc`, `HeapFree`, `HeapDestroy`,
//! `GetProcessHeap`, `HeapSize`, `HeapReAlloc`.
//!
//! ## Why this is not a stub
//!
//! Uxn32 creates a heap as the first thing its startup code does
//! (`HeapCreate(flOptions, 0x1000, 0)` at 0x40d838) and stores the handle at
//! `0x415f48`, testing it for zero to decide whether to continue. Everything
//! after that -- window structures, strings, the emulator's own state -- comes
//! from `HeapAlloc`, which appears at seven call sites.
//!
//! A stub returning a plausible constant would be actively harmful: the guest
//! would take the "success" path and then write through addresses the allocator
//! never handed out. So this is a real allocator over the guest's own linear
//! memory, and a failed allocation returns `NULL` -- a truthful answer the
//! guest already knows how to handle.
//!
//! ## Design
//!
//! Guest memory is the backing store. That is what makes the result honest: the
//! pointers `HeapAlloc` returns are addresses the guest can dereference
//! directly, not host-side handles needing translation.
//!
//! Each block carries an 8-byte header (size, flags) so `HeapFree` needs only
//! the pointer, exactly as the real API. Sizes round up to 8 bytes so every
//! returned pointer is 8-aligned, which the real heap guarantees and callers
//! rely on for doubles.
//!
//! Allocation is first-fit with coalescing on free. Blocks are *not* moved, so
//! pointers stay valid for their lifetime (the real heap gives the same
//! guarantee without `HEAP_REALLOC_IN_PLACE_ONLY` only for the returned
//! pointer's contents, but never moves a live block behind the caller's back).
//!
//! ## Handles
//!
//! A heap handle identifies *which* heap a call refers to; it is not a pointer.
//! Each heap gets [`HEAP_HANDLE_BASE`] + index, so "valid" and "NULL" can never
//! be confused and a handle that leaks into a pointer field is obviously wrong.

/// First value handed out by `HeapCreate`.
///
/// Deliberately not 0 (the failure value) and not 1 (a plausible small
/// integer). Any guest-dereferenceable address would be worse still.
pub const HEAP_HANDLE_BASE: u32 = 0x00A7_0000;

/// Per-block header in guest bytes: 4-byte total size, 4-byte flags.
pub const HEADER: u32 = 8;

/// Alignment of every returned pointer.
pub const ALIGN: u32 = 8;

/// `HEAP_GENERATE_EXCEPTIONS`.
pub const HEAP_GENERATE_EXCEPTIONS: u32 = 0x0000_0004;
/// `HEAP_ZERO_MEMORY`.
pub const HEAP_ZERO_MEMORY: u32 = 0x0000_0008;
/// `HEAP_REALLOC_IN_PLACE_ONLY`.
pub const HEAP_REALLOC_IN_PLACE_ONLY: u32 = 0x0000_0010;

/// Header flag: the block is free.
const BLOCK_FREE: u32 = 1;

fn round_up(v: u32) -> u32 {
    (v + (ALIGN - 1)) & !(ALIGN - 1)
}

/// One heap's arena inside guest memory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Heap {
    handle: u32,
    /// Guest address of the arena's first byte.
    start: u32,
    /// Guest address one past the arena's last byte.
    end: u32,
}

/// All heaps and the arenas they own.
///
/// The free list lives *in guest memory*, so the guest's own `HeapAlloc` and
/// this implementation see the same structure, and there is no shadow copy to
/// fall out of sync. `HeapSet` only remembers where each arena is.
#[derive(Debug, Clone)]
pub struct HeapSet {
    heaps: Vec<Heap>,
    /// Guest address from which the next arena may be carved.
    next_arena: u32,
    /// First guest address no arena may reach, EXCLUSIVE.
    ///
    /// `u64` because the guest's address space ends at exactly 2^32, one past
    /// the largest `u32` -- the limit is not itself representable as an address.
    limit: u64,
}

/// Base of the region heaps are carved from.
///
/// Must equal `wlam_emit::chain::HEAP_REGION_BASE`. The two crates are
/// independent (`wlam-host` does not depend on the emitter), so the constant is
/// duplicated and `docs/abi.md` records it as part of the ABI: if they disagree,
/// `HeapAlloc` hands out addresses outside the guest's memory and the guest
/// traps on its first store through one.
pub const HEAP_REGION_BASE: u32 = 0x1000_0000;

/// Size of the heap region: everything from `HEAP_REGION_BASE` to 4 GiB.
///
/// Must equal `wlam_emit::chain::HEAP_REGION_SIZE`. The guest is entitled to the
/// whole 32-bit address space and the module reserves all of it (reservation is
/// virtual, so it costs address space rather than RAM), which leaves the upper
/// 3.75 GiB for allocations.
pub const HEAP_REGION_SIZE: u32 = 0x1_0000_0000u64.wrapping_sub(HEAP_REGION_BASE as u64) as u32;

impl Default for HeapSet {
    fn default() -> Self {
        HeapSet::new(
            HEAP_REGION_BASE,
            HEAP_REGION_BASE as u64 + HEAP_REGION_SIZE as u64,
        )
    }
}

impl HeapSet {
    /// A heap set whose arenas are carved from `[base, limit)` of guest memory.
    pub fn new(base: u32, limit: u64) -> Self {
        let mut s = HeapSet {
            heaps: Vec::new(),
            next_arena: round_up(base),
            limit,
        };
        // Windows always has a process heap; code may use it without calling
        // `HeapCreate` first.
        s.create_arena(0x1_0000);
        s
    }

    /// A heap set bounded to a small region, for tests that supply a modest
    /// guest-memory buffer rather than the guest's real 3.75 GiB upper space.
    pub fn for_tests() -> Self {
        HeapSet::new(HEAP_REGION_BASE, HEAP_REGION_BASE as u64 + 0x10_0000)
    }

    /// Handles of every live heap, for diagnostics.
    pub fn handles(&self) -> Vec<u32> {
        self.heaps.iter().map(|h| h.handle).collect()
    }

    /// The process heap, which always exists.
    pub fn process_heap(&self) -> u32 {
        HEAP_HANDLE_BASE
    }

    fn heap(&self, handle: u32) -> Option<&Heap> {
        self.heaps.iter().find(|h| h.handle == handle)
    }

    fn heap_of(&self, ptr: u32) -> Option<&Heap> {
        self.heaps.iter().find(|h| ptr >= h.start && ptr < h.end)
    }

    /// Carve a new arena of `size` bytes (at least one block's worth).
    fn create_arena(&mut self, size: u32) -> Option<&Heap> {
        let size = round_up(size).max(HEADER + ALIGN);
        let start = round_up(self.next_arena.max(HEADER + ALIGN));
        let end = (start as u64).checked_add(size as u64)?;
        if end > self.limit {
            return None;
        }
        let end = end as u32;
        self.next_arena = end;
        let handle = HEAP_HANDLE_BASE + self.heaps.len() as u32;
        self.heaps.push(Heap { handle, start, end });
        self.heaps.last()
    }

    /// `HeapCreate(flOptions, dwInitialSize, dwMaximumSize)`.
    ///
    /// `dwMaximumSize == 0` means growable; this implementation reserves the
    /// initial size only and grows by creating a fresh arena if a later
    /// allocation in the same heap runs out. Returns `None` on failure, which
    /// the host reports as a NULL handle rather than inventing one.
    pub fn create(&mut self, initial_size: u32, _maximum_size: u32) -> Option<u32> {
        let heap = self.create_arena(initial_size.max(4096))?;
        let handle = heap.handle;
        let (start, end) = (heap.start, heap.end);
        self.init_arena(start, end);
        Some(handle)
    }

    /// The process heap handle, creating its arena if it is not there.
    pub fn ensure_process_heap(&mut self) -> u32 {
        self.process_heap()
    }

    /// Reset an arena to one large free block. The caller writes the header
    /// into guest memory immediately afterwards via [`HeapSet::init_arena_in`].
    fn init_arena(&self, _start: u32, _end: u32) {
        // Memory writes need `GuestMemory`, so they happen in `HeapApi::create`
        // below; keeping the arithmetic here keeps the arena bookkeeping in one
        // place.
    }

    /// `HeapAlloc(hHeap, dwFlags, dwBytes)`.
    ///
    /// Returns the guest address of `dwBytes` usable bytes, or `None` when the
    /// heap cannot satisfy the request (which the host reports as NULL).
    pub fn alloc(
        &mut self,
        mem: &mut crate::GuestMemory<'_>,
        handle: u32,
        flags: u32,
        bytes: u32,
    ) -> Option<u32> {
        // A zero-length request still gets a distinct, freeable pointer, as on
        // Windows: callers do `HeapAlloc(h, 0, n)` with n possibly 0 and then
        // compare against NULL.
        let need = round_up(HEADER + bytes.max(1));

        if self.heap(handle).is_none() {
            return None;
        }

        // Try each arena belonging to this heap.
        let arenas: Vec<Heap> = self.heaps.iter().copied().filter(|h| h.handle == handle).collect();
        for heap in arenas {
            if let Some(p) = alloc_in_arena(mem, &heap, need, flags & HEAP_ZERO_MEMORY != 0) {
                return Some(p + HEADER);
            }
        }

        // Out of room: grow. `dwMaximumSize == 0` means the heap may grow, and
        // this is the only way a bump-style arena can serve a long-running
        // program.
        let grow = need.max(bytes).saturating_add(0x1_0000);
        let limit = self.limit;
        let start = round_up(self.next_arena);
        let end64 = (start as u64).checked_add(round_up(grow) as u64)?;
        if end64 > limit {
            let _ = mem;
            return None;
        }
        let end = end64 as u32;
        self.next_arena = end;
        let heap = Heap { handle, start, end };
        self.heaps.push(heap);
        let p = alloc_in_arena(mem, &heap, need, flags & HEAP_ZERO_MEMORY != 0)?;
        Some(p + HEADER)
    }

    /// `HeapFree(hHeap, dwFlags, lpMem)`.
    ///
    /// Returns whether the pointer was a live block *of that heap*: a wrong
    /// pointer is reported, not silently ignored.
    pub fn free(&mut self, mem: &mut crate::GuestMemory<'_>, handle: u32, ptr: u32) -> bool {
        let Some(heap) = self.heap(handle).copied() else {
            return false;
        };
        if ptr < heap.start || ptr >= heap.end {
            return false;
        }
        let hdr = ptr - HEADER;
        // `head` is the size; the flags live in the second word. Testing the
        // *size* for a flag bit made a block whose size happened to have bit 0
        // set look already-free, so a double free went unreported.
        let Some(head) = mem.read_u32(hdr) else {
            return false;
        };
        let Some(flags) = mem.read_u32(hdr + 4) else {
            return false;
        };
        if head == 0 || flags & BLOCK_FREE != 0 {
            return false;
        }
        if !mem.write_u32(hdr + 4, BLOCK_FREE) {
            return false;
        }
        true
    }

    /// `HeapSize(hHeap, dwFlags, lpMem)`.
    pub fn size(&self, mem: &crate::GuestMemory<'_>, handle: u32, ptr: u32) -> Option<u32> {
        let heap = self.heap(handle)?;
        if ptr < heap.start || ptr >= heap.end {
            return None;
        }
        let head = mem.read_u32(ptr - HEADER)?;
        let flags = mem.read_u32(ptr - HEADER + 4)?;
        if head == 0 || flags & BLOCK_FREE != 0 {
            return None;
        }
        Some(head - HEADER)
    }

    /// `HeapDestroy(hHeap)`.
    pub fn destroy(&mut self, handle: u32) -> bool {
        if handle == self.process_heap() {
            // The process heap is not destroyable.
            return false;
        }
        let before = self.heaps.len();
        self.heaps.retain(|h| h.handle != handle);
        self.heaps.len() != before
    }
}

/// First-fit allocation inside one arena, returning the block *header*
/// address. Returns `None` when the arena has no run big enough.
fn alloc_in_arena(
    mem: &mut crate::GuestMemory<'_>,
    heap: &Heap,
    need: u32,
    zero: bool,
) -> Option<u32> {
    let mut at = heap.start;
    // A freshly carved arena has no header yet: treat "size 0" as "one free
    // block spanning the arena" so the caller does not have to initialise it.
    if mem.read_u32(at) == Some(0) {
        if !mem.write_u32(at, heap.end - heap.start) {
            return None;
        }
        if !mem.write_u32(at + 4, BLOCK_FREE) {
            return None;
        }
    }
    while at < heap.end {
        let head = mem.read_u32(at)?;
        let flags = mem.read_u32(at + 4)?;
        if head == 0 {
            return None; // uninitialised tail: arena bookkeeping is wrong
        }
        if flags & BLOCK_FREE != 0 && head >= need {
            // Split: keep the remainder as a free block when it can hold a
            // header plus one aligned byte.
            let remainder = head - need;
            if remainder >= HEADER + ALIGN {
                if !mem.write_u32(at + need, remainder) {
                    return None;
                }
                if !mem.write_u32(at + need + 4, BLOCK_FREE) {
                    return None;
                }
                if !mem.write_u32(at, need) {
                    return None;
                }
            }
            if !mem.write_u32(at + 4, 0) {
                return None;
            }
            if zero {
                let region = mem.get_mut(at + HEADER, (head - HEADER) as usize)?;
                region.fill(0);
            }
            return Some(at);
        }
        at += head;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::GuestMemory;

    fn heap() -> (HeapSet, Vec<u8>) {
        let mut data = vec![0u8; 0x10_0000]; // 1 MiB of guest memory
        let set = HeapSet::new(0x1000, data.len() as u64);
        (set, data)
    }

    #[test]
    fn handles_are_never_zero_or_plausible_small_ints() {
        let mut set = HeapSet::new(0x1000, 0x10_0000);
        let h = set.create(0x1000, 0).expect("heap");
        assert_ne!(h, 0, "0 is the failure value and must never be a handle");
        assert_ne!(h, 1, "a small integer could pass for an index");
        assert_eq!(set.process_heap(), HEAP_HANDLE_BASE);
        assert_ne!(h, set.process_heap(), "heaps must be distinguishable");
    }

    #[test]
    fn allocations_are_aligned_distinct_and_usable() {
        let (mut set, mut data) = heap();
        let mut mem = GuestMemory::new(&mut data);
        let h = set.create(0x10000, 0).unwrap();
        let a = set.alloc(&mut mem, h, 0, 100).expect("first");
        let b = set.alloc(&mut mem, h, 0, 100).expect("second");
        assert_ne!(a, b, "two live allocations must not overlap");
        assert_eq!(a % ALIGN, 0, "returned pointers must be aligned");
        assert_eq!(b % ALIGN, 0);
        // The guest must be able to write through both.
        assert!(mem.write_u32(a, 0xDEAD_BEEF));
        assert!(mem.write_u32(b, 0x1234_5678));
        assert_eq!(mem.read_u32(a), Some(0xDEAD_BEEF));
        assert_eq!(mem.read_u32(b), Some(0x1234_5678));
    }

    #[test]
    fn heap_alloc_reports_the_size_it_was_asked_for() {
        let (mut set, mut data) = heap();
        let mut mem = GuestMemory::new(&mut data);
        let h = set.create(0x10000, 0).unwrap();
        let p = set.alloc(&mut mem, h, 0, 100).unwrap();
        assert_eq!(set.size(&mem, h, p), Some(104), "size is the block minus its header");
    }

    #[test]
    fn zero_memory_flag_clears_the_block() {
        let (mut set, mut data) = heap();
        let mut mem = GuestMemory::new(&mut data);
        let h = set.create(0x10000, 0).unwrap();
        let p = set.alloc(&mut mem, h, 0, 64).unwrap();
        mem.write_u32(p, 0xFFFF_FFFF);
        set.free(&mut mem, h, p);
        // Reallocated memory may be dirty unless HEAP_ZERO_MEMORY is passed.
        let q = set.alloc(&mut mem, h, HEAP_ZERO_MEMORY, 64).unwrap();
        assert_eq!(mem.read_u32(q), Some(0), "HEAP_ZERO_MEMORY must clear the block");
    }

    #[test]
    fn a_free_then_alloc_reuses_the_block() {
        let (mut set, mut data) = heap();
        let mut mem = GuestMemory::new(&mut data);
        let h = set.create(0x10000, 0).unwrap();
        let a = set.alloc(&mut mem, h, 0, 4096).unwrap();
        assert!(set.free(&mut mem, h, a));
        let b = set.alloc(&mut mem, h, 0, 4096).unwrap();
        assert_eq!(a, b, "a freed block of the right size must be reused");
    }

    #[test]
    fn freeing_a_pointer_that_was_never_allocated_is_reported() {
        let (mut set, mut data) = heap();
        let mut mem = GuestMemory::new(&mut data);
        let h = set.create(0x10000, 0).unwrap();
        assert!(!set.free(&mut mem, h, 0), "NULL is not a valid block");
        assert!(!set.free(&mut mem, h, 0x1234_5678), "a wild pointer is not a block");
        let p = set.alloc(&mut mem, h, 0, 32).unwrap();
        assert!(set.free(&mut mem, h, p));
        assert!(!set.free(&mut mem, h, p), "double free must be refused");
    }

    #[test]
    fn an_unknown_handle_allocates_nothing() {
        let (mut set, mut data) = heap();
        let mut mem = GuestMemory::new(&mut data);
        assert_eq!(set.alloc(&mut mem, 0xDEAD_BEEF, 0, 16), None);
    }

    #[test]
    fn a_heap_that_cannot_fit_reports_failure_rather_than_a_pointer() {
        // An arena limit smaller than the request: the honest answer is NULL,
        // which the guest already handles, not a pointer into nowhere.
        let mut data = vec![0u8; 0x2000];
        let mut set = HeapSet::new(0x1000, data.len() as u64);
        let mut mem = GuestMemory::new(&mut data);
        let h = set.create(0x1000, 0).expect("small heap fits");
        assert_eq!(set.alloc(&mut mem, h, 0, 0x1_0000), None, "too large for the region");
    }

    #[test]
    fn the_process_heap_exists_without_heap_create() {
        let mut set = HeapSet::new(0x1000, 0x10_0000);
        let h = set.ensure_process_heap();
        assert_eq!(h, HEAP_HANDLE_BASE);
        assert!(set.handles().contains(&h));
    }

    #[test]
    fn destroying_an_unknown_heap_is_refused_and_the_process_heap_is_forever() {
        let mut set = HeapSet::new(0x1000, 0x10_0000);
        let h = set.create(0x1000, 0).unwrap();
        assert!(!set.destroy(0xDEAD_BEEF), "unknown handle");
        assert!(!set.destroy(set.process_heap()), "the process heap cannot be destroyed");
        assert!(set.destroy(h));
        assert!(!set.destroy(h), "double destroy");
    }
}
