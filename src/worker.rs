/// Optional-operation workers report failures to their owner. Their panics
/// cannot invalidate UI state because owned results cross a channel boundary.
pub(crate) fn spawn(
    name: &'static str,
    work: impl FnOnce() + Send + 'static,
    failure: impl Fn(String) + Send + Sync + 'static,
) -> Option<std::thread::JoinHandle<()>> {
    let failure = std::sync::Arc::new(failure);
    let worker_failure = failure.clone();
    match std::thread::Builder::new()
        .name(name.into())
        .spawn(move || {
            if let Err(panic) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(work)) {
                worker_failure(format!(
                    "{name} failed: {}",
                    crate::native_error::panic_detail(panic.as_ref())
                ));
            }
        }) {
        Ok(thread) => Some(thread),
        Err(error) => {
            failure(format!("Unable to start {name}: {error}"));
            None
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn worker_panic_reaches_owner_without_unwinding_into_ui() {
        let (tx, rx) = std::sync::mpsc::channel();
        super::spawn(
            "test worker",
            || panic!("test failure"),
            move |error| {
                tx.send(error).unwrap();
            },
        )
        .unwrap()
        .join()
        .unwrap();
        assert!(rx.recv().unwrap().contains("test failure"));
    }
}
