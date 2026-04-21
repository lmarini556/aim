use std::process::Stdio;

pub fn play_glass() {
    play("/System/Library/Sounds/Glass.aiff");
}

pub fn play_funk() {
    play("/System/Library/Sounds/Funk.aiff");
}

fn play(path: &str) {
    let _ = std::process::Command::new("afplay")
        .args(["-v", "0.5", path])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
}
