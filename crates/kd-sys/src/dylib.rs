use std::ffi::CString;
use std::os::raw::c_void;

/// A lazily opened private framework, kept open for the process lifetime.
///
/// Private symbols are resolved at runtime rather than linked, so a symbol that
/// disappears in a macOS update degrades one capability instead of preventing
/// the app from launching.
pub struct Framework {
    handle: *mut c_void,
}

// The handle is only ever passed to `dlsym`, which is thread-safe.
unsafe impl Send for Framework {}
unsafe impl Sync for Framework {}

impl Framework {
    pub fn open(path: &str) -> Option<Self> {
        let c_path = CString::new(path).ok()?;
        let handle = unsafe { libc::dlopen(c_path.as_ptr(), libc::RTLD_LAZY) };
        if handle.is_null() {
            None
        } else {
            Some(Self { handle })
        }
    }

    /// Resolves `name` and transmutes it to `F`.
    ///
    /// # Safety
    /// `F` must be a function pointer type matching the symbol's real signature.
    pub unsafe fn symbol<F: Copy>(&self, name: &str) -> Option<F> {
        debug_assert_eq!(
            std::mem::size_of::<F>(),
            std::mem::size_of::<*const c_void>()
        );
        let c_name = CString::new(name).ok()?;
        let sym = unsafe { libc::dlsym(self.handle, c_name.as_ptr()) };
        if sym.is_null() {
            None
        } else {
            Some(unsafe { *(&sym as *const *mut c_void as *const F) })
        }
    }
}
