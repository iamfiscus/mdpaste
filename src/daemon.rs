//! Long-running `mdpaste daemon` mode: watches for Ctrl+Alt+V with a Core
//! Graphics event tap (CGEventTap) and, on the V key *release*, converts
//! clipboard HTML to Markdown and re-pastes it in place.
//!
//! Design notes (per researched verdicts):
//! - The previous listener (Carbon RegisterEventHotKey via the global-hotkey
//!   crate) registered successfully but never delivered events to this
//!   bundle-less launchd-spawned process on macOS 15. A session-level
//!   CGEventTap does receive them (empirically confirmed with Hammerspoon's
//!   hs.eventtap, itself a CGEventTap, on this machine).
//! - A listen-only tap needs the same Accessibility grant the synthesized
//!   Cmd+V already requires, so trust is still enforced up front.
//! - The tap is created on the main thread and its mach port attached to the
//!   main run loop; CFRunLoopRun() there dispatches the callbacks. The heavy
//!   trigger path (clipboard read, 100ms settle, paste posting) runs on a
//!   worker thread so the tap callback never blocks.
//! - The tap ignores KeyDown and acts on KeyUp only: by release the physical
//!   modifiers are up, so they can't merge into the synthetic Cmd+V — and the
//!   tap being listen-only means our posted events pass straight through
//!   without re-triggering anything.

use std::ffi::c_void;
use std::io::Write as _;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use arboard::Clipboard;
use core_foundation::base::TCFType as _;
use core_foundation::runloop::{kCFRunLoopCommonModes, CFRunLoop};
use core_graphics::event::{
    CGEvent, CGEventFlags, CGEventTap, CGEventTapLocation, CGEventTapOptions,
    CGEventTapPlacement, CGEventTapProxy, CGEventType, EventField,
};
use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};

const KVK_ANSI_V: u16 = 0x09;
const PID_FILE_NAME: &str = ".mdpaste-daemon.pid";

// --- Single-instance guard ---------------------------------------------------
//
// Two simultaneous `mdpaste daemon` processes would both create an event tap
// and both post Cmd+V on one keypress — a double paste.
// Guard with a PID file checked for liveness via kill(pid, 0). Never remove
// the file on exit (signals would skip cleanup anyway); liveness is the guard.

/// Returns true if `pid` refers to a live process we may signal.
fn pid_is_alive(pid: u32) -> bool {
    pid > 0 && unsafe { libc::kill(pid as libc::pid_t, 0) } == 0
}

/// Decides whether the contents of the PID file describe a live, conflicting
/// daemon. Returns that daemon's PID, or None if the file is stale (dead PID
/// or unparseable content).
fn conflicting_pid(contents: &str) -> Option<u32> {
    let pid: u32 = contents.trim().parse().ok()?;
    pid_is_alive(pid).then_some(pid)
}

/// Refuses to start a second daemon if the PID file names a live process;
/// otherwise writes our own PID, silently overwriting any stale file.
fn enforce_single_instance() {
    let Some(home) = std::env::var_os("HOME") else {
        // Without HOME we can't persist anything; carry on (best effort).
        return;
    };
    let pid_path = std::path::PathBuf::from(home).join(PID_FILE_NAME);
    if let Ok(contents) = std::fs::read_to_string(&pid_path) {
        if let Some(pid) = conflicting_pid(&contents) {
            eprintln!("mdpaste daemon already running (pid {pid}). Refusing second instance.");
            std::process::exit(1);
        }
    }
    if let Err(e) = std::fs::write(&pid_path, format!("{}\n", std::process::id())) {
        eprintln!("mdpaste daemon: warning: could not write PID file: {e}");
    }
}

// --- Accessibility-trust FFI ------------------------------------------------
//
// Two verified landmines: CFDictionaryCreate needs the real
// kCFTypeDictionary{Key,Value}CallBacks (not NULL), and the option key must be
// the exported kAXTrustedCheckOptionPrompt constant — building a CFString
// whose *text* is the symbol name segfaults HIServices when untrusted.

type CFTypeRef = *const c_void;
type CFStringRef = *const c_void;
type CFDictionaryRef = *const c_void;
type CFBooleanRef = *const c_void;

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    static kCFBooleanTrue: CFBooleanRef;
    static kCFTypeDictionaryKeyCallBacks: c_void;
    static kCFTypeDictionaryValueCallBacks: c_void;
    fn CFDictionaryCreate(
        allocator: CFTypeRef,
        keys: *const CFTypeRef,
        values: *const CFTypeRef,
        num_values: isize,
        key_callbacks: *const c_void,
        value_callbacks: *const c_void,
    ) -> CFDictionaryRef;
}

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    static kAXTrustedCheckOptionPrompt: CFStringRef;
    fn AXIsProcessTrusted() -> bool;
    fn AXIsProcessTrustedWithOptions(options: CFDictionaryRef) -> bool;
}

#[link(name = "AppKit", kind = "framework")]
extern "C" {
    fn NSApplicationLoad() -> bool;
}

// The core-graphics crate wraps CGEventTapEnable behind CGEventTap::enable(),
// but the event-tap callback runs before/without access to that handle (the
// tap owns the callback). Re-declare the two symbols so the callback can
// re-enable a disabled tap through the mach-port ref stashed below.
#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGEventTapEnable(tap: *const c_void, enable: bool);
    fn CGEventTapIsEnabled(tap: *const c_void) -> bool;
}

/// Raw CFMachPort ref of the one and only event tap, stored right after
/// creation so the callback can re-enable the tap if the WindowServer
/// disables it. 0 until the tap exists. The tap outlives the process
/// (CFRunLoopRun never returns), so the pointer never dangles.
static TAP_MACH_PORT: AtomicUsize = AtomicUsize::new(0);

fn is_accessibility_trusted() -> bool {
    unsafe { AXIsProcessTrusted() }
}

/// Shows the system Accessibility prompt (best effort for a bundle-less
/// binary) and returns the current trust state.
fn prompt_for_accessibility() -> bool {
    unsafe {
        let keys = [kAXTrustedCheckOptionPrompt as CFTypeRef];
        let vals = [kCFBooleanTrue as CFTypeRef];
        let opts = CFDictionaryCreate(
            std::ptr::null(),
            keys.as_ptr(),
            vals.as_ptr(),
            1,
            &kCFTypeDictionaryKeyCallBacks as *const c_void,
            &kCFTypeDictionaryValueCallBacks as *const c_void,
        );
        AXIsProcessTrustedWithOptions(opts)
    }
}

/// The daemon's own process must be trusted (launchd makes it its own TCC
/// client). Prompts once, then polls until granted so the daemon simply
/// springs to life as soon as the user flips the switch.
fn ensure_accessibility_trusted() {
    if is_accessibility_trusted() {
        return;
    }
    eprintln!(
        "mdpaste daemon: Accessibility permission is required to simulate the paste keystroke.\n\
         Enable \"mdpaste\" under System Settings > Privacy & Security > Accessibility.\n\
         (Triggering the system prompt; if no dialog appears, add mdpaste there manually.)"
    );
    let _ = prompt_for_accessibility();
    let mut next_reminder = Instant::now() + Duration::from_secs(30);
    loop {
        if is_accessibility_trusted() {
            eprintln!("mdpaste daemon: Accessibility permission granted.");
            return;
        }
        if Instant::now() >= next_reminder {
            eprintln!("mdpaste daemon: still waiting for Accessibility permission...");
            next_reminder = Instant::now() + Duration::from_secs(30);
        }
        thread::sleep(Duration::from_secs(2));
    }
}

// --- Paste synthesis ---------------------------------------------------------

enum PasteMethod {
    /// In-process CGEvent posting at the HID tap (preferred; single binary
    /// grant covers it).
    CGEvent,
    /// Legacy osascript/System Events path, used only if CGEvent objects
    /// can't even be constructed at startup.
    Osascript,
}

impl PasteMethod {
    /// Probes CGEvent availability once. Note this only checks that sources
    /// and events can be *constructed* — actually posting requires the
    /// Accessibility trust already enforced above.
    fn choose() -> Self {
        let ok = CGEventSource::new(CGEventSourceStateID::HIDSystemState)
            .and_then(|src| CGEvent::new_keyboard_event(src, KVK_ANSI_V, true))
            .is_ok();
        if ok {
            Self::CGEvent
        } else {
            eprintln!("mdpaste daemon: CGEvent posting unavailable; falling back to osascript");
            Self::Osascript
        }
    }

    fn paste(&self) -> Result<(), String> {
        match self {
            Self::CGEvent => post_cmd_v(),
            Self::Osascript => crate::simulate_paste(),
        }
    }
}

/// Synthesizes Cmd+V (down, short pause, up) at the HID event tap.
fn post_cmd_v() -> Result<(), String> {
    let src = CGEventSource::new(CGEventSourceStateID::HIDSystemState)
        .map_err(|()| "failed to create CGEventSource".to_string())?;

    let down = CGEvent::new_keyboard_event(src.clone(), KVK_ANSI_V, true)
        .map_err(|()| "failed to create key-down CGEvent".to_string())?;
    down.set_flags(CGEventFlags::CGEventFlagCommand);
    down.post(CGEventTapLocation::HID);

    thread::sleep(Duration::from_millis(20)); // let the paste land

    let up = CGEvent::new_keyboard_event(src, KVK_ANSI_V, false)
        .map_err(|()| "failed to create key-up CGEvent".to_string())?;
    up.set_flags(CGEventFlags::CGEventFlagCommand);
    up.post(CGEventTapLocation::HID);
    Ok(())
}

// --- Trigger matching ----------------------------------------------------------

/// Decides whether a keyboard event is our trigger: keycode kVK_ANSI_V with
/// BOTH Control and Alternate (Option) held. Subset match against the
/// device-independent modifier bits, so harmless extras (caps-lock
/// AlphaShift, Fn, NumericPad, NonCoalesced) never block the trigger.
fn is_trigger_key(keycode: i64, flags: CGEventFlags) -> bool {
    let want = CGEventFlags::CGEventFlagControl | CGEventFlags::CGEventFlagAlternate;
    keycode == i64::from(KVK_ANSI_V) && flags & want == want
}

/// Event-tap callback. Runs on the main thread inside CFRunLoopRun; it must
/// stay cheap — it only matches and hands off to the worker over `tx`.
/// Listen-only tap: returning None always lets the event pass through.
fn tap_callback(
    _proxy: CGEventTapProxy,
    event_type: CGEventType,
    event: &CGEvent,
    tx: &mpsc::Sender<()>,
) -> Option<CGEvent> {
    match event_type {
        CGEventType::KeyUp => {
            let keycode = event.get_integer_value_field(EventField::KEYBOARD_EVENT_KEYCODE);
            if is_trigger_key(keycode, event.get_flags()) {
                // Loss of the receiver can only mean the worker died; either
                // way the hotkey simply does nothing.
                let _ = tx.send(());
            }
        }
        // The WindowServer disables taps on timeout or on user input at the
        // locked screen; re-enable through the stashed mach-port ref.
        CGEventType::TapDisabledByTimeout | CGEventType::TapDisabledByUserInput => {
            let port = TAP_MACH_PORT.load(Ordering::SeqCst);
            if port != 0 {
                unsafe { CGEventTapEnable(port as *const c_void, true) };
                eprintln!("mdpaste daemon: event tap was disabled; re-enabled");
            }
        }
        _ => {}
    }
    None
}

/// One trigger of the hotkey: clipboard HTML -> Markdown -> clipboard ->
/// paste. Any failure logs and returns; the daemon keeps listening.
fn handle_trigger(clipboard: &mut Clipboard, paste: &PasteMethod) {
    let html = match clipboard.get().html() {
        Ok(h) => h,
        Err(e) => {
            eprintln!("mdpaste daemon: no HTML on clipboard (copy rich text first): {e}");
            return;
        }
    };
    let markdown = crate::convert(&html);
    if let Err(e) = clipboard.set_text(&markdown) {
        eprintln!("mdpaste daemon: failed to write Markdown to clipboard: {e}");
        return;
    }
    // Let the clipboard write settle before the keystroke lands.
    thread::sleep(Duration::from_millis(100));
    if let Err(e) = paste.paste() {
        eprintln!("mdpaste daemon: paste failed: {e}");
    }
}

/// Runs the daemon forever (or exits the process on unrecoverable errors).
pub fn run() -> ! {
    enforce_single_instance();
    // A listen-only session tap silently delivers nothing without the AX
    // grant, so this gate must stay ahead of tap creation.
    ensure_accessibility_trusted();
    let paste = PasteMethod::choose();

    let (tx, rx) = mpsc::channel::<()>();

    // The tap is created on the main thread; its mach port attaches to the
    // main run loop just below, and CFRunLoopRun dispatches the callbacks.
    let tap = match CGEventTap::new(
        CGEventTapLocation::Session,
        CGEventTapPlacement::HeadInsertEventTap,
        CGEventTapOptions::ListenOnly,
        // KeyUp only; see module docs. KeyDown is deliberately absent.
        vec![CGEventType::KeyUp],
        move |proxy, event_type, event| tap_callback(proxy, event_type, event, &tx),
    ) {
        Ok(t) => t,
        Err(()) => {
            eprintln!(
                "mdpaste daemon: failed to create keyboard event tap.\n\
                 Listen-only taps require Accessibility trust; remove and re-grant \"mdpaste\"\n\
                 under System Settings > Privacy & Security > Accessibility, then restart."
            );
            std::process::exit(1);
        }
    };
    TAP_MACH_PORT.store(
        tap.mach_port.as_concrete_TypeRef() as usize,
        Ordering::SeqCst,
    );

    unsafe {
        let source = match tap.mach_port.create_runloop_source(0) {
            Ok(s) => s,
            Err(()) => {
                eprintln!("mdpaste daemon: failed to create event tap run loop source");
                std::process::exit(1);
            }
        };
        CFRunLoop::get_current().add_source(&source, kCFRunLoopCommonModes);
        tap.enable();
        if !CGEventTapIsEnabled(tap.mach_port.as_concrete_TypeRef() as *const c_void) {
            eprintln!("mdpaste daemon: keyboard event tap failed to enable");
            std::process::exit(1);
        }
    }

    // Only announce once the tap is confirmed enabled and attached.
    println!("mdpaste daemon listening: Ctrl+Alt+V");
    let _ = std::io::stdout().flush();

    // Worker thread owns the (non-Send) clipboard handle and runs the whole
    // trigger path; catch_unwind guarantees a bad paste never kills the loop.
    thread::spawn(move || {
        let mut clipboard = match Clipboard::new() {
            Ok(c) => c,
            Err(e) => {
                eprintln!("mdpaste daemon: failed to access clipboard: {e}");
                std::process::exit(1);
            }
        };
        // One unit per matched trigger; the tap callback never blocks on us.
        // The sender lives as long as the tap, so recv never returns Err.
        while rx.recv().is_ok() {
            let outcome =
                catch_unwind(AssertUnwindSafe(|| handle_trigger(&mut clipboard, &paste)));
            if outcome.is_err() {
                eprintln!("mdpaste daemon: conversion panicked; continuing");
            }
        }
    });

    // NSApplicationLoad keeps the bundle-less process good for AppKit
    // (arboard/NSPasteboard on the worker), then block the main thread on the
    // run loop that drives the event tap.
    unsafe {
        let _ = NSApplicationLoad();
        core_foundation::runloop::CFRunLoopRun();
    }
    unreachable!("CFRunLoopRun never returns");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trigger_match_is_ctrl_alt_v_only() {
        const V: i64 = 0x09;
        const CTRL: CGEventFlags = CGEventFlags::CGEventFlagControl;
        const ALT: CGEventFlags = CGEventFlags::CGEventFlagAlternate;

        // Exact match.
        assert!(is_trigger_key(V, CTRL | ALT));
        // Missing either modifier.
        assert!(!is_trigger_key(V, CTRL));
        assert!(!is_trigger_key(V, ALT));
        assert!(!is_trigger_key(V, CGEventFlags::empty()));
        // Harmless extra bits (caps-lock alpha-shift, Fn, numeric pad) must
        // not block the trigger.
        let extras = CGEventFlags::CGEventFlagAlphaShift
            | CGEventFlags::CGEventFlagSecondaryFn
            | CGEventFlags::CGEventFlagNumericPad;
        assert!(is_trigger_key(V, CTRL | ALT | extras));
        // Wrong keycode with the right modifiers.
        assert!(!is_trigger_key(0x08, CTRL | ALT)); // kVK_ANSI_C
    }

    #[test]
    fn pid_file_decision_distinguishes_live_from_stale() {
        // Our own PID is alive: a file containing it would refuse to start.
        let own = std::process::id();
        assert_eq!(conflicting_pid(&format!("{own}\n")), Some(own));

        // Unparseable and clearly-dead PIDs are stale: allowed to start.
        assert_eq!(conflicting_pid("not-a-pid"), None);
        assert_eq!(conflicting_pid(""), None);
        assert_eq!(conflicting_pid("0"), None); // pid 0 is never a daemon
        assert!(!pid_is_alive(0));
        // Search upward for a PID that doesn't exist on this system.
        let dead = (2_000_000u32..4_000_000)
            .find(|p| !pid_is_alive(*p))
            .expect("some PID space is empty");
        assert_eq!(conflicting_pid(&format!("{dead}")), None);
    }
}
