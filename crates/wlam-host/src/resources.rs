//! Numeric resources mapped directly from a PE image, with stable HRSRCs.
use std::collections::BTreeMap;
#[derive(Debug, Clone, Copy)]
pub struct Resource {
    pub address: u32,
    pub size: u32,
    pub language: u32,
}
#[derive(Debug)]
pub struct Resources {
    handles: crate::objects::Handles<Resource>,
    names: BTreeMap<(u32, u32), u32>,
}
impl Default for Resources {
    fn default() -> Self {
        Self {
            handles: crate::objects::Handles::with_namespace(8),
            names: BTreeMap::new(),
        }
    }
}
impl Resources {
    pub fn register(
        &mut self,
        kind: u32,
        name: u32,
        language: u32,
        address: u32,
        size: u32,
    ) -> Result<(), String> {
        let resource = Resource {
            address,
            size,
            language,
        };
        if let Some(handle) = self.names.get(&(kind, name)) {
            let old = self.handles.get_mut(*handle).unwrap();
            if language == 1033 || old.language != 1033 && language == 0 {
                *old = resource;
            }
        } else {
            let handle = self
                .handles
                .insert(resource)
                .ok_or("resource handles exhausted")?;
            self.names.insert((kind, name), handle);
        }
        Ok(())
    }
    pub fn find(&self, kind: u32, name: u32) -> Option<u32> {
        self.names.get(&(kind, name)).copied()
    }
    pub fn get(&self, handle: u32) -> Option<Resource> {
        self.handles.get(handle).copied()
    }
    pub fn named(&self, kind: u32, name: u32) -> Option<Resource> {
        self.get(self.find(kind, name)?)
    }
    pub fn at_address(&self, kind: u32, address: u32) -> Option<Resource> {
        self.names
            .iter()
            .filter(|((t, _), _)| *t == kind)
            .filter_map(|(_, handle)| self.get(*handle))
            .find(|resource| resource.address == address)
    }
}
