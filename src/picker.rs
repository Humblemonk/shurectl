//! Device picker: the screen shown at startup when `--device` names none and
//! there are several supported devices connected, or none yet. With none it
//! waits, and `main.rs` opens one plugged in by itself when it is the only
//! one and it opened cleanly (`only_device()`).
//! A device refused as not genuine (`device::NOT_GENUINE`) is listed here
//! too, even alone, so its owner reads why instead of a failed readback.
//! This is its state and keys; `ui::draw_picker()` draws it, and `main.rs`
//! opens the device it returns.

use crossterm::event::{KeyCode, KeyModifiers};

use crate::protocol::DeviceModel;

/// One connected device, as read once when the picker lists it.
pub struct PickerRow {
    pub path: String,
    pub model: DeviceModel,
    /// The serial printed on the device, or the USB one when it couldn't be
    /// read; `None` when it has neither.
    pub serial: Option<String>,
    /// The user-set device name, on models that have one.
    pub name: Option<String>,
    /// Why the device couldn't be opened to read its labels, such as MOTIV
    /// holding it. Opening it may still work later, unless it is `not_genuine`.
    pub problem: Option<String>,
    /// Refused as not genuine (`device::NOT_GENUINE`, also its `problem`):
    /// it is never opened, and the picker says why under the devices.
    pub not_genuine: bool,
}

pub struct Picker {
    pub rows: Vec<PickerRow>,
    pub selected: usize,
    pub status: String,
    pub status_is_error: bool,
}

/// What a key or click asks `main.rs` to do.
#[derive(Debug, PartialEq)]
pub enum Command {
    /// Open the device in this row.
    Open(usize),
    /// List the devices again.
    Rescan,
    Quit,
}

impl Picker {
    pub fn new(rows: Vec<PickerRow>) -> Self {
        let mut picker = Self {
            rows: Vec::new(),
            selected: 0,
            status: String::new(),
            status_is_error: false,
        };
        picker.set_rows(rows);
        picker
    }

    /// Replace the rows after a rescan, keeping the selected device selected
    /// when it is still there.
    pub fn set_rows(&mut self, rows: Vec<PickerRow>) {
        self.selected = self
            .rows
            .get(self.selected)
            .and_then(|old| rows.iter().position(|row| row.path == old.path))
            // A refused device stays selected only while nothing else is
            // listed, so a device that can be opened shows its own status.
            .filter(|kept| !rows[*kept].not_genuine)
            .or_else(|| rows.iter().position(|row| !row.not_genuine))
            .unwrap_or(0);
        self.rows = rows;
        let found = match self.rows.len() {
            0 => "Waiting for a Shure device…".to_string(),
            1 => "1 Shure device found.".to_string(),
            n => format!("{n} Shure devices found."),
        };
        self.set_ok(found);
    }

    /// The row of the only device listed, when that device read its labels
    /// without a problem. `main.rs` asks after a scan that started
    /// with no devices listed, so a mic plugged in while the picker waits
    /// opens by itself, as a single device does at startup.
    /// Devices refused as not genuine don't count: the user has nothing to
    /// choose between.
    pub fn only_device(&self) -> Option<usize> {
        let mut openable = self
            .rows
            .iter()
            .enumerate()
            .filter(|(_, row)| !row.not_genuine);
        match (openable.next(), openable.next()) {
            (Some((index, only)), None) if only.problem.is_none() => Some(index),
            _ => None,
        }
    }

    /// Whether no listed device can be opened: none is listed, or only ones
    /// refused as not genuine. `main.rs` then keeps watching for one plugged
    /// in, which opens by itself.
    pub fn is_waiting(&self) -> bool {
        self.rows.iter().all(|row| row.not_genuine)
    }

    fn set_status(&mut self, message: impl Into<String>, is_error: bool) {
        self.status = message.into();
        self.status_is_error = is_error;
    }

    pub fn set_ok(&mut self, message: impl Into<String>) {
        self.set_status(message, false);
    }

    pub fn set_err(&mut self, message: impl Into<String>) {
        self.set_status(message, true);
    }

    /// The command a key asks for, after moving the selection.
    pub fn handle_key(&mut self, code: KeyCode, mods: KeyModifiers) -> Option<Command> {
        match code {
            KeyCode::Char('q') | KeyCode::Char('Q') => Some(Command::Quit),
            KeyCode::Char('c') if mods.contains(KeyModifiers::CONTROL) => Some(Command::Quit),
            KeyCode::Up | KeyCode::Char('k') => {
                self.selected = self.selected.saturating_sub(1);
                None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.selected = (self.selected + 1).min(self.rows.len().saturating_sub(1));
                None
            }
            KeyCode::Enter | KeyCode::Char(' ') => {
                (self.selected < self.rows.len()).then_some(Command::Open(self.selected))
            }
            KeyCode::Char('r') => Some(Command::Rescan),
            _ => None,
        }
    }

    /// A click on row `index` selects it; a click on the selected row opens
    /// it, as a click on a control does in the main screen.
    pub fn click(&mut self, index: usize) -> Option<Command> {
        if index >= self.rows.len() {
            return None;
        }
        if index == self.selected {
            return Some(Command::Open(index));
        }
        self.selected = index;
        None
    }
}

/// Rows for tests and the golden renders: two devices, the second named.
#[cfg(test)]
pub fn sample_rows() -> Vec<PickerRow> {
    vec![
        PickerRow {
            path: "/dev/hidraw3".into(),
            model: DeviceModel::Mv7Plus,
            serial: Some("3EK26001276".into()),
            name: Some("Podcast mic".into()),
            problem: None,
            not_genuine: false,
        },
        PickerRow {
            path: "/dev/hidraw5".into(),
            model: DeviceModel::Mvx2uGen2,
            serial: Some("4FA19000331".into()),
            name: None,
            problem: None,
            not_genuine: false,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    const NONE: KeyModifiers = KeyModifiers::NONE;

    #[test]
    fn arrows_move_within_the_list_and_enter_opens() {
        let mut picker = Picker::new(sample_rows());
        assert_eq!(picker.handle_key(KeyCode::Up, NONE), None);
        assert_eq!(picker.selected, 0);
        picker.handle_key(KeyCode::Char('j'), NONE);
        picker.handle_key(KeyCode::Down, NONE);
        assert_eq!(picker.selected, 1);
        assert_eq!(
            picker.handle_key(KeyCode::Enter, NONE),
            Some(Command::Open(1))
        );
        picker.handle_key(KeyCode::Char('k'), NONE);
        assert_eq!(
            picker.handle_key(KeyCode::Char(' '), NONE),
            Some(Command::Open(0))
        );
    }

    #[test]
    fn quit_and_rescan_keys() {
        let mut picker = Picker::new(sample_rows());
        assert_eq!(
            picker.handle_key(KeyCode::Char('q'), NONE),
            Some(Command::Quit)
        );
        assert_eq!(
            picker.handle_key(KeyCode::Char('c'), KeyModifiers::CONTROL),
            Some(Command::Quit)
        );
        assert_eq!(picker.handle_key(KeyCode::Char('c'), NONE), None);
        assert_eq!(
            picker.handle_key(KeyCode::Char('r'), NONE),
            Some(Command::Rescan)
        );
    }

    #[test]
    fn a_click_selects_and_a_second_click_opens() {
        let mut picker = Picker::new(sample_rows());
        assert_eq!(picker.click(1), None);
        assert_eq!(picker.selected, 1);
        assert_eq!(picker.click(1), Some(Command::Open(1)));
        assert_eq!(picker.click(2), None);
        assert_eq!(picker.selected, 1);
    }

    #[test]
    fn an_empty_list_opens_nothing() {
        let mut picker = Picker::new(Vec::new());
        assert!(picker.status.contains("Waiting for a Shure device"));
        picker.handle_key(KeyCode::Down, NONE);
        assert_eq!(picker.selected, 0);
        assert_eq!(picker.handle_key(KeyCode::Enter, NONE), None);
    }

    #[test]
    fn only_a_single_device_that_opened_cleanly_opens_by_itself() {
        let mut picker = Picker::new(Vec::new());
        assert_eq!(picker.only_device(), None);
        picker.set_rows(sample_rows());
        assert_eq!(picker.only_device(), None);
        let mut rows: Vec<_> = sample_rows().into_iter().take(1).collect();
        picker.set_rows(rows);
        assert_eq!(picker.only_device(), Some(0));
        rows = sample_rows().into_iter().take(1).collect();
        rows[0].problem = Some("Cannot open /dev/hidraw3: device busy".into());
        picker.set_rows(rows);
        assert_eq!(picker.only_device(), None);
        // A refused device beside it doesn't stop the only real one opening.
        rows = sample_rows();
        rows[0].problem = Some("Not genuine".into());
        rows[0].not_genuine = true;
        picker.set_rows(rows);
        assert_eq!(picker.only_device(), Some(1));
    }

    #[test]
    fn a_picker_with_only_refused_devices_still_waits() {
        let mut picker = Picker::new(Vec::new());
        assert!(picker.is_waiting());
        let mut rows = sample_rows();
        rows[0].not_genuine = true;
        picker.set_rows(rows);
        assert!(!picker.is_waiting());
        // The device that can be opened is selected, not the refused one.
        assert_eq!(picker.selected, 1);
        let mut rows: Vec<_> = sample_rows().into_iter().take(1).collect();
        rows[0].not_genuine = true;
        picker.set_rows(rows);
        assert!(picker.is_waiting());
    }

    #[test]
    fn a_rescan_keeps_the_selected_device() {
        let mut picker = Picker::new(sample_rows());
        picker.selected = 1;
        picker.set_err("Cannot open device");
        let mut rows = sample_rows();
        rows.reverse();
        picker.set_rows(rows);
        assert_eq!(picker.rows[picker.selected].path, "/dev/hidraw5");
        assert!(!picker.status_is_error);
        picker.set_rows(sample_rows().into_iter().take(1).collect());
        assert_eq!(picker.selected, 0);
    }
}
