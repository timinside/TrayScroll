#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use enigo::{Enigo, Mouse, Settings};
use rdev::{listen, Event, EventType, Key};
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tao::event_loop::{ControlFlow, EventLoopBuilder};

use tray_icon::menu::{Menu, MenuEvent, MenuItem, Submenu};
use tray_icon::{Icon, TrayIconBuilder};

#[cfg(target_os = "windows")]
use windows::Win32::Foundation::{BOOL, HWND, LPARAM};
#[cfg(target_os = "windows")]
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetForegroundWindow, GetWindowTextW, GetWindowThreadProcessId, IsWindowVisible,
};
#[cfg(target_os = "windows")]
use windows::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
#[cfg(target_os = "windows")]
use windows::Win32::System::ProcessStatus::GetModuleBaseNameW;

fn config_path() -> Option<PathBuf> {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("exceptions.txt")))
}

fn load_exceptions() -> Vec<String> {
    let Some(path) = config_path() else {
        return Vec::new();
    };
    let Ok(s) = fs::read_to_string(path) else {
        return Vec::new();
    };
    s.lines()
        .map(|l| l.trim().to_lowercase())
        .filter(|l| !l.is_empty())
        .collect()
}

fn save_exceptions(list: &[String]) {
    let Some(path) = config_path() else {
        return;
    };
    let content = list.join("\n");
    let _ = fs::write(path, content);
}

#[cfg(target_os = "windows")]
fn get_process_name_for_hwnd(hwnd: HWND) -> String {
    unsafe {
        let mut pid: u32 = 0;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid == 0 {
            return String::new();
        }
        let Ok(hproc) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else {
            return String::new();
        };
        let mut buf = [0u16; 260];
        let len = GetModuleBaseNameW(hproc, None, &mut buf);
        let _ = windows::Win32::Foundation::CloseHandle(hproc);
        if len == 0 {
            return String::new();
        }
        String::from_utf16_lossy(&buf[..len as usize]).to_lowercase()
    }
}

#[cfg(target_os = "windows")]
fn list_open_windows() -> Vec<String> {
    use std::sync::Mutex as SMutex;
    let out: Arc<SMutex<Vec<String>>> = Arc::new(SMutex::new(Vec::new()));
    let out2 = out.clone();
    unsafe {
        let _ = EnumWindows(
            Some(enum_window_proc),
            LPARAM(out2.as_ref() as *const SMutex<Vec<String>> as isize),
        );
    }
    let mut v = out.lock().unwrap().clone();
    v.sort();
    v.dedup();
    v.retain(|s| !s.is_empty());
    v
}

#[cfg(target_os = "windows")]
unsafe extern "system" fn enum_window_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
    use std::sync::Mutex as SMutex;
    if !IsWindowVisible(hwnd).as_bool() {
        return BOOL(1);
    }
    let mut buf = [0u16; 512];
    let len = GetWindowTextW(hwnd, &mut buf);
    if len > 0 {
        let title = String::from_utf16_lossy(&buf[..len as usize]);
        let t = title.trim().to_string();
        if !t.is_empty() {
            let ptr = lparam.0 as *const SMutex<Vec<String>>;
            if let Some(m) = ptr.as_ref() {
                if let Ok(mut v) = m.lock() {
                    v.push(t);
                }
            }
        }
    }
    BOOL(1)
}

#[cfg(not(target_os = "windows"))]
fn list_open_windows() -> Vec<String> {
    Vec::new()
}

fn main() {
    let user_enabled = Arc::new(AtomicBool::new(true));
    let in_exception = Arc::new(AtomicBool::new(false));
    let exceptions: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(load_exceptions()));
    let last_real_title: Arc<Mutex<String>> = Arc::new(Mutex::new(String::new()));

    let user_enabled_for_keys = user_enabled.clone();
    let in_exception_for_keys = in_exception.clone();

    std::thread::spawn(move || {
        let _ = listen(move |event| {
            if !user_enabled_for_keys.load(Ordering::SeqCst) {
                return;
            }
            if in_exception_for_keys.load(Ordering::SeqCst) {
                return;
            }
            handle_key_event(event);
        });
    });

    {
        let exceptions_clone = exceptions.clone();
        let in_exception_clone = in_exception.clone();
        let last_real_title_clone = last_real_title.clone();
        std::thread::spawn(move || loop {
            std::thread::sleep(Duration::from_millis(400));
            let title = get_foreground_title();
            if !title.is_empty() {
                if let Ok(mut lt) = last_real_title_clone.lock() {
                    *lt = title.clone();
                }
            }
            let title_lc = title.to_lowercase();
            #[cfg(target_os = "windows")]
            let proc = get_process_name_for_hwnd(unsafe { windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow() });
            #[cfg(not(target_os = "windows"))]
            let proc = String::new();
            let mut matched = false;
            if let Ok(list) = exceptions_clone.lock() {
                for ex in list.iter() {
                    if ex.is_empty() {
                        continue;
                    }
                    if let Some(p) = ex.strip_prefix("proc:") {
                        if !proc.is_empty() && proc == p {
                            matched = true;
                            break;
                        }
                    } else if title_lc.contains(ex) {
                        matched = true;
                        break;
                    }
                }
            }
            in_exception_clone.store(matched, Ordering::SeqCst);
        });
    }

    let event_loop = EventLoopBuilder::<()>::new().build();

    #[cfg(target_os = "windows")]
    let windows_list = list_open_windows();
    #[cfg(not(target_os = "windows"))]
    let windows_list: Vec<String> = Vec::new();

    let menu = Menu::new();
    let toggle_item = MenuItem::new("Disable", true, None);

    let add_submenu = Submenu::new("Add exception (window title contains)", true);
    let mut add_items: Vec<(String, MenuItem)> = Vec::new();
    for title in windows_list.iter().take(20) {
        let label = if title.len() > 60 { format!("{}...", &title[..60]) } else { title.clone() };
        let mi = MenuItem::new(&label, true, None);
        let _ = add_submenu.append(&mi);
        add_items.push((title.clone(), mi));
    }
    if windows_list.is_empty() {
        let mi = MenuItem::new("(no windows found)", false, None);
        let _ = add_submenu.append(&mi);
    }

    let add_proc_submenu = Submenu::new("Add exception (by process .exe)", true);
    let mi_hint = MenuItem::new("(use 'Add current process (.exe) exception')", false, None);
    let _ = add_proc_submenu.append(&mi_hint);

    let del_submenu = Submenu::new("Delete exception (window title match)", true);
    let mut del_items: Vec<(String, MenuItem)> = Vec::new();
    for title in windows_list.iter().take(20) {
        let label = if title.len() > 60 { format!("{}...", &title[..60]) } else { title.clone() };
        let mi = MenuItem::new(&label, true, None);
        let _ = del_submenu.append(&mi);
        del_items.push((title.clone(), mi));
    }
    if windows_list.is_empty() {
        let mi = MenuItem::new("(no windows found)", false, None);
        let _ = del_submenu.append(&mi);
    }

    let add_proc_current_item = MenuItem::new("Add current process (.exe) exception", true, None);
    let list_exc_item = MenuItem::new("List exceptions", true, None);
    let clear_exc_item = MenuItem::new("Clear exceptions", true, None);
    let about_item = MenuItem::new("About", true, None);
    let exit_item = MenuItem::new("Exit", true, None);

    let _ = menu.append(&toggle_item);
    let _ = menu.append(&add_submenu);
    let _ = menu.append(&add_proc_submenu);
    let _ = menu.append(&add_proc_current_item);
    let _ = menu.append(&del_submenu);
    let _ = menu.append(&list_exc_item);
    let _ = menu.append(&clear_exc_item);
    let _ = menu.append(&about_item);
    let _ = menu.append(&exit_item);

    let icon_enabled = create_icon(false);
    let icon_disabled = create_icon(true);

    let tray = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_tooltip("TrayScroll - Key to Scroll")
        .with_icon(icon_enabled.clone())
        .build()
        .expect("failed to create tray icon");

    let menu_receiver = MenuEvent::receiver();
    let user_enabled_for_menu = user_enabled.clone();
    let in_exception_for_menu = in_exception.clone();
    let exceptions_for_menu = exceptions.clone();

    let mut last_eff: Option<bool> = None;

    event_loop.run(move |_event, _, control_flow| {
        *control_flow = ControlFlow::Wait;

        let eff_now = user_enabled_for_menu.load(Ordering::SeqCst) && !in_exception_for_menu.load(Ordering::SeqCst);
        if last_eff != Some(eff_now) {
            let desired_icon = if eff_now { &icon_enabled } else { &icon_disabled };
            let _ = tray.set_icon(Some(desired_icon.clone()));
            last_eff = Some(eff_now);
        }

        if let Ok(ev) = menu_receiver.try_recv() {
            if ev.id == toggle_item.id() {
                let now_enabled = user_enabled_for_menu.load(Ordering::SeqCst);
                let new_state = !now_enabled;
                user_enabled_for_menu.store(new_state, Ordering::SeqCst);
                let _ = toggle_item.set_text(if new_state { "Disable" } else { "Enable" });
            } else if ev.id == add_proc_current_item.id() {
                #[cfg(target_os = "windows")]
                {
                    let hwnd = unsafe { GetForegroundWindow() };
                    let proc = get_process_name_for_hwnd(hwnd);
                    if !proc.is_empty() {
                        let ex = format!("proc:{}", proc);
                        let mut added = false;
                        if let Ok(mut list) = exceptions_for_menu.lock() {
                            if !list.iter().any(|s| s == &ex) {
                                list.push(ex.clone());
                                added = true;
                            }
                            if added {
                                save_exceptions(&list);
                            }
                        }
                        if added {
                            show_message("Exception added", &format!("Added process exception:\n{}", ex));
                        } else {
                            show_message("Exception", "Already in exceptions.");
                        }
                    } else {
                        show_message("Exception", "Could not get process name for foreground window.");
                    }
                }
                #[cfg(not(target_os = "windows"))]
                {
                    show_message("Exception", "Process exceptions are Windows-only.");
                }
            } else if ev.id == list_exc_item.id() {
                let msg = if let Ok(list) = exceptions_for_menu.lock() {
                    if list.is_empty() {
                        "No exceptions set.".to_string()
                    } else {
                        format!("Exceptions (substring, lowercase):\n- {}", list.join("\n- "))
                    }
                } else {
                    "Failed to read exceptions.".to_string()
                };
                show_message("Exceptions", &msg);
            } else if ev.id == clear_exc_item.id() {
                if let Ok(mut list) = exceptions_for_menu.lock() {
                    list.clear();
                    save_exceptions(&list);
                }
                show_message("Exceptions", "All exceptions cleared.");
            } else if ev.id == about_item.id() {
                show_about();
            } else if ev.id == exit_item.id() {
                *control_flow = ControlFlow::Exit;
            } else {
                // check add items
                for (title, mi) in add_items.iter() {
                    if ev.id == mi.id() {
                        let ex = title.to_lowercase();
                        let mut added = false;
                        if let Ok(mut list) = exceptions_for_menu.lock() {
                            if !list.iter().any(|s| s == &ex) {
                                list.push(ex.clone());
                                added = true;
                            }
                            if added {
                                save_exceptions(&list);
                            }
                        }
                        if added {
                            show_message("Exception added", &format!("Added:\n{}", ex));
                        } else {
                            show_message("Exception", "Already in exceptions.");
                        }
                        break;
                    }
                }
                // check del items
                for (title, mi) in del_items.iter() {
                    if ev.id == mi.id() {
                        let ex = title.to_lowercase();
                        let mut removed = false;
                        if let Ok(mut list) = exceptions_for_menu.lock() {
                            let before = list.len();
                            list.retain(|s| s != &ex);
                            removed = list.len() != before;
                            if removed {
                                save_exceptions(&list);
                            }
                        }
                        if removed {
                            show_message("Exception removed", &format!("Removed:\n{}", ex));
                        } else {
                            show_message("Exception", "Not found in exceptions.");
                        }
                        break;
                    }
                }
            }
        }
    });
}

#[cfg(target_os = "windows")]
fn get_foreground_title() -> String {
    unsafe {
        let hwnd: HWND = GetForegroundWindow();
        if hwnd.0.is_null() {
            return String::new();
        }
        let mut buf = [0u16; 512];
        let len = GetWindowTextW(hwnd, &mut buf);
        if len <= 0 {
            return String::new();
        }
        String::from_utf16_lossy(&buf[..len as usize])
    }
}

#[cfg(not(target_os = "windows"))]
fn get_foreground_title() -> String {
    String::new()
}

fn show_message(title: &str, text: &str) {
    #[cfg(target_os = "windows")]
    {
        use std::ffi::OsStr;
        use std::iter::once;
        use std::os::windows::ffi::OsStrExt;
        use windows::core::PCWSTR;
        use windows::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONINFORMATION, MB_OK};

        fn to_wide(s: &str) -> Vec<u16> {
            OsStr::new(s).encode_wide().chain(once(0)).collect()
        }
        let wt = to_wide(title);
        let wm = to_wide(text);
        unsafe {
            let _ = MessageBoxW(None, PCWSTR(wm.as_ptr()), PCWSTR(wt.as_ptr()), MB_OK | MB_ICONINFORMATION);
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        println!("{}: {}", title, text);
    }
}

fn show_about() {
    show_message(
        "About",
        "TrayScroll\nConverts Up/Down/PageUp/PageDown keys into mouse wheel scroll events.\n\nExceptions: managed from tray menu.\nLicense: MIT",
    );
}

fn handle_key_event(event: Event) {
    if let EventType::KeyPress(key) = event.event_type {
        let dy = match key {
            Key::UpArrow => -1,
            Key::DownArrow => 1,
            Key::PageUp => -3,
            Key::PageDown => 3,
            _ => return,
        };
        if let Ok(mut enigo) = Enigo::new(&Settings::default()) {
            let _ = enigo.scroll(dy, enigo::Axis::Vertical);
        }
    }
}

fn create_icon(disabled: bool) -> Icon {
    let size: u32 = 16;
    let mut rgba = Vec::with_capacity((size * size * 4) as usize);
    for y in 0..size {
        for x in 0..size {
            if x == 0 || y == 0 || x == size - 1 || y == size - 1 {
                rgba.extend_from_slice(&[0, 0, 0, 255]);
            } else if disabled {
                rgba.extend_from_slice(&[220, 20, 60, 255]);
            } else {
                rgba.extend_from_slice(&[30, 144, 255, 255]);
            }
        }
    }
    Icon::from_rgba(rgba, size, size).expect("failed to create icon")
}
