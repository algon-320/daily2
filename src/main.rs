mod daily;
mod error;

fn main() {
    env_logger::init();

    let mut daily = daily::Daily::new().expect("failed to initialize daily");

    // NOTE: it might be better to move the management of keybindings into another process
    //       and use D-bus like message passing for controling the window manager.
    use x11rb::protocol::xproto::ModMask;
    let shift_super = ModMask::SHIFT | ModMask::M4;
    let keycode_q = 24;
    let keycode_r = 27;
    daily
        .bind_key(shift_super, keycode_q, daily::Command::Exit)
        .expect("failed to add a keybinding for Exit command");
    daily
        .bind_key(shift_super, keycode_r, daily::Command::Restart)
        .expect("failed to add a keybinding for Restart command");

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
