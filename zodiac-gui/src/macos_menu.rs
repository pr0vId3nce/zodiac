//! The native macOS menu bar.
//!
//! winit has no menu support, so without this the app has no menu bar at
//! all and ⌘Q / ⌘H / ⌘M simply do nothing — the clearest remaining sign
//! that the window was not made by a Mac app.
//!
//! Everything here uses AppKit's *standard* selectors (`terminate:`,
//! `hide:`, `performMiniaturize:`, …), which the responder chain
//! implements already. That means no menu-event plumbing back into the
//! winit loop, and the items behave exactly as their counterparts in every
//! other Mac app, including while a modal or a different window is up.
//!
//! ## Why this menu is deliberately short
//!
//! A menu item's key equivalent is claimed by AppKit *before* the key
//! event ever reaches the window, so adding a familiar-looking item can
//! silently break the app. zodiac already binds ⌘N (new agent), ⌘W (close
//! **pane**), ⌘1-9 (jump to pane), ⌘K (palette), ⌘, (settings) and ⌘V /
//! ⌘⇧C (clipboard). Every one of those is therefore absent here:
//!
//! - No Edit menu. The standard Cut/Copy/Paste items would take ⌘X/⌘C/⌘V
//!   away from the terminal view, which handles them itself — and their
//!   selectors would go to winit's view, which does not implement them, so
//!   the items would be dead *and* the app's own paste would stop working.
//! - Close Window is ⌘⇧W, not ⌘W. This is Terminal.app's convention: ⌘W
//!   closes the tab (here, the pane) and ⌘⇧W closes the window. Claiming
//!   ⌘W for the window would make it impossible to close a pane.

use objc2::rc::Retained;
// MainThreadOnly is what provides `alloc(mtm)` for these AppKit classes.
use objc2::{sel, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{NSApplication, NSEventModifierFlags, NSMenu, NSMenuItem};
use objc2_foundation::NSString;

/// Build and install the menu bar. Call once, on the main thread, after
/// the event loop has created `NSApplication`. A no-op off the main
/// thread rather than a panic — a missing menu bar is not worth aborting
/// a working session over.
pub fn install() {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let app = NSApplication::sharedApplication(mtm);
    let menubar = NSMenu::new(mtm);

    // ---- application menu ------------------------------------------
    // macOS always titles this one after the bundle, ignoring whatever we
    // set, so the submenu's own title is irrelevant.
    let app_menu = NSMenu::new(mtm);
    item(mtm, &app_menu, "About zodiac", sel!(orderFrontStandardAboutPanel:), "");
    app_menu.addItem(&NSMenuItem::separatorItem(mtm));
    item(mtm, &app_menu, "Hide zodiac", sel!(hide:), "h");
    let hide_others = item(mtm, &app_menu, "Hide Others", sel!(hideOtherApplications:), "h");
    hide_others.setKeyEquivalentModifierMask(
        NSEventModifierFlags::Command | NSEventModifierFlags::Option,
    );
    item(mtm, &app_menu, "Show All", sel!(unhideAllApplications:), "");
    app_menu.addItem(&NSMenuItem::separatorItem(mtm));
    // terminate: rather than zodiac's own detach-and-exit: the session is
    // server-owned, so a client going away is a detach either way and the
    // agents keep running. Routing this back into the winit loop would
    // need a custom target class for no behavioural gain.
    item(mtm, &app_menu, "Quit zodiac", sel!(terminate:), "q");
    let app_item = NSMenuItem::new(mtm);
    app_item.setSubmenu(Some(&app_menu));
    menubar.addItem(&app_item);

    // ---- window menu -----------------------------------------------
    let win_menu = NSMenu::initWithTitle(NSMenu::alloc(mtm), &NSString::from_str("Window"));
    item(mtm, &win_menu, "Minimize", sel!(performMiniaturize:), "m");
    item(mtm, &win_menu, "Zoom", sel!(performZoom:), "");
    win_menu.addItem(&NSMenuItem::separatorItem(mtm));
    let close = item(mtm, &win_menu, "Close Window", sel!(performClose:), "w");
    close.setKeyEquivalentModifierMask(
        NSEventModifierFlags::Command | NSEventModifierFlags::Shift,
    );
    win_menu.addItem(&NSMenuItem::separatorItem(mtm));
    item(mtm, &win_menu, "Bring All to Front", sel!(arrangeInFront:), "");
    let win_item = NSMenuItem::new(mtm);
    win_item.setSubmenu(Some(&win_menu));
    menubar.addItem(&win_item);

    app.setMainMenu(Some(&menubar));
    // Hands the Window menu to AppKit so it maintains the window list and
    // the checkmark on the front window by itself.
    app.setWindowsMenu(Some(&win_menu));
}

/// Append one item bound to a standard responder-chain selector. An empty
/// `key` means no key equivalent.
fn item(
    mtm: MainThreadMarker,
    menu: &NSMenu,
    title: &str,
    action: objc2::runtime::Sel,
    key: &str,
) -> Retained<NSMenuItem> {
    // SAFETY: every selector passed here is a standard AppKit action that
    // takes a single `id` sender, which is the signature NSMenuItem
    // invokes them with.
    let it = unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mtm),
            &NSString::from_str(title),
            Some(action),
            &NSString::from_str(key),
        )
    };
    menu.addItem(&it);
    it
}
