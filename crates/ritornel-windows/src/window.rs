//! A window that is never shown, for a process that has none.
//!
//! A program without a console hears no Ctrl+C, and Windows has no
//! SIGTERM. What it has is the message a window gets when it is asked to
//! close: `taskkill /PID <pid>`, without `/F`, posts `WM_CLOSE` to every
//! top-level window of the process, and the end of a session sends
//! `WM_ENDSESSION`, after which the process is ended. A hidden window turns
//! both into a clean stop. It is also what the system's media controls are
//! tied to, as they are to a window.
//!
//! The window lives on a thread of its own, which runs its messages.

use std::cell::RefCell;
use std::io;
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex, OnceLock, PoisonError};
use std::thread::JoinHandle;
use std::time::Duration;

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW, MSG,
    PostMessageW, PostQuitMessage, RegisterClassW, TranslateMessage, WM_APP, WM_CLOSE, WM_DESTROY,
    WM_ENDSESSION, WM_QUERYENDSESSION, WNDCLASSW,
};

/// Why the window was asked to close.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Closing {
    /// Something asked the process to close, as `taskkill /PID` does.
    Asked,
    /// The user is signing out, or the machine is shutting down. The
    /// process is ended once [`Window::stopped`] is called, or after
    /// [`ENDING_WAIT`].
    SessionEnding,
}

/// How long a session that is ending waits for the process to stop before
/// it lets the system end it.
pub const ENDING_WAIT: Duration = Duration::from_secs(10);

/// The message that tells the window's thread to end.
const WM_END_THREAD: u32 = WM_APP + 1;

/// The window class every such window is of.
const CLASS: &str = "RitornelHidden";

/// Whether the process has stopped, which a session that is ending waits
/// for.
#[derive(Debug, Default)]
struct Stopped {
    done: Mutex<bool>,
    changed: Condvar,
}

impl Stopped {
    fn set(&self) {
        *self.done.lock().unwrap_or_else(PoisonError::into_inner) = true;
        self.changed.notify_all();
    }

    fn wait(&self, longest: Duration) {
        let done = self.done.lock().unwrap_or_else(PoisonError::into_inner);
        let _ = self
            .changed
            .wait_timeout_while(done, longest, |done| !*done);
    }
}

type OnClose = Box<dyn Fn(Closing) + Send>;

thread_local! {
    /// What the window on this thread does when it is asked to close.
    static WINDOW: RefCell<Option<(OnClose, Arc<Stopped>)>> = const { RefCell::new(None) };
}

/// A hidden window and the thread that runs it. Dropping it closes the
/// window and ends the thread.
#[derive(Debug)]
pub struct Window {
    /// The window's handle, as a number so that it may cross threads.
    handle: isize,
    stopped: Arc<Stopped>,
    thread: Option<JoinHandle<()>>,
}

impl Window {
    /// Opens a hidden window named `title`, which calls `on_close` when it
    /// is asked to close. It does not close: the process stops, and drops
    /// the window as it does.
    pub fn open(title: &str, on_close: impl Fn(Closing) + Send + 'static) -> io::Result<Self> {
        let stopped = Arc::new(Stopped::default());
        let (opened, handle) = mpsc::channel();
        let title: Vec<u16> = title.encode_utf16().chain([0]).collect();
        let thread = std::thread::Builder::new()
            .name("hidden window".to_owned())
            .spawn({
                let stopped = Arc::clone(&stopped);
                move || {
                    let handle = match create(&title) {
                        Ok(handle) => handle,
                        Err(error) => {
                            let _ = opened.send(Err(error));
                            return;
                        }
                    };
                    WINDOW.with(|window| {
                        *window.borrow_mut() = Some((Box::new(on_close), stopped));
                    });
                    let _ = opened.send(Ok(handle as isize));
                    run();
                    WINDOW.with(|window| window.borrow_mut().take());
                }
            })?;
        let handle = handle
            .recv()
            .map_err(|_| io::Error::other("the window's thread ended"))??;
        Ok(Self {
            handle,
            stopped,
            thread: Some(thread),
        })
    }

    /// The window's handle, an `HWND`, for what must be tied to a window.
    pub fn handle(&self) -> isize {
        self.handle
    }

    /// Says that the process has stopped, so that a session that is ending
    /// may end it.
    pub fn stopped(&self) {
        self.stopped.set();
    }
}

impl Drop for Window {
    fn drop(&mut self) {
        // SAFETY: posting to a window that has gone fails and does nothing
        // else.
        let posted = unsafe { PostMessageW(self.handle as HWND, WM_END_THREAD, 0, 0) };
        if posted != 0
            && let Some(thread) = self.thread.take()
        {
            let _ = thread.join();
        }
    }
}

/// Registers the class, once for the process, and creates a window of it on
/// this thread.
fn create(title: &[u16]) -> io::Result<HWND> {
    static REGISTERED: OnceLock<Result<(), i32>> = OnceLock::new();
    let class: Vec<u16> = CLASS.encode_utf16().chain([0]).collect();
    // SAFETY: a null name asks for this program's own module.
    let instance = unsafe { GetModuleHandleW(std::ptr::null()) };
    REGISTERED
        .get_or_init(|| {
            // SAFETY: plain data, for which all zeroes is valid; the fields
            // that matter are set.
            let mut description: WNDCLASSW = unsafe { std::mem::zeroed() };
            description.lpfnWndProc = Some(procedure);
            description.hInstance = instance;
            description.lpszClassName = class.as_ptr();
            // SAFETY: the description and the name it points to are valid
            // for the call, which copies them.
            if unsafe { RegisterClassW(&description) } == 0 {
                return Err(io::Error::last_os_error().raw_os_error().unwrap_or(0));
            }
            Ok(())
        })
        .map_err(io::Error::from_raw_os_error)?;
    // SAFETY: the class is registered and both names end in a NUL. The
    // window has no style that shows it, no parent and no menu.
    let handle = unsafe {
        CreateWindowExW(
            0,
            class.as_ptr(),
            title.as_ptr(),
            0,
            0,
            0,
            0,
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            instance,
            std::ptr::null(),
        )
    };
    if handle.is_null() {
        return Err(io::Error::last_os_error());
    }
    Ok(handle)
}

/// Runs this thread's messages until the window is gone.
fn run() {
    // SAFETY: plain data, for which all zeroes is valid.
    let mut message: MSG = unsafe { std::mem::zeroed() };
    // SAFETY: `message` is written by each call and read by the next two.
    unsafe {
        while GetMessageW(&mut message, std::ptr::null_mut(), 0, 0) > 0 {
            TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
}

/// Calls the window's `on_close`.
fn closing(why: Closing) -> Option<Arc<Stopped>> {
    WINDOW.with(|window| {
        let window = window.borrow();
        let (on_close, stopped) = window.as_ref()?;
        on_close(why);
        Some(Arc::clone(stopped))
    })
}

unsafe extern "system" fn procedure(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_CLOSE => {
            closing(Closing::Asked);
            0
        }
        // The session may end; this window does not stand in its way.
        WM_QUERYENDSESSION => 1,
        WM_ENDSESSION => {
            if wparam != 0
                && let Some(stopped) = closing(Closing::SessionEnding)
            {
                stopped.wait(ENDING_WAIT);
            }
            0
        }
        WM_END_THREAD => {
            // SAFETY: the window is this thread's own.
            unsafe { DestroyWindow(window) };
            0
        }
        WM_DESTROY => {
            // SAFETY: ends this thread's message loop, and nothing else.
            unsafe { PostQuitMessage(0) };
            0
        }
        // SAFETY: what this window does not handle is the system's to.
        _ => unsafe { DefWindowProcW(window, message, wparam, lparam) },
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use windows_sys::Win32::UI::WindowsAndMessaging::SendMessageW;

    use super::*;

    /// One window test at a time: `taskkill` posts to every top-level
    /// window of the process, and fails on one that another test closes
    /// meanwhile.
    static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());

    #[test]
    fn taskkill_without_force_asks_the_window_to_close_and_nothing_more() {
        let _alone = ONE_AT_A_TIME.lock().unwrap_or_else(PoisonError::into_inner);
        let (asked, heard) = mpsc::channel();
        let window = Window::open("Ritornel test", move |why| {
            let _ = asked.send(why);
        })
        .unwrap();
        let done = std::process::Command::new("taskkill")
            .args(["/PID", &std::process::id().to_string()])
            .output()
            .unwrap();
        assert!(done.status.success(), "{done:?}");
        assert_eq!(
            heard.recv_timeout(Duration::from_secs(10)).unwrap(),
            Closing::Asked
        );
        // The process is still here, and the window closes when dropped.
        drop(window);
    }

    #[test]
    fn a_session_that_ends_waits_for_the_process_to_stop() {
        let _alone = ONE_AT_A_TIME.lock().unwrap_or_else(PoisonError::into_inner);
        let (asked, heard) = mpsc::channel();
        let window = Arc::new(
            Window::open("Ritornel test", move |why| {
                let _ = asked.send(why);
            })
            .unwrap(),
        );
        let handle = window.handle();
        let begun = Instant::now();
        let ending = std::thread::spawn(move || {
            // SAFETY: the window lives until this test drops it, after the
            // thread is joined.
            unsafe { SendMessageW(handle as HWND, WM_ENDSESSION, 1, 0) }
        });
        // The other test's taskkill may ask this window to close as well.
        while heard.recv_timeout(Duration::from_secs(10)).unwrap() != Closing::SessionEnding {}
        std::thread::sleep(Duration::from_millis(100));
        assert!(!ending.is_finished());
        window.stopped();
        ending.join().unwrap();
        assert!(begun.elapsed() < ENDING_WAIT);
    }
}
