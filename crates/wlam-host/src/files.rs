//! Read-only files supplied to the browser guest as bundle assets.
use crate::objects::Handles;
use std::collections::BTreeMap;

#[derive(Debug)]
struct OpenFile {
    bytes: Vec<u8>,
    position: usize,
}

#[derive(Debug)]
pub struct VirtualFiles {
    files: BTreeMap<String, Vec<u8>>,
    open: Handles<OpenFile>,
}

impl Default for VirtualFiles {
    fn default() -> Self {
        Self {
            files: BTreeMap::new(),
            open: Handles::with_namespace(4),
        }
    }
}

fn normalize(path: &[u8]) -> String {
    let path = String::from_utf8_lossy(path)
        .replace('\\', "/")
        .to_ascii_lowercase();
    let path = path.strip_prefix("c:/").unwrap_or(&path);
    path.split('/')
        .filter(|part| !part.is_empty() && *part != ".")
        .collect::<Vec<_>>()
        .join("/")
}

impl VirtualFiles {
    pub fn insert(&mut self, path: &[u8], bytes: Vec<u8>) {
        self.files.insert(normalize(path), bytes);
    }
    pub fn open(&mut self, path: &[u8]) -> Option<u32> {
        let bytes = self.files.get(&normalize(path))?.clone();
        self.open.insert(OpenFile { bytes, position: 0 })
    }
    pub fn contains_handle(&self, handle: u32) -> bool {
        self.open.get(handle).is_some()
    }
    pub fn close(&mut self, handle: u32) -> bool {
        self.open.remove(handle).is_some()
    }
    pub fn read(&mut self, handle: u32, length: usize) -> Option<Vec<u8>> {
        let file = self.open.get_mut(handle)?;
        let end = file.position.saturating_add(length).min(file.bytes.len());
        let bytes = file.bytes[file.position..end].to_vec();
        file.position = end;
        Some(bytes)
    }
    pub fn seek(&mut self, handle: u32, distance: i64, method: u32) -> Option<u32> {
        let file = self.open.get_mut(handle)?;
        let base = match method {
            0 => 0i64,
            1 => file.position as i64,
            2 => file.bytes.len() as i64,
            _ => return None,
        };
        let position = base.checked_add(distance)?;
        if position < 0 || position > file.bytes.len() as i64 {
            return None;
        }
        file.position = position as usize;
        Some(file.position as u32)
    }
}
