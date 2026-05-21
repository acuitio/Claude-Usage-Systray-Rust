// Usage history chart — Win32 host for a WebView2 control. Rendering lives
// in assets/chart.html. Rust pushes the usage_history array as JSON and
// listens for "ready" messages back.
//
// Mirrors src/dashboard.rs.

use std::cell::RefCell;
use std::ptr::null_mut;
use windows_sys::w;
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

use crate::common::*;
use crate::webview_host::ParentWindow;

static mut HWND_CHART: HWND = null_mut();

thread_local! {
    static WEBVIEW: RefCell<Option<wry::WebView>> = const { RefCell::new(None) };
}

const CHART_HTML: &str = include_str!("../assets/chart.html");

pub unsafe fn is_open() -> bool {
    !HWND_CHART.is_null() && IsWindow(HWND_CHART) != 0
}

pub unsafe fn on_data_changed() {
    if is_open() { push_history(); }
}

pub unsafe fn open(_owner: HWND) {
    if is_open() {
        if GetWindowLongW(HWND_CHART, GWL_STYLE) & WS_MINIMIZE as i32 != 0 {
            ShowWindow(HWND_CHART, SW_RESTORE);
        }
        SetForegroundWindow(HWND_CHART);
        return;
    }

    let class_name = w!("Win32ChartWebView");
    let instance = GetModuleHandleW(std::ptr::null());

    let wc = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        style: 0,
        lpfnWndProc: Some(wnd_proc),
        cbClsExtra: 0, cbWndExtra: 0,
        hInstance: instance,
        hIcon: null_mut(),
        hCursor: LoadCursorW(null_mut(), IDC_ARROW),
        hbrBackground: HBR_BG,
        lpszMenuName: std::ptr::null(),
        lpszClassName: class_name,
        hIconSm: null_mut(),
    };
    RegisterClassExW(&wc);

    let mut wa: RECT = std::mem::zeroed();
    SystemParametersInfoW(SPI_GETWORKAREA, 0, &mut wa as *mut _ as *mut _, 0);
    let cw = 760;
    let ch = 480;
    let cx = wa.left + ((wa.right - wa.left) - cw) / 2;
    let cy = wa.top  + ((wa.bottom - wa.top) - ch) / 2;

    HWND_CHART = CreateWindowExW(
        0, class_name, w!("Usage Chart"),
        WS_OVERLAPPEDWINDOW | WS_VISIBLE,
        cx, cy, cw, ch,
        null_mut(), null_mut(), instance, std::ptr::null(),
    );
    SetWindowPos(HWND_CHART, HWND_TOP, 0, 0, 0, 0,
        SWP_NOMOVE | SWP_NOSIZE | SWP_SHOWWINDOW);
    UpdateWindow(HWND_CHART);
    SetForegroundWindow(HWND_CHART);
}

unsafe fn attach_webview(parent_hwnd: HWND) {
    let parent = ParentWindow(parent_hwnd);
    let result = wry::WebViewBuilder::new_as_child(&parent)
        .with_html(CHART_HTML)
        .with_ipc_handler(handle_ipc)
        .with_on_page_load_handler(|_event, _url| push_history())
        .with_transparent(false)
        .build();

    match result {
        Ok(webview) => {
            resize_webview_to_parent(&webview, parent_hwnd);
            WEBVIEW.with(|c| *c.borrow_mut() = Some(webview));
        }
        Err(e) => {
            let msg = wstr(&format!(
                "Failed to initialize WebView2:\n{e}\n\n\
                Make sure the WebView2 Runtime is installed (built into Windows 11)."
            ));
            MessageBoxW(parent_hwnd, msg.as_ptr(), w!("Usage Chart"),
                        MB_OK | MB_ICONERROR);
        }
    }
}

fn resize_webview_to_parent(webview: &wry::WebView, parent: HWND) {
    let mut rc: RECT = unsafe { std::mem::zeroed() };
    unsafe { GetClientRect(parent, &mut rc); }
    let _ = webview.set_bounds(wry::Rect {
        position: wry::dpi::LogicalPosition::new(0.0, 0.0).into(),
        size:     wry::dpi::PhysicalSize::new(
            (rc.right - rc.left) as u32,
            (rc.bottom - rc.top) as u32,
        ).into(),
    });
}

fn push_history() {
    let hist = crate::usage_history::load();
    match serde_json::to_string(&hist) {
        Ok(json) => {
            let js = format!("window.applyHistory && window.applyHistory({json})");
            WEBVIEW.with(|c| {
                if let Some(wv) = c.borrow().as_ref() {
                    let _ = wv.evaluate_script(&js);
                }
            });
        }
        Err(e) => crate::common::dlog(&format!("chart: serialize history failed: {e}")),
    }
}

fn handle_ipc(req: wry::http::Request<String>) {
    let Ok(value): Result<serde_json::Value, _> = serde_json::from_str(req.body()) else { return; };
    let cmd = value.get("cmd").and_then(|v| v.as_str()).unwrap_or("");
    if cmd == "ready" { push_history(); }
}

extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_CREATE => { attach_webview(hwnd); 0 }
            WM_SIZE => {
                WEBVIEW.with(|c| {
                    if let Some(wv) = c.borrow().as_ref() {
                        resize_webview_to_parent(wv, hwnd);
                    }
                });
                0
            }
            WM_DPICHANGED => { handle_dpi_changed(hwnd, lp); 0 }
            WM_CLOSE      => { DestroyWindow(hwnd); 0 }
            WM_DESTROY    => {
                WEBVIEW.with(|c| { c.borrow_mut().take(); });
                HWND_CHART = null_mut();
                0
            }
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}
