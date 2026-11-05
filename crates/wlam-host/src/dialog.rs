//! Bounded parser for the standard and extended Win32 in-memory dialog templates.
//! Coordinates are retained in dialog units; USER32 maps them using the dialog font.

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TemplateError {
    Truncated,
    TooManyControls,
    InvalidExtraData,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OrdinalOrString {
    Empty,
    Ordinal(u16),
    String(Vec<u16>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DialogControl {
    pub style: u32,
    pub ex_style: u32,
    pub x: i16,
    pub y: i16,
    pub cx: i16,
    pub cy: i16,
    pub id: u32,
    pub class: OrdinalOrString,
    pub title: OrdinalOrString,
    pub extra: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DialogTemplate {
    pub extended: bool,
    pub style: u32,
    pub ex_style: u32,
    pub x: i16,
    pub y: i16,
    pub cx: i16,
    pub cy: i16,
    pub menu: OrdinalOrString,
    pub class: OrdinalOrString,
    pub title: OrdinalOrString,
    pub font_point_size: Option<u16>,
    pub font: Option<Vec<u16>>,
    pub controls: Vec<DialogControl>,
}

struct Cursor<'a> {
    bytes: &'a [u8],
    at: usize,
}
impl<'a> Cursor<'a> {
    fn take(&mut self, count: usize) -> Result<&'a [u8], TemplateError> {
        let end = self.at.checked_add(count).ok_or(TemplateError::Truncated)?;
        let bytes = self
            .bytes
            .get(self.at..end)
            .ok_or(TemplateError::Truncated)?;
        self.at = end;
        Ok(bytes)
    }
    fn u16(&mut self) -> Result<u16, TemplateError> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }
    fn i16(&mut self) -> Result<i16, TemplateError> {
        Ok(self.u16()? as i16)
    }
    fn u32(&mut self) -> Result<u32, TemplateError> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn align4(&mut self) -> Result<(), TemplateError> {
        self.at = self.at.checked_add(3).ok_or(TemplateError::Truncated)? & !3;
        if self.at > self.bytes.len() {
            return Err(TemplateError::Truncated);
        }
        Ok(())
    }
    fn field(&mut self) -> Result<OrdinalOrString, TemplateError> {
        match self.u16()? {
            0 => Ok(OrdinalOrString::Empty),
            0xffff => Ok(OrdinalOrString::Ordinal(self.u16()?)),
            first => {
                let mut value = vec![first];
                loop {
                    let unit = self.u16()?;
                    if unit == 0 {
                        break;
                    }
                    value.push(unit);
                }
                Ok(OrdinalOrString::String(value))
            }
        }
    }
    fn controls(
        &mut self,
        count: u16,
        extended: bool,
    ) -> Result<Vec<DialogControl>, TemplateError> {
        if count > 1024 {
            return Err(TemplateError::TooManyControls);
        }
        let mut controls = Vec::with_capacity(count as usize);
        for _ in 0..count {
            self.align4()?;
            let style = self.u32()?;
            let ex_style = self.u32()?;
            let (x, y, cx, cy, id) = if extended {
                (
                    self.i16()?,
                    self.i16()?,
                    self.i16()?,
                    self.i16()?,
                    self.u32()?,
                )
            } else {
                (
                    self.i16()?,
                    self.i16()?,
                    self.i16()?,
                    self.i16()?,
                    self.u16()? as u32,
                )
            };
            let class = self.field()?;
            let title = self.field()?;
            let extra_len = self.u16()? as usize;
            if extra_len > 4096 {
                return Err(TemplateError::InvalidExtraData);
            }
            let extra = self.take(extra_len)?.to_vec();
            controls.push(DialogControl {
                style,
                ex_style,
                x,
                y,
                cx,
                cy,
                id,
                class,
                title,
                extra,
            });
        }
        Ok(controls)
    }
}

impl DialogTemplate {
    pub fn parse(bytes: &[u8]) -> Result<Self, TemplateError> {
        let mut c = Cursor { bytes, at: 0 };
        let extended = bytes.get(..2) == Some(&[1, 0]);
        let (style, ex_style, count, x, y, cx, cy) = if extended {
            c.u16()?;
            c.u16()?; // dlgVer, signature
            let _help_id = c.u32()?;
            let ex_style = c.u32()?;
            let style = c.u32()?;
            (
                style,
                ex_style,
                c.u16()?,
                c.i16()?,
                c.i16()?,
                c.i16()?,
                c.i16()?,
            )
        } else {
            (
                c.u32()?,
                c.u32()?,
                c.u16()?,
                c.i16()?,
                c.i16()?,
                c.i16()?,
                c.i16()?,
            )
        };
        let menu = c.field()?;
        let class = c.field()?;
        let title = c.field()?;
        let (font_point_size, font) = if style & 0x40 != 0 {
            let point = c.u16()?;
            if extended {
                let _weight = c.u16()?;
                let _italic = c.take(1)?[0];
                let _charset = c.take(1)?[0];
            }
            let name = match c.field()? {
                OrdinalOrString::String(name) => name,
                _ => return Err(TemplateError::Truncated),
            };
            (Some(point), Some(name))
        } else {
            (None, None)
        };
        let controls = c.controls(count, extended)?;
        Ok(Self {
            extended,
            style,
            ex_style,
            x,
            y,
            cx,
            cy,
            menu,
            class,
            title,
            font_point_size,
            font,
            controls,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_hover_resource_146_template_and_static_controls() {
        let bytes = [
            0xc0, 0, 0, 0x90, 0, 0, 0, 0, 2, 0, 0, 0, 0, 0, 116, 0, 111, 0, 0, 0, 0, 0, 0, 0, 0x0e,
            0, 77, 0, 83, 0, 32, 0, 83, 0, 97, 0, 110, 0, 115, 0, 32, 0, 83, 0, 101, 0, 114, 0,
            105, 0, 102, 0, 0, 0, 0, 0, 7, 0, 0, 0x50, 0, 0, 0, 0, 2, 0, 2, 0, 108, 0, 76, 0, 14,
            4, 255, 255, 130, 0, 0, 0, 0, 0, 0, 0, 1, 0, 2, 0x50, 0, 0, 0, 0, 3, 0, 81, 0, 107, 0,
            0, 0, 88, 4, 255, 255, 130, 0, 0, 0, 0, 0, 0, 0,
        ];
        let dialog = DialogTemplate::parse(&bytes).unwrap();
        assert!(!dialog.extended);
        assert_eq!((dialog.cx, dialog.cy), (116, 111));
        assert_eq!(dialog.controls.len(), 2);
        assert_eq!(dialog.controls[0].id, 1038);
        assert_eq!(dialog.controls[0].style, 0x50000007);
        assert_eq!(dialog.controls[0].class, OrdinalOrString::Ordinal(130));
        assert_eq!(dialog.controls[1].id, 1112);
        assert_eq!(dialog.controls[1].x, 3);
        assert_eq!(dialog.font_point_size, Some(14));
        assert!(
            matches!(&dialog.font,Some(name) if String::from_utf16(name).unwrap()=="MS Sans Serif")
        );
    }
    #[test]
    fn rejects_truncated_controls_and_bounds_count() {
        assert_eq!(
            DialogTemplate::parse(&[0; 8]),
            Err(TemplateError::Truncated)
        );
        let mut bytes = vec![0; 18];
        bytes[8..10].copy_from_slice(&1025u16.to_le_bytes());
        assert_eq!(
            DialogTemplate::parse(&bytes),
            Err(TemplateError::TooManyControls)
        );
    }
}
