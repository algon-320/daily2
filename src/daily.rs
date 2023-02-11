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
    ToggleFloating,
}

#[derive(Debug, Clone, Copy, Default)]
struct Rect {
    x: i32,
    y: i32,
    w: i32,
    h: i32,
}

impl Rect {
    fn top(&self) -> i32 {
        self.y
    }
    fn bottom(&self) -> i32 {
        self.y + self.h
    }
    fn left(&self) -> i32 {
        self.x
    }
    fn right(&self) -> i32 {
        self.x + self.w
    }
    fn contains(&self, x: i32, y: i32) -> bool {
        self.left() <= x && x < self.right() && self.top() <= y && y < self.bottom()
    }
}

#[derive(Debug, Clone)]
struct Monitor {
    crtc: randr::Crtc,
    screen: usize,
    dummy_window: xproto::Window,
    geometry: Rect,
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
    geometry: Rect,
    floating: bool,

    // NOTE:
    // X11 core protocol does not provide a way to determine if an UnmapNotifyEvent was caused by
    // by this client or aother client. We are only interestead in the latter case,
    // so when we (actively) unmap a window turn on this flag and test it on UnmapNotifyEvents.
    // Is there any better way to deal with this issue?
    ignore_unmap_notify: bool,
}

pub struct Daily {
    ctx: Context,
    keybind: HashMap<(u16, u8), Command>,
    windows: HashMap<xproto::Window, Window>,
    monitors: Vec<Monitor>,
    screens: Vec<Screen>,
    focus: xproto::Window,
    dnd_position: Option<(i32, i32)>,
    button_count: usize,
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
            dnd_position: None,
            button_count: 0,
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

        // setup for screens
        {
            const NUM_SCREENS: usize = 100;
            self.screens = vec![Screen { monitor: None }; NUM_SCREENS];
        }

        // setup for monitors
        {
            // NOTE: randr version 1.2 or later
            self.ctx
                .conn
                .randr_select_input(self.ctx.root, randr::NotifyMask::CRTC_CHANGE)?;

            let crtcs = self
                .ctx
                .conn
                .randr_get_screen_resources_current(self.ctx.root)?
                .reply()?
                .crtcs;

            self.monitors.clear();
            for (i, crtc) in crtcs.into_iter().enumerate() {
                let crtc_info = self
                    .ctx
                    .conn
                    .randr_get_crtc_info(crtc, x11rb::CURRENT_TIME)?
                    .reply()?;
                log::debug!("Crtc {crtc}: {crtc_info:?}");

                if crtc_info.mode == x11rb::NONE {
                    // ignore disabled CRTCs
                    continue;
                }

                let geometry = Rect {
                    x: crtc_info.x as i32,
                    y: crtc_info.y as i32,
                    w: crtc_info.width as i32,
                    h: crtc_info.height as i32,
                };
                self.add_monitor(crtc, geometry, i)?;
            }
        }

        // grab mouse button(s)
        {
            let event_mask = xproto::EventMask::BUTTON_PRESS
                | xproto::EventMask::BUTTON_RELEASE
                | xproto::EventMask::BUTTON_MOTION;
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
                    xproto::ButtonIndex::ANY,
                    xproto::ModMask::ANY,
                )?
                .check()?;
        }

        // focus the first monitor
        {
            let dummy = self.monitors[0].dummy_window;
            self.change_focus(dummy)?;
        }

        self.ctx.conn.flush()?;
        Ok(())
    }

    fn add_monitor(&mut self, crtc: randr::Crtc, geometry: Rect, screen: usize) -> Result<()> {
        let i = self.monitors.len();
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
            geometry.x as i16, // x
            geometry.y as i16, // y
            1,                 // width
            1,                 // height
            0,                 // border-width
            class,
            visual,
            &aux,
        )?;
        self.ctx.conn.map_window(dummy_window)?;

        self.monitors.push(Monitor {
            crtc,
            screen,
            dummy_window,
            geometry,
        });
        self.screens[screen].monitor = Some(i);

        self.update_layout(i)?;
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
                self.button_count += 1;

                let x = button_press.root_x as i32;
                let y = button_press.root_y as i32;
                let clicked_window =
                    if button_press.child == x11rb::NONE && button_press.event == self.ctx.root {
                        self.ctx.root
                    } else {
                        button_press.child
                    };

                let mut allow = xproto::Allow::REPLAY_POINTER;

                const LEFT_BUTTON: u8 = 1;
                const RIGHT_BUTTON: u8 = 3;
                // FIXME: config
                if button_press.detail == LEFT_BUTTON || button_press.detail == RIGHT_BUTTON {
                    if clicked_window == self.ctx.root {
                        let mon = self
                            .monitors
                            .iter()
                            .position(|mon| {
                                mon.geometry.top() <= y
                                    && y < mon.geometry.bottom()
                                    && mon.geometry.left() <= x
                                    && x < mon.geometry.right()
                            })
                            .unwrap_or(0);
                        let dummy = self.monitors[mon].dummy_window;
                        self.change_focus(dummy)?;
                    } else {
                        self.change_focus(clicked_window)?;
                    }
                }

                // FIXME: config
                if u16::from(button_press.state) & u16::from(xproto::KeyButMask::MOD1) > 0 {
                    self.dnd_position = Some((x, y));
                    allow = xproto::Allow::SYNC_POINTER;
                }

                self.ctx.conn.allow_events(allow, x11rb::CURRENT_TIME)?;
                self.ctx.conn.flush()?;
            }

            Event::MotionNotify(motion) => {
                log::debug!("MotionNotify: {motion:?}");
                if let Some((prev_x, prev_y)) = self.dnd_position {
                    let x = motion.root_x as i32;
                    let y = motion.root_y as i32;
                    self.dnd_position = Some((x, y));

                    let dx = x - prev_x;
                    let dy = y - prev_y;

                    if let Some(window) = self.windows.get_mut(&self.focus) {
                        if !window.floating {
                            window.floating = true;
                            if let Some(monitor) = self.screens[window.screen].monitor {
                                self.update_layout(monitor)?;
                            }
                        }
                    }

                    if let Some(window) = self.windows.get_mut(&self.focus) {
                        let state = u16::from(motion.state);
                        let button1 = u16::from(xproto::KeyButMask::BUTTON1);
                        let button3 = u16::from(xproto::KeyButMask::BUTTON3);

                        if state & button1 > 0 {
                            window.geometry.x += dx;
                            window.geometry.y += dy;
                        } else if state & button3 > 0 {
                            window.geometry.w += dx;
                            window.geometry.h += dy;
                        }

                        let aux = xproto::ConfigureWindowAux::new()
                            .x(window.geometry.x)
                            .y(window.geometry.y)
                            .width(window.geometry.w as u32)
                            .height(window.geometry.h as u32)
                            .stack_mode(xproto::StackMode::ABOVE);
                        self.ctx.conn.configure_window(window.id, &aux)?;
                        self.ctx.conn.flush()?;
                    }
                }
            }

            Event::ButtonRelease(button_release) => {
                log::debug!("ButtonRelease: {button_release:?}");
                self.button_count -= 1;

                let x = button_release.root_x as i32;
                let y = button_release.root_y as i32;

                if button_release.detail == 1 {
                    if let Some(window) = self.windows.get_mut(&self.focus) {
                        for mon in self.monitors.iter() {
                            if !mon.geometry.contains(x, y) {
                                continue;
                            }

                            // FIXME: config
                            let d = 32;
                            let border_width = 1;

                            let g = mon.geometry;
                            let left = g.left() <= x && x < g.left() + d;
                            let right = g.right() - d <= x && x < g.right();
                            let top = g.top() <= y && y < g.top() + d;
                            let bottom = g.bottom() - d <= y && y < g.bottom();

                            if left && top {
                                window.geometry.x = g.left();
                                window.geometry.y = g.top();
                                window.geometry.w = g.w / 2 - border_width * 2;
                                window.geometry.h = g.h / 2 - border_width * 2;
                            } else if left && bottom {
                                window.geometry.x = g.left();
                                window.geometry.y = g.top() + g.h / 2;
                                window.geometry.w = g.w / 2 - border_width * 2;
                                window.geometry.h = g.h - g.h / 2 - border_width * 2;
                            } else if right && top {
                                window.geometry.x = g.left() + g.w / 2;
                                window.geometry.y = g.top();
                                window.geometry.w = g.w - g.w / 2 - border_width * 2;
                                window.geometry.h = g.h / 2 - border_width * 2;
                            } else if right && bottom {
                                window.geometry.x = g.left() + g.w / 2;
                                window.geometry.y = g.top() + g.h / 2;
                                window.geometry.w = g.w - g.w / 2 - border_width * 2;
                                window.geometry.h = g.h - g.h / 2 - border_width * 2;
                            } else if left {
                                window.geometry.x = g.left();
                                window.geometry.y = g.top();
                                window.geometry.w = g.w / 2 - border_width * 2;
                                window.geometry.h = g.h - border_width * 2;
                            } else if right {
                                window.geometry.x = g.left() + g.w / 2;
                                window.geometry.y = g.top();
                                window.geometry.w = g.w - g.w / 2 - border_width * 2;
                                window.geometry.h = g.h - border_width * 2;
                            } else if top {
                                window.geometry.x = g.left();
                                window.geometry.y = g.top();
                                window.geometry.w = g.w - border_width * 2;
                                window.geometry.h = g.h / 2 - border_width * 2;
                            } else if bottom {
                                window.geometry.x = g.left();
                                window.geometry.y = g.top() + g.h / 2;
                                window.geometry.w = g.w - border_width * 2;
                                window.geometry.h = g.h - g.h / 2 - border_width * 2;
                            } else {
                                break;
                            }

                            let aux = xproto::ConfigureWindowAux::new()
                                .x(window.geometry.x)
                                .y(window.geometry.y)
                                .width(window.geometry.w as u32)
                                .height(window.geometry.h as u32)
                                .stack_mode(xproto::StackMode::ABOVE);
                            self.ctx.conn.configure_window(window.id, &aux)?;
                            self.ctx.conn.flush()?;
                        }
                    }
                }

                if self.button_count > 0 {
                    self.ctx
                        .conn
                        .allow_events(xproto::Allow::SYNC_POINTER, x11rb::CURRENT_TIME)?;
                    self.ctx.conn.flush()?;
                }

                if self.button_count == 0 {
                    self.dnd_position = None;
                }
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
                    let monitor = self.focused_monitor().unwrap_or(0);
                    let screen = self.monitors[monitor].screen;

                    let window = Window {
                        id: req.window,
                        screen,
                        mapped: true,
                        geometry: Rect::default(),
                        floating: false, // FIXME: true if it's a dialog window
                        ignore_unmap_notify: false,
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
                    if window.ignore_unmap_notify {
                        window.ignore_unmap_notify = false;
                    } else {
                        if let Some(monitor) = self.screens[window.screen].monitor {
                            log::debug!("window 0x{:X} is unmapped", window.id);
                            window.mapped = false;

                            let screen = window.screen;
                            if self.focus == window.id {
                                let any_window_on_screen: xproto::Window = self
                                    .windows
                                    .values()
                                    .filter(|win| win.screen == screen && win.mapped)
                                    .map(|win| win.id)
                                    .next()
                                    .unwrap_or_else(|| self.monitors[monitor].dummy_window);
                                self.change_focus(any_window_on_screen)?;
                            }

                            self.update_layout(monitor)?;
                        }
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

            Event::RandrNotify(notify) => {
                if notify.sub_code == randr::Notify::CRTC_CHANGE {
                    let crtc_change = notify.u.as_cc();
                    log::debug!("RRCrtcChangeNotify: {crtc_change:?}");

                    let crtc = crtc_change.crtc;
                    if let Some(monitor) = self.monitors.iter().position(|mon| mon.crtc == crtc) {
                        if crtc_change.mode == x11rb::NONE {
                            // monitor was disabled

                            let screen = self.monitors[monitor].screen;
                            let wins: Vec<xproto::Window> = self
                                .windows
                                .values()
                                .filter(|win| win.screen == screen && win.mapped)
                                .map(|win| win.id)
                                .collect();

                            for window_id in wins {
                                if self.focus == window_id {
                                    self.change_focus(x11rb::NONE)?;
                                }
                                self.windows
                                    .get_mut(&window_id)
                                    .unwrap()
                                    .ignore_unmap_notify = true;
                                self.ctx.conn.unmap_window(window_id)?;
                            }
                            self.ctx.conn.flush()?;

                            self.screens[screen].monitor = None;

                            self.ctx
                                .conn
                                .destroy_window(self.monitors[monitor].dummy_window)?;

                            self.monitors.swap_remove(monitor);
                            if monitor < self.monitors.len() {
                                let screen = self.monitors[monitor].screen;
                                self.screens[screen].monitor = Some(monitor);
                            }
                        } else {
                            // monitor info was changed
                            let geometry = &mut self.monitors.get_mut(monitor).unwrap().geometry;
                            geometry.x = crtc_change.x as i32;
                            geometry.y = crtc_change.y as i32;
                            geometry.w = crtc_change.width as i32;
                            geometry.h = crtc_change.height as i32;
                            self.update_layout(monitor)?;
                        }
                    } else {
                        // monitor was enabled

                        let unmapped_screen = self
                            .screens
                            .iter()
                            .position(|scr| scr.monitor.is_none())
                            .expect("too many monitors");
                        let geometry = Rect {
                            x: crtc_change.x as i32,
                            y: crtc_change.y as i32,
                            w: crtc_change.width as i32,
                            h: crtc_change.height as i32,
                        };
                        self.add_monitor(crtc, geometry, unmapped_screen)?;

                        for window in self
                            .windows
                            .values()
                            .filter(|win| win.screen == unmapped_screen && win.mapped)
                            .map(|win| win.id)
                        {
                            self.ctx.conn.map_window(window)?;
                        }
                        self.ctx.conn.flush()?;
                    }
                }
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
                        .focused_monitor()
                        .map(|i| (i + 1) % self.monitors.len())
                        .unwrap_or(0);

                    let screen = self.monitors[next].screen;
                    let any_window_on_next_monitor: xproto::Window = self
                        .windows
                        .values()
                        .filter(|win| win.screen == screen && win.mapped)
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
                            .filter(|win| win.screen == screen && win.mapped)
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
                        let monitor_b = self.focused_monitor().unwrap_or(0);
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
                            .filter(|win| win.screen == new_screen && win.mapped)
                            .map(|win| win.id)
                            .next()
                            .unwrap_or_else(|| self.monitors[monitor_b].dummy_window);
                        self.change_focus(any_window_on_new_screen)?;
                    } else {
                        let monitor = self.focused_monitor().unwrap_or(0);
                        let current_screen = self.monitors[monitor].screen;

                        for window in self
                            .windows
                            .values_mut()
                            .filter(|win| win.screen == current_screen && win.mapped)
                        {
                            window.ignore_unmap_notify = true;
                            self.ctx.conn.unmap_window(window.id)?;
                        }
                        for window in self
                            .windows
                            .values_mut()
                            .filter(|win| win.screen == new_screen && win.mapped)
                        {
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
                            .filter(|win| win.screen == new_screen && win.mapped)
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
                            window.ignore_unmap_notify = true;
                            self.ctx.conn.unmap_window(window.id)?;
                            self.ctx.conn.flush()?;
                        }

                        if let Some(mon) = old_monitor {
                            self.update_layout(mon)?;
                        }
                        if let Some(mon) = new_monitor {
                            self.update_layout(mon)?;
                        }
                    }
                }

                Command::ToggleFloating => {
                    if let Some(window) = self.windows.get_mut(&self.focus) {
                        window.floating ^= true;
                        if window.floating {
                            let aux = xproto::ConfigureWindowAux::new()
                                .stack_mode(xproto::StackMode::ABOVE);
                            self.ctx.conn.configure_window(window.id, &aux)?;
                        }
                        if let Some(monitor) = self.screens[window.screen].monitor {
                            self.update_layout(monitor)?;
                        }
                    }
                }
            }
        }
        Ok(())
    }

    fn change_focus(&mut self, focus: xproto::Window) -> Result<()> {
        let old_focus = self.focus;
        let new_focus = focus;

        if old_focus == new_focus {
            return Ok(());
        }
        self.focus = new_focus;

        log::debug!("focus on window 0x{:X} ({})", new_focus, new_focus);
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
        Ok(())
    }

    fn remove_window(&mut self, window: xproto::Window) -> Result<()> {
        if let Some(window) = self.windows.remove(&window) {
            let screen = window.screen;
            log::debug!("window 0x{:X} removed from screen {}", window.id, screen);
            if let Some(monitor) = self.screens[screen].monitor {
                self.update_layout(monitor)?;
                if self.focus == window.id {
                    self.change_focus(x11rb::NONE)?;
                }
            }
        }
        Ok(())
    }

    fn update_layout(&mut self, monitor: usize) -> Result<()> {
        log::trace!("update_layout: {monitor}");

        let screen = self.monitors[monitor].screen;
        let mon_geo = self.monitors[monitor].geometry;

        let targets: Vec<xproto::Window> = self
            .windows
            .values()
            .filter(|win| win.screen == screen && win.mapped)
            .filter(|win| !win.floating)
            .map(|win| win.id)
            .collect();

        // NOTE: horizontal layout
        if !targets.is_empty() {
            let n = targets.len();
            let each_w = mon_geo.w / n as i32;
            let last_w = mon_geo.w - (n as i32 - 1) * each_w;
            let each_h = mon_geo.h;

            for (i, win) in targets.into_iter().enumerate() {
                let x = mon_geo.x + each_w * (i as i32);
                let y = mon_geo.y;
                let w = if i < n - 1 { each_w } else { last_w };

                let geo = Rect {
                    x,
                    y,
                    w: w - 2,
                    h: each_h - 2,
                };
                self.windows.get_mut(&win).unwrap().geometry = geo;

                let aux = xproto::ConfigureWindowAux::new()
                    .x(geo.x)
                    .y(geo.y)
                    .width(geo.w as u32)
                    .height(geo.h as u32)
                    .border_width(1);
                self.ctx.conn.configure_window(win, &aux)?;
            }
            self.ctx.conn.flush()?;
        }
        Ok(())
    }

    fn focused_monitor(&mut self) -> Option<usize> {
        if let Some(window) = self.windows.get(&self.focus) {
            self.screens[window.screen].monitor
        } else {
            self.monitors
                .iter()
                .position(|mon| mon.dummy_window == self.focus)
        }
    }
}
