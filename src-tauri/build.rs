fn main() {
    // tauri-build 未把 bundle.icon / installerIcon 加入 rerun-if-changed，
    // 图标更新后资源不会重编译；这里显式声明 bundler 可能消费的全部图标，
    // 避免改图标后旧图标残留（若文件被重命名，cargo 仅警告并以 no-op 处理，
    // 因此需与 tauri.conf.json 的 icon 清单保持同步）
    println!("cargo:rerun-if-changed=icons/icon.ico");
    println!("cargo:rerun-if-changed=icons/icon.icns");
    println!("cargo:rerun-if-changed=icons/icon.png");
    tauri_build::build()
}
