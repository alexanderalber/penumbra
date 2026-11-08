//! Data from and to other programs and devices: serial ports, UDP and TCP, as lines, JSON or bytes.
//!
//! - **`chan.data_in`** reads messages and makes the latest number of every name a channel, and
//!   the last messages a table (the names as the first row). The link's thread parses, and wakes
//!   the node when something arrived, so it cooks then and not every frame.
//! - **`chan.data_out`** sends its input's channels (as one message) and its table's rows (one
//!   message each), at most `rate` times a second: on change, or always.
//!
//! One kind each way with a `transport` param: the transports differ only in where the bytes go,
//! and a project switching from a USB cable to the network keeps its node, its wires and its
//! bindings. An in and an out naming the same serial port or TCP peer share one link (see
//! `penumbra-net`'s hub), so a request sent by one comes back to the other.

use std::sync::Arc;

use penumbra_core::{Channel, Chans, Error, Name, ParamSpec, Table, Unit, Value};
use penumbra_graph::{
    CookContext, CookError, Cooked, Data, DataKind, FnNodeType, NodeDef, NodeDesc, NodeTypes, Operator,
};
use penumbra_net::hub::{Endpoint, Hub, Reader, Writer};
use penumbra_net::parse::{MAX_MESSAGE, MAX_ROWS};
use penumbra_net::{Decode, Format, Split, encode, frame};

/// Kind of the input node.
pub const DATA_IN: &str = "chan.data_in";
/// Kind of the output node.
pub const DATA_OUT: &str = "chan.data_out";
/// The default UDP and TCP port.
pub const DEFAULT_PORT: i64 = 7000;
/// The default serial bit rate: what most Arduino examples use.
pub const DEFAULT_BAUD: i64 = 9600;

const IN_TRANSPORTS: [&str; 4] = ["udp", "tcp_listen", "tcp_connect", "serial"];
const OUT_TRANSPORTS: [&str; 4] = ["udp", "tcp_connect", "tcp_listen", "serial"];

fn transport_param(choices: &[&str], out: bool) -> ParamSpec {
    let p = ParamSpec::choice("transport", choices).doc(if out {
        "Where the messages go"
    } else {
        "Where the messages come from"
    });
    if out {
        p.choice_doc("udp", "UDP datagrams to `host` and `port`, one message each")
            .choice_doc(
                "tcp_connect",
                "a TCP connection to the server at `host` and `port`; reconnects when it drops",
            )
            .choice_doc("tcp_listen", "a TCP server on `port`: every client that connects gets the messages")
            .choice_doc("serial", "a serial port such as an Arduino's USB cable")
    } else {
        p.choice_doc("udp", "UDP datagrams sent to `port`")
            .choice_doc("tcp_listen", "a TCP server on `port`: programs connect to it and send")
            .choice_doc(
                "tcp_connect",
                "a TCP connection to the server at `host` and `port`; reconnects when it drops",
            )
            .choice_doc("serial", "a serial port such as an Arduino's USB cable")
    }
}

fn format_param() -> ParamSpec {
    ParamSpec::choice("format", &["lines", "json", "bytes"])
        .doc("What one message is")
        .choice_doc("lines", "a line of text: numbers split by commas or spaces, or `name:value`")
        .choice_doc("json", "one JSON value per line; over UDP a whole datagram, or each line of it")
        .choice_doc("bytes", "raw bytes, each 0 to 255")
}

fn common(desc: NodeDesc, out: bool) -> NodeDesc {
    desc.param(ParamSpec::text("host", if out { "127.0.0.1" } else { "" }).doc(
        "IP address of the other side, or localhost; `10.0.0.5:7000` sets the port too. Names are not looked up",
    ))
    .param(if out {
        ParamSpec::int("port", DEFAULT_PORT).clamp(0.0, 65535.0).range(1024.0, 65535.0).doc(
            "UDP or TCP port of the receiver; for tcp_listen, the port to listen on (there 0 picks any free port)",
        )
    } else {
        ParamSpec::int("port", DEFAULT_PORT).clamp(0.0, 65535.0).range(1024.0, 65535.0).special(0.0, "any free port").doc(
            "UDP or TCP port to listen on (net.status says which one 0 got); for tcp_connect, the server's port",
        )
    })
    .param(ParamSpec::bool("lan", false).doc(
        "Also take connections from other devices: listens on every network interface. Off: this machine only",
    ))
    .param(ParamSpec::text("serial", "").doc(
        "Serial port, such as COM3 or /dev/ttyACM0 (net.ports lists them). Empty takes the only one plugged in",
    ))
    .param(
        ParamSpec::int("baud", DEFAULT_BAUD)
            .clamp(50.0, 4_000_000.0)
            .range(1200.0, 1_000_000.0)
            .doc("Serial bit rate; match the device (Arduino: Serial.begin). Always 8 data bits, no parity, 1 stop bit"),
    )
    .param(format_param())
}

pub fn register(types: &mut NodeTypes) -> Result<(), Error> {
    let desc = NodeDesc::new(
        DATA_IN,
        "Numbers and text from a serial port, UDP or TCP (an Arduino, a sensor, another program) as channels and a table",
    )
    .block(penumbra_graph::Block::In)
    .param(transport_param(&IN_TRANSPORTS, false));
    let desc = common(desc, false)
        .param(
            ParamSpec::choice("split", &Split::NAMES)
                .doc("Where a line is split into fields")
                .choice_doc("auto", "at commas, semicolons and tabs; at spaces when the line has none of those")
                .choice_doc("comma", "at commas")
                .choice_doc("semicolon", "at semicolons")
                .choice_doc("tab", "at tabs")
                .choice_doc("space", "at runs of spaces and tabs")
                .choice_doc("none", "not at all: the whole line is one field"),
        )
        .param(ParamSpec::text("names", "").doc(
            "Space-separated names for the fields without a name, in order: `x y z` makes `1,2,3` channels x, y and z. \
             `-` leaves a field out. Fields past the names are chan3, chan4 and so on",
        ))
        .param(ParamSpec::text("delimiter", "\\n").doc(
            "Where a message ends: `\\n`, `\\r`, `;`, `\\xff`. For lines and JSON a `\\r` before the `\\n` is ignored. \
             Over UDP a datagram is a message; lines in one are split at it too",
        ))
        .param(ParamSpec::int("length", 0).clamp(0.0, 65536.0).range(0.0, 64.0).special(0.0, "each read").more().doc(
            "For bytes on a stream with an empty `delimiter`: every this many bytes is one message. 0 takes each read as it comes",
        ))
        .param(
            ParamSpec::int("rows", 16)
                .clamp(1.0, f64::from(u32::try_from(MAX_ROWS).unwrap_or(1024)))
                .range(1.0, 100.0)
                .doc("Messages the table keeps, the newest last"),
        )
        .param(
            ParamSpec::int("max", 65536)
                .clamp(16.0, f64::from(u32::try_from(MAX_MESSAGE).unwrap_or(1 << 20)))
                .range(256.0, 65536.0)
                .more()
                .doc("Longest message in bytes; longer ones are dropped and counted"),
        )
        .output("out", DataKind::Chan)
        .output("table", DataKind::Table)
        .describe_output("out", "A channel per name holding its latest number, in the order the names first came")
        .describe_output("table", "The last messages: the names as the first row, then one row per message");
    types.register(
        FnNodeType::new(desc, || Box::new(DataIn::default()) as Box<dyn Operator>).inactive(inactive_in),
    )?;

    let desc = NodeDesc::new(
        DATA_OUT,
        "Sends channels and table rows to a serial port, UDP or TCP, as lines, JSON or bytes",
    )
    .param(transport_param(&OUT_TRANSPORTS, true));
    let desc = common(desc, true)
        .param(
            ParamSpec::choice("separator", &["comma", "semicolon", "tab", "space"])
                .doc("What goes between the values of a line")
                .choice_doc("comma", "`0.5,1`")
                .choice_doc("semicolon", "`0.5;1`")
                .choice_doc("tab", "a tab")
                .choice_doc("space", "`0.5 1`"),
        )
        .param(ParamSpec::bool("names", false).doc(
            "Lines: each channel as `name:value`. Off: the values alone, in order. JSON always has the names",
        ))
        .param(ParamSpec::text("delimiter", "\\n").doc(
            "What ends each message on a serial port or TCP: `\\n`, `\\r\\n`, `;`, empty for none. UDP sends each message as one datagram without it",
        ))
        .param(
            ParamSpec::float("rate", 30.0)
                .clamp(0.1, 1000.0)
                .range(1.0, 120.0)
                .unit(Unit::Hertz)
                .doc("Sends per second at most"),
        )
        .param(
            ParamSpec::choice("send", &["changes", "always"])
                .doc("When to send")
                .choice_doc(
                    "changes",
                    "when a message changed; over UDP everything again once a second, in case a datagram was lost",
                )
                .choice_doc("always", "everything, `rate` times a second"),
        )
        .input("chans", DataKind::Chan)
        .input("table", DataKind::Table)
        .time_dependent();
    types.register(
        FnNodeType::new(desc, || Box::new(DataOut::default()) as Box<dyn Operator>).inactive(inactive_out),
    )?;
    Ok(())
}

fn inactive_in(node: &NodeDef, desc: &NodeDesc, name: &str) -> Option<&'static str> {
    let t = node.param(desc, "transport").and_then(Value::as_str).unwrap_or("udp");
    let f = node.param(desc, "format").and_then(Value::as_str).unwrap_or("lines");
    match name {
        "host" if t != "tcp_connect" => {
            Some("Only tcp_connect connects to a host; the others listen or use a serial port")
        }
        "port" if t == "serial" => Some("A serial port is named in `serial`"),
        "lan" if !matches!(t, "udp" | "tcp_listen") => Some("Only udp and tcp_listen listen"),
        "serial" | "baud" if t != "serial" => Some("Only for transport serial"),
        "split" if f != "lines" => Some("Only lines are split into fields"),
        "delimiter" if f == "bytes" && t == "udp" => Some("Over UDP each datagram is one message of bytes"),
        "length" if f != "bytes" || t == "udp" => Some("Only for bytes on a serial port or TCP"),
        _ => None,
    }
}

fn inactive_out(node: &NodeDef, desc: &NodeDesc, name: &str) -> Option<&'static str> {
    let t = node.param(desc, "transport").and_then(Value::as_str).unwrap_or("udp");
    let f = node.param(desc, "format").and_then(Value::as_str).unwrap_or("lines");
    match name {
        "host" if !matches!(t, "udp" | "tcp_connect") => Some("Only udp and tcp_connect send to a host"),
        "port" if t == "serial" => Some("A serial port is named in `serial`"),
        "lan" if t != "tcp_listen" => Some("Only tcp_listen listens"),
        "serial" | "baud" if t != "serial" => Some("Only for transport serial"),
        "separator" | "names" if f != "lines" => Some("Only for lines"),
        "delimiter" if t == "udp" => Some("UDP sends each message as one datagram"),
        _ => None,
    }
}

fn hub_of<'a>(cx: &'a CookContext<'_>) -> Result<&'a Hub, CookError> {
    cx.resource::<Hub>()
        .ok_or_else(|| CookError::failed("serial, UDP and TCP are not available in this process"))
}

/// Where the params point. `out` decides what `udp` means: listening in, sending out.
fn endpoint_of(cx: &CookContext<'_>, out: bool) -> Result<Endpoint, CookError> {
    let port = u16::try_from(cx.int("port")?).map_err(|_| CookError::failed("`port` is 0 to 65535"))?;
    let lan = cx.bool("lan")?;
    let to = |cx: &CookContext<'_>| -> Result<std::net::SocketAddr, CookError> {
        let host = cx.text("host")?.trim();
        if host.is_empty() {
            return Err(CookError::failed("`host` is empty: type the IP address of the other side"));
        }
        let a = crate::dmx::address(host, port).map_err(CookError::failed)?;
        if a.port() == 0 {
            return Err(CookError::failed("sending needs a port other than 0"));
        }
        Ok(a)
    };
    Ok(match cx.text("transport")? {
        "udp" if out => Endpoint::UdpTo(to(cx)?),
        "udp" => Endpoint::Udp { lan, port },
        "tcp_listen" => Endpoint::TcpListen { lan, port },
        "tcp_connect" => Endpoint::TcpConnect(to(cx)?),
        "serial" => Endpoint::Serial {
            name: cx.text("serial")?.trim().to_string(),
            baud: u32::try_from(cx.int("baud")?)
                .map_err(|_| CookError::failed("`baud` is a positive number"))?,
        },
        other => return Err(CookError::failed(format!("unknown transport `{other}`"))),
    })
}

fn format_of(cx: &CookContext<'_>) -> Result<Format, CookError> {
    Ok(match cx.text("format")? {
        "lines" => Format::Lines,
        "json" => Format::Json,
        "bytes" => Format::Bytes,
        other => return Err(CookError::failed(format!("unknown format `{other}`"))),
    })
}

fn delimiter_of(cx: &CookContext<'_>) -> Result<Vec<u8>, CookError> {
    frame::delimiter(cx.text("delimiter")?).map_err(|e| CookError::failed(format!("`delimiter`: {e}")))
}

fn decode_of(cx: &CookContext<'_>) -> Result<Decode, CookError> {
    let split = Split::parse(cx.text("split")?).ok_or_else(|| CookError::failed("unknown `split`"))?;
    let format = format_of(cx)?;
    let mut delimiter = delimiter_of(cx)?;
    let mut max = usize::try_from(cx.int("max")?).unwrap_or(MAX_MESSAGE).clamp(16, MAX_MESSAGE);
    // A line ending in `\r\n` is cut at the `\n`; the `\r` goes with the whitespace. Bytes keep it.
    if format != Format::Bytes && delimiter == b"\r\n" {
        delimiter = b"\n".to_vec();
        max += 1;
    }
    Ok(Decode {
        format,
        split,
        names: cx.text("names")?.split_whitespace().map(str::to_string).collect(),
        delimiter,
        length: usize::try_from(cx.int("length")?).unwrap_or(0),
        max,
        rows: usize::try_from(cx.int("rows")?).unwrap_or(16).clamp(1, MAX_ROWS),
    })
}

/// `1 message`, `3 messages`.
fn count_messages(n: u64) -> String {
    if n == 1 { "1 message".to_string() } else { format!("{n} messages") }
}

/// A warning for a serial port another node opened at another rate.
fn baud_clash(endpoint: &Endpoint, status_baud: Option<u32>) -> Option<String> {
    match (endpoint, status_baud) {
        (Endpoint::Serial { baud, .. }, Some(open)) if *baud != open => {
            Some(format!("the port is open at {open} baud for another node; `baud` {baud} does not count"))
        }
        _ => None,
    }
}

#[derive(Default)]
struct DataIn {
    reader: Option<Reader>,
    /// The store version, open state and error of the last cook.
    seen: Option<(u64, bool, Option<String>)>,
}

impl Operator for DataIn {
    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        let hub = hub_of(cx)?;
        let endpoint = endpoint_of(cx, false)?;
        let decode = decode_of(cx)?;
        if self.reader.as_ref().is_none_or(|r| *r.endpoint() != endpoint) {
            // The old one first, so a port it holds is free for the new one.
            self.reader = None;
            self.seen = None;
            let waker = cx.waker();
            let r = hub
                .reader(
                    cx.def().name.as_str(),
                    endpoint.clone(),
                    decode.clone(),
                    Arc::new(move || waker.wake()),
                )
                .map_err(CookError::failed)?;
            self.reader = Some(r);
        }
        let Some(reader) = &self.reader else { return Ok(Cooked::Outputs(Vec::new())) };
        reader.configure(&decode);
        let version = reader.version();
        let status = reader.status();
        let seen = (version, status.open, status.error.clone());
        let unchanged = !cx.params_changed() && !cx.first_cook() && self.seen.as_ref() == Some(&seen);
        let store = reader.store();
        let mut warnings = Vec::new();
        if let Some(w) = baud_clash(&endpoint, status.baud) {
            warnings.push(w);
        }
        let c = &store.counts;
        let dropped = c.malformed + c.too_long;
        if dropped > 0 {
            let why = c.last_problem.as_deref().unwrap_or("");
            warnings.push(format!("{} dropped (last: {why})", count_messages(dropped)));
        }
        if c.over_cap > 0 {
            warnings
                .push(format!("{} names left out: a node keeps 1024 channels and 256 columns", c.over_cap));
        }
        if let Some(e) = &status.error {
            if c.messages == 0 {
                if !warnings.is_empty() {
                    cx.warn(warnings.join("; "));
                }
                return Err(CookError::failed(e.clone()));
            }
            warnings.push(format!("{e}; holding the last values"));
        } else if c.messages == 0 && status.open {
            warnings.push(format!("nothing received yet on {endpoint}"));
        } else if !status.open {
            warnings.push(format!("opening {endpoint}"));
        }
        if !warnings.is_empty() {
            cx.warn(warnings.join("; "));
        }
        if unchanged {
            return Ok(Cooked::Unchanged);
        }
        self.seen = Some(seen);
        let mut channels = Vec::with_capacity(store.values.len());
        for (name, v) in &store.values {
            let Ok(n) = Name::new(name) else { continue };
            channels.push(Channel { name: n, samples: vec![*v] });
        }
        let chans = Chans::new(cx.rate().as_f64(), channels)?;
        Ok(Cooked::Outputs(vec![
            Data::new(DataKind::Chan, chans),
            Data::new(DataKind::Table, Table::new(store.table())?),
        ]))
    }
}

#[derive(Default)]
struct DataOut {
    writer: Option<Writer>,
    /// Show time of the last send, in seconds.
    last: Option<f64>,
    /// Show time of the last full send over UDP.
    last_full: Option<f64>,
    /// What was sent last.
    sent: Vec<Vec<u8>>,
    /// Why the last send dropped messages; shown until a send goes through.
    dropped: Option<String>,
    /// The last cook's warning, shown again on the frames that send nothing (a warning lasts
    /// one cook).
    warning: Option<String>,
}

/// Messages one cook sends at most: a table's rows beyond are left out and counted.
pub const MAX_MESSAGES: usize = penumbra_net::hub::MAX_MESSAGES;

fn separator_of(text: &str) -> &'static str {
    match text {
        "semicolon" => ";",
        "tab" => "\t",
        "space" => " ",
        _ => ",",
    }
}

impl Operator for DataOut {
    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        let hub = hub_of(cx)?;
        let endpoint = endpoint_of(cx, true)?;
        if self.writer.as_ref().is_none_or(|w| *w.endpoint() != endpoint) {
            self.writer = None;
            self.sent.clear();
            let w = hub.writer(cx.def().name.as_str(), endpoint.clone()).map_err(CookError::failed)?;
            self.writer = Some(w);
        }
        if cx.input(0).is_none() && cx.input(1).is_none() {
            return Err(crate::waiting(cx));
        }
        let now = cx.time().seconds();
        let period = 1.0 / cx.float("rate")?.max(0.1);
        // A little slack, so 30 Hz at 60 fps sends every second frame and not every third.
        if let Some(last) = self.last
            && now >= last
            && now - last < period * 0.9
            && !cx.params_changed()
        {
            if let Some(w) = &self.warning {
                cx.warn(w.clone());
            }
            return Ok(Cooked::Outputs(Vec::new()));
        }
        self.last = Some(now);
        let format = format_of(cx)?;
        let separator = separator_of(cx.text("separator")?);
        let names = cx.bool("names")?;
        let delimiter = delimiter_of(cx)?;
        let mut messages: Vec<Vec<u8>> = Vec::new();
        if let Some(chans) = cx.input_as::<Chans>(0)
            && !chans.channels().is_empty()
        {
            let values: Vec<(&str, f64)> =
                chans.channels().iter().map(|c| (c.name.as_str(), c.value())).collect();
            messages.push(match format {
                Format::Lines => encode::chans_line(&values, separator, names),
                Format::Json => encode::chans_json(&values),
                Format::Bytes => encode::chans_bytes(&values),
            });
        }
        let mut left_out = 0;
        if let Some(table) = cx.input_as::<Table>(1) {
            let room = MAX_MESSAGES.saturating_sub(messages.len());
            left_out = table.rows().len().saturating_sub(room);
            for row in table.rows().iter().take(room) {
                messages.push(match format {
                    Format::Lines => encode::row_line(row, separator),
                    Format::Json => encode::row_json(row),
                    Format::Bytes => encode::row_bytes(row),
                });
            }
        }
        let refresh = endpoint.datagrams() && self.last_full.is_none_or(|t| now < t || now - t >= 1.0);
        let send = cx.text("send")? == "always" || refresh || cx.params_changed() || messages != self.sent;
        let Some(writer) = &self.writer else { return Ok(Cooked::Outputs(Vec::new())) };
        let mut warnings = Vec::new();
        if left_out > 0 {
            warnings.push(format!(
                "{left_out} table rows left out: one send carries {MAX_MESSAGES} messages at most"
            ));
        }
        if send {
            if endpoint.datagrams() {
                self.last_full = Some(now);
            }
            let r = writer.send(&messages, &delimiter);
            let listening_alone = matches!(endpoint, Endpoint::TcpListen { .. }) && r.unconnected;
            self.dropped = match &r.reason {
                Some(why) if !listening_alone => {
                    let n = u64::try_from(r.dropped.max(1)).unwrap_or(u64::MAX);
                    Some(format!("{} dropped: {why}", count_messages(n)))
                }
                _ => None,
            };
            // A connection whose queue was full did not get them: try again at the next send.
            if !r.full {
                self.sent = messages;
            }
        }
        if let Some(d) = &self.dropped {
            warnings.push(d.clone());
        }
        let status = writer.status();
        if let Some(w) = baud_clash(&endpoint, status.baud) {
            warnings.push(w);
        }
        if let Some(e) = status.error.filter(|_| !status.open) {
            warnings.push(e);
        }
        self.warning = (!warnings.is_empty()).then(|| warnings.join("; "));
        if let Some(w) = &self.warning {
            cx.warn(w.clone());
        }
        Ok(Cooked::Outputs(Vec::new()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn separators() {
        assert_eq!(separator_of("comma"), ",");
        assert_eq!(separator_of("tab"), "\t");
        assert_eq!(separator_of("anything"), ",");
    }
}
