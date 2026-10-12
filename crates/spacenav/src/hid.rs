//! The reader thread on Windows, through hidapi's native backend.

use std::time::{Duration, Instant};

use hidapi::HidApi;

use crate::{PRODUCT_ID, Shared, State, VENDOR_ID};

/// How long a read waits; also how quickly the thread notices it should stop.
const READ_TIMEOUT_MS: i32 = 100;
/// Between two looks for an absent device.
const SEARCH_EVERY: Duration = Duration::from_secs(1);
/// After a device that would not open.
const RETRY_AFTER: Duration = Duration::from_secs(3);
/// Read errors in a row that mean the device is gone.
const MAX_ERRORS: u32 = 5;

/// HID usage page and usage of a multi-axis controller: the interface with the axes, where a
/// device lists several.
const GENERIC_DESKTOP: u16 = 0x01;
const MULTI_AXIS: u16 = 0x08;

pub(crate) fn run(shared: &Shared) {
    // One HidApi per process; it lives as long as the thread.
    let mut api = match HidApi::new() {
        Ok(a) => a,
        Err(e) => {
            shared.set_state(State::Failed, None, Some(format!("HID is not available: {e}")));
            return;
        }
    };
    shared.set_state(State::Searching, None, None);
    while !shared.stopping() {
        if let Err(e) = api.refresh_devices() {
            shared.set_state(State::Searching, None, Some(format!("cannot list HID devices: {e}")));
            if !shared.pause(SEARCH_EVERY) {
                return;
            }
            continue;
        }
        let mut found: Vec<&hidapi::DeviceInfo> = api
            .device_list()
            .filter(|d| d.vendor_id() == VENDOR_ID && d.product_id() == PRODUCT_ID)
            .collect();
        found.sort_by_key(|d| !(d.usage_page() == GENERIC_DESKTOP && d.usage() == MULTI_AXIS));
        let Some(info) = found.first() else {
            shared.set_state(State::Searching, None, Some("not plugged in".into()));
            if !shared.pause(SEARCH_EVERY) {
                return;
            }
            continue;
        };
        let product = info.product_string().map(str::to_string);
        let device = match info.open_device(&api) {
            Ok(d) => d,
            Err(e) => {
                shared.set_state(
                    State::Failed,
                    product,
                    Some(format!("cannot open it ({e}); 3DxWare may be holding it")),
                );
                if !shared.pause(RETRY_AFTER) {
                    return;
                }
                continue;
            }
        };
        shared.set_state(State::Connected, product, None);
        let mut buf = [0u8; 64];
        let mut errors = 0;
        while !shared.stopping() {
            match device.read_timeout(&mut buf, READ_TIMEOUT_MS) {
                Ok(0) => {}
                Ok(n) => {
                    errors = 0;
                    shared.report(buf.get(..n).unwrap_or(&[]), Instant::now());
                }
                Err(e) => {
                    errors += 1;
                    if errors >= MAX_ERRORS {
                        shared.set_state(State::Searching, None, Some(format!("unplugged ({e})")));
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
            }
        }
    }
}
