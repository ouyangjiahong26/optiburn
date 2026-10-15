//! 把文件放进系统剪贴板，供文件管理器直接粘贴。
//!
//! Linux 走 GTK3 剪贴板，同时提供 `text/uri-list` 与 `x-special/gnome-copied-files`
//! 两个目标。Windows 走 CF_HDROP 文件列表加 Preferred DropEffect（复制语义），粘贴
//! 方向读同一个格式。两侧都只写路径，不复制数据，所以剪贴板指向的文件必须在用户
//! 粘贴前一直存在（暂存生命周期见 ADR-0012）。剪贴板内容在应用存活期间有效。

#[cfg(target_os = "linux")]
pub use platform::{copy_files, read_files};

#[cfg(target_os = "windows")]
pub use win::{copy_files, read_files};

#[cfg(not(any(target_os = "linux", target_os = "windows")))]
pub fn copy_files(_paths: &[std::path::PathBuf]) -> Result<(), String> {
    Err(crate::i18n::pick(
        crate::i18n::lang(),
        "复制到系统剪贴板尚未支持该平台。",
        "Copying to the system clipboard is not supported on this platform yet.",
    )
    .to_string())
}

#[cfg(not(any(target_os = "linux", target_os = "windows")))]
pub fn read_files() -> Result<Vec<String>, String> {
    Err(crate::i18n::pick(
        crate::i18n::lang(),
        "从系统剪贴板粘贴尚未支持该平台。",
        "Pasting from the system clipboard is not supported on this platform yet.",
    )
    .to_string())
}

#[cfg(target_os = "linux")]
mod platform {
    use crate::i18n::{lang, pick};
    use gtk::gio;
    use gtk::prelude::*;
    use std::path::PathBuf;
    use std::sync::mpsc;

    /// 把一组本地文件放进剪贴板。
    pub fn copy_files(paths: &[PathBuf]) -> Result<(), String> {
        let uris: Vec<String> = paths
            .iter()
            .map(|path| gio::File::for_path(path).uri().to_string())
            .collect();
        // GTK 只能在主线程上使用，命令跑在工作线程，这里调度回主循环执行并等结果。
        let (sender, receiver) = mpsc::channel();
        gtk::glib::MainContext::default().invoke(move || {
            let _ = sender.send(set_clipboard(&uris));
        });
        receiver.recv().map_err(|_| {
            pick(
                lang(),
                "剪贴板主循环已退出。",
                "The clipboard main loop has exited.",
            )
            .to_string()
        })?
    }

    /// 在主线程上写剪贴板。返回 GTK 是否接受了写入请求。
    fn set_clipboard(uris: &[String]) -> Result<(), String> {
        let clipboard = gtk::Clipboard::get(&gtk::gdk::SELECTION_CLIPBOARD);
        let targets = [
            gtk::TargetEntry::new("text/uri-list", gtk::TargetFlags::empty(), 0),
            gtk::TargetEntry::new("x-special/gnome-copied-files", gtk::TargetFlags::empty(), 1),
        ];
        // text/uri-list 规范用 CRLF 分隔、每行以换行结尾，GNOME 的复制文件目标
        // 首行是操作名、每行以 LF 结尾。载荷都补齐行尾换行，不依赖消费端容错。
        let uri_list = format!("{}\r\n", uris.join("\r\n"));
        let gnome_list = format!("copy\n{}\n", uris.join("\n"));
        let accepted = clipboard.set_with_data(&targets, move |_, selection, info| {
            if info == 1 {
                selection.set(
                    &gtk::gdk::Atom::intern("x-special/gnome-copied-files"),
                    8,
                    gnome_list.as_bytes(),
                );
            } else {
                selection.set(
                    &gtk::gdk::Atom::intern("text/uri-list"),
                    8,
                    uri_list.as_bytes(),
                );
            }
        });
        if !accepted {
            return Err(pick(
                lang(),
                "写入系统剪贴板失败。",
                "Failed to write to the system clipboard.",
            )
            .to_string());
        }
        Ok(())
    }

    /// 读取剪贴板里的文件并转成路径，剪贴板里没有文件时返回空列表。
    pub fn read_files() -> Result<Vec<String>, String> {
        let (sender, receiver) = mpsc::channel();
        gtk::glib::MainContext::default().invoke(move || {
            let clipboard = gtk::Clipboard::get(&gtk::gdk::SELECTION_CLIPBOARD);
            let paths: Vec<String> = clipboard_uris(&clipboard)
                .iter()
                .filter_map(|uri| gio::File::for_uri(uri).path())
                .map(|path| path.display().to_string())
                .collect();
            let _ = sender.send(paths);
        });
        receiver.recv().map_err(|_| {
            pick(
                lang(),
                "剪贴板主循环已退出。",
                "The clipboard main loop has exited.",
            )
            .to_string()
        })
    }

    /// 取剪贴板里的文件 URI。优先标准目标 text/uri-list，兜底解析 GNOME 的
    /// x-special/gnome-copied-files（首行是 copy 或 cut，其后每行一个 URI）。
    fn clipboard_uris(clipboard: &gtk::Clipboard) -> Vec<String> {
        let uris: Vec<String> = clipboard
            .wait_for_uris()
            .iter()
            .map(|uri| uri.to_string())
            .collect();
        if !uris.is_empty() {
            return uris;
        }
        let target = gtk::gdk::Atom::intern("x-special/gnome-copied-files");
        let Some(selection) = clipboard.wait_for_contents(&target) else {
            return Vec::new();
        };
        let Some(text) = selection.text() else {
            return Vec::new();
        };
        text.lines()
            .skip(1)
            .map(str::trim)
            .filter(|line| line.starts_with("file://"))
            .map(str::to_string)
            .collect()
    }
}

#[cfg(target_os = "windows")]
mod win {
    use crate::i18n::{lang, pick};
    use std::os::windows::ffi::OsStrExt;
    use std::path::PathBuf;
    use windows_sys::Win32::Foundation::{GlobalFree, HANDLE};
    use windows_sys::Win32::System::DataExchange::{
        CloseClipboard, EmptyClipboard, GetClipboardData, IsClipboardFormatAvailable,
        OpenClipboard, RegisterClipboardFormatW, SetClipboardData,
    };
    use windows_sys::Win32::System::Memory::{
        GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock,
    };
    use windows_sys::Win32::UI::Shell::DragQueryFileW;

    /// CF_HDROP：剪贴板里的文件列表格式（WinUser.h 的常量，windows-sys 未导出）。
    const CF_HDROP: u32 = 15;
    /// DROPEFFECT_COPY：告诉接收端这是复制而不是移动。
    const DROPEFFECT_COPY: u32 = 1;

    /// 把一组本地文件放进剪贴板。路径要一直存在到粘贴完成，暂存生命周期与 Linux
    /// 侧同一套（ADR-0012）。
    pub fn copy_files(paths: &[PathBuf]) -> Result<(), String> {
        if paths.is_empty() {
            return Err(pick(lang(), "没有选中文件。", "No files are selected.").to_string());
        }
        let files = dropfiles_bytes(paths);
        let effect = DROPEFFECT_COPY.to_le_bytes();
        unsafe {
            if OpenClipboard(std::ptr::null_mut()) == 0 {
                return Err(clipboard_error());
            }
            let result = write_entries(&files, &effect);
            CloseClipboard();
            result
        }
    }

    /// 读取剪贴板里的文件路径，剪贴板里没有文件时返回空列表。
    pub fn read_files() -> Result<Vec<String>, String> {
        unsafe {
            if IsClipboardFormatAvailable(CF_HDROP) == 0 {
                return Ok(Vec::new());
            }
            if OpenClipboard(std::ptr::null_mut()) == 0 {
                return Err(clipboard_error());
            }
            let result = clipboard_paths();
            CloseClipboard();
            result
        }
    }

    /// 剪贴板必须打开才能写。清空旧的、再放文件列表与复制语义。
    fn write_entries(files: &[u8], effect: &[u8]) -> Result<(), String> {
        unsafe {
            if EmptyClipboard() == 0 {
                return Err(clipboard_error());
            }
            if !set_bytes(CF_HDROP, files) || !set_bytes(preferred_drop_effect(), effect) {
                return Err(clipboard_error());
            }
            Ok(())
        }
    }

    /// 把一段字节放进剪贴板。内存由 GlobalAlloc 分配，SetClipboardData 成功后系统
    /// 接管所有权（失败时由我们释放），所以这里不调用 GlobalFree。
    unsafe fn set_bytes(format: u32, bytes: &[u8]) -> bool {
        unsafe {
            let handle = GlobalAlloc(GMEM_MOVEABLE, bytes.len());
            if handle.is_null() {
                return false;
            }
            let pointer = GlobalLock(handle);
            if pointer.is_null() {
                GlobalFree(handle);
                return false;
            }
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), pointer.cast::<u8>(), bytes.len());
            GlobalUnlock(handle);
            if SetClipboardData(format, handle).is_null() {
                GlobalFree(handle);
                return false;
            }
            true
        }
    }

    /// 从已经打开的剪贴板里取 CF_HDROP 的文件路径。句柄归剪贴板所有，读完不释放。
    unsafe fn clipboard_paths() -> Result<Vec<String>, String> {
        unsafe {
            let handle = GetClipboardData(CF_HDROP);
            if handle.is_null() {
                return Ok(Vec::new());
            }
            let handle = handle as HANDLE;
            // ifile 传 0xFFFFFFFF 时返回条目数。传条目号时返回该路径的长度（不含 NUL）。
            let count = DragQueryFileW(handle, u32::MAX, std::ptr::null_mut(), 0);
            let mut paths = Vec::with_capacity(count as usize);
            for index in 0..count {
                let length = DragQueryFileW(handle, index, std::ptr::null_mut(), 0);
                let mut buffer = vec![0u16; length as usize + 1];
                let written =
                    DragQueryFileW(handle, index, buffer.as_mut_ptr(), buffer.len() as u32);
                paths.push(String::from_utf16_lossy(&buffer[..written as usize]));
            }
            Ok(paths)
        }
    }

    /// CF_HDROP 的字节：20 字节的 DROPFILES 头（偏移指向紧随其后的文件列表，fWide
    /// 置位表示宽字符），随后是每个路径的宽字符加 NUL，最后再用一个 NUL 收尾。
    /// 抽成函数是为了在测试里钉住字节布局。
    fn dropfiles_bytes(paths: &[PathBuf]) -> Vec<u8> {
        let mut units: Vec<u16> = Vec::new();
        for path in paths {
            units.extend(path.as_os_str().encode_wide());
            units.push(0);
        }
        units.push(0);
        let mut bytes = Vec::with_capacity(20 + units.len() * 2);
        bytes.extend_from_slice(&20u32.to_le_bytes()); // pFiles：头自身长度
        bytes.extend_from_slice(&0i32.to_le_bytes()); // pt.x
        bytes.extend_from_slice(&0i32.to_le_bytes()); // pt.y
        bytes.extend_from_slice(&0i32.to_le_bytes()); // fNC
        bytes.extend_from_slice(&1i32.to_le_bytes()); // fWide
        for unit in units {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
        bytes
    }

    /// “首选拖放效果”格式的编号（每次现注册，开销可忽略）。
    fn preferred_drop_effect() -> u32 {
        let name: Vec<u16> = "Preferred DropEffect\0".encode_utf16().collect();
        unsafe { RegisterClipboardFormatW(name.as_ptr()) }
    }

    fn clipboard_error() -> String {
        pick(
            lang(),
            "写入系统剪贴板失败。",
            "Failed to write to the system clipboard.",
        )
        .to_string()
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// 真机往返：写剪贴板再读回来。会动到系统剪贴板，默认不跑，需要时手动执行：
        /// `cargo test --manifest-path src-tauri/Cargo.toml --lib -- --ignored clipboard_round_trip`
        #[test]
        #[ignore = "touches the real clipboard"]
        fn clipboard_round_trip_through_the_win32_api() {
            let dir = std::env::temp_dir().join("optiburn-clipboard-check");
            std::fs::create_dir_all(&dir).unwrap();
            let first = dir.join("check-a.txt");
            let second = dir.join("检查-b.txt");
            std::fs::write(&first, b"a").unwrap();
            std::fs::write(&second, b"b").unwrap();
            copy_files(&[first.clone(), second.clone()]).expect("写剪贴板");
            let read = read_files().expect("读剪贴板");
            assert_eq!(
                read,
                vec![first.display().to_string(), second.display().to_string()]
            );
            std::fs::remove_dir_all(&dir).ok();
        }

        #[test]
        fn dropfiles_bytes_round_trips_the_paths() {
            let paths = vec![PathBuf::from(r"C:\救援\a.txt"), PathBuf::from("d.bin")];
            let bytes = dropfiles_bytes(&paths);
            assert_eq!(
                u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
                20,
                "文件列表从头之后开始"
            );
            assert_eq!(
                i32::from_le_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]),
                1,
                "宽字符标志"
            );
            let units: Vec<u16> = bytes[20..]
                .as_chunks::<2>()
                .0
                .iter()
                .map(|pair| u16::from_le_bytes(*pair))
                .collect();
            assert!(units.ends_with(&[0, 0]), "列表以双 NUL 结束");
            let decoded: Vec<String> = units
                .split(|unit| *unit == 0)
                .filter(|group| !group.is_empty())
                .map(String::from_utf16_lossy)
                .collect();
            assert_eq!(
                decoded,
                vec![r"C:\救援\a.txt".to_string(), "d.bin".to_string()]
            );
        }
    }
}
