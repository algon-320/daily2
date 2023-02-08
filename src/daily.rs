use std::collections::{HashMap, VecDeque};
use std::rc::Rc;

use x11rb::connection::Connection as _;
use x11rb::protocol::{randr, xproto, Event};
use x11rb::rust_connection::RustConnection;

use randr::ConnectionExt as _;
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
    SpawnProcess(String),
    FocusNextMonitor,
    FocusNextWindow,
    ChangeScreen(usize),
    MoveWindow(usize),
}

#[derive(Debug, Clone)]
struct Monitor {
    x: i16,
    y: i16,
    w: u16,
    h: u16,
    screen: usize,
    dummy_window: xproto::Window,
}

impl Monitor {
    fn top(&self) -> i32 {
        self.y as i32
    }
    fn bottom(&self) -> i32 {
        self.y as i32 + self.h as i32
    }
    fn left(&self) -> i32 {
        self.x as i32
    }
    fn right(&self) -> i32 {
        self.x as i32 + self.w as i32
    }
}

#[derive(Debug, Clone)]
struct Screen {
    monitor: Option<usize>,
}

#[derive(Debug, Clone)]
struct Window {
    id: xproto::Window,
    screen: usize,
    mapped: bool,
}

pub struct Daily {
    ctx: Context,
    keybind: HashMap<(u16, u8), Command>,
    windows: HashMap<xproto::Window, Window>,
    monitors: Vec<Monitor>,
    screens: Vec<Screen>,
    focus: xproto::Window,
}

// public interfaces
impl Daily {
    pub fn new() -> Result<Self> {
        Ok(Self {
            ctx: Context::new()?,
            keybind: HashMap::new(),
            windows: HashMap::new(),
            monitors: Vec::new(),
            screens: Vec::new(),
            focus: x11rb::NONE,
        })
    }

    pub fn bind_key(
        &mut self,
        modifiers: xproto::ModMask,
        keycode: u8,
        cmd: Command,
    ) -> Result<()> {
        self.ctx
            .conn
            .grab_key(
                false,
                self.ctx.root,
                modifiers,
                keycode,
                xproto::GrabMode::ASYNC, // pointer
                xproto::GrabMode::ASYNC, // keyboard
            )?
            .check()?;

        self.keybind
            .insert((modifiers.into(), keycode), cmd.clone());

        log::info!("new keybinding: state={modifiers:?}, detail={keycode}, cmd={cmd:?}");
        Ok(())
    }

    pub fn start(mut self) -> Result<()> {
        self.init()?;

        let mut cmdq = VecDeque::new();
        loop {
            let event = self.ctx.conn.wait_for_event()?;
            self.handle_event(event, &mut cmdq)?;
            self.process_commands(&mut cmdq)?;
        }
    }
}

impl Daily {
    fn init(&mut self) -> Result<()> {
        // become the window manager of the root window
        {
            let interest =
                xproto::EventMask::SUBSTRUCTURE_NOTIFY | xproto::EventMask::SUBSTRUCTURE_REDIRECT;
            let aux = xproto::ChangeWindowAttributesAux::new().event_mask(interest);
            self.ctx
                .conn
                .change_window_attributes(self.ctx.root, &aux)?
                .check()?;
            self.ctx.conn.flush()?;
        }

        // setup monitors
        {
            let mut monitors = self
                .ctx
                .conn
                .randr_get_monitors(self.ctx.root, true)?
                .reply()?
                .monitors;

            if let Some(primary_idx) = monitors.iter().position(|minfo| minfo.primary) {
                monitors.swap(0, primary_idx);
            }

            self.monitors.clear();
            for (i, mon) in monitors.into_iter().enumerate() {
                let dummy_window = self.ctx.conn.generate_id()?;
                log::debug!("dummy window for monitor {i}: {dummy_window}");

                let depth = x11rb::COPY_DEPTH_FROM_PARENT;
                let class = xproto::WindowClass::INPUT_ONLY;
                let visual = x11rb::COPY_FROM_PARENT;
                let aux = xproto::CreateWindowAux::new();
                self.ctx.conn.create_window(
                    depth,
                    dummy_window,
                    self.ctx.root,
                    mon.x, // x
                    mon.y, // y
                    1,     // width
                    1,     // height
                    0,     // border-width
                    class,
                    visual,
                    &aux,
                )?;
                self.ctx.conn.map_window(dummy_window)?;

                self.monitors.push(Monitor {
                    x: mon.x,
                    y: mon.y,
                    w: mon.width,
                    h: mon.height,
                    screen: i,
                    dummy_window,
                });
            }
        }

        // setup screens
        {
            const NUM_SCREENS: usize = 3;
            let num_screens = std::cmp::max(self.monitors.len(), NUM_SCREENS);
            let num_monitors = self.monitors.len();

            self.screens.clear();
            for i in 0..num_screens {
                let scr = if i < num_monitors {
                    Screen { monitor: Some(i) }
                } else {
                    Screen { monitor: None }
                };
                self.screens.push(scr);
            }
        }

        // grab mouse button(s)
        {
            let event_mask = xproto::EventMask::BUTTON_PRESS;
            self.ctx
                .conn
                .grab_button(
                    false,
                    self.ctx.root,
                    event_mask,
                    xproto::GrabMode::SYNC,  // pointer
                    xproto::GrabMode::ASYNC, // keyboard
                    x11rb::NONE,
                    x11rb::NONE,
                    xproto::ButtonIndex::M1,
                    xproto::ModMask::default(),
                )?
                .check()?;
        }

        {
            let dummy = self.monitors[0].dummy_window;
            self.change_focus(dummy)?;
        }

        self.ctx.conn.flush()?;
        Ok(())
    }

    fn handle_event(&mut self, event: Event, cmdq: &mut VecDeque<Command>) -> Result<()> {
        match event {
            Event::KeyPress(key_press) => {
                log::trace!("KeyPress: {event:?}");
                let keys: (u16, u8) = (key_press.state.into(), key_press.detail);
                if let Some(cmd) = self.keybind.get(&keys).cloned() {
                    cmdq.push_back(cmd);
                }
            }

            Event::ButtonPress(button_press) => {
                log::trace!("ButtonPress: {event:?}");

                if button_press.child == self.ctx.root
                    || (button_press.child == x11rb::NONE && button_press.event == self.ctx.root)
                {
                    let y = button_press.root_y as i32;
                    let x = button_press.root_x as i32;
                    let mon = self
                        .monitors
                        .iter()
                        .position(|mon| {
                            mon.top() <= y && y < mon.bottom() && mon.left() <= x && x < mon.right()
                        })
                        .unwrap_or(0);
                    let dummy = self.monitors[mon].dummy_window;
                    self.change_focus(dummy)?;
                } else {
                    self.change_focus(button_press.child)?;
                }

                self.ctx
                    .conn
                    .allow_events(xproto::Allow::REPLAY_POINTER, x11rb::CURRENT_TIME)?;
                self.ctx.conn.flush()?;
            }

            Event::MapRequest(req) => {
                if let Some(window) = self.windows.get_mut(&req.window) {
                    if let Some(monitor) = self.screens[window.screen].monitor {
                        window.mapped = true;
                        let window_id = window.id;
                        log::debug!(
                            "window 0x{:X} is mapped on screen {}",
                            window_id,
                            window.screen
                        );
                        self.update_layout(monitor)?;
                        self.ctx.conn.map_window(window_id)?;
                        self.change_focus(window_id)?;
                    }
                } else {
                    let monitor = self.focused_monitor()?.unwrap_or(0);
                    let screen = self.monitors[monitor].screen;

                    let window = Window {
                        id: req.window,
                        screen,
                        mapped: true,
                    };

                    let window_id = window.id;
                    log::debug!("window 0x{:X} added on screen {}", window_id, screen);
                    self.windows.insert(window_id, window);
                    self.update_layout(monitor)?;

                    self.ctx.conn.map_window(window_id)?;
                    self.change_focus(window_id)?;
                }
            }

            Event::UnmapNotify(notif) => {
                if let Some(window) = self.windows.get_mut(&notif.window) {
                    if self.screens[window.screen].monitor.is_some() {
                        log::debug!("window 0x{:X} is unmapped", window.id);
                        window.mapped = false;
                    }
                } else {
                    log::warn!("UnmapNotify: unknown window 0x{:X}", notif.window);
                }
            }

            Event::DestroyNotify(notif) => {
                self.remove_window(notif.window)?;
            }

            Event::Error(err) => {
                log::error!("X11 error: {err:?}");
            }

            _ => {
                log::trace!("unhandled event: {event:?}");
            }
        }
        Ok(())
    }

    fn process_commands(&mut self, cmdq: &mut VecDeque<Command>) -> Result<()> {
        for cmd in cmdq.drain(..) {
            log::debug!("cmd={cmd:?}");
            match cmd {
                Command::Exit => {
                    return Err(Error::Interrupted { restart: false });
                }
                Command::Restart => {
                    return Err(Error::Interrupted { restart: true });
                }

                Command::SpawnProcess(cmdline) => {
                    // FIXME: make this part cleaner
                    let shell_cmdline =
                        format!("({cmdline} 2>&1) | sed 's/^/spawned process: /' &");
                    let mut child = std::process::Command::new("/bin/sh")
                        .arg("-c")
                        .arg(shell_cmdline)
                        .spawn()
                        .unwrap();
                    child.wait().unwrap();
                }

                Command::FocusNextMonitor => {
                    let next = self
                        .focused_monitor()?
                        .map(|i| (i + 1) % self.monitors.len())
                        .unwrap_or(0);

                    let screen = self.monitors[next].screen;
                    let any_window_on_next_monitor: xproto::Window = self
                        .windows
                        .values()
                        .filter(|win| win.screen == screen)
                        .map(|win| win.id)
                        .next()
                        .unwrap_or_else(|| self.monitors[next].dummy_window);
                    self.change_focus(any_window_on_next_monitor)?;
                }

                Command::FocusNextWindow => {
                    if let Some(window) = self.windows.get(&self.focus) {
                        let screen = window.screen;
                        let monitor = self.screens[screen].monitor.unwrap();

                        let windows: Vec<xproto::Window> = self
                            .windows
                            .values()
                            .filter(|win| win.screen == screen)
                            .map(|win| win.id)
                            .collect();

                        if windows.len() > 1 {
                            let next_window = windows
                                .iter()
                                .copied()
                                .chain(windows.iter().copied())
                                .skip_while(|id| *id != window.id)
                                .nth(1)
                                .unwrap_or_else(|| self.monitors[monitor].dummy_window);
                            self.change_focus(next_window)?;
                        }
                    }
                }

                Command::ChangeScreen(new_screen) => {
                    if let Some(monitor_a) = self.screens[new_screen].monitor {
                        let screen_a = new_screen;
                        let monitor_b = self.focused_monitor()?.unwrap_or(0);
                        let screen_b = self.monitors[monitor_b].screen;

                        self.monitors[monitor_a].screen = screen_b;
                        self.monitors[monitor_b].screen = screen_a;
                        self.screens[screen_a].monitor = Some(monitor_b);
                        self.screens[screen_b].monitor = Some(monitor_a);
                        self.update_layout(monitor_a)?;
                        self.update_layout(monitor_b)?;

                        let any_window_on_new_screen: xproto::Window = self
                            .windows
                            .values()
                            .filter(|win| win.screen == new_screen)
                            .map(|win| win.id)
                            .next()
                            .unwrap_or_else(|| self.monitors[monitor_b].dummy_window);
                        self.change_focus(any_window_on_new_screen)?;
                    } else {
                        let monitor = self.focused_monitor()?.unwrap_or(0);
                        let current_screen = self.monitors[monitor].screen;

                        for window in self
                            .windows
                            .values_mut()
                            .filter(|w| w.screen == current_screen)
                        {
                            self.ctx.conn.unmap_window(window.id)?;
                        }
                        for window in self.windows.values_mut().filter(|w| w.screen == new_screen) {
                            self.ctx.conn.map_window(window.id)?;
                        }
                        self.ctx.conn.flush()?;

                        self.monitors[monitor].screen = new_screen;
                        self.screens[new_screen].monitor = Some(monitor);
                        self.screens[current_screen].monitor = None;
                        self.update_layout(monitor)?;

                        let any_window_on_new_screen: xproto::Window = self
                            .windows
                            .values()
                            .filter(|win| win.screen == new_screen)
                            .map(|win| win.id)
                            .next()
                            .unwrap_or_else(|| self.monitors[monitor].dummy_window);
                        self.change_focus(any_window_on_new_screen)?;
                    }
                }

                Command::MoveWindow(new_screen) => {
                    if let Some(window) = self.windows.get_mut(&self.focus) {
                        let old_screen = window.screen;
                        let old_monitor = self.screens[old_screen].monitor;
                        let new_monitor = self.screens[new_screen].monitor;

                        window.screen = new_screen;
                        if new_monitor.is_none() {
                            self.ctx.conn.unmap_window(window.id)?;
                            self.ctx.conn.flush()?;
                        }

                        if let Some(mon) = old_monitor {
                            self.update_layout(mon)?;
                        }
                        if let Some(mon) = new_monitor {
                            self.update_layout(mon)?;
                        }

                        let any_window_on_screen: xproto::Window = self
                            .windows
                            .values()
                            .filter(|win| win.screen == old_screen)
                            .map(|win| win.id)
                            .next()
                            .unwrap_or_else(|| {
                                self.monitors[old_monitor.expect("focus")].dummy_window
                            });
                        self.change_focus(any_window_on_screen)?;
                    }
                }
            }
        }
        Ok(())
    }

    fn change_focus(&mut self, focus: xproto::Window) -> Result<()> {
        let old_focus = self.focus;
        let new_focus = focus;

        log::debug!("focus window 0x{:X} ({})", new_focus, new_focus);
        // TODO: config
        if self.windows.contains_key(&old_focus) {
            let aux = xproto::ChangeWindowAttributesAux::new().border_pixel(0x000000);
            self.ctx.conn.change_window_attributes(old_focus, &aux)?;
        }
        if self.windows.contains_key(&new_focus) {
            let aux = xproto::ChangeWindowAttributesAux::new().border_pixel(0x00FF00);
            self.ctx.conn.change_window_attributes(new_focus, &aux)?;
        }

        self.ctx
            .conn
            .set_input_focus(
                xproto::InputFocus::NONE, // revert-to
                new_focus,
                x11rb::CURRENT_TIME,
            )?
            .check()?;
        self.focus = new_focus;
        Ok(())
    }

    fn remove_window(&mut self, window: xproto::Window) -> Result<()> {
        if let Some(window) = self.windows.remove(&window) {
            let screen = window.screen;
            log::debug!("window 0x{:X} removed from screen {}", window.id, screen);
            if let Some(monitor) = self.screens[screen].monitor {
                self.update_layout(monitor)?;

                if self.focus == window.id {
                    let new_focus: xproto::Window = self
                        .windows
                        .values()
                        .filter(|win| win.screen == screen)
                        .map(|win| win.id)
                        .next()
                        .unwrap_or_else(|| self.monitors[monitor].dummy_window);
                    self.change_focus(new_focus)?;
                }
            }
        }
        Ok(())
    }

    fn update_layout(&mut self, monitor: usize) -> Result<()> {
        let screen = self.monitors[monitor].screen;
        let targets: Vec<xproto::Window> = self
            .windows
            .values()
            .filter(|win| win.screen == screen)
            .map(|win| win.id)
            .collect();

        let mon = &self.monitors[monitor];

        // NOTE: horizontal layout
        if !targets.is_empty() {
            let n = targets.len();
            let each_w = (mon.w / n as u16) as u32;
            let last_w = (mon.w as u32) - (n as u32 - 1) * each_w;
            let each_h = mon.h as u32;

            for (i, win) in targets.into_iter().enumerate() {
                let x = (mon.x as i32) + (each_w as i32) * (i as i32);
                let y = mon.y as i32;
                let w = if i < n - 1 { each_w } else { last_w };

                let aux = xproto::ConfigureWindowAux::new()
                    .x(x)
                    .y(y)
                    .width(w - 2)
                    .height(each_h - 2)
                    .border_width(1);
                self.ctx.conn.configure_window(win, &aux)?;
            }
            self.ctx.conn.flush()?;
        }
        Ok(())
    }

    fn focused_monitor(&mut self) -> Result<Option<usize>> {
        if let Some(window) = self.windows.get(&self.focus) {
            Ok(self.screens[window.screen].monitor)
        } else {
            Ok(self
                .monitors
                .iter()
                .position(|mon| mon.dummy_window == self.focus))
        }
    }
}
