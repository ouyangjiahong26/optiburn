//! 主命名空间只提供兼容短名，转换后重名时分配编号，保留 Joliet 路径。

use std::collections::BTreeSet;

use super::PlanDir;

pub(super) fn assign_primary_ids(dir: &mut PlanDir) {
    let mut entries: Vec<_> = dir
        .dirs
        .iter_mut()
        .map(|child| (child.name.as_str(), &mut child.primary_id))
        .chain(
            dir.files
                .iter_mut()
                .map(|file| (file.name.as_str(), &mut file.primary_id)),
        )
        .collect();
    entries.sort_by(|left, right| left.0.cmp(right.0));
    // 先保留所有自然短名，编号不能占走尚未遍历到的条目名称。
    let mut reserved: BTreeSet<Vec<u8>> = entries.iter().map(|(_, id)| id.to_vec()).collect();
    let mut used = BTreeSet::new();
    for (_, id) in entries {
        if used.insert(id.to_vec()) {
            continue;
        }
        let mut number = 1;
        loop {
            let candidate = primary_alias(id, number);
            if reserved.insert(candidate.clone()) {
                used.insert(candidate.clone());
                *id = candidate;
                break;
            }
            number += 1;
        }
    }
    for child in &mut dir.dirs {
        assign_primary_ids(child);
    }
}

fn primary_alias(id: &[u8], number: usize) -> Vec<u8> {
    let suffix = format!("_{number}");
    let (name, version, limit) = match id.strip_suffix(b";1") {
        Some(name) => (name, b";1".as_slice(), 30usize),
        None => (id, b"".as_slice(), 31usize),
    };
    let (stem, extension) = match name.iter().rposition(|byte| *byte == b'.') {
        Some(dot) => (&name[..dot], &name[dot..]),
        None => (name, b"".as_slice()),
    };
    // 极长扩展名也要给编号留位置，文件上限不含版本后缀，目录上限为 31 字节。
    let extension = &extension[..extension.len().min(limit - suffix.len() - 1)];
    let stem = &stem[..stem.len().min(limit - extension.len() - suffix.len())];
    [stem, suffix.as_bytes(), extension, version].concat()
}
