//! Top-level windows that host plugin editors. Plugins embed into a native window: on Linux an
//! X11 window, with its own X connection (XWayland under Wayland) next to the egui window; on
//! Windows a Win32 window, sized for its monitor's DPI. They live on the plugin thread
//! ([`scythe_engine::plugin_thread`]), whose message loop serves them on Windows.

#[cfg(not(windows))]
pub use x11::PluginWindow;

#[cfg(windows)]
pub use win32::PluginWindow;

#[cfg(not(windows))]
mod x11 {
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
        window: u32,
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

        /// The window id editors embed into.
        pub fn handle(&self) -> usize {
            self.window as usize
        }

        /// X11 editors scale themselves; their window is sized as they ask.
        pub fn scale(&self) -> f64 {
            1.0
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
}

#[cfg(windows)]
mod win32 {
    use std::sync::OnceLock;
    use std::sync::atomic::{AtomicBool, Ordering};

    use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
    use windows_sys::Win32::Graphics::Gdi::CreateSolidBrush;
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::UI::HiDpi::{AdjustWindowRectExForDpi, GetDpiForWindow};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CS_HREDRAW, CS_VREDRAW, CW_USEDEFAULT, CreateWindowExW, DefWindowProcW, DestroyWindow, GWLP_USERDATA,
        GetWindowLongPtrW, IDC_ARROW, LoadCursorW, RegisterClassW, SW_SHOW, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOZORDER,
        SetWindowLongPtrW, SetWindowPos, ShowWindow, WM_CLOSE, WNDCLASSW, WS_CAPTION, WS_MINIMIZEBOX, WS_OVERLAPPED,
        WS_SYSMENU,
    };

    const STYLE: u32 = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX;

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    pub struct PluginWindow {
        hwnd: HWND,
        /// Set by the window procedure on `WM_CLOSE`; boxed so its address stays put.
        close: Box<AtomicBool>,
    }

    /// Closing only raises the flag, so the editor is detached before the window goes.
    unsafe extern "system" fn window_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        if msg == WM_CLOSE {
            // SAFETY: GWLP_USERDATA holds the window's close flag or 0, and the flag outlives
            // the window.
            let flag = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *const AtomicBool;
            if let Some(flag) = unsafe { flag.as_ref() } {
                flag.store(true, Ordering::Relaxed);
            }
            return 0;
        }
        // SAFETY: the default handling for every other message.
        unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
    }

    fn class() -> &'static [u16] {
        static CLASS: OnceLock<Vec<u16>> = OnceLock::new();
        CLASS.get_or_init(|| {
            let name = wide("ScythePluginWindow");
            // SAFETY: registers a class whose name and procedure live for the whole program.
            unsafe {
                let class = WNDCLASSW {
                    style: CS_HREDRAW | CS_VREDRAW,
                    lpfnWndProc: Some(window_proc),
                    cbClsExtra: 0,
                    cbWndExtra: 0,
                    hInstance: GetModuleHandleW(std::ptr::null()),
                    hIcon: std::ptr::null_mut(),
                    hCursor: LoadCursorW(std::ptr::null_mut(), IDC_ARROW),
                    hbrBackground: CreateSolidBrush(0x001c_1f1e),
                    lpszMenuName: std::ptr::null(),
                    lpszClassName: name.as_ptr(),
                };
                RegisterClassW(&class);
            }
            name
        })
    }

    /// The outer size of a window whose client area is `width` x `height` at `dpi`.
    fn outer(width: u32, height: u32, dpi: u32) -> (i32, i32) {
        let mut rect =
            RECT { left: 0, top: 0, right: width.clamp(1, 8192) as i32, bottom: height.clamp(1, 8192) as i32 };
        // SAFETY: only fills `rect`.
        unsafe { AdjustWindowRectExForDpi(&mut rect, STYLE, 0, 0, dpi) };
        (rect.right - rect.left, rect.bottom - rect.top)
    }

    impl PluginWindow {
        pub fn new(title: &str, width: u32, height: u32) -> Result<Self, String> {
            let class = class();
            let title = wide(title);
            let (w, h) = outer(width, height, 96);
            let close = Box::new(AtomicBool::new(false));
            // SAFETY: creates a top-level window of the registered class on this thread.
            let hwnd = unsafe {
                CreateWindowExW(
                    0,
                    class.as_ptr(),
                    title.as_ptr(),
                    STYLE,
                    CW_USEDEFAULT,
                    CW_USEDEFAULT,
                    w,
                    h,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    GetModuleHandleW(std::ptr::null()),
                    std::ptr::null(),
                )
            };
            if hwnd.is_null() {
                return Err(format!("cannot create a window: {}", std::io::Error::last_os_error()));
            }
            // SAFETY: the flag is boxed in the returned value, whose drop clears the pointer.
            unsafe {
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, &*close as *const AtomicBool as isize);
                ShowWindow(hwnd, SW_SHOW);
            }
            let window = Self { hwnd, close };
            window.resize(width, height);
            Ok(window)
        }

        /// The `HWND` editors embed into.
        pub fn handle(&self) -> usize {
            self.hwnd as usize
        }

        /// The scale of the monitor the window is on, 1.0 at 96 dpi.
        pub fn scale(&self) -> f64 {
            // SAFETY: a plain query on this window.
            let dpi = unsafe { GetDpiForWindow(self.hwnd) };
            if dpi == 0 { 1.0 } else { f64::from(dpi) / 96.0 }
        }

        /// Gives the window a client area of `width` x `height` physical pixels.
        pub fn resize(&self, width: u32, height: u32) {
            // SAFETY: a plain query on this window.
            let dpi = unsafe { GetDpiForWindow(self.hwnd) }.max(96);
            let (w, h) = outer(width, height, dpi);
            // SAFETY: resizes this window only.
            unsafe {
                SetWindowPos(self.hwnd, std::ptr::null_mut(), 0, 0, w, h, SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE)
            };
        }

        /// True once the user asked the window to close. Its messages arrive through the plugin
        /// thread's message loop.
        pub fn close_requested(&self) -> bool {
            self.close.load(Ordering::Relaxed)
        }
    }

    impl Drop for PluginWindow {
        fn drop(&mut self) {
            // SAFETY: the window belongs to this value; the flag pointer is cleared first so a
            // late WM_CLOSE cannot reach freed memory.
            unsafe {
                SetWindowLongPtrW(self.hwnd, GWLP_USERDATA, 0);
                DestroyWindow(self.hwnd);
            }
        }
    }
}
