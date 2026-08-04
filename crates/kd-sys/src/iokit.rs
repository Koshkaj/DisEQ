//! Minimal IOKit bindings — only what display discovery and DDC need.

use std::ffi::{c_char, c_void, CString};

use objc2_core_foundation::{CFDictionary, CFRetained, CFString, CFType};

pub type IOReturn = i32;
pub type IoObject = u32;
pub type IoIterator = u32;
pub type IoService = u32;

pub const KERN_SUCCESS: i32 = 0;
pub const IO_OBJECT_NULL: IoObject = 0;

pub const K_IO_REGISTRY_ITERATE_RECURSIVELY: u32 = 1;
pub const K_IO_REGISTRY_ITERATE_PARENTS: u32 = 2;

#[link(name = "IOKit", kind = "framework")]
extern "C" {
    static kIOMainPortDefault: u32;

    fn IOServiceMatching(name: *const c_char) -> *mut c_void;
    fn IOServiceGetMatchingServices(
        main_port: u32,
        matching: *mut c_void,
        existing: *mut IoIterator,
    ) -> i32;
    fn IOIteratorNext(iterator: IoIterator) -> IoObject;
    fn IOObjectRelease(object: IoObject) -> i32;
    fn IORegistryEntrySearchCFProperty(
        entry: IoService,
        plane: *const c_char,
        key: *const CFString,
        allocator: *const c_void,
        options: u32,
    ) -> *const CFType;
    fn IORegistryEntryGetParentEntry(
        entry: IoService,
        plane: *const c_char,
        parent: *mut IoService,
    ) -> i32;
    fn IOObjectRetain(object: IoObject) -> i32;
}

/// Iterates every IORegistry service matching `class_name`.
///
/// The caller receives each service while it is still alive; releasing is
/// handled here so callers cannot leak ports.
pub fn for_each_service(class_name: &str, mut f: impl FnMut(IoService) -> bool) {
    let Ok(name) = CString::new(class_name) else {
        return;
    };

    let matching = unsafe { IOServiceMatching(name.as_ptr()) };
    if matching.is_null() {
        return;
    }

    let mut iterator: IoIterator = 0;
    // IOServiceGetMatchingServices consumes the matching dictionary reference.
    let result =
        unsafe { IOServiceGetMatchingServices(kIOMainPortDefault, matching, &mut iterator) };
    if result != KERN_SUCCESS {
        return;
    }

    loop {
        let service = unsafe { IOIteratorNext(iterator) };
        if service == IO_OBJECT_NULL {
            break;
        }
        let keep_going = f(service);
        unsafe { IOObjectRelease(service) };
        if !keep_going {
            break;
        }
    }
    unsafe { IOObjectRelease(iterator) };
}

/// Looks up `key` on `service`, walking up the service plane.
pub fn search_parent_property(service: IoService, key: &str) -> Option<CFRetained<CFType>> {
    search_property(
        service,
        key,
        K_IO_REGISTRY_ITERATE_RECURSIVELY | K_IO_REGISTRY_ITERATE_PARENTS,
    )
}

/// Looks up `key` on `service` or any of its descendants.
pub fn search_child_property(service: IoService, key: &str) -> Option<CFRetained<CFType>> {
    search_property(service, key, K_IO_REGISTRY_ITERATE_RECURSIVELY)
}

fn search_property(service: IoService, key: &str, options: u32) -> Option<CFRetained<CFType>> {
    let plane = CString::new("IOService").ok()?;
    let key = CFString::from_str(key);

    let value = unsafe {
        IORegistryEntrySearchCFProperty(service, plane.as_ptr(), &*key, std::ptr::null(), options)
    };
    if value.is_null() {
        return None;
    }
    // IORegistryEntrySearchCFProperty follows the Create rule.
    Some(unsafe { CFRetained::from_raw(std::ptr::NonNull::new(value.cast_mut())?) })
}

/// Walks up from `service`, offering each ancestor to `f` until it returns a value.
///
/// Display metadata does not live on the node that carries the AV service: on
/// Apple Silicon `DisplayAttributes` sits on a sibling `AppleCLCD2` under a
/// shared `dispN` ancestor. Climbing one level at a time and searching that
/// ancestor's descendants finds the *nearest* match, which is the one belonging
/// to this display rather than another panel's.
pub fn find_in_ancestry<T>(
    service: IoService,
    max_levels: usize,
    mut f: impl FnMut(IoService) -> Option<T>,
) -> Option<T> {
    let plane = CString::new("IOService").ok()?;
    let mut current = service;
    unsafe { IOObjectRetain(current) };

    let mut result = f(current);
    for _ in 0..max_levels {
        if result.is_some() {
            break;
        }
        let mut parent: IoService = 0;
        let ok = unsafe {
            IORegistryEntryGetParentEntry(current, plane.as_ptr(), &mut parent) == KERN_SUCCESS
        };
        unsafe { IOObjectRelease(current) };
        if !ok || parent == IO_OBJECT_NULL {
            return None;
        }
        current = parent;
        result = f(current);
    }
    unsafe { IOObjectRelease(current) };
    result
}

/// IORegistry dictionaries are always string-keyed, so they are handed back
/// typed — the untyped `CFDictionary` cannot be indexed.
pub type PropertyDictionary = CFDictionary<CFString, CFType>;

pub fn property_dictionary(
    service: IoService,
    key: &str,
) -> Option<CFRetained<PropertyDictionary>> {
    let value = search_parent_property(service, key)?;
    let dictionary = value.downcast::<CFDictionary>().ok()?;
    Some(unsafe { CFRetained::cast_unchecked(dictionary) })
}

pub fn property_string(service: IoService, key: &str) -> Option<String> {
    let value = search_parent_property(service, key)?;
    let string = value.downcast::<CFString>().ok()?;
    Some(string.to_string())
}
