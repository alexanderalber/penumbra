//! The Enttec Open DMX USB: an FTDI FT232R behind an RS-485 driver, no microcontroller. The
//! host makes the whole signal through the serial port (FTDI's virtual COM port driver on
//! Windows, `ftdi_sio` on Linux): 250 kbaud, 8 data bits, no parity, 2 stop bits; a break (the
//! line held low for at least 88 us), the mark after it, then the start code 0 and 512 slots.
//! Without a microcontroller nothing repeats the frame, so a thread sends it again and again.
//!
//! The break goes through the driver as a USB request, so it lasts at least as long as the
//! sleep between setting and clearing it plus the USB round trips (the two calls take about
//! 3 ms here: far above the 88 us minimum, far below the one second maximum). The mark after the break is
//! the gap until the first data byte leaves, again at least one USB frame. A break must not
//! start before the previous frame has left the chip, or it cuts the last slots short; so after
//! writing, [`Line::send`] waits for the driver to drain (flush) and in any case until the
//! frame's time on the wire has passed.

use std::io::Write;
use std::time::{Duration, Instant};

use serde::Serialize;
use serialport::{DataBits, FlowControl, Parity, SerialPort, SerialPortType, StopBits};

use crate::SLOTS;

/// The DMX512 bit rate.
pub const BAUD: u32 = 250_000;
/// FTDI's USB vendor id.
pub const FTDI_VID: u16 = 0x0403;
/// The FT232R's product id, which the Open DMX USB keeps (Enttec did not give it its own).
pub const FT232R_PID: u16 = 0x6001;
/// How long the break is asked to last. The USB round trips add to it.
pub const BREAK: Duration = Duration::from_micros(150);
/// One frame on the wire: start code and 512 slots of 11 bits each at 250 kbaud.
pub const FRAME_ON_WIRE: Duration = Duration::from_micros(11 * 4 * (SLOTS as u64 + 1));
/// Added to [`FRAME_ON_WIRE`] before the next break may start: the mark after the break and
/// the last USB packet still in the chip.
pub const DRAIN_MARGIN: Duration = Duration::from_millis(2);
/// How long a write may block before it counts as failed (a full frame takes 23 ms).
const WRITE_TIMEOUT: Duration = Duration::from_millis(250);

/// A serial port of this machine, for choosing one.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PortInfo {
    /// What to open: `COM5`, `/dev/ttyUSB0`.
    pub name: String,
    /// USB vendor and product id, when it is a USB device.
    pub vid: Option<u16>,
    pub pid: Option<u16>,
    pub serial: Option<String>,
    pub product: Option<String>,
    /// An FTDI FT232R, as the Open DMX USB is (other FT232R adapters look the same).
    pub open_dmx: bool,
}

/// The serial ports of this machine, FT232R ones first.
pub fn ports() -> Result<Vec<PortInfo>, String> {
    // Without libudev, the Linux listing `expect`s `/sys/class/tty` to exist.
    let list = std::panic::catch_unwind(serialport::available_ports)
        .map_err(|_| "cannot list the serial ports".to_string())?
        .map_err(|e| format!("cannot list the serial ports: {e}"))?;
    let mut out: Vec<PortInfo> = list
        .into_iter()
        .map(|p| match p.port_type {
            SerialPortType::UsbPort(u) => PortInfo {
                name: p.port_name,
                vid: Some(u.vid),
                pid: Some(u.pid),
                serial: u.serial_number,
                product: u.product,
                open_dmx: u.vid == FTDI_VID && u.pid == FT232R_PID,
            },
            _ => PortInfo {
                name: p.port_name,
                vid: None,
                pid: None,
                serial: None,
                product: None,
                open_dmx: false,
            },
        })
        .collect();
    out.sort_by(|a, b| b.open_dmx.cmp(&a.open_dmx).then_with(|| a.name.cmp(&b.name)));
    Ok(out)
}

/// The first FT232R's port, the Open DMX USB if one is plugged in.
pub fn find_open_dmx() -> Option<String> {
    ports().ok()?.into_iter().find(|p| p.open_dmx).map(|p| p.name)
}

/// How long the parts of one frame took, as seen from the host.
#[derive(Clone, Copy, Debug)]
pub struct Timing {
    /// From asking for the break to the call that clears it returning.
    pub brk: Duration,
    /// Writing the frame until the driver had sent it.
    pub write: Duration,
}

/// An open Open DMX USB.
pub struct Line {
    port: Box<dyn SerialPort>,
    name: String,
}

impl Line {
    /// Opens `name` with DMX's line settings.
    pub fn open(name: &str) -> Result<Line, String> {
        let mut port = serialport::new(name, BAUD)
            .data_bits(DataBits::Eight)
            .parity(Parity::None)
            .stop_bits(StopBits::Two)
            .flow_control(FlowControl::None)
            .timeout(WRITE_TIMEOUT)
            .open()
            .map_err(|e| format!("cannot open {name}: {e}"))?;
        // As Enttec's and QLC+'s drivers do; RTS is not wired to the line driver on every
        // revision, but low is the documented state.
        port.write_request_to_send(false).ok();
        port.clear(serialport::ClearBuffer::All).ok();
        Ok(Line { port, name: name.to_string() })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// Sends one frame: break, mark, start code 0 and `slots` (cut or padded to 512).
    pub fn send(&mut self, slots: &[u8]) -> Result<Timing, String> {
        let mut frame = [0u8; SLOTS + 1];
        for (d, s) in frame.iter_mut().skip(1).zip(slots) {
            *d = *s;
        }
        let t0 = Instant::now();
        self.port.set_break().map_err(|e| self.failed("break", e))?;
        std::thread::sleep(BREAK);
        self.port.clear_break().map_err(|e| self.failed("break", e))?;
        let t1 = Instant::now();
        self.port.write_all(&frame).map_err(|e| self.failed("write", e))?;
        self.port.flush().map_err(|e| self.failed("write", e))?;
        // The driver reports the data as sent while the last of it still sits in the chip
        // (measured with FTDI's VCP driver: 20.4 ms for a frame that takes 22.6 ms on the wire,
        // from a mark after the break of unknown length). A break then would cut the last
        // slots, so none starts before the frame and a margin can have left.
        let on_wire = t1.elapsed();
        if let Some(rest) = (FRAME_ON_WIRE + DRAIN_MARGIN).checked_sub(on_wire) {
            std::thread::sleep(rest);
        }
        Ok(Timing { brk: t1 - t0, write: on_wire })
    }

    fn failed(&self, what: &str, e: impl std::fmt::Display) -> String {
        format!("{} {what}: {e}", self.name)
    }
}
