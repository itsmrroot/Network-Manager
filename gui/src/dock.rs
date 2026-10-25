//! The menu of the Dock icon. macOS asks the app's delegate for it when the
//! icon is clicked with the right button (or held down); the delegate that
//! winit makes has no such method, so it is added here, once, and answers with
//! the menu the menu bar code builds (`menu.rs`).

use std::ffi::c_void;
use std::sync::Once;
use std::sync::atomic::{AtomicPtr, Ordering};

use objc2::ffi::class_addMethod;
use objc2::runtime::{AnyClass, AnyObject, Imp, Sel};
use objc2::{class, msg_send, sel};

/// The NSMenu to show (owned by the menu bar code, which keeps it alive).
static MENU: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static INSTALLED: Once = Once::new();

/// `- (NSMenu *)applicationDockMenu:(NSApplication *)sender`
extern "C-unwind" fn dock_menu(_this: *mut AnyObject, _cmd: Sel, _sender: *mut AnyObject) -> *mut c_void {
    MENU.load(Ordering::Acquire)
}

/// Shows this menu from now on (and installs the method the first time).
pub fn set(menu: *mut c_void) {
    MENU.store(menu, Ordering::Release);
    INSTALLED.call_once(install);
}

fn app_delegate() -> Option<(*mut AnyObject, *mut AnyObject)> {
    // SAFETY: NSApplication is there, and its delegate is read on the main thread.
    unsafe {
        let app: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
        if app.is_null() {
            return None;
        }
        let delegate: *mut AnyObject = msg_send![app, delegate];
        (!delegate.is_null()).then_some((app, delegate))
    }
}

fn install() {
    let Some((_, delegate)) = app_delegate() else {
        log::warn!("dock menu: the app has no delegate yet");
        return;
    };
    // SAFETY: the method has the signature the Dock expects (id, SEL, id) -> id,
    // and is added to the delegate's class before the Dock asks.
    unsafe {
        let class = (*delegate).class();
        let imp: Imp = std::mem::transmute::<
            extern "C-unwind" fn(*mut AnyObject, Sel, *mut AnyObject) -> *mut c_void,
            Imp,
        >(dock_menu);
        let added = class_addMethod(
            class as *const AnyClass as *mut AnyClass,
            sel!(applicationDockMenu:),
            imp,
            c"@@:@".as_ptr(),
        );
        log::debug!("dock menu: method added: {}", added.as_bool());
    }
}

/// Development aid: what the Dock would be given: the number of items of the
/// menu the delegate returns (or why there is none).
#[cfg(debug_assertions)]
pub fn describe() -> String {
    let Some((app, delegate)) = app_delegate() else { return "no delegate".into() };
    // SAFETY: asks the delegate the same question the Dock asks.
    unsafe {
        let class = (*delegate).class();
        if !class.responds_to(sel!(applicationDockMenu:)) {
            return "the delegate does not answer applicationDockMenu:".into();
        }
        let menu: *mut AnyObject = msg_send![delegate, applicationDockMenu: app];
        if menu.is_null() {
            return "the delegate answers with no menu".into();
        }
        let count: isize = msg_send![menu, numberOfItems];
        let mut titles = Vec::new();
        for i in 0..count {
            let item: *mut AnyObject = msg_send![menu, itemAtIndex: i];
            let title: *mut AnyObject = msg_send![item, title];
            let text: *const std::ffi::c_char = msg_send![title, UTF8String];
            let separator: bool = msg_send![item, isSeparatorItem];
            titles.push(if separator {
                "-".to_string()
            } else {
                std::ffi::CStr::from_ptr(text).to_string_lossy().into_owned()
            });
        }
        format!("{count} items: {}", titles.join(" | "))
    }
}
