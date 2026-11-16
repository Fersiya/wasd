// Встраивает манифест "запуск от администратора": без прав админа
// sing-box не сможет создать TUN-адаптер и поднять маршруты.
use embed_manifest::{embed_manifest, manifest::ExecutionLevel, new_manifest};

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let target_windows = std::env::var_os("CARGO_CFG_WINDOWS").is_some();
    let skip = std::env::var_os("SKIP_MANIFEST").is_some();
    if target_windows && !skip {
        embed_manifest(
            new_manifest("Nyxar")
                .requested_execution_level(ExecutionLevel::RequireAdministrator),
        )
        .expect("не удалось встроить манифест");
    }
}
