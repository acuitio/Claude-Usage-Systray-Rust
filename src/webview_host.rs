// Bridge between our raw-Win32 HWNDs and wry's WebViewBuilder, which expects
// types that implement HasWindowHandle + HasDisplayHandle from the
// raw-window-handle crate.
//
// Used by dashboard.rs / settings.rs / chart.rs to embed a WebView2 control
// as a child of the existing Win32 host window.

use std::num::NonZeroIsize;
use windows_sys::Win32::Foundation::HWND;
use wry::raw_window_handle::{
    HandleError, HasWindowHandle, RawWindowHandle,
    Win32WindowHandle, WindowHandle,
};

/// Wraps a raw `HWND` so `wry::WebViewBuilder::new_as_child(&parent)` can
/// accept it. We don't need to implement HasDisplayHandle — the child-window
/// builder only consults the window handle.
pub struct ParentWindow(pub HWND);

unsafe impl Send for ParentWindow {}
unsafe impl Sync for ParentWindow {}

impl HasWindowHandle for ParentWindow {
    fn window_handle(&self) -> Result<WindowHandle<'_>, HandleError> {
        let nz = NonZeroIsize::new(self.0 as isize).ok_or(HandleError::Unavailable)?;
        let handle = Win32WindowHandle::new(nz);
        // SAFETY: the HWND is valid for the lifetime of `self`. We borrow it
        // here for the returned WindowHandle; wry uses it synchronously.
        unsafe { Ok(WindowHandle::borrow_raw(RawWindowHandle::Win32(handle))) }
    }
}
