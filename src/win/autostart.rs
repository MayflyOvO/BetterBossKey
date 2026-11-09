//! 开机启动：`HKCU\...\CurrentVersion\Run` 下的一个值，不需要管理员权限。
//!
//! 开关状态以注册表为准，不另存配置：值存在、且没有在任务管理器的“启动应用”里
//! 被禁用，即为开启。值名在伪装时用别名（与进程名一致，见 [`super::disguise`]），
//! 否则用 `BetterBossKey`；值指向当前运行的 exe 并带上 [`SILENT_ARG`]，
//! 开机时只在后台运行，不弹出设置窗口。

use std::ffi::c_void;

use windows_sys::Win32::System::Registry::{
    RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW, HKEY_CURRENT_USER, REG_SZ, RRF_RT_ANY,
    RRF_RT_REG_BINARY,
};

use crate::util::wide;

/// 开机启动时附带的参数：启动后不显示设置窗口。
pub const SILENT_ARG: &str = "--autostart";

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
/// 任务管理器“启动应用”的启用 / 禁用记录：首字节为奇数表示已禁用。
const APPROVED_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run";
const PLAIN_NAME: &str = "BetterBossKey";

/// 本进程应当使用的值名：伪装副本用自己的进程名（即别名），否则用应用名。
fn current_name() -> String {
    if super::disguise::is_disguised() {
        if let Some(stem) = std::env::current_exe()
            .ok()
            .and_then(|p| p.file_stem().and_then(|s| s.to_str()).map(str::to_string))
        {
            return stem;
        }
    }
    PLAIN_NAME.to_string()
}

/// 本程序可能用过的全部值名（切换伪装后要清掉旧名字）。
fn our_names(alias: &str) -> Vec<String> {
    let mut names = vec![PLAIN_NAME.to_string(), current_name()];
    if !alias.is_empty() {
        names.push(alias.to_string());
    }
    names.sort_unstable_by_key(|n| n.to_ascii_lowercase());
    names.dedup_by(|a, b| a.eq_ignore_ascii_case(b));
    names
}

fn value_exists(key: &str, name: &str) -> bool {
    let (key, name) = (wide(key), wide(name));
    unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            name.as_ptr(),
            RRF_RT_ANY,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        ) == 0
    }
}

/// 是否被任务管理器禁用（没有记录即视为启用）。
fn disabled_by_user(name: &str) -> bool {
    let (key, name) = (wide(APPROVED_KEY), wide(name));
    let mut buf = [0u8; 16];
    let mut size = buf.len() as u32;
    let ok = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            name.as_ptr(),
            RRF_RT_REG_BINARY,
            std::ptr::null_mut(),
            buf.as_mut_ptr() as *mut c_void,
            &mut size,
        ) == 0
    };
    ok && size > 0 && buf[0] & 1 == 1
}

fn delete_value(key: &str, name: &str) {
    let (key, name) = (wide(key), wide(name));
    unsafe {
        RegDeleteKeyValueW(HKEY_CURRENT_USER, key.as_ptr(), name.as_ptr());
    }
}

/// 开机启动当前是否生效。`alias` 为配置里的伪装别名。
pub fn is_enabled(alias: &str) -> bool {
    our_names(alias)
        .iter()
        .any(|n| value_exists(RUN_KEY, n) && !disabled_by_user(n))
}

/// 开启或关闭开机启动。开启时清掉旧值名与任务管理器里的禁用记录，
/// 再以当前进程的名字与路径写入。
pub fn set(on: bool, alias: &str) {
    for n in our_names(alias) {
        delete_value(RUN_KEY, &n);
        delete_value(APPROVED_KEY, &n);
    }
    if !on {
        return;
    }
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let (key, name) = (wide(RUN_KEY), wide(&current_name()));
    let data = wide(&format!("\"{}\" {}", exe.display(), SILENT_ARG));
    unsafe {
        RegSetKeyValueW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            name.as_ptr(),
            REG_SZ,
            data.as_ptr() as *const c_void,
            (data.len() * 2) as u32,
        );
    }
}

/// 已开启时，把注册项指向当前运行的 exe（程序被移动、切换伪装后保持有效）。
pub fn sync(alias: &str) {
    if is_enabled(alias) {
        set(true, alias);
    }
}
