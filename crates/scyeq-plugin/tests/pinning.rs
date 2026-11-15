//! The built library, once pinned as the editor pins it before a file dialog, stays loaded
//! through its last close. Run on the library cargo built for this test run, not on the test
//! executable, which cannot be pinned so.

#![cfg(unix)]

use std::ffi::CString;
use std::os::unix::ffi::OsStrExt;

#[path = "built/library.rs"]
mod library;

#[test]
fn a_pinned_library_outlives_its_last_close() {
    let path = CString::new(library::library("scyeq_plugin").as_os_str().as_bytes()).unwrap();
    unsafe {
        let handle = libc::dlopen(path.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL);
        assert!(!handle.is_null());
        let entry = libc::dlsym(handle, c"clap_entry".as_ptr());
        assert!(!entry.is_null());
        assert!(scyeq_plugin::pin_library_of(entry as *const ()));
        assert_eq!(libc::dlclose(handle), 0);
        let still = libc::dlopen(path.as_ptr(), libc::RTLD_NOW | libc::RTLD_NOLOAD);
        assert!(!still.is_null(), "unloaded by its last close");
        libc::dlclose(still);
        assert!(!scyeq_plugin::pin_library_of(std::ptr::null()), "no library holds a null pointer");
    }
}
