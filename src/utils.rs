use std::rc::Rc;

use x11rb::connection::Connection as _;
use x11rb::protocol::xproto;
use x11rb::rust_connection::RustConnection;
use xproto::ConnectionExt as _;

use crate::error::Result;

x11rb::atom_manager! {
    pub AtomCollection: AtomCollectionCookie {
        _NET_SUPPORTED,
        _NET_SUPPORTING_WM_CHECK,
        _NET_WM_ALLOWED_ACTIONS,
        _NET_WM_ACTION_FULLSCREEN,
        _NET_WM_MOVERESIZE,
        _NET_MOVERESIZE_WINDOW,
        _NET_WM_STATE,
        _NET_WM_STATE_FULLSCREEN,
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

pub fn get_atom_name(ctx: &Context, atom: xproto::Atom) -> Result<String> {
    let name_reply = ctx.conn.get_atom_name(atom)?.reply()?;
    let len = name_reply.name_len() as usize;
    let bytes = &name_reply.name.as_slice()[..len];
    let name = std::str::from_utf8(bytes).unwrap().to_owned();
    Ok(name)
}

pub fn get_net_wm_window_type(
    ctx: &Context,
    window: xproto::Window,
) -> Result<Option<xproto::Atom>> {
    let net_wm_type = ctx.atom._NET_WM_WINDOW_TYPE;
    Ok(ctx
        .conn
        .get_property(false, window, net_wm_type, xproto::AtomEnum::ATOM, 0, 1)?
        .reply()?
        .value32()
        .and_then(|mut iter| iter.next()))
}

pub enum Property<'a> {
    Window(xproto::Window),
    AtomList(&'a [xproto::Atom]),
}

pub fn replace_property(
    ctx: &Context,
    target: xproto::Window,
    key: xproto::Atom,
    value: Property<'_>,
) -> Result<()> {
    let (type_, format, data): (xproto::AtomEnum, u8, Vec<u8>);
    match value {
        Property::Window(window) => {
            type_ = xproto::AtomEnum::WINDOW;
            format = 32;
            data = window.to_ne_bytes().to_vec();
        }
        Property::AtomList(atoms) => {
            type_ = xproto::AtomEnum::ATOM;
            format = 32;
            data = atoms.iter().flat_map(|a| a.to_ne_bytes()).collect();
        }
    };

    ctx.conn.change_property(
        xproto::PropMode::REPLACE,
        target,
        key,
        type_,
        format,
        (data.len() as u32) / (format as u32 / 8),
        &data,
    )?;
    ctx.conn.flush()?;
    Ok(())
}
