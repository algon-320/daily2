use std::collections::{HashMap, VecDeque};
use std::rc::Rc;

use x11rb::connection::Connection as _;
use x11rb::protocol::{xproto, Event};
use x11rb::rust_connection::RustConnection;
use xproto::ConnectionExt as _;

use crate::error::Result;

x11rb::atom_manager! {
    pub AtomCollection: AtomCollectionCookie {
        WM_STATE,
        _NET_WM_WINDOW_TYPE,
        _NET_WM_WINDOW_TYPE_DIALOG,
    }
}

#[derive(Clone)]
pub struct Context {
    pub conn: Rc<RustConnection>,
    pub root: xproto::Window,
    pub atom: AtomCollection,
}

impl Context {
    pub fn init() -> Result<Self> {
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
}

pub struct Daily {
    ctx: Context,
    keybind: HashMap<(u16, u8), Command>,
    cmdq: VecDeque<Command>,
}

impl Daily {
    pub fn new() -> Result<Self> {
        let ctx = Context::init()?;
        Ok(Self {
            ctx,
            keybind: HashMap::new(),
            cmdq: VecDeque::new(),
        })
    }

    pub fn bind_key(&mut self, mo: xproto::ModMask, keycode: u8, cmd: Command) -> Result<()> {
        log::info!("new keybind added: state={mo:?}, detail={keycode}, cmd={cmd:?}");

        let async_ = xproto::GrabMode::ASYNC;
        let root = self.ctx.root;
        self.ctx
            .conn
            .grab_key(true, root, mo, keycode, async_, async_)?
            .check()?;

        let state: u16 = mo.into();
        self.keybind.insert((state, keycode), cmd);
        Ok(())
    }

    pub fn start(mut self) -> Result<()> {
        log::info!("daily started");

        loop {
            let event = self.ctx.conn.wait_for_event()?;
            self.handle_event(event)?;
            self.ctx.conn.flush()?;

            for cmd in self.cmdq.drain(..) {
                log::debug!("cmd={cmd:?}");
                match cmd {
                    Command::Exit => {
                        return Ok(());
                    }
                }
            }
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
}
