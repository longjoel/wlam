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
    },
    Pen {
        style: u32,
        width: u32,
        color: u32,
    },
    Bitmap {
        dib: Dib,
    },
    Font {
        height: i32,
        width: i32,
        weight: u32,
        italic: bool,
    },
    StockBitmap,
    MemoryDc {
        stock: u32,
    },
    WindowDc {
        window: u32,
        clip: Rect,
    },
}
#[derive(Debug, Clone)]
pub struct DcState {
    pub text_color: u32,
    pub background_color: u32,
    pub background_mode: u32,
    pub text_align: u32,
    pub stretch_mode: u32,
    pub rop2: u32,
    pub current_position: (i32, i32),
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
pub struct Gdi {
    objects: Handles<Object>,
    surfaces: BTreeMap<u32, Surface>,
    stock_objects: BTreeMap<u32, u32>,
    dcs: BTreeMap<u32, Dc>,
}
impl Default for Gdi {
    fn default() -> Self {
        Self {
            objects: Handles::with_namespace(2),
            surfaces: BTreeMap::new(),
            stock_objects: BTreeMap::new(),
            dcs: BTreeMap::new(),
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
                    })
                    .ok_or(Error::Exhausted)?
            }
            6 => self
                .objects
                .insert(Object::Pen {
                    style: 0,
                    width: 1,
                    color: 0xffffff,
                })
                .ok_or(Error::Exhausted)?,
            7 => self
                .objects
                .insert(Object::Pen {
                    style: 0,
                    width: 1,
                    color: 0,
                })
                .ok_or(Error::Exhausted)?,
            8 => self
                .objects
                .insert(Object::Pen {
                    style: 5,
                    width: 1,
                    color: 0,
                })
                .ok_or(Error::Exhausted)?,
            10..=14 | 16 | 17 => self
                .objects
                .insert(Object::Font {
                    height: -14,
                    width: 0,
                    weight: 400,
                    italic: false,
                })
                .ok_or(Error::Exhausted)?,
            _ => return Err(Error::InvalidHandle),
        };
        self.stock_objects.insert(index, handle);
        Ok(handle)
    }
    pub fn resize_surface(&mut self, window: u32, width: u32, height: u32) -> Result<(), Error> {
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
        if style > 6 || (width as i32) < 0 {
            return Err(Error::UnsupportedOperation);
        }
        self.objects
            .insert(Object::Pen {
                style,
                width: width.max(1),
                color: color & 0xffffff,
            })
            .ok_or(Error::Exhausted)
    }
    pub fn solid_brush(&mut self, color: u32) -> Result<u32, Error> {
        self.objects
            .insert(Object::Brush {
                color: color & 0xffffff,
                null: false,
            })
            .ok_or(Error::Exhausted)
    }
    pub fn font(
        &mut self,
        height: i32,
        width: i32,
        weight: u32,
        italic: bool,
    ) -> Result<u32, Error> {
        self.objects
            .insert(Object::Font {
                height,
                width,
                weight,
                italic,
            })
            .ok_or(Error::Exhausted)
    }
    pub fn is_dc(&self, handle: u32) -> bool {
        matches!(
            self.objects.get(handle),
            Some(Object::MemoryDc { .. } | Object::WindowDc { .. })
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
                    stretch_mode: 1,
                    rop2: 13,
                    current_position: (0, 0),
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
        dc.saved.push(dc.state.clone());
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
        dc.state = dc.saved[index as usize].clone();
        dc.saved.truncate(index as usize);
        Ok(true)
    }
    pub fn memory_dc(&mut self) -> Result<u32, Error> {
        let stock = self
            .objects
            .insert(Object::StockBitmap)
            .ok_or(Error::Exhausted)?;
        let handle = self
            .objects
            .insert(Object::MemoryDc { stock })
            .ok_or(Error::Exhausted)?;
        self.init_dc(handle, Some(stock))
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
            Some(Object::Bitmap { .. } | Object::StockBitmap) => 3,
            _ => return Err(Error::WrongType),
        };
        if kind == 3 {
            let Some(Object::MemoryDc { stock }) = self.objects.get(dc) else {
                return Err(Error::WrongType);
            };
            if matches!(self.objects.get(object), Some(Object::StockBitmap)) && object != *stock {
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
        Ok(())
    }
    pub fn delete_dc(&mut self, dc: u32) -> Result<(), Error> {
        match self.objects.get(dc) {
            Some(Object::MemoryDc { stock }) => {
                let stock = *stock;
                self.objects.remove(stock);
            }
            Some(Object::WindowDc { .. }) => {}
            _ => return Err(Error::WrongType),
        }
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
            Some(Object::MemoryDc { .. }) => {
                let bitmap = self.dc_state(dc)?.bitmap.ok_or(Error::WrongType)?;
                match self.objects.get(bitmap) {
                    Some(Object::Bitmap { dib }) => Ok(Rect {
                        left: 0,
                        top: 0,
                        right: dib.width as i32,
                        bottom: dib.height as i32,
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
                let Some(Object::Bitmap { dib }) = self
                    .objects
                    .get(self.dc_state(dc)?.bitmap.ok_or(Error::WrongType)?)
                else {
                    return Err(Error::UnsupportedOperation);
                };
                Ok(dib.rgba_row(memory, x, y, output)?)
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
                let Some(Object::Bitmap { dib }) = self
                    .objects
                    .get(self.dc_state(dc)?.bitmap.ok_or(Error::WrongType)?)
                else {
                    return Err(Error::UnsupportedOperation);
                };
                Ok(dib.write_rgba_row(memory, x, y, pixels)?)
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
        let widths: Vec<i32> = advances.map_or_else(
            || {
                text.iter()
                    .map(|byte| crate::font::extent(&[*byte]).0 as i32)
                    .collect()
            },
            |values| values.to_vec(),
        );
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
            if let Some(Object::WindowDc { window, .. }) = self.objects.get(src) {
                let surface = self.surfaces.get(window).ok_or(Error::InvalidHandle)?;
                Rect {
                    left: 0,
                    top: 0,
                    right: surface.width as i32,
                    bottom: surface.height as i32,
                }
            } else {
                self.target_bounds(src)?
            }
        } else {
            Rect {
                left: i32::MIN,
                top: i32::MIN,
                right: i32::MAX,
                bottom: i32::MAX,
            }
        };
        let left = 0i64
            .max(bounds.left as i64 - x as i64)
            .max(source.left as i64 - sx as i64);
        let top = 0i64
            .max(bounds.top as i64 - y as i64)
            .max(source.top as i64 - sy as i64);
        let right = (width as i64)
            .min(bounds.right as i64 - x as i64)
            .min(source.right as i64 - sx as i64);
        let bottom = (height as i64)
            .min(bounds.bottom as i64 - y as i64)
            .min(source.bottom as i64 - sy as i64);
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
        let needs_pattern = (table & 15) != (table >> 4);
        let pattern = if needs_pattern {
            match self.objects.get(self.dc_state(dst)?.brush) {
                Some(Object::Brush { color, null: false }) => [
                    (color & 255) as u8,
                    ((color >> 8) & 255) as u8,
                    ((color >> 16) & 255) as u8,
                ],
                _ => return Err(Error::UnsupportedOperation),
            }
        } else {
            [0; 3]
        };
        let mut destination = vec![0; stride];
        for (row, input) in pixels.chunks_exact(stride).enumerate() {
            let dx = (x as i64 + left) as u32;
            let dy = (y as i64 + top + row as i64) as u32;
            self.read_row(memory, dst, dx, dy, &mut destination)?;
            for (source, target) in input.chunks_exact(4).zip(destination.chunks_exact_mut(4)) {
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
        let font = gdi.font(-14, 0, 400, false).unwrap();
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
