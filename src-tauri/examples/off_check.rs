// Temporary: dictation off/on through the real host code, measured.
use flow_core::engine::Transcriber;
use flow_lib::host;
fn sh(cmd: &str) -> String {
    let out = std::process::Command::new("sh").arg("-c").arg(cmd).output().unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}
fn report(label: &str) {
    let pid = std::process::id();
    let vram = sh(&format!("nvidia-smi | grep ' {pid} ' | grep -oE '[0-9]+MiB' | tail -1"));
    let ollama = sh("curl -s localhost:11434/api/ps | python3 -c \"import sys,json; print([m['name'] for m in json.load(sys.stdin)['models']])\"");
    println!("{label:<10} flow {:<8} ollama {ollama}", if vram.is_empty() { "none".into() } else { vram });
}
fn wait_ready(shared: &host::Shared) {
    while !shared.transcriber.lock().unwrap().as_ref().is_some_and(|(_, t)| t.loaded()) {
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    std::thread::sleep(std::time::Duration::from_millis(1500)); // the cleanup model warms after
}
fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();
    let shared = host::Shared::load();
    host::start(&shared, None).unwrap();
    wait_ready(&shared);
    report("on");
    host::turn_off(&shared, None);
    report("off");
    host::start(&shared, None).unwrap();
    wait_ready(&shared);
    report("on again");
    host::stop(&shared, None);
    host::release_gpu(&shared);
    report("quit");
}
