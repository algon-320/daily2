use std::collections::{HashMap, VecDeque};
use std::rc::Rc;

use x11rb::connection::Connection as _;
use x11rb::protocol::{xproto, Event};
use x11rb::rust_connection::RustConnection;
use xproto::ConnectionExt as _;

use crate::error::{Error, Result};

x11rb::atom_manager! {
    pub AtomCollection: AtomCollectionCookie {}
}

#[derive(Clone)]
pub struct Context {
    pub conn: Rc<RustConnection>,
    pub root: xproto::Window,
    pub atom: AtomCollection,
}

impl Context {
    pub fn new() -> Result<Self> {
        let conn = match RustConnection::connect(None) {
            Ok((conn, _)) => conn,
            Err(err) => {
                panic!("Failed to connect with the X server: {}", err);
            }
        };
        let root = conn.setup().roots[0].root;
        let atom = AtomCollection::new(&conn)?.reply()?;
        Ok(Self {
            conn: Rc::new(conn),
            root,
            atom,
        })
    }
}

#[derive(Debug, Clone)]
pub enum Command {
    Exit,
    Restart,
}

pub struct Daily {
    ctx: Context,
    keybind: HashMap<(u16, u8), Command>,
    cmdq: VecDeque<Command>,
}

// public interfaces
impl Daily {
    pub fn new() -> Result<Self> {
        Ok(Self {
            ctx: Context::new()?,
            keybind: HashMap::new(),
            cmdq: VecDeque::new(),
        })
    }

    pub fn bind_key(&mut self, modif: xproto::ModMask, keycode: u8, cmd: Command) -> Result<()> {
        let async_ = xproto::GrabMode::ASYNC;
        let root = self.ctx.root;
        self.ctx
            .conn
            .grab_key(true, root, modif, keycode, async_, async_)?
            .check()?;

        self.keybind.insert((modif.into(), keycode), cmd.clone());

        log::info!("new keybinding: state={modif:?}, detail={keycode}, cmd={cmd:?}");
        Ok(())
    }

    pub fn start(mut self) -> Result<()> {
        loop {
            let event = self.ctx.conn.wait_for_event()?;
            self.handle_event(event)?;
            self.process_cmdq()?;
        }
    }
}

impl Daily {
    fn handle_event(&mut self, event: Event) -> Result<()> {
        match event {
            Event::KeyPress(button_press) => {
                let keys: (u16, u8) = (button_press.state.into(), button_press.detail);
                if let Some(cmd) = self.keybind.get(&keys).cloned() {
                    self.cmdq.push_back(cmd);
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn process_cmdq(&mut self) -> Result<()> {
        for cmd in self.cmdq.drain(..) {
            log::debug!("cmd={cmd:?}");
            match cmd {
                Command::Exit => {
                    return Err(Error::Interrupted { restart: false });
                }
                Command::Restart => {
                    return Err(Error::Interrupted { restart: true });
                }
            }
        }
        Ok(())
    }
}
