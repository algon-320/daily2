mod daily;
mod utils;
mod error;

fn main() {
    env_logger::init();

    let mut daily = daily::Daily::new().expect("failed to initialize daily");

    // NOTE: it might be better to move the management of keybindings into another process
    //       and use D-bus like message passing for controling the window manager.
    {
        use x11rb::protocol::xproto::ModMask;

        let super_shift = ModMask::M4 | ModMask::SHIFT;
        let super_ = ModMask::M4;

        let keycode_1 = 10;
        let keycode_2 = 11;
        let keycode_3 = 12;
        let keycode_4 = 13;
        let keycode_5 = 14;
        let keycode_6 = 15;
        let keycode_7 = 16;
        let keycode_8 = 17;
        let keycode_9 = 18;
        let keycode_0 = 19;
        let keycode_tab = 23;
        let keycode_q = 24;
        let keycode_r = 27;
        let keycode_t = 28;
        let keycode_p = 33;
        let keycode_s = 39;
        let keycode_j = 44;

        // FIXME: config
        daily
            .bind_key(super_shift, keycode_q, daily::Command::Exit)
            .expect("failed to add a keybinding for Exit command");
        daily
            .bind_key(super_shift, keycode_r, daily::Command::Restart)
            .expect("failed to add a keybinding for Restart command");

        // FIXME
        let cmd_dmenu = daily::Command::SpawnProcess("/home/algon/scripts/dmenu/run.sh".to_owned());
        daily
            .bind_key(super_, keycode_p, cmd_dmenu)
            .expect("failed to add a keybinding for dmenu");
        let cmd_term = daily::Command::SpawnProcess("/home/algon/.cargo/bin/toyterm".to_owned());
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
            .bind_key(super_, keycode_s, daily::Command::ToggleFloating)
            .expect("failed to add a keybinding for ToggleFloating command");

        let digit_keys = [
            keycode_1, keycode_2, keycode_3, keycode_4, keycode_5, keycode_6, keycode_7, keycode_8,
            keycode_9, keycode_0,
        ];
        for (i, kc) in digit_keys.into_iter().enumerate() {
            daily
                .bind_key(super_, kc, daily::Command::ChangeScreen(i))
                .expect("failed to add a keybinding for ChangeScreen command");
            daily
                .bind_key(super_shift, kc, daily::Command::MoveWindow(i))
                .expect("failed to add a keybinding for MoveWindow command");
        }
    }

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
