//! Test-only per-worker observation; changes no execution or terminal policy.
//! Install on the injecting worker, acknowledge only at error conversion.
use std::{cell::RefCell, sync::mpsc::Sender};

thread_local! {
    static OBSERVER: RefCell<Option<Sender<()>>> = const { RefCell::new(None) };
}

pub(crate) fn install(sender: Sender<()>) {
    OBSERVER.with(|observer| assert!(observer.replace(Some(sender)).is_none()));
}

pub(crate) fn converted() {
    OBSERVER.with(|observer| {
        if let Some(sender) = observer.take() {
            // A disconnected test receiver must not alter worker execution.
            let _ = sender.send(());
        }
    });
}
