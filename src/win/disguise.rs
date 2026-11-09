//! 进程伪装：让任务管理器 / tasklist 里运行的进程看起来像一个正常的小软件。
//!
//! 做法：启动时（配置开启且当前不是副本）把自己复制为
//! `%LOCALAPPDATA%\<别名>\<别名>.exe` 并启动它，原进程随即退出。别名（如
//! `NovaSync`）首次随机生成后写入配置，之后**保持不变**——正常软件的进程名
//! 从不变化，“每次启动换一个怪名字”本身才是恶意软件特征。
//!
//! 副本据“父目录名与文件主名一致且位于 %LOCALAPPDATA%”识别，不会再次伪装
//! （避免无限自我复制）。固定别名意味着副本始终是同一个文件，每次覆盖，
//! 不产生垃圾。

use std::path::{Path, PathBuf};

use windows_sys::Win32::Foundation::CloseHandle;
use windows_sys::Win32::Storage::FileSystem::SYNCHRONIZE;
use windows_sys::Win32::System::Threading::{
    CreateEventW, CreateProcessW, OpenEventW, PROCESS_INFORMATION, STARTUPINFOW,
};

use crate::config::Config;
use crate::util::wide;

/// “接力重启”标记事件：切换伪装时由前一个进程在启动接棒者之前创建。
/// 接棒者据此走“等待前任退出”的路径，而不是“唤起现有窗口”的路径。
/// 句柄由创建进程持有到退出；两个进程都退出后对象自动销毁。
const RELAY_EVENT: &str = "Local\\{7D3C1A55-8E42-4B60-9C2F-1A6B5D84E930}";

/// 别名词根池：前缀 + 后缀组合出像正常软件的驼峰名（如 `NovaSync`）。
/// 只取常见科技词根，两两组合既不像随机串，也不与系统进程 / 知名软件重名。
const ALIAS_PREFIXES: &[&str] = &[
    "Nova", "Luma", "Echo", "Vertex", "Pixel", "Nimbus", "Cobalt", "Amber", "Zenith", "Flux",
    "Astra", "Terra", "Quill", "Ember", "Onyx", "Sage",
];
const ALIAS_SUFFIXES: &[&str] = &[
    "Sync", "Hub", "Kit", "Lab", "Desk", "Flow", "Notes", "Tune", "Guard", "Link", "Works",
    "Task", "Cal", "Doc", "Pad", "Clip",
];

/// 当前是否处于接力重启（前一个进程正把运行权交给新副本）。
pub fn relay_in_progress() -> bool {
    let name = wide(RELAY_EVENT);
    unsafe {
        let h = OpenEventW(SYNCHRONIZE, 0, name.as_ptr());
        if h.is_null() {
            return false;
        }
        CloseHandle(h);
        true
    }
}

/// 创建接力标记。失败忽略（退化为普通启动路径，功能仍正确）。
fn create_relay_marker() {
    let name = wide(RELAY_EVENT);
    unsafe {
        let h = CreateEventW(std::ptr::null(), 1, 1, name.as_ptr());
        // 故意不关闭句柄：保持标记存活到本进程退出。
        if h.is_null() {
            // 已存在（异常残留）也无妨：复用即可。
        }
    }
}

/// 取一个 64 位伪随机数。仅用于挑词根，不要求密码学随机。
fn rand_u64(seed: u64) -> u64 {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let pid = std::process::id() as u64;
    let stack = &nanos as *const u64 as u64;
    let mut s = nanos ^ pid.rotate_left(32) ^ stack.rotate_left(17) ^ seed;
    // xorshift64*
    s ^= s >> 12;
    s ^= s << 25;
    s ^= s >> 27;
    s.wrapping_mul(0x2545_F491_4F6C_DD1D)
}

/// 随机生成一个像正常软件名的别名（如 `NovaSync`）。
fn random_alias() -> String {
    let a = rand_u64(0x9E37_79B9_7F4A_7C15) as usize % ALIAS_PREFIXES.len();
    let b = rand_u64(0xA5A5_5A5A_C3C3_3C3C) as usize % ALIAS_SUFFIXES.len();
    format!("{}{}", ALIAS_PREFIXES[a], ALIAS_SUFFIXES[b])
}

/// 名字是否是合法的别名（字母数字组成，防止配置被写坏后拼出怪路径）。
fn is_valid_alias(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 24
        && name.bytes().all(|b| b.is_ascii_alphanumeric())
}

/// 两个路径是否指向同一目录（规范化后比较，规避大小写与分隔符差异）。
fn same_dir(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// 当前进程是否已是伪装副本：位于 `%LOCALAPPDATA%\<别名>\<别名>.exe`
/// （父目录名 == 文件主名，据此识别，不依赖配置文件）。
pub fn is_disguised() -> bool {
    let Ok(exe) = std::env::current_exe() else {
        return false;
    };
    let Some(la) = std::env::var_os("LOCALAPPDATA") else {
        return false;
    };
    match (
        exe.parent(),
        exe.file_stem().and_then(|s| s.to_str()),
    ) {
        (Some(parent), Some(stem)) => same_dir(parent, &PathBuf::from(la).join(stem)),
        _ => false,
    }
}

/// 启动一个进程并立即与之脱钩。用于“接力重启”（见 [`crate::app`] 的伪装切换）
/// 与启动伪装副本，后者会先创建接力标记让接棒者走等待路径。成功返回 true。
pub fn spawn(exe: &Path) -> bool {
    create_relay_marker();
    let mut si: STARTUPINFOW = unsafe { std::mem::zeroed() };
    si.cb = std::mem::size_of::<STARTUPINFOW>() as u32;
    let mut pi: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
    let app = wide(&exe.to_string_lossy());
    let ok = unsafe {
        CreateProcessW(
            app.as_ptr(),
            std::ptr::null_mut(),
            std::ptr::null(),
            std::ptr::null(),
            0,
            0,
            std::ptr::null(),
            std::ptr::null(),
            &si,
            &mut pi,
        ) != 0
    };
    if ok {
        unsafe {
            CloseHandle(pi.hProcess);
            CloseHandle(pi.hThread);
        }
    }
    ok
}

/// 把自己复制为别名副本并启动。成功返回 true（调用方应随即退出），
/// 并在成功时把所用别名写入 `cfg` 持久化（首次生成，之后固定复用）。
pub fn launch_disguised_copy(cfg: &mut Config) -> bool {
    let Ok(src) = std::env::current_exe() else {
        return false;
    };
    let Some(la) = std::env::var_os("LOCALAPPDATA") else {
        return false;
    };

    if !is_valid_alias(&cfg.alias) {
        cfg.alias = random_alias();
    }
    let dir = PathBuf::from(&la).join(&cfg.alias);
    let dst = dir.join(format!("{}.exe", cfg.alias));

    if std::fs::create_dir_all(&dir).is_err() {
        return false;
    }
    for _ in 0..3 {
        // 目标不存在，或删得掉（正常运行中不会有本程序的副本在跑：单实例已挡住）
        // 才重新复制；删不掉说明文件被占用（如杀软扫描中），退而启动已有副本。
        if !dst.exists() || std::fs::remove_file(&dst).is_ok() {
            if std::fs::copy(&src, &dst).is_ok() && spawn(&dst) {
                cfg.save();
                return true;
            }
        } else if spawn(&dst) {
            // 沿用上次的副本文件：别名未变，它就是本程序的副本。
            return true;
        }
    }
    false
}
