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
#[derive(Debug, Clone, Copy)]
pub struct Rect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}
#[derive(Debug)]
enum Object {
    Brush { color: u32 },
    Pen { style: u32, width: u32, color: u32 },
    Bitmap { dib: Dib, selected: Option<u32> },
    Font { height: i32, width: i32, weight: u32, italic: bool, selected: Option<u32> },
    StockBitmap,
    MemoryDc { bitmap: u32, stock: u32 },
    WindowDc { window: u32, clip: Rect },
}
#[derive(Debug)]
pub struct Gdi {
    objects: Handles<Object>,
    surfaces: BTreeMap<u32, Surface>,
    stock_objects: BTreeMap<u32, u32>,
    selected_fonts: BTreeMap<u32, u32>,
}
impl Default for Gdi {
    fn default() -> Self {
        Self {
            objects: Handles::with_namespace(2),
            surfaces: BTreeMap::new(),
            stock_objects: BTreeMap::new(),
            selected_fonts: BTreeMap::new(),
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
                    .insert(Object::Brush { color })
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
            17 => self.objects.insert(Object::Font {height:-14,width:0,weight:400,italic:false,selected:None}).ok_or(Error::Exhausted)?,
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
            })
            .ok_or(Error::Exhausted)
    }
    pub fn font(&mut self,height:i32,width:i32,weight:u32,italic:bool)->Result<u32,Error>{
        self.objects.insert(Object::Font {height,width,weight,italic,selected:None}).ok_or(Error::Exhausted)
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
    pub fn memory_dc(&mut self) -> Result<u32, Error> {
        let stock = self
            .objects
            .insert(Object::StockBitmap)
            .ok_or(Error::Exhausted)?;
        match self.objects.insert(Object::MemoryDc {
            bitmap: stock,
            stock,
        }) {
            Some(dc) => Ok(dc),
            None => {
                self.objects.remove(stock);
                Err(Error::Exhausted)
            }
        }
    }
    pub fn paint_dc(&mut self, window: u32, clip: Rect) -> Result<u32, Error> {
        self.surfaces.get(&window).ok_or(Error::InvalidHandle)?;
        self.objects
            .insert(Object::WindowDc { window, clip })
            .ok_or(Error::Exhausted)
    }
    pub fn bitmap(
        &mut self,
        memory: &dyn MemoryAccess,
        header: u32,
        bits: u32,
    ) -> Result<u32, Error> {
        let dib = Dib::from_header(memory, header, bits)?;
        self.objects
            .insert(Object::Bitmap {
                dib,
                selected: None,
            })
            .ok_or(Error::Exhausted)
    }
    pub fn select(&mut self, dc: u32, bitmap: u32) -> Result<u32, Error> {
        if matches!(self.objects.get(dc),Some(Object::MemoryDc {..}|Object::WindowDc {..}))
            && matches!(self.objects.get(bitmap),Some(Object::Font {..})) {
            let default=self.stock_object(17)?;
            let previous=self.selected_fonts.get(&dc).copied().unwrap_or(default);
            if previous!=bitmap {
                if let Some(Object::Font {selected,..})=self.objects.get_mut(previous){*selected=None;}
                if let Some(Object::Font {selected,..})=self.objects.get_mut(bitmap){*selected=Some(dc);}
                self.selected_fonts.insert(dc,bitmap);
            }
            return Ok(previous)
        }
        let (previous, stock) = match self.objects.get(dc) {
            Some(Object::MemoryDc { bitmap, stock }) => (*bitmap, *stock),
            _ => return Err(Error::WrongType),
        };
        if bitmap == previous {
            return Ok(previous);
        }
        match self.objects.get(bitmap) {
            Some(Object::Bitmap { selected: None, .. }) => {}
            Some(Object::StockBitmap) if bitmap == stock => {}
            Some(Object::Bitmap { .. }) => return Err(Error::SelectedObject),
            _ => return Err(Error::WrongType),
        }
        if let Some(Object::Bitmap { selected, .. }) = self.objects.get_mut(previous) {
            *selected = None;
        }
        if let Some(Object::Bitmap { selected, .. }) = self.objects.get_mut(bitmap) {
            *selected = Some(dc);
        }
        if let Some(Object::MemoryDc {
            bitmap: current, ..
        }) = self.objects.get_mut(dc)
        {
            *current = bitmap;
        }
        Ok(previous)
    }
    pub fn delete_object(&mut self, bitmap: u32) -> Result<(), Error> {
        match self.objects.get(bitmap) {
            Some(Object::Brush { .. } | Object::Pen { .. }) => {}
            Some(Object::Font { selected: None, .. }) => {}
            Some(Object::Font { .. }) => return Err(Error::SelectedObject),
            Some(Object::Bitmap { selected: None, .. }) => {}
            Some(Object::Bitmap { .. }) => return Err(Error::SelectedObject),
            _ => return Err(Error::WrongType),
        }
        self.objects.remove(bitmap);
        Ok(())
    }
    pub fn delete_dc(&mut self, dc: u32) -> Result<(), Error> {
        match self.objects.get(dc) {
            Some(Object::MemoryDc { bitmap, stock }) => {
                let (bitmap, stock) = (*bitmap, *stock);
                if let Some(Object::Bitmap { selected, .. }) = self.objects.get_mut(bitmap) {
                    *selected = None;
                }
                self.objects.remove(stock);
            }
            Some(Object::WindowDc { .. }) => {}
            _ => return Err(Error::WrongType),
        }
        self.objects.remove(dc);
        Ok(())
    }
    /// First interactive slice: a selected DIB copied into a clipped window DC.
    /// Unsupported destinations are refused rather than silently approximated.
    pub fn bit_blt(
        &mut self,
        memory: &dyn MemoryAccess,
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
        if rop != SRCCOPY {
            return Err(Error::UnsupportedOperation);
        }
        if width < 0 || height < 0 {
            return Err(Error::InvalidDimensions);
        }
        let bitmap = match self.objects.get(src) {
            Some(Object::MemoryDc { bitmap, .. }) => *bitmap,
            _ => return Err(Error::WrongType),
        };
        let dib = match self.objects.get(bitmap) {
            Some(Object::Bitmap { dib, .. }) => dib.clone(),
            _ => return Err(Error::UnsupportedOperation),
        };
        let (window, clip) = match self.objects.get(dst) {
            Some(Object::WindowDc { window, clip }) => (*window, *clip),
            _ => return Err(Error::UnsupportedOperation),
        };
        let surface = self.surfaces.get(&window).ok_or(Error::InvalidHandle)?;
        let left = 0i64
            .max(-(x as i64))
            .max(-(sx as i64))
            .max(clip.left as i64 - x as i64);
        let top = 0i64
            .max(-(y as i64))
            .max(-(sy as i64))
            .max(clip.top as i64 - y as i64);
        let right = (width as i64)
            .min(surface.width as i64 - x as i64)
            .min(dib.width as i64 - sx as i64)
            .min(clip.right as i64 - x as i64);
        let bottom = (height as i64)
            .min(surface.height as i64 - y as i64)
            .min(dib.height as i64 - sy as i64)
            .min(clip.bottom as i64 - y as i64);
        if left >= right || top >= bottom {
            return Ok(());
        }
        let stride = (right - left) as usize * 4;
        let length = stride
            .checked_mul((bottom - top) as usize)
            .ok_or(Error::InvalidDimensions)?;
        let mut pixels = Vec::new();
        pixels
            .try_reserve_exact(length)
            .map_err(|_| Error::Exhausted)?;
        pixels.resize(length, 0);
        for (row, output) in pixels.chunks_exact_mut(stride).enumerate() {
            dib.rgba_row(
                memory,
                (sx as i64 + left) as u32,
                (sy as i64 + top + row as i64) as u32,
                output,
            )?;
        }
        let surface = self.surfaces.get_mut(&window).unwrap();
        for (row, input) in pixels.chunks_exact(stride).enumerate() {
            let at = (((y as i64 + top) as usize + row) * surface.width as usize
                + (x as i64 + left) as usize)
                * 4;
            surface.rgba[at..at + stride].copy_from_slice(input);
        }
        surface.dirty = true;
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn clipped_dib_blit_and_object_lifetimes() {
        let mut bytes = vec![0; 128];
        bytes[0..4].copy_from_slice(&40u32.to_le_bytes());
        bytes[4..8].copy_from_slice(&2u32.to_le_bytes());
        bytes[8..12].copy_from_slice(&(-1i32).to_le_bytes());
        bytes[12..14].copy_from_slice(&1u16.to_le_bytes());
        bytes[14..16].copy_from_slice(&32u16.to_le_bytes());
        bytes[64..72].copy_from_slice(&[1, 2, 3, 0, 4, 5, 6, 0]);
        let memory = crate::GuestMemory::new(&mut bytes);
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
            gdi.bit_blt(&memory, dst, 0, 0, 2, 1, src, 0, 0, 0),
            Err(Error::UnsupportedOperation)
        );
        assert!(!gdi.surface(1).unwrap().dirty);
        gdi.bit_blt(&memory, dst, 0, 0, 2, 1, src, 0, 0, SRCCOPY)
            .unwrap();
        assert_eq!(gdi.surface(1).unwrap().rgba, [3, 2, 1, 255, 0, 0, 0, 255]);
        gdi.select(src, stock).unwrap();
        gdi.delete_object(bitmap).unwrap();
        assert_eq!(gdi.select(src, bitmap), Err(Error::WrongType));
        gdi.delete_dc(src).unwrap();
        gdi.delete_dc(dst).unwrap();
    }
}
