//! Standard handles and the console APIs that use them.
//!
//! ## Why this is on the critical path
//!
//! Hover initialises its three standard handles from `STARTUPINFOA`. When a
//! handle is `INVALID_HANDLE_VALUE` it falls back to:
//!
//! ```text
//!   push  -10                 ; STD_INPUT_HANDLE
//!   call [GetStdHandle]
//!   mov   [esi], eax          ; store the handle
//!   cmp   eax, -1
//!   je    no_console
//!   push  eax
//!   call [GetFileType]
//!   and   eax, 0xff
//!   cmp   eax, 2              ; FILE_TYPE_CHAR
//!   je    is_console
//! ```
//!
//! So `GetStdHandle` must return a handle that is neither `NULL` nor
//! `INVALID_HANDLE_VALUE`, and `GetFileType` must report what that handle
//! actually is. Returning a plausible constant from either would send the game
//! down a console path that cannot work, or hide the console path entirely.
//!
//! ## What the handles are
//!
//! They are opaque host handles from [`crate::objects::Handles`], in their own
//! namespace, so they can never be confused with a window (`namespace 1`), a
//! GDI object (`namespace 2`), or a guest pointer. The three streams exist for
//! the lifetime of the process; `SetStdHandle` may redirect a slot, and the
//! redirected value is returned verbatim (it may be a file handle the host does
//! not own).
//!
//! Output written to a standard handle is captured in [`Stdio::output`] rather
//! than discarded. The browser and non-interactive runners have no terminal, so
//! dropping it would silently lose the guest's only report of why it failed;
//! keeping the bytes lets a caller surface them and lets a test assert that the
//! write really happened.

use crate::objects::Handles;

/// `STD_INPUT_HANDLE`: `(DWORD)-10`.
pub const STD_INPUT_HANDLE: u32 = 0xffff_fff6;
/// `STD_OUTPUT_HANDLE`: `(DWORD)-11`.
pub const STD_OUTPUT_HANDLE: u32 = 0xffff_fff5;
/// `STD_ERROR_HANDLE`: `(DWORD)-12`.
pub const STD_ERROR_HANDLE: u32 = 0xffff_fff4;

/// `INVALID_HANDLE_VALUE`.
pub const INVALID_HANDLE_VALUE: u32 = 0xffff_ffff;

/// `FILE_TYPE_UNKNOWN`.
pub const FILE_TYPE_UNKNOWN: u32 = 0x0000;
/// `FILE_TYPE_DISK`.
pub const FILE_TYPE_DISK: u32 = 0x0001;
/// `FILE_TYPE_CHAR` (the console).
pub const FILE_TYPE_CHAR: u32 = 0x0002;
/// `FILE_TYPE_PIPE`.
pub const FILE_TYPE_PIPE: u32 = 0x0003;

/// A standard stream the host owns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stream {
    Input,
    Output,
    Error,
}

/// The three standard handles and the bytes written through them.
#[derive(Debug)]
pub struct Stdio {
    handles: Handles<Stream>,
    standard: [u32; 3],
    output: Vec<u8>,
    input: Vec<u8>,
    input_read: usize,
}

impl Default for Stdio {
    fn default() -> Self {
        // Namespace 3 is reserved for standard handles: windows use 1 and GDI
        // objects use 2, so a handle leaking between tables is detectable.
        let mut handles = Handles::with_namespace(3);
        // Three slots cannot fail to insert; if they ever did, a process with no
        // standard handles would be a bug worth stopping on.
        let input = handles.insert(Stream::Input).expect("stdin handle");
        let output = handles.insert(Stream::Output).expect("stdout handle");
        let error = handles.insert(Stream::Error).expect("stderr handle");
        Stdio {
            handles,
            standard: [input, output, error],
            output: Vec::new(),
            input: Vec::new(),
            input_read: 0,
        }
    }
}

impl Stdio {
    /// The handle `GetStdHandle` returns for `nStdHandle`.
    ///
    /// Any other index is a caller bug and yields `INVALID_HANDLE_VALUE`, which
    /// is exactly what Windows does.
    pub fn get_std_handle(&self, n_std_handle: u32) -> u32 {
        match n_std_handle {
            STD_INPUT_HANDLE => self.standard[0],
            STD_OUTPUT_HANDLE => self.standard[1],
            STD_ERROR_HANDLE => self.standard[2],
            _ => INVALID_HANDLE_VALUE,
        }
    }

    /// `SetStdHandle`: redirect one standard slot. Returns `false` for an
    /// unknown index; the slot is left unchanged.
    pub fn set_std_handle(&mut self, n_std_handle: u32, handle: u32) -> bool {
        let slot = match n_std_handle {
            STD_INPUT_HANDLE => 0,
            STD_OUTPUT_HANDLE => 1,
            STD_ERROR_HANDLE => 2,
            _ => return false,
        };
        self.standard[slot] = handle;
        true
    }

    /// `GetFileType`.
    ///
    /// `NULL` and `INVALID_HANDLE_VALUE` are `FILE_TYPE_UNKNOWN`, as are handles
    /// this host never issued. A redirected standard slot holding somebody
    /// else's handle is also unknown: claiming to know its type would be a
    /// guess, and the caller can only act on a truthful answer.
    pub fn file_type(&self, handle: u32) -> u32 {
        if handle == 0 || handle == INVALID_HANDLE_VALUE {
            return FILE_TYPE_UNKNOWN;
        }
        match self.handles.get(handle) {
            Some(Stream::Input | Stream::Output | Stream::Error) => FILE_TYPE_CHAR,
            None => FILE_TYPE_UNKNOWN,
        }
    }

    /// Write bytes to a standard handle.
    ///
    /// Returns `true` when the handle is one of the host's output streams and
    /// the bytes were captured. Reading from or writing to the input stream is
    /// a failure, as it is in Windows.
    pub fn write(&mut self, handle: u32, bytes: &[u8]) -> bool {
        match self.handles.get(handle) {
            Some(Stream::Output | Stream::Error) => {
                self.output.extend_from_slice(bytes);
                true
            }
            _ => false,
        }
    }

    /// Bytes written to stdout/stderr so far, in order.
    pub fn output(&self) -> &[u8] {
        &self.output
    }

    /// Queue bytes for a subsequent `ReadFile` on stdin.
    ///
    /// Console input in a browser has no blocking source, so the runner feeds
    /// it explicitly. Empty input reads as zero bytes, not as fabricated text.
    pub fn push_input(&mut self, bytes: &[u8]) {
        self.input.extend_from_slice(bytes);
    }

    /// Read at most `max` bytes from stdin, advancing the cursor.
    ///
    /// Returns `None` for a handle that is not the input stream. An exhausted
    /// queue yields an empty slice (end of input).
    pub fn read(&mut self, handle: u32, max: usize) -> Option<Vec<u8>> {
        match self.handles.get(handle) {
            Some(Stream::Input) => {
                let available = &self.input[self.input_read..];
                let n = max.min(available.len());
                let out = available[..n].to_vec();
                self.input_read += n;
                Some(out)
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_standard_index_has_its_own_handle() {
        let stdio = Stdio::default();
        let handles = [
            stdio.get_std_handle(STD_INPUT_HANDLE),
            stdio.get_std_handle(STD_OUTPUT_HANDLE),
            stdio.get_std_handle(STD_ERROR_HANDLE),
        ];
        for handle in handles {
            assert_ne!(handle, 0, "NULL is the failure value");
            assert_ne!(handle, INVALID_HANDLE_VALUE);
            assert_eq!(stdio.file_type(handle), FILE_TYPE_CHAR);
        }
        assert_ne!(handles[0], handles[1]);
        assert_ne!(handles[1], handles[2]);
        assert_ne!(handles[0], handles[2]);
    }

    #[test]
    fn an_unknown_standard_index_is_invalid_and_refused() {
        let mut stdio = Stdio::default();
        assert_eq!(stdio.get_std_handle(7), INVALID_HANDLE_VALUE);
        assert!(!stdio.set_std_handle(7, 1));
    }

    #[test]
    fn set_std_handle_redirects_only_that_slot() {
        let mut stdio = Stdio::default();
        let out = stdio.get_std_handle(STD_OUTPUT_HANDLE);
        assert!(stdio.set_std_handle(STD_INPUT_HANDLE, 0x1234));
        assert_eq!(stdio.get_std_handle(STD_INPUT_HANDLE), 0x1234);
        assert_eq!(stdio.get_std_handle(STD_OUTPUT_HANDLE), out);
        // A foreign handle is returned but its type is not invented.
        assert_eq!(stdio.file_type(0x1234), FILE_TYPE_UNKNOWN);
    }

    #[test]
    fn writing_output_is_captured_and_input_refuses_it() {
        let mut stdio = Stdio::default();
        let out = stdio.get_std_handle(STD_OUTPUT_HANDLE);
        let err = stdio.get_std_handle(STD_ERROR_HANDLE);
        let inp = stdio.get_std_handle(STD_INPUT_HANDLE);
        assert!(stdio.write(out, b"hello "));
        assert!(stdio.write(err, b"world"));
        assert!(!stdio.write(inp, b"nope"));
        assert_eq!(stdio.output(), b"hello world");
    }

    #[test]
    fn input_is_drained_without_inventing_data() {
        let mut stdio = Stdio::default();
        let inp = stdio.get_std_handle(STD_INPUT_HANDLE);
        assert_eq!(stdio.read(inp, 4), Some(Vec::new()));
        stdio.push_input(b"abcdef");
        assert_eq!(stdio.read(inp, 4), Some(b"abcd".to_vec()));
        assert_eq!(stdio.read(inp, 4), Some(b"ef".to_vec()));
        assert_eq!(stdio.read(inp, 4), Some(Vec::new()));
    }
}
