//! OS bridge. Call initialization and Dock updates on the UI thread. All other
//! results are channels so authorization/dispatch never block the UI thread.

use std::sync::mpsc::{self, Receiver};

use super::delivery::{Permission, Target};

pub struct Notification {
    #[allow(dead_code)]
    pub tag: String,
    #[allow(dead_code)]
    pub subtitle: String,
    #[allow(dead_code)]
    pub body: String,
    #[allow(dead_code)]
    pub sound: bool,
}

pub fn initialize() -> bool {
    false
}

pub fn permission() -> Receiver<Permission> {
    resolved(Permission::Unsupported)
}

pub fn request_permission() -> Receiver<Permission> {
    permission()
}

pub fn show(_: Notification) -> Receiver<Result<(), String>> {
    resolved(Err(
        "Native notifications are unavailable on this platform".into()
    ))
}

pub fn drain_clicks() -> Vec<Target> {
    Vec::new()
}

pub fn set_dock_badge(_: usize) {}

pub fn dismiss(_: &str) {}

fn resolved<T>(value: T) -> Receiver<T> {
    let (sender, receiver) = mpsc::channel();
    let _ = sender.send(value);
    receiver
}
