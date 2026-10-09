//! 回读校验的本地目录对比（ADR-0010）。
//!
//! 以源目录为准做单向比较：源里的每个条目都要在盘上树里存在且内容一致。盘上多出的
//! 条目不算差异，多区段盘上还有不属于本次源目录的旧文件。对比只认目录结构、文件名
//! 与内容字节：ISO 会话里的时间戳与权限不可靠（抽回本地的 ctime 必然是新值，Rock
//! Ridge 时间戳也有舍入），把它们算作差异只会制造假警报。

use std::io::Read;
use std::path::Path;

/// 单向对比两棵目录树，返回差异描述（`lang` 传 `"zh"` 或 `"en"`），空表示一致。
///
/// `source` 是写入内容的来源（追加的源目录或抽取出的镜像树），`disc` 是盘上树抽到
/// 本地的目录。符号链接不参与对比：链接是否落盘、读取端把它还原成链接还是目标
/// 文件，取决于写入参数与抽取方式，语义不稳定，内容承诺只覆盖普通文件与目录。
pub fn compare_trees(source: &Path, disc: &Path, lang: &str) -> Vec<String> {
    let mut differences = Vec::new();
    compare_dir(source, disc, Path::new(""), lang, &mut differences);
    differences
}

/// 比较一个目录层级。`relative` 是自根起的相对路径，用于差异描述。
fn compare_dir(
    source: &Path,
    disc: &Path,
    relative: &Path,
    lang: &str,
    differences: &mut Vec<String>,
) {
    let entries = match sorted_entries(source) {
        Ok(entries) => entries,
        Err(e) => {
            differences.push(match lang {
                "zh" => format!("无法读取源目录 {}：{e}", display_path(relative)),
                _ => format!(
                    "Failed to read source directory {}: {e}",
                    display_path(relative)
                ),
            });
            return;
        }
    };
    for entry in entries {
        let name = entry.file_name();
        let child_relative = relative.join(&name);
        let disc_path = disc.join(&name);
        let Ok(kind) = entry.file_type() else {
            differences.push(match lang {
                "zh" => format!("无法读取源条目：{}", display_path(&child_relative)),
                _ => format!(
                    "Failed to read source entry: {}",
                    display_path(&child_relative)
                ),
            });
            continue;
        };
        if kind.is_symlink() {
            continue;
        }
        if kind.is_dir() {
            if !disc_path.is_dir() {
                differences.push(match lang {
                    "zh" => format!("盘上缺少目录：{}", display_path(&child_relative)),
                    _ => format!(
                        "Directory missing on disc: {}",
                        display_path(&child_relative)
                    ),
                });
                continue;
            }
            compare_dir(
                &source.join(&name),
                &disc_path,
                &child_relative,
                lang,
                differences,
            );
        } else if kind.is_file() {
            if !disc_path.is_file() {
                differences.push(match lang {
                    "zh" => format!("盘上缺少文件：{}", display_path(&child_relative)),
                    _ => format!("File missing on disc: {}", display_path(&child_relative)),
                });
                continue;
            }
            compare_file(
                &source.join(&name),
                &disc_path,
                &child_relative,
                lang,
                differences,
            );
        } else {
            differences.push(match lang {
                "zh" => format!("未支持的条目不参与比较：{}", display_path(&child_relative)),
                _ => format!(
                    "Unsupported entry skipped: {}",
                    display_path(&child_relative)
                ),
            });
        }
    }
}

/// 按大小与字节比较两个文件，把差异写进列表。
fn compare_file(
    source: &Path,
    disc: &Path,
    relative: &Path,
    lang: &str,
    differences: &mut Vec<String>,
) {
    let source_len = match std::fs::metadata(source) {
        Ok(metadata) => metadata.len(),
        Err(e) => {
            differences.push(match lang {
                "zh" => format!("无法读取源文件 {}：{e}", display_path(relative)),
                _ => format!("Failed to stat source file {}: {e}", display_path(relative)),
            });
            return;
        }
    };
    let disc_len = match std::fs::metadata(disc) {
        Ok(metadata) => metadata.len(),
        Err(e) => {
            differences.push(match lang {
                "zh" => format!("无法读取盘上文件 {}：{e}", display_path(relative)),
                _ => format!("Failed to stat disc file {}: {e}", display_path(relative)),
            });
            return;
        }
    };
    if source_len != disc_len {
        differences.push(match lang {
            "zh" => format!(
                "大小不一致：{}（盘上 {disc_len} 字节，源 {source_len} 字节）",
                display_path(relative)
            ),
            _ => format!(
                "Size mismatch: {} ({disc_len} bytes on disc, {source_len} bytes in source)",
                display_path(relative)
            ),
        });
        return;
    }
    match files_equal(source, disc) {
        Ok(true) => {}
        Ok(false) => differences.push(match lang {
            "zh" => format!("内容不一致：{}", display_path(relative)),
            _ => format!("Content mismatch: {}", display_path(relative)),
        }),
        Err(e) => differences.push(match lang {
            "zh" => format!("比较失败：{}：{e}", display_path(relative)),
            _ => format!("Comparison failed for {}: {e}", display_path(relative)),
        }),
    }
}

/// 逐块比较两个文件的内容。
fn files_equal(left: &Path, right: &Path) -> std::io::Result<bool> {
    let mut left = std::fs::File::open(left)?;
    let mut right = std::fs::File::open(right)?;
    let mut left_buf = vec![0u8; 64 * 1024];
    let mut right_buf = vec![0u8; 64 * 1024];
    loop {
        let left_read = read_full(&mut left, &mut left_buf)?;
        let right_read = read_full(&mut right, &mut right_buf)?;
        if left_read != right_read || left_buf[..left_read] != right_buf[..right_read] {
            return Ok(false);
        }
        if left_read == 0 {
            return Ok(true);
        }
    }
}

/// 尽量读满缓冲区，返回实际读到的字节数（`Read::read` 允许短读）。
fn read_full(file: &mut std::fs::File, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut filled = 0;
    while filled < buf.len() {
        let read = file.read(&mut buf[filled..])?;
        if read == 0 {
            break;
        }
        filled += read;
    }
    Ok(filled)
}

/// 目录条目按文件名排序，保证差异列表顺序稳定、测试可断言。
fn sorted_entries(dir: &Path) -> std::io::Result<Vec<std::fs::DirEntry>> {
    let mut entries = std::fs::read_dir(dir)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(|entry| entry.file_name());
    Ok(entries)
}

/// 差异描述里的路径：根目录用 `/`，其余用相对路径。
fn display_path(relative: &Path) -> String {
    if relative.as_os_str().is_empty() {
        "/".to_string()
    } else {
        relative.display().to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// 每个测试一个独立临时目录：进程号加计数器，避免并发测试相互踩。
    fn temp_dir(tag: &str) -> PathBuf {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "optiburn-verify-test-{}-{tag}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    fn write(path: &Path, content: &[u8]) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create parent dir");
        }
        std::fs::write(path, content).expect("write file");
    }

    #[test]
    fn identical_trees_have_no_differences() {
        let source = temp_dir("same-src");
        let disc = temp_dir("same-disc");
        write(&source.join("a.txt"), b"hello");
        write(&disc.join("a.txt"), b"hello");
        write(&source.join("sub/b.bin"), &[0u8, 1, 2, 3]);
        write(&disc.join("sub/b.bin"), &[0u8, 1, 2, 3]);
        assert_eq!(compare_trees(&source, &disc, "zh"), Vec::<String>::new());
        let _ = std::fs::remove_dir_all(&source);
        let _ = std::fs::remove_dir_all(&disc);
    }

    #[test]
    fn missing_modified_and_extra_files_are_classified() {
        let source = temp_dir("diff-src");
        let disc = temp_dir("diff-disc");
        write(&source.join("gone.txt"), b"x");
        write(&source.join("edited.txt"), b"aaaa");
        write(&disc.join("edited.txt"), b"aaab");
        write(&source.join("bigger.txt"), b"123456");
        write(&disc.join("bigger.txt"), b"123");
        write(&source.join("dir/deep.txt"), b"y");
        // 盘上多出的条目不算差异：多区段盘上还有旧文件。
        write(&disc.join("old-session.txt"), b"z");
        let differences = compare_trees(&source, &disc, "zh");
        assert_eq!(differences.len(), 4, "{differences:?}");
        assert!(
            differences
                .iter()
                .any(|d| d.contains("盘上缺少文件：gone.txt")),
            "{differences:?}"
        );
        assert!(
            differences
                .iter()
                .any(|d| d.contains("内容不一致：edited.txt")),
            "{differences:?}"
        );
        assert!(
            differences
                .iter()
                .any(|d| d.contains("大小不一致：bigger.txt")),
            "{differences:?}"
        );
        assert!(
            differences.iter().any(|d| d.contains("盘上缺少目录：dir")),
            "{differences:?}"
        );
        assert!(
            !differences.iter().any(|d| d.contains("old-session")),
            "盘上多出的文件不该出现在差异里：{differences:?}"
        );
        // 英文清单同样按语言输出。
        let differences = compare_trees(&source, &disc, "en");
        assert!(
            differences
                .iter()
                .any(|d| d.contains("File missing on disc: gone.txt")),
            "{differences:?}"
        );
        let _ = std::fs::remove_dir_all(&source);
        let _ = std::fs::remove_dir_all(&disc);
    }

    #[test]
    fn empty_source_matches_any_disc_tree() {
        let source = temp_dir("empty-src");
        let disc = temp_dir("empty-disc");
        write(&disc.join("something.bin"), b"data");
        assert_eq!(compare_trees(&source, &disc, "zh"), Vec::<String>::new());
        let _ = std::fs::remove_dir_all(&source);
        let _ = std::fs::remove_dir_all(&disc);
    }
}
