//! The window a file dialog belongs to: the one the user just clicked in, which holds the
//! editor, so the dialog opens in front of it rather than behind the host's windows.

use raw_window_handle::{DisplayHandle, HandleError, HasDisplayHandle, HasWindowHandle, RawWindowHandle, WindowHandle};

/// A window a dialog is shown over; used only while the dialog is being set up.
pub(crate) struct Owner(RawWindowHandle);

impl HasWindowHandle for Owner {
    fn window_handle(&self) -> Result<WindowHandle<'_>, HandleError> {
        Ok(unsafe { WindowHandle::borrow_raw(self.0) })
    }
}

impl HasDisplayHandle for Owner {
    fn display_handle(&self) -> Result<DisplayHandle<'_>, HandleError> {
        Err(HandleError::Unavailable)
    }
}

/// The X11 window the window manager says is active.
#[cfg(all(unix, not(target_os = "macos")))]
pub(crate) fn active() -> Option<Owner> {
    use x11rb::connection::Connection;
    use x11rb::protocol::xproto::{AtomEnum, ConnectionExt};
    let (conn, screen) = x11rb::connect(None).ok()?;
    let root = conn.setup().roots.get(screen)?.root;
    let atom = conn.intern_atom(true, b"_NET_ACTIVE_WINDOW").ok()?.reply().ok()?.atom;
    let reply = conn.get_property(false, root, atom, AtomEnum::WINDOW, 0, 1).ok()?.reply().ok()?;
    let window = std::num::NonZeroU32::new(reply.value32()?.next()?)?;
    Some(Owner(raw_window_handle::XcbWindowHandle::new(window).into()))
}

/// The key window's content view; on the main thread only, where the editor's frame runs.
#[cfg(target_os = "macos")]
pub(crate) fn active() -> Option<Owner> {
    let mtm = objc2::MainThreadMarker::new()?;
    let window = objc2_app_kit::NSApplication::sharedApplication(mtm).keyWindow()?;
    let view = window.contentView()?;
    let view = std::ptr::NonNull::from(&*view).cast();
    Some(Owner(raw_window_handle::AppKitWindowHandle::new(view).into()))
}
