mod daily;
mod error;

fn main() {
    env_logger::init();

    let mut daily = daily::Daily::new().expect("failed to initialize daily");

    // NOTE: it might be better to move the management of keybindings into another process
    //       and use D-bus like message passing for controling the window manager.
    use x11rb::protocol::xproto::ModMask;
    let shift_super = ModMask::SHIFT | ModMask::M4;
    let super_ = ModMask::M4;
    let keycode_1 = 10;
    let keycode_2 = 11;
    let keycode_3 = 12;
    let keycode_tab = 23;
    let keycode_q = 24;
    let keycode_r = 27;
    let keycode_t = 28;
    let keycode_p = 33;
    let keycode_j = 44;
    daily
        .bind_key(shift_super, keycode_q, daily::Command::Exit)
        .expect("failed to add a keybinding for Exit command");
    daily
        .bind_key(shift_super, keycode_r, daily::Command::Restart)
        .expect("failed to add a keybinding for Restart command");
    let cmd_dmenu = daily::Command::SpawnProcess("/usr/bin/dmenu_run".to_owned());
    daily
        .bind_key(super_, keycode_p, cmd_dmenu)
        .expect("failed to add a keybinding for dmenu");
    let cmd_term = daily::Command::SpawnProcess("/usr/bin/alacritty".to_owned());
    daily
        .bind_key(super_, keycode_t, cmd_term)
        .expect("failed to add a keybinding for terminal");
    daily
        .bind_key(super_, keycode_j, daily::Command::FocusNextMonitor)
        .expect("failed to add a keybinding for FocusNextMonitor command");
    daily
        .bind_key(super_, keycode_tab, daily::Command::FocusNextWindow)
        .expect("failed to add a keybinding for FocusNextWindow command");

    daily
        .bind_key(super_, keycode_1, daily::Command::ChangeScreen(0))
        .expect("failed to add a keybinding for ChangeScreen(0) command");
    daily
        .bind_key(super_, keycode_2, daily::Command::ChangeScreen(1))
        .expect("failed to add a keybinding for ChangeScreen(1) command");
    daily
        .bind_key(super_, keycode_3, daily::Command::ChangeScreen(2))
        .expect("failed to add a keybinding for ChangeScreen(2) command");

    daily
        .bind_key(shift_super, keycode_1, daily::Command::MoveWindow(0))
        .expect("failed to add a keybinding for MoveWindow(0) command");
    daily
        .bind_key(shift_super, keycode_2, daily::Command::MoveWindow(1))
        .expect("failed to add a keybinding for MoveWindow(1) command");
    daily
        .bind_key(shift_super, keycode_3, daily::Command::MoveWindow(2))
        .expect("failed to add a keybinding for MoveWindow(2) command");

    log::info!("start");
    match daily.start() {
        Ok(()) => {}

        Err(error::Error::Interrupted { restart }) => {
            if restart {
                log::info!("try to restart");
                std::process::exit(2);
            }
        }

        Err(error::Error::X11(x11_err)) => {
            log::error!("{x11_err:?}");
            std::process::exit(1);
        }
    }
    log::info!("stop");
}
