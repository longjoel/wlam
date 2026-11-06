//! Shared software GDI objects and window surfaces.
use crate::{
    memory::MemoryAccess,
    objects::Handles,
    raster::{Dib, RasterError},
};
use std::collections::BTreeMap;
pub const SRCCOPY: u32 = 0x00cc0020;
#[derive(Debug, PartialEq, Eq)]
pub enum Error {
    InvalidHandle,
    WrongType,
    SelectedObject,
    InvalidDimensions,
    UnsupportedOperation,
    Exhausted,
    Raster(RasterError),
}
impl From<RasterError> for Error {
    fn from(e: RasterError) -> Self {
        Self::Raster(e)
    }
}
#[derive(Debug)]
pub struct Surface {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
    pub dirty: bool,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}
/// Signed rectangles walk from their leading edge. Negative spans start one
/// pixel before the supplied edge. Widen endpoint arithmetic before narrowing.
#[derive(Debug)]
struct StretchAxis {
    destination_origin: i32,
    destination_step: i32,
    destination_count: u32,
    destination_min: i32,
    destination_max: i32,
    source_origin: i32,
    source_step: i32,
    source_count: u32,
    source_min: i32,
    source_max: i32,
}
impl StretchAxis {
    fn new(destination: i32, count: i32, source: i32, source_count: i32) -> Result<Self, Error> {
        if count == 0 || source_count == 0 || count == i32::MIN || source_count == i32::MIN {
            return Err(Error::InvalidDimensions);
        }
        let destination = if count < 0 {
            destination.checked_sub(1).ok_or(Error::InvalidDimensions)?
        } else {
            destination
        };
        let source = if source_count < 0 {
            source.checked_sub(1).ok_or(Error::InvalidDimensions)?
        } else {
            source
        };
        let bounds = |origin: i32, count: i32| -> Result<(i32, i32), Error> {
            let end = origin as i64 + count as i64 - count.signum() as i64;
            let low = (origin as i64).min(end);
            let high = (origin as i64).max(end) + 1;
            Ok((
                low.try_into().map_err(|_| Error::InvalidDimensions)?,
                high.try_into().map_err(|_| Error::InvalidDimensions)?,
            ))
        };
        let (destination_min, destination_max) = bounds(destination, count)?;
        let (source_min, source_max) = bounds(source, source_count)?;
        Ok(Self {
            destination_origin: destination,
            destination_step: count.signum(),
            destination_count: count.unsigned_abs(),
            destination_min,
            destination_max,
            source_origin: source,
            source_step: source_count.signum(),
            source_count: source_count.unsigned_abs(),
            source_min,
            source_max,
        })
    }
    fn destination_index(&self, coordinate: i32) -> u32 {
        ((coordinate as i64 - self.destination_origin as i64) * self.destination_step as i64) as u32
    }
    fn source_coordinate(&self, index: u32) -> i32 {
        (self.source_origin as i64 + index as i64 * self.source_step as i64) as i32
    }
    fn nearest(&self, index: u32) -> usize {
        (((2 * index as u64 + 1) * self.source_count as u64) / (2 * self.destination_count as u64))
            .min(self.source_count as u64 - 1) as usize
    }
    fn reduction_range(&self, index: u32) -> std::ops::Range<usize> {
        if self.source_count <= self.destination_count {
            let at = self.nearest(index);
            return at..at + 1;
        }
        let start = index as u64 * self.source_count as u64 / self.destination_count as u64;
        let end = (index as u64 + 1) * self.source_count as u64 / self.destination_count as u64;
        start as usize..end as usize
    }
    fn coverage(&self, index: u32) -> std::ops::Range<usize> {
        let start = index as u64 * self.source_count as u64;
        let end = (index as u64 + 1) * self.source_count as u64;
        (start / self.destination_count as u64) as usize
            ..end.div_ceil(self.destination_count as u64) as usize
    }
    fn weight(&self, index: u32, source: usize) -> u64 {
        let start = index as u64 * self.source_count as u64;
        let end = (index as u64 + 1) * self.source_count as u64;
        end.min((source as u64 + 1) * self.destination_count as u64)
            - start.max(source as u64 * self.destination_count as u64)
    }
}
fn sample_stretch(
    source: &[u8],
    stride: usize,
    x: &StretchAxis,
    y: &StretchAxis,
    i: u32,
    j: u32,
    mode: u32,
) -> [u8; 3] {
    if mode == 3 {
        let at = y.nearest(j) * stride + x.nearest(i) * 4;
        return source[at..at + 3].try_into().unwrap();
    }
    if mode == 4 {
        // Deterministic area filtering. Driver-specific HALFTONE dithering is
        // not modeled; differential Windows captures must qualify fidelity.
        let mut sum = [0u128; 3];
        for sy in y.coverage(j) {
            for sx in x.coverage(i) {
                let weight = x.weight(i, sx) as u128 * y.weight(j, sy) as u128;
                let at = sy * stride + sx * 4;
                for channel in 0..3 {
                    sum[channel] += source[at + channel] as u128 * weight;
                }
            }
        }
        let denominator = x.source_count as u128 * y.source_count as u128;
        return sum.map(|value| ((value + denominator / 2) / denominator) as u8);
    }
    let mut color = if mode == 1 { [255; 3] } else { [0; 3] };
    for sy in y.reduction_range(j) {
        for sx in x.reduction_range(i) {
            let at = sy * stride + sx * 4;
            for channel in 0..3 {
                if mode == 1 {
                    color[channel] &= source[at + channel];
                } else {
                    color[channel] |= source[at + channel];
                }
            }
        }
    }
    color
}
fn rop2(mode: u32, pen: u8, destination: u8) -> u8 {
    let table = mode.saturating_sub(1) as u8;
    let mut result = 0;
    for index in 0..4 {
        if table & (1 << index) != 0 {
            result |= (if index & 2 != 0 { pen } else { !pen })
                & (if index & 1 != 0 {
                    destination
                } else {
                    !destination
                });
        }
    }
    result
}
fn rop3(table: u8, pattern: u8, source: u8, destination: u8) -> u8 {
    let mut result = 0;
    for index in 0..8 {
        if table & (1 << index) != 0 {
            result |= (if index & 4 != 0 { pattern } else { !pattern })
                & (if index & 2 != 0 { source } else { !source })
                & (if index & 1 != 0 {
                    destination
                } else {
                    !destination
                });
        }
    }
    result
}
fn intersect(a: Rect, b: Rect) -> Rect {
    Rect {
        left: a.left.max(b.left),
        top: a.top.max(b.top),
        right: a.right.min(b.right),
        bottom: a.bottom.min(b.bottom),
    }
}
#[derive(Debug)]
enum Object {
    Brush {
        color: u32,
        null: bool,
        style: u32,
        hatch: u32,
    },
    Pen {
        style: u32,
        width: u32,
        color: u32,
        ext: Option<ExtPen>,
    },
    Bitmap {
        dib: Dib,
    },
    Region {
        rects: Vec<Rect>,
    },
    MetaDc {
        enhanced: bool,
    },
    Metafile {
        enhanced: bool,
    },
    Font {
        height: i32,
        width: i32,
        weight: u32,
        italic: bool,
        face: Vec<u8>,
    },
    StockBitmap {
        bits: u32,
    },
    MemoryDc {
        stock: u32,
    },
    WindowDc {
        window: u32,
        clip: Rect,
    },
}
#[derive(Debug, Clone, Copy)]
struct GeometryPen {
    color: u32,
    alternate: bool,
}

#[derive(Debug, Clone)]
struct ExtPen {
    style: u32,
    width: u32,
    brush_style: u32,
    color: u32,
    hatch: u32,
    user_style: Vec<u32>,
}

/// Read-only object state for `GetObjectA`-style queries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExtPenInfo<'a> {
    Fields {
        style: u32,
        width: u32,
        brush_style: u32,
        color: u32,
        hatch: u32,
        user_style: &'a [u32],
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObjectInfo<'a> {
    Pen {
        style: u32,
        width: u32,
        color: u32,
        extended: Option<ExtPenInfo<'a>>,
    },
    Brush {
        style: u32,
        color: u32,
        hatch: u32,
    },
    Font {
        height: i32,
        width: i32,
        weight: u32,
        italic: bool,
        face: &'a [u8],
    },
    Bitmap {
        width: u32,
        height: u32,
        stride: u32,
        bits: u32,
        bit_count: u16,
        section: bool,
    },
}

#[derive(Debug, Clone)]
pub struct DcState {
    pub text_color: u32,
    pub background_color: u32,
    pub background_mode: u32,
    pub text_align: u32,
    pub text_extra: i32,
    pub poly_fill_mode: u32,
    pub mapper_flags: u32,
    pub map_mode: u32,
    pub miter_limit: f32,
    pub bounds_enabled: bool,
    pub bounds: Option<Rect>,
    pub window_ext: (i32, i32),
    pub viewport_ext: (i32, i32),
    pub window_org: (i32, i32),
    pub viewport_org: (i32, i32),
    pub layout: u32,
    pub stretch_mode: u32,
    pub rop2: u32,
    pub current_position: (i32, i32),
    pub brush_origin: (i32, i32),
    pub clip: Option<Rect>,
    pub bitmap: Option<u32>,
    pub palette: Option<u32>,
    pub brush: u32,
    pub pen: u32,
    pub font: u32,
}
#[derive(Debug)]
struct Dc {
    state: DcState,
    saved: Vec<DcState>,
}
#[derive(Debug)]
struct DibPattern {
    width: u32,
    height: u32,
    stride: u32,
    indices: Vec<u8>,
    color_table: Vec<u16>,
}

#[derive(Debug)]
pub struct Gdi {
    objects: Handles<Object>,
    surfaces: BTreeMap<u32, Surface>,
    stock_objects: BTreeMap<u32, u32>,
    dcs: BTreeMap<u32, Dc>,
    patterns: BTreeMap<u32, (u32, u32, Vec<u8>)>,
    dib_patterns: BTreeMap<u32, DibPattern>,
    palette_colors: BTreeMap<u32, Vec<[u8; 4]>>,
    /// Deleted DC handles keep reporting their object type (`OBJ_DC`), as
    /// Windows does, while every other operation fails.
    deleted_dcs: BTreeMap<u32, u32>,
}
impl Default for Gdi {
    fn default() -> Self {
        Self {
            objects: Handles::with_namespace(2),
            surfaces: BTreeMap::new(),
            stock_objects: BTreeMap::new(),
            dcs: BTreeMap::new(),
            patterns: BTreeMap::new(),
            dib_patterns: BTreeMap::new(),
            palette_colors: BTreeMap::new(),
            deleted_dcs: BTreeMap::new(),
        }
    }
}
impl Gdi {
    pub fn stock_object(&mut self, index: u32) -> Result<u32, Error> {
        if let Some(&handle) = self.stock_objects.get(&index) {
            return Ok(handle);
        }
        // Win32 stock brushes: WHITE, LTGRAY, GRAY, DKGRAY, BLACK, NULL.
        // Stock pens: WHITE, BLACK, NULL. Hover asks for LTGRAY_BRUSH (1)
        // during its initial window setup.
        let handle = match index {
            0..=5 => {
                let color = match index {
                    0 => 0xffffff,
                    1 => 0xc0c0c0,
                    2 => 0x808080,
                    3 => 0x404040,
                    4 => 0,
                    index => index,
                };
                self.objects
                    .insert(Object::Brush {
                        color,
                        null: index == 5,
                        style: u32::from(index == 5),
                        hatch: 0,
                    })
                    .ok_or(Error::Exhausted)?
            }
            6 => self
                .objects
                .insert(Object::Pen {
                    style: 0,
                    width: 1,
                    color: 0xffffff,
                    ext: None,
                })
                .ok_or(Error::Exhausted)?,
            7 => self
                .objects
                .insert(Object::Pen {
                    style: 0,
                    width: 1,
                    color: 0,
                    ext: None,
                })
                .ok_or(Error::Exhausted)?,
            8 => self
                .objects
                .insert(Object::Pen {
                    style: 5,
                    width: 1,
                    color: 0,
                    ext: None,
                })
                .ok_or(Error::Exhausted)?,
            10..=14 | 16 | 17 => self
                .objects
                .insert(Object::Font {
                    height: -14,
                    width: 0,
                    weight: 400,
                    italic: false,
                    face: b"System".to_vec(),
                })
                .ok_or(Error::Exhausted)?,
            _ => return Err(Error::InvalidHandle),
        };
        self.stock_objects.insert(index, handle);
        Ok(handle)
    }
    pub fn resize_surface(&mut self, window: u32, width: u32, height: u32) -> Result<(), Error> {
        let current = self.surfaces.get(&window).ok_or(Error::InvalidHandle)?;
        if current.width == width && current.height == height {
            return Ok(());
        }
        if width == 0 || height == 0 || width > i32::MAX as u32 || height > i32::MAX as u32 {
            return Err(Error::InvalidDimensions);
        }
        let len = (width as usize)
            .checked_mul(height as usize)
            .and_then(|v| v.checked_mul(4))
            .ok_or(Error::InvalidDimensions)?;
        let mut rgba = vec![0; len];
        for p in rgba.chunks_exact_mut(4) {
            p[3] = 255;
        }
        let surface = self.surfaces.get_mut(&window).ok_or(Error::InvalidHandle)?;
        let copied_width = surface.width.min(width) as usize * 4;
        for row in 0..surface.height.min(height) as usize {
            let source = row * surface.width as usize * 4;
            let destination = row * width as usize * 4;
            rgba[destination..destination + copied_width]
                .copy_from_slice(&surface.rgba[source..source + copied_width]);
        }
        surface.width = width;
        surface.height = height;
        surface.rgba = rgba;
        surface.dirty = true;
        Ok(())
    }
    pub fn fill_surface(&mut self, window: u32, rect: Rect, color: u32) -> Result<(), Error> {
        let surface = self.surfaces.get_mut(&window).ok_or(Error::InvalidHandle)?;
        let left = rect.left.max(0).min(surface.width as i32) as u32;
        let top = rect.top.max(0).min(surface.height as i32) as u32;
        let right = rect.right.max(0).min(surface.width as i32) as u32;
        let bottom = rect.bottom.max(0).min(surface.height as i32) as u32;
        if left >= right || top >= bottom {
            return Ok(());
        }
        let rgb = [
            (color & 255) as u8,
            ((color >> 8) & 255) as u8,
            ((color >> 16) & 255) as u8,
            255,
        ];
        for y in top..bottom {
            for x in left..right {
                let at = ((y * surface.width + x) * 4) as usize;
                surface.rgba[at..at + 4].copy_from_slice(&rgb);
            }
        }
        surface.dirty = true;
        Ok(())
    }
    pub fn frame_surface(&mut self, window: u32, rect: Rect, color: u32) -> Result<(), Error> {
        self.fill_surface(
            window,
            Rect {
                left: rect.left,
                top: rect.top,
                right: rect.right,
                bottom: rect.top + 1,
            },
            color,
        )?;
        self.fill_surface(
            window,
            Rect {
                left: rect.left,
                top: rect.bottom - 1,
                right: rect.right,
                bottom: rect.bottom,
            },
            color,
        )?;
        self.fill_surface(
            window,
            Rect {
                left: rect.left,
                top: rect.top,
                right: rect.left + 1,
                bottom: rect.bottom,
            },
            color,
        )?;
        self.fill_surface(
            window,
            Rect {
                left: rect.right - 1,
                top: rect.top,
                right: rect.right,
                bottom: rect.bottom,
            },
            color,
        )
    }
    pub fn draw_default_text(
        &mut self,
        window: u32,
        x: i32,
        y: i32,
        width: u32,
        text: &[u8],
        color: u32,
    ) -> Result<(), Error> {
        let (text_width, _) = crate::font::extent(text);
        let mut cursor = x + ((width as i32 - text_width as i32).max(0) / 2);
        let rgb = [
            (color & 255) as u8,
            ((color >> 8) & 255) as u8,
            ((color >> 16) & 255) as u8,
        ];
        let surface = self.surfaces.get_mut(&window).ok_or(Error::InvalidHandle)?;
        for byte in text {
            for gy in 0..16 {
                for gx in 0..16 {
                    if crate::font::glyph(*byte)[gy * 16 + gx] < 128 {
                        continue;
                    }
                    let px = cursor + gx as i32;
                    let py = y + gy as i32;
                    if px < 0 || py < 0 || px >= surface.width as i32 || py >= surface.height as i32
                    {
                        continue;
                    }
                    let at = ((py as u32 * surface.width + px as u32) * 4) as usize;
                    surface.rgba[at..at + 3].copy_from_slice(&rgb);
                    surface.rgba[at + 3] = 255;
                }
            }
            cursor += crate::font::extent(&[*byte]).0 as i32;
        }
        surface.dirty = true;
        Ok(())
    }
    pub fn composite(&mut self, parent: u32, child: u32, x: i32, y: i32) -> Result<(), Error> {
        let source = self.surfaces.get(&child).ok_or(Error::InvalidHandle)?;
        let (sw, sh, rgba) = (source.width, source.height, source.rgba.clone());
        let target = self.surfaces.get_mut(&parent).ok_or(Error::InvalidHandle)?;
        for sy in 0..sh {
            let dy = y + sy as i32;
            if dy < 0 || dy >= target.height as i32 {
                continue;
            }
            for sx in 0..sw {
                let dx = x + sx as i32;
                if dx < 0 || dx >= target.width as i32 {
                    continue;
                }
                let src = ((sy * sw + sx) * 4) as usize;
                let dst = ((dy as u32 * target.width + dx as u32) * 4) as usize;
                target.rgba[dst..dst + 4].copy_from_slice(&rgba[src..src + 4]);
            }
        }
        target.dirty = true;
        Ok(())
    }
    pub fn pen(&mut self, style: u32, width: u32, color: u32) -> Result<u32, Error> {
        // GDI normalizes unsupported cosmetic styles to PS_SOLID. Negative
        // widths are stored as their magnitude; PS_NULL always reports width 1
        // and no color.
        let width = (width as i32).unsigned_abs();
        let (style, width, color) = if style == 5 {
            (5, 1, 0)
        } else {
            (if style > 6 { 0 } else { style }, width, color & 0xffffff)
        };
        if style == 5 {
            return self.stock_object(8);
        }
        self.objects
            .insert(Object::Pen {
                style,
                width,
                color,
                ext: None,
            })
            .ok_or(Error::Exhausted)
    }
    /// `ExtCreatePen` with its extended pen record.
    ///
    /// The validation follows the observable Win32 contracts exercised by
    /// Wine's pen suite: cosmetic pens are one pixel except PS_NULL, geometric
    /// pens accept widths and PS_INSIDEFRAME, and user-style arrays are 2..16
    /// non-negative entries.
    pub fn ext_pen(
        &mut self,
        style: u32,
        width: u32,
        brush_style: u32,
        color: u32,
        hatch: u32,
        user_style: &[u32],
    ) -> Result<u32, Error> {
        const PS_GEOMETRIC: u32 = 0x10000;
        const PS_NULL: u32 = 5;
        const PS_INSIDEFRAME: u32 = 6;
        const PS_USERSTYLE: u32 = 7;
        const PS_ALTERNATE: u32 = 8;
        let geometric = style & PS_GEOMETRIC != 0;
        let base = style & 0xf;
        if base > PS_ALTERNATE
            || (base == PS_INSIDEFRAME && !geometric)
            || (base == PS_ALTERNATE && geometric)
        {
            return Err(Error::UnsupportedOperation);
        }
        if base == PS_USERSTYLE {
            if !(2..=16).contains(&user_style.len())
                || user_style.iter().any(|entry| (*entry as i32) < 0)
                || user_style.iter().all(|entry| *entry == 0)
            {
                return Err(Error::UnsupportedOperation);
            }
        } else if !user_style.is_empty() {
            return Err(Error::UnsupportedOperation);
        }
        // Cosmetic pens only accept a solid brush; geometric pens accept the
        // classic brush styles. BS_NULL becomes the stock null pen.
        if !geometric && brush_style != 0 {
            return Err(Error::UnsupportedOperation);
        }
        if geometric && brush_style == 1 {
            return self.stock_object(8);
        }
        if geometric && !matches!(brush_style, 0 | 2 | 3 | 5 | 6) {
            return Err(Error::UnsupportedOperation);
        }
        // ExtCreatePen reports a DIBPATTERN handle as DIBPATTERNPT.
        let brush_style = if brush_style == 5 { 6 } else { brush_style };
        // Windows returns the stock NULL_PEN for either cosmetic or geometric
        // PS_NULL extended pens.
        if base == PS_NULL {
            return self.stock_object(8);
        }
        let width = if base == PS_NULL {
            if geometric { 0 } else { 1 }
        } else {
            let width = (width as i32).unsigned_abs();
            if !geometric && width != 1 {
                return Err(Error::UnsupportedOperation);
            }
            width
        };
        let (color, hatch) = if base == PS_NULL || brush_style >= 3 {
            (0, hatch)
        } else {
            (color & 0xffffff, hatch)
        };
        let ext = ExtPen {
            style,
            width,
            brush_style,
            color,
            hatch,
            user_style: user_style.to_vec(),
        };
        self.objects
            .insert(Object::Pen {
                style: base,
                width,
                color,
                ext: Some(ext),
            })
            .ok_or(Error::Exhausted)
    }

    pub fn hatch_brush(&mut self, hatch: u32, color: u32) -> Result<u32, Error> {
        // HS_API_MAX is 12. The documented hatch styles are 0..5; Windows
        // accepts 6..11 as solid brushes with no hatch.
        if hatch >= 12 {
            return Err(Error::UnsupportedOperation);
        }
        let (style, hatch) = if hatch <= 5 { (2, hatch) } else { (0, 0) };
        self.objects
            .insert(Object::Brush {
                color: color & 0xffffff,
                null: false,
                style, // BS_HATCHED or BS_SOLID
                hatch,
            })
            .ok_or(Error::Exhausted)
    }

    pub fn solid_brush(&mut self, color: u32) -> Result<u32, Error> {
        self.objects
            .insert(Object::Brush {
                color: color & 0xffffff,
                null: false,
                style: 0,
                hatch: 0,
            })
            .ok_or(Error::Exhausted)
    }
    /// `CreateDIBPatternBrushPt`.
    ///
    /// The brush snapshots a palette-indexed 8-bpp DIB pattern. A positive
    /// `biHeight` is a bottom-up DIB, so its memory rows are flipped into
    /// top-down pattern rows; a negative height is already top-down.
    pub fn dib_pattern_brush(
        &mut self,
        memory: &dyn MemoryAccess,
        info: u32,
        usage: u32,
    ) -> Result<u32, Error> {
        let header = |offset: u32| -> Result<u32, Error> {
            let mut bytes = [0u8; 4];
            memory
                .read(info.checked_add(offset).ok_or(Error::InvalidDimensions)?, &mut bytes)
                .map_err(|_| Error::UnsupportedOperation)?;
            Ok(u32::from_le_bytes(bytes))
        };
        let width = header(4)?;
        let signed_height = header(8)? as i32;
        let bit_count = header(14)? & 0xffff;
        let compression = header(16)?;
        if width == 0 || signed_height == 0 || width > i32::MAX as u32 || compression != 0 {
            return Err(Error::UnsupportedOperation);
        }
        let height = signed_height.unsigned_abs();
        let bottom_up = signed_height > 0;
        let handle = self
            .objects
            .insert(Object::Brush {
                color: 0,
                null: false,
                style: 5, // BS_DIBPATTERN
                hatch: info,
            })
            .ok_or(Error::Exhausted)?;
        if usage == 1 && bit_count == 8 {
            let mut count_bytes = [0u8; 4];
            memory
                .read(info.checked_add(32).ok_or(Error::InvalidDimensions)?, &mut count_bytes)
                .map_err(|_| Error::UnsupportedOperation)?;
            let mut colors = u32::from_le_bytes(count_bytes) as usize;
            if colors == 0 {
                colors = 256;
            }
            if colors > 256 {
                return Err(Error::UnsupportedOperation);
            }
            let mut raw = vec![0u8; colors * 2];
            memory
                .read(info.checked_add(40).ok_or(Error::InvalidDimensions)?, &mut raw)
                .map_err(|_| Error::UnsupportedOperation)?;
            let color_table = raw
                .chunks_exact(2)
                .map(|entry| u16::from_le_bytes([entry[0], entry[1]]))
                .collect();
            let stride = ((width as u64 * 8 + 31) / 32 * 4) as u32;
            let mut indices = vec![0u8; stride as usize * height as usize];
            let bits = info
                .checked_add(40 + (colors as u32) * 2)
                .ok_or(Error::InvalidDimensions)?;
            memory
                .read(bits, &mut indices)
                .map_err(|_| Error::UnsupportedOperation)?;
            if bottom_up {
                let stride = stride as usize;
                for row in 0..height as usize / 2 {
                    let other = height as usize - 1 - row;
                    for column in 0..stride {
                        indices.swap(row * stride + column, other * stride + column);
                    }
                }
            }
            self.dib_patterns.insert(
                handle,
                DibPattern {
                    width,
                    height,
                    stride,
                    indices,
                    color_table,
                },
            );
        }
        Ok(handle)
    }

    pub fn set_palette_colors(&mut self, palette: u32, colors: Vec<[u8; 4]>) {
        self.palette_colors.insert(palette, colors);
    }

    /// `FillRect`: fill a rectangle with a caller-supplied brush.
    pub fn fill_rect(
        &mut self,
        memory: &mut dyn MemoryAccess,
        dc: u32,
        rect: Rect,
        brush: u32,
    ) -> Result<(), Error> {
        let state = self.dc_state(dc)?.clone();
        match self.objects.get(brush) {
            Some(Object::Brush { null: true, .. }) => Ok(()),
            Some(Object::Brush { null: false, style: 0, color, .. }) => {
                self.fill_dc(memory, dc, rect, *color)
            }
            Some(Object::Brush { null: false, style: 3 | 5, .. }) => {
                if self.patterns.get(&brush).is_none() {
                    return Err(Error::UnsupportedOperation);
                }
                let clipped = intersect(self.clip_bounds(dc)?, rect);
                if clipped.left >= clipped.right || clipped.top >= clipped.bottom {
                    return Ok(());
                }
                let mut row = vec![0; (clipped.right - clipped.left) as usize * 4];
                for y in clipped.top..clipped.bottom {
                    self.read_row(memory, dc, clipped.left as u32, y as u32, &mut row)?;
                    for (index, pixel) in row.chunks_exact_mut(4).enumerate() {
                        let x = clipped.left + index as i32;
                        if let Some(color) = self.pattern_pixel(
                            dc,
                            brush,
                            x - state.brush_origin.0,
                            y - state.brush_origin.1,
                        ) {
                            for channel in 0..3 {
                                pixel[channel] = rop2(state.rop2, color[channel], pixel[channel]);
                            }
                            pixel[3] = 255;
                        }
                    }
                    self.write_row(memory, dc, clipped.left as u32, y as u32, &row)?;
                }
                Ok(())
            }
            _ => Err(Error::WrongType),
        }
    }

    /// `CreateBrushIndirect`.
    pub fn brush_indirect(
        &mut self,
        memory: &dyn MemoryAccess,
        style: u32,
        color: u32,
        hatch: u32,
    ) -> Result<u32, Error> {
        match style {
            0 => self.solid_brush(color),
            1 => self.stock_object(5),
            2 => self.hatch_brush(hatch, color),
            3 | 7 => self.pattern_brush(memory, hatch),
            // ExtCreatePen reports DIBPATTERN handles as DIBPATTERNPT. Keep
            // the pointer/handle for queries; pattern drawing needs a DIB
            // decoder and is not implemented yet.
            5 | 6 => self
                .objects
                .insert(Object::Brush {
                    color: 0,
                    null: false,
                    style: 5,
                    // HGLOBAL DIBPATTERN handles are copied by Windows; the
                    // flat guest heap has no separate handle to expose.
                    hatch: if style == 5 { 0 } else { hatch },
                })
                .ok_or(Error::Exhausted),
            _ => Err(Error::UnsupportedOperation),
        }
    }

    /// Snapshot an 8x8 device pattern. The brush owns its pixels independently
    /// of the source bitmap's subsequent mutation or deletion.
    pub fn pattern_brush(&mut self, memory: &dyn MemoryAccess, bitmap: u32) -> Result<u32, Error> {
        let Some(Object::Bitmap { dib }) = self.objects.get(bitmap) else { return Err(Error::WrongType); };
        let width = dib.width.min(8);
        let height = dib.height.min(8);
        let mut pixels = vec![0; (width * height * 4) as usize];
        for (y, row) in pixels.chunks_exact_mut(width as usize * 4).enumerate() {
            dib.rgba_row(memory, 0, y as u32, row)?;
        }
        let handle = self.objects.insert(Object::Brush { color: 0, null: false, style: 3, hatch: bitmap }).ok_or(Error::Exhausted)?;
        self.patterns.insert(handle, (width, height, pixels));
        Ok(handle)
    }
    fn pattern_pixel(&self, dc: u32, brush: u32, x: i32, y: i32) -> Option<[u8; 3]> {
        if let Some((width, height, pixels)) = self.patterns.get(&brush) {
            let x = x.rem_euclid(*width as i32) as usize;
            let y = y.rem_euclid(*height as i32) as usize;
            let at = (y * *width as usize + x) * 4;
            return Some([pixels[at], pixels[at + 1], pixels[at + 2]]);
        }
        let pattern = self.dib_patterns.get(&brush)?;
        let x = x.rem_euclid(pattern.width as i32) as u32;
        let y = y.rem_euclid(pattern.height as i32) as u32;
        let index = *pattern
            .indices
            .get((y * pattern.stride + x) as usize)? as usize;
        let color_index = *pattern.color_table.get(index)? as usize;
        let palette = self.dc_state(dc).ok()?.palette?;
        let color = self.palette_colors.get(&palette)?.get(color_index)?;
        Some([color[0], color[1], color[2]])
    }
    pub fn font(
        &mut self,
        height: i32,
        width: i32,
        weight: u32,
        italic: bool,
        face: &[u8],
    ) -> Result<u32, Error> {
        self.objects
            .insert(Object::Font {
                height,
                width,
                weight,
                italic,
                face: face.iter().copied().take(31).collect(),
            })
            .ok_or(Error::Exhausted)
    }
    /// The object currently selected in a DC slot.
    ///
    /// `kind` is the GDI `OBJ_*` value. An invalid DC or an empty slot returns
    /// `InvalidHandle`; the dispatcher turns that into the API's NULL result.
    pub fn current_object(&self, dc: u32, kind: u32) -> Result<u32, Error> {
        let state = self.dc_state(dc)?;
        Ok(match kind {
            1 | 11 => state.pen,         // OBJ_PEN / OBJ_EXTPEN
            2 => state.brush,            // OBJ_BRUSH
            5 => state.palette.unwrap_or(0), // OBJ_PALETTE
            6 => state.font,             // OBJ_FONT
            7 => state.bitmap.unwrap_or(0), // OBJ_BITMAP
            _ => return Err(Error::UnsupportedOperation),
        })
    }

    /// `GetObjectType` for objects owned by GDI. Palettes live in the runtime
    /// and are handled by the dispatcher.
    pub fn object_type(&self, handle: u32) -> Option<u32> {
        match self.objects.get(handle) {
            Some(Object::Pen { ext: Some(ext), .. }) if (ext.style & 0xf) != 5 => Some(11), // OBJ_EXTPEN
            Some(Object::Pen { .. }) => Some(1),                 // OBJ_PEN
            Some(Object::Brush { .. }) => Some(2),               // OBJ_BRUSH
            Some(Object::WindowDc { .. }) => Some(3),            // OBJ_DC
            Some(Object::MemoryDc { .. }) => Some(10),           // OBJ_MEMDC
            Some(Object::Font { .. }) => Some(6),                // OBJ_FONT
            Some(Object::Bitmap { .. } | Object::StockBitmap { .. }) => Some(7), // OBJ_BITMAP
            Some(Object::Region { .. }) => Some(8), // OBJ_REGION
            Some(Object::MetaDc { enhanced: true }) => Some(12), // OBJ_ENHMETADC
            Some(Object::MetaDc { enhanced: false }) => Some(4), // OBJ_METADC
            Some(Object::Metafile { enhanced: true }) => Some(13), // OBJ_ENHMETAFILE
            Some(Object::Metafile { enhanced: false }) => Some(9), // OBJ_METAFILE
            None => self.deleted_dcs.get(&handle).copied(),
        }
    }

    pub fn object_info(&self, handle: u32) -> Option<ObjectInfo<'_>> {
        match self.objects.get(handle) {
            Some(Object::Pen {
                style,
                width,
                color,
                ext,
            }) => Some(ObjectInfo::Pen {
                style: *style,
                width: *width,
                color: *color,
                extended: ext.as_ref().map(|ext| ExtPenInfo::Fields {
                    style: ext.style,
                    width: ext.width,
                    brush_style: ext.brush_style,
                    color: ext.color,
                    hatch: ext.hatch,
                    user_style: &ext.user_style,
                }),
            }),
            Some(Object::Brush {
                color,
                null,
                style,
                hatch,
            }) => Some(ObjectInfo::Brush {
                style: if *null { 1 } else { *style },
                color: *color,
                hatch: *hatch,
            }),
            Some(Object::Font {
                height,
                width,
                weight,
                italic,
                face,
            }) => Some(ObjectInfo::Font {
                height: *height,
                width: *width,
                weight: *weight,
                italic: *italic,
                face,
            }),
            Some(Object::Bitmap { dib }) => Some(ObjectInfo::Bitmap {
                width: dib.width,
                height: dib.height,
                stride: dib.stride(),
                bits: dib.bits,
                bit_count: dib.bit_count(),
                section: dib.is_section(),
            }),
            Some(Object::StockBitmap { bits }) => Some(ObjectInfo::Bitmap {
                width: 1,
                height: 1,
                stride: 4,
                bits: *bits,
                bit_count: 1,
                section: false,
            }),
            _ => None,
        }
    }

    pub fn font_face(&self, dc: u32) -> Result<Vec<u8>, Error> {
        let font = self.dc_state(dc)?.font;
        match self.objects.get(font) {
            Some(Object::Font { face, .. }) => Ok(face.clone()),
            _ => Err(Error::WrongType),
        }
    }

    /// Advance width used by the raster text path for one ANSI byte.
    pub fn char_width(&self, dc: u32, byte: u8) -> Result<u32, Error> {
        self.dc_state(dc)?;
        Ok(crate::font::extent(&[byte]).0)
    }

    /// Map a COLORREF to the nearest color available in the DC target.
    /// Palette colour for an index of the selected DIB.
    pub fn dib_palette_color(&self, dc: u32, index: usize) -> Option<u32> {
        let bitmap = self.dc_state(dc).ok()?.bitmap?;
        let Some(Object::Bitmap { dib }) = self.objects.get(bitmap) else {
            return None;
        };
        let palette = dib.palette();
        let entry = palette.get(index).or_else(|| palette.first())?;
        Some(u32::from(entry[0]) | (u32::from(entry[1]) << 8) | (u32::from(entry[2]) << 16))
    }
    /// Nearest palette index in the selected DIB, for index-preserving pixel writes.
    pub fn dib_nearest_index(&self, dc: u32, color: u32) -> Option<u16> {
        let bitmap = self.dc_state(dc).ok()?.bitmap?;
        let Some(Object::Bitmap { dib }) = self.objects.get(bitmap) else {
            return None;
        };
        if dib.bit_count() > 8 || dib.palette().is_empty() {
            return None;
        }
        let (red, green, blue) = (
            (color & 255) as i32,
            ((color >> 8) & 255) as i32,
            ((color >> 16) & 255) as i32,
        );
        dib.palette()
            .iter()
            .enumerate()
            .min_by_key(|(_, entry)| {
                let distance = (entry[0] as i32 - red).pow(2)
                    + (entry[1] as i32 - green).pow(2)
                    + (entry[2] as i32 - blue).pow(2);
                (distance, -(entry[0] as i32 + entry[1] as i32 + entry[2] as i32))
            })
            .map(|(index, _)| index as u16)
    }
    /// Write a palette index directly (DIBINDEX/PALETTEINDEX semantics).
    pub fn set_pixel_index(
        &mut self,
        memory: &mut dyn MemoryAccess,
        dc: u32,
        x: i32,
        y: i32,
        index: usize,
    ) -> Result<u32, Error> {
        let bitmap = self.dc_state(dc)?.bitmap.ok_or(Error::WrongType)?;
        let Some(Object::Bitmap { dib }) = self.objects.get(bitmap) else {
            return Err(Error::WrongType);
        };
        let palette = dib.palette();
        if palette.is_empty() {
            return Err(Error::UnsupportedOperation);
        }
        let entry = palette.get(index).or_else(|| palette.first()).copied().unwrap_or([0, 0, 0, 255]);
        let stored_index = if index < palette.len() { index } else { 0 };
        dib.write_index(memory, x, y, stored_index as u8)
            .map_err(|_| Error::UnsupportedOperation)?;
        Ok(u32::from(entry[0]) | (u32::from(entry[1]) << 8) | (u32::from(entry[2]) << 16))
    }
    /// Resolve `DIBINDEX` against the selected DIB's color table.
    pub fn dib_index_color(&self, dc: u32, index: usize) -> Option<u32> {
        let bitmap = self.dc_state(dc).ok()?.bitmap?;
        let Some(Object::Bitmap { dib }) = self.objects.get(bitmap) else {
            return None;
        };
        let palette = dib.palette();
        let entry = palette.get(index).or_else(|| palette.first())?;
        Some(u32::from(entry[0]) | (u32::from(entry[1]) << 8) | (u32::from(entry[2]) << 16))
    }
    pub fn nearest_color(&self, dc: u32, color: u32) -> Result<u32, Error> {
        let state = self.dc_state(dc)?;
        let Some(bitmap) = state.bitmap else {
            return Ok(color & 0xffffff);
        };
        let Some(Object::Bitmap { dib }) = self.objects.get(bitmap) else {
            return Ok(color & 0xffffff);
        };
        if dib.bit_count() > 8 || dib.palette().is_empty() {
            return Ok(color & 0xffffff);
        }
        let (red, green, blue) = (
            (color & 255) as i32,
            ((color >> 8) & 255) as i32,
            ((color >> 16) & 255) as i32,
        );
        let palette = dib.palette();
        let nearest = palette
            .iter()
            .min_by_key(|entry| {
                let distance = (entry[0] as i32 - red).pow(2)
                    + (entry[1] as i32 - green).pow(2)
                    + (entry[2] as i32 - blue).pow(2);
                // GDI prefers white over black on an exact tie.
                (distance, -(entry[0] as i32 + entry[1] as i32 + entry[2] as i32))
            })
            .copied()
            .unwrap_or([0, 0, 0, 255]);
        Ok(u32::from(nearest[0]) | (u32::from(nearest[1]) << 8) | (u32::from(nearest[2]) << 16))
    }

    /// Read raw bitmap bits for `GetBitmapBits`.
    pub fn bitmap_bits_read(
        &self,
        memory: &dyn MemoryAccess,
        bitmap: u32,
        output: &mut [u8],
    ) -> Result<usize, Error> {
        let Some(Object::Bitmap { dib }) = self.objects.get(bitmap) else {
            return Err(Error::WrongType);
        };
        let length = (dib.stride() as usize)
            .checked_mul(dib.height as usize)
            .ok_or(Error::InvalidDimensions)?
            .min(output.len());
        memory
            .read(dib.bits, &mut output[..length])
            .map_err(|_| Error::UnsupportedOperation)?;
        Ok(length)
    }

    /// Write raw bitmap bits for `SetBitmapBits`.
    pub fn bitmap_bits_write(
        &self,
        memory: &mut dyn MemoryAccess,
        bitmap: u32,
        input: &[u8],
    ) -> Result<usize, Error> {
        let Some(Object::Bitmap { dib }) = self.objects.get(bitmap) else {
            return Err(Error::WrongType);
        };
        let length = (dib.stride() as usize)
            .checked_mul(dib.height as usize)
            .ok_or(Error::InvalidDimensions)?
            .min(input.len());
        memory
            .write(dib.bits, &input[..length])
            .map_err(|_| Error::UnsupportedOperation)?;
        Ok(length)
    }

    /// Create an owned bitmap over already-allocated guest pixels.
    pub fn bitmap_pixels(
        &mut self,
        memory: &dyn MemoryAccess,
        width: u32,
        height: u32,
        bits: u32,
        bit_count: u16,
    ) -> Result<u32, Error> {
        let dib = Dib::new(memory, width, height, bits, bit_count)?;
        self.objects
            .insert(Object::Bitmap { dib })
            .ok_or(Error::Exhausted)
    }

    /// `CreateRectRgn` and friends: a region is a list of (disjoint)
    /// rectangles. Degenerate rectangles describe the empty region.
    pub fn region(&mut self, rect: Rect) -> Result<u32, Error> {
        let rects = if rect.left < rect.right && rect.top < rect.bottom {
            vec![rect]
        } else {
            Vec::new()
        };
        self.objects
            .insert(Object::Region { rects })
            .ok_or(Error::Exhausted)
    }
    pub fn set_region(&mut self, handle: u32, rect: Rect) -> Result<(), Error> {
        let rects = if rect.left < rect.right && rect.top < rect.bottom {
            vec![rect]
        } else {
            Vec::new()
        };
        match self.objects.get_mut(handle) {
            Some(Object::Region { rects: current }) => {
                *current = rects;
                Ok(())
            }
            _ => Err(Error::WrongType),
        }
    }
    pub fn region_rects(&self, handle: u32) -> Result<&[Rect], Error> {
        match self.objects.get(handle) {
            Some(Object::Region { rects }) => Ok(rects),
            _ => Err(Error::WrongType),
        }
    }
    /// `GetRgnBox`: region type (NULLREGION/SIMPLEREGION) and bounding box.
    pub fn region_box(&self, handle: u32) -> Result<(u32, Rect), Error> {
        let rects = self.region_rects(handle)?;
        let Some(first) = rects.first() else {
            return Ok((
                1,
                Rect {
                    left: 0,
                    top: 0,
                    right: 0,
                    bottom: 0,
                },
            ));
        };
        let mut box_rect = *first;
        for rect in &rects[1..] {
            box_rect.left = box_rect.left.min(rect.left);
            box_rect.top = box_rect.top.min(rect.top);
            box_rect.right = box_rect.right.max(rect.right);
            box_rect.bottom = box_rect.bottom.max(rect.bottom);
        }
        let kind = if rects.len() == 1 { 2 } else { 3 };
        Ok((kind, box_rect))
    }
    /// `GetRegionData` bytes: RGNDATAHEADER followed by RECT entries.
    pub fn region_data(&self, handle: u32) -> Result<Vec<u8>, Error> {
        let rects = self.region_rects(handle)?;
        let (_, bound) = self.region_box(handle)?;
        let mut bytes = Vec::with_capacity(32 + rects.len() * 16);
        bytes.extend_from_slice(&32u32.to_le_bytes()); // dwSize
        bytes.extend_from_slice(&1u32.to_le_bytes()); // RDH_RECTANGLES
        bytes.extend_from_slice(&(rects.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&((rects.len() * 16) as u32).to_le_bytes());
        for value in [bound.left, bound.top, bound.right, bound.bottom] {
            bytes.extend_from_slice(&(value as u32).to_le_bytes());
        }
        for rect in rects {
            for value in [rect.left, rect.top, rect.right, rect.bottom] {
                bytes.extend_from_slice(&(value as u32).to_le_bytes());
            }
        }
        Ok(bytes)
    }
    /// Copy the DC clip into a region handle (`GetClipRgn`).
    pub fn get_clip_region(&mut self, dc: u32, handle: u32) -> Result<bool, Error> {
        let clip = self.dc_state(dc)?.clip;
        let Some(clip) = clip else { return Ok(false) };
        let rects = if clip.left < clip.right && clip.top < clip.bottom {
            vec![clip]
        } else {
            Vec::new()
        };
        match self.objects.get_mut(handle) {
            Some(Object::Region { rects: current }) => {
                *current = rects;
                Ok(true)
            }
            _ => Err(Error::WrongType),
        }
    }
    /// `SelectClipRgn`.
    pub fn select_clip_region(&mut self, dc: u32, handle: u32) -> Result<u32, Error> {
        let (kind, rects) = if handle == 0 {
            (1, Vec::new())
        } else {
            let rects = self.region_rects(handle)?.to_vec();
            let kind = if rects.is_empty() {
                1
            } else if rects.len() == 1 {
                2
            } else {
                3
            };
            (kind, rects)
        };
        self.dc_state(dc)?;
        if rects.len() > 1 {
            return Err(Error::UnsupportedOperation);
        }
        let rect = rects.first().copied().unwrap_or(Rect {
            left: 0,
            top: 0,
            right: 0,
            bottom: 0,
        });
        self.dc_state_mut(dc)?.clip = Some(rect);
        Ok(kind)
    }
    /// `SetMapMode`: reset the window/viewport extents for the new mode.
    pub fn set_map_mode(&mut self, dc: u32, mode: u32) -> Result<(), Error> {
        let current = self.dc_state(dc)?.map_mode;
        if mode == current && matches!(mode, 7 | 8) {
            return Ok(());
        }
        let muldiv = |a: i32, b: i32, c: i32| ((a as i64 * b as i64 + c as i64 / 2) / c as i64) as i32;
        let (window_ext, viewport_ext) = match mode {
            1 => ((1, 1), (1, 1)),
            2 | 7 => ((1690, 1270), (640, -480)),
            3 => ((16900, 12700), (640, -480)),
            4 => ((muldiv(1000, 169, 254), muldiv(1000, 127, 254)), (640, -480)),
            5 => ((muldiv(10000, 169, 254), muldiv(10000, 127, 254)), (640, -480)),
            6 => ((muldiv(14400, 169, 254), muldiv(14400, 127, 254)), (640, -480)),
            8 => {
                let state = self.dc_state(dc)?;
                (state.window_ext, state.viewport_ext)
            }
            _ => return Err(Error::UnsupportedOperation),
        };
        let state = self.dc_state_mut(dc)?;
        state.map_mode = mode;
        state.window_ext = window_ext;
        state.viewport_ext = viewport_ext;
        Ok(())
    }
    pub fn clear_bounds(&mut self, dc: u32) {
        if let Ok(state) = self.dc_state_mut(dc) {
            state.bounds = None;
        }
    }
    /// Extra inflation, in device units, that Windows applies to a geometric
    /// pen's drawing bounds. The values mirror the heuristics Wine's own
    /// conformance suite pins down for join and end-cap styles.
    pub fn pen_bounds_inflation(&self, dc: u32) -> i32 {
        let Ok(state) = self.dc_state(dc) else {
            return 0;
        };
        let Some(Object::Pen {
            ext: Some(ext), ..
        }) = self.objects.get(state.pen)
        else {
            return 0;
        };
        if ext.style & 0x10000 == 0 {
            return 0; // cosmetic pen
        }
        const PS_ENDCAP_MASK: u32 = 0x0f00;
        const PS_ENDCAP_SQUARE: u32 = 0x0100;
        const PS_JOIN_MITER: u32 = 0x2000;
        let mut inflate = ext.width.max(1) as i32 + 2;
        let join = ext.style & 0x3000;
        let endcap = ext.style & PS_ENDCAP_MASK;
        if join == PS_JOIN_MITER {
            inflate *= 5;
            if endcap == PS_ENDCAP_SQUARE {
                inflate = (inflate * 3 + 1) / 2;
            }
        } else if endcap == PS_ENDCAP_SQUARE {
            inflate -= inflate / 4;
        } else {
            inflate = (inflate + 1) / 2;
        }
        inflate
    }
    /// Round like GDI's `GDI_ROUND`, which rounds halves away from zero.
    fn gdi_round(value: f64) -> i32 {
        if value >= 0.0 {
            (value + 0.5).floor() as i32
        } else {
            (value - 0.5).ceil() as i32
        }
    }
    /// `LPtoDP` for one point, using the DC's window/viewport mapping and
    /// layout. RTL layout mirrors around the visible width.
    pub fn lp_to_dp_point(&self, dc: u32, x: i32, y: i32) -> Result<(i32, i32), Error> {
        let state = self.dc_state(dc)?;
        let mut scale_x = state.viewport_ext.0 as f64 / state.window_ext.0 as f64;
        let scale_y = state.viewport_ext.1 as f64 / state.window_ext.1 as f64;
        if state.layout & 1 != 0 {
            scale_x = -scale_x;
        }
        let mut dx = state.viewport_org.0 as f64 - scale_x * state.window_org.0 as f64;
        let dy = state.viewport_org.1 as f64 - scale_y * state.window_org.1 as f64;
        if state.layout & 1 != 0 {
            let width = self.target_bounds(dc)?.right as f64;
            dx = width - 1.0 - dx;
        }
        Ok((
            Self::gdi_round(x as f64 * scale_x + dx),
            Self::gdi_round(y as f64 * scale_y + dy),
        ))
    }
    /// `DPtoLP` for one point: the inverse of `lp_to_dp_point`.
    pub fn dp_to_lp_point(&self, dc: u32, x: i32, y: i32) -> Result<(i32, i32), Error> {
        let state = self.dc_state(dc)?;
        let mut scale_x = state.viewport_ext.0 as f64 / state.window_ext.0 as f64;
        let scale_y = state.viewport_ext.1 as f64 / state.window_ext.1 as f64;
        if state.layout & 1 != 0 {
            scale_x = -scale_x;
        }
        let mut dx = state.viewport_org.0 as f64 - scale_x * state.window_org.0 as f64;
        let dy = state.viewport_org.1 as f64 - scale_y * state.window_org.1 as f64;
        if state.layout & 1 != 0 {
            let width = self.target_bounds(dc)?.right as f64;
            dx = width - 1.0 - dx;
        }
        Ok((
            Self::gdi_round((x as f64 - dx) / scale_x),
            Self::gdi_round((y as f64 - dy) / scale_y),
        ))
    }
    /// Add one rectangle to the DC's accumulated drawing bounds.
    pub fn add_bounds(&mut self, dc: u32, rect: Rect) {
        if rect.left >= rect.right || rect.top >= rect.bottom {
            return;
        }
        if let Ok(state) = self.dc_state_mut(dc) {
            state.bounds = Some(match state.bounds {
                Some(bounds) => Rect {
                    left: bounds.left.min(rect.left),
                    top: bounds.top.min(rect.top),
                    right: bounds.right.max(rect.right),
                    bottom: bounds.bottom.max(rect.bottom),
                },
                None => rect,
            });
        }
    }
    /// `SetBoundsRect`. Returns the previous bounds state flags.
    pub fn set_bounds_rect(&mut self, dc: u32, rect: Option<Rect>, flags: u32) -> Result<u32, Error> {
        const DCB_RESET: u32 = 0x0001;
        const DCB_ACCUMULATE: u32 = 0x0002;
        const DCB_ENABLE: u32 = 0x0004;
        const DCB_DISABLE: u32 = 0x0008;
        let state = self.dc_state(dc)?;
        let mut previous = if state.bounds.is_some() { 0x3 } else { DCB_RESET };
        if state.bounds_enabled {
            previous |= DCB_ENABLE;
        } else {
            previous |= DCB_DISABLE;
        }
        if flags & DCB_RESET != 0 {
            self.dc_state_mut(dc)?.bounds = None;
        }
        if flags & DCB_ACCUMULATE != 0 {
            if let Some(rect) = rect {
                // The rectangle is logical; the bounds accumulate in device
                // space. RTL layout mirrors the mapped rectangle, which can
                // turn an inverted logical rect into a non-empty device one.
                let (left, top) = self.lp_to_dp_point(dc, rect.left, rect.top)?;
                let (right, bottom) = self.lp_to_dp_point(dc, rect.right, rect.bottom)?;
                self.add_bounds(
                    dc,
                    Rect {
                        left,
                        top,
                        right,
                        bottom,
                    },
                );
            }
        }
        let state = self.dc_state_mut(dc)?;
        if flags & DCB_ENABLE != 0 {
            state.bounds_enabled = true;
        }
        if flags & DCB_DISABLE != 0 {
            state.bounds_enabled = false;
        }
        Ok(previous)
    }
    /// `GetBoundsRect`: clip to the device surface, then convert to logical.
    pub fn get_bounds_rect(&mut self, dc: u32, flags: u32) -> Result<(u32, Rect), Error> {
        const DCB_RESET: u32 = 0x0001;
        let target = self.target_bounds(dc)?;
        let state = self.dc_state(dc)?;
        let (result, rect) = match state.bounds {
            Some(bounds) => {
                let device = Rect {
                    left: bounds.left.max(0),
                    top: bounds.top.max(0),
                    right: bounds.right.min(target.right),
                    bottom: bounds.bottom.min(target.bottom),
                };
                let (left, top) = self.dp_to_lp_point(dc, device.left, device.top)?;
                let (right, bottom) = self.dp_to_lp_point(dc, device.right, device.bottom)?;
                (0x3, Rect { left, top, right, bottom })
            }
            None => (
                DCB_RESET,
                Rect {
                    left: 0,
                    top: 0,
                    right: 0,
                    bottom: 0,
                },
            ),
        };
        if flags & DCB_RESET != 0 {
            self.dc_state_mut(dc)?.bounds = None;
        }
        Ok((result, rect))
    }
    pub fn is_dc(&self, handle: u32) -> bool {
        matches!(
            self.objects.get(handle),
            Some(Object::MemoryDc { .. } | Object::WindowDc { .. } | Object::MetaDc { .. })
        )
    }
    /// Window associated with a window DC; memory DCs have no window target.
    pub fn window_for_dc(&self, handle: u32) -> Option<u32> {
        match self.objects.get(handle) {
            Some(Object::WindowDc { window, .. }) => Some(*window),
            _ => None,
        }
    }
    pub fn create_surface(&mut self, window: u32, width: u32, height: u32) -> Result<(), Error> {
        if window == 0
            || width == 0
            || height == 0
            || width > i32::MAX as u32
            || height > i32::MAX as u32
            || self.surfaces.contains_key(&window)
        {
            return Err(Error::InvalidDimensions);
        }
        let length = (width as usize)
            .checked_mul(height as usize)
            .and_then(|v| v.checked_mul(4))
            .ok_or(Error::InvalidDimensions)?;
        let mut rgba = Vec::new();
        rgba.try_reserve_exact(length)
            .map_err(|_| Error::Exhausted)?;
        rgba.resize(length, 0);
        for p in rgba.chunks_exact_mut(4) {
            p[3] = 255;
        }
        self.surfaces.insert(
            window,
            Surface {
                width,
                height,
                rgba,
                dirty: true,
            },
        );
        Ok(())
    }
    pub fn surface(&self, window: u32) -> Option<&Surface> {
        self.surfaces.get(&window)
    }
    pub fn mark_surface_dirty(&mut self, window: u32) -> Result<(), Error> {
        self.surfaces
            .get_mut(&window)
            .ok_or(Error::InvalidHandle)?
            .dirty = true;
        Ok(())
    }
    pub fn mark_all_surfaces_dirty(&mut self) {
        for surface in self.surfaces.values_mut() {
            surface.dirty = true;
        }
    }
    /// Replace a window's pixels from a host renderer and mark them for
    /// presentation through the existing browser framebuffer path.
    pub fn present_rgba(&mut self, window: u32, rgba: &[u8]) -> Result<(), Error> {
        let surface = self.surfaces.get_mut(&window).ok_or(Error::InvalidHandle)?;
        if rgba.len() != surface.rgba.len() {
            return Err(Error::InvalidDimensions);
        }
        surface.rgba.copy_from_slice(rgba);
        // Win32's BI_RGB window surface and the selected TOLYMPIC pixel
        // format have no alpha channel. Treat the RGBA renderer output as
        // opaque when presenting it through GDI/browser pixels.
        for pixel in surface.rgba.chunks_exact_mut(4) {
            pixel[3] = 0xff;
        }
        surface.dirty = true;
        Ok(())
    }
    pub fn acknowledge_present(&mut self, window: u32) -> Result<(), Error> {
        self.surfaces
            .get_mut(&window)
            .ok_or(Error::InvalidHandle)?
            .dirty = false;
        Ok(())
    }
    pub fn remove_surface(&mut self, window: u32) -> Result<(), Error> {
        self.surfaces.remove(&window).ok_or(Error::InvalidHandle)?;
        Ok(())
    }
    fn init_dc(&mut self, handle: u32, bitmap: Option<u32>) -> Result<u32, Error> {
        let brush = self.stock_object(0)?;
        let pen = self.stock_object(7)?;
        let font = self.stock_object(13)?;
        self.dcs.insert(
            handle,
            Dc {
                state: DcState {
                    text_color: 0,
                    background_color: 0xffffff,
                    background_mode: 2,
                    text_align: 0,
                    text_extra: 0,
                    poly_fill_mode: 1,
                    mapper_flags: 0,
                    map_mode: 1,
                    miter_limit: 10.0,
                    bounds_enabled: false,
                    bounds: None,
                    window_ext: (1, 1),
                    viewport_ext: (1, 1),
                    window_org: (0, 0),
                    viewport_org: (0, 0),
                    layout: 0,
                    stretch_mode: 1,
                    rop2: 13,
                    current_position: (0, 0),
                    brush_origin: (0, 0),
                    clip: None,
                    bitmap,
                    palette: None,
                    brush,
                    pen,
                    font,
                },
                saved: Vec::new(),
            },
        );
        Ok(handle)
    }
    pub fn dc_state(&self, handle: u32) -> Result<&DcState, Error> {
        self.dcs
            .get(&handle)
            .map(|dc| &dc.state)
            .ok_or(Error::InvalidHandle)
    }
    pub fn dc_state_mut(&mut self, handle: u32) -> Result<&mut DcState, Error> {
        self.dcs
            .get_mut(&handle)
            .map(|dc| &mut dc.state)
            .ok_or(Error::InvalidHandle)
    }
    pub fn save_dc(&mut self, handle: u32) -> Result<u32, Error> {
        let dc = self.dcs.get_mut(&handle).ok_or(Error::InvalidHandle)?;
        // Windows does not include the bounds rectangle in SaveDC/RestoreDC.
        let mut state = dc.state.clone();
        state.bounds = None;
        state.bounds_enabled = false;
        dc.saved.push(state);
        Ok(dc.saved.len() as u32)
    }
    pub fn restore_dc(&mut self, handle: u32, level: i32) -> Result<bool, Error> {
        let dc = self.dcs.get_mut(&handle).ok_or(Error::InvalidHandle)?;
        let index = if level < 0 {
            dc.saved.len() as i64 + level as i64
        } else {
            level as i64 - 1
        };
        if index < 0 || index >= dc.saved.len() as i64 {
            return Ok(false);
        }
        let bounds = dc.state.bounds;
        let bounds_enabled = dc.state.bounds_enabled;
        dc.state = dc.saved[index as usize].clone();
        dc.state.bounds = bounds;
        dc.state.bounds_enabled = bounds_enabled;
        dc.saved.truncate(index as usize);
        Ok(true)
    }
    pub fn memory_dc(&mut self) -> Result<u32, Error> {
        self.memory_dc_with_bits(0)
    }
    /// A memory DC whose initial 1x1 stock bitmap lives at `bits`.
    pub fn memory_dc_with_bits(&mut self, bits: u32) -> Result<u32, Error> {
        let stock = self
            .objects
            .insert(Object::StockBitmap { bits })
            .ok_or(Error::Exhausted)?;
        let handle = self
            .objects
            .insert(Object::MemoryDc { stock })
            .ok_or(Error::Exhausted)?;
        self.init_dc(handle, Some(stock))
    }
    /// Insert an already-built DIB section object.
    pub fn insert_bitmap_dib(&mut self, dib: Dib) -> Result<u32, Error> {
        self.objects
            .insert(Object::Bitmap { dib })
            .ok_or(Error::Exhausted)
    }
    /// The bitmap selected into a memory DC, if it is a DIB-backed one.
    pub fn selected_dib_bitmap(&self, dc: u32) -> Option<u32> {
        let bitmap = self.dc_state(dc).ok()?.bitmap?;
        match self.objects.get(bitmap) {
            Some(Object::Bitmap { .. }) => Some(bitmap),
            _ => None,
        }
    }
    pub fn dib_color_table(&self, bitmap: u32) -> Option<Vec<[u8; 4]>> {
        match self.objects.get(bitmap) {
            Some(Object::Bitmap { dib }) => Some(dib.palette().to_vec()),
            _ => None,
        }
    }
    pub fn set_dib_color_table(
        &mut self,
        bitmap: u32,
        start: usize,
        colors: &[[u8; 4]],
    ) -> usize {
        match self.objects.get_mut(bitmap) {
            Some(Object::Bitmap { dib }) => dib.set_palette_entries(start, colors),
            _ => 0,
        }
    }
    /// Clone the DIB behind a bitmap for `GetDIBits`.
    pub fn bitmap_dib(&self, bitmap: u32) -> Option<Dib> {
        match self.objects.get(bitmap) {
            Some(Object::Bitmap { dib }) => Some(dib.clone()),
            _ => None,
        }
    }
    /// Depth of the bitmap selected into a memory DC. Returns `None` for
    /// device DCs, which use the display depth instead.
    pub fn dc_bitmap_depth(&self, dc: u32) -> Option<u16> {
        if !matches!(self.objects.get(dc), Some(Object::MemoryDc { .. })) {
            return None;
        }
        let bitmap = self.dc_state(dc).ok()?.bitmap?;
        match self.objects.get(bitmap) {
            Some(Object::Bitmap { dib }) => Some(dib.bit_count()),
            Some(Object::StockBitmap { .. }) => Some(1),
            _ => None,
        }
    }
    /// Packed bitmap bits at the DDB stride (16-bit aligned), which is what
    /// `GetBitmapBits` exposes.
    pub fn bitmap_bits_packed(
        &self,
        memory: &dyn MemoryAccess,
        bitmap: u32,
    ) -> Result<Vec<u8>, Error> {
        let Some(Object::Bitmap { dib }) = self.objects.get(bitmap) else {
            return Err(Error::WrongType);
        };
        let word_stride = ((dib.width * dib.bit_count() as u32 + 15) >> 3) & !1;
        let mut output = vec![0u8; word_stride as usize * dib.height as usize];
        for row in 0..dib.height {
            let source = dib
                .bits
                .checked_add(row * dib.stride())
                .ok_or(Error::InvalidDimensions)?;
            let destination = row as usize * word_stride as usize;
            memory
                .read(source, &mut output[destination..destination + word_stride as usize])
                .map_err(|_| Error::UnsupportedOperation)?;
        }
        Ok(output)
    }
    /// A metafile DC. Enhanced metafile DCs mirror the display device; old
    /// (Win16-style) metafile DCs report no capabilities.
    pub fn meta_dc(&mut self, enhanced: bool) -> Result<u32, Error> {
        let handle = self
            .objects
            .insert(Object::MetaDc { enhanced })
            .ok_or(Error::Exhausted)?;
        self.init_dc(handle, None)
    }
    pub fn close_metafile(&mut self, dc: u32, enhanced: bool) -> Result<u32, Error> {
        match self.objects.get(dc) {
            Some(Object::MetaDc { enhanced: is_enhanced }) if *is_enhanced == enhanced => {}
            _ => return Err(Error::WrongType),
        }
        self.delete_dc(dc)?;
        self.objects
            .insert(Object::Metafile { enhanced })
            .ok_or(Error::Exhausted)
    }
    pub fn is_meta_dc(&self, dc: u32) -> Option<bool> {
        match self.objects.get(dc) {
            Some(Object::MetaDc { enhanced }) => Some(*enhanced),
            _ => None,
        }
    }
    pub fn paint_dc(&mut self, window: u32, clip: Rect) -> Result<u32, Error> {
        self.surfaces.get(&window).ok_or(Error::InvalidHandle)?;
        let handle = self
            .objects
            .insert(Object::WindowDc { window, clip })
            .ok_or(Error::Exhausted)?;
        self.init_dc(handle, None)
    }
    pub fn bitmap(
        &mut self,
        memory: &dyn MemoryAccess,
        header: u32,
        bits: u32,
    ) -> Result<u32, Error> {
        let dib = Dib::from_header(memory, header, bits)?;
        self.objects
            .insert(Object::Bitmap { dib })
            .ok_or(Error::Exhausted)
    }
    fn state_references(state: &DcState, object: u32) -> bool {
        state.bitmap == Some(object)
            || state.palette == Some(object)
            || [state.brush, state.pen, state.font].contains(&object)
    }
    pub fn select(&mut self, dc: u32, object: u32) -> Result<u32, Error> {
        self.dc_state(dc)?;
        let kind = match self.objects.get(object) {
            Some(Object::Brush { .. }) => 0,
            Some(Object::Pen { .. }) => 1,
            Some(Object::Font { .. }) => 2,
            Some(Object::Bitmap { .. } | Object::StockBitmap { .. }) => 3,
            _ => return Err(Error::WrongType),
        };
        if kind == 3 {
            // A memory DC accepts only the depths the raster backend can
            // represent; Windows rejects 2-bpp and 16-bpp DDBs on a 32-bpp DC.
            if let Some(Object::Bitmap { dib }) = self.objects.get(object) {
                // DIB sections may use any supported depth; DDBs can only be
                // selected at the depths the raster backend represents.
                if !dib.is_section() && !matches!(dib.bit_count(), 1 | 8 | 32) {
                    return Err(Error::WrongType);
                }
            }
            let Some(Object::MemoryDc { stock }) = self.objects.get(dc) else {
                return Err(Error::WrongType);
            };
            if matches!(self.objects.get(object), Some(Object::StockBitmap { .. })) && object != *stock {
                return Err(Error::WrongType);
            }
            if self.dcs.iter().any(|(handle, value)| {
                *handle != dc
                    && (value.state.bitmap == Some(object)
                        || value.saved.iter().any(|state| state.bitmap == Some(object)))
            }) {
                return Err(Error::SelectedObject);
            }
        }
        let state = self.dc_state_mut(dc)?;
        let slot = match kind {
            0 => &mut state.brush,
            1 => &mut state.pen,
            2 => &mut state.font,
            _ => return Ok(state.bitmap.replace(object).unwrap()),
        };
        Ok(std::mem::replace(slot, object))
    }
    pub fn delete_metafile(&mut self, handle: u32) -> Result<(), Error> {
        match self.objects.get(handle) {
            Some(Object::Metafile { .. }) => {
                self.objects.remove(handle);
                Ok(())
            }
            _ => Err(Error::WrongType),
        }
    }
    pub fn delete_object(&mut self, object: u32) -> Result<(), Error> {
        if self.stock_objects.values().any(|handle| *handle == object) {
            return Ok(());
        }
        if !matches!(
            self.objects.get(object),
            Some(
                Object::Brush { .. }
                    | Object::Pen { .. }
                    | Object::Font { .. }
                    | Object::Bitmap { .. }
            )
        ) {
            return Err(Error::WrongType);
        }
        if self.dcs.values().any(|dc| {
            Self::state_references(&dc.state, object)
                || dc
                    .saved
                    .iter()
                    .any(|state| Self::state_references(state, object))
        }) {
            return Err(Error::SelectedObject);
        }
        self.objects.remove(object);
        self.patterns.remove(&object);
        self.dib_patterns.remove(&object);
        Ok(())
    }
    pub fn delete_dc(&mut self, dc: u32) -> Result<(), Error> {
        match self.objects.get(dc) {
            Some(Object::MemoryDc { stock }) => {
                let stock = *stock;
                self.objects.remove(stock);
            }
            Some(Object::WindowDc { .. } | Object::MetaDc { .. }) => {}
            _ => return Err(Error::WrongType),
        }
        self.deleted_dcs.insert(dc, self.object_type(dc).unwrap_or(3));
        self.dcs.remove(&dc);
        self.objects.remove(dc);
        Ok(())
    }
    pub fn references_object(&self, object: u32) -> bool {
        self.dcs.values().any(|dc| {
            Self::state_references(&dc.state, object)
                || dc
                    .saved
                    .iter()
                    .any(|state| Self::state_references(state, object))
        })
    }
    fn target_bounds(&self, dc: u32) -> Result<Rect, Error> {
        match self.objects.get(dc) {
            Some(Object::WindowDc { window, clip }) => {
                let surface = self.surfaces.get(window).ok_or(Error::InvalidHandle)?;
                Ok(intersect(
                    *clip,
                    Rect {
                        left: 0,
                        top: 0,
                        right: surface.width as i32,
                        bottom: surface.height as i32,
                    },
                ))
            }
            Some(Object::MetaDc { .. }) => Ok(Rect {
                left: 0,
                top: 0,
                right: 640,
                bottom: 480,
            }),
            Some(Object::MemoryDc { .. }) => {
                let bitmap = self.dc_state(dc)?.bitmap.ok_or(Error::WrongType)?;
                match self.objects.get(bitmap) {
                    Some(Object::Bitmap { dib }) => Ok(Rect {
                        left: 0,
                        top: 0,
                        right: dib.width as i32,
                        bottom: dib.height as i32,
                    }),
                    // The stock bitmap is the 1x1 monochrome default.
                    Some(Object::StockBitmap { .. }) => Ok(Rect {
                        left: 0,
                        top: 0,
                        right: 1,
                        bottom: 1,
                    }),
                    _ => Err(Error::UnsupportedOperation),
                }
            }
            _ => Err(Error::InvalidHandle),
        }
    }
    pub fn clip_bounds(&self, dc: u32) -> Result<Rect, Error> {
        let bounds = self.target_bounds(dc)?;
        Ok(self
            .dc_state(dc)?
            .clip
            .map_or(bounds, |clip| intersect(bounds, clip)))
    }
    pub fn intersect_clip(&mut self, dc: u32, rect: Rect) -> Result<u32, Error> {
        let clip = intersect(self.clip_bounds(dc)?, rect);
        self.dc_state_mut(dc)?.clip = Some(clip);
        Ok(if clip.left >= clip.right || clip.top >= clip.bottom {
            1
        } else {
            2
        })
    }
    fn read_row(
        &self,
        memory: &dyn MemoryAccess,
        dc: u32,
        x: u32,
        y: u32,
        output: &mut [u8],
    ) -> Result<(), Error> {
        match self.objects.get(dc) {
            Some(Object::WindowDc { window, .. }) => {
                let surface = self.surfaces.get(window).ok_or(Error::InvalidHandle)?;
                let offset = (y as usize * surface.width as usize + x as usize) * 4;
                output.copy_from_slice(
                    surface
                        .rgba
                        .get(offset..offset + output.len())
                        .ok_or(Error::InvalidDimensions)?,
                );
                Ok(())
            }
            Some(Object::MemoryDc { .. }) => {
                match self
                    .objects
                    .get(self.dc_state(dc)?.bitmap.ok_or(Error::WrongType)?)
                {
                    Some(Object::Bitmap { dib }) => Ok(dib.rgba_row(memory, x, y, output)?),
                    Some(Object::StockBitmap { bits }) if *bits != 0 => {
                        let dib = Dib::new(memory, 1, 1, *bits, 1)?;
                        Ok(dib.rgba_row(memory, x, y, output)?)
                    }
                    _ => Err(Error::UnsupportedOperation),
                }
            }
            _ => Err(Error::InvalidHandle),
        }
    }
    fn write_row(
        &mut self,
        memory: &mut dyn MemoryAccess,
        dc: u32,
        x: u32,
        y: u32,
        pixels: &[u8],
    ) -> Result<(), Error> {
        match self.objects.get(dc) {
            Some(Object::WindowDc { window, .. }) => {
                let surface = self.surfaces.get_mut(window).ok_or(Error::InvalidHandle)?;
                let offset = (y as usize * surface.width as usize + x as usize) * 4;
                surface
                    .rgba
                    .get_mut(offset..offset + pixels.len())
                    .ok_or(Error::InvalidDimensions)?
                    .copy_from_slice(pixels);
                surface.dirty = true;
                Ok(())
            }
            Some(Object::MemoryDc { .. }) => {
                match self
                    .objects
                    .get(self.dc_state(dc)?.bitmap.ok_or(Error::WrongType)?)
                {
                    Some(Object::Bitmap { dib }) => Ok(dib.write_rgba_row(memory, x, y, pixels)?),
                    Some(Object::StockBitmap { bits }) if *bits != 0 => {
                        let dib = Dib::new(memory, 1, 1, *bits, 1)?;
                        Ok(dib.write_rgba_row(memory, x, y, pixels)?)
                    }
                    _ => Err(Error::UnsupportedOperation),
                }
            }
            _ => Err(Error::InvalidHandle),
        }
    }
    pub fn fill_dc(
        &mut self,
        memory: &mut dyn MemoryAccess,
        dc: u32,
        rect: Rect,
        color: u32,
    ) -> Result<(), Error> {
        let rect = intersect(self.clip_bounds(dc)?, rect);
        if rect.left >= rect.right || rect.top >= rect.bottom {
            return Ok(());
        }
        let pixel = [
            (color & 255) as u8,
            ((color >> 8) & 255) as u8,
            ((color >> 16) & 255) as u8,
            255,
        ];
        let row = pixel.repeat((rect.right - rect.left) as usize);
        for y in rect.top..rect.bottom {
            self.write_row(memory, dc, rect.left as u32, y as u32, &row)?;
        }
        Ok(())
    }
    pub fn text_out(
        &mut self,
        memory: &mut dyn MemoryAccess,
        dc: u32,
        x: i32,
        y: i32,
        text: &[u8],
        options: u32,
        rect: Option<Rect>,
        advances: Option<&[i32]>,
    ) -> Result<(), Error> {
        if options & !6 != 0 || (options != 0 && rect.is_none()) {
            return Err(Error::UnsupportedOperation);
        }
        if advances.is_some_and(|values| values.len() != text.len()) {
            return Err(Error::InvalidDimensions);
        }
        let state = self.dc_state(dc)?.clone();
        let (mut x, mut y) = if state.text_align & 1 != 0 {
            state.current_position
        } else {
            (x, y)
        };
        let character_extra = state.text_extra;
        let mut widths: Vec<i32> = advances.map_or_else(
            || {
                text.iter()
                    .map(|byte| crate::font::extent(&[*byte]).0 as i32)
                    .collect()
            },
            |values| values.to_vec(),
        );
        if character_extra != 0 {
            for width in &mut widths {
                *width = width.saturating_add(character_extra);
            }
        }
        let width = widths
            .iter()
            .try_fold(0i32, |sum, value| sum.checked_add(*value))
            .ok_or(Error::InvalidDimensions)?;
        match state.text_align & 6 {
            0 => {}
            2 => x = x.checked_sub(width).ok_or(Error::InvalidDimensions)?,
            6 => x = x.checked_sub(width / 2).ok_or(Error::InvalidDimensions)?,
            _ => return Err(Error::UnsupportedOperation),
        }
        let metrics = crate::font::metrics();
        let height = crate::font::extent(b"").1 as i32;
        let ascent = u32::from_le_bytes(metrics[4..8].try_into().unwrap()) as i32;
        match state.text_align & 24 {
            0 => {}
            8 => y = y.checked_sub(height).ok_or(Error::InvalidDimensions)?,
            24 => y = y.checked_sub(ascent).ok_or(Error::InvalidDimensions)?,
            _ => return Err(Error::UnsupportedOperation),
        }
        if options & 2 != 0 {
            self.fill_dc(memory, dc, rect.unwrap(), state.background_color)?;
        }
        let mut clip = self.clip_bounds(dc)?;
        if options & 4 != 0 {
            clip = intersect(clip, rect.unwrap());
        }
        let text_right = x.checked_add(width).ok_or(Error::InvalidDimensions)?;
        let text_bottom = y.checked_add(height).ok_or(Error::InvalidDimensions)?;
        // Paint only the text background through ETO_CLIPPED; ETO_OPAQUE's
        // explicit rectangle is independent of the glyph bounding box.
        if state.background_mode == 2 && options & 2 == 0 {
            self.fill_dc(
                memory,
                dc,
                intersect(
                    clip,
                    Rect {
                        left: x,
                        top: y,
                        right: text_right,
                        bottom: text_bottom,
                    },
                ),
                state.background_color,
            )?;
        }
        let fg = [
            (state.text_color & 255) as u8,
            ((state.text_color >> 8) & 255) as u8,
            ((state.text_color >> 16) & 255) as u8,
            255,
        ];
        let mut cursor = x;
        for (byte, advance) in text.iter().zip(widths) {
            let glyph = crate::font::glyph(*byte);
            for gy in 0..16i32 {
                let py = y as i64 + gy as i64;
                let left = (cursor as i64).max(clip.left as i64);
                let right = (cursor as i64 + 16).min(clip.right as i64);
                if py < clip.top as i64 || py >= clip.bottom as i64 || left >= right {
                    continue;
                }
                let mut row = vec![0; (right - left) as usize * 4];
                self.read_row(memory, dc, left as u32, py as u32, &mut row)?;
                for (gx, pixel) in row.chunks_exact_mut(4).enumerate() {
                    if glyph[gy as usize * 16 + (left - cursor as i64) as usize + gx] >= 128 {
                        pixel.copy_from_slice(&fg);
                    }
                }
                self.write_row(memory, dc, left as u32, py as u32, &row)?;
            }
            cursor = cursor
                .checked_add(advance)
                .ok_or(Error::InvalidDimensions)?;
        }
        if state.text_align & 1 != 0 {
            self.dc_state_mut(dc)?.current_position.0 = text_right;
        }
        Ok(())
    }
    pub fn get_pixel(
        &self,
        memory: &dyn MemoryAccess,
        dc: u32,
        x: i32,
        y: i32,
    ) -> Result<u32, Error> {
        let clip = self.clip_bounds(dc)?;
        if x < clip.left || y < clip.top || x >= clip.right || y >= clip.bottom {
            return Err(Error::InvalidDimensions);
        }
        let mut pixel = [0; 4];
        self.read_row(memory, dc, x as u32, y as u32, &mut pixel)?;
        Ok(pixel[0] as u32 | ((pixel[1] as u32) << 8) | ((pixel[2] as u32) << 16))
    }
    pub fn set_pixel(
        &mut self,
        memory: &mut dyn MemoryAccess,
        dc: u32,
        x: i32,
        y: i32,
        color: u32,
    ) -> Result<u32, Error> {
        if color & 0xff000000 != 0 {
            return Err(Error::UnsupportedOperation);
        }
        self.get_pixel(memory, dc, x, y)?;
        self.write_row(
            memory,
            dc,
            x as u32,
            y as u32,
            &[
                (color & 255) as u8,
                ((color >> 8) & 255) as u8,
                ((color >> 16) & 255) as u8,
                255,
            ],
        )?;
        self.get_pixel(memory, dc, x, y)
    }
    fn geometry_pen(&self, dc: u32) -> Result<Option<GeometryPen>, Error> {
        match self.objects.get(self.dc_state(dc)?.pen) {
            Some(Object::Pen { style: 5, .. }) => Ok(None),
            Some(Object::Pen {
                ext: Some(ext), ..
            }) if (ext.style & 0xf) == 8 => Ok(Some(GeometryPen {
                color: ext.color,
                alternate: true,
            })),
            // A geometric pen with a solid brush draws with its colour. Thick
            // rasterization is not implemented yet, but the object is drawable.
            Some(Object::Pen {
                ext: Some(ext), ..
            }) if ext.brush_style == 0 => Ok(Some(GeometryPen {
                color: ext.color,
                alternate: false,
            })),
            Some(Object::Pen {
                style: 0,
                width: 0 | 1,
                color,
                ..
            }) => Ok(Some(GeometryPen {
                color: *color,
                alternate: false,
            })),
            _ => Err(Error::UnsupportedOperation),
        }
    }
    fn geometry_brush(&self, dc: u32) -> Result<Option<u32>, Error> {
        match self.objects.get(self.dc_state(dc)?.brush) {
            Some(Object::Brush { null: true, .. }) => Ok(None),
            Some(Object::Brush { null: false, color, .. }) => Ok(Some(*color)),
            _ => Err(Error::UnsupportedOperation),
        }
    }
    fn geometry_pixel(
        &mut self,
        memory: &mut dyn MemoryAccess,
        dc: u32,
        x: i32,
        y: i32,
        color: u32,
        mode: u32,
    ) -> Result<(), Error> {
        let mut pixel = [0; 4];
        self.read_row(memory, dc, x as u32, y as u32, &mut pixel)?;
        for channel in 0..3 {
            pixel[channel] = rop2(mode, (color >> (channel * 8)) as u8, pixel[channel]);
        }
        pixel[3] = 255;
        self.write_row(memory, dc, x as u32, y as u32, &pixel)
    }
    /// Cosmetic solid lines in MM_TEXT, excluding the final endpoint. Clip the
    /// major-axis walk before rasterizing so huge offscreen lines stay bounded.
    pub fn line_to(
        &mut self,
        memory: &mut dyn MemoryAccess,
        dc: u32,
        x: i32,
        y: i32,
    ) -> Result<(), Error> {
        let state = self.dc_state(dc)?.clone();
        let pen = self.geometry_pen(dc)?;
        if let Some(pen) = pen {
            let color = pen.color;
            let clip = self.clip_bounds(dc)?;
            let (x0, y0) = state.current_position;
            let dx = (x as i64 - x0 as i64).unsigned_abs();
            let dy = (y as i64 - y0 as i64).unsigned_abs();
            let step_x = (x as i64 - x0 as i64).signum();
            let step_y = (y as i64 - y0 as i64).signum();
            let major = dx.max(dy);
            let horizontal = dx >= dy;
            let (origin, step, low, high) = if horizontal {
                (x0 as i64, step_x, clip.left as i64, clip.right as i64)
            } else {
                (y0 as i64, step_y, clip.top as i64, clip.bottom as i64)
            };
            let (first, end) = if step >= 0 {
                ((low - origin).max(0), (high - origin).min(major as i64))
            } else {
                (
                    (origin - high + 1).max(0),
                    (origin - low + 1).min(major as i64),
                )
            };
            if major > 0 && clip.left < clip.right && clip.top < clip.bottom && first < end {
                for index in first as u64..end as u64 {
                    if pen.alternate && index % 2 == 1 {
                        continue;
                    }
                    let (px, py) = if horizontal {
                        (
                            x0 as i64 + step_x * index as i64,
                            y0 as i64 + step_y * ((index * dy + dx / 2) / dx) as i64,
                        )
                    } else {
                        (
                            x0 as i64 + step_x * ((index * dx + dy / 2) / dy) as i64,
                            y0 as i64 + step_y * index as i64,
                        )
                    };
                    if px >= clip.left as i64
                        && px < clip.right as i64
                        && py >= clip.top as i64
                        && py < clip.bottom as i64
                    {
                        self.geometry_pixel(memory, dc, px as i32, py as i32, color, state.rop2)?;
                    }
                }
            }
        }
        if state.bounds_enabled {
            let (x0, y0) = state.current_position;
            let (left, top) = self.lp_to_dp_point(dc, x0, y0)?;
            let (right, bottom) = self.lp_to_dp_point(dc, x, y)?;
            let inflate = self.pen_bounds_inflation(dc);
            self.add_bounds(
                dc,
                Rect {
                    left: left.min(right) - inflate,
                    top: top.min(bottom) - inflate,
                    right: left.max(right) + 1 + inflate,
                    bottom: top.max(bottom) + 1 + inflate,
                },
            );
        }
        self.dc_state_mut(dc)?.current_position = (x, y);
        Ok(())
    }
    pub fn rectangle(
        &mut self,
        memory: &mut dyn MemoryAccess,
        dc: u32,
        rect: Rect,
    ) -> Result<(), Error> {
        // A metafile DC records drawing instead of rasterizing; the record
        // itself is not exposed yet, and nothing is drawn.
        if self.is_meta_dc(dc).is_some() {
            return Ok(());
        }
        let state = self.dc_state(dc)?.clone();
        let pen = self.geometry_pen(dc)?.map(|pen| pen.color);
        let brush = self.geometry_brush(dc)?;
        if state.bounds_enabled {
            let (left, top) = self.lp_to_dp_point(dc, rect.left, rect.top)?;
            let (right, bottom) = self.lp_to_dp_point(dc, rect.right, rect.bottom)?;
            self.add_bounds(
                dc,
                Rect {
                    left,
                    top,
                    right,
                    bottom,
                },
            );
        }
        let clipped = intersect(self.clip_bounds(dc)?, rect);
        if clipped.left >= clipped.right || clipped.top >= clipped.bottom {
            return Ok(());
        }
        let mut row = vec![0; (clipped.right - clipped.left) as usize * 4];
        for y in clipped.top..clipped.bottom {
            self.read_row(memory, dc, clipped.left as u32, y as u32, &mut row)?;
            for (index, pixel) in row.chunks_exact_mut(4).enumerate() {
                let x = clipped.left + index as i32;
                let color = if pen.is_some()
                    && (x == rect.left
                        || y == rect.top
                        || x == rect.right - 1
                        || y == rect.bottom - 1)
                {
                    pen
                } else {
                    self.pattern_pixel(dc, state.brush, x, y)
                        .map(|p| u32::from(p[0]) | (u32::from(p[1]) << 8) | (u32::from(p[2]) << 16))
                        .or(brush)
                };
                if let Some(color) = color {
                    for channel in 0..3 {
                        pixel[channel] =
                            rop2(state.rop2, (color >> (channel * 8)) as u8, pixel[channel]);
                    }
                    pixel[3] = 255;
                }
            }
            self.write_row(memory, dc, clipped.left as u32, y as u32, &row)?;
        }
        Ok(())
    }
    /// Solid black/white raster operations for DIB and window DCs.
    pub fn pat_blt(
        &mut self,
        memory: &mut dyn MemoryAccess,
        dc: u32,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
        rop: u32,
    ) -> Result<(), Error> {
        let table = (rop >> 16) as u8;
        if rop == 0 || rop & 0xff00_0000 != 0 || (table & 0x33) != ((table >> 2) & 0x33) {
            return Err(Error::UnsupportedOperation);
        }
        self.bit_blt(memory, dc, x, y, width, height, dc, x, y, rop)
    }
    /// Stretch a source snapshot through the destination DC's stretch mode and
    /// ROP3, preserving rectangle mapping when the destination is clipped.
    pub fn stretch_blt(
        &mut self,
        memory: &mut dyn MemoryAccess,
        dst: u32,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
        src: u32,
        sx: i32,
        sy: i32,
        source_width: i32,
        source_height: i32,
        rop: u32,
    ) -> Result<(), Error> {
        if rop == 0 || rop & 0xff00_0000 != 0 {
            return Err(Error::UnsupportedOperation);
        }
        let axis_x = StretchAxis::new(x, width, sx, source_width)?;
        let axis_y = StretchAxis::new(y, height, sy, source_height)?;
        let mode = self.dc_state(dst)?.stretch_mode;
        if !(1..=4).contains(&mode) {
            return Err(Error::UnsupportedOperation);
        }
        let table = (rop >> 16) as u8;
        let needs_source = (table & 0x33) != ((table >> 2) & 0x33);
        let target = intersect(
            self.clip_bounds(dst)?,
            Rect {
                left: axis_x.destination_min,
                top: axis_y.destination_min,
                right: axis_x.destination_max,
                bottom: axis_y.destination_max,
            },
        );
        if target.left >= target.right || target.top >= target.bottom {
            return Ok(());
        }
        let source_stride = axis_x
            .source_count
            .checked_mul(4)
            .ok_or(Error::InvalidDimensions)? as usize;
        let mut source = Vec::new();
        if needs_source {
            let bounds = self.source_bounds(src)?;
            if axis_x.source_min < bounds.left
                || axis_y.source_min < bounds.top
                || axis_x.source_max > bounds.right
                || axis_y.source_max > bounds.bottom
            {
                return Err(Error::InvalidDimensions);
            }
            let len = source_stride
                .checked_mul(axis_y.source_count as usize)
                .ok_or(Error::InvalidDimensions)?;
            source
                .try_reserve_exact(len)
                .map_err(|_| Error::Exhausted)?;
            source.resize(len, 0);
            // Capture the entire source before the first write, including when
            // source and destination select the same DIB or window surface.
            for (index, output) in source.chunks_exact_mut(source_stride).enumerate() {
                let py = axis_y.source_coordinate(index as u32);
                self.read_row(memory, src, axis_x.source_min as u32, py as u32, output)?;
                if source_width < 0 {
                    let count = axis_x.source_count as usize;
                    for at in 0..count / 2 {
                        for channel in 0..4 {
                            output.swap(at * 4 + channel, (count - 1 - at) * 4 + channel);
                        }
                    }
                }
            }
        }
        let pattern = self.rop_pattern(dst, table)?;
        let brush = self.dc_state(dst)?.brush;
        let mut row = vec![0; (target.right - target.left) as usize * 4];
        for py in target.top..target.bottom {
            self.read_row(memory, dst, target.left as u32, py as u32, &mut row)?;
            let j = axis_y.destination_index(py);
            for (column, pixel) in row.chunks_exact_mut(4).enumerate() {
                let pattern = self.pattern_pixel(dst, brush, target.left + column as i32, py).unwrap_or(pattern);
                let i = axis_x.destination_index(target.left + column as i32);
                let sampled = if needs_source {
                    sample_stretch(&source, source_stride, &axis_x, &axis_y, i, j, mode)
                } else {
                    [0; 3]
                };
                for channel in 0..3 {
                    pixel[channel] =
                        rop3(table, pattern[channel], sampled[channel], pixel[channel]);
                }
                pixel[3] = 255;
            }
            self.write_row(memory, dst, target.left as u32, py as u32, &row)?;
        }
        Ok(())
    }
    fn source_bounds(&self, dc: u32) -> Result<Rect, Error> {
        if let Some(Object::WindowDc { window, .. }) = self.objects.get(dc) {
            let surface = self.surfaces.get(window).ok_or(Error::InvalidHandle)?;
            Ok(Rect {
                left: 0,
                top: 0,
                right: surface.width as i32,
                bottom: surface.height as i32,
            })
        } else {
            self.target_bounds(dc)
        }
    }
    fn rop_pattern(&self, dc: u32, table: u8) -> Result<[u8; 3], Error> {
        if (table & 15) == (table >> 4) {
            return Ok([0; 3]);
        }
        let brush = self.dc_state(dc)?.brush;
        if let Some(pixel) = self.pattern_pixel(dc, brush, 0, 0) { return Ok(pixel); }
        match self.objects.get(brush) {
            Some(Object::Brush { color, null: false, .. }) => Ok([
                (color & 255) as u8,
                ((color >> 8) & 255) as u8,
                ((color >> 16) & 255) as u8,
            ]),
            _ => Err(Error::UnsupportedOperation),
        }
    }
    pub fn stretch_dib(
        &mut self,
        memory: &mut dyn MemoryAccess,
        dst: u32,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
        sx: i32,
        sy: i32,
        source_width: i32,
        source_height: i32,
        bits: u32,
        header: u32,
        rop: u32,
    ) -> Result<(), Error> {
        let dib = Dib::from_header(memory, header, bits)?;
        // StretchDIBits source coordinates are measured from the DIB origin:
        // bottom-left for a positive biHeight, top-left for a negative height.
        let sy = if dib.top_down {
            sy
        } else {
            (dib.height as i64 - sy as i64 - source_height as i64)
                .try_into()
                .map_err(|_| Error::InvalidDimensions)?
        };
        let bitmap = self
            .objects
            .insert(Object::Bitmap { dib })
            .ok_or(Error::Exhausted)?;
        let dc = match self.memory_dc() {
            Ok(dc) => dc,
            Err(error) => {
                self.objects.remove(bitmap);
                return Err(error);
            }
        };
        let result = self.select(dc, bitmap).and_then(|_| {
            self.stretch_blt(
                memory,
                dst,
                x,
                y,
                width,
                height,
                dc,
                sx,
                sy,
                source_width,
                source_height,
                rop,
            )
        });
        self.delete_dc(dc)?;
        self.delete_object(bitmap)?;
        result
    }
    /// Copy through a ternary raster operation. Snapshot the source first so
    /// overlapping copies within a DIB or window do not smear their own input.
    pub fn bit_blt(
        &mut self,
        memory: &mut dyn MemoryAccess,
        dst: u32,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
        src: u32,
        sx: i32,
        sy: i32,
        rop: u32,
    ) -> Result<(), Error> {
        if rop == 0 || rop & 0xff00_0000 != 0 {
            return Err(Error::UnsupportedOperation);
        }
        if width < 0 || height < 0 {
            return Err(Error::InvalidDimensions);
        }
        let bounds = self.clip_bounds(dst)?;
        let table = (rop >> 16) as u8;
        let needs_source = (table & 0x33) != ((table >> 2) & 0x33);
        let source = if needs_source {
            Some(self.source_bounds(src)?)
        } else {
            None
        };
        let left = 0i64
            .max(bounds.left as i64 - x as i64)
            .max(source.map_or(0, |rect| rect.left as i64 - sx as i64));
        let top = 0i64
            .max(bounds.top as i64 - y as i64)
            .max(source.map_or(0, |rect| rect.top as i64 - sy as i64));
        let right = (width as i64)
            .min(bounds.right as i64 - x as i64)
            .min(source.map_or(width as i64, |rect| rect.right as i64 - sx as i64));
        let bottom = (height as i64)
            .min(bounds.bottom as i64 - y as i64)
            .min(source.map_or(height as i64, |rect| rect.bottom as i64 - sy as i64));
        if left >= right || top >= bottom {
            return Ok(());
        }
        let stride = (right - left) as usize * 4;
        let len = stride
            .checked_mul((bottom - top) as usize)
            .ok_or(Error::InvalidDimensions)?;
        let mut pixels = Vec::new();
        pixels
            .try_reserve_exact(len)
            .map_err(|_| Error::Exhausted)?;
        pixels.resize(len, 0);
        for (row, output) in pixels.chunks_exact_mut(stride).enumerate() {
            if needs_source {
                self.read_row(
                    memory,
                    src,
                    (sx as i64 + left) as u32,
                    (sy as i64 + top + row as i64) as u32,
                    output,
                )?;
            }
        }
        let pattern = self.rop_pattern(dst, table)?;
        let brush = self.dc_state(dst)?.brush;
        let mut destination = vec![0; stride];
        for (row, input) in pixels.chunks_exact(stride).enumerate() {
            let dx = (x as i64 + left) as u32;
            let dy = (y as i64 + top + row as i64) as u32;
            self.read_row(memory, dst, dx, dy, &mut destination)?;
            for (column, (source, target)) in input.chunks_exact(4).zip(destination.chunks_exact_mut(4)).enumerate() {
                let pattern = self.pattern_pixel(dst, brush, dx as i32 + column as i32, dy as i32).unwrap_or(pattern);
                for channel in 0..3 {
                    target[channel] =
                        rop3(table, pattern[channel], source[channel], target[channel]);
                }
                target[3] = 255;
            }
            self.write_row(memory, dst, dx, dy, &destination)?;
        }
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unchanged_surface_size_preserves_window_pixels() {
        let mut gdi = Gdi::default();
        gdi.create_surface(1, 4, 3).unwrap();
        gdi.fill_surface(1, Rect { left: 0, top: 0, right: 4, bottom: 3 }, 0x123456).unwrap();
        let expected = gdi.surface(1).unwrap().rgba.clone();
        gdi.resize_surface(1, 4, 3).unwrap();
        assert_eq!(gdi.surface(1).unwrap().rgba, expected);
        gdi.resize_surface(1, 6, 4).unwrap();
        let enlarged = gdi.surface(1).unwrap();
        for row in 0..3 { assert_eq!(&enlarged.rgba[row * 24..row * 24 + 16], &expected[row * 16..row * 16 + 16]); }
        gdi.resize_surface(1, 2, 2).unwrap();
        assert_eq!(&gdi.surface(1).unwrap().rgba[..8], &expected[..8]);
    }
    #[test]
    fn dib_pattern_brush_maps_indices_through_the_selected_palette() {
        fn put(storage: &mut [u8], at: usize, value: u32) {
            storage[at..at + 4].copy_from_slice(&value.to_le_bytes());
        }
        let mut storage = vec![0u8; 1024];
        put(&mut storage, 0, 40); // biSize
        put(&mut storage, 4, 2); // width
        put(&mut storage, 8, 1); // height
        storage[12..14].copy_from_slice(&1u16.to_le_bytes());
        storage[14..16].copy_from_slice(&8u16.to_le_bytes());
        put(&mut storage, 16, 0); // BI_RGB
        for index in 0..256usize {
            storage[40 + index * 2..40 + index * 2 + 2]
                .copy_from_slice(&((255 - index) as u16).to_le_bytes());
        }
        storage[40 + 512] = 0;
        storage[40 + 513] = 1;
        let mut memory = crate::GuestMemory::new(&mut storage);
        let mut gdi = Gdi::default();
        let brush = gdi.dib_pattern_brush(&memory, 0, 1).unwrap();
        let dc = gdi.memory_dc().unwrap();
        gdi.dc_state_mut(dc).unwrap().palette = Some(0x1234);
        let palette: Vec<[u8; 4]> = (0..256)
            .map(|index| [(index * 2) as u8, (index * 2) as u8, (index * 2) as u8, 255])
            .collect();
        gdi.set_palette_colors(0x1234, palette);
        assert_eq!(gdi.pattern_pixel(dc, brush, 0, 0), Some([254, 254, 254]));
        assert_eq!(gdi.pattern_pixel(dc, brush, 1, 0), Some([252, 252, 252]));
        assert_eq!(gdi.pattern_pixel(dc, brush, 2, 0), Some([254, 254, 254]));
        let _ = &mut memory;
    }

    #[test]
    fn dib_pattern_brush_flips_bottom_up_dib_rows() {
        fn put(storage: &mut [u8], at: usize, value: u32) {
            storage[at..at + 4].copy_from_slice(&value.to_le_bytes());
        }
        let mut storage = vec![0u8; 1024];
        put(&mut storage, 0, 40);
        put(&mut storage, 4, 1); // width
        put(&mut storage, 8, 2); // height, bottom-up
        storage[12..14].copy_from_slice(&1u16.to_le_bytes());
        storage[14..16].copy_from_slice(&8u16.to_le_bytes());
        put(&mut storage, 16, 0);
        for index in 0..256usize {
            storage[40 + index * 2..40 + index * 2 + 2]
                .copy_from_slice(&(index as u16).to_le_bytes());
        }
        // Bottom-up memory row 0 is the bottom of the pattern.
        storage[40 + 512] = 10;
        storage[40 + 512 + 4] = 20;
        let memory = crate::GuestMemory::new(&mut storage);
        let mut gdi = Gdi::default();
        let brush = gdi.dib_pattern_brush(&memory, 0, 1).unwrap();
        let dc = gdi.memory_dc().unwrap();
        gdi.dc_state_mut(dc).unwrap().palette = Some(0x1234);
        let palette: Vec<[u8; 4]> = (0..256)
            .map(|index| [(index * 3) as u8, (index * 2) as u8, (index * 5) as u8, 255])
            .collect();
        gdi.set_palette_colors(0x1234, palette);
        assert_eq!(gdi.pattern_pixel(dc, brush, 0, 0), Some([60, 40, 100]));
        assert_eq!(gdi.pattern_pixel(dc, brush, 0, 1), Some([30, 20, 50]));
    }

    #[test]
    fn pattern_brush_snapshots_pixels_and_keeps_device_phase_when_clipped() {
        let mut storage = vec![0; 64];
        storage[..16].copy_from_slice(&[0,0,255,0, 0,255,0,0, 255,0,0,0, 255,255,255,0]);
        let mut memory = crate::GuestMemory::new(&mut storage);
        let mut gdi = Gdi::default();
        let bitmap = gdi.bitmap_pixels(&memory, 2, 2, 0, 32).unwrap();
        let brush = gdi.pattern_brush(&memory, bitmap).unwrap();
        memory.write(0, &[0;16]).unwrap();
        gdi.delete_object(bitmap).unwrap();
        gdi.create_surface(1,4,3).unwrap();
        let dc = gdi.paint_dc(1,Rect {left:1,top:1,right:4,bottom:3}).unwrap();
        gdi.select(dc,brush).unwrap();
        gdi.pat_blt(&mut memory,dc,-1,0,5,3,0x00f0_0021).unwrap();
        let surface = gdi.surface(1).unwrap();
        let pixel = |x:usize,y:usize| &surface.rgba[(y*4+x)*4..(y*4+x+1)*4];
        assert_eq!(pixel(1,1),[255,255,255,255]);
        assert_eq!(pixel(2,1),[0,0,255,255]);
        assert_eq!(pixel(1,2),[0,255,0,255]);
        assert_eq!(pixel(2,2),[255,0,0,255]);
    }
    #[test]
    fn scaling_filters_have_distinct_results_and_clipping_preserves_mapping() {
        let mut storage = [0; 1];
        let mut memory = crate::GuestMemory::new(&mut storage);
        let mut gdi = Gdi::default();
        gdi.create_surface(1, 3, 1).unwrap();
        gdi.create_surface(2, 4, 1).unwrap();
        let source = gdi
            .paint_dc(
                1,
                Rect {
                    left: 0,
                    top: 0,
                    right: 3,
                    bottom: 1,
                },
            )
            .unwrap();
        let target = gdi
            .paint_dc(
                2,
                Rect {
                    left: 0,
                    top: 0,
                    right: 4,
                    bottom: 1,
                },
            )
            .unwrap();
        gdi.present_rgba(1, &[0, 0, 0, 255, 100, 100, 100, 255, 200, 200, 200, 255])
            .unwrap();
        gdi.dc_state_mut(target).unwrap().stretch_mode = 4;
        gdi.stretch_blt(&mut memory, target, 0, 0, 2, 1, source, 0, 0, 3, 1, SRCCOPY)
            .unwrap();
        assert_eq!(
            &gdi.surface(2).unwrap().rgba[..8],
            &[33, 33, 33, 255, 167, 167, 167, 255]
        );
        for (mode, expected) in [
            (1, vec![0, 0, 0, 255, 64, 64, 64, 255]),
            (2, vec![0, 0, 0, 255, 236, 236, 236, 255]),
            (3, vec![0, 0, 0, 255, 200, 200, 200, 255]),
        ] {
            gdi.dc_state_mut(target).unwrap().stretch_mode = mode;
            gdi.stretch_blt(&mut memory, target, 0, 0, 2, 1, source, 0, 0, 3, 1, SRCCOPY)
                .unwrap();
            assert_eq!(&gdi.surface(2).unwrap().rgba[..8], expected);
        }
        gdi.present_rgba(2, &[7; 16]).unwrap();
        gdi.intersect_clip(
            target,
            Rect {
                left: 1,
                top: 0,
                right: 3,
                bottom: 1,
            },
        )
        .unwrap();
        gdi.stretch_blt(&mut memory, target, 0, 0, 4, 1, source, 0, 0, 3, 1, SRCCOPY)
            .unwrap();
        assert_eq!(
            gdi.surface(2).unwrap().rgba,
            [7, 7, 7, 255, 100, 100, 100, 255, 100, 100, 100, 255, 7, 7, 7, 255]
        );
    }
    #[test]
    fn signed_stretch_extents_and_overflow_are_bounded_before_writes() {
        let mut storage = [0; 1];
        let mut memory = crate::GuestMemory::new(&mut storage);
        let mut gdi = Gdi::default();
        gdi.create_surface(1, 2, 2).unwrap();
        gdi.create_surface(2, 2, 2).unwrap();
        let clip = Rect {
            left: 0,
            top: 0,
            right: 2,
            bottom: 2,
        };
        let src = gdi.paint_dc(1, clip).unwrap();
        let dst = gdi.paint_dc(2, clip).unwrap();
        gdi.dc_state_mut(dst).unwrap().stretch_mode = 3;
        let colors = [1, 2, 3, 255, 4, 5, 6, 255, 7, 8, 9, 255, 10, 11, 12, 255];
        gdi.present_rgba(1, &colors).unwrap();
        gdi.stretch_blt(&mut memory, dst, 0, 0, 2, 2, src, 2, 2, -2, -2, SRCCOPY)
            .unwrap();
        assert_eq!(
            gdi.surface(2).unwrap().rgba,
            [10, 11, 12, 255, 7, 8, 9, 255, 4, 5, 6, 255, 1, 2, 3, 255]
        );
        gdi.stretch_blt(&mut memory, dst, 2, 2, -2, -2, src, 2, 2, -2, -2, SRCCOPY)
            .unwrap();
        assert_eq!(gdi.surface(2).unwrap().rgba, colors);
        let before = gdi.surface(2).unwrap().rgba.clone();
        for (x, width) in [(i32::MAX, 2), (0, i32::MIN), (0, 0)] {
            assert_eq!(
                gdi.stretch_blt(&mut memory, dst, x, 0, width, 2, src, 0, 0, 2, 2, SRCCOPY),
                Err(Error::InvalidDimensions)
            );
        }
        assert_eq!(gdi.surface(2).unwrap().rgba, before);
    }
    #[test]
    fn saved_selections_keep_objects_alive_and_fonts_can_be_shared() {
        let mut bytes = vec![0; 128];
        bytes[0..4].copy_from_slice(&40u32.to_le_bytes());
        bytes[4..8].copy_from_slice(&2u32.to_le_bytes());
        bytes[8..12].copy_from_slice(&(-1i32).to_le_bytes());
        bytes[12..14].copy_from_slice(&1u16.to_le_bytes());
        bytes[14..16].copy_from_slice(&32u16.to_le_bytes());
        let memory = crate::GuestMemory::new(&mut bytes);
        let mut gdi = Gdi::default();
        let a = gdi.memory_dc().unwrap();
        let b = gdi.memory_dc().unwrap();
        let font = gdi.font(-14, 0, 400, false, b"System").unwrap();
        let old_a = gdi.select(a, font).unwrap();
        let old_b = gdi.select(b, font).unwrap();
        assert_eq!(gdi.delete_object(font), Err(Error::SelectedObject));
        gdi.select(a, old_a).unwrap();
        assert_eq!(gdi.delete_object(font), Err(Error::SelectedObject));
        gdi.select(b, old_b).unwrap();
        gdi.delete_object(font).unwrap();
        let bitmap = gdi.bitmap(&memory, 0, 64).unwrap();
        let stock = gdi.select(a, bitmap).unwrap();
        let saved = gdi.save_dc(a).unwrap();
        gdi.select(a, stock).unwrap();
        assert_eq!(gdi.delete_object(bitmap), Err(Error::SelectedObject));
        assert_eq!(gdi.select(b, bitmap), Err(Error::SelectedObject));
        assert!(gdi.restore_dc(a, saved as i32).unwrap());
        assert_eq!(gdi.dc_state(a).unwrap().bitmap, Some(bitmap));
        assert!(!gdi.restore_dc(a, 0).unwrap());
        assert!(!gdi.restore_dc(a, -1).unwrap());
        gdi.select(a, stock).unwrap();
        gdi.select(b, bitmap).unwrap();
        gdi.delete_dc(b).unwrap();
        gdi.delete_object(bitmap).unwrap();
    }
    #[test]
    fn text_opaque_and_clipped_glyphs_agree_on_window_and_dib_targets() {
        let mut bytes = vec![0; 4096];
        bytes[0..4].copy_from_slice(&40u32.to_le_bytes());
        bytes[4..8].copy_from_slice(&16u32.to_le_bytes());
        bytes[8..12].copy_from_slice(&(-16i32).to_le_bytes());
        bytes[12..14].copy_from_slice(&1u16.to_le_bytes());
        bytes[14..16].copy_from_slice(&32u16.to_le_bytes());
        let mut memory = crate::GuestMemory::new(&mut bytes);
        let mut gdi = Gdi::default();
        gdi.create_surface(1, 16, 16).unwrap();
        let window = gdi
            .paint_dc(
                1,
                Rect {
                    left: 0,
                    top: 0,
                    right: 16,
                    bottom: 16,
                },
            )
            .unwrap();
        let dc = gdi.memory_dc().unwrap();
        let bitmap = gdi.bitmap(&memory, 0, 64).unwrap();
        gdi.select(dc, bitmap).unwrap();
        let rect = Rect {
            left: 2,
            top: 2,
            right: 8,
            bottom: 10,
        };
        for target in [window, dc] {
            let state = gdi.dc_state_mut(target).unwrap();
            state.text_color = 0xffffff;
            state.background_color = 0x0000ff00;
            gdi.text_out(&mut memory, target, 0, 0, b"M", 6, Some(rect), None)
                .unwrap();
        }
        let mut rgba = [0; 64];
        for y in 0..16 {
            gdi.read_row(&memory, dc, 0, y, &mut rgba).unwrap();
            assert_eq!(
                &rgba,
                &gdi.surface(1).unwrap().rgba[y as usize * 64..y as usize * 64 + 64]
            );
            for x in 0..16 {
                if !(2..8).contains(&x) || !(2..10).contains(&y) {
                    assert_eq!(&rgba[x as usize * 4..x as usize * 4 + 4], &[0, 0, 0, 255]);
                }
            }
        }
        assert!(gdi
            .surface(1)
            .unwrap()
            .rgba
            .chunks_exact(4)
            .any(|pixel| pixel == [255, 255, 255, 255]));
    }
    #[test]
    fn solid_pat_blt_respects_window_clip_and_rejects_other_rops() {
        let mut bytes = vec![0; 1];
        let mut memory = crate::GuestMemory::new(&mut bytes);
        let mut gdi = Gdi::default();
        gdi.create_surface(1, 2, 1).unwrap();
        let dc = gdi
            .paint_dc(
                1,
                Rect {
                    left: 1,
                    top: 0,
                    right: 2,
                    bottom: 1,
                },
            )
            .unwrap();
        gdi.pat_blt(&mut memory, dc, -1, 0, 5, 1, 0xff0062).unwrap();
        assert_eq!(
            gdi.surface(1).unwrap().rgba,
            [0, 0, 0, 255, 255, 255, 255, 255]
        );
        assert_eq!(
            gdi.pat_blt(&mut memory, dc, 0, 0, 2, 1, SRCCOPY),
            Err(Error::UnsupportedOperation)
        );
        gdi.pat_blt(&mut memory, dc, 0, 0, 2, 1, 0x42).unwrap();
        assert_eq!(gdi.surface(1).unwrap().rgba, [0, 0, 0, 255, 0, 0, 0, 255]);
    }
    #[test]
    fn clipped_dib_blit_and_object_lifetimes() {
        let mut bytes = vec![0; 128];
        bytes[0..4].copy_from_slice(&40u32.to_le_bytes());
        bytes[4..8].copy_from_slice(&2u32.to_le_bytes());
        bytes[8..12].copy_from_slice(&(-1i32).to_le_bytes());
        bytes[12..14].copy_from_slice(&1u16.to_le_bytes());
        bytes[14..16].copy_from_slice(&32u16.to_le_bytes());
        bytes[64..72].copy_from_slice(&[1, 2, 3, 0, 4, 5, 6, 0]);
        let mut memory = crate::GuestMemory::new(&mut bytes);
        let mut gdi = Gdi::default();
        gdi.create_surface(1, 2, 1).unwrap();
        let src = gdi.memory_dc().unwrap();
        let bitmap = gdi.bitmap(&memory, 0, 64).unwrap();
        let stock = gdi.select(src, bitmap).unwrap();
        assert_eq!(gdi.delete_object(bitmap), Err(Error::SelectedObject));
        let dst = gdi
            .paint_dc(
                1,
                Rect {
                    left: 0,
                    top: 0,
                    right: 1,
                    bottom: 1,
                },
            )
            .unwrap();
        gdi.acknowledge_present(1).unwrap();
        assert_eq!(
            gdi.bit_blt(&mut memory, dst, 0, 0, 2, 1, src, 0, 0, 0),
            Err(Error::UnsupportedOperation)
        );
        assert!(!gdi.surface(1).unwrap().dirty);
        gdi.bit_blt(&mut memory, dst, 0, 0, 2, 1, src, 0, 0, SRCCOPY)
            .unwrap();
        assert_eq!(gdi.surface(1).unwrap().rgba, [3, 2, 1, 255, 0, 0, 0, 255]);
        gdi.select(src, stock).unwrap();
        gdi.delete_object(bitmap).unwrap();
        assert_eq!(gdi.select(src, bitmap), Err(Error::WrongType));
        gdi.delete_dc(src).unwrap();
        gdi.delete_dc(dst).unwrap();
    }
}
