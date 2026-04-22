use std::path::PathBuf;
use std::sync::{Mutex, Once};

static INIT: Once = Once::new();

pub static FS_LOCK: Mutex<()> = Mutex::new(());

pub fn set_test_home() -> PathBuf {
    INIT.call_once(|| {
        let tmp = std::env::temp_dir().join(format!(
            "aim-lib-tests-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&tmp).unwrap();
        std::env::set_var("HOME", &tmp);
    });
    dirs::home_dir().unwrap()
}

pub fn reset_fs() {
    let home = set_test_home();
    for sub in [".claude", ".claude-instances-ui"] {
        let p = home.join(sub);
        if p.is_dir() {
            let _ = std::fs::remove_dir_all(&p);
        }
    }
}
