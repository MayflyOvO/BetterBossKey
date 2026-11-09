//! 音频：按程序静音（隐藏时静音），以及监视默认输出设备（耳机断开时隐藏）。
//!
//! windows-sys 不含 COM 接口定义，这里按 vtable 槽位手写调用所需的少量 Core Audio
//! 接口（IMMDeviceEnumerator / IMMDevice / IAudioSessionManager2 等），不引入额外依赖。
//! 除设备通知回调外都在主线程（STA）上调用；回调在系统线程上触发，只做 PostMessage。
//!
//! 静音按**进程可执行名**匹配音频会话：浏览器 / Electron 程序的声音由同名的子进程播放，
//! 只按窗口所属进程 ID 匹配会漏掉它们。只登记并恢复“由本程序静音”的会话，用户自己
//! 静音的会话不受影响。

use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::atomic::{AtomicIsize, AtomicPtr, AtomicU32, Ordering};
use std::sync::Mutex;

use windows_sys::core::GUID;
use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::System::Com::{CoCreateInstance, CoTaskMemFree, CLSCTX_ALL};
use windows_sys::Win32::UI::WindowsAndMessaging::PostMessageW;

use crate::win::window;

type Ptr = *mut c_void;

const CLSID_MM_DEVICE_ENUMERATOR: GUID = GUID::from_u128(0xBCDE0395_E52F_467C_8E3D_C4579291692E);
const IID_IMM_DEVICE_ENUMERATOR: GUID = GUID::from_u128(0xA95664D2_9614_4F35_A746_DE8DB63617E6);
const IID_IAUDIO_SESSION_MANAGER2: GUID = GUID::from_u128(0x77AA99A0_1BD6_484F_8BC7_2C654C9A9B6F);
const IID_IAUDIO_SESSION_CONTROL2: GUID = GUID::from_u128(0xBFB7FF88_7239_4FC9_8FA2_07C950BE9C6D);
const IID_ISIMPLE_AUDIO_VOLUME: GUID = GUID::from_u128(0x87CE5498_68D6_44E5_9215_6DA47EF883D8);
const IID_IMM_NOTIFICATION_CLIENT: GUID = GUID::from_u128(0x7991EEC9_7E89_4D85_8390_6C703CEC60C0);
const IID_IUNKNOWN: GUID = GUID::from_u128(0x00000000_0000_0000_C000_000000000046);

/// PROPERTYKEY。
#[repr(C)]
#[derive(Clone, Copy)]
struct PropKey {
    fmtid: GUID,
    pid: u32,
}

const PKEY_AUDIO_ENDPOINT_FORM_FACTOR: PropKey = PropKey {
    fmtid: GUID::from_u128(0x1DA5D803_D492_4EDD_8C23_E0C0FFEE7F0E),
    pid: 0,
};

const E_RENDER: i32 = 0;
const E_CONSOLE: i32 = 0;
const DEVICE_STATE_ACTIVE: u32 = 1;
const STGM_READ: u32 = 0;
const VT_UI4: u16 = 19;
const E_NOINTERFACE: i32 = 0x8000_4002_u32 as i32;
/// EndpointFormFactor：耳机 / 耳麦。
const FORM_HEADPHONES: u32 = 3;
const FORM_HEADSET: u32 = 5;

#[link(name = "ole32")]
extern "system" {
    fn PropVariantClear(pvar: *mut c_void) -> i32;
}

fn same_guid(a: &GUID, b: &GUID) -> bool {
    a.data1 == b.data1 && a.data2 == b.data2 && a.data3 == b.data3 && a.data4 == b.data4
}

/// 调用 COM 方法：`call!(对象, 槽位, (参数类型…), 参数…)`，返回 HRESULT。
macro_rules! call {
    ($obj:expr, $slot:expr, ($($ty:ty),*) $(, $arg:expr)*) => {{
        let f: unsafe extern "system" fn(Ptr $(, $ty)*) -> i32 = std::mem::transmute($obj.slot($slot));
        f($obj.0 $(, $arg)*)
    }};
}

/// 持有一个 COM 接口指针，Drop 时 Release。
struct Com(Ptr);

impl Com {
    /// 第 `i` 个 vtable 槽位（0..2 为 IUnknown）。
    unsafe fn slot(&self, i: usize) -> *const c_void {
        let vtbl = *(self.0 as *const *const *const c_void);
        *vtbl.add(i)
    }

    fn query(&self, iid: &GUID) -> Option<Com> {
        out_com(|p| unsafe { call!(self, 0, (*const GUID, *mut Ptr), iid, p) })
    }
}

impl Drop for Com {
    fn drop(&mut self) {
        unsafe {
            let release: unsafe extern "system" fn(Ptr) -> u32 = std::mem::transmute(self.slot(2));
            release(self.0);
        }
    }
}

/// 调用一个“输出接口指针”的方法，成功则包装为 [`Com`]。
fn out_com(f: impl FnOnce(*mut Ptr) -> i32) -> Option<Com> {
    let mut p: Ptr = std::ptr::null_mut();
    (f(&mut p) >= 0 && !p.is_null()).then_some(Com(p))
}

/// 调用一个“输出 CoTaskMem 宽字符串”的方法，取回字符串并释放内存。
fn out_string(f: impl FnOnce(*mut *mut u16) -> i32) -> Option<String> {
    let mut p: *mut u16 = std::ptr::null_mut();
    if f(&mut p) < 0 || p.is_null() {
        return None;
    }
    unsafe {
        let mut n = 0;
        while *p.add(n) != 0 {
            n += 1;
        }
        let s = String::from_utf16_lossy(std::slice::from_raw_parts(p, n));
        CoTaskMemFree(p as *const c_void);
        Some(s)
    }
}

fn enumerator() -> Option<Com> {
    out_com(|p| unsafe {
        CoCreateInstance(
            &CLSID_MM_DEVICE_ENUMERATOR,
            std::ptr::null_mut(),
            CLSCTX_ALL,
            &IID_IMM_DEVICE_ENUMERATOR,
            p,
        )
    })
}

/// 遍历所有活动输出设备上的音频会话：`f(音量接口, 进程可执行名, 会话实例标识)`。
fn for_each_session(mut f: impl FnMut(&Com, &str, &str)) {
    let Some(en) = enumerator() else {
        return;
    };
    let Some(devices) =
        out_com(|p| unsafe { call!(en, 3, (i32, u32, *mut Ptr), E_RENDER, DEVICE_STATE_ACTIVE, p) })
    else {
        return;
    };
    let mut exe_of: HashMap<u32, String> = HashMap::new();
    let mut n = 0u32;
    unsafe {
        call!(devices, 3, (*mut u32), &mut n);
    }
    for i in 0..n {
        let Some(dev) = out_com(|p| unsafe { call!(devices, 4, (u32, *mut Ptr), i, p) }) else {
            continue;
        };
        let Some(mgr) = out_com(|p| unsafe {
            call!(
                dev,
                3,
                (*const GUID, u32, *const c_void, *mut Ptr),
                &IID_IAUDIO_SESSION_MANAGER2,
                CLSCTX_ALL,
                std::ptr::null(),
                p
            )
        }) else {
            continue;
        };
        let Some(sessions) = out_com(|p| unsafe { call!(mgr, 5, (*mut Ptr), p) }) else {
            continue;
        };
        let mut m = 0i32;
        unsafe {
            call!(sessions, 3, (*mut i32), &mut m);
        }
        for j in 0..m {
            let Some(ctl) = out_com(|p| unsafe { call!(sessions, 4, (i32, *mut Ptr), j, p) }) else {
                continue;
            };
            let Some(ctl2) = ctl.query(&IID_IAUDIO_SESSION_CONTROL2) else {
                continue;
            };
            let mut pid = 0u32;
            unsafe {
                call!(ctl2, 14, (*mut u32), &mut pid);
            }
            if pid == 0 {
                continue; // 系统声音
            }
            let exe = exe_of.entry(pid).or_insert_with(|| window::process_name_of(pid));
            if exe.is_empty() {
                continue;
            }
            let Some(id) = out_string(|p| unsafe { call!(ctl2, 13, (*mut *mut u16), p) }) else {
                continue;
            };
            if let Some(vol) = ctl.query(&IID_ISIMPLE_AUDIO_VOLUME) {
                f(&vol, exe, &id);
            }
        }
    }
}

fn is_muted(vol: &Com) -> bool {
    let mut m = 0i32;
    unsafe {
        call!(vol, 6, (*mut i32), &mut m);
    }
    m != 0
}

fn set_mute(vol: &Com, on: bool) {
    unsafe {
        call!(vol, 5, (i32, *const GUID), on as i32, std::ptr::null());
    }
}

/// 由本程序静音的会话：(进程可执行名, 会话实例标识)。
static MUTED: Mutex<Vec<(String, String)>> = Mutex::new(Vec::new());

/// 静音这些程序（按可执行名）当前所有未静音的会话，并登记以便恢复。
/// 可重复调用：隐藏期间新出现的会话（如新开始播放）也会被补上。
pub fn mute(exes: &[String]) {
    if exes.is_empty() {
        return;
    }
    let mut newly: Vec<(String, String)> = Vec::new();
    for_each_session(|vol, exe, id| {
        if exes.iter().any(|e| e.eq_ignore_ascii_case(exe)) && !is_muted(vol) {
            set_mute(vol, true);
            newly.push((exe.to_string(), id.to_string()));
        }
    });
    if newly.is_empty() {
        return;
    }
    if let Ok(mut reg) = MUTED.lock() {
        for e in newly {
            if !reg.iter().any(|r| r.1 == e.1) {
                reg.push(e);
            }
        }
    }
    crate::recovery::persist();
}

/// 取消所有由本程序施加的静音。
pub fn unmute_all() {
    let ids: Vec<String> = match MUTED.lock() {
        Ok(mut reg) => std::mem::take(&mut *reg).into_iter().map(|(_, id)| id).collect(),
        Err(_) => return,
    };
    if ids.is_empty() {
        return;
    }
    for_each_session(|vol, _, id| {
        if ids.iter().any(|i| i == id) {
            set_mute(vol, false);
        }
    });
    crate::recovery::persist();
}

/// 崩溃恢复：取消这些程序所有会话的静音（上次运行被强制结束时留下的）。
pub fn unmute_exes(exes: &[String]) {
    if exes.is_empty() {
        return;
    }
    for_each_session(|vol, exe, _| {
        if exes.iter().any(|e| e.eq_ignore_ascii_case(exe)) && is_muted(vol) {
            set_mute(vol, false);
        }
    });
}

/// 当前被本程序静音的程序（去重），供崩溃恢复文件持久化。
pub fn muted_exes() -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    if let Ok(reg) = MUTED.try_lock() {
        for (exe, _) in reg.iter() {
            if !out.iter().any(|e| e.eq_ignore_ascii_case(exe)) {
                out.push(exe.clone());
            }
        }
    }
    out
}

// ——————————————————————— 默认输出设备 ———————————————————————

/// 默认输出设备：(设备 ID, 是否耳机 / 耳麦)。
pub fn default_output() -> Option<(String, bool)> {
    let en = enumerator()?;
    let dev = out_com(|p| unsafe { call!(en, 4, (i32, i32, *mut Ptr), E_RENDER, E_CONSOLE, p) })?;
    let id = out_string(|p| unsafe { call!(dev, 5, (*mut *mut u16), p) })?;
    let form = out_com(|p| unsafe { call!(dev, 4, (u32, *mut Ptr), STGM_READ, p) })
        .and_then(|store| unsafe {
            // PROPVARIANT：x64 下 24 字节，vt 在偏移 0，数值在偏移 8。
            let mut pv = [0u64; 3];
            let hr = call!(
                store,
                5,
                (*const PropKey, *mut c_void),
                &PKEY_AUDIO_ENDPOINT_FORM_FACTOR,
                pv.as_mut_ptr() as *mut c_void
            );
            if hr < 0 {
                return None;
            }
            let vt = pv[0] as u16;
            let value = pv[1] as u32;
            PropVariantClear(pv.as_mut_ptr() as *mut c_void);
            (vt == VT_UI4).then_some(value)
        });
    let headphones = matches!(form, Some(FORM_HEADPHONES | FORM_HEADSET));
    Some((id, headphones))
}

static NOTIFY_HWND: AtomicIsize = AtomicIsize::new(0);
static NOTIFY_MSG: AtomicU32 = AtomicU32::new(0);
/// 注册了通知回调的设备枚举器（注销时需要同一个对象）。
static WATCH_ENUM: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());

/// IMMNotificationClient 的 vtable。
#[repr(C)]
struct NotifyVtbl {
    query_interface: unsafe extern "system" fn(Ptr, *const GUID, *mut Ptr) -> i32,
    add_ref: unsafe extern "system" fn(Ptr) -> u32,
    release: unsafe extern "system" fn(Ptr) -> u32,
    on_device_state_changed: unsafe extern "system" fn(Ptr, *const u16, u32) -> i32,
    on_device_added: unsafe extern "system" fn(Ptr, *const u16) -> i32,
    on_device_removed: unsafe extern "system" fn(Ptr, *const u16) -> i32,
    on_default_device_changed: unsafe extern "system" fn(Ptr, i32, i32, *const u16) -> i32,
    on_property_value_changed: unsafe extern "system" fn(Ptr, *const u16, PropKey) -> i32,
}

/// 静态的通知对象：生命周期贯穿进程，引用计数无需真实维护。
#[repr(C)]
struct NotifyClient {
    vtbl: &'static NotifyVtbl,
}

static NOTIFY_VTBL: NotifyVtbl = NotifyVtbl {
    query_interface: nc_query_interface,
    add_ref: nc_add_ref,
    release: nc_add_ref,
    on_device_state_changed: nc_ignore_state,
    on_device_added: nc_ignore,
    on_device_removed: nc_ignore,
    on_default_device_changed: nc_default_changed,
    on_property_value_changed: nc_ignore_property,
};
static NOTIFY_CLIENT: NotifyClient = NotifyClient { vtbl: &NOTIFY_VTBL };

unsafe extern "system" fn nc_query_interface(this: Ptr, iid: *const GUID, out: *mut Ptr) -> i32 {
    if same_guid(&*iid, &IID_IUNKNOWN) || same_guid(&*iid, &IID_IMM_NOTIFICATION_CLIENT) {
        *out = this;
        0
    } else {
        *out = std::ptr::null_mut();
        E_NOINTERFACE
    }
}
unsafe extern "system" fn nc_add_ref(_: Ptr) -> u32 {
    1
}
unsafe extern "system" fn nc_ignore_state(_: Ptr, _: *const u16, _: u32) -> i32 {
    0
}
unsafe extern "system" fn nc_ignore(_: Ptr, _: *const u16) -> i32 {
    0
}
unsafe extern "system" fn nc_ignore_property(_: Ptr, _: *const u16, _: PropKey) -> i32 {
    0
}
unsafe extern "system" fn nc_default_changed(_: Ptr, flow: i32, role: i32, _: *const u16) -> i32 {
    if flow == E_RENDER && role == E_CONSOLE {
        let hwnd = NOTIFY_HWND.load(Ordering::SeqCst) as HWND;
        PostMessageW(hwnd, NOTIFY_MSG.load(Ordering::SeqCst), 0, 0);
    }
    0
}

/// 开始监视默认输出设备：变化时向 `hwnd` 投递 `msg`。
pub fn watch_default_output(hwnd: HWND, msg: u32) -> bool {
    unwatch_default_output();
    let Some(en) = enumerator() else {
        return false;
    };
    NOTIFY_HWND.store(hwnd as isize, Ordering::SeqCst);
    NOTIFY_MSG.store(msg, Ordering::SeqCst);
    let client = &NOTIFY_CLIENT as *const NotifyClient as Ptr;
    if unsafe { call!(en, 6, (Ptr), client) } < 0 {
        return false;
    }
    WATCH_ENUM.store(en.0, Ordering::SeqCst);
    std::mem::forget(en); // 由 unwatch_default_output 释放
    true
}

pub fn unwatch_default_output() {
    let p = WATCH_ENUM.swap(std::ptr::null_mut(), Ordering::SeqCst);
    if !p.is_null() {
        let en = Com(p);
        let client = &NOTIFY_CLIENT as *const NotifyClient as Ptr;
        unsafe {
            call!(en, 7, (Ptr), client);
        }
    }
}
