fn main() {
    // tauri-build 未把 bundle.icon 加入 rerun-if-changed，图标更新后
    // 资源不会重编译；这里显式声明，避免改图标后旧图标残留
    println!("cargo:rerun-if-changed=icons/icon.ico");
    tauri_build::build()
}
