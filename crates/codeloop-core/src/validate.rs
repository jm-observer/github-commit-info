//! §4.1 三方一致性校验：Claude.cwd / Codex.cwd / target_path 各自向上找 `.git` 求 repo
//! root，要求两端会话同根、且 target 落在 root 内。拒绝跑错仓。
//!
//! 越界校验**纯词法（lexical）归一**后再 `starts_with`：`std::fs::canonicalize` 对**不存在**的
//! target 会失败回退原始路径，使 `D:\repo\..\outside\new.md` 之类的 `..` 能骗过 `starts_with`
//! （`Path::starts_with` 按 component 比较，认为它仍以 `D:\repo` 开头）。这里改为先词法消解
//! `.`/`..`，再 canonicalize 最近的**已存在祖先** + 拼接归一尾段，使比较两侧前缀一致
//! （含 Windows `\\?\` 扩展长度前缀）。

use anyhow::{anyhow, bail, Result};
use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};

/// 从 `start` 向上逐级找含 `.git` 的目录（工作树根）。找不到返回 `None`。
///
/// 纯文件系统查找，不依赖 git 进程。`.git` 可以是目录（普通仓）或文件（worktree / submodule）。
pub fn find_repo_root(start: &Path) -> Option<PathBuf> {
    let mut cur = Some(start);
    while let Some(dir) = cur {
        if dir.join(".git").exists() {
            return Some(dir.to_path_buf());
        }
        cur = dir.parent();
    }
    None
}

/// 校验结果：两端共同的 repo root，以及归一化后的 target 绝对路径。
/// 两者都已 canonicalize（Windows 下含 `\\?\` 前缀），用于安全比较。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Validated {
    pub repo_root: PathBuf,
    pub target_abs: PathBuf,
}

/// 仅校验两端 session 的 cwd 落在同一个 git 工作树（用于 Continuation 入口，target_path 缺省）。
/// 成功返回共同 repo root。
pub fn validate_two_way(claude_cwd: &Path, codex_cwd: &Path) -> Result<PathBuf> {
    let claude_root = find_repo_root(claude_cwd)
        .ok_or_else(|| anyhow!("Claude cwd 向上未找到 .git：{}", claude_cwd.display()))?;
    let codex_root = find_repo_root(codex_cwd)
        .ok_or_else(|| anyhow!("Codex cwd 向上未找到 .git：{}", codex_cwd.display()))?;
    let cr = canon(&claude_root);
    let dr = canon(&codex_root);
    if cr != dr {
        bail!(
            "两端不在同一工作树：Claude repo root={}，Codex repo root={}（claude cwd={}，codex cwd={}）",
            claude_root.display(),
            codex_root.display(),
            claude_cwd.display(),
            codex_cwd.display(),
        );
    }
    Ok(cr)
}

/// 三方一致性校验。
///
/// - `claude_cwd` / `codex_cwd`：两会话解析出的工作目录。
/// - `target_path`：可为相对（相对 repo root）或绝对路径。
///
/// 成功返回共同 repo root + target 绝对路径；两端不同根 / 找不到 `.git` / target 越界 → `Err`
/// （消息含实际路径，供 400 提示）。
pub fn validate_three_way(
    claude_cwd: &Path,
    codex_cwd: &Path,
    target_path: &str,
) -> Result<Validated> {
    let claude_root = find_repo_root(claude_cwd)
        .ok_or_else(|| anyhow!("Claude cwd 向上未找到 .git：{}", claude_cwd.display()))?;
    let codex_root = find_repo_root(codex_cwd)
        .ok_or_else(|| anyhow!("Codex cwd 向上未找到 .git：{}", codex_cwd.display()))?;

    // 两端 repo root 必须相等（canonicalize 抹平 .. / 大小写 / 符号链接差异）。
    let cr = canon(&claude_root);
    let dr = canon(&codex_root);
    if cr != dr {
        bail!(
            "两端不在同一工作树：Claude repo root={}，Codex repo root={}（claude cwd={}，codex cwd={}）",
            claude_root.display(),
            codex_root.display(),
            claude_cwd.display(),
            codex_cwd.display(),
        );
    }
    let repo_root = cr;

    // target 可能是相对路径（相对 repo root 解析）或绝对路径。
    let raw = {
        let p = Path::new(target_path);
        if p.is_absolute() {
            p.to_path_buf()
        } else {
            repo_root.join(p)
        }
    };
    // 词法归一 + canonicalize 最近已存在祖先（避免对未创建的 target 文件报错，且与 repo_root
    // 的 canonical 前缀对齐）。
    let target_abs = normalized_abs(&raw);

    // target 必须落在 root 内（此时两侧前缀一致，`..` 已被消解）。
    if !target_abs.starts_with(&repo_root) {
        bail!(
            "target_path 越界 / 不在 repo root 内：target={}，root={}（原始 target={}）",
            target_abs.display(),
            repo_root.display(),
            target_path,
        );
    }

    Ok(Validated {
        repo_root,
        target_abs,
    })
}

/// canonicalize 兜底：路径不存在时退回逻辑归一（仅用于必存在的 repo root）。
fn canon(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

/// 纯词法归一：消解 `.` 与 `..`（不触碰文件系统），`..` 不会越过 root/prefix。
fn normalize_lexical(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for comp in p.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                // pop 到 root（含 Windows 盘符前缀）即停，`..` 不会逃出 root。
                out.pop();
            }
            Component::Prefix(_) | Component::RootDir | Component::Normal(_) => {
                out.push(comp.as_os_str());
            }
        }
    }
    out
}

/// 词法归一后，canonicalize 最近的已存在祖先，再拼接归一过的尾段。
///
/// 这样即便 target 文件尚未创建，比较两侧（target 与 repo_root）也都经过 canonicalize，
/// 前缀（如 Windows `\\?\`）一致，`starts_with` 判定可靠。
fn normalized_abs(raw: &Path) -> PathBuf {
    let lex = normalize_lexical(raw);
    let mut tail: Vec<OsString> = Vec::new();
    let mut cur: &Path = &lex;
    loop {
        if cur.exists() {
            let mut base = std::fs::canonicalize(cur).unwrap_or_else(|_| cur.to_path_buf());
            for name in tail.iter().rev() {
                base.push(name);
            }
            return base;
        }
        match (cur.file_name(), cur.parent()) {
            (Some(name), Some(parent)) => {
                tail.push(name.to_os_string());
                cur = parent;
            }
            // 无更上层（已到 root 且不存在）：退回词法归一结果（best-effort）。
            _ => return lex,
        }
    }
}

/// 解析 repo root 对应的 git **common dir**（绝对、canonicalize 后），用于判定两个工作树
/// 是否同属一个仓库。
///
/// - 普通仓：`<root>/.git` 是目录，common dir 即它本身（除非其下有 `commondir` 文件）。
/// - linked worktree：`<root>/.git` 是文件，内容 `gitdir: <main>/.git/worktrees/<name>`；
///   该 gitdir 下的 `commondir` 文件指回主仓 `.git`。
///
/// 纯文件系统解析，不起 git 进程（与 `find_repo_root` 一致）。任一步失败返回 `None`。
pub fn git_common_dir(repo_root: &Path) -> Option<PathBuf> {
    let dot_git = repo_root.join(".git");
    let meta = std::fs::metadata(&dot_git).ok()?;
    let git_dir = if meta.is_dir() {
        dot_git
    } else {
        // `.git` 文件：`gitdir: <path>`（可为相对 repo_root 的路径）。
        let content = std::fs::read_to_string(&dot_git).ok()?;
        let rest = content.lines().find_map(|l| l.strip_prefix("gitdir:"))?;
        let p = Path::new(rest.trim());
        if p.is_absolute() {
            p.to_path_buf()
        } else {
            repo_root.join(p)
        }
    };
    // git_dir 下若有 `commondir`（worktree 情形），据它回溯到主仓 git dir；否则 git_dir 即 common。
    let common = match std::fs::read_to_string(git_dir.join("commondir")) {
        Ok(rel) => git_dir.join(rel.trim()),
        Err(_) => git_dir,
    };
    Some(canon(&normalize_lexical(&common)))
}

/// 两个工作树是否同属一个 git 仓库（common dir 相等）。任一侧解析失败 → `false`（fail-closed）。
pub fn same_repo(a: &Path, b: &Path) -> bool {
    match (git_common_dir(a), git_common_dir(b)) {
        (Some(x), Some(y)) => x == y,
        _ => false,
    }
}

/// 去掉 Windows `\\?\` 扩展长度前缀，得到适合显示 / 传给子进程 `--cd` 的常规路径。
/// 非 Windows / 无前缀时原样返回。
pub fn display_path(p: &Path) -> PathBuf {
    let s = p.to_string_lossy();
    if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{rest}"));
    }
    if let Some(rest) = s.strip_prefix(r"\\?\") {
        return PathBuf::from(rest);
    }
    p.to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// 造一个临时 git 工作树：root/.git + root/docs/foo.md + root/crates/x/。
    fn make_repo() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        fs::create_dir_all(root.join(".git")).unwrap();
        fs::create_dir_all(root.join("docs")).unwrap();
        fs::create_dir_all(root.join("crates/x")).unwrap();
        fs::write(root.join("docs/foo.md"), "doc").unwrap();
        tmp
    }

    #[test]
    fn find_root_walks_up() {
        let tmp = make_repo();
        let root = canon(tmp.path());
        let from_sub = find_repo_root(&tmp.path().join("crates/x")).unwrap();
        assert_eq!(canon(&from_sub), root);
    }

    #[test]
    fn find_root_none_when_no_git() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(find_repo_root(tmp.path()).is_none());
    }

    #[test]
    fn three_way_ok_from_subdirs() {
        let tmp = make_repo();
        let root = tmp.path();
        // Claude 从 docs 启动，Codex 从 crates/x 启动，target 相对路径。
        let v =
            validate_three_way(&root.join("docs"), &root.join("crates/x"), "docs/foo.md").unwrap();
        assert_eq!(v.repo_root, canon(root));
        assert_eq!(v.target_abs, canon(&root.join("docs/foo.md")));
    }

    #[test]
    fn three_way_rejects_different_trees() {
        let a = make_repo();
        let b = make_repo();
        let err = validate_three_way(
            &a.path().join("docs"),
            &b.path().join("crates/x"),
            "docs/foo.md",
        )
        .unwrap_err();
        assert!(format!("{err}").contains("同一工作树"));
    }

    #[test]
    fn three_way_rejects_no_git() {
        let tmp = tempfile::tempdir().unwrap();
        let err = validate_three_way(tmp.path(), tmp.path(), "foo.md").unwrap_err();
        assert!(format!("{err}").contains(".git"));
    }

    #[test]
    fn three_way_ok_for_nonexistent_target_in_repo() {
        // target 尚未创建：仍应通过（落在 repo root 内）。
        let tmp = make_repo();
        let root = tmp.path();
        let v = validate_three_way(root, root, "docs/new-not-created.md").unwrap();
        assert!(v.target_abs.starts_with(&v.repo_root));
        assert!(v.target_abs.ends_with("new-not-created.md"));
    }

    #[test]
    fn three_way_rejects_parent_escape_even_when_nonexistent() {
        // `..` 逃出 repo root（且目标不存在）必须被拒——这是修复前能绕过的越界路径。
        let tmp = make_repo();
        let root = tmp.path();
        let err = validate_three_way(root, root, "../outside/new.md").unwrap_err();
        assert!(format!("{err}").contains("repo root"));
    }

    #[test]
    fn three_way_rejects_absolute_outside_root() {
        let tmp = make_repo();
        let other = tempfile::tempdir().unwrap();
        let outside = other.path().join("evil.md");
        let err =
            validate_three_way(tmp.path(), tmp.path(), &outside.to_string_lossy()).unwrap_err();
        assert!(format!("{err}").contains("repo root"));
    }

    /// 在 `main/.git/worktrees/<name>` 下造 commondir，并返回一个独立的 worktree 目录，
    /// 其 `.git` 文件 `gitdir:` 指向该 worktree git dir。
    fn make_linked_worktree(main_root: &Path, name: &str) -> tempfile::TempDir {
        let wt_git = main_root.join(".git").join("worktrees").join(name);
        fs::create_dir_all(&wt_git).unwrap();
        // commondir 指回主仓 .git（git 的标准布局：worktrees/<name> 的上两级即 .git）。
        fs::write(wt_git.join("commondir"), "../..\n").unwrap();
        let wt = tempfile::tempdir().unwrap();
        fs::write(
            wt.path().join(".git"),
            format!("gitdir: {}\n", wt_git.display()),
        )
        .unwrap();
        wt
    }

    #[test]
    fn same_repo_true_for_self() {
        let tmp = make_repo();
        assert!(same_repo(tmp.path(), tmp.path()));
    }

    #[test]
    fn same_repo_true_for_linked_worktree() {
        let main = make_repo();
        let wt = make_linked_worktree(main.path(), "wt1");
        assert!(same_repo(main.path(), wt.path()));
        assert!(same_repo(wt.path(), main.path()));
    }

    #[test]
    fn same_repo_false_for_different_repos() {
        let a = make_repo();
        let b = make_repo();
        assert!(!same_repo(a.path(), b.path()));
    }

    #[test]
    fn same_repo_false_when_no_git() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = make_repo();
        assert!(!same_repo(repo.path(), tmp.path()));
    }

    #[test]
    fn three_way_accepts_dotdot_that_stays_within_root() {
        // crates/../docs/foo.md 词法归一 = docs/foo.md，仍在 root 内 → 通过。
        let tmp = make_repo();
        let root = tmp.path();
        let v = validate_three_way(root, root, "crates/../docs/foo.md").unwrap();
        assert_eq!(v.target_abs, canon(&root.join("docs/foo.md")));
    }
}
