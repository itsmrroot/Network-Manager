//! The menu of the Dock icon. macOS asks the app's delegate for it when the
//! icon is clicked with the right button (or held down); the delegate that
//! winit makes has no such method, so it is added here, once, and answers with
//! the menu the menu bar code builds (`menu.rs`).

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicPtr, Ordering};

use objc2::ffi::class_addMethod;
use objc2::runtime::{AnyClass, AnyObject, Bool, Imp, Sel};
use objc2::{class, msg_send, sel};

/// The NSMenu to show (owned by the menu bar code, which keeps it alive).
static MENU: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
/// The method is on the delegate (tried again on the next call until it is).
static INSTALLED: AtomicBool = AtomicBool::new(false);

/// `- (NSMenu *)applicationDockMenu:(NSApplication *)sender`
extern "C-unwind" fn dock_menu(_this: *mut AnyObject, _cmd: Sel, _sender: *mut AnyObject) -> *mut c_void {
    MENU.load(Ordering::Acquire)
}

/// Shows this menu from now on (and installs the method the first time).
/// A null pointer: no menu (the owner is gone).
pub fn set(menu: *mut c_void) {
    MENU.store(menu, Ordering::Release);
    if !menu.is_null() && !INSTALLED.load(Ordering::Acquire) && install() {
        INSTALLED.store(true, Ordering::Release);
    }
}

/// Makes every picture in a menu (and its submenus) a template image: macOS
/// then draws it black in a light menu, white in a dark one, and dimmed when
/// the item is off, whatever colour the bitmap has (only its alpha is used).
pub fn template_icons(menu: *mut c_void) {
    unsafe fn walk(menu: *mut AnyObject, depth: u32) {
        if menu.is_null() || depth > 4 {
            return;
        }
        // SAFETY: plain reads of the menu's items, and a flag on their images.
        unsafe {
            let count: isize = msg_send![menu, numberOfItems];
            for i in 0..count {
                let item: *mut AnyObject = msg_send![menu, itemAtIndex: i];
                if item.is_null() {
                    continue;
                }
                let image: *mut AnyObject = msg_send![item, image];
                if !image.is_null() {
                    let _: () = msg_send![image, setTemplate: Bool::YES];
                }
                let sub: *mut AnyObject = msg_send![item, submenu];
                walk(sub, depth + 1);
            }
        }
    }
    // SAFETY: the pointer is an NSMenu the caller owns; called on the main thread.
    unsafe { walk(menu.cast(), 0) }
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

fn install() -> bool {
    let Some((_, delegate)) = app_delegate() else {
        log::warn!("dock menu: the app has no delegate yet");
        return false;
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
        added.as_bool()
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
            let mut line =
                if separator { "-".to_string() } else { std::ffi::CStr::from_ptr(text).to_string_lossy().into_owned() };
            let sub: *mut AnyObject = msg_send![item, submenu];
            if !sub.is_null() {
                let n: isize = msg_send![sub, numberOfItems];
                let inner: Vec<String> = (0..n)
                    .map(|k| {
                        let it: *mut AnyObject = msg_send![sub, itemAtIndex: k];
                        let t: *mut AnyObject = msg_send![it, title];
                        let c: *const std::ffi::c_char = msg_send![t, UTF8String];
                        std::ffi::CStr::from_ptr(c).to_string_lossy().into_owned()
                    })
                    .collect();
                line = format!("{line} [{}]", inner.join(" | "));
            }
            titles.push(line);
        }
        format!("{count} items: {}", titles.join(" | "))
    }
}

/// Development aid: the menu bar as macOS has it: each menu with its items,
/// and `*` after an item that has a picture.
#[cfg(debug_assertions)]
pub fn describe_menu_bar() -> String {
    // SAFETY: reads titles and pictures of the main menu on the main thread.
    unsafe fn title_of(item: *mut AnyObject) -> String {
        unsafe {
            let separator: bool = msg_send![item, isSeparatorItem];
            if separator {
                return "-".into();
            }
            let title: *mut AnyObject = msg_send![item, title];
            let text: *const std::ffi::c_char = msg_send![title, UTF8String];
            let image: *mut AnyObject = msg_send![item, image];
            let name = std::ffi::CStr::from_ptr(text).to_string_lossy().into_owned();
            if image.is_null() {
                name
            } else {
                // * a picture the system tints (a template), + one with its own colour.
                let template: bool = msg_send![image, isTemplate];
                format!("{name}{}", if template { "*" } else { "+" })
            }
        }
    }
    let Some((app, _)) = app_delegate() else { return "no app".into() };
    unsafe {
        let main: *mut AnyObject = msg_send![app, mainMenu];
        if main.is_null() {
            return "no main menu".into();
        }
        let count: isize = msg_send![main, numberOfItems];
        let mut out = Vec::new();
        for i in 0..count {
            let top: *mut AnyObject = msg_send![main, itemAtIndex: i];
            let sub: *mut AnyObject = msg_send![top, submenu];
            let mut line = title_of(top);
            if !sub.is_null() {
                let n: isize = msg_send![sub, numberOfItems];
                let items: Vec<String> = (0..n)
                    .map(|k| {
                        let it: *mut AnyObject = msg_send![sub, itemAtIndex: k];
                        title_of(it)
                    })
                    .collect();
                line = format!("{line}: {}", items.join(" | "));
            }
            out.push(line);
        }
        out.join("\n")
    }
}

/// Shows the app again when a Dock menu item is chosen while it is hidden or
/// minimised: such a window draws nothing, so the command would wait until the
/// user clicks the Dock icon.
pub fn wake() {
    unsafe {
        let app: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
        if app.is_null() {
            return;
        }
        let _: () = msg_send![app, unhide: std::ptr::null::<AnyObject>()];
        let _: () = msg_send![app, activateIgnoringOtherApps: Bool::YES];
        let windows: *mut AnyObject = msg_send![app, windows];
        let count: usize = msg_send![windows, count];
        for i in 0..count {
            let window: *mut AnyObject = msg_send![windows, objectAtIndex: i];
            let minimised: Bool = msg_send![window, isMiniaturized];
            if minimised.as_bool() {
                let _: () = msg_send![window, deminiaturize: std::ptr::null::<AnyObject>()];
            }
        }
    }
}
