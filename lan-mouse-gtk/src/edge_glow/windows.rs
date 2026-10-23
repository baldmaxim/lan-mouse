//! Windows backend: a click-through layered window on a thread of its own.
//!
//! The window is created lazily on the first flash and reused afterwards.
//! The pixels are uploaded once per flash (`UpdateLayeredWindow` with a
//! premultiplied BGRA DIB), every animation frame only changes the constant
//! alpha of the blend function.

use std::{
    cell::RefCell,
    ffi::c_void,
    mem::{size_of, zeroed},
    ptr::{copy_nonoverlapping, null, null_mut},
    sync::{Mutex, OnceLock, mpsc},
    time::Instant,
};

use input_event::edge::{Edge, Rect};
use windows_sys::Win32::{
    Foundation::{HWND, LPARAM, LRESULT, POINT, SIZE, WPARAM},
    Graphics::Gdi::{
        AC_SRC_ALPHA, AC_SRC_OVER, BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BLENDFUNCTION,
        CreateCompatibleDC, CreateDIBSection, DIB_RGB_COLORS, DeleteObject, EnumDisplayMonitors,
        GetMonitorInfoW, HBITMAP, HDC, HMONITOR, MONITORINFO, SelectObject,
    },
    System::LibraryLoader::GetModuleHandleW,
    UI::{
        HiDpi::{
            DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForMonitor, MDT_EFFECTIVE_DPI,
            SetThreadDpiAwarenessContext,
        },
        WindowsAndMessaging::{
            CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW, HWND_TOPMOST,
            KillTimer, MSG, PostMessageW, RegisterClassExW, SPI_GETCLIENTAREAANIMATION, SW_HIDE,
            SW_SHOWNOACTIVATE, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SetTimer, SetWindowPos,
            ShowWindow, SystemParametersInfoW, TranslateMessage, ULW_ALPHA, UpdateLayeredWindow,
            WM_APP, WM_TIMER, WNDCLASSEXW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
            WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
        },
    },
};

use super::render::{
    GLOW_DEPTH, GLOW_LENGTH, glow_opacity, locate, place, render_glow, swap_red_blue,
};

/// posted to the glow window to start a flash, see [`PENDING`]
const WM_FLASH: u32 = WM_APP;
const TIMER_ID: usize = 1;
/// animation frame interval
const FRAME_MS: u32 = 15;
const BASE_DPI: f64 = 96.0;

/// the flash requested last, picked up by the glow thread
static PENDING: Mutex<Option<(Edge, f64)>> = Mutex::new(None);
/// handle of the glow window (as integer to be `Sync`), `None` if it could not be created
static WINDOW: OnceLock<Option<isize>> = OnceLock::new();

struct Monitor {
    rect: Rect,
    dpi: u32,
}

/// state of the glow thread
struct Glow {
    /// memory DC the current image is selected in
    dc: HDC,
    /// DIB section with the current image
    bitmap: HBITMAP,
    /// start of the running animation
    started: Instant,
}

thread_local! {
    static GLOW: RefCell<Option<Glow>> = const { RefCell::new(None) };
}

pub fn flash(edge: Edge, fraction: f64) {
    let Some(hwnd) = *WINDOW.get_or_init(spawn_glow_thread) else {
        return;
    };
    *PENDING.lock().unwrap_or_else(|e| e.into_inner()) = Some((edge, fraction));
    // SAFETY: posting to a window handle is sound for any value
    unsafe { PostMessageW(hwnd as HWND, WM_FLASH, 0, 0) };
}

fn spawn_glow_thread() -> Option<isize> {
    let (tx, rx) = mpsc::channel();
    let spawned = std::thread::Builder::new()
        .name("edge-glow".into())
        .spawn(move || {
            // SAFETY: only called once, on the new thread
            unsafe { run_glow_thread(tx) }
        });
    if let Err(e) = spawned {
        log::warn!("edge glow: failed to spawn thread: {e}");
        return None;
    }
    rx.recv().ok().flatten()
}

unsafe fn run_glow_thread(tx: mpsc::Sender<Option<isize>>) {
    // the thread (and its window) works in physical pixels of every monitor
    SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);

    let hwnd = create_window();
    if hwnd.is_null() {
        log::warn!("edge glow: failed to create window");
        let _ = tx.send(None);
        return;
    }
    GLOW.with(|g| {
        *g.borrow_mut() = Some(Glow {
            dc: CreateCompatibleDC(null_mut()),
            bitmap: null_mut(),
            started: Instant::now(),
        });
    });
    let _ = tx.send(Some(hwnd as isize));

    let mut msg: MSG = zeroed();
    while GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {
        TranslateMessage(&msg);
        DispatchMessageW(&msg);
    }
}

unsafe fn create_window() -> HWND {
    let class_name: Vec<u16> = "LanMouseEdgeGlow\0".encode_utf16().collect();
    let instance = GetModuleHandleW(null());

    let mut class: WNDCLASSEXW = zeroed();
    class.cbSize = size_of::<WNDCLASSEXW>() as u32;
    class.lpfnWndProc = Some(window_proc);
    class.hInstance = instance;
    class.lpszClassName = class_name.as_ptr();
    // an already registered class is fine, window creation reports real errors
    RegisterClassExW(&class);

    CreateWindowExW(
        WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
        class_name.as_ptr(),
        null(),
        WS_POPUP,
        0,
        0,
        1,
        1,
        null_mut(),
        null_mut(),
        instance,
        null(),
    )
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_FLASH => {
            start_flash(hwnd);
            0
        }
        WM_TIMER if wparam == TIMER_ID => {
            tick(hwnd);
            0
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

/// `true` if the user turned off animations in the system settings
unsafe fn reduced_motion() -> bool {
    let mut enabled: i32 = 1;
    let ok = SystemParametersInfoW(
        SPI_GETCLIENTAREAANIMATION,
        0,
        &mut enabled as *mut i32 as *mut c_void,
        0,
    );
    ok != 0 && enabled == 0
}

unsafe fn start_flash(hwnd: HWND) {
    let Some((edge, fraction)) = PENDING.lock().unwrap_or_else(|e| e.into_inner()).take() else {
        return;
    };
    if reduced_motion() {
        return;
    }

    let monitors = monitors();
    let rects: Vec<Rect> = monitors.iter().map(|m| m.rect).collect();
    let Some((display, point)) = locate(&rects, edge, fraction) else {
        return;
    };
    let dpi = monitors
        .iter()
        .find(|m| m.rect == display)
        .map_or(96, |m| m.dpi);
    let scale = dpi as f64 / BASE_DPI;
    let placement = place(
        display,
        edge,
        point,
        GLOW_DEPTH * scale,
        GLOW_LENGTH * scale,
    );

    let rect = placement.rect;
    let (x, y) = (rect.x_min as i32, rect.y_min as i32);
    let (w, h) = (
        (rect.x_max - rect.x_min) as i32,
        (rect.y_max - rect.y_min) as i32,
    );
    let mut pixels = render_glow(edge, w as u32, h as u32, placement.center_along);
    swap_red_blue(&mut pixels);

    GLOW.with(|g| {
        let mut g = g.borrow_mut();
        let Some(glow) = g.as_mut() else { return };

        // upload the new image
        let mut info: BITMAPINFO = zeroed();
        info.bmiHeader.biSize = size_of::<BITMAPINFOHEADER>() as u32;
        info.bmiHeader.biWidth = w;
        // negative height: top-down rows
        info.bmiHeader.biHeight = -h;
        info.bmiHeader.biPlanes = 1;
        info.bmiHeader.biBitCount = 32;
        info.bmiHeader.biCompression = BI_RGB;
        let mut bits: *mut c_void = null_mut();
        let bitmap = CreateDIBSection(null_mut(), &info, DIB_RGB_COLORS, &mut bits, null_mut(), 0);
        if bitmap.is_null() || bits.is_null() {
            log::warn!("edge glow: failed to create bitmap");
            return;
        }
        copy_nonoverlapping(pixels.as_ptr(), bits as *mut u8, pixels.len());
        SelectObject(glow.dc, bitmap);
        if !glow.bitmap.is_null() {
            DeleteObject(glow.bitmap);
        }
        glow.bitmap = bitmap;
        glow.started = Instant::now();

        // position and image, still fully transparent
        let dst = POINT { x, y };
        let size = SIZE { cx: w, cy: h };
        let src = POINT { x: 0, y: 0 };
        let blend = blend_function(0);
        UpdateLayeredWindow(
            hwnd,
            null_mut(),
            &dst,
            &size,
            glow.dc,
            &src,
            0,
            &blend,
            ULW_ALPHA,
        );
    });

    // (re)start the animation without stealing the focus
    ShowWindow(hwnd, SW_SHOWNOACTIVATE);
    SetWindowPos(
        hwnd,
        HWND_TOPMOST,
        0,
        0,
        0,
        0,
        SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
    );
    SetTimer(hwnd, TIMER_ID, FRAME_MS, None);
    tick(hwnd);
}

unsafe fn tick(hwnd: HWND) {
    GLOW.with(|g| {
        let g = g.borrow();
        let Some(glow) = g.as_ref() else { return };
        match glow_opacity(glow.started.elapsed()) {
            Some(opacity) => {
                // only the constant alpha changes: no position, size or new image
                let src = POINT { x: 0, y: 0 };
                let blend = blend_function((opacity * 255.0).round() as u8);
                UpdateLayeredWindow(
                    hwnd,
                    null_mut(),
                    null(),
                    null(),
                    glow.dc,
                    &src,
                    0,
                    &blend,
                    ULW_ALPHA,
                );
            }
            None => {
                KillTimer(hwnd, TIMER_ID);
                ShowWindow(hwnd, SW_HIDE);
            }
        }
    });
}

fn blend_function(alpha: u8) -> BLENDFUNCTION {
    BLENDFUNCTION {
        BlendOp: AC_SRC_OVER as u8,
        BlendFlags: 0,
        SourceConstantAlpha: alpha,
        AlphaFormat: AC_SRC_ALPHA as u8,
    }
}

/// all monitors in virtual screen coordinates (physical pixels) with their DPI
unsafe fn monitors() -> Vec<Monitor> {
    unsafe extern "system" fn collect(
        monitor: HMONITOR,
        _dc: HDC,
        _rect: *mut windows_sys::Win32::Foundation::RECT,
        data: LPARAM,
    ) -> i32 {
        let list = &mut *(data as *mut Vec<Monitor>);
        let mut info: MONITORINFO = zeroed();
        info.cbSize = size_of::<MONITORINFO>() as u32;
        if GetMonitorInfoW(monitor, &mut info) != 0 {
            let (mut dpi_x, mut dpi_y) = (0u32, 0u32);
            let dpi = if GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y) == 0
                && dpi_x != 0
            {
                dpi_x
            } else {
                96
            };
            let r = info.rcMonitor;
            list.push(Monitor {
                rect: Rect {
                    x_min: r.left as f64,
                    y_min: r.top as f64,
                    x_max: r.right as f64,
                    y_max: r.bottom as f64,
                },
                dpi,
            });
        }
        1
    }

    let mut list: Vec<Monitor> = Vec::new();
    EnumDisplayMonitors(
        null_mut(),
        null(),
        Some(collect),
        &mut list as *mut Vec<Monitor> as LPARAM,
    );
    list
}
