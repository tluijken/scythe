//! The main window's place on screen, and the main window as the owner of dialogs.

/// A rectangle in physical pixels: left, top, right, bottom.
pub type Bounds = [i32; 4];

/// `window` moved, and shrunk where it is larger, to lie inside `work` (a monitor's work area).
/// A window that was larger than the area is centred in it.
pub fn fit(window: Bounds, work: Bounds) -> Bounds {
    let (width, height) = (window[2] - window[0], window[3] - window[1]);
    let (room_w, room_h) = (work[2] - work[0], work[3] - work[1]);
    let (w, h) = (width.min(room_w), height.min(room_h));
    let (left, top) = if width > room_w || height > room_h {
        (work[0] + (room_w - w) / 2, work[1] + (room_h - h) / 2)
    } else {
        (window[0].clamp(work[0], work[2] - w), window[1].clamp(work[1], work[3] - h))
    };
    [left, top, left + w, top + h]
}

#[cfg(windows)]
mod windows {
    use std::num::NonZeroIsize;
    use std::sync::OnceLock;

    use raw_window_handle::{
        DisplayHandle, HandleError, HasDisplayHandle, HasWindowHandle, RawWindowHandle, Win32WindowHandle, WindowHandle,
    };
    use windows_sys::Win32::Foundation::{HWND, RECT};
    use windows_sys::Win32::Graphics::Gdi::{
        GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{GetWindowRect, SWP_NOACTIVATE, SWP_NOZORDER, SetWindowPos};

    /// The main window, for dialogs to be owned by it: Windows then opens them over it.
    #[derive(Clone, Copy)]
    pub struct Owner(NonZeroIsize);

    static MAIN: OnceLock<Owner> = OnceLock::new();

    /// Remembers the main window and keeps it inside its monitor's work area: a size given in
    /// points can be larger than the screen at a high scaling, and Windows cascades each new
    /// window further from the corner.
    pub fn place(window: &impl HasWindowHandle) {
        let Ok(handle) = window.window_handle() else { return };
        let RawWindowHandle::Win32(handle) = handle.as_raw() else { return };
        let _ = MAIN.set(Owner(handle.hwnd));
        let hwnd = handle.hwnd.get() as HWND;
        // SAFETY: the window exists while the creation context lends its handle; the calls only
        // read its rectangle and its monitor's, and move it.
        unsafe {
            let mut rect: RECT = std::mem::zeroed();
            let mut info: MONITORINFO = std::mem::zeroed();
            info.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
            if GetWindowRect(hwnd, &mut rect) == 0
                || GetMonitorInfoW(MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST), &mut info) == 0
            {
                return;
            }
            let now = [rect.left, rect.top, rect.right, rect.bottom];
            let work = info.rcWork;
            let [left, top, right, bottom] = super::fit(now, [work.left, work.top, work.right, work.bottom]);
            if [left, top, right, bottom] != now {
                SetWindowPos(
                    hwnd,
                    std::ptr::null_mut(),
                    left,
                    top,
                    right - left,
                    bottom - top,
                    SWP_NOZORDER | SWP_NOACTIVATE,
                );
            }
        }
    }

    /// The main window, once [`place`] has seen it.
    pub fn owner() -> Option<Owner> {
        MAIN.get().copied()
    }

    impl HasWindowHandle for Owner {
        fn window_handle(&self) -> Result<WindowHandle<'_>, HandleError> {
            // SAFETY: the main window lives as long as the process shows dialogs.
            Ok(unsafe { WindowHandle::borrow_raw(RawWindowHandle::Win32(Win32WindowHandle::new(self.0))) })
        }
    }

    impl HasDisplayHandle for Owner {
        fn display_handle(&self) -> Result<DisplayHandle<'_>, HandleError> {
            Ok(DisplayHandle::windows())
        }
    }
}

#[cfg(windows)]
pub use windows::{owner, place};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_window_larger_than_the_work_area_is_shrunk_and_centred() {
        // As measured at 150 % on a 2048x1536 screen with the taskbar at the bottom.
        assert_eq!(fit([342, 342, 2412, 1808], [0, 0, 2048, 1488]), [0, 11, 2048, 1477]);
        assert_eq!(fit([100, 50, 1100, 2050], [0, 0, 1920, 1032]), [460, 0, 1460, 1032]);
    }

    #[test]
    fn a_window_that_fits_is_moved_back_on_screen() {
        assert_eq!(fit([900, 700, 2300, 1600], [0, 0, 2048, 1488]), [648, 588, 2048, 1488]);
        assert_eq!(fit([-300, -20, 700, 580], [0, 0, 2048, 1488]), [0, 0, 1000, 600]);
        assert_eq!(fit([228, 228, 1628, 1128], [0, 0, 2048, 1488]), [228, 228, 1628, 1128], "already inside");
        assert_eq!(fit([2100, 100, 2900, 700], [1920, 0, 3840, 1040]), [2100, 100, 2900, 700], "on a second monitor");
    }
}
