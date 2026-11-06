//! Per-process Win32 registry hive for applications that store preferences.
//!
//! This is an in-memory compatibility store, not a view of the host OS
//! registry. Key and value names are case-insensitive, while value payloads
//! retain their original bytes and type.
use crate::memory::MemoryAccess;
use std::collections::{BTreeMap, BTreeSet};

const SUCCESS: u32 = 0;
const FILE_NOT_FOUND: u32 = 2;
const ACCESS_DENIED: u32 = 5;
const INVALID_HANDLE: u32 = 6;
const INVALID_PARAMETER: u32 = 87;
const MORE_DATA: u32 = 234;

const HKEY_CLASSES_ROOT: u32 = 0x8000_0000;
const HKEY_CURRENT_USER: u32 = 0x8000_0001;
const HKEY_LOCAL_MACHINE: u32 = 0x8000_0002;
const HKEY_USERS: u32 = 0x8000_0003;
const HKEY_CURRENT_CONFIG: u32 = 0x8000_0005;
const HKEY_DYN_DATA: u32 = 0x8000_0006;

#[derive(Debug, Clone, PartialEq, Eq)]
struct Value {
    kind: u32,
    bytes: Vec<u8>,
}

#[derive(Debug)]
pub struct Registry {
    keys: BTreeSet<Vec<u8>>,
    values: BTreeMap<(Vec<u8>, Vec<u8>), Value>,
    handles: BTreeMap<u32, Vec<u8>>,
    next_handle: u32,
}

impl Default for Registry {
    fn default() -> Self {
        let roots = [
            b"classes_root".as_slice(),
            b"current_user",
            b"local_machine",
            b"users",
            b"current_config",
            b"dyn_data",
        ];
        Self {
            keys: roots.into_iter().map(<[u8]>::to_vec).collect(),
            values: BTreeMap::new(),
            handles: BTreeMap::new(),
            next_handle: 1,
        }
    }
}

impl Registry {
    fn root(handle: u32) -> Option<&'static [u8]> {
        match handle {
            HKEY_CLASSES_ROOT => Some(b"classes_root"),
            HKEY_CURRENT_USER => Some(b"current_user"),
            HKEY_LOCAL_MACHINE => Some(b"local_machine"),
            HKEY_USERS => Some(b"users"),
            HKEY_CURRENT_CONFIG => Some(b"current_config"),
            HKEY_DYN_DATA => Some(b"dyn_data"),
            _ => None,
        }
    }

    fn path(&self, handle: u32) -> Result<Vec<u8>, u32> {
        if let Some(root) = Self::root(handle) {
            return Ok(root.to_vec());
        }
        self.handles.get(&handle).cloned().ok_or(INVALID_HANDLE)
    }

    fn child_path(parent: &[u8], child: &[u8]) -> Vec<u8> {
        let mut path = parent.to_vec();
        for part in child.split(|byte| *byte == b'\\') {
            if part.is_empty() {
                continue;
            }
            path.push(b'\\');
            path.extend(part.iter().map(u8::to_ascii_lowercase));
        }
        path
    }

    fn allocate_handle(&mut self, path: Vec<u8>) -> u32 {
        let handle = 0x7200_0000u32.wrapping_add(self.next_handle);
        self.next_handle = self.next_handle.wrapping_add(1).max(1);
        self.handles.insert(handle, path);
        handle
    }

    pub fn open_key(&mut self, root: u32, subkey: &[u8]) -> Result<u32, u32> {
        let base = self.path(root)?;
        let path = Self::child_path(&base, subkey);
        if !self.keys.contains(&path) {
            return Err(FILE_NOT_FOUND);
        }
        Ok(self.allocate_handle(path))
    }

    pub fn create_key(&mut self, root: u32, subkey: &[u8]) -> Result<(u32, bool), u32> {
        let base = self.path(root)?;
        let mut path = base;
        let mut created = false;
        for part in subkey
            .split(|byte| *byte == b'\\')
            .filter(|part| !part.is_empty())
        {
            path.push(b'\\');
            path.extend(part.iter().map(u8::to_ascii_lowercase));
            if self.keys.insert(path.clone()) {
                created = true;
            }
        }
        Ok((self.allocate_handle(path), created))
    }

    pub fn close_key(&mut self, handle: u32) -> u32 {
        if Self::root(handle).is_some() {
            // Predefined hive handles are process constants, not opened keys.
            return SUCCESS;
        }
        if self.handles.remove(&handle).is_some() {
            SUCCESS
        } else {
            INVALID_HANDLE
        }
    }

    pub fn set_value(&mut self, handle: u32, name: &[u8], kind: u32, bytes: Vec<u8>) -> u32 {
        let path = match self.path(handle) {
            Ok(path) => path,
            Err(error) => return error,
        };
        self.values.insert(
            (path, name.iter().map(u8::to_ascii_lowercase).collect()),
            Value { kind, bytes },
        );
        SUCCESS
    }

    pub fn query_value(&self, handle: u32, name: &[u8]) -> Result<(u32, &[u8]), u32> {
        let path = self.path(handle)?;
        self.values
            .get(&(path, name.iter().map(u8::to_ascii_lowercase).collect()))
            .map(|value| (value.kind, value.bytes.as_slice()))
            .ok_or(FILE_NOT_FOUND)
    }

    pub fn delete_value(&mut self, handle: u32, name: &[u8]) -> u32 {
        let path = match self.path(handle) {
            Ok(path) => path,
            Err(error) => return error,
        };
        if self
            .values
            .remove(&(path, name.iter().map(u8::to_ascii_lowercase).collect()))
            .is_some()
        {
            SUCCESS
        } else {
            FILE_NOT_FOUND
        }
    }

    pub fn delete_key(&mut self, handle: u32, subkey: &[u8]) -> u32 {
        let base = match self.path(handle) {
            Ok(path) => path,
            Err(error) => return error,
        };
        let path = Self::child_path(&base, subkey);
        if path == base || !self.keys.contains(&path) {
            return FILE_NOT_FOUND;
        }
        let mut prefix = path.clone();
        prefix.push(b'\\');
        if self.keys.iter().any(|key| key.starts_with(&prefix)) {
            return ACCESS_DENIED;
        }
        self.keys.remove(&path);
        self.values.retain(|(key, _), _| key != &path);
        self.handles.retain(|_, key| key != &path);
        SUCCESS
    }

    fn read_string(memory: &dyn MemoryAccess, address: u32) -> Result<Vec<u8>, String> {
        if address == 0 {
            return Ok(Vec::new());
        }
        let mut value = Vec::new();
        for index in 0..4096u32 {
            let at = address
                .checked_add(index)
                .ok_or("registry string address overflow")?;
            let mut byte = [0];
            memory
                .read(at, &mut byte)
                .map_err(|error| format!("registry string read: {error:?}"))?;
            if byte[0] == 0 {
                return Ok(value);
            }
            value.push(byte[0]);
        }
        Err("registry string exceeds 4096 bytes".into())
    }

    fn write_word(memory: &mut dyn MemoryAccess, address: u32, value: u32) -> Result<(), String> {
        if address == 0 {
            return Err("null registry output pointer".into());
        }
        memory
            .write_word(address, value)
            .map_err(|error| format!("registry output write: {error:?}"))
    }

    /// Dispatch a Win32 registry API. These calls return LSTATUS directly;
    /// they do not use GetLastError for ordinary registry failures.
    pub fn call(
        &mut self,
        name: &str,
        args: &[u32],
        memory: &mut dyn MemoryAccess,
    ) -> Result<u32, String> {
        let a = |index: usize| args.get(index).copied().unwrap_or(0);
        match name {
            "RegOpenKeyA" | "RegOpenKeyExA" => {
                let (root, subkey, output) = if name == "RegOpenKeyA" {
                    (a(0), Self::read_string(memory, a(1))?, a(2))
                } else {
                    if a(2) != 0 {
                        return Ok(INVALID_PARAMETER);
                    }
                    (a(0), Self::read_string(memory, a(1))?, a(4))
                };
                if output == 0 {
                    return Ok(INVALID_PARAMETER);
                }
                match self.open_key(root, &subkey) {
                    Ok(handle) => {
                        Self::write_word(memory, output, handle)?;
                        Ok(SUCCESS)
                    }
                    Err(error) => Ok(error),
                }
            }
            "RegCreateKeyExA" => {
                if a(2) != 0 || a(4) > 1 || a(7) == 0 {
                    return Ok(INVALID_PARAMETER);
                }
                let subkey = Self::read_string(memory, a(1))?;
                let (handle, created) = match self.create_key(a(0), &subkey) {
                    Ok(result) => result,
                    Err(error) => return Ok(error),
                };
                Self::write_word(memory, a(7), handle)?;
                if a(8) != 0 {
                    Self::write_word(memory, a(8), if created { 1 } else { 2 })?;
                }
                Ok(SUCCESS)
            }
            "RegCloseKey" => Ok(self.close_key(a(0))),
            "RegSetValueExA" => {
                if a(2) != 0 {
                    return Ok(INVALID_PARAMETER);
                }
                let name = Self::read_string(memory, a(1))?;
                let size = a(5);
                if size > 16 * 1024 * 1024 || (size != 0 && a(4) == 0) {
                    return Ok(INVALID_PARAMETER);
                }
                let mut bytes = vec![0; size as usize];
                if size != 0 {
                    memory
                        .read(a(4), &mut bytes)
                        .map_err(|error| format!("registry value read: {error:?}"))?;
                }
                Ok(self.set_value(a(0), &name, a(3), bytes))
            }
            "RegQueryValueExA" => {
                if a(2) != 0 || a(5) == 0 {
                    return Ok(INVALID_PARAMETER);
                }
                let name = Self::read_string(memory, a(1))?;
                let (kind, bytes) = match self.query_value(a(0), &name) {
                    Ok(value) => value,
                    Err(error) => return Ok(error),
                };
                if a(3) != 0 {
                    Self::write_word(memory, a(3), kind)?;
                }
                let capacity = memory
                    .read_word(a(5))
                    .map_err(|error| format!("registry value-size read: {error:?}"))?;
                if a(4) == 0 {
                    Self::write_word(memory, a(5), bytes.len() as u32)?;
                    return Ok(SUCCESS);
                }
                if capacity < bytes.len() as u32 {
                    Self::write_word(memory, a(5), bytes.len() as u32)?;
                    return Ok(MORE_DATA);
                }
                memory
                    .write(a(4), bytes)
                    .map_err(|error| format!("registry value write: {error:?}"))?;
                Self::write_word(memory, a(5), bytes.len() as u32)?;
                Ok(SUCCESS)
            }
            "RegQueryValueA" => {
                let base = match self.path(a(0)) {
                    Ok(path) => path,
                    Err(error) => return Ok(error),
                };
                let subkey = Self::read_string(memory, a(1))?;
                let path = Self::child_path(&base, &subkey);
                let Some(value) = self.values.get(&(path, Vec::new())) else {
                    return Ok(FILE_NOT_FOUND);
                };
                if a(3) == 0 {
                    return Ok(INVALID_PARAMETER);
                }
                let capacity = memory
                    .read_word(a(3))
                    .map_err(|error| format!("legacy registry value-size read: {error:?}"))?;
                if a(2) == 0 {
                    Self::write_word(memory, a(3), value.bytes.len() as u32)?;
                    return Ok(SUCCESS);
                }
                if capacity < value.bytes.len() as u32 {
                    Self::write_word(memory, a(3), value.bytes.len() as u32)?;
                    return Ok(MORE_DATA);
                }
                memory
                    .write(a(2), &value.bytes)
                    .map_err(|error| format!("legacy registry value write: {error:?}"))?;
                Self::write_word(memory, a(3), value.bytes.len() as u32)?;
                Ok(SUCCESS)
            }
            "RegDeleteValueA" => {
                let name = Self::read_string(memory, a(1))?;
                Ok(self.delete_value(a(0), &name))
            }
            "RegDeleteKeyA" => {
                let subkey = Self::read_string(memory, a(1))?;
                Ok(self.delete_key(a(0), &subkey))
            }
            _ => Err(format!("unsupported registry API {name}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::GuestMemory;

    #[test]
    fn key_creation_opening_and_value_round_trip_follow_win32_statuses() {
        let mut storage = [0; 0x4000];
        let mut memory = GuestMemory::new(&mut storage);
        let mut registry = Registry::default();
        memory
            .write(0x100, b"Software\\Microsoft\\Hover!\0")
            .unwrap();
        memory.write(0x180, b"SkipQuickHelp\0").unwrap();
        memory.write(0x200, &[1, 0, 0, 0]).unwrap();

        assert_eq!(
            registry
                .call(
                    "RegOpenKeyExA",
                    &[HKEY_CURRENT_USER, 0x100, 0, 0xf003f, 0x300],
                    &mut memory
                )
                .unwrap(),
            FILE_NOT_FOUND
        );
        assert_eq!(
            registry
                .call(
                    "RegCreateKeyExA",
                    &[HKEY_CURRENT_USER, 0x100, 0, 0, 0, 0xf003f, 0, 0x300, 0x304],
                    &mut memory
                )
                .unwrap(),
            SUCCESS
        );
        let key = memory.read_word(0x300).unwrap();
        assert_eq!(memory.read_word(0x304).unwrap(), 1);
        assert_eq!(
            registry
                .call("RegSetValueExA", &[key, 0x180, 0, 4, 0x200, 4], &mut memory)
                .unwrap(),
            SUCCESS
        );
        assert_eq!(
            registry
                .call(
                    "RegCreateKeyExA",
                    &[HKEY_CURRENT_USER, 0x100, 0, 0, 0, 0, 0, 0x308, 0x30c],
                    &mut memory
                )
                .unwrap(),
            SUCCESS
        );
        assert_eq!(memory.read_word(0x30c).unwrap(), 2);
        let reopened = memory.read_word(0x308).unwrap();
        memory.write_word(0x314, 4).unwrap();
        assert_eq!(
            registry
                .call(
                    "RegQueryValueExA",
                    &[reopened, 0x180, 0, 0x310, 0x200, 0x314],
                    &mut memory
                )
                .unwrap(),
            SUCCESS
        );
        assert_eq!(memory.read_word(0x310).unwrap(), 4);
        assert_eq!(memory.read_word(0x314).unwrap(), 4);
        assert_eq!(memory.read_word(0x200).unwrap(), 1);
        assert_eq!(
            registry.call("RegCloseKey", &[key], &mut memory).unwrap(),
            SUCCESS
        );
        assert_eq!(
            registry.call("RegCloseKey", &[key], &mut memory).unwrap(),
            INVALID_HANDLE
        );
    }

    #[test]
    fn query_reports_required_size_and_delete_rejects_nonempty_parent() {
        let mut storage = [0; 0x4000];
        let mut memory = GuestMemory::new(&mut storage);
        let mut registry = Registry::default();
        memory.write(0x100, b"Software\\Game\0").unwrap();
        memory.write(0x180, b"Mode\0").unwrap();
        memory.write(0x200, b"classic\0").unwrap();
        assert_eq!(
            registry
                .call(
                    "RegCreateKeyExA",
                    &[HKEY_CURRENT_USER, 0x100, 0, 0, 0, 0, 0, 0x300, 0],
                    &mut memory
                )
                .unwrap(),
            SUCCESS
        );
        let key = memory.read_word(0x300).unwrap();
        assert_eq!(
            registry
                .call("RegSetValueExA", &[key, 0x180, 0, 1, 0x200, 8], &mut memory)
                .unwrap(),
            SUCCESS
        );
        memory.write_word(0x314, 2).unwrap();
        assert_eq!(
            registry
                .call(
                    "RegQueryValueExA",
                    &[key, 0x180, 0, 0, 0x320, 0x314],
                    &mut memory
                )
                .unwrap(),
            MORE_DATA
        );
        assert_eq!(memory.read_word(0x314).unwrap(), 8);
        memory.write(0x240, b"Software\\Game\\Child\0").unwrap();
        assert_eq!(
            registry
                .call(
                    "RegCreateKeyExA",
                    &[HKEY_CURRENT_USER, 0x240, 0, 0, 0, 0, 0, 0x304, 0],
                    &mut memory
                )
                .unwrap(),
            SUCCESS
        );
        assert_eq!(
            registry
                .call("RegDeleteKeyA", &[HKEY_CURRENT_USER, 0x100], &mut memory)
                .unwrap(),
            ACCESS_DENIED
        );
    }
}
