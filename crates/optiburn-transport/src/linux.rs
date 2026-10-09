//! Linux 实现：`/dev/sr*` 上的 `SG_IO` ioctl（`scsi/sg.h`）。
//!
//! 思路参考 vicr123/alight 的 `src/scsi/linux.rs`（该仓库无许可证，故本文件按
//! `sg_io_hdr` 的 C 布局自行编写，见 ADR-0001）。

use std::fs::{File, OpenOptions};
use std::os::fd::AsRawFd;
use std::path::PathBuf;
use std::time::Duration;

use crate::{Completion, Direction, MAX_CDB_LEN, MAX_SENSE_LEN, ScsiTransport, TransportError};

/// `SG_IO` 的 ioctl 请求码（`scsi/sg.h`）。
const SG_IO: libc::c_ulong = 0x2285;
/// `sg_io_hdr.dxfer_direction`：无数据阶段。
const SG_DXFER_NONE: i32 = -1;
/// `sg_io_hdr.dxfer_direction`：数据从主机发往设备。
const SG_DXFER_TO_DEV: i32 = -2;
/// `sg_io_hdr.dxfer_direction`：数据从设备发回主机。
const SG_DXFER_FROM_DEV: i32 = -3;

/// `sg_io_hdr` 的 C 布局（`scsi/sg.h`，64 位平台 sizeof 为 88）。
#[repr(C)]
struct SgIoHdr {
    interface_id: i32,
    dxfer_direction: i32,
    cmd_len: u8,
    mx_sb_len: u8,
    iovec_count: u16,
    dxfer_len: u32,
    dxferp: *mut core::ffi::c_void,
    cmdp: *mut u8,
    sbp: *mut u8,
    timeout: u32,
    flags: u32,
    pack_id: i32,
    usr_ptr: *mut core::ffi::c_void,
    status: u8,
    masked_status: u8,
    msg_status: u8,
    sb_len_wr: u8,
    host_status: u16,
    driver_status: u16,
    resid: i32,
    duration: u32,
    info: u32,
}

/// Linux 上的一条 SCSI 通道：持有打开的 `/dev/sr*` 句柄。
pub struct LinuxSg {
    file: File,
    path: String,
}

impl LinuxSg {
    /// 以读写方式打开设备节点。`ENOENT` 映射为 [`TransportError::NotFound`]。
    ///
    /// 必须带 `O_NONBLOCK`：内核在非阻塞打开光驱块设备时会同步做介质检查，空白盘在
    /// 写打开路径会被判成只读设备（`EROFS`）。`SG_IO` 走 ioctl，不受这个标志影响。
    /// cdrecord 等同类工具也以非阻塞方式打开。
    pub fn open(device: &str) -> Result<Self, TransportError> {
        use std::os::unix::fs::OpenOptionsExt;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(device)
            .map_err(|e| match e.kind() {
                std::io::ErrorKind::NotFound => TransportError::NotFound(device.to_string()),
                _ => TransportError::Io(e),
            })?;
        Ok(Self {
            file,
            path: device.to_string(),
        })
    }

    /// 组装 `sg_io_hdr`（`scsi/sg.h` 的字段顺序）。指针指向调用方传入的缓冲区，由调用方
    /// 保证它们在 ioctl 期间存活。
    fn build_hdr(
        cdb: &[u8],
        cdb_buf: &mut [u8; MAX_CDB_LEN],
        dir: Direction,
        data: &mut [u8],
        sense: &mut [u8; MAX_SENSE_LEN],
        timeout: Duration,
    ) -> SgIoHdr {
        SgIoHdr {
            interface_id: i32::from(b'S'),
            dxfer_direction: match dir {
                Direction::None => SG_DXFER_NONE,
                Direction::ToDevice => SG_DXFER_TO_DEV,
                Direction::FromDevice => SG_DXFER_FROM_DEV,
            },
            cmd_len: cdb.len() as u8,
            mx_sb_len: MAX_SENSE_LEN as u8,
            iovec_count: 0,
            dxfer_len: data.len() as u32,
            dxferp: if data.is_empty() {
                std::ptr::null_mut()
            } else {
                data.as_mut_ptr().cast()
            },
            cmdp: cdb_buf.as_mut_ptr(),
            sbp: sense.as_mut_ptr(),
            timeout: timeout.as_millis().min(u128::from(u32::MAX)) as u32,
            flags: 0,
            pack_id: 0,
            usr_ptr: std::ptr::null_mut(),
            status: 0,
            masked_status: 0,
            msg_status: 0,
            sb_len_wr: 0,
            host_status: 0,
            driver_status: 0,
            resid: 0,
            duration: 0,
            info: 0,
        }
    }

    /// 把 ioctl 结果翻译成 [`Completion`]。
    ///
    /// 宿主机/驱动层报错时状态字节可能仍为 0，必须一并当失败，否则上层会把超时或设备
    /// 重置误判成成功。
    fn completion(
        cdb: &[u8],
        hdr: &SgIoHdr,
        sense: &[u8; MAX_SENSE_LEN],
    ) -> Result<Completion, TransportError> {
        let sense_len = usize::from(hdr.sb_len_wr).min(MAX_SENSE_LEN);
        if hdr.status != 0 || hdr.host_status != 0 || hdr.driver_status != 0 {
            return Err(TransportError::CommandFailed {
                cdb: cdb.to_vec(),
                scsi_status: hdr.status,
                sense: sense[..sense_len].to_vec(),
            });
        }

        Ok(Completion {
            scsi_status: hdr.status,
            sense: sense[..sense_len].to_vec(),
            // resid 是“未传送的字节数”。写方向下它可能为负，那不代表有残留。
            residual: hdr.resid.max(0) as usize,
        })
    }
}

impl ScsiTransport for LinuxSg {
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
        let mut sense = [0u8; MAX_SENSE_LEN];

        let mut hdr = Self::build_hdr(cdb, &mut cdb_buf, dir, data, &mut sense, timeout);

        // SAFETY: hdr 的布局与 sg_io_hdr 一致（下方测试断言尺寸），其 cmdp/sbp/dxferp
        // 都指向本次调用内存中存活的缓冲区，ioctl 同步返回后才离开作用域。
        let rc = unsafe { libc::ioctl(self.file.as_raw_fd(), SG_IO, &mut hdr as *mut SgIoHdr) };
        if rc < 0 {
            return Err(TransportError::Io(std::io::Error::last_os_error()));
        }

        Self::completion(cdb, &hdr, &sense)
    }

    fn device_path(&self) -> &str {
        &self.path
    }
}

/// `/dev` 下形如 `sr0`、`sr12` 的光驱设备节点路径，按名字排序。
pub(super) fn optical_devices() -> Vec<String> {
    let Ok(entries) = std::fs::read_dir("/dev") else {
        return Vec::new();
    };
    let mut devices = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name();
            let name = name.to_str()?;
            let index = name.strip_prefix("sr")?;
            (!index.is_empty() && index.bytes().all(|b| b.is_ascii_digit()))
                .then(|| format!("/dev/{name}"))
        })
        .collect::<Vec<_>>();
    devices.sort();
    devices
}

/// 设备被系统挂载时返回挂载点。
///
/// 从 `/proc/self/mountinfo` 按设备的 major:minor 匹配，不依赖挂载来源怎么写
/// （`/dev/sr0`、`/dev/cdrom`、udev 符号链接都可能出现）。挂载点里的空格等字符
/// 在 mountinfo 里是八进制转义（如 `\040`），解析时还原。
pub(super) fn mounted_at(device: &str) -> Option<PathBuf> {
    use std::os::unix::fs::MetadataExt;

    let target = std::fs::metadata(device).ok()?.rdev();
    let major = libc::major(target) as u32;
    let minor = libc::minor(target) as u32;
    let mountinfo = std::fs::read_to_string("/proc/self/mountinfo").ok()?;
    find_mount_point(&mountinfo, major, minor)
}

/// 在 mountinfo 文本里查 major:minor 对应的挂载点。抽成纯函数，测试可以喂固定文本。
///
/// 每行前五列是：id、parent、major:minor、根、挂载点。其后是可选字段与“-”分隔符，
/// 这里只看前五列。
fn find_mount_point(mountinfo: &str, major: u32, minor: u32) -> Option<PathBuf> {
    for line in mountinfo.lines() {
        let mut fields = line.split(' ');
        let (Some(_id), Some(_parent), Some(majmin), Some(_root), Some(point)) = (
            fields.next(),
            fields.next(),
            fields.next(),
            fields.next(),
            fields.next(),
        ) else {
            continue;
        };
        let key = majmin
            .split_once(':')
            .and_then(|(maj, min)| Some((maj.parse::<u32>().ok()?, min.parse::<u32>().ok()?)));
        if key == Some((major, minor)) {
            return Some(PathBuf::from(unescape_mountinfo(point)));
        }
    }
    None
}

/// 还原 mountinfo 字段里的八进制转义（`\040` 空格、`\011` 制表、`\012` 换行、`\134` 反斜杠）。
fn unescape_mountinfo(field: &str) -> String {
    let bytes = field.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        // 一个合法转义占 4 字节：反斜杠加三位八进制数字。
        let is_escape = bytes[index] == b'\\' && index + 3 < bytes.len();
        if is_escape {
            let digits = &bytes[index + 1..index + 4];
            if digits.iter().all(|b| (b'0'..=b'7').contains(b)) {
                let value = u16::from(digits[0] - b'0') * 64
                    + u16::from(digits[1] - b'0') * 8
                    + u16::from(digits[2] - b'0');
                out.push(value as u8);
                index += 4;
                continue;
            }
        }
        out.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sg_io_hdr_layout_matches_c() {
        // 布局一旦漂移，ioctl 会静默读到错误字段而不是报错。
        assert_eq!(std::mem::size_of::<SgIoHdr>(), 88);
        assert_eq!(std::mem::align_of::<SgIoHdr>(), 8);
    }

    #[test]
    fn mountinfo_finds_mount_point_by_major_minor() {
        let text = "\
29 1 8:1 / / rw,relatime - ext4 /dev/sda1 rw
1129 30 11:0 / /run/media/u/我的光盘 ro,relatime - iso9660 /dev/sr0 ro
";
        assert_eq!(
            find_mount_point(text, 11, 0),
            Some(PathBuf::from("/run/media/u/我的光盘"))
        );
        assert_eq!(find_mount_point(text, 8, 1), Some(PathBuf::from("/")));
        assert_eq!(find_mount_point(text, 11, 1), None);
        // 残缺行不该让匹配提前收场。
        assert_eq!(find_mount_point("junk\n", 11, 0), None);
    }

    #[test]
    fn mountinfo_escape_is_restored() {
        let text = "1129 30 11:0 / /run/media/u/my\\040disc ro - iso9660 /dev/sr0 ro\n";
        assert_eq!(
            find_mount_point(text, 11, 0),
            Some(PathBuf::from("/run/media/u/my disc"))
        );
        assert_eq!(unescape_mountinfo("a\\134b"), "a\\b");
        assert_eq!(unescape_mountinfo("tail\\04"), "tail\\04");
    }
}
