mod daily;
mod error;

fn main() -> error::Result<()> {
    env_logger::init();

    let mut daily = daily::Daily::new()?;

    use x11rb::protocol::xproto::ModMask;
    let shift_super = ModMask::SHIFT | ModMask::M4;
    let keycode_q = 24;
    daily.bind_key(shift_super, keycode_q, daily::Command::Exit)?;

    daily.start()
}
