//! Process-hosted global atoms used by window property names.
use std::collections::BTreeMap;
#[derive(Debug, Default)]
pub struct Atoms {
    entries: BTreeMap<u16, (Vec<u8>, u32)>,
}
impl Atoms {
    pub fn add(&mut self, name: &[u8]) -> Result<u16, u32> {
        if name.is_empty() || name.len() > 255 || !name.is_ascii() {
            return Err(87);
        }
        if name[0] == b'#' {
            if let Ok(value) = std::str::from_utf8(&name[1..]).unwrap_or("").parse::<u16>() {
                if (1..0xc000).contains(&value) {
                    return Ok(value);
                }
            }
        }
        if let Some((id, (_, references))) = self
            .entries
            .iter_mut()
            .find(|(_, (existing, _))| existing.eq_ignore_ascii_case(name))
        {
            *references = references.checked_add(1).ok_or(8u32)?;
            return Ok(*id);
        }
        let id = (0xc000..=0xffff)
            .find(|id| !self.entries.contains_key(id))
            .ok_or(8u32)?;
        self.entries.insert(id, (name.to_vec(), 1));
        Ok(id)
    }
    pub fn delete(&mut self, id: u16) -> bool {
        if (1..0xc000).contains(&id) {
            return true;
        }
        let Some((_, count)) = self.entries.get_mut(&id) else {
            return false;
        };
        *count -= 1;
        if *count == 0 {
            self.entries.remove(&id);
        }
        true
    }
    pub fn name(&self, id: u16) -> Option<Vec<u8>> {
        if (1..0xc000).contains(&id) {
            Some(format!("#{id}").into_bytes())
        } else {
            self.entries.get(&id).map(|(name, _)| name.clone())
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn case_insensitive_names_are_reference_counted_and_reusable() {
        let mut atoms = Atoms::default();
        let id = atoms.add(b"AfxOldWndProc").unwrap();
        assert_eq!(atoms.add(b"afxoldwndproc"), Ok(id));
        assert!(atoms.delete(id));
        assert!(atoms.name(id).is_some());
        assert!(atoms.delete(id));
        assert!(atoms.name(id).is_none());
        assert!(!atoms.delete(id));
        assert_eq!(atoms.add(b"another"), Ok(id));
        assert_eq!(atoms.add(b"#123"), Ok(123));
        assert_eq!(atoms.name(123), Some(b"#123".to_vec()));
    }
}
