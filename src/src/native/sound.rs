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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn play_invalid_path_does_not_panic() {
        play("/nonexistent/path.aiff");
    }

    #[test]
    fn play_glass_does_not_panic() {
        play_glass();
    }

    #[test]
    fn play_funk_does_not_panic() {
        play_funk();
    }

    #[test]
    fn play_tolerates_afplay_not_installed() {
        play("/does/not/matter.aiff");
    }
}
