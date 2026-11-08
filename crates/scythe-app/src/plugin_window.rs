//! Top-level X11 windows that host plugin editors. Plugins on Linux embed into an X11 window,
//! so these use their own X connection (XWayland under Wayland) next to the egui window.

use x11rb::connection::Connection as _;
use x11rb::protocol::Event;
use x11rb::protocol::xproto::{
    AtomEnum, ClientMessageEvent, ConfigureWindowAux, ConnectionExt as _, CreateWindowAux, EventMask, PropMode,
    WindowClass,
};
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;

pub struct PluginWindow {
    conn: RustConnection,
    pub window: u32,
    delete: u32,
}

impl PluginWindow {
    pub fn new(title: &str, width: u32, height: u32) -> Result<Self, String> {
        let (conn, screen) = x11rb::connect(None).map_err(|e| format!("no X11 display: {e}"))?;
        let root = conn.setup().roots[screen].root;
        let window = conn.generate_id().map_err(|e| e.to_string())?;
        let aux = CreateWindowAux::new().background_pixel(0x1e1f1c).event_mask(EventMask::STRUCTURE_NOTIFY);
        conn.create_window(
            x11rb::COPY_DEPTH_FROM_PARENT,
            window,
            root,
            0,
            0,
            width.clamp(1, 8192) as u16,
            height.clamp(1, 8192) as u16,
            0,
            WindowClass::INPUT_OUTPUT,
            x11rb::COPY_FROM_PARENT,
            &aux,
        )
        .map_err(|e| e.to_string())?;
        let atom = |name: &str| -> Result<u32, String> {
            Ok(conn
                .intern_atom(false, name.as_bytes())
                .map_err(|e| e.to_string())?
                .reply()
                .map_err(|e| e.to_string())?
                .atom)
        };
        let (protocols, delete, utf8, net_name) =
            (atom("WM_PROTOCOLS")?, atom("WM_DELETE_WINDOW")?, atom("UTF8_STRING")?, atom("_NET_WM_NAME")?);
        conn.change_property32(PropMode::REPLACE, window, protocols, AtomEnum::ATOM, &[delete])
            .map_err(|e| e.to_string())?;
        conn.change_property8(PropMode::REPLACE, window, AtomEnum::WM_NAME, AtomEnum::STRING, title.as_bytes())
            .map_err(|e| e.to_string())?;
        conn.change_property8(PropMode::REPLACE, window, net_name, utf8, title.as_bytes())
            .map_err(|e| e.to_string())?;
        conn.map_window(window).map_err(|e| e.to_string())?;
        conn.flush().map_err(|e| e.to_string())?;
        Ok(Self { conn, window, delete })
    }

    pub fn resize(&self, width: u32, height: u32) {
        let aux = ConfigureWindowAux::new().width(width.clamp(1, 8192)).height(height.clamp(1, 8192));
        let _ = self.conn.configure_window(self.window, &aux);
        let _ = self.conn.flush();
    }

    /// Handles pending events; true once the window manager asked the window to close.
    pub fn close_requested(&self) -> bool {
        let mut close = false;
        while let Ok(Some(event)) = self.conn.poll_for_event() {
            if let Event::ClientMessage(ClientMessageEvent { data, .. }) = event
                && data.as_data32()[0] == self.delete
            {
                close = true;
            }
        }
        close
    }
}

impl Drop for PluginWindow {
    fn drop(&mut self) {
        let _ = self.conn.destroy_window(self.window);
        let _ = self.conn.flush();
    }
}
