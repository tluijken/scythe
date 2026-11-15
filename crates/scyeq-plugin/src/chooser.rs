//! Whether a file dialog can show on Linux. rfd asks the desktop's file chooser portal and falls
//! back to zenity; when neither is there it returns no file, as it does after a cancel.

use std::ffi::{CStr, OsStr, c_char, c_int, c_uint, c_void};
use std::path::Path;

/// Whether rfd has a file dialog to show: zenity on the `PATH`, or a file chooser portal on the
/// session bus.
pub(crate) fn available() -> bool {
    std::env::var_os("PATH").is_some_and(|path| on_path(&path, "zenity")) || file_chooser_portal(None)
}

fn on_path(path: &OsStr, program: &str) -> bool {
    use std::os::unix::fs::PermissionsExt;
    let executable = |file: &Path| file.metadata().is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0);
    std::env::split_paths(path).any(|dir| executable(&dir.join(program)))
}

/// Whether the session bus, or the bus at `address`, answers for the portal's file chooser, the
/// interface rfd calls. The bus starts the portal when it can, as it would for rfd.
fn file_chooser_portal(address: Option<&CStr>) -> bool {
    Dbus::open().is_some_and(|dbus| unsafe { dbus.has_file_chooser(address) })
}

#[repr(C)]
struct DbusError {
    name: *const c_char,
    message: *const c_char,
    dummy: c_uint,
    padding: *const c_void,
}

const SESSION_BUS: c_int = 0;
const TYPE_STRING: c_int = b's' as c_int;
const TYPE_INVALID: c_int = 0;

/// The few libdbus calls the check needs, from the libdbus-1 rfd loads too.
struct Dbus {
    error_init: unsafe extern "C" fn(*mut DbusError),
    error_free: unsafe extern "C" fn(*mut DbusError),
    bus_get_private: unsafe extern "C" fn(c_int, *mut DbusError) -> *mut c_void,
    connection_open_private: unsafe extern "C" fn(*const c_char, *mut DbusError) -> *mut c_void,
    bus_register: unsafe extern "C" fn(*mut c_void, *mut DbusError) -> c_uint,
    set_exit_on_disconnect: unsafe extern "C" fn(*mut c_void, c_uint),
    message_new_method_call:
        unsafe extern "C" fn(*const c_char, *const c_char, *const c_char, *const c_char) -> *mut c_void,
    message_append_args: unsafe extern "C" fn(*mut c_void, c_int, ...) -> c_uint,
    send_with_reply_and_block: unsafe extern "C" fn(*mut c_void, *mut c_void, c_int, *mut DbusError) -> *mut c_void,
    message_unref: unsafe extern "C" fn(*mut c_void),
    connection_close: unsafe extern "C" fn(*mut c_void),
    connection_unref: unsafe extern "C" fn(*mut c_void),
}

impl Dbus {
    /// Never closed: libdbus keeps state for the process, and rfd holds the same library open.
    fn open() -> Option<Self> {
        unsafe {
            let lib = [c"libdbus-1.so.3", c"libdbus-1.so"]
                .into_iter()
                .map(|name| libc::dlopen(name.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL))
                .find(|lib| !lib.is_null())?;
            macro_rules! symbol {
                ($name:literal) => {{
                    let symbol = libc::dlsym(lib, $name.as_ptr());
                    if symbol.is_null() {
                        return None;
                    }
                    std::mem::transmute_copy(&symbol)
                }};
            }
            Some(Self {
                error_init: symbol!(c"dbus_error_init"),
                error_free: symbol!(c"dbus_error_free"),
                bus_get_private: symbol!(c"dbus_bus_get_private"),
                connection_open_private: symbol!(c"dbus_connection_open_private"),
                bus_register: symbol!(c"dbus_bus_register"),
                set_exit_on_disconnect: symbol!(c"dbus_connection_set_exit_on_disconnect"),
                message_new_method_call: symbol!(c"dbus_message_new_method_call"),
                message_append_args: symbol!(c"dbus_message_append_args"),
                send_with_reply_and_block: symbol!(c"dbus_connection_send_with_reply_and_block"),
                message_unref: symbol!(c"dbus_message_unref"),
                connection_close: symbol!(c"dbus_connection_close"),
                connection_unref: symbol!(c"dbus_connection_unref"),
            })
        }
    }

    unsafe fn has_file_chooser(&self, address: Option<&CStr>) -> bool {
        unsafe {
            let mut error = std::mem::zeroed::<DbusError>();
            (self.error_init)(&mut error);
            let conn = match address {
                None => (self.bus_get_private)(SESSION_BUS, &mut error),
                Some(address) => {
                    let conn = (self.connection_open_private)(address.as_ptr(), &mut error);
                    if !conn.is_null() && (self.bus_register)(conn, &mut error) == 0 {
                        (self.connection_close)(conn);
                        (self.connection_unref)(conn);
                        std::ptr::null_mut()
                    } else {
                        conn
                    }
                }
            };
            if conn.is_null() {
                (self.error_free)(&mut error);
                return false;
            }
            // libdbus would end the host's process should the bus go away during the call.
            (self.set_exit_on_disconnect)(conn, 0);
            let message = (self.message_new_method_call)(
                c"org.freedesktop.portal.Desktop".as_ptr(),
                c"/org/freedesktop/portal/desktop".as_ptr(),
                c"org.freedesktop.DBus.Properties".as_ptr(),
                c"Get".as_ptr(),
            );
            let mut found = false;
            if !message.is_null() {
                let interface = c"org.freedesktop.portal.FileChooser".as_ptr();
                let property = c"version".as_ptr();
                let appended = (self.message_append_args)(
                    message,
                    TYPE_STRING,
                    &interface as *const *const c_char,
                    TYPE_STRING,
                    &property as *const *const c_char,
                    TYPE_INVALID,
                );
                if appended != 0 {
                    let reply = (self.send_with_reply_and_block)(conn, message, -1, &mut error);
                    if !reply.is_null() {
                        found = true;
                        (self.message_unref)(reply);
                    }
                }
                (self.message_unref)(message);
            }
            (self.error_free)(&mut error);
            (self.connection_close)(conn);
            (self.connection_unref)(conn);
            found
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_program_is_found_only_where_it_is_executable() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("scyeq-chooser-{}", std::process::id()));
        let (bin, empty) = (dir.join("bin"), dir.join("empty"));
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::create_dir_all(&empty).unwrap();
        std::fs::write(bin.join("zenity"), "").unwrap();
        std::fs::write(bin.join("readme"), "").unwrap();
        std::fs::set_permissions(bin.join("zenity"), std::fs::Permissions::from_mode(0o755)).unwrap();
        let path = std::env::join_paths([&empty, &bin]).unwrap();
        let found = (on_path(&path, "zenity"), on_path(&path, "readme"), on_path(empty.as_os_str(), "zenity"));
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(found, (true, false, false));
    }

    #[test]
    fn a_bus_that_is_not_there_has_no_file_chooser() {
        assert!(!file_chooser_portal(Some(c"unix:path=/nonexistent/scyeq-bus")));
    }
}
