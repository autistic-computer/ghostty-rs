//! Port of `src/apprt/gtk/desktop_notifications.zig`.
//!
//! STATUS: ported
//!
//! Sends and clears desktop notifications associated with a single GTK surface.
//!
//! Zig's timeout callbacks get a `*DesktopNotifications` as user data. The
//! port keeps the tracked list in an `Rc<RefCell<..>>` that the timeout
//! closures reference weakly. `Application.default()` is
//! [`Application::try_default`]: without a default Ghostty application
//! (e.g. in unit tests) sending and withdrawing are skipped instead of
//! asserting like Zig. Allocator
//! parameters are dropped; tracking cannot fail, so the Zig test
//! "desktop notifications allocation failure preserves entries"
//! (`checkAllAllocationFailures`) has no counterpart.
#![cfg(feature = "gtk")]

use std::cell::RefCell;
use std::rc::{Rc, Weak};

use gio::prelude::*;

use crate::apprt::gtk::class::application::Application;
use crate::apprt::gtk::ext::source_remove;
use crate::terminal::hash_map::Wyhash;

const LOG: &str = "gtk_desktop_notifications";

const DEFAULT_LIMIT: usize = 64;
// Match macOS: notifications sent while focused expire after three seconds.
const FOCUSED_TIMEOUT_MS: u32 = 3 * 1000;

#[derive(Debug)]
pub struct Notification {
    pub hash: u64,
    timeout_source: Option<glib::SourceId>,
}

impl Notification {
    fn new(hash: u64) -> Notification {
        Notification {
            hash,
            timeout_source: None,
        }
    }
}

#[derive(Debug)]
pub struct DesktopNotifications {
    /// Tracked notification hashes. Replacements are appended as new entries.
    pub notifications: Rc<RefCell<Vec<Notification>>>,
    /// Set when the core surface initializes and retained through cleanup.
    pub surface_id: u64,
    /// Bound retained entries because GIO does not report desktop-side dismissals.
    pub limit: usize,
}

impl Default for DesktopNotifications {
    fn default() -> Self {
        DesktopNotifications {
            notifications: Rc::default(),
            surface_id: 0,
            limit: DEFAULT_LIMIT,
        }
    }
}

fn application() -> Option<gio::Application> {
    Application::try_default().map(|app| app.upcast::<gio::Application>())
}

impl DesktopNotifications {
    pub fn init(surface_id: u64) -> DesktopNotifications {
        assert!(surface_id != 0);
        DesktopNotifications {
            surface_id,
            ..Default::default()
        }
    }

    /// Send or replace a notification for this surface. The caller may release
    /// title and body on return.
    pub fn send(&mut self, focused: bool, title: &str, body: &str) {
        assert!(self.surface_id != 0);
        let Some(app) = application() else {
            log::warn!(target: LOG, "no application to send desktop notification");
            return;
        };

        let display_title = if title.is_empty() { "Ghostty" } else { title };
        let notification = gio::Notification::new(display_title);
        notification.set_body(Some(body));

        let icon = gio::ThemedIcon::new("com.mitchellh.ghostty");
        notification.set_icon(&icon);
        notification.set_default_action_and_target_value(
            "app.present-surface",
            Some(&self.surface_id.to_variant()),
        );

        let hash = notification_hash(title.as_bytes(), body.as_bytes());
        let evicted = self.track(hash);
        if let Some(removed) = evicted {
            self.clear_removed(removed);
        }
        let tracked = true;

        let id = format_id(self.surface_id, hash);
        app.send_notification(Some(&id), &notification);

        if focused && tracked {
            self.schedule_timeout(hash);
        }
    }

    /// Withdraw delivered notifications and cancel their timers. GIO's freedesktop
    /// backend cannot withdraw a notification until its asynchronous Notify reply
    /// arrives. A notification still in flight may therefore outlive this call.
    pub fn clear(&mut self) {
        let removed: Vec<Notification> = std::mem::take(&mut *self.notifications.borrow_mut());
        for removed in removed {
            self.clear_removed(removed);
        }
    }

    /// Store a hash and return any evicted entry for withdrawal by the caller.
    fn track(&mut self, hash: u64) -> Option<Notification> {
        assert!(self.limit > 0);
        if let Some(removed) = self.remove(hash) {
            // Move the entry to the newest position and reset its timer.
            cancel_timeout(removed.timeout_source);
            self.notifications.borrow_mut().push(Notification::new(hash));
            return None;
        }

        let mut notifications = self.notifications.borrow_mut();
        let mut evicted = None;
        if notifications.len() >= self.limit {
            evicted = Some(notifications.remove(0));
        }

        notifications.push(Notification::new(hash));
        evicted
    }

    fn find(&self, hash: u64) -> Option<usize> {
        self.notifications.borrow().iter().position(|n| n.hash == hash)
    }

    /// Remove a matching entry without preserving the order of the remaining items.
    fn remove(&mut self, hash: u64) -> Option<Notification> {
        remove_from(&self.notifications, hash)
    }

    /// Cancel the removed entry's timer and withdraw it.
    fn clear_removed(&self, removed: Notification) {
        cancel_timeout(removed.timeout_source);
        withdraw(self.surface_id, removed.hash);
    }

    fn schedule_timeout(&mut self, hash: u64) {
        let Some(index) = self.find(hash) else { return };
        debug_assert!(self.notifications.borrow()[index].timeout_source.is_none());

        let weak: Weak<RefCell<Vec<Notification>>> = Rc::downgrade(&self.notifications);
        let surface_id = self.surface_id;
        let source = glib::timeout_add_local_full(
            std::time::Duration::from_millis(FOCUSED_TIMEOUT_MS as u64),
            glib::Priority::DEFAULT,
            move || {
                timeout_callback(&weak, surface_id, hash);
                glib::ControlFlow::Break
            },
        );
        self.notifications.borrow_mut()[index].timeout_source = Some(source);
    }

    /// Release notifications, timers, and storage. Safe after clear or deinit.
    pub fn deinit(&mut self) {
        self.clear();
    }
}

fn remove_from(notifications: &RefCell<Vec<Notification>>, hash: u64) -> Option<Notification> {
    let mut notifications = notifications.borrow_mut();
    let i = notifications.iter().position(|n| n.hash == hash)?;
    Some(notifications.swap_remove(i))
}

fn withdraw(surface_id: u64, hash: u64) {
    let id = format_id(surface_id, hash);
    if let Some(app) = application() {
        app.withdraw_notification(&id);
    }
}

fn timeout_callback(notifications: &Weak<RefCell<Vec<Notification>>>, surface_id: u64, hash: u64) {
    let Some(notifications) = notifications.upgrade() else { return };
    let Some(removed) = remove_from(&notifications, hash) else { return };
    debug_assert!(removed.timeout_source.is_some());
    // This source is already dispatching. Let SOURCE_REMOVE destroy its callback
    // data after return instead of cancelling it through clearRemoved.
    std::mem::forget(removed.timeout_source);
    withdraw(surface_id, removed.hash);
}

fn cancel_timeout(source: Option<glib::SourceId>) {
    let Some(source) = source else { return };
    if !source_remove(source) {
        log::warn!(target: LOG, "unable to remove desktop notification timer");
    }
}

fn notification_hash(title: &[u8], body: &[u8]) -> u64 {
    let mut hasher = Wyhash::init(0);
    hasher.update(title);
    hasher.update(body);
    hasher.final_()
}

/// GIO requires the same ID for sending and withdrawing. Including the surface
/// ID keeps identical notifications from different surfaces independent.
/// The fixed buffer fits the prefix, two 16-digit hex values, and a terminator.
fn format_id(surface_id: u64, hash: u64) -> String {
    format!("ghostty-surface-{surface_id:x}-{hash:x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hashes(n: &DesktopNotifications) -> Vec<u64> {
        n.notifications.borrow().iter().map(|n| n.hash).collect()
    }

    #[test]
    fn desktop_notification_ids_are_stable_per_surface_and_content() {
        let first = notification_hash(b"Title", b"Body");
        let repeated = notification_hash(b"Title", b"Body");
        let other_title = notification_hash(b"Other", b"Body");
        let other_body = notification_hash(b"Title", b"Other");

        let surface_id = u64::MAX;
        let first_id = format_id(surface_id, first);
        let repeated_id = format_id(surface_id, repeated);
        let other_surface_id = format_id(2, first);
        let other_title_id = format_id(surface_id, other_title);
        let other_body_id = format_id(surface_id, other_body);

        assert_eq!(first_id, repeated_id);
        assert_ne!(first_id, other_surface_id);
        assert_ne!(first_id, other_title_id);
        assert_ne!(first_id, other_body_id);
        assert!(first_id.len() < 64);

        // Title and body are hashed as concatenated bytes.
        let a = format_id(surface_id, notification_hash(b"ab", b"c"));
        let b = format_id(surface_id, notification_hash(b"a", b"bc"));
        assert_eq!(a, b);
    }

    #[test]
    fn desktop_notifications_replace_and_evict_first_entry() {
        let mut notifications = DesktopNotifications {
            limit: 3,
            ..Default::default()
        };

        let _ = notifications.track(1);
        let _ = notifications.track(2);
        let _ = notifications.track(3);

        assert!(notifications.track(1).is_none());

        let evicted = notifications.track(4).unwrap();
        assert_eq!(3, evicted.hash);
        assert_eq!(3, notifications.notifications.borrow().len());
    }

    #[test]
    fn desktop_notifications_remove_and_drain_independently() {
        let mut first = DesktopNotifications::init(1);
        let mut second = DesktopNotifications::init(2);

        let _ = first.track(1);
        let _ = first.track(2);
        let _ = second.track(1);

        let removed = first.remove(1).unwrap();
        assert_eq!(1, removed.hash);
        assert_eq!(1, first.notifications.borrow().len());
        assert_eq!(1, second.notifications.borrow().len());

        let drained = first.remove(2).unwrap();
        assert_eq!(2, drained.hash);
        assert_eq!(0, first.notifications.borrow().len());
        assert!(first.remove(2).is_none());
        assert_eq!(1, second.notifications.borrow().len());
    }

    #[test]
    fn desktop_notification_removal_swaps_in_the_last_entry() {
        let mut notifications = DesktopNotifications {
            limit: 3,
            ..Default::default()
        };
        notifications.notifications.borrow_mut().reserve_exact(3);
        for hash in [1u64, 2, 3, 4] {
            let _ = notifications.track(hash);
        }
        assert!(notifications.track(3).is_none());
        assert_eq!(hashes(&notifications), vec![2, 4, 3]);
        assert!(notifications.remove(5).is_none());
        let _ = notifications.remove(4).unwrap();
        assert_eq!(hashes(&notifications), vec![2, 3]);
    }

    #[test]
    fn desktop_notifications_cleanup_releases_storage_and_is_repeatable() {
        let mut notifications = DesktopNotifications::default();
        notifications.clear();
        assert_eq!(0, notifications.notifications.borrow().capacity());

        notifications = DesktopNotifications::init(42);
        let _ = notifications.track(1);
        let _ = notifications.remove(1).unwrap();
        assert_eq!(0, notifications.notifications.borrow().len());
        assert!(notifications.notifications.borrow().capacity() > 0);

        notifications.clear();
        assert_eq!(0, notifications.notifications.borrow().capacity());
        notifications.deinit();
        notifications.deinit();
        assert_eq!(0, notifications.notifications.borrow().len());
        assert_eq!(0, notifications.notifications.borrow().capacity());
        assert_eq!(42, notifications.surface_id);
    }
}
