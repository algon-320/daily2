use crate::daily::{Command, Modifier};

pub const HOT_KEY: Modifier = Modifier::Super;

pub const WINDOW_BORDER_WIDTH: u32 = 1;

pub const SNAPPING_WIDTH: u32 = 64;

// This program will be run in shell when a monitor is connected or disconnected
// Expected usage is to specify a script that updates monitor layout using xrandr utility.
pub const MONITOR_UPDATE_PROG: Option<&str> = Some(r#"echo 'monitor changed'"#);

// maximum number of the virtual desktops
pub const NUM_DESKTOPS: usize = 20;

const KEYCODE_1: u8 = 10;
const KEYCODE_2: u8 = 11;
const KEYCODE_3: u8 = 12;
const KEYCODE_4: u8 = 13;
const KEYCODE_5: u8 = 14;
const KEYCODE_6: u8 = 15;
const KEYCODE_7: u8 = 16;
const KEYCODE_8: u8 = 17;
const KEYCODE_9: u8 = 18;
const KEYCODE_0: u8 = 19;
const KEYCODE_TAB: u8 = 23;
const KEYCODE_Q: u8 = 24;
const KEYCODE_R: u8 = 27;
const KEYCODE_T: u8 = 28;
const KEYCODE_P: u8 = 33;
const KEYCODE_S: u8 = 39;
const KEYCODE_J: u8 = 44;

pub fn keybindings() -> Vec<(&'static [Modifier], u8, Command)> {
    #[rustfmt::skip]
    let mut list: Vec<(&[Modifier], _, _)> = vec![
        // keys to exit the WM
        (&[HOT_KEY, Modifier::Shift], KEYCODE_Q, Command::Exit),

        // keys to restart the WM
        (&[HOT_KEY, Modifier::Shift], KEYCODE_R, Command::Restart),

        // keys to change the input focus to another monitor
        (&[HOT_KEY], KEYCODE_J, Command::FocusNextMonitor),

        // keys to change the input focus to another window on the same screen
        (&[HOT_KEY], KEYCODE_TAB, Command::FocusNextWindow),

        // keys to toggle floating mode of the focused window
        (&[HOT_KEY], KEYCODE_S, Command::ToggleFloating),

        // dmenu_run
        (&[HOT_KEY], KEYCODE_P, Command::SpawnProcess("/usr/bin/dmenu_run".into())),

        // terminal
        (&[HOT_KEY], KEYCODE_T, Command::SpawnProcess("/usr/bin/xterm".into())),
    ];

    let digit_keys = [
        KEYCODE_1, KEYCODE_2, KEYCODE_3, KEYCODE_4, KEYCODE_5, KEYCODE_6, KEYCODE_7, KEYCODE_8,
        KEYCODE_9, KEYCODE_0,
    ];
    for (i, kc) in digit_keys.into_iter().enumerate() {
        list.push((&[HOT_KEY], kc, Command::ChangeDesktop(i)));
        list.push((&[HOT_KEY, Modifier::Shift], kc, Command::MoveWindow(i)));
    }

    list
}
