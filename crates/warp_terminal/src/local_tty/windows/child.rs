use std::ffi::c_void;

use mio::event::Source;
use mio::{Interest, Registry, Token};
use windows::Win32::Foundation::{HANDLE, INVALID_HANDLE_VALUE};
use windows::Win32::System::Threading::{
    INFINITE, RegisterWaitForSingleObject, UnregisterWaitEx, WT_EXECUTEINWAITTHREAD,
    WT_EXECUTEONLYONCE,
};

use crate::local_tty::mio_channel;
use crate::local_tty::windows::ShareableHandle;
use crate::writeable_pty::Message;

struct ChildExitSender {
    sender: mio_channel::Sender<Message>,
}

/// WinAPI callback to run when child process exits.
extern "system" fn child_exit_callback(ctx: *mut c_void, timed_out: bool) {
    let event_tx = unsafe { &*ctx.cast::<ChildExitSender>() };

    // This will not be hit by our current invocation strategy, as we
    // call RegisterWaitForSingleObject with both a timeout of INFINITE
    // and with the flag WT_EXECUTEONLYONCE. But it's still here in case
    // this ever gets refactored to break those guarantees.
    if timed_out {
        return;
    }

    event_tx.sender.send(Message::ChildExited).ok();
}

pub(super) struct ChildExitWatcher {
    wait_handle: ShareableHandle,
    _sender: Box<ChildExitSender>,
}

// Mark `ChildExitWatcher` as being safe to share between threads,
// even though `HANDLE` holds a `*mut c_void`, which isn't inherently
// safe to share.
unsafe impl Send for ChildExitWatcher {}
unsafe impl Sync for ChildExitWatcher {}

impl ChildExitWatcher {
    pub fn new(
        child_handle: HANDLE,
        event_loop_tx: mio_channel::Sender<Message>,
    ) -> windows::core::Result<ChildExitWatcher> {
        let mut wait_handle = HANDLE::default();
        let mut sender = Box::new(ChildExitSender {
            sender: event_loop_tx,
        });

        unsafe {
            RegisterWaitForSingleObject(
                &mut wait_handle,
                child_handle,
                Some(child_exit_callback),
                Some((&mut *sender as *mut ChildExitSender).cast()),
                INFINITE,
                WT_EXECUTEINWAITTHREAD | WT_EXECUTEONLYONCE,
            )?
        };

        Ok(ChildExitWatcher {
            wait_handle: ShareableHandle(wait_handle),
            _sender: sender,
        })
    }
}

impl Source for ChildExitWatcher {
    fn register(
        &mut self,
        _registry: &Registry,
        _token: Token,
        _interest: Interest,
    ) -> std::io::Result<()> {
        // Nothing to do.
        Ok(())
    }

    fn reregister(
        &mut self,
        _registry: &Registry,
        _token: Token,
        _interest: Interest,
    ) -> std::io::Result<()> {
        // Nothing to do.
        Ok(())
    }

    fn deregister(&mut self, _registry: &Registry) -> std::io::Result<()> {
        // Nothing to do.
        Ok(())
    }
}

impl Drop for ChildExitWatcher {
    fn drop(&mut self) {
        unsafe {
            // A replacement must not receive the previous process's delayed exit notification.
            let _ = UnregisterWaitEx(self.wait_handle.0, Some(INVALID_HANDLE_VALUE));
        }
    }
}

#[cfg(test)]
#[path = "child_tests.rs"]
mod tests;
