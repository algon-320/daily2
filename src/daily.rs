use std::collections::{HashMap, VecDeque};

use x11rb::connection::Connection as _;
use x11rb::protocol::{randr, xproto, Event};

use randr::ConnectionExt as _;
use xproto::ConnectionExt as _;

use crate::config;
use crate::error::{Error, Result};
use crate::utils;

#[derive(Debug, Clone)]
pub enum Command {
    Exit,
    Restart,
    SpawnProcess(String),
    FocusNextMonitor,
    FocusNextWindow,
    SwitchDesktop(usize),
    MoveWindow(usize),
    ToggleFloating,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
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
    /// a region occupied by this monitor (absolute coordinates)
    geometry: Rect,
    /// ID of the desktop displayed on this monitor
    desktop: usize,
    /// a dummy window used to control input focus
    dummy_window: xproto::Window,
}

#[derive(Debug, Clone)]
struct Desktop {
    monitor: Option<usize>,
}

#[derive(Debug, Clone)]
struct Window {
    id: xproto::Window,
    desktop: usize,
    mapped: bool,
    floating: bool,
    fullscreen: bool,

    /// a region occupied by this window, not-including borders (coordinates are relative to the monitor region)
    geometry: Rect,

    stacking_order: u64,

    // NOTE:
    // X11 core protocol does not provide a way to determine if an UnmapNotifyEvent was caused by
    // by this client or aother client. We are only interestead in the latter case,
    // so when we (actively) unmap a window turn on this flag and test it on UnmapNotifyEvents.
    // Is there any better way to deal with this issue?
    ignore_unmap_notify: bool,
}

#[allow(unused)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Modifier {
    Shift,
    Control,
    Super,
    Alt,
}

impl Modifier {
    pub fn keybutmask(&self) -> xproto::KeyButMask {
        match self {
            Modifier::Shift => xproto::KeyButMask::SHIFT,
            Modifier::Control => xproto::KeyButMask::CONTROL,

            // FIXME: these mappings can be changed
            Modifier::Super => xproto::KeyButMask::MOD4,
            Modifier::Alt => xproto::KeyButMask::MOD1,
        }
    }

    pub fn modmask(&self) -> xproto::ModMask {
        let keybutmask = self.keybutmask();
        u16::from(keybutmask).into()
    }
}

pub struct Daily {
    ctx: utils::Context,
    keybind: HashMap<(u16, u8), Command>,
    windows: HashMap<xproto::Window, Window>,
    monitors: Vec<Monitor>,
    desktops: Vec<Desktop>,
    focus: xproto::Window,
    dnd_position: Option<(i32, i32)>,
    button_count: usize,
    preview_window: xproto::Window,
    preview_geometry: Rect,
    stacking_counter: u64,
}

impl Daily {
    pub fn new() -> Result<Self> {
        Ok(Self {
            ctx: utils::Context::new()?,
            keybind: HashMap::new(),
            windows: HashMap::new(),
            monitors: Vec::new(),
            desktops: Vec::new(),
            focus: x11rb::NONE,
            dnd_position: None,
            button_count: 0,
            preview_window: x11rb::NONE,
            preview_geometry: Rect::default(),
            stacking_counter: 0,
        })
    }

    pub fn bind_key(&mut self, modifiers: &[Modifier], keycode: u8, cmd: Command) -> Result<()> {
        let mut modmask = xproto::ModMask::default();
        for m in modifiers {
            modmask = modmask | m.modmask();
        }

        self.ctx
            .conn
            .grab_key(
                false,
                self.ctx.root,
                modmask,
                keycode,
                xproto::GrabMode::ASYNC, // pointer
                xproto::GrabMode::ASYNC, // keyboard
            )?
            .check()?;

        self.keybind.insert((modmask.into(), keycode), cmd.clone());

        log::info!(
            "new keybinding: state={modmask:?} ({modifiers:?}), detail={keycode}, cmd={cmd:?}"
        );
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

macro_rules! mapped_windows {
    ($slf:expr, $desktop:expr) => {
        $slf.windows
            .values()
            .filter(|win| win.desktop == $desktop && win.mapped)
    };
}
macro_rules! mapped_windows_mut {
    ($slf:expr, $desktop:expr) => {
        $slf.windows
            .values_mut()
            .filter(|win| win.desktop == $desktop && win.mapped)
    };
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

            // _NET_SUPPORTED
            let hints = [
                self.ctx.atom._NET_SUPPORTING_WM_CHECK,
                self.ctx.atom._NET_WM_ALLOWED_ACTIONS,
                self.ctx.atom._NET_WM_ACTION_FULLSCREEN,
                self.ctx.atom._NET_WM_STATE,
                self.ctx.atom._NET_WM_STATE_FULLSCREEN,
                self.ctx.atom._NET_WM_WINDOW_TYPE,
                self.ctx.atom._NET_WM_WINDOW_TYPE_DIALOG,
                self.ctx.atom._NET_WM_MOVERESIZE,
                self.ctx.atom._NET_MOVERESIZE_WINDOW,
            ];
            utils::replace_property(
                &self.ctx,
                self.ctx.root,
                self.ctx.atom._NET_SUPPORTED,
                utils::Property::AtomList(&hints),
            )?;

            // _NET_SUPPORTING_WM_CHECK
            let ewmh_dummy_window = self.ctx.conn.generate_id()?;
            let depth = x11rb::COPY_DEPTH_FROM_PARENT;
            let class = xproto::WindowClass::INPUT_ONLY;
            let visual = x11rb::COPY_FROM_PARENT;
            let aux = xproto::CreateWindowAux::new();
            self.ctx.conn.create_window(
                depth,
                ewmh_dummy_window,
                self.ctx.root,
                -1, // x
                -1, // y
                1,  // width
                1,  // height
                0,  // border-width
                class,
                visual,
                &aux,
            )?;
            utils::replace_property(
                &self.ctx,
                self.ctx.root,
                self.ctx.atom._NET_SUPPORTING_WM_CHECK,
                utils::Property::Window(ewmh_dummy_window),
            )?;
            utils::replace_property(
                &self.ctx,
                ewmh_dummy_window,
                self.ctx.atom._NET_SUPPORTING_WM_CHECK,
                utils::Property::Window(ewmh_dummy_window),
            )?;
        }

        // create preview window
        {
            let (mut visual, mut depth) = (x11rb::COPY_FROM_PARENT, x11rb::COPY_DEPTH_FROM_PARENT);

            let setup = self.ctx.conn.setup();
            for d in setup.roots[0]
                .allowed_depths
                .iter()
                .filter(|d| d.depth == 32)
            {
                if let Some(v) = d.visuals.iter().find(|v| {
                    v.class == xproto::VisualClass::TRUE_COLOR && v.bits_per_rgb_value == 8
                }) {
                    visual = v.visual_id;
                    depth = 32;
                    break;
                }
            }

            let colormap = self.ctx.conn.generate_id()?;
            self.ctx
                .conn
                .create_colormap(xproto::ColormapAlloc::NONE, colormap, self.ctx.root, visual)?
                .check()?;

            let window = self.ctx.conn.generate_id()?;
            let class = xproto::WindowClass::INPUT_OUTPUT;

            // FIXME: config color
            let alpha = 0x80;
            let (red, green, blue) = (0xA3, 0x7A, 0x29);
            let bg_color = (alpha << 24)
                | (((red * alpha) >> 8) << 16)
                | (((green * alpha) >> 8) << 8)
                | ((blue * alpha) >> 8);

            let aux = xproto::CreateWindowAux::new()
                .colormap(colormap)
                .border_pixel(0xFFfaab23)
                .background_pixel(bg_color);
            self.ctx.conn.create_window(
                depth,
                window,
                self.ctx.root,
                -1, // x
                -1, // y
                1,  // w
                1,  // h
                config::WINDOW_BORDER_WIDTH as u16,
                class,
                visual,
                &aux,
            )?;
            self.ctx.conn.flush()?;

            self.preview_window = window;
        }

        // setup for desktops
        {
            self.desktops = vec![Desktop { monitor: None }; config::NUM_DESKTOPS];
        }

        // setup for monitors
        {
            // NOTE: randr version 1.2 or later
            self.ctx.conn.randr_select_input(
                self.ctx.root,
                randr::NotifyMask::CRTC_CHANGE | randr::NotifyMask::OUTPUT_CHANGE,
            )?;

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

    fn handle_event(&mut self, event: Event, cmdq: &mut VecDeque<Command>) -> Result<()> {
        log::trace!("handle_event: {event:?}");
        match event {
            Event::KeyPress(key_press) => {
                let keys: (u16, u8) = (key_press.state.into(), key_press.detail);
                if let Some(cmd) = self.keybind.get(&keys).cloned() {
                    cmdq.push_back(cmd);
                }
            }

            Event::ButtonPress(button_press) => {
                let x = button_press.root_x as i32;
                let y = button_press.root_y as i32;
                let clicked_window =
                    if button_press.child == x11rb::NONE && button_press.event == self.ctx.root {
                        None
                    } else {
                        Some(button_press.child)
                    };

                let mut allow = xproto::Allow::REPLAY_POINTER;

                const MOUSE_L: u8 = 1;
                const MOUSE_R: u8 = 3;
                if matches!(button_press.detail, MOUSE_L | MOUSE_R) {
                    let focus = clicked_window.unwrap_or_else(|| {
                        let mon = self
                            .monitors
                            .iter()
                            .position(|mon| mon.geometry.contains(x, y))
                            .unwrap_or(0);
                        self.monitors[mon].dummy_window
                    });
                    self.change_focus(focus)?;

                    if let Some(window) = self.windows.get_mut(&focus) {
                        if window.floating {
                            window.stacking_order = self.stacking_counter;
                            self.stacking_counter += 1;

                            let aux = xproto::ConfigureWindowAux::new()
                                .stack_mode(xproto::StackMode::BELOW)
                                .sibling(self.preview_window);
                            self.ctx.conn.configure_window(window.id, &aux)?;
                            self.ctx.conn.flush()?;
                        }
                    }
                }

                let hotkey = u16::from(config::HOT_KEY.keybutmask());
                if u16::from(button_press.state) & hotkey > 0 {
                    self.dnd_position = Some((x, y));
                    self.button_count += 1;
                    allow = xproto::Allow::SYNC_POINTER;
                }

                self.ctx.conn.allow_events(allow, x11rb::CURRENT_TIME)?;
                self.ctx.conn.flush()?;
            }

            Event::MotionNotify(motion) => {
                if let Some((prev_x, prev_y)) = self.dnd_position {
                    let x = motion.root_x as i32;
                    let y = motion.root_y as i32;
                    self.dnd_position = Some((x, y));

                    let dx = x - prev_x;
                    let dy = y - prev_y;

                    if let Some(window) = self.windows.get_mut(&self.focus) {
                        if !window.floating {
                            window.floating = true;
                            if let Some(monitor) = self.desktops[window.desktop].monitor {
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

                        let mon = self.desktops[window.desktop].monitor.unwrap();
                        let mg = self.monitors[mon].geometry;
                        let ax = mg.x + window.geometry.x;
                        let ay = mg.y + window.geometry.y;

                        if !mg.contains(x, y) {
                            // went out of the monitor

                            if let Some(new_monitor) =
                                self.monitors.iter().find(|mon| mon.geometry.contains(x, y))
                            {
                                window.desktop = new_monitor.desktop;
                                window.geometry.x = ax - new_monitor.geometry.x;
                                window.geometry.y = ay - new_monitor.geometry.y;
                            }
                        }

                        let mon = self.desktops[window.desktop].monitor.unwrap();
                        let mg = self.monitors[mon].geometry;
                        let aux = xproto::ConfigureWindowAux::new()
                            .x(mg.left() + window.geometry.x)
                            .y(mg.top() + window.geometry.y)
                            .width(window.geometry.w as u32)
                            .height(window.geometry.h as u32)
                            .stack_mode(xproto::StackMode::BELOW)
                            .sibling(self.preview_window);
                        self.ctx.conn.configure_window(window.id, &aux)?;
                        self.ctx.conn.flush()?;

                        let mut preview_visible = false;
                        if let Some(monitor) =
                            self.monitors.iter().find(|mon| mon.geometry.contains(x, y))
                        {
                            if let Some(geometry) = snap(monitor.geometry, x, y) {
                                preview_visible = true;
                                if geometry != self.preview_geometry {
                                    self.preview_geometry = geometry;
                                    let aux = xproto::ConfigureWindowAux::new()
                                        .stack_mode(xproto::StackMode::TOP_IF)
                                        .x(geometry.x)
                                        .y(geometry.y)
                                        .width(geometry.w as u32)
                                        .height(geometry.h as u32);
                                    self.ctx.conn.configure_window(self.preview_window, &aux)?;
                                }
                            }
                        }
                        if state & button1 > 0 && preview_visible {
                            self.ctx.conn.map_window(self.preview_window)?;
                            self.ctx.conn.flush()?;
                        } else {
                            self.preview_geometry = Rect::default();
                            self.ctx.conn.unmap_window(self.preview_window)?;
                            self.ctx.conn.flush()?;
                        }
                    }
                }
            }

            Event::ButtonRelease(button_release) => {
                self.button_count -= 1;

                let x = button_release.root_x as i32;
                let y = button_release.root_y as i32;

                if button_release.detail == 1 {
                    if let Some(window) = self.windows.get_mut(&self.focus) {
                        if let Some(monitor) = self
                            .monitors
                            .iter()
                            .position(|mon| mon.geometry.contains(x, y))
                        {
                            let mg = self.monitors[monitor].geometry;
                            if let Some(mut geometry) = snap(mg, x, y) {
                                geometry.x -= mg.x;
                                geometry.y -= mg.y;
                                if geometry != window.geometry {
                                    window.geometry = geometry;
                                    self.update_layout(monitor)?;
                                }
                            }
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
                    self.preview_geometry = Rect::default();
                    self.ctx.conn.unmap_window(self.preview_window)?;
                    self.ctx.conn.flush()?;
                }
            }

            Event::MapRequest(req) => {
                if let Some(window) = self.windows.get_mut(&req.window) {
                    if let Some(monitor) = self.desktops[window.desktop].monitor {
                        window.mapped = true;
                        window.stacking_order = self.stacking_counter;
                        self.stacking_counter += 1;

                        let window_id = window.id;
                        log::debug!(
                            "window 0x{:X} is mapped on desktop {}",
                            window_id,
                            window.desktop
                        );
                        self.update_layout(monitor)?;
                        self.ctx.conn.map_window(window_id)?;
                        self.change_focus(window_id)?;
                    }
                } else {
                    let geo = self.ctx.conn.get_geometry(req.window)?.reply()?;
                    let stacking_order = self.stacking_counter;
                    self.stacking_counter += 1;

                    let monitor = self.focused_monitor().unwrap_or(0);
                    let mon_geo = self.monitors[monitor].geometry;
                    let desktop = self.monitors[monitor].desktop;

                    let mut window = Window {
                        id: req.window,
                        desktop,
                        mapped: true,
                        floating: false,
                        fullscreen: false,
                        geometry: Rect {
                            x: (geo.x as i32) - mon_geo.x,
                            y: (geo.y as i32) - mon_geo.y,
                            w: geo.width as i32,
                            h: geo.height as i32,
                        },
                        stacking_order,
                        ignore_unmap_notify: false,
                    };

                    // place this window at the center of the monitor if its type is dialog
                    if utils::get_net_wm_window_type(&self.ctx, window.id)?
                        == Some(self.ctx.atom._NET_WM_WINDOW_TYPE_DIALOG)
                    {
                        window.floating = true;

                        let (center_x, center_y) = (mon_geo.w / 2, mon_geo.h / 2);
                        window.geometry.x = center_x - window.geometry.w / 2;
                        window.geometry.y = center_y - window.geometry.h / 2;
                    }

                    // _NET_WM_ALLOWED_ACTIONS
                    let actions = [self.ctx.atom._NET_WM_ACTION_FULLSCREEN];
                    utils::replace_property(
                        &self.ctx,
                        window.id,
                        self.ctx.atom._NET_WM_ALLOWED_ACTIONS,
                        utils::Property::AtomList(&actions),
                    )?;

                    let window_id = window.id;
                    log::debug!("window 0x{:X} added on desktop {}", window_id, desktop);
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
                        if let Some(monitor) = self.desktops[window.desktop].monitor {
                            log::debug!("window 0x{:X} is unmapped", window.id);
                            window.mapped = false;

                            if self.focus == window.id {
                                let desktop = window.desktop;
                                let any_window_on_desktop: xproto::Window =
                                    mapped_windows!(self, desktop)
                                        .map(|win| win.id)
                                        .next()
                                        .unwrap_or_else(|| self.monitors[monitor].dummy_window);
                                self.change_focus(any_window_on_desktop)?;
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
                if notify.sub_code == randr::Notify::OUTPUT_CHANGE {
                    let output_change = notify.u.as_oc();
                    log::debug!("RROutputChangeNotify: {output_change:?}");

                    if let Some(prog) = config::MONITOR_UPDATE_PROG {
                        cmdq.push_back(Command::SpawnProcess(prog.to_owned()));
                    }
                } else if notify.sub_code == randr::Notify::CRTC_CHANGE {
                    let crtc_change = notify.u.as_cc();
                    log::debug!("RRCrtcChangeNotify: {crtc_change:?}");

                    let crtc = crtc_change.crtc;
                    if let Some(monitor) = self.monitors.iter().position(|mon| mon.crtc == crtc) {
                        if crtc_change.mode == x11rb::NONE {
                            // monitor was disabled

                            let desktop = self.monitors[monitor].desktop;
                            let wins: Vec<xproto::Window> =
                                mapped_windows!(self, desktop).map(|win| win.id).collect();

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

                            self.desktops[desktop].monitor = None;

                            self.ctx
                                .conn
                                .destroy_window(self.monitors[monitor].dummy_window)?;
                            self.ctx.conn.flush()?;

                            self.monitors.swap_remove(monitor);
                            if monitor < self.monitors.len() {
                                let desktop = self.monitors[monitor].desktop;
                                self.desktops[desktop].monitor = Some(monitor);
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

                        let desktop = self
                            .desktops
                            .iter()
                            .position(|scr| scr.monitor.is_none())
                            .expect("too many monitors");
                        let geometry = Rect {
                            x: crtc_change.x as i32,
                            y: crtc_change.y as i32,
                            w: crtc_change.width as i32,
                            h: crtc_change.height as i32,
                        };
                        let monitor = self.add_monitor(crtc, geometry, desktop)?;

                        let mut focus = None;
                        for window in mapped_windows!(self, desktop) {
                            focus = Some(window.id);
                            self.ctx.conn.map_window(window.id)?;
                        }
                        self.ctx.conn.flush()?;

                        let focus: xproto::Window =
                            focus.unwrap_or_else(|| self.monitors[monitor].dummy_window);
                        self.change_focus(focus)?;
                    }
                }
            }

            Event::ConfigureRequest(req) => {
                if let Some(window) = self.windows.get_mut(&req.window) {
                    if window.floating {
                        let mut x = req.x as i32;
                        let mut y = req.y as i32;

                        if let Some(monitor) = self.desktops[window.desktop].monitor {
                            x -= self.monitors[monitor].geometry.x;
                            y -= self.monitors[monitor].geometry.y;
                        } else {
                            x = 0;
                            y = 0;
                        }

                        window.geometry.x = x;
                        window.geometry.y = y;
                        window.geometry.w = req.width as i32;
                        window.geometry.h = req.height as i32;

                        let aux = xproto::ConfigureWindowAux::from_configure_request(&req);
                        self.ctx.conn.configure_window(window.id, &aux)?;
                        self.ctx.conn.flush()?;
                    }
                } else {
                    let aux = xproto::ConfigureWindowAux::from_configure_request(&req);
                    self.ctx.conn.configure_window(req.window, &aux)?;
                    self.ctx.conn.flush()?;
                }
            }

            Event::ClientMessage(msg) => {
                log::debug!(
                    "ClientMessage({}): {:?}",
                    utils::get_atom_name(&self.ctx, msg.type_)?,
                    msg
                );

                // FIXME: tidy up this part
                if msg.type_ == self.ctx.atom._NET_WM_STATE {
                    let action = msg.data.as_data32()[0];
                    let first = msg.data.as_data32()[1];
                    let second = msg.data.as_data32()[2];

                    if action == 0 {
                        log::debug!("actioin: _NET_WM_STATE_REMOVE");
                    } else if action == 1 {
                        log::debug!("actioin: _NET_WM_STATE_ADD");
                    } else if action == 2 {
                        log::debug!("actioin: _NET_WM_STATE_TOGGLE");
                    }

                    log::debug!("first: {}", utils::get_atom_name(&self.ctx, first)?);
                    if second != 0 {
                        log::debug!("second: {}", utils::get_atom_name(&self.ctx, second)?);
                    }

                    if first == self.ctx.atom._NET_WM_STATE_FULLSCREEN
                        || second == self.ctx.atom._NET_WM_STATE_FULLSCREEN
                    {
                        if action == 0 {
                            // REMOVE
                            if let Some(window) = self.windows.get_mut(&msg.window) {
                                window.fullscreen = false;
                                if let Some(monitor) = self.desktops[window.desktop].monitor {
                                    self.update_layout(monitor)?;
                                }

                                let state = [];
                                utils::replace_property(
                                    &self.ctx,
                                    msg.window,
                                    self.ctx.atom._NET_WM_STATE,
                                    utils::Property::AtomList(&state),
                                )?;
                            }
                        } else if action == 1 {
                            // SET/ADD
                            if let Some(window) = self.windows.get_mut(&msg.window) {
                                window.fullscreen = true;
                                if let Some(monitor) = self.desktops[window.desktop].monitor {
                                    self.update_layout(monitor)?;
                                }

                                let state = [self.ctx.atom._NET_WM_STATE_FULLSCREEN];
                                utils::replace_property(
                                    &self.ctx,
                                    msg.window,
                                    self.ctx.atom._NET_WM_STATE,
                                    utils::Property::AtomList(&state),
                                )?;
                            }
                        }
                    }
                }
            }

            _ => {
                log::trace!("unhandled");
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
                    use std::process::{Command, Stdio};
                    let shell_cmdline = format!("{cmdline} &");
                    let mut child = Command::new("/bin/sh")
                        .arg("-c")
                        .arg(shell_cmdline)
                        .stdin(Stdio::null())
                        .stdout(Stdio::null())
                        .stderr(Stdio::null())
                        .spawn()
                        .unwrap();
                    child.wait().unwrap();
                }

                Command::FocusNextMonitor => {
                    let next = self
                        .focused_monitor()
                        .map(|i| (i + 1) % self.monitors.len())
                        .unwrap_or(0);

                    let desktop = self.monitors[next].desktop;
                    let any_window_on_next_monitor: xproto::Window = mapped_windows!(self, desktop)
                        .map(|win| win.id)
                        .next()
                        .unwrap_or_else(|| self.monitors[next].dummy_window);
                    self.change_focus(any_window_on_next_monitor)?;
                }

                Command::FocusNextWindow => {
                    if let Some(window) = self.windows.get(&self.focus) {
                        let desktop = window.desktop;
                        let monitor = self.desktops[desktop].monitor.unwrap();

                        let windows: Vec<xproto::Window> =
                            mapped_windows!(self, desktop).map(|win| win.id).collect();

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

                Command::SwitchDesktop(new_desktop) => {
                    if let Some(monitor_a) = self.desktops[new_desktop].monitor {
                        let desktop_a = new_desktop;
                        let monitor_b = self.focused_monitor().unwrap_or(0);
                        let desktop_b = self.monitors[monitor_b].desktop;

                        self.monitors[monitor_a].desktop = desktop_b;
                        self.monitors[monitor_b].desktop = desktop_a;
                        self.desktops[desktop_a].monitor = Some(monitor_b);
                        self.desktops[desktop_b].monitor = Some(monitor_a);
                        self.update_layout(monitor_a)?;
                        self.update_layout(monitor_b)?;

                        let any_window_on_new_desktop: xproto::Window =
                            mapped_windows!(self, new_desktop)
                                .map(|win| win.id)
                                .next()
                                .unwrap_or_else(|| self.monitors[monitor_b].dummy_window);
                        self.change_focus(any_window_on_new_desktop)?;
                    } else {
                        let monitor = self.focused_monitor().unwrap_or(0);
                        let current_desktop = self.monitors[monitor].desktop;

                        for window in mapped_windows_mut!(self, current_desktop) {
                            window.ignore_unmap_notify = true;
                            self.ctx.conn.unmap_window(window.id)?;
                        }
                        for window in mapped_windows!(self, new_desktop) {
                            self.ctx.conn.map_window(window.id)?;
                        }
                        self.ctx.conn.flush()?;

                        self.monitors[monitor].desktop = new_desktop;
                        self.desktops[new_desktop].monitor = Some(monitor);
                        self.desktops[current_desktop].monitor = None;
                        self.update_layout(monitor)?;

                        let any_window_on_new_desktop: xproto::Window =
                            mapped_windows!(self, new_desktop)
                                .map(|win| win.id)
                                .next()
                                .unwrap_or_else(|| self.monitors[monitor].dummy_window);
                        self.change_focus(any_window_on_new_desktop)?;
                    }
                }

                Command::MoveWindow(new_desktop) => {
                    if let Some(window) = self.windows.get_mut(&self.focus) {
                        let old_desktop = window.desktop;
                        let old_monitor = self.desktops[old_desktop].monitor.unwrap();
                        let new_monitor = self.desktops[new_desktop].monitor;

                        window.desktop = new_desktop;
                        if new_monitor.is_none() {
                            window.ignore_unmap_notify = true;
                            self.ctx.conn.unmap_window(window.id)?;
                            self.ctx.conn.flush()?;

                            if self.focus == window.id {
                                let any_window_on_desktop: xproto::Window =
                                    mapped_windows!(self, old_desktop)
                                        .map(|win| win.id)
                                        .next()
                                        .unwrap_or_else(|| self.monitors[old_monitor].dummy_window);
                                self.change_focus(any_window_on_desktop)?;
                            }
                        }

                        self.update_layout(old_monitor)?;
                        if let Some(mon) = new_monitor {
                            self.update_layout(mon)?;
                        }
                    }
                }

                Command::ToggleFloating => {
                    if let Some(window) = self.windows.get_mut(&self.focus) {
                        window.floating ^= true;
                        if let Some(monitor) = self.desktops[window.desktop].monitor {
                            self.update_layout(monitor)?;
                        }
                    }
                }
            }
        }
        Ok(())
    }

    fn add_monitor(&mut self, crtc: randr::Crtc, geometry: Rect, desktop: usize) -> Result<usize> {
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
            desktop,
            dummy_window,
            geometry,
        });
        self.desktops[desktop].monitor = Some(i);

        self.update_layout(i)?;
        Ok(i)
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
            let desktop = window.desktop;
            log::debug!("window 0x{:X} removed from desktop {}", window.id, desktop);
            if let Some(monitor) = self.desktops[desktop].monitor {
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

        let desktop = self.monitors[monitor].desktop;
        let mon_geo = self.monitors[monitor].geometry;
        let bwidth = config::WINDOW_BORDER_WIDTH as i32;

        // normal windows

        let sinked_windows: Vec<xproto::Window> = mapped_windows!(self, desktop)
            .filter(|win| !win.floating && !win.fullscreen)
            .map(|win| win.id)
            .collect();

        // NOTE: horizontal layout
        if !sinked_windows.is_empty() {
            let n = sinked_windows.len();
            let each_w = mon_geo.w / n as i32;
            let last_w = mon_geo.w - (n as i32 - 1) * each_w;
            let each_h = mon_geo.h;

            for (i, win) in sinked_windows.into_iter().enumerate() {
                let x = each_w * (i as i32);
                let y = 0;
                let w = if i < n - 1 { each_w } else { last_w };

                let geo = Rect {
                    x,
                    y,
                    w: w - bwidth * 2,
                    h: each_h - bwidth * 2,
                };
                self.windows.get_mut(&win).unwrap().geometry = geo;

                let aux = xproto::ConfigureWindowAux::new()
                    .stack_mode(xproto::StackMode::ABOVE)
                    .x(mon_geo.x + geo.x)
                    .y(mon_geo.y + geo.y)
                    .width(geo.w as u32)
                    .height(geo.h as u32)
                    .border_width(bwidth as u32);
                self.ctx.conn.configure_window(win, &aux)?;
            }
        }

        // floating windows

        let mut floating_windows: Vec<Window> = mapped_windows!(self, desktop)
            .filter(|win| win.floating && !win.fullscreen)
            .cloned()
            .collect();
        floating_windows.sort_by_key(|win| win.stacking_order);

        for win in floating_windows {
            let aux = xproto::ConfigureWindowAux::new()
                .stack_mode(xproto::StackMode::ABOVE)
                .x(mon_geo.x + win.geometry.x)
                .y(mon_geo.y + win.geometry.y)
                .width(win.geometry.w as u32)
                .height(win.geometry.h as u32)
                .border_width(bwidth as u32);
            self.ctx.conn.configure_window(win.id, &aux)?;
        }

        // fullscreen windows

        let mut fullscreen_windows: Vec<Window> = mapped_windows!(self, desktop)
            .filter(|win| win.fullscreen)
            .cloned()
            .collect();
        fullscreen_windows.sort_by_key(|win| win.stacking_order);

        for win in fullscreen_windows {
            let aux = xproto::ConfigureWindowAux::new()
                .stack_mode(xproto::StackMode::ABOVE)
                .x(mon_geo.x)
                .y(mon_geo.y)
                .width(mon_geo.w as u32)
                .height(mon_geo.h as u32)
                .border_width(0);
            self.ctx.conn.configure_window(win.id, &aux)?;
        }

        let aux = xproto::ConfigureWindowAux::new().stack_mode(xproto::StackMode::ABOVE);
        self.ctx.conn.configure_window(self.preview_window, &aux)?;

        self.ctx.conn.flush()?;
        Ok(())
    }

    fn focused_monitor(&mut self) -> Option<usize> {
        if let Some(window) = self.windows.get(&self.focus) {
            self.desktops[window.desktop].monitor
        } else {
            self.monitors
                .iter()
                .position(|mon| mon.dummy_window == self.focus)
        }
    }
}

fn snap(monitor_geometry: Rect, x: i32, y: i32) -> Option<Rect> {
    let mg = monitor_geometry;
    let d = config::SNAPPING_WIDTH as i32;
    let bwidth = config::WINDOW_BORDER_WIDTH as i32;

    let left = mg.left() <= x && x < mg.left() + d;
    let right = mg.right() - d <= x && x < mg.right();
    let top = mg.top() <= y && y < mg.top() + d;
    let bottom = mg.bottom() - d <= y && y < mg.bottom();
    let x_center = mg.x + mg.w / 2 - d <= x && x <= mg.x + mg.w / 2 + d;
    let y_center = mg.y + mg.h / 2 - d <= y && y <= mg.y + mg.h / 2 + d;

    let mut geometry = Rect::default();
    if left && top {
        geometry.x = mg.x;
        geometry.y = mg.y;
        geometry.w = mg.w / 2 - bwidth * 2;
        geometry.h = mg.h / 2 - bwidth * 2;
    } else if left && bottom {
        geometry.x = mg.x;
        geometry.y = mg.y + mg.h / 2;
        geometry.w = mg.w / 2 - bwidth * 2;
        geometry.h = mg.h - mg.h / 2 - bwidth * 2;
    } else if right && top {
        geometry.x = mg.x + mg.w / 2;
        geometry.y = mg.y;
        geometry.w = mg.w - mg.w / 2 - bwidth * 2;
        geometry.h = mg.h / 2 - bwidth * 2;
    } else if right && bottom {
        geometry.x = mg.x + mg.w / 2;
        geometry.y = mg.y + mg.h / 2;
        geometry.w = mg.w - mg.w / 2 - bwidth * 2;
        geometry.h = mg.h - mg.h / 2 - bwidth * 2;
    } else if left {
        geometry.x = mg.x;
        geometry.y = mg.y;
        geometry.w = mg.w / 2 - bwidth * 2;
        geometry.h = mg.h - bwidth * 2;
    } else if right {
        geometry.x = mg.x + mg.w / 2;
        geometry.y = mg.y;
        geometry.w = mg.w - mg.w / 2 - bwidth * 2;
        geometry.h = mg.h - bwidth * 2;
    } else if top {
        geometry.x = mg.x;
        geometry.y = mg.y;
        geometry.w = mg.w - bwidth * 2;
        geometry.h = mg.h / 2 - bwidth * 2;
    } else if bottom {
        geometry.x = mg.x;
        geometry.y = mg.y + mg.h / 2;
        geometry.w = mg.w - bwidth * 2;
        geometry.h = mg.h - mg.h / 2 - bwidth * 2;
    } else if x_center && y_center {
        geometry.x = mg.x;
        geometry.y = mg.y;
        geometry.w = mg.w - bwidth * 2;
        geometry.h = mg.h - bwidth * 2;
    } else {
        return None;
    }

    Some(geometry)
}
