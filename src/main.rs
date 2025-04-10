#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::{
    ops::{ControlFlow, Range},
    sync::{OnceLock, mpsc},
    time::Duration,
};
use windows::Win32::{
    Foundation::{GetLastError, HINSTANCE, HWND, LPARAM, RECT},
    Graphics::Gdi as gdi,
    UI::{Accessibility as acc, WindowsAndMessaging as wam},
};
use windows::core::{BOOL, Result};

fn overlap_range(a: Range<i32>, b: Range<i32>) -> bool {
    a.contains(&b.start) || b.contains(&a.start)
}

fn intersect(a: &RECT, b: &RECT) -> bool {
    overlap_range(a.top..a.bottom, b.top..b.bottom)
        && overlap_range(a.left..a.right, b.left..b.right)
}

fn find_my_tv() -> Option<(gdi::HMONITOR, RECT)> {
    let mut ret = None;
    _ = enumerate_display_monitors(None, None, |hmon, _hdc, area| {
        if area.bottom - area.top == 2160 {
            ret = Some((hmon, *area));
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    });
    ret
}

static NOTIFY: OnceLock<mpsc::Sender<()>> = OnceLock::new();

fn main() -> windows::core::Result<()> {
    let (tx, rx) = mpsc::channel();
    _ = tx.send(());
    _ = NOTIFY.set(tx);

    std::thread::spawn(|| updater_thread(rx));

    unsafe {
        acc::SetWinEventHook(
            wam::EVENT_OBJECT_LOCATIONCHANGE,
            wam::EVENT_OBJECT_LOCATIONCHANGE,
            None,
            Some(handler),
            0,
            0,
            wam::WINEVENT_OUTOFCONTEXT,
        );
    }

    let mut msg = wam::MSG::default();
    loop {
        unsafe { wam::GetMessageA(&mut msg, None, 0, 0) }.ok()?;
    }
}

fn updater_thread(rx: mpsc::Receiver<()>) {
    loop {
        // Wait for one message
        let Ok(()) = rx.recv() else { break };
        // Wait for the Windows stuff to actually update
        std::thread::sleep(Duration::from_millis(5));

        // Clear the queue
        rx.try_iter().for_each(drop);

        let Some((hmon, mut area)) = find_my_tv() else {
            continue;
        };

        area.left += 50;

        if let Err(e) = update(hmon, area) {
            println!("oh no: {e}");
        }

        std::thread::sleep(Duration::from_millis(250));
    }
}

extern "system" fn handler(
    _: acc::HWINEVENTHOOK,
    event: u32,
    _hwnd: HWND,
    object_id: i32,
    _child_id: i32,
    _: u32,
    _: u32,
) {
    if object_id == wam::OBJID_CURSOR.0 {
        return;
    }

    if event != wam::EVENT_OBJECT_LOCATIONCHANGE {
        return;
    }

    _ = NOTIFY.get().unwrap().send(());
}

fn update(hmon: gdi::HMONITOR, area: RECT) -> Result<()> {
    let show_bar = any_windows_in(area)?;

    let mut taskbar_hwnd = HWND::default();
    _ = enumerate_windows(|hwnd| unsafe {
        if gdi::MonitorFromWindow(hwnd, gdi::MONITOR_DEFAULTTONULL) != hmon {
            return ControlFlow::Continue(());
        }

        if (Window { hwnd }).class_name().unwrap() != "Shell_SecondaryTrayWnd" {
            return ControlFlow::Continue(());
        }

        taskbar_hwnd = hwnd;
        ControlFlow::Break(())
    });

    unsafe {
        let sw = if show_bar { wam::SW_SHOW } else { wam::SW_HIDE };
        _ = wam::ShowWindow(taskbar_hwnd, sw);
    }

    Ok(())
}

fn any_windows_in(area: RECT) -> Result<bool> {
    if (area.bottom - area.top) * (area.right - area.left) <= 0 {
        return Ok(false);
    }

    let mut the_result = Ok(false);

    _ = enumerate_windows(|hwnd| unsafe {
        match check(hwnd, &area) {
            done @ (Ok(true) | Err(_)) => {
                the_result = done;
                ControlFlow::Break(())
            }
            Ok(false) => ControlFlow::Continue(()),
        }
    });

    the_result
}

pub unsafe fn check(hwnd: HWND, area: &RECT) -> Result<bool> {
    let window = Window { hwnd };

    let info = window.info()?;

    let not_a_real_window = !intersect(area, &info.rcWindow)
        || !info.dwStyle.contains(wam::WS_VISIBLE)
        || info.dwExStyle.contains(wam::WS_EX_TOOLWINDOW);

    if not_a_real_window {
        return Ok(false);
    }

    Ok(true)
}

pub fn enumerate_windows<F: FnMut(HWND) -> ControlFlow<(), ()>>(
    mut f: F,
) -> windows::core::Result<()> {
    unsafe extern "system" fn wnd_enum_proc<F: FnMut(HWND) -> ControlFlow<(), ()>>(
        param0: HWND,
        param1: LPARAM,
    ) -> BOOL {
        let ret: ControlFlow<(), ()> = unsafe {
            let func: *mut F = std::ptr::with_exposed_provenance_mut(param1.0 as usize);
            (*func)(param0)
        };
        ret.is_continue().into()
    }

    let lparam = LPARAM((&raw mut f).expose_provenance() as isize);
    unsafe { wam::EnumWindows(Some(wnd_enum_proc::<F>), lparam) }
}

pub fn enumerate_display_monitors<
    F: FnMut(gdi::HMONITOR, gdi::HDC, &RECT) -> ControlFlow<(), ()>,
>(
    hdc: Option<gdi::HDC>,
    clip: Option<&RECT>,
    mut f: F,
) -> windows::core::Result<()> {
    unsafe extern "system" fn monitor_enum_proc<
        F: FnMut(gdi::HMONITOR, gdi::HDC, &RECT) -> ControlFlow<(), ()>,
    >(
        monitor: gdi::HMONITOR,
        context: gdi::HDC,
        area: *mut RECT,
        userdata: LPARAM,
    ) -> BOOL {
        let ret: ControlFlow<(), ()> = unsafe {
            let func: *mut F = std::ptr::with_exposed_provenance_mut(userdata.0 as usize);
            (*func)(monitor, context, &*area)
        };
        ret.is_continue().into()
    }

    let lparam = LPARAM((&raw mut f).expose_provenance() as isize);
    unsafe {
        gdi::EnumDisplayMonitors(
            hdc,
            clip.map(|x| &raw const *x),
            Some(monitor_enum_proc::<F>),
            lparam,
        )
        .ok()
    }
}

#[derive(Debug)]
struct Window {
    hwnd: HWND,
}

#[allow(dead_code)]
impl Window {
    pub unsafe fn new(handle: HWND) -> Self {
        Self { hwnd: handle }
    }

    pub fn info(&self) -> Result<wam::WINDOWINFO> {
        let mut info = wam::WINDOWINFO::default();
        info.cbSize = std::mem::size_of_val(&info) as u32;
        unsafe { wam::GetWindowInfo(self.hwnd, &mut info) }?;
        Ok(info)
    }

    pub fn text(&self) -> Result<String> {
        unsafe {
            let len = wam::GetWindowTextLengthA(self.hwnd) as usize;
            if len == 0 {
                return Ok(String::new());
            }
            let mut buf = vec![0; len + 1];
            let len = wam::GetWindowTextA(self.hwnd, &mut buf) as usize;
            buf.truncate(len);
            Ok(String::from_utf8(buf).unwrap())
        }
    }

    fn hinstance(&self) -> Result<HINSTANCE> {
        unsafe {
            let ret = wam::GetWindowLongPtrA(self.hwnd, wam::GWL_HINSTANCE);
            if ret == 0 {
                GetLastError().ok()?;
            }
            Ok(HINSTANCE(std::ptr::with_exposed_provenance_mut(
                ret as usize,
            )))
        }
    }

    fn class_name(&self) -> Result<String> {
        let mut buf = vec![0; 256];
        unsafe {
            let len = wam::GetClassNameA(self.hwnd, &mut buf) as usize;
            if len == 0 {
                GetLastError().ok()?;
            }
            buf.truncate(len);
        }
        Ok(String::from_utf8(buf).unwrap())
    }
}
