/// Windows WeChat 进程内存密钥扫描器
///
/// 使用 Windows API：
/// - CreateToolhelp32Snapshot + Process32Next: 枚举进程找 Weixin.exe
/// - OpenProcess: 获取进程句柄（需要 PROCESS_VM_READ | PROCESS_QUERY_INFORMATION）
/// - VirtualQueryEx: 枚举内存区域
/// - ReadProcessMemory: 读取内存内容
use anyhow::{bail, Result};
use regex::Regex;
use std::collections::HashSet;
use std::path::Path;
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::System::Diagnostics::Debug::ReadProcessMemory;
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32First, Process32Next, PROCESSENTRY32, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::Memory::{VirtualQueryEx, MEMORY_BASIC_INFORMATION, MEM_COMMIT};
use windows::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_INFORMATION, PROCESS_VM_READ};

use super::{
    collect_db_salts, collect_salt_adjacent_keys, decode_salt_hex, is_writable_readable_page,
    match_raw_keys, scan_key_patterns, KeyEntry, MAX_PATTERN_BYTES,
};

const CHUNK_SIZE: usize = 2 * 1024 * 1024;
const MAX_REGION_SIZE: usize = 0x1000_0000;

// WeChat 4.1.x stores per-database key literals in a
// `com.Tencent.WCDB.Config.Cipher` object. The object layout and XOR mask are
// derived from this Apache-2.0 reference implementation (pinned source):
// https://github.com/fanyuantaier/wechatauto-replica/blob/f4fdf73621f667bb68df221c4301be7222770bf6/wechatauto/db.py#L46-L51
// https://github.com/fanyuantaier/wechatauto-replica/blob/f4fdf73621f667bb68df221c4301be7222770bf6/wechatauto/db.py#L1075-L1153
const CONFIG_CIPHER_NAME: &[u8] = b"com.Tencent.WCDB.Config.Cipher";
const CONFIG_XOR_MASK: [u8; 32] = [
    0xd2, 0xc7, 0x44, 0x24, 0x58, 0x02, 0x00, 0x00, 0x00, 0x48, 0x89, 0x44, 0x24, 0x50, 0x48, 0x8b,
    0x45, 0x00, 0x48, 0x84, 0x4c, 0x24, 0x48, 0x48, 0x89, 0x44, 0x25, 0x40, 0x48, 0x58, 0x4c, 0x24,
];

/// 查找 Weixin.exe 进程 PID
pub(crate) fn find_wechat_pid() -> Option<u32> {
    find_wechat_pids().into_iter().next()
}

fn find_wechat_pids() -> Vec<u32> {
    // SAFETY: CreateToolhelp32Snapshot 标准 Windows API
    let Ok(snap) = (unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) }) else {
        return Vec::new();
    };

    let mut entry = PROCESSENTRY32 {
        dwSize: std::mem::size_of::<PROCESSENTRY32>() as u32,
        ..Default::default()
    };
    let mut pids = Vec::new();

    // SAFETY: Process32First/Process32Next 标准快照遍历
    unsafe {
        if Process32First(snap, &mut entry).is_err() {
            let _ = CloseHandle(snap);
            return pids;
        }
        loop {
            let name =
                std::ffi::CStr::from_ptr(entry.szExeFile.as_ptr() as *const i8).to_string_lossy();
            if name.eq_ignore_ascii_case("Weixin.exe") {
                pids.push(entry.th32ProcessID);
            }
            if Process32Next(snap, &mut entry).is_err() {
                break;
            }
        }
        let _ = CloseHandle(snap);
    }
    pids
}

pub fn scan_keys(db_dir: &Path) -> Result<Vec<KeyEntry>> {
    let pids = find_wechat_pids();
    if pids.is_empty() {
        bail!("找不到 Weixin.exe 进程，请确认微信正在运行");
    }
    eprintln!("WeChat PIDs: {:?}", pids);

    let db_salts = collect_db_salts(db_dir);
    let salt_bytes: Vec<[u8; 16]> = db_salts
        .iter()
        .filter_map(|(salt, _)| decode_salt_hex(salt))
        .collect();
    eprintln!("找到 {} 个加密数据库", db_salts.len());

    eprintln!("扫描 Config.Cipher 对象...");
    let mut raw_keys = Vec::new();
    let mut seen_keys = HashSet::new();
    let mut opened = 0usize;
    for pid in &pids {
        // SAFETY: OpenProcess 仅请求只读查询权限。
        let Ok(process) =
            (unsafe { OpenProcess(PROCESS_VM_READ | PROCESS_QUERY_INFORMATION, false, *pid) })
        else {
            continue;
        };
        opened += 1;
        let config_keys = scan_config_cipher_keys(process);
        for key in config_keys {
            if seen_keys.insert(key.clone()) {
                raw_keys.push((key, String::new()));
            }
        }
        unsafe {
            let _ = CloseHandle(process);
        }
    }
    if opened == 0 {
        bail!("OpenProcess 失败，请以管理员权限运行");
    }

    // 兼容旧版 WCDB：主进程中仍可能保留 x'key+salt' 或 salt 邻近 key。
    if let Some(pid) = pids.first() {
        if let Ok(process) =
            unsafe { OpenProcess(PROCESS_VM_READ | PROCESS_QUERY_INFORMATION, false, *pid) }
        {
            let legacy = scan_memory(process, &salt_bytes)?;
            for (key, salt) in legacy {
                if seen_keys.insert(key.clone()) {
                    raw_keys.push((key, salt));
                }
            }
            unsafe {
                let _ = CloseHandle(process);
            }
        }
    }
    eprintln!("找到 {} 个去重候选密钥", raw_keys.len());

    let entries = match_raw_keys(db_dir, &raw_keys, &db_salts);
    eprintln!(
        "匹配到 {}/{} 个数据库密钥（来自 {} 个候选 key）",
        entries.len(),
        db_salts.len(),
        raw_keys.len()
    );
    Ok(entries)
}

fn scan_config_cipher_keys(process: HANDLE) -> Vec<String> {
    let mut keys = Vec::new();
    let mut seen = HashSet::new();
    let name_addresses = find_bytes(process, CONFIG_CIPHER_NAME);

    for name_address in &name_addresses {
        let mut pair = Vec::with_capacity(16);
        pair.extend_from_slice(&(*name_address as u64).to_le_bytes());
        pair.extend_from_slice(&(CONFIG_CIPHER_NAME.len() as u64).to_le_bytes());

        for pair_address in find_bytes(process, &pair) {
            let Some(node_address) = pair_address.checked_sub(0x10) else {
                continue;
            };
            let Some(node) = read_remote(process, node_address, 0x50) else {
                continue;
            };
            if read_u64(&node, 0x10) != Some(*name_address as u64)
                || read_u64(&node, 0x18) != Some(CONFIG_CIPHER_NAME.len() as u64)
            {
                continue;
            }
            let Some(config_ptr) = read_u64(&node, 0x28).map(|value| value as usize) else {
                continue;
            };
            if !(0x1_0000..0x8000_0000_0000).contains(&config_ptr) {
                continue;
            }
            let Some(object) = read_remote(process, config_ptr + 0x88, 0x28) else {
                continue;
            };
            let Some(data_ptr) = read_u64(&object, 0x08).map(|value| value as usize) else {
                continue;
            };
            let Some(data_len) = read_u64(&object, 0x10).map(|value| value as usize) else {
                continue;
            };
            if data_len == 0 || data_len > 1024 || !(0x1_0000..0x8000_0000_0000).contains(&data_ptr)
            {
                continue;
            }
            let Some(blob) = read_remote(process, data_ptr, data_len) else {
                continue;
            };
            let decoded: Vec<u8> = blob
                .iter()
                .enumerate()
                .map(|(index, value)| value ^ CONFIG_XOR_MASK[index % CONFIG_XOR_MASK.len()])
                .collect();
            collect_hex_literal_keys(&decoded, &mut keys, &mut seen);
        }
    }
    keys
}

fn collect_hex_literal_keys(decoded: &[u8], keys: &mut Vec<String>, seen: &mut HashSet<String>) {
    let Ok(pattern) = Regex::new(r"(?i)x'([0-9a-f]{64,192})'") else {
        return;
    };
    let text = String::from_utf8_lossy(decoded);
    for captures in pattern.captures_iter(&text) {
        let Some(run_match) = captures.get(1) else {
            continue;
        };
        let run = run_match.as_str();
        let mut starts = vec![0usize];
        if run.len() > 96 {
            starts.extend((0..=run.len() - 64).step_by(32));
            starts.push(run.len() - 64);
        }
        starts.sort_unstable();
        starts.dedup();
        for start in starts {
            if start + 64 > run.len() {
                continue;
            }
            let key = run[start..start + 64].to_ascii_lowercase();
            if probable_key_hex(&key) && seen.insert(key.clone()) {
                keys.push(key);
            }
        }
    }
}

fn probable_key_hex(key: &str) -> bool {
    if key.len() != 64 {
        return false;
    }
    let mut unique = HashSet::new();
    for pair in key.as_bytes().chunks_exact(2) {
        let Ok(pair) = std::str::from_utf8(pair) else {
            return false;
        };
        let Ok(value) = u8::from_str_radix(pair, 16) else {
            return false;
        };
        unique.insert(value);
    }
    unique.len() >= 15
}

fn read_u64(bytes: &[u8], offset: usize) -> Option<u64> {
    let value: [u8; 8] = bytes.get(offset..offset + 8)?.try_into().ok()?;
    Some(u64::from_le_bytes(value))
}

fn read_remote(process: HANDLE, address: usize, size: usize) -> Option<Vec<u8>> {
    let mut bytes = vec![0u8; size];
    let mut bytes_read = 0usize;
    let ok = unsafe {
        ReadProcessMemory(
            process,
            address as *const _,
            bytes.as_mut_ptr() as *mut _,
            size,
            Some(&mut bytes_read),
        )
        .is_ok()
    };
    if !ok || bytes_read != size {
        return None;
    }
    Some(bytes)
}

fn find_bytes(process: HANDLE, needle: &[u8]) -> Vec<usize> {
    let mut hits = Vec::new();
    if needle.is_empty() {
        return hits;
    }
    let mut address = 0usize;
    loop {
        let mut mbi = MEMORY_BASIC_INFORMATION::default();
        let read = unsafe {
            VirtualQueryEx(
                process,
                Some(address as *const _),
                &mut mbi,
                std::mem::size_of::<MEMORY_BASIC_INFORMATION>(),
            )
        };
        if read == 0 {
            break;
        }
        let base = mbi.BaseAddress as usize;
        let region_size = mbi.RegionSize;
        if mbi.State == MEM_COMMIT
            && region_size > 0
            && region_size < MAX_REGION_SIZE
            && is_readable_page(mbi.Protect.0)
        {
            scan_region_for_needle(process, base, region_size, needle, &mut hits);
        }
        address = base.saturating_add(region_size);
        if address == 0 {
            break;
        }
    }
    hits
}

fn scan_region_for_needle(
    process: HANDLE,
    base: usize,
    size: usize,
    needle: &[u8],
    hits: &mut Vec<usize>,
) {
    let overlap = needle.len().saturating_sub(1);
    let mut offset = 0usize;
    while offset < size {
        let chunk_size = std::cmp::min(CHUNK_SIZE, size - offset);
        let address = base + offset;
        let mut buffer = vec![0u8; chunk_size];
        let mut bytes_read = 0usize;
        let ok = unsafe {
            ReadProcessMemory(
                process,
                address as *const _,
                buffer.as_mut_ptr() as *mut _,
                chunk_size,
                Some(&mut bytes_read),
            )
            .is_ok()
        };
        if ok && bytes_read >= needle.len() {
            buffer.truncate(bytes_read);
            let mut start = 0usize;
            while start + needle.len() <= buffer.len() {
                let Some(relative) = buffer[start..]
                    .windows(needle.len())
                    .position(|window| window == needle)
                else {
                    break;
                };
                let hit = start + relative;
                hits.push(address + hit);
                start = hit + 1;
            }
        }
        if chunk_size > overlap {
            offset += chunk_size - overlap;
        } else {
            offset += chunk_size;
        }
    }
}

fn is_readable_page(protect: u32) -> bool {
    const PAGE_GUARD: u32 = 0x100;
    const PAGE_NOCACHE: u32 = 0x200;
    const PAGE_WRITECOMBINE: u32 = 0x400;
    const PAGE_READONLY: u32 = 0x02;
    const PAGE_READWRITE: u32 = 0x04;
    const PAGE_WRITECOPY: u32 = 0x08;
    const PAGE_EXECUTE_READ: u32 = 0x20;
    const PAGE_EXECUTE_READWRITE: u32 = 0x40;
    const PAGE_EXECUTE_WRITECOPY: u32 = 0x80;

    if protect & PAGE_GUARD != 0 {
        return false;
    }
    let base = protect & !(PAGE_GUARD | PAGE_NOCACHE | PAGE_WRITECOMBINE);
    matches!(
        base,
        PAGE_READONLY
            | PAGE_READWRITE
            | PAGE_WRITECOPY
            | PAGE_EXECUTE_READ
            | PAGE_EXECUTE_READWRITE
            | PAGE_EXECUTE_WRITECOPY
    )
}

fn scan_memory(process: HANDLE, salts: &[[u8; 16]]) -> Result<Vec<(String, String)>> {
    let mut results: Vec<(String, String)> = Vec::new();
    let mut adjacent_keys: Vec<String> = Vec::new();
    let mut seen_adjacent = std::collections::HashSet::new();
    let mut addr: usize = 0;

    loop {
        let mut mbi = MEMORY_BASIC_INFORMATION::default();
        // SAFETY: VirtualQueryEx 枚举进程内存区域
        let ret = unsafe {
            VirtualQueryEx(
                process,
                Some(addr as *const _),
                &mut mbi,
                std::mem::size_of::<MEMORY_BASIC_INFORMATION>(),
            )
        };
        if ret == 0 {
            break;
        }

        let region_size = mbi.RegionSize;
        let base = mbi.BaseAddress as usize;

        // 只扫描已提交的可读可写页面（含 WRITECOPY / EXECUTE_*WRITE*；见
        // `is_writable_readable_page`，从 old-main #54 捞回）。
        if mbi.State == MEM_COMMIT && is_writable_readable_page(mbi.Protect.0) {
            scan_region(
                process,
                base,
                region_size,
                salts,
                &mut results,
                &mut adjacent_keys,
                &mut seen_adjacent,
            );
        }

        addr = base.saturating_add(region_size);
        if addr == 0 {
            break; // overflow
        }
    }

    // `match_raw_keys` 会把空 salt 的条目当作普通候选，并用实际数据库
    // HMAC/首页解密逐一验证；因此不会把邻近内存中的随机 32 字节写入配置。
    results.extend(adjacent_keys.into_iter().map(|key| (key, String::new())));
    Ok(results)
}

fn scan_region(
    process: HANDLE,
    base: usize,
    size: usize,
    salts: &[[u8; 16]],
    results: &mut Vec<(String, String)>,
    adjacent_keys: &mut Vec<String>,
    seen_adjacent: &mut std::collections::HashSet<String>,
) {
    let overlap = MAX_PATTERN_BYTES;
    let mut offset = 0usize;

    loop {
        if offset >= size {
            break;
        }
        let chunk_size = std::cmp::min(CHUNK_SIZE, size - offset);
        let addr = base + offset;
        let mut buf = vec![0u8; chunk_size];
        let mut bytes_read: usize = 0;

        // SAFETY: ReadProcessMemory 读取目标进程内存
        let ok = unsafe {
            ReadProcessMemory(
                process,
                addr as *const _,
                buf.as_mut_ptr() as *mut _,
                chunk_size,
                Some(&mut bytes_read),
            )
            .is_ok()
        };

        if ok && bytes_read > 0 {
            buf.truncate(bytes_read);
            scan_key_patterns(&buf, results);
            collect_salt_adjacent_keys(&buf, salts, adjacent_keys, seen_adjacent);
        }

        if chunk_size > overlap {
            offset += chunk_size - overlap;
        } else {
            offset += chunk_size;
        }
    }
}
