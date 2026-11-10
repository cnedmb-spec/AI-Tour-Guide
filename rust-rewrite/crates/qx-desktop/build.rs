//! 构建期把原页面与主题包打包进 exe：把 `web/` 与 `themes/` 压成
//! `OUT_DIR/bundle.zip`，运行时由 main.rs 解压到临时目录后由 Rust HTTP 服务托管。
//! 这样产出的 qx-desktop.exe 是自包含的单个文件，不依赖任何外部目录 / Python。

use std::fs::{self, File};
use std::io::Write;
use std::path::Path;
use zip::write::FileOptions;
use zip::CompressionMethod;

fn main() {
    let manifest = env!("CARGO_MANIFEST_DIR");
    // crates/qx-desktop → crates → rust-rewrite
    let root = Path::new(manifest).join("../../");

    let out_dir = std::env::var("OUT_DIR").expect("OUT_DIR 未设置");
    let zip_path = Path::new(&out_dir).join("bundle.zip");
    let file = File::create(&zip_path).expect("无法创建 bundle.zip");
    let mut zw = zip::ZipWriter::new(file);
    let opts = FileOptions::default().compression_method(CompressionMethod::Deflated);

    add_dir(&mut zw, &root.join("web"), Path::new("web"), &opts);
    add_dir(&mut zw, &root.join("themes"), Path::new("themes"), &opts);
    // 离线神经语音资产（espeakbridge.pyd + espeak-ng-data + onnxruntime.dll +
    // huayan VITS 模型），随 exe 分发、启动释放到临时目录，不依赖外部文件。
    add_dir(&mut zw, &root.join("tts"), Path::new("tts"), &opts);
    // 赛前预生成的标准普通话讲解语音（可选增强）：按 tts-cache 键命名，
    // 解压到 bundle 根目录后运行时直接命中磁盘缓存，演示全程离线。
    // 目录为空时 add_dir 静默跳过，等价于纯离线 Piper 方案。
    add_dir(&mut zw, &root.join("prebuilt-voices"), Path::new("tts-cache"), &opts);
    zw.finish().expect("打包失败");

    // 页面/主题/语音资产有变动时重新打包
    println!("cargo:rerun-if-changed={}", root.join("web").display());
    println!("cargo:rerun-if-changed={}", root.join("themes").display());
    println!("cargo:rerun-if-changed={}", root.join("tts").display());
    println!("cargo:rerun-if-changed={}", root.join("prebuilt-voices").display());
}

fn add_dir(zw: &mut zip::ZipWriter<File>, base: &Path, prefix: &Path, opts: &FileOptions) {
    let Ok(rd) = fs::read_dir(base) else { return };
    for entry in rd.flatten() {
        let path = entry.path();
        let rel = prefix.join(entry.file_name());
        if path.is_dir() {
            add_dir(zw, &path, &rel, opts);
        } else {
            let Ok(data) = fs::read(&path) else { continue };
            zw.start_file(rel.to_string_lossy().replace('\\', "/"), *opts)
                .expect("写入条目失败");
            zw.write_all(&data).expect("写入数据失败");
        }
    }
}
