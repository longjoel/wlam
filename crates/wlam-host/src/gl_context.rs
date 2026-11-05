//! Window pixel formats and the single-thread WGL context lifecycle.
//!
//! This service deliberately owns no GL drawing state. The renderer can query
//! [`GlContexts::current`] after guest calls and require a current context
//! before executing OpenGL commands.

use crate::objects::Handles;
use std::collections::BTreeMap;

pub const PIXEL_FORMAT: u32 = 1;
const PFD_DOUBLEBUFFER: u32 = 0x0000_0001;
const PFD_DRAW_TO_WINDOW: u32 = 0x0000_0004;
const PFD_DRAW_TO_BITMAP: u32 = 0x0000_0008;
const PFD_SUPPORT_GDI: u32 = 0x0000_0010;
const PFD_SUPPORT_OPENGL: u32 = 0x0000_0020;
const PFD_TYPE_RGBA: u8 = 0;
const PFD_TYPE_COLORINDEX: u8 = 1;
const PFD_MAIN_PLANE: i8 = 0;
pub const PFD_SIZE: usize = 40;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PixelFormatDescriptor {
    pub flags: u32,
    pub pixel_type: u8,
    pub color_bits: u8,
    pub alpha_bits: u8,
    pub depth_bits: u8,
    pub stencil_bits: u8,
    pub layer_type: i8,
}

impl PixelFormatDescriptor {
    /// Descriptor for the browser's sole window format: RGBA, double buffered,
    /// with depth and stencil attachments. This is the advertised virtual
    /// format; it does not promise native GPU-specific attributes.
    pub const fn rgba_double_buffered() -> Self {
        Self {
            flags: PFD_DOUBLEBUFFER | PFD_DRAW_TO_WINDOW | PFD_SUPPORT_OPENGL,
            pixel_type: PFD_TYPE_RGBA,
            color_bits: 32,
            alpha_bits: 8,
            depth_bits: 24,
            stencil_bits: 8,
            layer_type: PFD_MAIN_PLANE,
        }
    }

    pub fn decode(bytes: &[u8; PFD_SIZE]) -> Option<Self> {
        let size = u16::from_le_bytes([bytes[0], bytes[1]]);
        let version = u16::from_le_bytes([bytes[2], bytes[3]]);
        if size < PFD_SIZE as u16 || version != 1 {
            return None;
        }
        Some(Self {
            flags: u32::from_le_bytes(bytes[4..8].try_into().ok()?),
            pixel_type: bytes[8],
            color_bits: bytes[9],
            alpha_bits: bytes[16],
            depth_bits: bytes[23],
            stencil_bits: bytes[24],
            layer_type: bytes[26] as i8,
        })
    }

    pub fn encode(self) -> [u8; PFD_SIZE] {
        let mut bytes = [0u8; PFD_SIZE];
        bytes[0..2].copy_from_slice(&(PFD_SIZE as u16).to_le_bytes());
        bytes[2..4].copy_from_slice(&1u16.to_le_bytes());
        bytes[4..8].copy_from_slice(&self.flags.to_le_bytes());
        bytes[8] = self.pixel_type;
        bytes[9] = self.color_bits;
        bytes[10] = 8; // cRedBits
        bytes[11] = 0; // cRedShift
        bytes[12] = 8; // cGreenBits
        bytes[13] = 8; // cGreenShift
        bytes[14] = 8; // cBlueBits
        bytes[15] = 16; // cBlueShift
        bytes[16] = self.alpha_bits;
        bytes[17] = 24; // cAlphaShift
        bytes[23] = self.depth_bits;
        bytes[24] = self.stencil_bits;
        bytes[26] = self.layer_type as u8;
        bytes
    }

    fn supports_request(self, requested: Self) -> bool {
        let flags_ok = requested.flags & PFD_DRAW_TO_BITMAP == 0
            && requested.flags & PFD_DRAW_TO_WINDOW != 0
            && requested.flags & PFD_SUPPORT_OPENGL != 0;
        // PFD_SUPPORT_GDI is not advertised by this WebGL-backed format. The
        // remaining optional swap/palette/acceleration flags are not promises
        // the browser backend can make, so reject requests that require them.
        let unsupported = requested.flags
            & (PFD_SUPPORT_GDI | 0x0000_0040 | 0x0000_0080 | 0x0000_0100 | 0x0000_0200);
        flags_ok
            && unsupported == 0
            && requested.pixel_type == self.pixel_type
            && requested.layer_type == self.layer_type
            && requested.color_bits <= self.color_bits
            && requested.alpha_bits <= self.alpha_bits
            && requested.depth_bits <= self.depth_bits
            && requested.stencil_bits <= self.stencil_bits
    }
}

#[derive(Debug)]
struct Context {
    pixel_format: u32,
}

#[derive(Debug)]
pub struct GlContexts {
    pixel_formats: BTreeMap<u32, u32>, // HWND -> one-time pixel-format index
    contexts: Handles<Context>,
    current: Option<(u32, u32, u32)>, // (HGLRC, HDC, HWND)
    swap_pending: Option<(u32, u32, u32)>,
}

impl Default for GlContexts {
    fn default() -> Self {
        Self {
            pixel_formats: BTreeMap::new(),
            contexts: Handles::with_namespace(12),
            current: None,
            swap_pending: None,
        }
    }
}

impl GlContexts {
    pub fn pixel_format_count(&self) -> u32 {
        1
    }

    pub fn descriptor(&self, index: u32) -> Option<PixelFormatDescriptor> {
        (index == PIXEL_FORMAT).then(PixelFormatDescriptor::rgba_double_buffered)
    }

    pub fn choose_pixel_format(&self, request: PixelFormatDescriptor) -> u32 {
        self.descriptor(PIXEL_FORMAT)
            .filter(|available| available.supports_request(request))
            .map_or(0, |_| PIXEL_FORMAT)
    }

    pub fn set_pixel_format(&mut self, hwnd: u32, index: u32) -> bool {
        if hwnd == 0 || index != PIXEL_FORMAT || self.pixel_formats.contains_key(&hwnd) {
            return false;
        }
        self.pixel_formats.insert(hwnd, index);
        true
    }

    pub fn pixel_format(&self, hwnd: u32) -> u32 {
        self.pixel_formats.get(&hwnd).copied().unwrap_or(0)
    }

    pub fn window_destroyed(&mut self, hwnd: u32) {
        self.pixel_formats.remove(&hwnd);
        if self
            .current
            .is_some_and(|(_, _, current_hwnd)| current_hwnd == hwnd)
        {
            self.current = None;
        }
        if self
            .swap_pending
            .is_some_and(|(_, _, pending_hwnd)| pending_hwnd == hwnd)
        {
            self.swap_pending = None;
        }
    }

    pub fn create_context(&mut self, pixel_format: u32) -> u32 {
        if pixel_format != PIXEL_FORMAT {
            return 0;
        }
        self.contexts.insert(Context { pixel_format }).unwrap_or(0)
    }

    /// Bind a context to a window DC. A null context unbinds the current
    /// context; WGL ignores the DC argument in that case. The current thread
    /// model is single-threaded, so ownership is represented by this tuple.
    pub fn make_current(&mut self, hdc: u32, hwnd: u32, hglrc: u32) -> bool {
        if hglrc == 0 {
            self.current = None;
            return true;
        }
        if hdc == 0 || hwnd == 0 {
            return false;
        }
        let Some(&format) = self.pixel_formats.get(&hwnd) else {
            return false;
        };
        let Some(context) = self.contexts.get(hglrc) else {
            return false;
        };
        if context.pixel_format != format {
            return false;
        }
        self.current = Some((hglrc, hdc, hwnd));
        true
    }

    /// Deleting a context current on this single guest thread first unbinds it.
    pub fn delete_context(&mut self, hglrc: u32) -> bool {
        if self.current.is_some_and(|(current, _, _)| current == hglrc) {
            self.current = None;
        }
        self.contexts.remove(hglrc).is_some()
    }

    pub fn current(&self) -> Option<(u32, u32, u32)> {
        self.current
    }

    /// Return the currently bound tuple, or a diagnostic suitable for a GL
    /// command made without a valid current window/context.
    pub fn require_current(&self) -> Result<(u32, u32, u32), &'static str> {
        let tuple = self
            .current
            .ok_or("OpenGL call without a current context")?;
        if self.contexts.get(tuple.0).is_none() || self.pixel_format(tuple.2) == 0 {
            return Err("OpenGL current context is no longer valid");
        }
        Ok(tuple)
    }

    /// Record a successful swap boundary for the renderer/presenter.
    pub fn swap_buffers(&mut self, hdc: u32, hwnd: u32) -> bool {
        let Some((hglrc, current_dc, current_window)) = self.current else {
            return false;
        };
        if current_dc != hdc || current_window != hwnd || self.pixel_format(hwnd) == 0 {
            return false;
        }
        self.swap_pending = Some((hglrc, hdc, hwnd));
        true
    }

    /// Consume the most recent successful SwapBuffers notification.
    pub fn take_swap(&mut self) -> Option<(u32, u32, u32)> {
        self.swap_pending.take()
    }
}

/// Whether a supplied pixel-format descriptor is valid for the virtual format.
pub fn accepts_descriptor(request: PixelFormatDescriptor) -> bool {
    PixelFormatDescriptor::rgba_double_buffered().supports_request(request)
}

/// PFD_TYPE_COLORINDEX is exposed for callers to reject explicitly instead of
/// accidentally treating its pixel values as RGBA.
pub const fn is_color_index(pixel_type: u8) -> bool {
    pixel_type == PFD_TYPE_COLORINDEX
}
