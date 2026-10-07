// Copyright (C) 2026 leeymxz
// SPDX-License-Identifier: GPL-3.0-or-later

//! Native pickers that neither freeze the interface nor get lost behind it.
//!
//! A bare `rfd::FileDialog::new().pick_folder()` written inside `update` gets
//! two things wrong on Windows:
//!
//! * **Z order.** With no parent set, rfd shows the dialog with a NULL owner
//!   (`IFileDialog::Show(NULL)`), while every PlayDL dialog window is made an
//!   owned window of the main one through `GWLP_HWNDPARENT` (see
//!   [`crate::dialog_parent`]). An owned window is always drawn above its
//!   owner, so an ownerless picker can end up below both — it opens, nothing
//!   is visible, and moving a window is the only way to find it. Giving the
//!   picker the window's handle makes it owned too, so it stays above them.
//! * **The event loop.** `pick_folder()` blocks the thread it runs on. Called
//!   from `update`, that is iced's own thread: the download list, the tray
//!   menu and every progress bar stop for as long as the dialog is open, and
//!   Windows reports the window as unresponsive. Running the call on a worker
//!   thread and answering through a message leaves the loop alive — and the
//!   message is also what repaints the window afterwards, because iced asks
//!   for a redraw on every message `update` handles.
//!
//! macOS is the exception to the second point: AppKit dialogs have to be run
//! from the main thread, and the async rfd variants hang there, so the call
//! stays where it is. Ownership is set on Windows only; elsewhere the dialog
//! behaves exactly as it did before.

use iced::Task;

/// A task that reads a window's platform handle and caches it, so a dialog
/// opened later from `update` can be given that window as its owner.
pub fn token_task(id: iced::window::Id) -> Task<crate::app::Message> {
    iced::window::run(id, crate::dialog_parent::token)
        .map(move |handle| crate::app::Message::WindowToken(id, handle))
}

/// Runs `pick` in a native dialog and turns the answer into `wrap`.
///
/// `owner` is the raw handle of the window the dialog should belong to, as
/// cached in the GUI's `handles` map; `None` opens it top-level.
pub fn run<T, M>(
    owner: Option<usize>,
    pick: impl FnOnce(rfd::FileDialog) -> Option<T> + Send + 'static,
    wrap: impl FnOnce(Option<T>) -> M + Send + 'static,
) -> Task<M>
where
    T: Send + 'static,
    M: Send + 'static,
{
    // macOS: AppKit needs the main thread, so nothing crosses it here.
    #[cfg(target_os = "macos")]
    let task = Task::done(wrap(pick(owned(owner))));

    // Everywhere else: a worker thread, so the interface keeps running while
    // the user is choosing. COM is initialised there too, which keeps the
    // picture away from whatever winit set up on the event-loop thread.
    #[cfg(not(target_os = "macos"))]
    let task = {
        let (tx, rx) = iced::futures::channel::oneshot::channel();
        std::thread::spawn(move || {
            let answer = pick(owned(owner));
            let _ = tx.send(answer);
        });
        Task::perform(rx, move |answer| wrap(answer.unwrap_or(None)))
    };

    task
}

/// The dialog, owned by the window `owner` names.
///
/// Exported for the one caller that still shows a picker synchronously (see
/// the comment at the permission re-grant in `app::App::update`).
pub fn owned(owner: Option<usize>) -> rfd::FileDialog {
    let dialog = rfd::FileDialog::new();
    match owner {
        #[cfg(target_os = "windows")]
        Some(hwnd) => dialog.set_parent(&OwnedWindow(hwnd)),
        _ => dialog,
    }
}

/// A window known only by its `HWND`, enough to own a native dialog.
///
/// `iced::window::run` hands the handle out asynchronously, so the token is
/// cached when each window opens and rebuilt here at dialog time.
#[cfg(target_os = "windows")]
struct OwnedWindow(usize);

#[cfg(target_os = "windows")]
impl iced::window::raw_window_handle::HasWindowHandle for OwnedWindow {
    fn window_handle(
        &self,
    ) -> Result<
        iced::window::raw_window_handle::WindowHandle<'_>,
        iced::window::raw_window_handle::HandleError,
    > {
        use iced::window::raw_window_handle as rwh;
        let hwnd =
            std::num::NonZeroIsize::new(self.0 as isize).ok_or(rwh::HandleError::Unavailable)?;
        // SAFETY: the handle comes from a window this process owns and is
        // only used for the duration of one `IFileDialog::Show` call.
        Ok(unsafe {
            rwh::WindowHandle::borrow_raw(rwh::RawWindowHandle::Win32(rwh::Win32WindowHandle::new(
                hwnd,
            )))
        })
    }
}

#[cfg(target_os = "windows")]
impl iced::window::raw_window_handle::HasDisplayHandle for OwnedWindow {
    fn display_handle(
        &self,
    ) -> Result<
        iced::window::raw_window_handle::DisplayHandle<'_>,
        iced::window::raw_window_handle::HandleError,
    > {
        Ok(iced::window::raw_window_handle::DisplayHandle::windows())
    }
}
