//! 崩溃恢复文件：`%APPDATA%\BetterBossKey\recovery.txt`（内部文件，无需手动编辑）。
//!
//! 记录此刻被本程序隐藏的窗口、被改了半透明 / 穿透样式的窗口、被置顶的窗口、被去掉标题栏的窗口，
//! 以及被静音的程序。
//! 正常退出时它们都已复原，文件随之清空；若进程被强制结束（无法执行任何收尾代码），
//! 下次启动时据此把窗口与声音找回来（见 `App::recover`）。

use std::fs;
use std::path::PathBuf;
use std::sync::Mutex;

use crate::config::Config;
use crate::win::{audio, window};

/// 文件中记录的一个窗口。HWND 会被系统回收复用，故同时记下进程 ID 与类名用于核对身份。
pub struct Entry {
    pub hwnd: isize,
    pub pid: u32,
    pub class: String,
}

impl Entry {
    /// 该句柄此刻是否仍是记录时的那个窗口。
    pub fn is_same_window(&self) -> bool {
        window::is_alive(self.hwnd)
            && window::window_pid(self.hwnd) == self.pid
            && window::class_of(self.hwnd) == self.class
    }
}

#[derive(Default)]
pub struct Recovery {
    pub hidden: Vec<Entry>,
    pub styled: Vec<Entry>,
    pub topmost: Vec<Entry>,
    /// 被去掉标题栏的窗口及去掉的样式位。
    pub captionless: Vec<(Entry, u32)>,
    pub muted: Vec<String>,
}

/// 上次写入的内容，避免内容未变时重复写盘。
static LAST: Mutex<Option<String>> = Mutex::new(None);

fn path() -> Option<PathBuf> {
    Some(Config::dir()?.join("recovery.txt"))
}

/// 一个窗口的记录字段：`hwnd<TAB>pid<TAB>class`。
fn entry_fields(h: isize) -> String {
    format!("{h}\t{}\t{}", window::window_pid(h), window::class_of(h))
}

fn parse_entry(fields: &str) -> Option<Entry> {
    let mut parts = fields.splitn(3, '\t');
    let (h, pid, class) = (parts.next()?, parts.next()?, parts.next()?);
    Some(Entry {
        hwnd: h.parse().ok()?,
        pid: pid.parse().ok()?,
        class: class.to_string(),
    })
}

/// 把当前登记表写入恢复文件（内容不变则跳过）。
pub fn persist() {
    let mut out = String::new();
    for (key, list) in window::registry_snapshot() {
        for h in list {
            if window::is_alive(h) {
                out.push_str(&format!("{key}={}\n", entry_fields(h)));
            }
        }
    }
    // 去掉标题栏的窗口额外记下去掉的样式位，放在最前：`captionless=bits<TAB>hwnd<TAB>pid<TAB>class`。
    for (h, bits) in window::captionless_snapshot() {
        if window::is_alive(h) {
            out.push_str(&format!("captionless={bits}\t{}\n", entry_fields(h)));
        }
    }
    for exe in audio::muted_exes() {
        out.push_str(&format!("muted={exe}\n"));
    }

    let Ok(mut last) = LAST.lock() else {
        return;
    };
    if last.as_deref() == Some(out.as_str()) {
        return;
    }
    let Some(p) = path() else {
        return;
    };
    if out.is_empty() {
        let _ = fs::remove_file(&p);
    } else {
        if let Some(dir) = p.parent() {
            let _ = fs::create_dir_all(dir);
        }
        let _ = fs::write(&p, &out);
    }
    *last = Some(out);
}

/// 读取上次运行留下的恢复记录（没有则为空）。
pub fn load() -> Recovery {
    let mut r = Recovery::default();
    let Some(text) = path().and_then(|p| fs::read_to_string(p).ok()) else {
        return r;
    };
    for line in text.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        match key {
            "hidden" | "styled" | "topmost" => {
                let Some(e) = parse_entry(value) else {
                    continue;
                };
                match key {
                    "hidden" => r.hidden.push(e),
                    "styled" => r.styled.push(e),
                    _ => r.topmost.push(e),
                }
            }
            "captionless" => {
                let Some((bits, fields)) = value.split_once('\t') else {
                    continue;
                };
                if let (Ok(bits), Some(e)) = (bits.parse(), parse_entry(fields)) {
                    r.captionless.push((e, bits));
                }
            }
            "muted" if !value.is_empty() => r.muted.push(value.to_string()),
            _ => {}
        }
    }
    r
}
