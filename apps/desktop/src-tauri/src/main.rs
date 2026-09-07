fn main() {
    std::panic::set_hook(Box::new(|info| {
        let msg = format!("Panic occurred: {:?}", info);
        eprintln!("{}", msg);
        let _ = std::fs::write(std::env::temp_dir().join("karaoke-tauri-panic.log"), &msg);
    }));

    println!("[*] Starting appsdesktop_lib::run()...");
    appsdesktop_lib::run();
    println!("[*] appsdesktop_lib::run() finished cleanly.");

    println!("\n[!] アプリケーションが終了しました。Enterキーを押すとこの画面を閉じます...");
    let mut buf = String::new();
    let _ = std::io::stdin().read_line(&mut buf);
}
