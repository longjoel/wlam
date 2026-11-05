//! Single-thread USER32 state. Callbacks are requests, never calls under a borrow.
use crate::objects::Handles;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
pub const WM_CREATE: u32 = 1;
pub const WM_DESTROY: u32 = 2;
pub const WM_PAINT: u32 = 15;
pub const WM_QUIT: u32 = 18;
pub const WM_SIZE: u32 = 5;
pub const WM_NCCREATE: u32 = 129;
pub const WM_NCDESTROY: u32 = 130;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Message {
    pub window: u32,
    pub message: u32,
    pub wparam: u32,
    pub lparam: u32,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    Return(u32),
    Invoke {
        token: u64,
        procedure: u32,
        args: [u32; 4],
    },
    Wait,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WindowError {
    InvalidHandle,
    InvalidClass,
    InvalidDimensions,
    InvalidContinuation,
    Exhausted,
    PaintState,
    UnsupportedControl,
}
#[derive(Debug)]
pub struct Window {
    pub procedure: u32,
    pub width: u32,
    pub height: u32,
    pub visible: bool,
    pub invalid: bool,
    pub painting: bool,
    pub parent: u32,
    pub id: u32,
    pub x: i32,
    pub y: i32,
    pub style: u32,
}
#[derive(Debug)]
enum Pending {
    Return,
    Create {
        window: u32,
        parameter: u32,
        nonclient: bool,
    },
    Destroy {
        window: u32,
        nonclient: bool,
    },
}
#[derive(Debug)]
pub struct Windows {
    classes: BTreeMap<Vec<u8>, u32>,
    common_controls: BTreeSet<Vec<u8>>,
    windows: Handles<Window>,
    order: Vec<u32>,
    queue: VecDeque<Message>,
    pending: BTreeMap<u64, Pending>,
    next_token: u64,
    pub last_error: u32,
}
impl Default for Windows {
    fn default() -> Self {
        Self {
            classes: BTreeMap::new(),
            common_controls: BTreeSet::new(),
            windows: Handles::with_namespace(1),
            order: Vec::new(),
            queue: VecDeque::new(),
            pending: BTreeMap::new(),
            next_token: 0,
            last_error: 0,
        }
    }
}
impl Windows {
    pub fn cancel_create(&mut self, window: u32) {
        self.windows.remove(window);
        self.order.retain(|handle| *handle != window);
        self.queue.retain(|message| message.window != window);
        self.pending.retain(
            |_, pending| !matches!(pending,Pending::Create {window:handle,..} if *handle==window),
        );
    }
    pub fn reserve_queue(&mut self, messages: u32) -> bool {
        self.queue.try_reserve(messages as usize).is_ok()
    }
    pub fn initialize_common_controls(&mut self) {
        for name in [
            "ToolbarWindow32",
            "msctls_statusbar32",
            "msctls_progress32",
            "msctls_trackbar32",
            "msctls_updown32",
            "SysListView32",
            "SysTreeView32",
            "SysTabControl32",
            "SysHeader32",
            "tooltips_class32",
        ] {
            self.common_controls.insert(name.as_bytes().to_vec());
        }
    }
    pub fn register(&mut self, name: Vec<u8>, procedure: u32) -> Result<(), WindowError> {
        if name.is_empty() || procedure == 0 || self.classes.contains_key(&name) {
            return Err(WindowError::InvalidClass);
        }
        self.classes.insert(name, procedure);
        Ok(())
    }
    pub fn get(&self, window: u32) -> Result<&Window, WindowError> {
        self.windows.get(window).ok_or(WindowError::InvalidHandle)
    }
    pub fn configure(
        &mut self,
        handle: u32,
        parent: u32,
        id: u32,
        x: i32,
        y: i32,
        style: u32,
    ) -> Result<(), WindowError> {
        if parent != 0 {
            self.get(parent)?;
        }
        let window = self
            .windows
            .get_mut(handle)
            .ok_or(WindowError::InvalidHandle)?;
        window.parent = parent;
        window.id = id;
        window.x = x;
        window.y = y;
        window.style = style;
        window.visible = style & 0x1000_0000 != 0;
        Ok(())
    }
    pub fn is_visible(&self, mut handle: u32) -> bool {
        if handle == 0 { return false; }
        let mut depth = 0;
        while handle != 0 {
            let Some(window) = self.windows.get(handle) else { return false; };
            if !window.visible { return false; }
            handle = window.parent;
            depth += 1;
            if depth > 64 { return false; }
        }
        true
    }
    pub fn top_child(&self, parent: u32) -> u32 {
        self.order
            .iter()
            .rev()
            .find(|handle| {
                self.windows
                    .get(**handle)
                    .is_some_and(|window| window.parent == parent)
            })
            .copied()
            .unwrap_or(0)
    }
    pub fn foreground_window(&self) -> u32 {
        self.order.iter().rev().find_map(|handle| {
            self.windows.get(*handle).and_then(|window| {
                (window.parent == 0 && self.is_visible(*handle)).then_some(*handle)
            })
        }).unwrap_or(0)
    }
    pub fn related(&self, handle: u32, command: u32) -> u32 {
        let Some(window) = self.windows.get(handle) else {
            return 0;
        };
        let parent = window.parent;
        let peers = self
            .order
            .iter()
            .filter(|h| self.windows.get(**h).is_some_and(|w| w.parent == parent))
            .copied()
            .collect::<Vec<_>>();
        let Some(index) = peers.iter().position(|h| *h == handle) else {
            return 0;
        };
        match command {
            0 => peers.first().copied().unwrap_or(0),
            1 => peers.last().copied().unwrap_or(0),
            2 => peers.get(index + 1).copied().unwrap_or(0),
            3 => index
                .checked_sub(1)
                .and_then(|i| peers.get(i).copied())
                .unwrap_or(0),
            4 => 0,
            5 => self.top_child(handle),
            6 => 0,
            _ => 0,
        }
    }
    pub fn screen_origin(&self, mut handle: u32) -> Option<(i32, i32)> {
        let mut x = 0i32;
        let mut y = 0i32;
        let mut depth = 0;
        while handle != 0 {
            let w = self.windows.get(handle)?;
            x = x.saturating_add(w.x);
            y = y.saturating_add(w.y);
            handle = w.parent;
            depth += 1;
            if depth > 64 {
                return None;
            }
        }
        Some((x, y))
    }
    pub fn set_procedure(&mut self, window: u32, procedure: u32) -> Result<u32, WindowError> {
        let window = self
            .windows
            .get_mut(window)
            .ok_or(WindowError::InvalidHandle)?;
        Ok(std::mem::replace(&mut window.procedure, procedure))
    }
    fn invoke(&mut self, message: Message, pending: Pending) -> Result<Action, WindowError> {
        let procedure = self.get(message.window)?.procedure;
        let token = self
            .next_token
            .checked_add(1)
            .ok_or(WindowError::Exhausted)?;
        self.next_token = token;
        self.pending.insert(token, pending);
        Ok(Action::Invoke {
            token,
            procedure,
            args: [
                message.window,
                message.message,
                message.wparam,
                message.lparam,
            ],
        })
    }
    /// `parameter` points to a driver-owned guest CREATESTRUCTA which must stay
    /// alive until this creation continuation completes.
    pub fn create(
        &mut self,
        class: &[u8],
        width: u32,
        height: u32,
        parameter: u32,
    ) -> Result<Action, WindowError> {
        let procedure = *self.classes.get(class).ok_or_else(|| {
            if self.common_controls.contains(class) {
                WindowError::UnsupportedControl
            } else {
                WindowError::InvalidClass
            }
        })?;
        if width == 0 || height == 0 || width > i32::MAX as u32 || height > i32::MAX as u32 {
            return Err(WindowError::InvalidDimensions);
        }
        let window = self
            .windows
            .insert(Window {
                procedure,
                width,
                height,
                visible: false,
                invalid: true,
                painting: false,
                parent: 0,
                id: 0,
                x: 0,
                y: 0,
                style: 0,
            })
            .ok_or(WindowError::Exhausted)?;
        self.order.push(window);
        self.invoke(
            Message {
                window,
                message: WM_NCCREATE,
                wparam: 0,
                lparam: parameter,
            },
            Pending::Create {
                window,
                parameter,
                nonclient: true,
            },
        )
    }
    /// Create an OS-managed control window. It participates in USER32 lookup,
    /// sizing, and painting, while its class procedure is supplied by the host.
    pub fn create_builtin(
        &mut self,
        procedure: u32,
        parent: u32,
        id: u32,
        x: i32,
        y: i32,
        width: u32,
        height: u32,
        style: u32,
    ) -> Result<u32, WindowError> {
        if width == 0 || height == 0 || width > i32::MAX as u32 || height > i32::MAX as u32 {
            return Err(WindowError::InvalidDimensions);
        }
        if parent != 0 {
            self.get(parent)?;
        }
        let window = self
            .windows
            .insert(Window {
                procedure,
                width,
                height,
                visible: true,
                invalid: true,
                painting: false,
                parent,
                id,
                x,
                y,
                style,
            })
            .ok_or(WindowError::Exhausted)?;
        self.order.push(window);
        Ok(window)
    }
    pub fn resume(&mut self, token: u64, result: u32) -> Result<Action, WindowError> {
        let pending = self
            .pending
            .remove(&token)
            .ok_or(WindowError::InvalidContinuation)?;
        match pending {
            Pending::Return => Ok(Action::Return(result)),
            Pending::Create {
                window,
                parameter,
                nonclient,
            } => {
                if (nonclient && result == 0) || (!nonclient && result == u32::MAX) {
                    self.windows.remove(window);
                    self.order.retain(|h| *h != window);
                    self.queue.retain(|m| m.window != window);
                    return Ok(Action::Return(0));
                }
                self.get(window)?;
                if nonclient {
                    self.invoke(
                        Message {
                            window,
                            message: WM_CREATE,
                            wparam: 0,
                            lparam: parameter,
                        },
                        Pending::Create {
                            window,
                            parameter,
                            nonclient: false,
                        },
                    )
                } else {
                    Ok(Action::Return(window))
                }
            }
            Pending::Destroy { window, nonclient } => {
                if !nonclient {
                    self.invoke(
                        Message {
                            window,
                            message: WM_NCDESTROY,
                            wparam: 0,
                            lparam: 0,
                        },
                        Pending::Destroy {
                            window,
                            nonclient: true,
                        },
                    )
                } else {
                    self.windows
                        .remove(window)
                        .ok_or(WindowError::InvalidHandle)?;
                    self.order.retain(|h| *h != window);
                    self.queue.retain(|m| m.window != window);
                    Ok(Action::Return(1))
                }
            }
        }
    }
    pub fn send(&mut self, message: Message) -> Result<Action, WindowError> {
        self.invoke(message, Pending::Return)
    }
    pub fn destroy(&mut self, window: u32) -> Result<Action, WindowError> {
        self.invoke(
            Message {
                window,
                message: WM_DESTROY,
                wparam: 0,
                lparam: 0,
            },
            Pending::Destroy {
                window,
                nonclient: false,
            },
        )
    }
    pub fn show(&mut self, window: u32, visible: bool) -> Result<bool, WindowError> {
        let w = self
            .windows
            .get_mut(window)
            .ok_or(WindowError::InvalidHandle)?;
        let previous = w.visible;
        w.visible = visible;
        Ok(previous)
    }
    /// Move a valid window to the top of its sibling Z order.
    pub fn bring_to_top(&mut self, window: u32) -> bool {
        if self.windows.get(window).is_none() {
            return false;
        }
        self.order.retain(|handle| *handle != window);
        self.order.push(window);
        true
    }
    pub fn move_window(
        &mut self,
        window: u32,
        x: i32,
        y: i32,
        width: u32,
        height: u32,
    ) -> Result<(), WindowError> {
        if width == 0 || height == 0 || width > i32::MAX as u32 || height > i32::MAX as u32 {
            return Err(WindowError::InvalidDimensions);
        }
        let w = self
            .windows
            .get_mut(window)
            .ok_or(WindowError::InvalidHandle)?;
        w.x = x;
        w.y = y;
        w.width = width;
        w.height = height;
        w.invalid = true;
        Ok(())
    }
    pub fn invalidate(&mut self, window: u32) -> Result<(), WindowError> {
        self.windows
            .get_mut(window)
            .ok_or(WindowError::InvalidHandle)?
            .invalid = true;
        Ok(())
    }
    pub fn validate_paint(&mut self, window: u32) -> bool {
        let Some(w) = self.windows.get_mut(window) else {
            return false;
        };
        w.invalid = false;
        true
    }
    pub fn begin_paint(&mut self, window: u32) -> Result<(u32, u32), WindowError> {
        let w = self
            .windows
            .get_mut(window)
            .ok_or(WindowError::InvalidHandle)?;
        if w.painting {
            return Err(WindowError::PaintState);
        }
        w.painting = true;
        w.invalid = false;
        Ok((w.width, w.height))
    }
    pub fn end_paint(&mut self, window: u32) -> Result<(), WindowError> {
        let w = self
            .windows
            .get_mut(window)
            .ok_or(WindowError::InvalidHandle)?;
        if !w.painting {
            return Err(WindowError::PaintState);
        }
        w.painting = false;
        Ok(())
    }
    pub fn post(&mut self, message: Message) -> Result<(), WindowError> {
        self.get(message.window)?;
        self.queue.push_back(message);
        Ok(())
    }
    pub fn post_quit(&mut self, code: u32) {
        self.queue.push_back(Message {
            window: 0,
            message: WM_QUIT,
            wparam: code,
            lparam: 0,
        });
    }
    /// Posted input precedes synthesized paint. Paint stays pending until
    /// BeginPaint validates the region; merely retrieving WM_PAINT doesn't.
    pub fn peek(&mut self, remove: bool) -> Option<Message> {
        self.peek_filtered(0, 0, 0, remove)
    }

    /// Retrieve the first queued message matching PeekMessage/GetMessage
    /// filters. `window == 0` selects every window; `u32::MAX` selects
    /// thread messages. A zero filter bound is open ended, and WM_QUIT is
    /// always returned as required by USER32.
    pub fn peek_filtered(
        &mut self,
        window: u32,
        filter_min: u32,
        filter_max: u32,
        remove: bool,
    ) -> Option<Message> {
        let index = self.queue.iter().position(|message| {
            if message.message == WM_QUIT {
                return true;
            }
            let window_matches = match window {
                0 => true,
                u32::MAX => message.window == 0,
                handle => message.window == handle,
            };
            let message_matches = match (filter_min, filter_max) {
                (0, 0) => true,
                (0, max) => message.message <= max,
                (min, 0) => message.message >= min,
                (min, max) => (min..=max).contains(&message.message),
            };
            window_matches && message_matches
        });
        if let Some(index) = index {
            return if remove {
                self.queue.remove(index)
            } else {
                self.queue.get(index).copied()
            };
        }
        self.order.iter().find_map(|h| {
            let w = self.windows.get(*h)?;
            let window_matches = window == 0 || window == *h;
            let message_matches = (filter_min == 0 && filter_max == 0)
                || (filter_min == 0 && WM_PAINT <= filter_max)
                || (filter_max == 0 && WM_PAINT >= filter_min)
                || (filter_min..=filter_max).contains(&WM_PAINT);
            (window_matches
                && message_matches
                && self.is_visible(*h)
                && w.invalid
                && !w.painting)
                .then_some(Message {
                window: *h,
                message: WM_PAINT,
                wparam: 0,
                lparam: 0,
            })
        })
    }
    pub fn default_result(message: u32) -> u32 {
        if message == WM_NCCREATE {
            1
        } else {
            0
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn token(action: Action, message: u32) -> (u64, u32) {
        match action {
            Action::Invoke {
                token,
                procedure,
                args,
            } => {
                assert_eq!(procedure, 0x401000);
                assert_eq!(args[1], message);
                (token, args[0])
            }
            _ => panic!("expected callback"),
        }
    }
    fn create(w: &mut Windows) -> u32 {
        w.register(b"fixture".to_vec(), 0x401000).unwrap();
        let (nc, hwnd) = token(w.create(b"fixture", 16, 12, 0x5000).unwrap(), WM_NCCREATE);
        let (client, _) = token(w.resume(nc, 1).unwrap(), WM_CREATE);
        assert_eq!(w.resume(client, 0), Ok(Action::Return(hwnd)));
        hwnd
    }
    #[test]
    fn callbacks_allow_reentry_and_preserve_outer_continuations() {
        let mut w = Windows::default();
        let hwnd = create(&mut w);
        let (outer, _) = token(
            w.send(Message {
                window: hwnd,
                message: 100,
                wparam: 7,
                lparam: 8,
            })
            .unwrap(),
            100,
        );
        let (inner, _) = token(
            w.send(Message {
                window: hwnd,
                message: 101,
                wparam: 0,
                lparam: 0,
            })
            .unwrap(),
            101,
        );
        assert_eq!(w.resume(inner, 42), Ok(Action::Return(42)));
        assert_eq!(w.resume(outer, 9), Ok(Action::Return(9)));
        assert_eq!(w.resume(outer, 9), Err(WindowError::InvalidContinuation));
    }
    #[test]
    fn input_precedes_paint_and_begin_paint_validates() {
        let mut w = Windows::default();
        let hwnd = create(&mut w);
        w.show(hwnd, true).unwrap();
        let click = Message {
            window: hwnd,
            message: 0x201,
            wparam: 1,
            lparam: 0x30002,
        };
        w.post(click).unwrap();
        assert_eq!(w.peek(false), Some(click));
        assert_eq!(w.peek(true), Some(click));
        assert_eq!(w.peek(true).unwrap().message, WM_PAINT);
        assert_eq!(w.begin_paint(hwnd), Ok((16, 12)));
        assert_eq!(w.peek(true), None);
        w.invalidate(hwnd).unwrap();
        w.end_paint(hwnd).unwrap();
        assert_eq!(w.peek(true).unwrap().message, WM_PAINT);
        w.post_quit(5);
        assert_eq!(w.peek(true).unwrap().wparam, 5);
    }
    #[test]
    fn rejected_creation_and_destroyed_handles_are_invalid() {
        let mut w = Windows::default();
        w.register(b"fixture".to_vec(), 0x401000).unwrap();
        let (t, hwnd) = token(w.create(b"fixture", 1, 1, 0).unwrap(), WM_NCCREATE);
        assert_eq!(w.resume(t, 0), Ok(Action::Return(0)));
        assert!(w.get(hwnd).is_err());
        let (t, hwnd2) = token(w.create(b"fixture", 1, 1, 0).unwrap(), WM_NCCREATE);
        let (t, _) = token(w.resume(t, 1).unwrap(), WM_CREATE);
        w.resume(t, 0).unwrap();
        assert_ne!(hwnd, hwnd2);
        let (t, _) = token(w.destroy(hwnd2).unwrap(), WM_DESTROY);
        let (t, _) = token(w.resume(t, 0).unwrap(), WM_NCDESTROY);
        assert_eq!(w.resume(t, 0), Ok(Action::Return(1)));
        assert!(w.get(hwnd2).is_err());
    }
}
