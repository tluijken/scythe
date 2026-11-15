//! Loads a VST3 module and calls its platform's entry point, as a host does before anything else.

use crate::built::Built;

/// Stays loaded for the whole run, as it would in a host.
pub fn load(built: &Built) -> libloading::Library {
    unsafe {
        let library = libloading::Library::new(&built.binary).unwrap();
        #[cfg(windows)]
        {
            let entry: libloading::Symbol<extern "system" fn() -> bool> = library.get(b"InitDll\0").unwrap();
            assert!(entry());
        }
        #[cfg(target_os = "macos")]
        {
            let entry: libloading::Symbol<extern "system" fn(*mut std::ffi::c_void) -> bool> =
                library.get(b"bundleEntry\0").unwrap();
            assert!(mac::with_bundle(&built.path, |bundle| entry(bundle)));
        }
        #[cfg(not(any(windows, target_os = "macos")))]
        {
            let entry: libloading::Symbol<extern "system" fn(*mut std::ffi::c_void) -> bool> =
                library.get(b"ModuleEntry\0").unwrap();
            assert!(entry(std::ptr::null_mut()));
        }
        library
    }
}

#[cfg(target_os = "macos")]
mod mac {
    use std::ffi::c_void;
    use std::path::Path;

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFURLCreateFromFileSystemRepresentation(
            allocator: *const c_void,
            buffer: *const u8,
            length: isize,
            is_directory: u8,
        ) -> *const c_void;
        fn CFBundleCreate(allocator: *const c_void, url: *const c_void) -> *mut c_void;
        fn CFRelease(cf: *const c_void);
    }

    /// Calls `f` with a `CFBundleRef` for `bundle`, as `bundleEntry` takes. The module retains it if
    /// it keeps it.
    pub fn with_bundle(bundle: &Path, f: impl FnOnce(*mut c_void) -> bool) -> bool {
        let bytes = bundle.as_os_str().as_encoded_bytes();
        unsafe {
            let url =
                CFURLCreateFromFileSystemRepresentation(std::ptr::null(), bytes.as_ptr(), bytes.len() as isize, 1);
            assert!(!url.is_null());
            let cf_bundle = CFBundleCreate(std::ptr::null(), url);
            assert!(!cf_bundle.is_null(), "no bundle at {}", bundle.display());
            let entered = f(cf_bundle);
            CFRelease(cf_bundle);
            CFRelease(url);
            entered
        }
    }
}
