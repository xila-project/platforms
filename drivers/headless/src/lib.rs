#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
compile_error!("drivers_headless is only available on native host platforms");

mod audio;

pub use audio::HeadlessAudioOutput;
pub use graphics::headless_screen::{HeadlessError, HeadlessScreenDevice};
