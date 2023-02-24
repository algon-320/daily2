mod config;
mod daily;
mod error;
mod utils;

fn main() {
    env_logger::init();

    let mut daily = daily::Daily::new().expect("failed to initialize daily");

    for (modifiers, keycode, command) in config::keybindings() {
        daily
            .bind_key(modifiers, keycode, command.clone())
            .unwrap_or_else(|err| {
                log::error!(
                    "Failed to add a keybinding: modifiers:{:?}, keycode:{}, command:{:?}",
                    modifiers,
                    keycode,
                    command
                );
                log::debug!("detail: {err:?}");
            });
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
