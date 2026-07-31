//! 截图元数据 sidecar：`<workspace>/screenshots/index.json`。
//!
//! 只记「文件本身之外」的信息（当前是收藏标记，`tags` 预留给后续标签能力），
//! 以**文件名**为 key —— 不用绝对路径，整个目录搬家后索引依旧有效；也不用改文件名
//! / 挪子目录的方案，避免缩略图 URL、复制、删除等一串路径引用跟着变。
//!
//! 读写策略：全量读 → 改 → 全量写（截图条目量级很小，无需增量）。索引缺失/损坏一律
//! 按空索引处理，绝不因为 sidecar 坏了就让画廊打不开。**改动一律走 `set_starred` /
//! `forget`**，它们在 `INDEX_LOCK` 内完成整个读改写，避免并发覆盖（见该常量注释）。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

/// 单张截图的元数据。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ItemMeta {
    /// 收藏标记：true = 永久保留，自动清理不会碰它。
    #[serde(default)]
    pub starred: bool,
    /// 标签（当前未在 UI 暴露，先占好位置，后续加标签不必改文件结构）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
}

/// index.json 的整体结构（外层包一个对象，将来加全局字段不破坏兼容）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MetaIndex {
    /// 文件名 → 元数据。用 BTreeMap 让落盘顺序稳定，diff 友好。
    #[serde(default)]
    pub items: BTreeMap<String, ItemMeta>,
}

impl MetaIndex {
    /// 该文件名是否已收藏（无记录 = 未收藏）。
    pub fn is_starred(&self, name: &str) -> bool {
        self.items.get(name).map(|m| m.starred).unwrap_or(false)
    }
}

/// 索引读改写的串行锁。
///
/// Tauri 命令跑在线程池上，用户连点两张图的星标会并发进来；若各自「读全量 → 只改自己
/// 那条 → 写全量」，后写的会把先写的改动整个盖掉（表现为收藏点了没生效），极端情况下
/// 两个写还可能交错出半截 JSON。桌面端只有一个 workspace，用一把进程内全局锁最省事。
static INDEX_LOCK: Mutex<()> = Mutex::new(());

/// 索引文件路径。
pub fn index_path(workspace: &Path) -> PathBuf {
    super::output::screenshots_dir(workspace).join("index.json")
}

/// 读索引。文件不存在 / 解析失败都返回空索引（sidecar 坏掉不该拖垮画廊）。
pub fn load(workspace: &Path) -> MetaIndex {
    let path = index_path(workspace);
    match std::fs::read_to_string(&path) {
        Ok(s) => serde_json::from_str(&s).unwrap_or_else(|e| {
            log::warn!(target: "screenshot", "截图索引解析失败，按空索引处理: {e}");
            MetaIndex::default()
        }),
        Err(_) => MetaIndex::default(),
    }
}

/// 写索引（目录不存在则创建）。
pub fn save(workspace: &Path, index: &MetaIndex) -> Result<(), String> {
    let path = index_path(workspace);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("创建截图目录失败: {e}"))?;
    }
    let json = serde_json::to_string_pretty(index).map_err(|e| e.to_string())?;
    std::fs::write(&path, json).map_err(|e| format!("写截图索引失败: {e}"))
}

/// 设置收藏标记并落盘。取消收藏且该条目再无其它信息时直接删行，索引不留空壳。
/// 整个读改写在 `INDEX_LOCK` 内完成，并发调用不会互相覆盖。
pub fn set_starred(workspace: &Path, name: &str, starred: bool) -> Result<(), String> {
    let _guard = lock();
    let mut index = load(workspace);
    if starred {
        index.items.entry(name.to_string()).or_default().starred = true;
    } else if let Some(meta) = index.items.get_mut(name) {
        meta.starred = false;
        if meta.tags.is_empty() {
            index.items.remove(name);
        }
    } else {
        return Ok(()); // 本来就没记录，无需改动。
    }
    save(workspace, &index)
}

/// 取锁。持锁线程 panic 导致中毒时照常继续——索引本身是「坏了就当空」的容错结构，
/// 没有需要保护的不变量，为它把整个应用卡住不值当。
fn lock() -> std::sync::MutexGuard<'static, ()> {
    INDEX_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// 删除某文件名的元数据（截图被删时同步清理，避免索引长期堆积孤儿条目）。
/// 无该条目则不写盘。同 `set_starred`，读改写在锁内完成。
pub fn forget(workspace: &Path, name: &str) -> Result<(), String> {
    let _guard = lock();
    let mut index = load(workspace);
    if index.items.remove(name).is_none() {
        return Ok(());
    }
    save(workspace, &index)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 索引缺失时按空处理() {
        let dir = tempfile::tempdir().unwrap();
        let index = load(dir.path());
        assert!(index.items.is_empty());
        assert!(!index.is_starred("a.png"));
    }

    #[test]
    fn 索引损坏时按空处理() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(super::super::output::screenshots_dir(dir.path())).unwrap();
        std::fs::write(index_path(dir.path()), "{ 这不是 json").unwrap();
        assert!(load(dir.path()).items.is_empty());
    }

    #[test]
    fn 收藏与取消收藏往返() {
        let dir = tempfile::tempdir().unwrap();
        set_starred(dir.path(), "a.png", true).unwrap();
        assert!(load(dir.path()).is_starred("a.png"));

        set_starred(dir.path(), "a.png", false).unwrap();
        let index = load(dir.path());
        assert!(!index.is_starred("a.png"));
        // 取消收藏且无标签 → 条目被清掉，不留空壳。
        assert!(!index.items.contains_key("a.png"));
    }

    #[test]
    fn 取消收藏保留标签条目() {
        let dir = tempfile::tempdir().unwrap();
        let mut index = MetaIndex::default();
        index.items.insert(
            "a.png".to_string(),
            ItemMeta {
                starred: true,
                tags: vec!["网络".to_string()],
            },
        );
        save(dir.path(), &index).unwrap();

        set_starred(dir.path(), "a.png", false).unwrap();
        let index = load(dir.path());
        assert!(!index.is_starred("a.png"));
        assert_eq!(index.items["a.png"].tags, vec!["网络".to_string()]);
    }

    /// 并发收藏不同的图：两条都得留下（无锁时后写会盖掉先写）。
    #[test]
    fn 并发收藏不互相覆盖() {
        let dir = tempfile::tempdir().unwrap();
        let names: Vec<String> = (0..16).map(|i| format!("{i}.png")).collect();
        std::thread::scope(|s| {
            for name in &names {
                let path = dir.path();
                s.spawn(move || set_starred(path, name, true).unwrap());
            }
        });
        let index = load(dir.path());
        for name in &names {
            assert!(index.is_starred(name), "{name} 的收藏被覆盖了");
        }
    }

    #[test]
    fn 删除截图同步清索引() {
        let dir = tempfile::tempdir().unwrap();
        set_starred(dir.path(), "a.png", true).unwrap();
        forget(dir.path(), "a.png").unwrap();
        assert!(load(dir.path()).items.is_empty());
        // 重复 forget 不报错。
        forget(dir.path(), "a.png").unwrap();
    }
}
