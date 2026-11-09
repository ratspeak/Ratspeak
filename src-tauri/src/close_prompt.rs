//! Native prompt for the main window's close button on Windows.

use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::UI::Controls::{
    TaskDialogIndirect, TASKDIALOGCONFIG, TASKDIALOG_BUTTON, TDF_ALLOW_DIALOG_CANCELLATION,
    TDF_POSITION_RELATIVE_TO_WINDOW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::IDCANCEL;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Choice {
    Quit,
    Tray,
    Cancel,
}

const ID_QUIT: i32 = 1001;
const ID_TRAY: i32 = 1002;

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Blocks until answered. Returns the choice and whether "Don't ask again" was
/// checked; Esc and the dialog's own close button answer Cancel.
pub fn ask(parent: isize) -> (Choice, bool) {
    let title = wide("Ratspeak");
    let content = wide("Quit Ratspeak or keep it running in the system tray?");
    let remember_text = wide("Don't ask again");
    let quit = wide("Quit");
    let tray = wide("Minimize to tray");
    let cancel = wide("Cancel");
    let buttons = [
        TASKDIALOG_BUTTON {
            nButtonID: ID_QUIT,
            pszButtonText: quit.as_ptr(),
        },
        TASKDIALOG_BUTTON {
            nButtonID: ID_TRAY,
            pszButtonText: tray.as_ptr(),
        },
        TASKDIALOG_BUTTON {
            nButtonID: IDCANCEL,
            pszButtonText: cancel.as_ptr(),
        },
    ];
    let config = TASKDIALOGCONFIG {
        cbSize: std::mem::size_of::<TASKDIALOGCONFIG>() as u32,
        hwndParent: parent as HWND,
        dwFlags: TDF_ALLOW_DIALOG_CANCELLATION | TDF_POSITION_RELATIVE_TO_WINDOW,
        pszWindowTitle: title.as_ptr(),
        pszContent: content.as_ptr(),
        cButtons: buttons.len() as u32,
        pButtons: buttons.as_ptr(),
        nDefaultButton: ID_TRAY,
        pszVerificationText: remember_text.as_ptr(),
        ..Default::default()
    };
    let mut pressed = 0;
    let mut remember = 0;
    // SAFETY: every pointer in `config` borrows a local that outlives this
    // synchronous call; the radio-button out-parameter may be null.
    let result =
        unsafe { TaskDialogIndirect(&config, &mut pressed, std::ptr::null_mut(), &mut remember) };
    if result < 0 {
        // A prompt that cannot open keeps the previous behavior.
        return (Choice::Tray, false);
    }
    let choice = match pressed {
        ID_QUIT => Choice::Quit,
        ID_TRAY => Choice::Tray,
        _ => Choice::Cancel,
    };
    (choice, remember != 0 && choice != Choice::Cancel)
}
