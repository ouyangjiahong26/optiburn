//! 应用内更新的入口判断。更新本体（检查、下载、安装）由 tauri-plugin-updater
//! 在前端驱动，这里只回答“当前这份安装能不能就地更新”，取舍见 ADR-0015。

/// 应用内更新入口是否可用：前端据其显示或隐藏“检查更新”。
///
/// Windows 的安装形态只有 NSIS 安装包，updater 会下载新安装包静默安装。
/// Linux 上 deb 安装的文件归包管理器管，就地替换会破坏 dpkg 记录，只有
/// AppImage 允许，其运行时会置 `APPIMAGE` 环境变量指向自身路径。
/// macOS 不在本项目支持范围内，恒为假。
#[tauri::command]
pub fn can_self_update() -> bool {
    self_update_supported()
}

/// 纯判断，供测试：平台是常量分支，Linux 看 `APPIMAGE` 是否存在。
fn self_update_supported() -> bool {
    if cfg!(target_os = "windows") {
        true
    } else if cfg!(target_os = "linux") {
        std::env::var_os("APPIMAGE").is_some()
    } else {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::self_update_supported;

    // 只测 Linux 分支：Windows 是常量真，没有可观察行为。
    // 本 crate 的其它测试不读 APPIMAGE，临时改动再恢复不会串到邻居。
    #[cfg(target_os = "linux")]
    #[test]
    fn linux_self_update_follows_appimage_env() {
        let saved = std::env::var_os("APPIMAGE");
        // SAFETY: 测试串行运行在本进程内，且本文件是 APPIMAGE 的唯一读写方。
        unsafe { std::env::remove_var("APPIMAGE") };
        assert!(!self_update_supported());
        // SAFETY: 同上。
        unsafe { std::env::set_var("APPIMAGE", "/tmp/OptiBurn.AppImage") };
        assert!(self_update_supported());
        match saved {
            // SAFETY: 同上。
            Some(value) => unsafe { std::env::set_var("APPIMAGE", value) },
            // SAFETY: 同上。
            None => unsafe { std::env::remove_var("APPIMAGE") },
        }
    }
}
