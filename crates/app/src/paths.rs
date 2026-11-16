//! 桌面形态的目录布局。
//!
//! server 配置里的 `data_dir` 默认是相对路径 `./data`。如果直接按进程工作目录
//! 展开，从 macOS Finder 双击 `.app`（cwd = `/`）启动时会尝试创建 `/data`：
//! 失败后写认证密码那一步 `expect` 直接 abort（issue #1）。因此桌面形态统一把
//! 相对路径锚定到用户目录，而不是进程 cwd。
//!
//! - **便携模式**：当前目录有 `ximed.toml`（开发、绿色部署）→ 一切留在该目录；
//! - **其余**：平台用户数据目录 `.../ximed`——macOS `~/Library/Application Support/ximed`，
//!   Linux `~/.local/share/ximed`，Windows `%LOCALAPPDATA%\ximed`。

use std::path::{Path, PathBuf};

/// 相对 `data_dir` 的锚点目录（绝对路径）。
pub fn base_dir() -> PathBuf {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
    if cwd.join("ximed.toml").is_file() {
        return cwd;
    }
    dirs::data_local_dir()
        // 取不到用户目录（极端的无 HOME 环境）时退回临时目录：至少可写，
        // 不会再因为不可写而 abort。
        .unwrap_or_else(std::env::temp_dir)
        .join("ximed")
}

/// 把配置里的 `data_dir` 展开成绝对路径：相对路径锚定在 `base`，绝对路径原样使用。
///
/// 同时清掉冗余的 `.` 分量（`base/./data` → `base/data`），避免界面和日志里出现
/// 难看的中间路径。
pub fn resolve_data_dir(raw: &str, base: &Path) -> PathBuf {
    let path = Path::new(raw);
    let joined = if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.join(path)
    };
    joined.components().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 相对路径锚定到_base_而不是进程_cwd() {
        let base = Path::new("/Users/alice/Library/Application Support/ximed");
        assert_eq!(
            resolve_data_dir("./data", base),
            base.join("data"),
            "默认 ./data 应落在应用目录，而不是 /data"
        );
    }

    #[test]
    fn 绝对路径原样保留() {
        assert_eq!(
            resolve_data_dir("/var/lib/ximed", Path::new("/tmp")),
            PathBuf::from("/var/lib/ximed")
        );
    }

    #[test]
    fn 去掉冗余的点分量() {
        assert_eq!(
            resolve_data_dir("././data", Path::new("/tmp/ximed")),
            PathBuf::from("/tmp/ximed/data")
        );
    }
}
