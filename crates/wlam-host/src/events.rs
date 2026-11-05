//! Process-local Win32 event objects and independently closable handles.
use std::collections::BTreeMap;

#[derive(Debug)]
struct Event {
    name: Option<Vec<u8>>,
    manual: bool,
    signaled: bool,
    references: u32,
}
#[derive(Debug)]
pub struct Events {
    handles: crate::objects::Handles<u64>,
    objects: BTreeMap<u64, Event>,
    next: u64,
}
impl Default for Events {
    fn default() -> Self {
        Self {
            handles: crate::objects::Handles::with_namespace(10),
            objects: BTreeMap::new(),
            next: 1,
        }
    }
}
impl Events {
    pub fn create(
        &mut self,
        name: Option<Vec<u8>>,
        manual: bool,
        signaled: bool,
    ) -> Option<(u32, bool)> {
        let existing = name.as_ref().and_then(|name| {
            self.objects
                .iter()
                .find(|(_, event)| event.name.as_ref() == Some(name))
                .map(|(&id, _)| id)
        });
        let id = existing.unwrap_or(self.next);
        let handle = self.handles.insert(id)?;
        if let Some(event) = self.objects.get_mut(&id) {
            event.references += 1;
        } else {
            self.next += 1;
            self.objects.insert(
                id,
                Event {
                    name,
                    manual,
                    signaled,
                    references: 1,
                },
            );
        }
        Some((handle, existing.is_some()))
    }
    pub fn close(&mut self, handle: u32) -> bool {
        let Some(id) = self.handles.remove(handle) else {
            return false;
        };
        let event = self.objects.get_mut(&id).unwrap();
        event.references -= 1;
        if event.references == 0 {
            self.objects.remove(&id);
        }
        true
    }
    pub fn signal(&mut self, handle: u32, signaled: bool) -> Option<()> {
        let id = *self.handles.get(handle)?;
        self.objects.get_mut(&id)?.signaled = signaled;
        Some(())
    }
    pub fn poll(&mut self, handle: u32) -> Option<bool> {
        let id = *self.handles.get(handle)?;
        let event = self.objects.get_mut(&id)?;
        let result = event.signaled;
        if !event.manual {
            event.signaled = false;
        }
        Some(result)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn named_events_share_state_until_last_close_and_auto_reset_once() {
        let mut events = Events::default();
        let (a, exists) = events.create(Some(b"Hover".to_vec()), false, true).unwrap();
        assert!(!exists);
        let (b, exists) = events.create(Some(b"Hover".to_vec()), true, false).unwrap();
        assert!(exists);
        assert_ne!(a, b);
        assert_eq!(events.poll(b), Some(true));
        assert_eq!(events.poll(a), Some(false));
        events.signal(a, true).unwrap();
        assert!(events.close(a));
        assert_eq!(events.poll(a), None);
        assert_eq!(events.poll(b), Some(true));
        assert!(events.close(b));
        assert!(!events.close(b));
        assert!(
            !events
                .create(Some(b"Hover".to_vec()), true, false)
                .unwrap()
                .1
        );
    }
    #[test]
    fn manual_reset_events_remain_signaled_and_names_are_case_sensitive() {
        let mut events = Events::default();
        let (a, _) = events.create(Some(b"Hover".to_vec()), true, true).unwrap();
        assert_eq!(events.poll(a), Some(true));
        assert_eq!(events.poll(a), Some(true));
        assert!(
            !events
                .create(Some(b"hover".to_vec()), true, false)
                .unwrap()
                .1
        );
        events.signal(a, false).unwrap();
        assert_eq!(events.poll(a), Some(false));
    }
}
