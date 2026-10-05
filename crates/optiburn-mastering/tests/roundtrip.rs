//! 与参考实现 xorriso 对拍：本 crate 写出的镜像，xorriso 必须能解出同样的文件与字节。
//!
//! 这是“镜像能被参考工具读”这件事唯一的真实验证（hadris-cd 自己的测试只用它的
//! reader 回读，没有第三方消费者）。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use optiburn_mastering::{DiscProfile, ImageSpec, build_image};

/// 用 xorriso 把镜像解到 `dest`，成功返回 true。
///
/// xorriso 不在 PATH 上时打印 SKIP 并返回 false：没有参考实现可用，对拍无法进行。
fn extract_with_xorriso(iso: &Path, dest: &Path) -> bool {
    let output = match Command::new("xorriso")
        .args(["-osirrox", "on", "-indev"])
        .arg(iso)
        .arg("-extract")
        .arg("/")
        .arg(dest)
        .output()
    {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            eprintln!("SKIP: xorriso 不在 PATH 上，镜像未与参考实现对拍");
            return false;
        }
        Err(e) => panic!("启动 xorriso 失败：{e}"),
        Ok(output) => output,
    };
    assert!(
        output.status.success(),
        "xorriso 解镜像失败（{}）：\n{}\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    true
}

#[test]
fn xorriso_reads_back_every_file_byte_for_byte() {
    let tmp = TempDir::new();
    let src = tmp.path().join("src");
    let dest = tmp.path().join("dest");
    std::fs::create_dir_all(&dest).unwrap();
    write_fixture(&src);

    let iso = tmp.path().join("out.iso");
    let info = build_image(
        &src,
        &iso,
        &ImageSpec {
            profile: DiscProfile::Dvd,
            volume_id: "ROUNDTRIP".to_string(),
            joliet: true,
        },
    )
    .unwrap();
    assert!(info.bytes > 0);
    assert!(info.sectors > 0);
    if !extract_with_xorriso(&iso, &dest) {
        return;
    }

    let expected = snapshot(&src);
    let actual = snapshot(&dest);
    assert_eq!(
        actual.keys().collect::<Vec<_>>(),
        expected.keys().collect::<Vec<_>>(),
        "文件清单不一致（同名中文文件名必须原样保留）"
    );
    assert_eq!(actual, expected, "文件内容与源目录不一致");
    assert!(actual.contains_key("说明.txt"), "中文文件名丢了");
}

/// 源目录：中文文件名、两级子目录、空文件、全字节二进制文件。
fn write_fixture(root: &Path) {
    std::fs::create_dir_all(root.join("photos/2026")).unwrap();
    std::fs::write(root.join("说明.txt"), "中文文件名与内容\n").unwrap();
    std::fs::write(root.join("readme.txt"), b"hello optiburn\n").unwrap();
    std::fs::write(root.join("empty.txt"), b"").unwrap();
    std::fs::write(
        root.join("photos/cover.bin"),
        (0u16..=255).map(|b| b as u8).collect::<Vec<u8>>(),
    )
    .unwrap();
    std::fs::write(root.join("photos/2026/deep.bin"), [0x00, 0xff, 0x7f, 0x80]).unwrap();
}

/// 递归读出目录里所有文件的相对路径与内容，按路径排序。
fn snapshot(root: &Path) -> BTreeMap<String, Vec<u8>> {
    fn walk(dir: &Path, prefix: &str, out: &mut BTreeMap<String, Vec<u8>>) {
        let mut entries = std::fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap())
            .collect::<Vec<_>>();
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            let name = entry.file_name().to_string_lossy().into_owned();
            let relative = match prefix.is_empty() {
                true => name,
                false => format!("{prefix}/{name}"),
            };
            if entry.file_type().unwrap().is_dir() {
                walk(&entry.path(), &relative, out);
            } else {
                out.insert(relative, std::fs::read(entry.path()).unwrap());
            }
        }
    }

    let mut files = BTreeMap::new();
    walk(root, "", &mut files);
    files
}

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("optiburn-roundtrip-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("create temp dir");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
