// Windows 下把 assets/icon.ico 嵌入可执行文件资源（exe 图标）。
// embed-resource 内部会检查目标平台，非 Windows 目标自动跳过。
fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        embed_resource::compile("assets/app.rc", embed_resource::NONE)
            .manifest_optional()
            .expect("failed to embed app icon");
    }
}
