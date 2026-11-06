//! State shared by BUTTON messages, dialog helpers and control rendering.
#[derive(Debug, Default, Clone, Copy)]
pub struct ButtonState {
    pub checked: u32,
    pub pressed: bool,
}
impl ButtonState {
    pub fn check(&self, style: u32) -> u32 {
        if matches!(style & 15, 2..=6 | 9) {
            self.checked
        } else {
            0
        }
    }
    pub fn set_check(&mut self, style: u32, value: u32) {
        if matches!(style & 15, 2..=6 | 9) {
            let maximum = if matches!(style & 15, 5 | 6) { 2 } else { 1 };
            if value <= maximum {
                self.checked = value;
            }
        }
    }
    pub fn state(&self, style: u32, focused: bool) -> u32 {
        self.check(style) | if self.pressed { 4 } else { 0 } | if focused { 8 } else { 0 }
    }
    pub fn activate(&mut self, style: u32) {
        match style & 15 {
            3 => self.checked ^= 1,
            6 => self.checked = (self.checked + 1) % 3,
            9 => self.checked = 1,
            _ => {}
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn manual_and_automatic_checks_preserve_control_type() {
        let mut button = ButtonState::default();
        button.set_check(0, 1);
        assert_eq!(button.check(0), 0);
        button.set_check(2, 1);
        button.activate(2);
        assert_eq!(button.check(2), 1);
        button.set_check(2, 2);
        assert_eq!(button.check(2), 1);
        button.activate(3);
        assert_eq!(button.check(3), 0);
        button.activate(3);
        assert_eq!(button.check(3), 1);
        button.set_check(6, 0);
        for expected in [1, 2, 0] {
            button.activate(6);
            assert_eq!(button.check(6), expected);
        }
        button.pressed = true;
        assert_eq!(button.state(6, true), 12);
        button.activate(9);
        assert_eq!(button.state(9, false), 5);
    }
}
