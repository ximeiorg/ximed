//! 诊断：真实 ximed_sync 插件的 testConnection 是否真的发出 HTTP 请求。
//!
//! 依赖 /tmp/ximed-sync.xipk（手动下载的真实插件包），仅本地诊断用。

use std::io::{Read, Write};
use std::net::TcpListener;

use client::plugin::SyncPluginManager;

#[test]
fn real_plugin_test_connection_makes_request() {
    let xipk = std::path::Path::new("/tmp/ximed-sync.xipk");
    if !xipk.exists() {
        eprintln!("skip: /tmp/ximed-sync.xipk not present");
        return;
    }

    // mock HTTP server：记录收到的请求行与头部，返回 200
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = std::thread::spawn(move || {
        let (mut sock, _) = listener.accept().unwrap();
        let mut buf = [0u8; 4096];
        let n = sock.read(&mut buf).unwrap();
        let req = String::from_utf8_lossy(&buf[..n]).to_string();
        sock.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
            .unwrap();
        req
    });

    let dir = std::env::temp_dir().join(format!("ximed_real_plugin_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let manager = SyncPluginManager::new(dir.join("plugins"));
    let record = manager.install_from_zip(xipk, false).unwrap();
    println!("installed: {} v{}", record.id, record.version);
    println!("fields: {:?}", manager.config_fields(&record.id));

    manager
        .save_config(
            &record.id,
            &[
                ("serverUrl".to_string(), format!("http://127.0.0.1:{port}")),
                ("username".to_string(), "alice".to_string()),
                ("password".to_string(), "pw".to_string()),
            ]
            .into_iter()
            .collect(),
        )
        .unwrap();

    let result = manager.test_connection(&record.id);
    println!("test_connection result: {result:?}");

    let req = server.join().unwrap();
    println!("=== mock server received ===\n{req}");
    assert!(
        req.starts_with("GET /api/clipboard"),
        "no request reached mock server"
    );
    assert!(
        matches!(result, Ok(None)),
        "expected success (None), got {result:?}"
    );

    std::fs::remove_dir_all(&dir).unwrap();
}
