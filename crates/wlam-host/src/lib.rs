//! Win32 host layer implemented in Rust (heap, IO, windows, graphics, audio).
//!
//! ## Cranky by default
//!
//! The host refuses to guess. An import that has not been *deliberately*
//! implemented does not return a plausible zero — it traps and records why.
//!
//! This is a deliberate inversion of the usual shim behaviour. A stub that
//! returns `0` for `OpenFileA` is indistinguishable at run time from a real
//! implementation reporting "no file", so the guest carries on with a garbage
//! handle and the eventual failure surfaces far from its cause. A trap with a
//! recorded diagnostic points at the actual gap.
//!
//! The cost is that programs do not limp along; they stop at the first thing
//! that is not implemented. That is the intended trade: this layer exists to
//! be *truthful* about what it supports, and `docs/abi.md` already requires the
//! diagnostic.
//!
//! ## What is registered
//!
//! Every import the module declares is registered, so the set is exactly the
//! set the guest can reach. `Host::unimplemented()` is the default for all of
//! them; `Host::implement` replaces one with real behaviour and records that it
//! was deliberate.

#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::fmt;

pub mod heap;
pub mod startup;
pub mod version;

pub use version::{Platform, WindowsVersion};

/// A Win32 API identified the way the module imports it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Api {
    /// WASM import namespace: the DLL name as written in the image.
    pub library: String,
    /// Import field: the symbol name or `#Ordinal`.
    pub name: String,
}

impl Api {
    pub fn new(library: impl Into<String>, name: impl Into<String>) -> Self {
        Api {
            library: library.into(),
            name: name.into(),
        }
    }
}

impl fmt::Display for Api {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}!{}", self.library, self.name)
    }
}

/// Why a host call could not be served.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// No implementation was registered for this API at all.
    Unimplemented,
    /// Registered, but deliberately marked as out of scope, with a reason.
    OutOfScope(String),
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Refusal::Unimplemented => write!(f, "not implemented"),
            Refusal::OutOfScope(why) => write!(f, "out of scope: {why}"),
        }
    }
}

/// One recorded diagnostic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    pub api: Api,
    pub refusal: Refusal,
    /// Guest stack pointer at the call, i.e. where the arguments are.
    pub args_ptr: i32,
}

impl fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}: {} (args at {:#x})",
            self.api, self.refusal, self.args_ptr
        )
    }
}

/// What the host does when a call cannot be served.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnRefuse {
    /// Record the diagnostic and trap. This is the default: the guest cannot
    /// silently continue past something that is not implemented.
    Trap,
    /// Record the diagnostic and return a value. Only for callers that are
    /// deliberately probing behaviour (tests, feature discovery).
    Return(i32),
}

/// The outcome of one host call.
pub type Outcome = Result<i32, Diagnostic>;

/// Guest linear memory, as the host sees it.
///
/// Host callbacks receive this instead of a bare slice because `args_ptr` alone
/// cannot express a call like `GetVersionExA(lpVersionInformation)`: an OUT
/// parameter means the host has to *write* into the guest, and a call like
/// `CreateFileA(lpFileName, ...)` means it has to read a NUL-terminated string
/// out of it. Passing the whole memory keeps every option open without the
/// host having to know the guest's address space is a wasm memory.
pub struct GuestMemory<'a> {
    data: &'a mut [u8],
}

impl<'a> GuestMemory<'a> {
    pub fn new(data: &'a mut [u8]) -> Self {
        GuestMemory { data }
    }

    /// Total size of the guest's address space.
    pub fn len(&self) -> usize {
        self.data.len()
    }

    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    /// The bytes at `addr..addr+len`, or `None` if the guest asked for memory
    /// it does not have. A host that silently returned zeros here would be
    /// lying about a wild pointer, so the absence is reported.
    pub fn get(&self, addr: u32, len: usize) -> Option<&[u8]> {
        let start = addr as usize;
        let end = start.checked_add(len)?;
        self.data.get(start..end)
    }

    /// Mutable access to the same range, with the same all-or-nothing rule.
    pub fn get_mut(&mut self, addr: u32, len: usize) -> Option<&mut [u8]> {
        let start = addr as usize;
        let end = start.checked_add(len)?;
        self.data.get_mut(start..end)
    }

    /// An `i32` from the little-endian bytes at `addr`.
    pub fn read_u32(&self, addr: u32) -> Option<u32> {
        let b = self.get(addr, 4)?;
        Some(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    /// Write an `i32` little-endian at `addr`.
    pub fn write_u32(&mut self, addr: u32, value: u32) -> bool {
        match self.get_mut(addr, 4) {
            Some(b) => {
                b.copy_from_slice(&value.to_le_bytes());
                true
            }
            None => false,
        }
    }

    /// Read a NUL-terminated byte string starting at `addr`.
    ///
    /// Returns `None` if `addr` is out of bounds or no terminator is found
    /// before the end of memory. An empty-but-valid string is `Some(&[])`,
    /// which is deliberately distinct from a bad pointer.
    pub fn read_cstr(&self, addr: u32) -> Option<Vec<u8>> {
        let start = addr as usize;
        if start >= self.data.len() {
            return None;
        }
        let tail = &self.data[start..];
        let end = tail.iter().position(|&b| b == 0)?;
        Some(tail[..end].to_vec())
    }
}

/// What a host callback is given.
pub struct Call<'a> {
    /// Guest stack pointer at the call: the arguments begin here.
    pub args_ptr: u32,
    /// Guest linear memory.
    pub mem: &'a mut GuestMemory<'a>,
}

/// A host function. Returns EAX.
///
/// Host callbacks must be `Send` because the harness links them into a wasmi
/// instance, whose host functions carry a `Send` bound. `&mut GuestMemory`
/// keeps the borrow inside one call, so `Send` is preserved.
type Impl = Box<dyn FnMut(Call<'_>) -> i32 + Send>;

/// A registered import.
struct Entry {
    behaviour: Behaviour,
    calls: u64,
}

enum Behaviour {
    Implemented(Impl),
    Refused(Refusal),
}

/// The host layer for one guest module.
///
/// Construct with [`Host::new`], implement what you support, then hand it to
/// the wasm linker as the set of imports.
pub struct Host {
    entries: BTreeMap<Api, Entry>,
    on_refuse: OnRefuse,
    diagnostics: Vec<Diagnostic>,
    traps: u64,
}

impl Default for Host {
    fn default() -> Self {
        Self::new()
    }
}

impl Host {
    /// An empty host. Every API added with [`Host::declare`] starts as
    /// unimplemented, which is the cranky default.
    pub fn new() -> Self {
        Host {
            entries: BTreeMap::new(),
            on_refuse: OnRefuse::Trap,
            diagnostics: Vec::new(),
            traps: 0,
        }
    }

    /// Declare an API the module imports. It is deliberately *not* implemented:
    /// declaring only reserves the name and makes the gap explicit.
    pub fn declare(&mut self, api: Api) -> &mut Self {
        self.entries.entry(api).or_insert(Entry {
            behaviour: Behaviour::Refused(Refusal::Unimplemented),
            calls: 0,
        });
        self
    }

    /// Implement an API whose behaviour depends only on its arguments.
    ///
    /// Convenience over [`Host::implement_with_memory`] for the common case of
    /// an API that computes a value from its arguments and touches no guest
    /// memory (e.g. `GetVersion`, `GetTickCount`).
    pub fn implement(
        &mut self,
        api: Api,
        mut f: impl FnMut(i32) -> i32 + Send + 'static,
    ) -> &mut Self {
        self.implement_with_memory(api, move |c: Call<'_>| f(c.args_ptr as i32))
    }

    /// Implement an API with full access to guest memory.
    pub fn implement_with_memory(
        &mut self,
        api: Api,
        f: impl FnMut(Call<'_>) -> i32 + Send + 'static,
    ) -> &mut Self {
        self.entries.insert(
            api,
            Entry {
                behaviour: Behaviour::Implemented(Box::new(f)),
                calls: 0,
            },
        );
        self
    }

    /// Mark an API as deliberately out of scope, with the reason recorded.
    ///
    /// Distinct from leaving it undeclared: this records a decision rather
    /// than an omission.
    pub fn out_of_scope(&mut self, api: Api, why: impl Into<String>) -> &mut Self {
        self.entries.insert(
            api,
            Entry {
                behaviour: Behaviour::Refused(Refusal::OutOfScope(why.into())),
                calls: 0,
            },
        );
        self
    }

    /// Change what happens on refusal. `Trap` (the default) is the cranky mode.
    pub fn on_refuse(&mut self, policy: OnRefuse) -> &mut Self {
        self.on_refuse = policy;
        self
    }

    /// Serve one call. Returns the value to hand back to the guest, or a
    /// diagnostic explaining why it cannot be served.
    ///
    /// The caller is expected to trap when this returns `Err`; the guest must
    /// never continue past an unimplemented API.
    pub fn call(&mut self, api: &Api, args_ptr: i32) -> Outcome {
        self.call_with_memory(api, args_ptr, &mut GuestMemory::new(&mut []))
    }

    /// Serve one call with access to the guest's linear memory.
    ///
    /// An OUT parameter (`GetVersionExA`'s `lpVersionInformation`) is only
    /// servable through this entry point: writing it is half of what the API
    /// does, and reporting success without the write would be the exact kind
    /// of plausible-looking lie this layer exists to avoid.
    pub fn call_with_memory<'a>(
        &mut self,
        api: &Api,
        args_ptr: i32,
        mem: &mut GuestMemory<'a>,
    ) -> Outcome {
        let Some(entry) = self.entries.get_mut(api) else {
            // Not even declared: the module imported something the host was
            // never told about. Refuse rather than pretend.
            let d = Diagnostic {
                api: api.clone(),
                refusal: Refusal::Unimplemented,
                args_ptr,
            };
            self.diagnostics.push(d.clone());
            self.traps += 1;
            return Err(d);
        };
        entry.calls += 1;
        match &mut entry.behaviour {
            Behaviour::Implemented(f) => {
                // Reborrow the slice for exactly this call: the callback
                // cannot keep a handle to guest memory past its return.
                let mut scoped = GuestMemory { data: mem.data };
                let call = Call {
                    args_ptr: args_ptr as u32,
                    mem: &mut scoped,
                };
                Ok(f(call))
            }
            Behaviour::Refused(refusal) => {
                let d = Diagnostic {
                    api: api.clone(),
                    refusal: refusal.clone(),
                    args_ptr,
                };
                self.diagnostics.push(d.clone());
                self.traps += 1;
                Err(d)
            }
        }
    }

    /// Resolve a call according to the refusal policy.
    ///
    /// This is the function a wasm import shim calls: it either yields a value
    /// or panics with the diagnostic, which the harness turns into a trap.
    pub fn resolve(&mut self, api: &Api, args_ptr: i32) -> i32 {
        self.resolve_with_memory(api, args_ptr, &mut GuestMemory::new(&mut []))
    }

    /// Resolve a call with guest memory attached to it.
    pub fn resolve_with_memory(
        &mut self,
        api: &Api,
        args_ptr: i32,
        mem: &mut GuestMemory<'_>,
    ) -> i32 {
        match self.call_with_memory(api, args_ptr, mem) {
            Ok(v) => v,
            Err(d) => match self.on_refuse {
                OnRefuse::Trap => panic!("wlam host refused: {d}"),
                OnRefuse::Return(v) => v,
            },
        }
    }

    /// Every diagnostic recorded so far, oldest first.
    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }

    /// Number of refused calls.
    pub fn traps(&self) -> u64 {
        self.traps
    }

    /// Calls served per API, for reporting coverage.
    pub fn call_counts(&self) -> BTreeMap<&Api, u64> {
        self.entries.iter().map(|(k, v)| (k, v.calls)).collect()
    }

    /// APIs that are declared but not implemented.
    pub fn unimplemented(&self) -> Vec<&Api> {
        self.entries
            .iter()
            .filter(|(_, e)| matches!(e.behaviour, Behaviour::Refused(_)))
            .map(|(k, _)| k)
            .collect()
    }

    /// APIs that have been deliberately implemented.
    pub fn implemented(&self) -> Vec<&Api> {
        self.entries
            .iter()
            .filter(|(_, e)| matches!(e.behaviour, Behaviour::Implemented(_)))
            .map(|(k, _)| k)
            .collect()
    }
}

/// The Windows version every host implementation is written against.
///
/// One constant, referenced from `GetVersion` and (later) `GetVersionExA`,
/// because two APIs reporting different versions to the same program is a bug
/// that only shows up as inexplicable divergence.
pub const HOST_VERSION: WindowsVersion = WindowsVersion::WIN2000;

/// Register the KERNEL32 APIs this host genuinely implements.
///
/// Registering is not the same as supporting: everything here is deliberately
/// wired, and anything the module imports but this function does not touch
/// stays refused. That difference is the whole point -- `Host::implemented()`
/// is the honest list of what the host does.
pub fn kernel32(h: &mut Host) -> &mut Host {
    let api = |name: &str| Api::new("KERNEL32.dll", name);

    // `GetVersion` -> DWORD. No arguments and no memory access: the value is a
    // property of the host, not of the call.
    h.implement(api("GetVersion"), |_| HOST_VERSION.version_dword() as i32);

    /// The heap set `kernel32` serves from.
    ///
    /// Split out so tests can supply one bounded to their own buffer: the real
    /// set spans the guest's upper 3.75 GiB, which no test should allocate.
    #[cfg(not(test))]
    fn heap_set() -> heap::HeapSet {
        heap::HeapSet::default()
    }

    #[cfg(test)]
    fn heap_set() -> heap::HeapSet {
        heap::HeapSet::for_tests()
    }

    // A poisoned lock means a previous call panicked mid-mutation. Recovering
    // the guard is deliberate: refusing to allocate afterwards would turn one
    // trap into a cascade of unrelated failures.
    fn lock(
        heap: &std::sync::Arc<std::sync::Mutex<heap::HeapSet>>,
    ) -> std::sync::MutexGuard<'_, heap::HeapSet> {
        heap.lock().unwrap_or_else(|e| e.into_inner())
    }

    // `Arc<Mutex<_>>` rather than `Rc<RefCell<_>>`: host callbacks must be `Send`
    // because the harness shares them across threads. The lock is only ever
    // held for the duration of one call.
    let heap_shared = std::sync::Arc::new(std::sync::Mutex::new(heap_set()));

    // `GetStartupInfoA(lpStartupInfo) -> void`
    //
    // Uxn32 calls this at 0x40cd51 with a local `STARTUPINFOA` and branches on
    // `dwFlags & STARTF_USESHOWWINDOW`. Reporting "no flags" is truthful here:
    // there is no parent process, so there is genuinely nothing to report, and
    // the guest handles that by taking its default.
    {
        h.implement_with_memory(api("GetStartupInfoA"), move |c: Call<'_>| {
            let at = match c.mem.read_u32(c.args_ptr) {
                Some(v) => v,
                None => return 0,
            };
            // `GetStartupInfoA` returns void; a refused write is still reported
            // by leaving the struct untouched, which the guest's zeroed local
            // already treats as "no flags".
            let _ = startup::StartupInfo::default().write(c.mem, at);
            0
        });
    }

    // `SetHandleCount(wNumber) -> UINT`
    //
    // Windows returns the number of handles actually granted. This host imposes
    // no per-process handle limit, so the honest answer is the number asked
    // for. Echoing it is not a cop-out: returning a fixed constant would be
    // wrong for any caller that asks for a different number, and returning 0
    // would claim a limit that does not exist.
    h.implement_with_memory(api("SetHandleCount"), move |c: Call<'_>| {
        c.mem.read_u32(c.args_ptr).unwrap_or(0) as i32
    });

    // `GetVersionExA(lpVersionInformation) -> BOOL`
    //
    // Uxn32 calls this with an `OSVERSIONINFOA` whose `dwOSVersionInfoSize` is
    // 0x94 (148) at 0x40d6df. Under a stub returning 0 the guest could not
    // determine the platform and spun forever, so this is on the critical path
    // for startup even though `GetVersion` already worked.
    {
        h.implement_with_memory(api("GetVersionExA"), move |c: Call<'_>| {
            let at = match c.mem.read_u32(c.args_ptr) {
                Some(v) => v,
                None => return 0, // arg pointer itself is out of bounds
            };
            match HOST_VERSION.write_os_version_info(c.mem, at) {
                Ok(()) => 1, // TRUE
                // The real API returns FALSE and sets an error code; the guest
                // treats FALSE as "unknown platform" and takes its fallback.
                Err(_) => 0,
            }
        });
    }

    // `GetVersionExW` cannot be served from bytes: its structure holds UTF-16
    // text. It is deliberately left refusing rather than half-implemented.

    // `HeapCreate(flOptions, dwInitialSize, dwMaximumSize) -> HANDLE`
    //
    // Uxn32 calls this at 0x40d838 with (flOptions, 0x1000, 0) and stores the
    // result at 0x415f48, branching to its failure path when it is 0. Returning
    // a real handle here is what lets the rest of the program allocate.
    {
        let heap = std::sync::Arc::clone(&heap_shared);
        h.implement_with_memory(api("HeapCreate"), move |c: Call<'_>| {
            let initial = c.mem.read_u32(c.args_ptr + 4).unwrap_or(0);
            let maximum = c.mem.read_u32(c.args_ptr + 8).unwrap_or(0);
            match lock(&heap).create(initial, maximum) {
                Some(handle) => handle as i32,
                // The real API returns NULL on failure; the guest checks for it.
                None => 0,
            }
        });
    }

    // `GetProcessHeap() -> HANDLE`
    {
        let heap = std::sync::Arc::clone(&heap_shared);
        h.implement(api("GetProcessHeap"), move |_| {
            lock(&heap).ensure_process_heap() as i32
        });
    }

    // `HeapAlloc(hHeap, dwFlags, dwBytes) -> LPVOID`
    {
        let heap = std::sync::Arc::clone(&heap_shared);
        h.implement_with_memory(api("HeapAlloc"), move |c: Call<'_>| {
            let h_heap = c.mem.read_u32(c.args_ptr).unwrap_or(0);
            let flags = c.mem.read_u32(c.args_ptr + 4).unwrap_or(0);
            let bytes = c.mem.read_u32(c.args_ptr + 8).unwrap_or(0);
            // A failed allocation returns NULL, which the guest already handles.
            lock(&heap)
                .alloc(c.mem, h_heap, flags, bytes)
                .unwrap_or(0) as i32
        });
    }

    // `HeapFree(hHeap, dwFlags, lpMem) -> BOOL`
    {
        let heap = std::sync::Arc::clone(&heap_shared);
        h.implement_with_memory(api("HeapFree"), move |c: Call<'_>| {
            let h_heap = c.mem.read_u32(c.args_ptr).unwrap_or(0);
            let ptr = c.mem.read_u32(c.args_ptr + 8).unwrap_or(0);
            i32::from(lock(&heap).free(c.mem, h_heap, ptr))
        });
    }

    // `HeapReAlloc(hHeap, dwFlags, lpMem, dwBytes) -> LPVOID`
    //
    // Growing or shrinking in place when the block already has room; otherwise
    // allocate, copy the smaller of the two sizes, and free the original. The
    // real API may move a block, so callers already expect a new pointer.
    {
        let heap = std::sync::Arc::clone(&heap_shared);
        h.implement_with_memory(api("HeapReAlloc"), move |c: Call<'_>| {
            let h_heap = c.mem.read_u32(c.args_ptr).unwrap_or(0);
            let flags = c.mem.read_u32(c.args_ptr + 4).unwrap_or(0);
            let old = c.mem.read_u32(c.args_ptr + 8).unwrap_or(0);
            let bytes = c.mem.read_u32(c.args_ptr + 12).unwrap_or(0);
            if old == 0 {
                // ReAlloc of NULL behaves as Alloc.
                return lock(&heap)
                    .alloc(c.mem, h_heap, flags & !heap::HEAP_REALLOC_IN_PLACE_ONLY, bytes)
                    .unwrap_or(0) as i32;
            }
            let mut hs = lock(&heap);
            let Some(have) = hs.size(c.mem, h_heap, old) else {
                return 0;
            };
            if have >= bytes {
                return old as i32;
            }
            if flags & heap::HEAP_REALLOC_IN_PLACE_ONLY != 0 {
                return 0;
            }
            let new = hs
                .alloc(c.mem, h_heap, flags & !heap::HEAP_REALLOC_IN_PLACE_ONLY, bytes)
                .unwrap_or(0);
            if new == 0 {
                return 0;
            }
            let copy = have.min(bytes) as usize;
            // Copy through a temporary: `get` and `get_mut` cannot both borrow
            // the same memory at once.
            match c.mem.get(old, copy) {
                Some(src) => {
                    let tmp = src.to_vec();
                    if let Some(dst) = c.mem.get_mut(new, copy) {
                        dst.copy_from_slice(&tmp);
                    }
                }
                None => return 0,
            }
            hs.free(c.mem, h_heap, old);
            new as i32
        });
    }

    // `HeapSize(hHeap, dwFlags, lpMem) -> SIZE_T`, (SIZE_T)-1 on failure.
    {
        let heap = std::sync::Arc::clone(&heap_shared);
        h.implement_with_memory(api("HeapSize"), move |c: Call<'_>| {
            let h_heap = c.mem.read_u32(c.args_ptr).unwrap_or(0);
            let ptr = c.mem.read_u32(c.args_ptr + 8).unwrap_or(0);
            match lock(&heap).size(c.mem, h_heap, ptr) {
                Some(n) => n as i32,
                None => -1, // (SIZE_T)-1
            }
        });
    }

    // `HeapDestroy(hHeap) -> BOOL`: nonzero on success.
    {
        let heap = std::sync::Arc::clone(&heap_shared);
        h.implement_with_memory(api("HeapDestroy"), move |c: Call<'_>| {
            let h_heap = c.mem.read_u32(c.args_ptr).unwrap_or(0);
            if lock(&heap).destroy(h_heap) {
                1
            } else {
                0
            }
        });
    }

    h
}

#[cfg(test)]
mod tests {
    use super::*;

    fn api(name: &str) -> Api {
        Api::new("KERNEL32.dll", name)
    }

    #[test]
    fn an_undeclared_call_is_refused_and_recorded() {
        let mut h = Host::new();
        let err = h.call(&api("OpenFile"), 0x1000).unwrap_err();
        assert_eq!(err.refusal, Refusal::Unimplemented);
        assert_eq!(err.args_ptr, 0x1000);
        assert_eq!(h.diagnostics().len(), 1);
        assert_eq!(h.traps(), 1);
    }

    #[test]
    fn declaring_does_not_implement() {
        let mut h = Host::new();
        h.declare(api("CreateWindowExA"));
        // Declared, but still refuses: the cranky default.
        assert!(h.call(&api("CreateWindowExA"), 0).is_err());
        assert_eq!(h.unimplemented(), vec![&api("CreateWindowExA")]);
        assert!(h.implemented().is_empty());
    }

    #[test]
    fn implemented_calls_return_their_value() {
        let mut h = Host::new();
        h.implement(api("GetVersion"), |_| 0x0A28_0000);
        assert_eq!(h.call(&api("GetVersion"), 0), Ok(0x0A28_0000));
        assert_eq!(h.implemented(), vec![&api("GetVersion")]);
        assert!(h.diagnostics().is_empty());
    }

    #[test]
    fn out_of_scope_records_the_reason_not_a_value() {
        let mut h = Host::new();
        h.out_of_scope(api("WSAStartup"), "winsock is not part of V1");
        let err = h.call(&api("WSAStartup"), 0).unwrap_err();
        match err.refusal {
            Refusal::OutOfScope(why) => assert!(why.contains("winsock"), "{why}"),
            other => panic!("expected OutOfScope, got {other:?}"),
        }
    }

    #[test]
    fn refusal_policy_trap_is_the_default() {
        let h = Host::new();
        assert_eq!(h.on_refuse, OnRefuse::Trap);
    }

    #[test]
    #[should_panic(expected = "wlam host refused")]
    fn resolve_traps_by_default() {
        let mut h = Host::new();
        h.resolve(&api("Nope"), 0);
    }

    #[test]
    fn resolve_can_return_a_probe_value_when_asked() {
        let mut h = Host::new();
        h.on_refuse(OnRefuse::Return(-1));
        assert_eq!(h.resolve(&api("Nope"), 0), -1);
        // The refusal is still recorded: probing does not erase the truth.
        assert_eq!(h.diagnostics().len(), 1);
    }

    #[test]
    fn call_counts_track_only_what_was_reached() {
        let mut h = Host::new();
        h.implement(api("GetVersion"), |_| 1);
        h.implement(api("GetTickCount"), |_| 2);
        h.call(&api("GetVersion"), 0).unwrap();
        h.call(&api("GetVersion"), 0).unwrap();
        let counts = h.call_counts();
        assert_eq!(counts[&api("GetVersion")], 2);
        assert_eq!(counts[&api("GetTickCount")], 0);
    }

    // -- GetVersion -------------------------------------------------------

    #[test]
    fn get_version_packs_major_minor_and_build_the_way_the_guest_unpacks_them() {
        let mut h = Host::new();
        kernel32(&mut h);
        let v = h.call(&api("GetVersion"), 0).expect("implemented") as u32;

        // These are the exact extractions Uxn32 performs at 0x40ccde:
        //   major = (v >> 8) & 0xff, minor = v & 0xff
        let major = (v >> 8) & 0xff;
        let minor = v & 0xff;
        assert_eq!(major, HOST_VERSION.major as u32);
        assert_eq!(minor, HOST_VERSION.minor as u32);
        assert_eq!(v >> 16, HOST_VERSION.build as u32);
    }

    #[test]
    fn reporting_nt_is_what_the_guest_tests_for() {
        // Uxn32 derives its platform tag with `major > 4`, so the value is not
        // decorative: 5.0 is the oldest version that makes it take the NT path.
        assert!(HOST_VERSION.major > 4, "the host must present as NT-platform");
        assert_eq!(HOST_VERSION.platform, Platform::Win32Nt);
        // And the NT bit is clear, which is the other way callers tell NT from 9x.
        assert_eq!(HOST_VERSION.version_dword() & 0x8000_0000, 0);
    }

    #[test]
    fn the_9x_bit_is_set_for_the_windows_platform() {
        let v = WindowsVersion {
            major: 4,
            minor: 10,
            platform: Platform::Win32Windows,
            build: 0,
        };
        assert_eq!(v.version_dword() & 0x8000_0000, 0x8000_0000);
        assert_eq!(v.version_dword() & 0x7fff_ffff, 0x0000_040a);
    }

    #[test]
    fn get_version_is_not_implemented_by_declaration_alone() {
        let mut h = Host::new();
        h.declare(api("GetVersion"));
        assert!(
            h.call(&api("GetVersion"), 0).is_err(),
            "declaring must not implement"
        );
    }

    // -- guest memory -----------------------------------------------------

    #[test]
    fn guest_memory_reads_and_writes_within_bounds_only() {
        let mut data = vec![0u8; 0x100];
        data[0x10..0x14].copy_from_slice(&0xdead_beefu32.to_le_bytes());
        data[0x20..0x23].copy_from_slice(b"abc");
        let mut mem = GuestMemory::new(&mut data);
        assert_eq!(mem.read_u32(0x10), Some(0xdead_beef));
        assert_eq!(mem.read_cstr(0x20), Some(b"abc".to_vec()));
        // Out of bounds: reported, never a plausible zero.
        assert_eq!(mem.read_u32(0xffff_fff0), None);
        assert_eq!(mem.read_cstr(0x1000), None);
        assert!(mem.write_u32(0x10, 7));
        assert!(!mem.write_u32(0x1_0000, 7));
        assert_eq!(mem.read_u32(0x10), Some(7));
    }

    // -- heap APIs through the host dispatch path -------------------------

    /// Build a host served by `kernel32`, with a guest memory big enough to
    /// hold the heap region, and push arguments for the next call.
    fn heap_host() -> (Host, Vec<u8>) {
        let mut h = Host::new();
        kernel32(&mut h);
        // A small heap set placed in a modest buffer: the real set spans the
        // guest's whole 3.75 GiB upper address space, which no test should
        // allocate just to exercise the API surface.
        let end = heap::HEAP_REGION_BASE as usize + 0x10_0000;
        (h, vec![0u8; end])
    }

    /// Lay out arguments at `args_ptr` exactly as a cdecl caller would: the
    /// first argument lowest.
    fn args(data: &mut [u8], at: u32, values: &[u32]) {
        for (i, v) in values.iter().enumerate() {
            let o = at as usize + i * 4;
            data[o..o + 4].copy_from_slice(&v.to_le_bytes());
        }
    }

    fn word(data: &[u8], at: u32) -> u32 {
        let o = at as usize;
        u32::from_le_bytes(data[o..o + 4].try_into().unwrap())
    }

    #[test]
    fn heap_create_through_the_host_returns_a_real_handle() {
        let (mut h, mut data) = heap_host();
        let mut mem = GuestMemory::new(&mut data);
        args(mem.data, 0x1000, &[0, 0x1000, 0]);
        let handle = h.call_with_memory(&api("HeapCreate"), 0x1000, &mut mem);
        let handle = handle.expect("HeapCreate is implemented") as u32;
        // The guest calls this first thing and branches on zero.
        assert_ne!(handle, 0, "a successful HeapCreate must not return NULL");
        assert_ne!(handle, 1, "the handle must not look like an index");
    }

    #[test]
    fn heap_alloc_through_the_host_returns_usable_guest_memory() {
        let (mut h, mut data) = heap_host();
        let mut mem = GuestMemory::new(&mut data);
        args(mem.data, 0x1000, &[0, 0x1000, 0]);
        let handle = h.call_with_memory(&api("HeapCreate"), 0x1000, &mut mem).unwrap() as u32;

        // HeapAlloc(hHeap, flags, bytes)
        args(mem.data, 0x2000, &[handle, 0, 64]);
        let p = h
            .call_with_memory(&api("HeapAlloc"), 0x2000, &mut mem)
            .expect("HeapAlloc is implemented") as u32;
        assert_ne!(p, 0, "a small allocation from a fresh heap must succeed");

        // The whole point: the guest can write through it.
        mem.write_u32(p, 0xFEED_FACE);
        assert_eq!(mem.read_u32(p), Some(0xFEED_FACE));
    }

    #[test]
    fn heap_alloc_and_free_round_trip_through_the_host() {
        let (mut h, mut data) = heap_host();
        let mut mem = GuestMemory::new(&mut data);
        args(mem.data, 0x1000, &[0, 0x1000, 0]);
        let handle = h.call_with_memory(&api("HeapCreate"), 0x1000, &mut mem).unwrap() as u32;

        args(mem.data, 0x2000, &[handle, 0, 128]);
        let p = h.call_with_memory(&api("HeapAlloc"), 0x2000, &mut mem).unwrap() as u32;

        // HeapSize(hHeap, flags, ptr) reports the usable size.
        args(mem.data, 0x3000, &[handle, 0, p]);
        let size = h.call_with_memory(&api("HeapSize"), 0x3000, &mut mem).unwrap();
        assert!(size >= 128, "HeapSize must report at least what was asked for");

        // HeapFree(hHeap, flags, ptr) -> TRUE
        args(mem.data, 0x4000, &[handle, 0, p]);
        let freed = h.call_with_memory(&api("HeapFree"), 0x4000, &mut mem).unwrap();
        assert_eq!(freed, 1, "HeapFree must report success");

        // A double free is refused rather than silently accepted.
        let again = h.call_with_memory(&api("HeapFree"), 0x4000, &mut mem).unwrap();
        assert_eq!(again, 0, "freeing the same block twice must fail");
    }

    #[test]
    fn heap_apis_refuse_an_unknown_handle_without_inventing_a_pointer() {
        let (mut h, mut data) = heap_host();
        let mut mem = GuestMemory::new(&mut data);
        args(mem.data, 0x2000, &[0xDEAD_BEEF, 0, 64]);
        let p = h.call_with_memory(&api("HeapAlloc"), 0x2000, &mut mem).unwrap();
        assert_eq!(p, 0, "an unknown heap must yield NULL, not a pointer");

        args(mem.data, 0x3000, &[0xDEAD_BEEF, 0, 0x1000]);
        let size = h.call_with_memory(&api("HeapSize"), 0x3000, &mut mem).unwrap();
        assert_eq!(size, -1, "HeapSize reports (SIZE_T)-1 for a bad handle");
    }

    #[test]
    fn get_process_heap_is_available_without_heap_create() {
        let (mut h, mut data) = heap_host();
        let mut mem = GuestMemory::new(&mut data);
        let handle = h.call_with_memory(&api("GetProcessHeap"), 0, &mut mem).unwrap() as u32;
        assert_ne!(handle, 0);
        args(mem.data, 0x2000, &[handle, 0, 32]);
        let p = h.call_with_memory(&api("HeapAlloc"), 0x2000, &mut mem).unwrap() as u32;
        assert_ne!(p, 0, "the process heap must be usable");
    }

    #[test]
    fn heap_realloc_preserves_the_contents_it_keeps() {
        let (mut h, mut data) = heap_host();
        let mut mem = GuestMemory::new(&mut data);
        args(mem.data, 0x1000, &[0, 0x1000, 0]);
        let handle = h.call_with_memory(&api("HeapCreate"), 0x1000, &mut mem).unwrap() as u32;
        args(mem.data, 0x2000, &[handle, 0, 16]);
        let p = h.call_with_memory(&api("HeapAlloc"), 0x2000, &mut mem).unwrap() as u32;
        for (i, b) in b"ABCDEFGHIJKLMNOP".iter().enumerate() {
            mem.get_mut(p + i as u32, 1).unwrap()[0] = *b;
        }

        // HeapReAlloc(hHeap, flags, lpMem, dwBytes) -- grows, so it may move.
        args(mem.data, 0x3000, &[handle, 0, p, 256]);
        let q = h.call_with_memory(&api("HeapReAlloc"), 0x3000, &mut mem).unwrap() as u32;
        assert_ne!(q, 0, "growing a small block must succeed");
        assert_eq!(
            mem.get(q, 16),
            Some(&b"ABCDEFGHIJKLMNOP"[..]),
            "the first 16 bytes must survive the move"
        );
    }

    // -- startup APIs through the host dispatch path ----------------------

    #[test]
    fn get_startup_info_a_fills_a_struct_the_guest_can_branch_on() {
        let (mut h, mut data) = heap_host();
        let mut mem = GuestMemory::new(&mut data);
        // GetStartupInfoA(lpStartupInfo): the caller sets cb first.
        let si = 0x2000u32;
        args(mem.data, si, &[startup::STARTUPINFOA_SIZE]);
        let _ = h.call_with_memory(&api("GetStartupInfoA"), 0x1000, &mut mem);
        // Uxn32 reads `dwFlags & STARTF_USESHOWWINDOW` and, if clear, takes its
        // default; reporting no flags must leave that bit clear.
        let flags = word(mem.data, si + startup::offset::DW_FLAGS);
        assert_eq!(flags & startup::STARTF_USESHOWWINDOW, 0);
        assert_eq!(word(mem.data, si + startup::offset::CB), startup::STARTUPINFOA_SIZE);
    }

    #[test]
    fn set_handle_count_echoes_what_was_asked_for() {
        let (mut h, mut data) = heap_host();
        let mut mem = GuestMemory::new(&mut data);
        args(mem.data, 0x1000, &[20]);
        let n = h
            .call_with_memory(&api("SetHandleCount"), 0x1000, &mut mem)
            .unwrap();
        // Not 0: a zero answer claims a handle limit the host does not impose.
        assert_eq!(n, 20);
        args(mem.data, 0x1000, &[7]);
        let n = h
            .call_with_memory(&api("SetHandleCount"), 0x1000, &mut mem)
            .unwrap();
        assert_eq!(n, 7, "the answer must follow the request, not be a constant");
    }

    #[test]
    fn an_out_of_bounds_write_is_reported_rather_than_silently_dropped() {
        let mut data = vec![0u8; 0x10];
        let mut mem = GuestMemory::new(&mut data);
        let mut h = Host::new();
        h.implement_with_memory(api("WriteOut"), |c: Call<'_>| {
            // A host that reported success without writing would leave the
            // guest reading whatever was there before, which is the failure
            // mode this whole layer exists to prevent.
            if c.mem.write_u32(c.args_ptr, 0x1234) {
                1 // TRUE
            } else {
                0 // FALSE, as the real API does for a bad pointer
            }
        });
        assert_eq!(h.call_with_memory(&api("WriteOut"), 0x8, &mut mem), Ok(1));
        assert_eq!(
            h.call_with_memory(&api("WriteOut"), 0x0_f, &mut mem),
            Ok(0),
            "a u32 write straddling the end of a 16-byte memory is a bad pointer"
        );
        assert_eq!(GuestMemory::new(&mut []).read_u32(0), None);
    }
}
