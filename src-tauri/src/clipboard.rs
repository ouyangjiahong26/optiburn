//! 把文件放进系统剪贴板，供文件管理器直接粘贴（GNOME 的“复制文件”语义）。
//!
//! Linux 走 GTK3 剪贴板，同时提供 `text/uri-list` 与 `x-special/gnome-copied-files`
//! 两个目标，文件管理器读取 URI 后从本地路径复制数据，所以剪贴板指向的文件必须
//! 在用户粘贴前一直存在（暂存生命周期见 ADR-0012）。剪贴板内容在应用存活期间有效，
//! GNOME 默认没有独立剪贴板管理器。Windows 尚未实现，返回明确错误。

#[cfg(target_os = "linux")]
pub use platform::{copy_files, read_files};

#[cfg(not(target_os = "linux"))]
pub fn copy_files(_paths: &[std::path::PathBuf]) -> Result<(), String> {
    Err("复制到系统剪贴板尚未支持该平台。".to_string())
}

#[cfg(not(target_os = "linux"))]
pub fn read_files() -> Result<Vec<String>, String> {
    Err("从系统剪贴板粘贴尚未支持该平台。".to_string())
}

#[cfg(target_os = "linux")]
mod platform {
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
        receiver
            .recv()
            .map_err(|_| "剪贴板主循环已退出。".to_string())?
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
            return Err("写入系统剪贴板失败。".to_string());
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
        receiver
            .recv()
            .map_err(|_| "剪贴板主循环已退出。".to_string())
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
