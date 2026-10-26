//! The registry-lookup progress bar shared by every binary.
//!
//! The style used to be copy-pasted per call site and drifted: ncu ended up
//! without elapsed time or ETA. Building it in one place keeps ccu, pcu and
//! ncu identical.

use indicatif::{ProgressBar, ProgressStyle};

/// A progress bar of `len` steps in the house style.
pub fn progress_bar(len: u64) -> ProgressBar {
    let bar = ProgressBar::new(len);
    bar.set_style(
        ProgressStyle::default_bar()
            .template(
                "{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {pos}/{len} ({eta})",
            )
            .expect("valid progress template")
            .progress_chars("#>-"),
    );
    bar
}
