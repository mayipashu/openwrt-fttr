//! Desired bits are reconciled only by the PLOAM hardware owner.
use std::sync::{Mutex, OnceLock};

/// Shared owner-side completion policy; platform adapters retain register layout.
/// Attempts for one autonomous reconciliation before it is reported as failed.
const RECONCILE_ATTEMPTS: u32 = 3;
/// Settle time between reconciliation attempts.
const RECONCILE_SETTLE: std::time::Duration = std::time::Duration::from_millis(2);

fn verify(
    value: crate::platform::resources::PonMacReadback,
    desired: (bool, bool),
) -> std::io::Result<crate::platform::resources::PonMacReadback> {
    if value.tx_enabled != Some(desired.0) || value.fec_enabled != Some(desired.1) {
        Err(std::io::Error::other("GPON TX/FEC readback mismatch"))
    } else {
        Ok(value)
    }
}

pub(crate) fn reconcile(
    desired: (bool, bool),
    mut quiesce: impl FnMut() -> std::io::Result<()>,
    mut write: impl FnMut(bool, bool) -> std::io::Result<crate::platform::resources::PonMacReadback>,
) -> std::io::Result<()> {
    /*
     * This runs once a second from the PLOAM owner, so a single stale read-back
     * must not be treated as a hardware fault. Retry the whole transaction with
     * the owned-bit write re-asserted before reporting a failure.
     */
    let mut result =
        apply(desired, &mut quiesce, &mut write).and_then(|value| verify(value, desired));
    let mut attempt = 0;
    while result.is_err() && attempt + 1 < RECONCILE_ATTEMPTS {
        attempt += 1;
        std::thread::sleep(RECONCILE_SETTLE);
        result = apply(desired, &mut quiesce, &mut write).and_then(|value| verify(value, desired));
    }
    let error = result.as_ref().err().map(ToString::to_string);
    record(result);
    error.map_or(Ok(()), |error| Err(std::io::Error::other(error)))
}

/// Common cadence for external configuration updates and hardware readback.
pub(crate) struct Poll {
    last: std::time::Instant,
}
impl Default for Poll {
    fn default() -> Self {
        Self {
            last: std::time::Instant::now() - std::time::Duration::from_secs(2),
        }
    }
}
impl Poll {
    pub(crate) fn due(&mut self) -> bool {
        if self.last.elapsed() < std::time::Duration::from_secs(1) {
            return false;
        }
        self.last = std::time::Instant::now();
        if let Err(error) = crate::optical::refresh() {
            eprintln!("[-] Optical configuration: {error}");
        }
        true
    }
}

pub(crate) fn apply<T>(
    desired: (bool, bool),
    mut quiesce: impl FnMut() -> std::io::Result<()>,
    mut write: impl FnMut(bool, bool) -> std::io::Result<T>,
) -> std::io::Result<T> {
    let cleanup = if !desired.0 { quiesce() } else { Ok(()) };
    // Failure to drain workers must not prevent disabling transmission.
    let written = write(desired.0, desired.1);
    match (cleanup, written) {
        (Ok(()), result) => result,
        (Err(error), Ok(_)) => Err(error),
        (Err(cleanup), Err(write)) => Err(std::io::Error::other(format!(
            "cleanup: {cleanup}; GPON control: {write}"
        ))),
    }
}

fn cache() -> &'static Mutex<Option<fttr_api::PonMacStatus>> {
    static CACHE: OnceLock<Mutex<Option<fttr_api::PonMacStatus>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(None))
}

pub(crate) fn desired() -> (bool, bool) {
    let settings = crate::optical::settings();
    (
        settings.enabled && settings.downstream_tx_enabled,
        settings.enabled && settings.fec_enabled,
    )
}

pub(crate) fn record(result: std::io::Result<crate::platform::resources::PonMacReadback>) -> bool {
    let desired = desired();
    let (readback, error) = match result {
        Ok(value) => (Some(value), None),
        Err(error) => (None, Some(error.to_string())),
    };
    /*
     * A failed sample is not evidence that transmission stopped: the FPGA can
     * miss a single SPI transaction while the downstream PON engine is busy.
     * Fall back to the last good readback so `mac_tx_allowed()` survives a
     * transient hiccup instead of stalling the PLOAM engine, which would drop
     * every downstream ONU until fttrd is reloaded.
     */
    let mut cached = cache().lock().unwrap();
    let previous = cached.take();
    let (register, tx_enabled, fec_enabled, allowed) = match readback.as_ref() {
        Some(value) => (
            value.raw,
            value.tx_enabled,
            value.fec_enabled,
            value.tx_enabled == Some(true),
        ),
        None => match previous.as_ref() {
            Some(previous) => (
                previous.register,
                previous.tx_enabled,
                previous.fec_enabled,
                previous.tx_enabled == Some(true),
            ),
            None => (None, None, None, false),
        },
    };
    *cached = Some(fttr_api::PonMacStatus {
        desired_tx_enabled: desired.0,
        desired_fec_enabled: desired.1,
        register,
        tx_enabled,
        fec_enabled,
        error,
    });
    allowed
}

pub(crate) fn snapshot() -> fttr_api::PonMacStatus {
    let desired = desired();
    let mut status = cache()
        .lock()
        .unwrap()
        .clone()
        .unwrap_or(fttr_api::PonMacStatus {
            desired_tx_enabled: desired.0,
            desired_fec_enabled: desired.1,
            register: None,
            tx_enabled: None,
            fec_enabled: None,
            error: Some("awaiting_hardware_owner".into()),
        });
    status.desired_tx_enabled = desired.0;
    status.desired_fec_enabled = desired.1;
    status
}

#[cfg(test)]
mod tests {
    use super::apply;
    use std::cell::RefCell;

    #[test]
    fn disable_invalidates_workers_before_write_even_when_cleanup_fails() {
        for failed in [false, true] {
            let events = RefCell::new(Vec::new());
            let result = apply(
                (false, true),
                || {
                    events.borrow_mut().push("quiesce");
                    if failed {
                        Err(std::io::Error::other("carrier"))
                    } else {
                        Ok(())
                    }
                },
                |tx, fec| {
                    assert!(!tx);
                    assert!(fec);
                    events.borrow_mut().push("write");
                    Ok(8)
                },
            );
            assert_eq!(*events.borrow(), ["quiesce", "write"]);
            assert_eq!(result.is_err(), failed);
        }
    }

    #[test]
    fn fec_changes_with_transmission_enabled_do_not_drop_registrations() {
        for fec in [false, true] {
            assert!(apply(
                (true, fec),
                || panic!("must not quiesce active transmission"),
                |tx, actual_fec| {
                    assert!(tx);
                    assert_eq!(actual_fec, fec);
                    Ok(if fec { 10 } else { 2 })
                }
            )
            .is_ok());
        }
    }

    #[test]
    fn completion_rejects_unknown_or_mismatched_readback() {
        for (tx, fec) in [
            (None, Some(true)),
            (Some(false), Some(true)),
            (Some(true), None),
            (Some(true), Some(false)),
        ] {
            assert!(super::reconcile(
                (true, true),
                || panic!("active TX must not quiesce"),
                |_, _| Ok(crate::platform::resources::PonMacReadback {
                    raw: None,
                    tx_enabled: tx,
                    fec_enabled: fec
                })
            )
            .is_err());
        }
    }
}
