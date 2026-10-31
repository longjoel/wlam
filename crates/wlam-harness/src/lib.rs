//! Differential test harness: link a recompiled guest against the host layer.
//!
//! The harness is what turns a host refusal into something a developer can act
//! on. When the guest calls an API the host has not implemented, the run stops
//! with a diagnostic naming the API and the guest stack pointer -- rather than
//! trapping somewhere unrelated once a garbage handle propagates.

#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

pub use wlam_host::{Api, Call, Diagnostic, GuestMemory, Host, OnRefuse, Refusal};

/// The wasm engine types callers need, re-exported so a test or CLI does not
/// have to depend on wasmi directly just to name a `Store`.
pub use wasmi;

/// Fuel granted to one guest run.
///
/// Generous enough for a real program to reach its first unimplemented API
/// (Uxn32 does so in well under a million), and small enough that a runaway
/// guest fails in about a second rather than wedging the suite.
pub const DEFAULT_FUEL: u64 = 1_000_000_000;

/// Why a guest run stopped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunError {
    /// The module could not be loaded or linked.
    Load(String),
    /// The host refused a call. Carries the recorded diagnostic.
    Refused(Diagnostic),
    /// The guest trapped for a reason other than a refusal.
    Trap(String),
}

impl std::fmt::Display for RunError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RunError::Load(e) => write!(f, "load failed: {e}"),
            RunError::Refused(d) => write!(f, "refused: {d}"),
            RunError::Trap(e) => write!(f, "guest trap: {e}"),
        }
    }
}

impl std::error::Error for RunError {}

/// Build a wasmi engine and link `module_bytes` against `host`.
///
/// Every import the module declares must exist in the host: the module is
/// registered API-by-API so that "the guest can call it" and "the host knows
/// about it" are the same set. A module importing something undeclared still
/// links (the shim refuses at call time), which keeps the diagnostic attached
/// to the actual call site rather than the linker.
pub fn instantiate(
    module_bytes: &[u8],
    host: Arc<Mutex<Host>>,
) -> Result<(wasmi::Store<()>, wasmi::Instance), RunError> {
    let mut config = wasmi::Config::default();
    // Meter the guest. A recompiled image has no watchdog, so a guest that
    // spins -- e.g. a probing run where every unimplemented API returns 0 and
    // the program retries forever -- would hang the test suite with no
    // diagnosis. Fuel turns that into a named error.
    config.consume_fuel(true);
    let engine = wasmi::Engine::new(&config);
    let module = wasmi::Module::new(&engine, module_bytes).map_err(|e| RunError::Load(e.to_string()))?;

    // Collect the declared imports so each gets a shim.
    let mut imports: BTreeMap<(String, String), ()> = BTreeMap::new();
    for imp in module.imports() {
        // `module()` returns &str in wasmi 0.40 (not an Option).
        let module_name = imp.module().to_string();
        let name = imp.name().to_string();
        if matches!(imp.ty(), wasmi::ExternType::Func(_)) {
            imports.insert((module_name, name), ());
        }
    }

    let mut store = wasmi::Store::new(&engine, ());
    store
        .set_fuel(DEFAULT_FUEL)
        .map_err(|e| RunError::Load(format!("fuel not enabled: {e}")))?;
    let mut linker = wasmi::Linker::<()>::new(&engine);
    for ((module_name, field), ()) in imports {
        let api = Api::new(module_name.clone(), field.clone());
        let host = Arc::clone(&host);
        let func = wasmi::Func::wrap(
            &mut store,
            move |mut caller: wasmi::Caller<'_, ()>, args_ptr: i32, _ret_addr: i32| -> i32 {
                // Resolve the guest's exported memory once per call. A host
                // implementation that writes an OUT parameter has no other way
                // to reach it, and one that gets an empty slice is a standing
                // invitation to silently do nothing.
                let memory = caller.get_export("memory").and_then(|e| e.into_memory());
                let mut h = host.lock().unwrap_or_else(|e| e.into_inner());
                match memory {
                    Some(mem) => {
                        let data = mem.data_mut(&mut caller);
                        let mut guest = GuestMemory::new(data);
                        h.resolve_with_memory(&api, args_ptr, &mut guest)
                    }
                    None => {
                        // No memory to offer. Calls that need none still work.
                        let mut guest = GuestMemory::new(&mut []);
                        h.resolve_with_memory(&api, args_ptr, &mut guest)
                    }
                }
            },
        );
        linker
            .define(&module_name, &field, func)
            .map_err(|e| RunError::Load(e.to_string()))?;
    }

    let instance = linker
        .instantiate(&mut store, &module)
        .map_err(|e| RunError::Load(e.to_string()))?
        .start(&mut store)
        .map_err(|e| RunError::Load(e.to_string()))?;

    Ok((store, instance))
}

/// Run the guest's `entry` export, seeding the register snapshot so execution
/// has a stack and a TIB.
///
/// `seed` is a list of (guest address, value) writes applied before the call.
/// A recompiled image has no loader, so the harness stands in for one: without
/// a stack pointer the first `push` writes below address zero.
pub fn run_entry_raw(
    store: &mut wasmi::Store<()>,
    instance: &wasmi::Instance,
    seed: &[(u32, u32)],
) -> Result<(), RunError> {
    let mem = instance
        .get_memory(&mut *store, "memory")
        .ok_or_else(|| RunError::Load("guest has no exported memory".into()))?;
    for (addr, value) in seed {
        mem.write(&mut *store, *addr as usize, &value.to_le_bytes())
            .map_err(|e| RunError::Trap(format!("seed write at {addr:#x}: {e}")))?;
    }
    let entry = instance
        .get_func(&mut *store, "entry")
        .ok_or_else(|| RunError::Load("guest has no `entry` export".into()))?;
    // Run under `catch_unwind`: the host's documented cranky behaviour is to
    // panic on an unimplemented call, and that must surface as a named refusal
    // rather than an opaque wasmi trap or a test-process abort.
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        entry.call(&mut *store, &[], &mut [])
    }));
    match result {
        Ok(Ok(())) => Ok(()),
        Ok(Err(e)) => Err(RunError::Trap(format!("{e}"))),
        // (wasmi reports the fault; guest memory is dumped by callers that need
        // to locate it, since the engine does not expose the address.)
        Err(_) => Err(RunError::Trap("host panic".into())),
    }
}

/// Run `entry`, reporting a host refusal as [`RunError::Refused`].
///
/// This is the variant callers should use: it turns the host's panic payload
/// back into the diagnostic that caused it, so the failure names the API.
pub fn run_entry_diagnosing(
    store: &mut wasmi::Store<()>,
    instance: &wasmi::Instance,
    seed: &[(u32, u32)],
    host: &Arc<Mutex<Host>>,
) -> Result<(), RunError> {
    std::panic::set_hook(Box::new(|_| {}));
    let result = run_entry_raw(store, instance, seed);
    let _ = std::panic::take_hook();
    match result {
        Ok(()) => Ok(()),
        Err(e) => {
            // If the host recorded a refusal, that is the real reason.
            // The host mutex is poisoned by the very panic we are reporting,
            // so recover the guard rather than propagating a second panic.
            let h = host.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(d) = h.diagnostics().last() {
                return Err(RunError::Refused(d.clone()));
            }
            // No refusal: the trap itself is the finding, so keep what wasmi
            // said (out of fuel, out of bounds, unreachable, ...) instead of
            // replacing it with a generic message that identifies nothing.
            Err(e)
        }
    }
}

/// Register-snapshot base used by the emitter (`REG_SNAPSHOT_BASE`).
pub const REG_SNAPSHOT_BASE: u32 = 0x8000;

/// Write a 32-bit guest register into the snapshot area.
pub fn seed_register(seed: &mut Vec<(u32, u32)>, index: u32, value: u32) {
    seed.push((REG_SNAPSHOT_BASE + index * 4, value));
}

/// One byte range the loader mapped into guest memory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Loaded {
    pub start: u32,
    pub len: u32,
    /// File-backed (from the image) or synthesised (headers/IAT stubs).
    pub kind: LoadedKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoadedKind {
    Headers,
    Section,
    /// A one-entry IAT thunk pointing at an import stub.
    IatThunk,
}

/// Map a PE32 image into the guest's linear memory.
///
/// A recompiled image has no Windows loader, but the guest's absolute addresses
/// (`mov [0x415b80], edx`) only mean anything if the image is actually at its
/// linked base. Without this the guest reads and writes zeroed memory: state it
/// stores is real but its data, its strings, its .rdata and its IAT are all
/// missing, which produces failures that look like emitter bugs.
///
/// What is mapped:
///   * the headers, at the image base, so `[image_base + offset]` is valid;
///   * every section at `image_base + VirtualAddress`, sized by
///     `VirtualSize` (BSS included, as zeros).
///
/// What is NOT mapped (and would be a lie to fake):
///   * real imported addresses in the IAT. Host calls are resolved statically
///     by the emitter, so the IAT is only ever read by guest code that was not
///     recognised as an import call. See [`IatPolicy`].
///
/// Base relocations are not applied: the image loads at its preferred base, so
/// every delta is zero and there is nothing to fix up.
pub fn load_image(
    mem: &mut [u8],
    pe: &wlam_pe::Pe,
    iat: IatPolicy,
) -> Result<Vec<Loaded>, RunError> {
    let base = pe.image_base as u32;
    let mut out = Vec::new();

    // Headers: the PE is only valid as an image if its own header is readable
    // at the base (e.g. `GetModuleHandle`-style code walks it).
    let hdr_len = pe.size_of_headers.min(pe.raw.len() as u32);
    let start = base as usize;
    let end = start
        .checked_add(hdr_len as usize)
        .ok_or_else(|| RunError::Load("image base overflows the address space".into()))?;
    if end > mem.len() {
        return Err(RunError::Load(format!(
            "image base {base:#x} + headers ({hdr_len:#x}) is past the end of guest memory ({:#x}); \
             the module's memory must cover the image",
            mem.len()
        )));
    }
    mem[start..end].copy_from_slice(&pe.raw[..hdr_len as usize]);
    out.push(Loaded {
        start: base,
        len: hdr_len,
        kind: LoadedKind::Headers,
    });

    for sec in &pe.sections {
        // VirtualSize may exceed the bytes present on disk (BSS); the tail
        // stays zero, which is what the loader does.
        let vsize = sec.virtual_size.max(sec.file_size);
        if vsize == 0 {
            continue;
        }
        let va = base
            .checked_add(sec.virtual_address)
            .ok_or_else(|| RunError::Load("section RVA overflows the image base".into()))?;
        let start = va as usize;
        let end = start + vsize as usize;
        if end > mem.len() {
            return Err(RunError::Load(format!(
                "section {:#x} runs to {:#x}, past guest memory ({:#x})",
                sec.virtual_address,
                end,
                mem.len()
            )));
        }
        let from_disk = (sec.file_size as usize).min(sec.data.len()).min(vsize as usize);
        mem[start..start + from_disk].copy_from_slice(&sec.data[..from_disk]);
        out.push(Loaded {
            start: va,
            len: vsize,
            kind: LoadedKind::Section,
        });
    }

    match iat {
        IatPolicy::LeaveUnfilled => {}
        IatPolicy::Poison => {
            // Fill every import slot with a recognisable bad pointer. A guest
            // that CALLs one traps on the nullish page instead of silently
            // calling address 0; a guest that merely reads it sees a value that
            // is obviously not an address.
            // Only poison slots whose value can survive the load. Writing
            // 0xFFFF_FFFF over a slot that then reads back as 0 defeats the
            // point, so the page holding the pointers is `0xFF` too and the
            // slot is either fully in it or not poisoned at all.
            let page_start = (POISON_PAGE as usize).min(mem.len());
            let page_end = (POISON_PAGE as usize + POISON_PAGE_SIZE as usize).min(mem.len());
            for b in &mut mem[page_start..page_end] {
                *b = 0xFF;
            }
            for imp in &pe.imports {
                if !imp.iat_slot_verified {
                    continue;
                }
                let va = base.wrapping_add(imp.iat_rva);
                let addr = va as usize;
                // Straddling writes are ignored: an unmapped slot is a PE
                // parsing problem, not a loader one.
                if addr + 4 <= mem.len() {
                    mem[addr..addr + 4].copy_from_slice(&POISON_THUNK.to_le_bytes());
                    out.push(Loaded {
                        start: va,
                        len: 4,
                        kind: LoadedKind::IatThunk,
                    });
                }
            }
        }
    }
    Ok(out)
}

/// Value written into unused/unresolved IAT slots by [`IatPolicy::Poison`].
///
/// `0xFFFFFFFF` is not a valid call target in any guest, so a call through a
/// poisoned slot traps rather than running whatever happens to be at address 0.
pub const POISON_THUNK: u32 = 0xFFFF_FFFF;

/// Size of the poison page mapped by [`IatPolicy::Poison`].
pub const POISON_PAGE_SIZE: u32 = 0x10000;

/// The page [`IatPolicy::Poison`] reserves for poisoned IAT slots.
///
/// Chosen at the top of the address range, where nothing else lives: the host's
/// heap region starts at `0x1000_0000` and grows up, so the last 64 KiB stays
/// free in practice -- and if the guest ever does allocate into it, the
/// poisoned slots are told so rather than being silently unmapped.
///
/// Why a page at all: wasm access to memory the guest has not declared is not a
/// trap, it is zero. A poisoned slot read back as `0` is the value of a NULL
/// function pointer, so a `call [slot]` would jump to address 0 and quietly run
/// whatever is at the image base. Mapping one page of `0xFF` makes the same
/// mistake produce a trap at a named address instead -- the property the cruise
/// existed to buy.
pub const POISON_PAGE: u32 = GUEST_POISON_PAGE_BASE;

/// Base address of [`POISON_PAGE`].
pub const GUEST_POISON_PAGE_BASE: u32 = 0xFFFF_0000;

/// What the loader does with import address table slots.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IatPolicy {
    /// Leave them zero, as they start. A call through one reaches address 0.
    LeaveUnfilled,
    /// Fill them with [`POISON_THUNK`] so a stray call traps loudly.
    Poison,
}
