//! Keeps the system from slowing down work the user asked for.
//!
//! On macOS, App Nap throttles an app whose windows are all out of sight: an install left
//! behind other windows would download and unpack more slowly. A [`UserActivity`] tells the
//! system the work was started by the user, which lifts that and keeps the Mac from idle
//! sleep until it ends. Elsewhere it does nothing.

/// Held while work the user started runs; dropping it ends the activity.
#[derive(Debug)]
pub struct UserActivity {
    #[cfg(target_os = "macos")]
    _end: std::sync::mpsc::Sender<()>,
}

impl UserActivity {
    /// Begins an activity; `reason` is what the system shows for it (`pmset -g assertions`).
    pub fn begin(reason: &str) -> Self {
        #[cfg(target_os = "macos")]
        {
            let (end, ended) = std::sync::mpsc::channel::<()>();
            let reason = reason.to_owned();
            // The system holds the activity while the block runs on this thread, and the
            // block waits until the sender is dropped.
            let thread = std::thread::Builder::new().name("suite-activity".into()).spawn(move || {
                use objc2_foundation::{NSActivityOptions, NSProcessInfo, NSString};
                let block = block2::RcBlock::new(move || {
                    let _ = ended.recv();
                });
                NSProcessInfo::processInfo().performActivityWithOptions_reason_usingBlock(
                    NSActivityOptions::UserInitiated,
                    &NSString::from_str(&reason),
                    &block,
                );
            });
            if let Err(e) = thread {
                tracing::debug!("no activity thread: {e}");
            }
            Self { _end: end }
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = reason;
            Self {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn begins_and_ends() {
        let _held = UserActivity::begin("stockroom test");
    }
}
