//! Generation-checked object handles. Retired generations are never reused.
#[derive(Debug)]
struct Slot<T> {
    generation: u16,
    value: Option<T>,
}
#[derive(Debug)]
pub struct Handles<T> {
    slots: Vec<Slot<T>>,
    namespace: u16,
}
impl<T> Default for Handles<T> {
    fn default() -> Self {
        Self::with_namespace(0)
    }
}
impl<T> Handles<T> {
    pub fn with_namespace(namespace: u16) -> Self {
        assert!(namespace < 16);
        Self {
            slots: Vec::new(),
            namespace,
        }
    }
    pub fn insert(&mut self, value: T) -> Option<u32> {
        for (index, slot) in self.slots.iter_mut().enumerate() {
            if slot.value.is_none() && slot.generation < (self.namespace << 12 | 0x0fff) {
                slot.generation += 1;
                slot.value = Some(value);
                return Some((u32::from(slot.generation) << 16) | (index as u32 + 1));
            }
        }
        if self.slots.len() >= u16::MAX as usize {
            return None;
        }
        self.slots.push(Slot {
            generation: (self.namespace << 12) + 1,
            value: Some(value),
        });
        Some((u32::from((self.namespace << 12) + 1) << 16) | self.slots.len() as u32)
    }
    fn slot(&self, handle: u32) -> Option<usize> {
        let index = (handle & 0xffff).checked_sub(1)? as usize;
        let slot = self.slots.get(index)?;
        (slot.generation as u32 == handle >> 16 && slot.value.is_some()).then_some(index)
    }
    pub fn get(&self, handle: u32) -> Option<&T> {
        self.slots.get(self.slot(handle)?)?.value.as_ref()
    }
    pub fn get_mut(&mut self, handle: u32) -> Option<&mut T> {
        let index = self.slot(handle)?;
        self.slots[index].value.as_mut()
    }
    pub fn remove(&mut self, handle: u32) -> Option<T> {
        let index = self.slot(handle)?;
        self.slots[index].value.take()
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stale_and_null_handles_never_resolve() {
        let mut objects = Handles::default();
        let first = objects.insert(10).unwrap();
        assert_eq!(objects.remove(first), Some(10));
        let second = objects.insert(20).unwrap();
        assert_ne!(first, second);
        assert_eq!(objects.get(first), None);
        assert_eq!(objects.get(0), None);
        assert_eq!(objects.get(second), Some(&20));
    }
}

#[cfg(test)]
mod namespace_tests {
    use super::*;
    #[test]
    fn handles_cannot_cross_object_namespaces() {
        let mut windows = Handles::with_namespace(1);
        let mut graphics = Handles::with_namespace(2);
        let window = windows.insert(1).unwrap();
        let bitmap = graphics.insert(2).unwrap();
        assert_ne!(window, bitmap);
        assert!(windows.get(bitmap).is_none());
        assert!(graphics.get(window).is_none());
    }
}
