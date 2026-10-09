//! Windows 实现：SPTI 的 `IOCTL_SCSI_PASS_THROUGH_DIRECT`（`ntddscsi.h`）。
//!
//! 只借用 windows-sys 的 `CreateFileW`/`DeviceIoControl`/`CloseHandle` 与句柄类型。
//! IOCTL 码、枚举值与 `SCSI_PASS_THROUGH_DIRECT` 布局按头文件在本地定义（windows-sys
//! 未导出这些符号，见 ADR-0005）。

use std::ffi::c_void;
use std::os::windows::ffi::OsStrExt;
use std::time::Duration;

use windows_sys::Win32::Foundation::{
    CloseHandle, GENERIC_READ, GENERIC_WRITE, GetLastError, HANDLE, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE, GetDriveTypeW,
    GetLogicalDrives, OPEN_EXISTING,
};
use windows_sys::Win32::System::IO::DeviceIoControl;

use crate::{Completion, Direction, MAX_CDB_LEN, MAX_SENSE_LEN, ScsiTransport, TransportError};

/// `ntddscsi.h`: CTL_CODE(IOCTL_SCSI_BASE, 0x0405, METHOD_BUFFERED, FILE_READ_ACCESS|FILE_WRITE_ACCESS)。
const IOCTL_SCSI_PASS_THROUGH_DIRECT: u32 = 0x0004_D014;
/// `ntddscsi.h` 的 `SCSI_IOCTL_DATA_UNSPECIFIED`：无数据阶段。
const SCSI_IOCTL_DATA_UNSPECIFIED: u8 = 2;
/// `ntddscsi.h` 的 `SCSI_IOCTL_DATA_OUT`：数据从主机发往设备。
const SCSI_IOCTL_DATA_OUT: u8 = 0;
/// `ntddscsi.h` 的 `SCSI_IOCTL_DATA_IN`：数据从设备发回主机。
const SCSI_IOCTL_DATA_IN: u8 = 1;

/// `winerror.h`（`GetLastError`）的错误码：都表示“这个设备路径打不开”。
const ERROR_FILE_NOT_FOUND: u32 = 2;
const ERROR_PATH_NOT_FOUND: u32 = 3;
const ERROR_INVALID_NAME: u32 = 123;
const ERROR_BAD_PATHNAME: u32 = 161;
/// `winerror.h` 的错误码：设备不支持该 IOCTL。
const ERROR_INVALID_FUNCTION: u32 = 1;
const ERROR_NOT_SUPPORTED: u32 = 50;
/// `winbase.h` 的 `DRIVE_CDROM`：`GetDriveTypeW` 判定介质为光驱。windows-sys 把它
/// 放在未启用的 `Win32_System_WindowsProgramming` feature 里，故按头文件本地定义。
const DRIVE_CDROM: u32 = 5;

/// `ntddscsi.h` 的 `_SCSI_PASS_THROUGH_DIRECT`。
#[repr(C)]
struct ScsiPassThroughDirect {
    length: u16,
    scsi_status: u8,
    path_id: u8,
    target_id: u8,
    lun: u8,
    cdb_length: u8,
    sense_info_length: u8,
    data_in: u8,
    data_transfer_length: u32,
    time_out_value: u32,
    data_buffer: *mut c_void,
    sense_info_offset: u32,
    cdb: [u8; MAX_CDB_LEN],
}

/// SPTD 与紧跟其后的 sense 缓冲区。`SenseInfoOffset` 指向 `sense`。
#[repr(C)]
struct SptdWithBuffer {
    sptd: ScsiPassThroughDirect,
    sense: [u8; MAX_SENSE_LEN],
}

/// Windows 上的一条 SCSI 通道：持有 `CreateFileW` 打开的句柄。
pub struct WindowsSpti {
    handle: HANDLE,
    path: String,
}

impl WindowsSpti {
    /// 打开设备句柄。找不到设备时返回 [`TransportError::NotFound`]。
    pub fn open(device: &str) -> Result<Self, TransportError> {
        let name = device_name(device);
        let wide: Vec<u16> = std::ffi::OsStr::new(&name)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();

        // SAFETY: wide 是 NUL 结尾的 UTF-16 缓冲区。其余参数按头文件语义给常量。
        let handle = unsafe {
            CreateFileW(
                wide.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                std::ptr::null(),
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL,
                std::ptr::null_mut(),
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            return Err(map_open_error(device));
        }
        Ok(Self {
            handle,
            path: device.to_string(),
        })
    }

    /// 组装 SPTD 与紧随其后的 sense 缓冲区。`data` 的指针由调用方保证在 ioctl 期间有效。
    fn build_request(
        cdb: &[u8],
        cdb_buf: [u8; MAX_CDB_LEN],
        dir: Direction,
        data: &mut [u8],
        timeout: Duration,
    ) -> SptdWithBuffer {
        SptdWithBuffer {
            sptd: ScsiPassThroughDirect {
                length: std::mem::size_of::<ScsiPassThroughDirect>() as u16,
                scsi_status: 0,
                path_id: 0,
                target_id: 0,
                lun: 0,
                cdb_length: cdb.len() as u8,
                sense_info_length: MAX_SENSE_LEN as u8,
                data_in: match dir {
                    Direction::None => SCSI_IOCTL_DATA_UNSPECIFIED,
                    Direction::ToDevice => SCSI_IOCTL_DATA_OUT,
                    Direction::FromDevice => SCSI_IOCTL_DATA_IN,
                },
                data_transfer_length: data.len() as u32,
                // SPTD 的 TimeOutValue 单位是秒，与 SG_IO 的毫秒不同。
                time_out_value: timeout.as_secs().clamp(1, u64::from(u32::MAX)) as u32,
                data_buffer: if data.is_empty() {
                    std::ptr::null_mut()
                } else {
                    data.as_mut_ptr().cast()
                },
                sense_info_offset: std::mem::size_of::<ScsiPassThroughDirect>() as u32,
                cdb: cdb_buf,
            },
            sense: [0u8; MAX_SENSE_LEN],
        }
    }

    /// 把 SPTD 的返回结果翻译成 [`Completion`]。状态字节非 0 即失败。
    fn completion(cdb: &[u8], buf: &SptdWithBuffer) -> Result<Completion, TransportError> {
        // SPTD 不回传已写入的 sense 长度，只能裁掉尾部填充的 0。
        let sense_end = buf.sense.iter().rposition(|b| *b != 0).map_or(0, |i| i + 1);
        let sense = buf.sense[..sense_end].to_vec();

        if buf.sptd.scsi_status != 0 {
            return Err(TransportError::CommandFailed {
                cdb: cdb.to_vec(),
                scsi_status: buf.sptd.scsi_status,
                sense,
            });
        }

        Ok(Completion {
            scsi_status: buf.sptd.scsi_status,
            sense,
            // SCSI_PASS_THROUGH_DIRECT 没有 residual 字段。
            residual: 0,
        })
    }
}

impl Drop for WindowsSpti {
    fn drop(&mut self) {
        // SAFETY: handle 来自 CreateFileW 且只在 drop 里关闭一次。
        unsafe {
            CloseHandle(self.handle);
        }
    }
}

impl ScsiTransport for WindowsSpti {
    fn issue(
        &mut self,
        cdb: &[u8],
        dir: Direction,
        data: &mut [u8],
        timeout: Duration,
    ) -> Result<Completion, TransportError> {
        if cdb.len() > MAX_CDB_LEN {
            return Err(TransportError::CdbTooLong(cdb.len()));
        }

        let mut cdb_buf = [0u8; MAX_CDB_LEN];
        cdb_buf[..cdb.len()].copy_from_slice(cdb);

        let mut buf = Self::build_request(cdb, cdb_buf, dir, data, timeout);

        let buf_size = std::mem::size_of::<SptdWithBuffer>() as u32;
        let mut returned = 0u32;
        // SAFETY: 该 IOCTL 是 METHOD_BUFFERED，输入输出缓冲区允许重叠，故两处都传 buf。
        // buf 在调用期间存活，数据指针指向调用方的 data。
        let ok = unsafe {
            DeviceIoControl(
                self.handle,
                IOCTL_SCSI_PASS_THROUGH_DIRECT,
                (&raw const buf).cast(),
                buf_size,
                (&raw mut buf).cast(),
                buf_size,
                &mut returned,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            return Err(map_io_error());
        }

        Self::completion(cdb, &buf)
    }

    fn device_path(&self) -> &str {
        &self.path
    }
}

/// `E:` 这类设备路径要写成 SPTI 要求的 `\\.\E:`。已带前缀或其它形式原样返回。
fn device_name(device: &str) -> String {
    let mut chars = device.chars();
    let is_drive = matches!(
        (chars.next(), chars.next()),
        (Some(letter), Some(':')) if letter.is_ascii_alphabetic()
    );
    if is_drive && !device.starts_with(r"\\.\") {
        format!(r"\\.\{device}")
    } else {
        device.to_string()
    }
}

/// 打开阶段的 `GetLastError` 分类。
fn map_open_error(device: &str) -> TransportError {
    match unsafe { GetLastError() } {
        ERROR_FILE_NOT_FOUND | ERROR_PATH_NOT_FOUND | ERROR_INVALID_NAME | ERROR_BAD_PATHNAME => {
            TransportError::NotFound(device.to_string())
        }
        ERROR_INVALID_FUNCTION | ERROR_NOT_SUPPORTED => TransportError::Unsupported,
        err => TransportError::Io(std::io::Error::from_raw_os_error(err as i32)),
    }
}

/// `DeviceIoControl` 失败时的 `GetLastError` 分类。
fn map_io_error() -> TransportError {
    match unsafe { GetLastError() } {
        ERROR_INVALID_FUNCTION | ERROR_NOT_SUPPORTED => TransportError::Unsupported,
        err => TransportError::Io(std::io::Error::from_raw_os_error(err as i32)),
    }
}

/// 枚举光驱盘符：从 [`GetLogicalDrives`] 的位掩码里挑出
/// [`GetDriveTypeW`] 判为 `DRIVE_CDROM` 的盘符，产出升序的 `X:` 形式路径。
pub(crate) fn list_optical_devices() -> Vec<String> {
    // 位掩码第 i 位代表盘符 `A + i` 是否存在；函数失败返回 0，按“无任何盘符”处理。
    let bitmask = unsafe { GetLogicalDrives() };
    device_letters(bitmask)
        .into_iter()
        // 只有 `GetDriveTypeW` 判为 `DRIVE_CDROM` 的才是光驱；硬盘、U 盘、网络盘一律排除。
        .filter(|letter| unsafe { GetDriveTypeW(drive_wide(letter).as_ptr()) } == DRIVE_CDROM)
        .collect()
}

/// Windows 上恒为 `None`：卷占用由 SPTI 打开设备时的错误表达，没有 Linux 式的
/// 挂载点可查。等设备枚举落地、有真机可验证时再按打开错误分类补齐（ADR-0005）。
pub(super) fn mounted_at(_device: &str) -> Option<std::path::PathBuf> {
    None
}

/// 位掩码第 i 位对应盘符 `A + i`（第 0 位是 `A:`），按 A 到 Z 升序产出 `X:` 形式。
fn device_letters(bitmask: u32) -> Vec<String> {
    (0..26)
        .filter(|&i| bitmask & (1 << i) != 0)
        .map(|i| format!("{}:", (b'A' + i as u8) as char))
        .collect()
}

/// 把 `device_letters` 产出的 `X:` 拼成 `GetDriveTypeW` 要求的根路径形式
/// `X:\` 的 UTF-16 编码（字母、`:`、`\`、NUL）。
fn drive_wide(letter: &str) -> [u16; 4] {
    let bytes = letter.as_bytes();
    [u16::from(bytes[0]), u16::from(b':'), u16::from(b'\\'), 0]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sptd_with_buffer_layout_matches_c() {
        assert_eq!(std::mem::size_of::<ScsiPassThroughDirect>(), 56);
        assert_eq!(std::mem::size_of::<SptdWithBuffer>(), 88);
    }

    #[test]
    fn drive_letters_get_device_namespace_prefix() {
        assert_eq!(device_name("E:"), r"\\.\E:");
        assert_eq!(device_name(r"\\.\E:"), r"\\.\E:");
        assert_eq!(device_name("/nonexistent"), "/nonexistent");
    }

    #[test]
    fn device_letters_maps_bits_to_ascending_letters() {
        assert!(device_letters(0).is_empty());
        assert_eq!(device_letters(1 << 4), ["E:"]);
        let all = device_letters(u32::MAX);
        assert_eq!(all.len(), 26);
        assert_eq!(all.first().map(String::as_str), Some("A:"));
        assert_eq!(all.last().map(String::as_str), Some("Z:"));
        let mut sorted = all.clone();
        sorted.sort();
        assert_eq!(all, sorted);
    }

    #[test]
    fn drive_wide_builds_root_path_utf16() {
        assert_eq!(
            drive_wide("E:"),
            [b'E' as u16, b':' as u16, b'\\' as u16, 0]
        );
    }
}
