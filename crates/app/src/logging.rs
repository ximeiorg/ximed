//! Windows 桌面形态的日志落盘。
//!
//! `ximed-desktop` / 带托盘形态的 `ximed-client` 以 GUI 子系统链接（见各 bin 的
//! `windows_subsystem`），进程没有控制台：`tracing_subscriber` 默认的 stdout writer
//! 会把日志写进 void，用户反馈问题时一点痕迹都没有。这里改成写文件：
//! `<配置目录>/logs/ximed-desktop.log`（client 形态为 `ximed-client.log`）。
//!
//! 只做一件事——把已经存在的日志流改道，不引入滚动/压缩依赖。桌面端常驻运行，
//! 单个文件超过 [`MAX_BYTES`] 时在启动阶段滚动成 `.1`，只保留一份历史，够定位
//! 最近一次运行的问题。

use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use tracing_subscriber::EnvFilter;

/// 单个日志文件上限（5 MiB）。超过则在启动时滚动，避免长期运行无限增长。
const MAX_BYTES: u64 = 5 * 1024 * 1024;

/// 日志文件路径：`<config_dir>/logs/<stem>.log`。
pub fn log_path(config_dir: &str, stem: &str) -> PathBuf {
    Path::new(config_dir)
        .join("logs")
        .join(format!("{stem}.log"))
}

/// 初始化文件日志。
///
/// 日志写不出来不应阻止程序启动（只读目录、磁盘满），因此所有失败都静默跳过——
/// 但这个函数仍会尝试创建目录。
pub fn init_file(level: &str, config_dir: &str, stem: &str) {
    let path = log_path(config_dir, stem);
    let Some(file) = open_rotated(&path) else {
        return;
    };
    // 与 xime_sync_server::init_tracing 保持同一套 target 过滤
    let filter = EnvFilter::new(format!(
        "server={level},service={level},client={level},app={level}"
    ));
    let _ = tracing_subscriber::fmt()
        .with_ansi(false) // 文件里不需要 ANSI 转义序列
        .with_writer(Mutex::new(file))
        .with_env_filter(filter)
        .try_init();
}

/// 打开日志文件；超过上限时先滚动成 `.1`（覆盖上一份历史）。
fn open_rotated(path: &Path) -> Option<File> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).ok()?;
    }
    let too_big = fs::metadata(path)
        .map(|meta| meta.len() > MAX_BYTES)
        .unwrap_or(false);
    if too_big {
        let rotated = path.with_extension("log.1");
        let _ = fs::rename(path, rotated);
    }
    OpenOptions::new().create(true).append(true).open(path).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 日志路径落在配置目录的_logs_子目录() {
        let path = log_path("/tmp/ximed", "ximed-desktop");
        assert!(path.ends_with("logs/ximed-desktop.log"));
    }

    #[test]
    fn 超过上限时滚动并重新开始() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().to_string_lossy().into_owned();
        let path = log_path(&config, "ximed-desktop");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        // 造一个超过上限的旧文件（用 set_len 避免真写 5 MiB）
        let file = File::create(&path).unwrap();
        file.set_len(MAX_BYTES + 1).unwrap();
        drop(file);

        let reopened = open_rotated(&path).expect("应能重新打开");
        drop(reopened);

        assert!(path.exists(), "新日志文件应存在");
        assert_eq!(fs::metadata(&path).unwrap().len(), 0);
        assert!(path.with_extension("log.1").exists(), "旧日志应被滚动成 .1");
    }

    #[test]
    fn 未超过上限时追加不滚动() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().to_string_lossy().into_owned();
        let path = log_path(&config, "ximed-desktop");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, b"old line\n").unwrap();

        drop(open_rotated(&path).unwrap());

        assert_eq!(fs::read(&path).unwrap(), b"old line\n");
        assert!(!path.with_extension("log.1").exists());
    }
}
