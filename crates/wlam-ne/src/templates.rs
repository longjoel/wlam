//! Explicit resource-layout conversion for the shared Win32 services.
//! Win16 classic menu strings are byte strings; Win32 templates use UTF-16.
pub fn menu_to_win32(bytes: &[u8]) -> Result<Vec<u8>, String> {
    fn word(bytes: &[u8], at: &mut usize) -> Result<u16, String> {
        let value = bytes.get(*at..*at + 2).ok_or("truncated Win16 menu")?;
        *at += 2;
        Ok(u16::from_le_bytes([value[0], value[1]]))
    }
    fn items(
        bytes: &[u8],
        at: &mut usize,
        out: &mut Vec<u8>,
        depth: usize,
        budget: &mut usize,
    ) -> Result<(), String> {
        if depth > 64 {
            return Err("menu nesting exceeds limit".into());
        }
        loop {
            if *budget == 0 {
                return Err("menu item count exceeds limit".into());
            }
            *budget -= 1;
            let flags = word(bytes, at)?;
            out.extend_from_slice(&flags.to_le_bytes());
            if flags & 0x10 == 0 {
                out.extend_from_slice(&word(bytes, at)?.to_le_bytes());
            }
            loop {
                let byte = *bytes.get(*at).ok_or("unterminated menu string")?;
                *at += 1;
                // Reuse the host's CP1252 encoder in reverse. Most strings use
                // direct Latin-1 values; the C1 range is a small lookup.
                let value = if (0x80..0xa0).contains(&byte) {
                    (0..=u16::MAX)
                        .find(|value| wlam_host::locale::ansi_encode(*value) == byte)
                        .ok_or("unknown ANSI menu byte")?
                } else {
                    u16::from(byte)
                };
                out.extend_from_slice(&value.to_le_bytes());
                if byte == 0 {
                    break;
                }
            }
            if flags & 0x10 != 0 {
                items(bytes, at, out, depth + 1, budget)?;
            }
            if flags & 0x80 != 0 {
                return Ok(());
            }
        }
    }
    let mut at = 0;
    if word(bytes, &mut at)? != 0 {
        return Err("unsupported Win16 menu version".into());
    }
    let extra = word(bytes, &mut at)? as usize;
    at = at
        .checked_add(extra)
        .filter(|end| *end <= bytes.len())
        .ok_or("invalid menu header size")?;
    let mut output = vec![0; 4];
    items(bytes, &mut at, &mut output, 0, &mut 4096)?;
    Ok(output)
}

/// Locate a mutable classic-menu flags word by command ID or root position.
pub fn menu_item_flags(
    bytes: &[u8],
    item: u16,
    by_position: bool,
) -> Result<Option<usize>, String> {
    Ok(menu_item_location(bytes, item, by_position)?.0)
}

fn menu_item_location(
    bytes: &[u8],
    item: u16,
    by_position: bool,
) -> Result<(Option<usize>, usize), String> {
    fn word(bytes: &[u8], at: &mut usize) -> Result<u16, String> {
        let b = bytes.get(*at..*at + 2).ok_or("truncated menu template")?;
        *at += 2;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }
    fn walk(
        bytes: &[u8],
        at: &mut usize,
        depth: usize,
        item: u16,
        position: bool,
        budget: &mut usize,
        found: &mut Option<usize>,
    ) -> Result<(), String> {
        if depth > 64 {
            return Err("menu nesting exceeds limit".into());
        }
        let mut index = 0;
        loop {
            if *budget == 0 {
                return Err("menu item count exceeds limit".into());
            }
            *budget -= 1;
            let flags_at = *at;
            let flags = word(bytes, at)?;
            let id = if flags & 0x10 == 0 {
                Some(word(bytes, at)?)
            } else {
                None
            };
            if found.is_none()
                && (if position {
                    depth == 0 && index == item
                } else {
                    id == Some(item)
                })
            {
                *found = Some(flags_at);
            }
            while word(bytes, at)? != 0 {}
            if flags & 0x10 != 0 {
                walk(bytes, at, depth + 1, item, position, budget, found)?;
            }
            if flags & 0x80 != 0 {
                return Ok(());
            }
            index += 1;
        }
    }
    let mut at = 0;
    if word(bytes, &mut at)? != 0 {
        return Err("unsupported menu template version".into());
    }
    let extra = word(bytes, &mut at)? as usize;
    at += extra;
    if at == bytes.len() {
        return Ok((None, at));
    }
    let mut found = None;
    walk(bytes, &mut at, 0, item, by_position, &mut 4096, &mut found)?;
    Ok((found, at))
}

pub fn modify_menu_item(
    bytes: &[u8],
    item: u16,
    by_position: bool,
    flags: u16,
    id: u16,
    text: &[u8],
) -> Result<Option<Vec<u8>>, String> {
    let (target, length) = menu_item_location(bytes, item, by_position)?;
    let Some(start) = target else {
        return Ok(None);
    };
    let previous_flags = u16::from_le_bytes(bytes[start..start + 2].try_into().unwrap());
    if (previous_flags | flags) & 0x10 != 0 {
        return Err("popup replacement is not implemented".into());
    }
    if flags & (4 | 0x100) != 0 {
        return Err("non-string menu replacement is not implemented".into());
    }
    let mut end = start + 4;
    loop {
        let word = bytes.get(end..end + 2).ok_or("truncated menu label")?;
        end += 2;
        if word == [0, 0] {
            break;
        }
    }
    let mut item = ((flags & !0x480) | (previous_flags & 0x80))
        .to_le_bytes()
        .to_vec();
    item.extend_from_slice(&id.to_le_bytes());
    for byte in text.iter().copied().chain(std::iter::once(0)) {
        let value = if (0x80..0xa0).contains(&byte) {
            (0..=u16::MAX)
                .find(|value| wlam_host::locale::ansi_encode(*value) == byte)
                .ok_or("unknown menu character")?
        } else {
            u16::from(byte)
        };
        item.extend_from_slice(&value.to_le_bytes());
    }
    let mut output = bytes[..length].to_vec();
    output.splice(start..end, item);
    Ok(Some(output))
}

/// Remove a classic-menu item while preserving the end marker of its siblings.
pub fn remove_menu_item(
    bytes: &[u8],
    item: u16,
    by_position: bool,
) -> Result<Option<Vec<u8>>, String> {
    let Some(target) = menu_item_flags(bytes, item, by_position)? else {
        return Ok(None);
    };
    fn word(bytes: &[u8], at: &mut usize) -> Result<u16, String> {
        let raw = bytes.get(*at..*at + 2).ok_or("truncated menu removal")?;
        *at += 2;
        Ok(u16::from_le_bytes([raw[0], raw[1]]))
    }
    fn list(
        bytes: &[u8],
        at: &mut usize,
        target: usize,
        depth: usize,
        budget: &mut usize,
        found: &mut Option<(usize, usize, Option<usize>, bool)>,
    ) -> Result<(), String> {
        if depth > 64 {
            return Err("menu nesting exceeds limit".into());
        }
        let mut previous = None;
        loop {
            if *budget == 0 {
                return Err("menu item count exceeds limit".into());
            }
            *budget -= 1;
            let start = *at;
            let flags = word(bytes, at)?;
            if flags & 0x10 == 0 {
                word(bytes, at)?;
            }
            while word(bytes, at)? != 0 {}
            if flags & 0x10 != 0 {
                list(bytes, at, target, depth + 1, budget, found)?;
            }
            if start == target {
                if previous.is_none() && flags & 0x80 != 0 && depth != 0 {
                    return Err("empty submenu serialization is unsupported".into());
                }
                *found = Some((start, *at, previous, flags & 0x80 != 0));
            }
            if flags & 0x80 != 0 {
                return Ok(());
            }
            previous = Some(start);
        }
    }
    let mut at = 4 + u16::from_le_bytes(bytes[2..4].try_into().unwrap()) as usize;
    let mut found = None;
    list(bytes, &mut at, target, 0, &mut 4096, &mut found)?;
    let (start, end, previous, last) = found.ok_or("menu removal target disappeared")?;
    let mut output = bytes[..at].to_vec();
    if last {
        if let Some(previous) = previous {
            output[previous] |= 0x80;
        }
    }
    output.drain(start..end);
    Ok(Some(output))
}

/// Convert the packed, byte-aligned Windows 3.x dialog resource layout.
pub fn dialog_to_win32(bytes: &[u8]) -> Result<Vec<u8>, String> {
    struct Input<'a> {
        bytes: &'a [u8],
        at: usize,
    }
    impl Input<'_> {
        fn take(&mut self, n: usize) -> Result<&[u8], String> {
            let end = self.at.checked_add(n).ok_or("dialog size overflow")?;
            let value = self
                .bytes
                .get(self.at..end)
                .ok_or("truncated Win16 dialog")?;
            self.at = end;
            Ok(value)
        }
        fn byte(&mut self) -> Result<u8, String> {
            Ok(self.take(1)?[0])
        }
        fn field(&mut self, out: &mut Vec<u8>, ordinal: bool) -> Result<(), String> {
            let mut byte = self.byte()?;
            if ordinal && byte == 0xff {
                out.extend_from_slice(&0xffffu16.to_le_bytes());
                out.extend_from_slice(self.take(2)?);
                return Ok(());
            }
            loop {
                let value = if (0x80..0xa0).contains(&byte) {
                    (0..=u16::MAX)
                        .find(|v| wlam_host::locale::ansi_encode(*v) == byte)
                        .ok_or("unknown ANSI dialog byte")?
                } else {
                    u16::from(byte)
                };
                out.extend_from_slice(&value.to_le_bytes());
                if byte == 0 {
                    return Ok(());
                }
                byte = self.byte()?;
            }
        }
    }
    let mut input = Input { bytes, at: 0 };
    let style = u32::from_le_bytes(input.take(4)?.try_into().unwrap());
    let count = input.byte()?;
    let mut out = style.to_le_bytes().to_vec();
    out.extend_from_slice(&0u32.to_le_bytes()); // extended style
    out.extend_from_slice(&u16::from(count).to_le_bytes());
    out.extend_from_slice(input.take(8)?);
    input.field(&mut out, true)?; // menu
    input.field(&mut out, false)?; // class
    input.field(&mut out, false)?; // caption
    if style & 0x40 != 0 {
        out.extend_from_slice(input.take(2)?);
        input.field(&mut out, false)?;
    }
    for _ in 0..count {
        while out.len() % 4 != 0 {
            out.push(0);
        }
        let geometry_id = input.take(10)?.to_vec();
        out.extend_from_slice(input.take(4)?); // style
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&geometry_id);
        let class = *input
            .bytes
            .get(input.at)
            .ok_or("missing dialog control class")?;
        if class & 0x80 != 0 {
            input.byte()?;
            if !(0x80..=0x85).contains(&class) {
                return Err("invalid dialog control class".into());
            }
            out.extend_from_slice(&0xffffu16.to_le_bytes());
            out.extend_from_slice(&u16::from(class).to_le_bytes());
        } else {
            input.field(&mut out, false)?;
        }
        input.field(&mut out, true)?;
        let extra = usize::from(input.byte()?);
        out.extend_from_slice(&(extra as u16).to_le_bytes());
        out.extend_from_slice(input.take(extra)?);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn menu_removal_preserves_last_item_flags_and_an_empty_root() {
        let bytes =
            menu_to_win32(&[0, 0, 0, 0, 0, 0, 10, 0, b'A', 0, 0x80, 0, 20, 0, b'B', 0]).unwrap();
        let modified = modify_menu_item(&bytes, 20, false, 1, 21, b"Longer label")
            .unwrap()
            .unwrap();
        let offset = menu_item_flags(&modified, 21, false).unwrap().unwrap();
        assert_eq!(modified[offset], 0x81);
        assert_eq!(menu_item_flags(&modified, 10, false).unwrap(), Some(4));
        assert_eq!(menu_item_flags(&modified, 20, false).unwrap(), None);
        let last_removed = remove_menu_item(&bytes, 20, false).unwrap().unwrap();
        assert_eq!(menu_item_flags(&last_removed, 10, false).unwrap(), Some(4));
        assert_eq!(last_removed[4] & 0x80, 0x80);
        assert_eq!(menu_item_flags(&last_removed, 20, false).unwrap(), None);
        let first_removed = remove_menu_item(&bytes, 0, true).unwrap().unwrap();
        assert_eq!(menu_item_flags(&first_removed, 20, false).unwrap(), Some(4));
        let empty = remove_menu_item(&first_removed, 20, false)
            .unwrap()
            .unwrap();
        assert_eq!(empty, [0; 4]);
        assert_eq!(menu_item_flags(&empty, 0, true).unwrap(), None);
        assert!(remove_menu_item(&bytes, 99, false).unwrap().is_none());
    }
    #[test]
    fn packed_dialog_controls_preserve_geometry_ordinals_and_creation_data() {
        let mut raw = 0x40u32.to_le_bytes().to_vec();
        raw.push(1);
        for n in [1u16, 2, 100, 60] {
            raw.extend_from_slice(&n.to_le_bytes());
        }
        raw.extend_from_slice(b"\0\0Title\0");
        raw.extend_from_slice(&9u16.to_le_bytes());
        raw.extend_from_slice(b"Helv\0");
        for n in [3u16, 4, 20, 10, 7] {
            raw.extend_from_slice(&n.to_le_bytes());
        }
        raw.extend_from_slice(&0x50000003u32.to_le_bytes());
        raw.extend_from_slice(&[0x82, 0xff, 42, 0, 3, 11, 22, 33]);
        let bytes = dialog_to_win32(&raw).unwrap();
        let parsed = wlam_host::dialog::DialogTemplate::parse(&bytes).unwrap();
        assert_eq!(
            (parsed.cx, parsed.cy, parsed.font_point_size),
            (100, 60, Some(9))
        );
        let c = &parsed.controls[0];
        assert_eq!((c.x, c.y, c.cx, c.cy, c.id), (3, 4, 20, 10, 7));
        assert_eq!(c.class, wlam_host::dialog::OrdinalOrString::Ordinal(0x82));
        assert_eq!(c.title, wlam_host::dialog::OrdinalOrString::Ordinal(42));
        assert_eq!(c.extra, [11, 22, 33]);
        for end in 0..raw.len() {
            assert!(dialog_to_win32(&raw[..end]).is_err(), "truncation at {end}");
        }
    }
    #[test]
    fn menu_popup_and_command_layouts_convert_without_ansi_alignment_padding() {
        let mut raw = vec![0, 0, 0, 0, 0x90, 0];
        raw.extend_from_slice(b"File\0");
        raw.extend_from_slice(&[0x80, 0, 100, 0]);
        raw.extend_from_slice(b"Open\0");
        let output = menu_to_win32(&raw).unwrap();
        let mut expected = vec![0, 0, 0, 0, 0x90, 0];
        for value in "File\0".encode_utf16() {
            expected.extend_from_slice(&value.to_le_bytes());
        }
        expected.extend_from_slice(&[0x80, 0, 100, 0]);
        for value in "Open\0".encode_utf16() {
            expected.extend_from_slice(&value.to_le_bytes());
        }
        assert_eq!(output, expected);
        assert_eq!(menu_item_flags(&output, 0, true).unwrap(), Some(4));
        assert_eq!(menu_item_flags(&output, 100, false).unwrap(), Some(16));
        assert_eq!(menu_item_flags(&output, 1, true).unwrap(), None);
        assert_eq!(menu_item_flags(&output, 101, false).unwrap(), None);
        assert!(menu_item_flags(&output[..output.len() - 1], 100, false).is_err());
        assert!(menu_to_win32(&raw[..raw.len() - 1]).is_err());
    }
}
