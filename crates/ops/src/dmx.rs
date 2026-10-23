//! `out.dmx`: channels to DMX slots, through an Enttec Open DMX USB or over the network
//! (Art-Net, sACN, DDP). `chan.dmx_in`: DMX slots from the network (Art-Net, sACN) as channels.
//!
//! The node only hands its slots to the [`Hub`]; a thread per target sends them again and again
//! at `rate`, also while the show is paused or nothing changes (the Open DMX USB has no memory,
//! sACN receivers drop a silent source). So the node cooks when its input or params change, and
//! when its output opens, closes or fails (the hub wakes it), to show that. Removing the node
//! stops what it sent; nodes sending to the same target share it by their slot ranges.
//!
//! `chan.dmx_in` asks the [`Inputs`] for its universes; the listener's thread wakes it when one of
//! them changes, so it cooks when something arrived, not every frame.

use std::collections::BTreeSet;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;

use penumbra_core::{Channel, Chans, Error, Name, ParamSpec, Unit, Value};
use penumbra_dmx::hub::{Hub, Output, Target};
use penumbra_dmx::input::{self, Input, Inputs, Listen, Protocol, Reading};
use penumbra_dmx::{SLOTS, artnet, ddp, sacn};
use penumbra_graph::{
    CookContext, CookError, Cooked, Data, DataKind, FnNodeType, NodeDef, NodeDesc, NodeTypes, Operator,
};

/// Kind of the DMX output node.
pub const DMX: &str = "out.dmx";
/// Kind of the DMX input node.
pub const DMX_IN: &str = "chan.dmx_in";
/// The rate the Open DMX USB can keep up at most with a full universe: break calls about 3 ms,
/// the frame 22.6 ms on the wire, a 2 ms margin.
pub const OPEN_DMX_MAX_RATE: f64 = 35.0;

pub fn register(types: &mut NodeTypes) -> Result<(), Error> {
    types.register(FnNodeType::new(
        NodeDesc::new(DMX, "Channels to DMX slots: Enttec Open DMX USB, Art-Net, sACN or DDP")
            .param(
                ParamSpec::choice("protocol", &["open_dmx", "art_net", "sacn", "ddp"])
                    .doc("How the slots leave this machine")
                    .choice_doc("open_dmx", "an Enttec Open DMX USB on a serial port, one universe")
                    .choice_doc("art_net", "Art-Net over UDP, to a node or as a broadcast")
                    .choice_doc("sacn", "sACN (E1.31) over UDP, to each universe's multicast group or a host")
                    .choice_doc("ddp", "DDP over UDP to a pixel controller such as WLED, no universes"),
            )
            .param(ParamSpec::text("port", "").doc(
                "Serial port of the Open DMX USB, such as COM5 or /dev/ttyUSB0; empty takes the first \
                 FTDI FT232R plugged in (dmx.ports lists them)",
            ))
            .param(ParamSpec::text("host", "").doc(
                "IP address to send to, optionally with :port; empty broadcasts Art-Net and sends sACN \
                 to the multicast groups; DDP needs one",
            ))
            .param(ParamSpec::int("universe", 1).clamp(0.0, 63999.0).range(0.0, 16.0).doc(
                "Universe of the first slot, as the protocol counts (Art-Net from 0, sACN from 1); \
                 more than 512 slots spill into the next. Not used by the Open DMX USB and DDP",
            ))
            .param(
                ParamSpec::int("address", 1)
                    .clamp(1.0, 512.0)
                    .range(1.0, 512.0)
                    .doc("DMX address (slot, from 1) the first channel goes to; for DDP the byte, from 1"),
            )
            .param(
                ParamSpec::choice("values", &["unit", "byte"])
                    .doc("What the channel values mean")
                    .choice_doc("unit", "0 to 1, sent as 0 to 255")
                    .choice_doc("byte", "0 to 255, sent as they are (rounded)"),
            )
            .param(ParamSpec::float("rate", 30.0).clamp(0.5, 1000.0).range(1.0, 60.0).unit(Unit::Hertz).doc(
                "Frames per second, sent whether or not anything changed; the Open DMX USB manages about 35",
            ))
            .input("chans", DataKind::Chan),
        || Box::new(DmxOut::default()) as Box<dyn Operator>,
    )
    .inactive(inactive))?;
    types.register(FnNodeType::new(
        NodeDesc::new(
            DMX_IN,
            "DMX slots from the network (Art-Net or sACN) as channels, from a lighting desk say",
        )
        .param(
            ParamSpec::choice("protocol", &["art_net", "sacn"])
                .doc("What the sender speaks")
                .choice_doc("art_net", "Art-Net, sent to this machine or as a broadcast")
                .choice_doc("sacn", "sACN (E1.31), to the universe's multicast group or to this machine"),
        )
        .param(ParamSpec::int("universe", 1).clamp(0.0, 63999.0).range(0.0, 16.0).doc(
            "Universe of the first slot, as the protocol counts (Art-Net from 0, sACN from 1); \
                 channels past its end read on in the next",
        ))
        .param(
            ParamSpec::int("address", 1)
                .clamp(1.0, 512.0)
                .range(1.0, 512.0)
                .doc("DMX address (slot, from 1) that becomes ch1"),
        )
        .param(
            ParamSpec::int("count", 512)
                .clamp(1.0, MAX_IN_CHANNELS)
                .range(1.0, 512.0)
                .doc("Channels made: ch1 is slot `address`, ch2 the next and so on"),
        )
        .param(
            ParamSpec::choice("values", &["unit", "byte"])
                .doc("What the channels hold")
                .choice_doc("unit", "0 to 1, from 0 to 255")
                .choice_doc("byte", "0 to 255, as the slots are"),
        )
        .param(ParamSpec::text("interface", "").more().doc(
            "IP address of this machine's network card to listen on; empty listens on all \
                 (sACN multicast then joins on the system's default one)",
        ))
        .param(
            ParamSpec::int("port", 0)
                .clamp(0.0, 65535.0)
                .range(1024.0, 65535.0)
                .special(0.0, "the protocol's port")
                .more()
                .doc("UDP port to listen on; 0 is the protocol's own (Art-Net 6454, sACN 5568)"),
        )
        .output("out", DataKind::Chan)
        .describe_output("out", "ch1 to chN: the slots from `address` on, the last values held"),
        || Box::new(DmxIn::default()) as Box<dyn Operator>,
    ))?;
    Ok(())
}

/// Why param `name` of an `out.dmx` does not count with its protocol. `None` while it counts.
fn inactive(node: &NodeDef, desc: &NodeDesc, name: &str) -> Option<&'static str> {
    let protocol = node.param(desc, "protocol").and_then(Value::as_str).unwrap_or("open_dmx");
    match (protocol, name) {
        ("open_dmx", "host") => Some("The Open DMX USB sits on a serial port, see `port`"),
        ("open_dmx", "universe") => Some("The Open DMX USB carries one universe"),
        ("ddp", "universe") => Some("DDP has no universes: `address` is the byte the first channel goes to"),
        ("art_net" | "sacn" | "ddp", "port") => Some("Only the Open DMX USB uses a serial port"),
        _ => None,
    }
}

#[derive(Default)]
struct DmxOut {
    output: Option<Output>,
}

/// `host` as an address: an IP address or `localhost`, optionally with `:port` (an IPv6 address
/// with a port in brackets). Names are not looked up, so the cook thread never waits for DNS.
pub fn address(host: &str, default_port: u16) -> Result<SocketAddr, String> {
    let host = host.trim();
    if let Ok(a) = host.parse::<SocketAddr>() {
        return Ok(a);
    }
    if let Ok(ip) = host.parse::<IpAddr>() {
        return Ok(SocketAddr::new(ip, default_port));
    }
    let (name, port) = match host.rsplit_once(':') {
        Some((n, p)) => (n, p.parse::<u16>().map_err(|_| format!("`{p}` is not a port"))?),
        None => (host, default_port),
    };
    if name.eq_ignore_ascii_case("localhost") {
        return Ok(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port));
    }
    Err(format!("`{host}` is not an IP address (names are not looked up)"))
}

/// The target the params name, and the first slot as [`Output::set`] counts it.
fn target_of(cx: &CookContext<'_>) -> Result<(Target, usize), CookError> {
    let host = cx.text("host")?.trim();
    let universe = cx.int("universe")?;
    let address = usize::try_from(cx.int("address")?.clamp(1, 512) - 1).unwrap_or(0);
    let spread = |max: u16| -> Result<usize, CookError> {
        let u = u16::try_from(universe)
            .ok()
            .filter(|u| *u <= max)
            .ok_or_else(|| CookError::failed(format!("`universe` is at most {max} here")))?;
        Ok(usize::from(u) * SLOTS + address)
    };
    match cx.text("protocol")? {
        "open_dmx" => Ok((Target::OpenDmx { port: cx.text("port")?.trim().to_string() }, address)),
        "art_net" => {
            let to = if host.is_empty() {
                SocketAddr::new(IpAddr::V4(Ipv4Addr::BROADCAST), artnet::PORT)
            } else {
                address_of(host, artnet::PORT)?
            };
            Ok((Target::ArtNet { to }, spread(artnet::MAX_UNIVERSE)?))
        }
        "sacn" => {
            if universe < 1 {
                return Err(CookError::failed("sACN universes start at 1"));
            }
            let to = if host.is_empty() { None } else { Some(address_of(host, sacn::PORT)?) };
            Ok((Target::Sacn { to }, spread(sacn::MAX_UNIVERSE)?))
        }
        "ddp" => {
            if host.is_empty() {
                return Err(CookError::failed("DDP needs a `host`: the controller's IP address"));
            }
            Ok((Target::Ddp { to: address_of(host, ddp::PORT)? }, address))
        }
        other => Err(CookError::failed(format!("unknown protocol `{other}`"))),
    }
}

fn address_of(host: &str, port: u16) -> Result<SocketAddr, CookError> {
    address(host, port).map_err(CookError::failed)
}

/// A channel value as a slot.
pub fn slot(value: f64, unit: bool) -> u8 {
    let v = if unit { value * 255.0 } else { value };
    if v.is_nan() {
        return 0;
    }
    // `as`: clamped to 0..=255 first.
    v.round().clamp(0.0, 255.0) as u8
}

impl Operator for DmxOut {
    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        let hub =
            cx.resource::<Hub>().ok_or_else(|| CookError::failed("DMX is not available in this process"))?;
        let (target, start) = target_of(cx)?;
        let Some(chans) = cx.input_as::<Chans>(0) else {
            return Err(crate::waiting(cx));
        };
        if self.output.as_ref().is_none_or(|o| *o.target() != target) {
            // The old one first, so a port it holds is free for the new one.
            self.output = None;
            let waker = cx.waker();
            let out = hub
                .output(cx.def().name.as_str(), target, Arc::new(move || waker.wake()))
                .map_err(CookError::failed)?;
            self.output = Some(out);
        }
        let Some(out) = &self.output else { return Ok(Cooked::Outputs(Vec::new())) };
        let unit = cx.text("values")? == "unit";
        let slots: Vec<u8> = chans.channels().iter().map(|c| slot(c.value(), unit)).collect();
        let rate = cx.float("rate")?;
        out.set(start, &slots, rate);
        let st = out.status();
        let mut warnings = Vec::new();
        if st.dropped > 0 {
            warnings.push(match out.target() {
                Target::OpenDmx { .. } => {
                    format!("{} slots past 512 left out: the Open DMX USB carries one universe", st.dropped)
                }
                _ => format!("{} slots past what the protocol carries left out", st.dropped),
            });
        }
        if st.overlap > 0 {
            warnings.push(format!(
                "{} slots also written by another node sending there ({}); the later one wins",
                st.overlap,
                st.writers.join(", ")
            ));
        }
        if matches!(out.target(), Target::OpenDmx { .. }) && rate > OPEN_DMX_MAX_RATE {
            warnings
                .push(format!("the Open DMX USB manages about {OPEN_DMX_MAX_RATE} Hz with a full universe"));
        }
        if !warnings.is_empty() {
            cx.warn(warnings.join("; "));
        }
        if let Some(e) = st.error {
            return Err(CookError::failed(e));
        }
        Ok(Cooked::Outputs(Vec::new()))
    }
}

/// Most channels one `chan.dmx_in` makes: as many universes as one input may ask for.
// `as`: a small count, exact as a float.
const MAX_IN_CHANNELS: f64 = (SLOTS * input::MAX_UNIVERSES) as f64;

#[derive(Default)]
struct DmxIn {
    input: Option<Input>,
    /// What the last cook that made channels read.
    last: Option<Reading>,
}

/// `interface` as an address: empty is every interface, else an IPv4 address or `localhost`.
fn interface_of(text: &str) -> Result<Ipv4Addr, CookError> {
    let t = text.trim();
    if t.is_empty() {
        return Ok(Ipv4Addr::UNSPECIFIED);
    }
    if t.eq_ignore_ascii_case("localhost") {
        return Ok(Ipv4Addr::LOCALHOST);
    }
    t.parse::<Ipv4Addr>()
        .map_err(|_| CookError::failed(format!("`interface`: `{t}` is not an IPv4 address of this machine")))
}

/// Names a list of universes: `3`, `3 and 4`, `3, 4 and 5`.
fn universe_list(us: &[u16]) -> String {
    let words: Vec<String> = us.iter().map(u16::to_string).collect();
    match words.split_last() {
        Some((last, rest)) if !rest.is_empty() => format!("{} and {last}", rest.join(", ")),
        _ => words.concat(),
    }
}

impl Operator for DmxIn {
    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        let hub = cx
            .resource::<Inputs>()
            .ok_or_else(|| CookError::failed("DMX input is not available in this process"))?;
        let protocol = match cx.text("protocol")? {
            "art_net" => Protocol::ArtNet,
            "sacn" => Protocol::Sacn,
            other => return Err(CookError::failed(format!("unknown protocol `{other}`"))),
        };
        let port = match cx.int("port")? {
            0 => protocol.port(),
            p => u16::try_from(p).map_err(|_| CookError::failed("`port` is 0 to 65535"))?,
        };
        let listen = Listen { protocol, interface: interface_of(cx.text("interface")?)?, port };
        let (lowest, highest) = protocol.universes();
        let universe = u16::try_from(cx.int("universe")?)
            .ok()
            .filter(|u| (lowest..=highest).contains(u))
            .ok_or_else(|| CookError::failed(format!("`universe` is {lowest} to {highest} here")))?;
        let address = usize::try_from(cx.int("address")?.clamp(1, 512) - 1).unwrap_or(0);
        let count = usize::try_from(cx.int("count")?).unwrap_or(1).clamp(1, SLOTS * input::MAX_UNIVERSES);
        let first = usize::from(universe) * SLOTS + address;
        let last_universe = u16::try_from((first + count - 1) / SLOTS)
            .ok()
            .filter(|u| *u <= highest)
            .ok_or_else(|| CookError::failed(format!("the channels reach past universe {highest}")))?;
        let wanted: BTreeSet<u16> = (universe..=last_universe).collect();

        if self.input.as_ref().is_none_or(|i| i.listen() != listen) {
            // The old one first, so a port it holds is free for the new one.
            self.input = None;
            self.last = None;
            let waker = cx.waker();
            let i = hub
                .input(cx.def().name.as_str(), listen, Arc::new(move || waker.wake()))
                .map_err(CookError::failed)?;
            self.input = Some(i);
        }
        let Some(input) = &self.input else { return Ok(Cooked::Outputs(Vec::new())) };
        input.want(&wanted);
        let reading = input.read();
        if let Some(e) = &reading.error {
            return Err(CookError::failed(e.clone()));
        }
        let mut warnings = Vec::new();
        let never: Vec<u16> =
            reading.universes.iter().filter(|u| u.slots.is_none()).map(|u| u.universe).collect();
        let held: Vec<u16> =
            reading.universes.iter().filter(|u| u.slots.is_some() && !u.live).map(|u| u.universe).collect();
        if !never.is_empty() {
            warnings.push(format!("nothing received on universe {} yet", universe_list(&never)));
        }
        if !held.is_empty() {
            warnings.push(format!("universe {} fell silent; holding the last values", universe_list(&held)));
        }
        if let Some(m) = &reading.multicast {
            warnings.push(m.clone());
        }
        if !warnings.is_empty() {
            cx.warn(warnings.join("; "));
        }
        if !cx.params_changed() && !cx.first_cook() && self.last.as_ref() == Some(&reading) {
            return Ok(Cooked::Unchanged);
        }
        let unit = cx.text("values")? == "unit";
        let mut channels = Vec::with_capacity(count);
        for i in 0..count {
            let slot = first + i;
            let v = reading
                .universes
                .iter()
                .find(|u| usize::from(u.universe) == slot / SLOTS)
                .and_then(|u| u.slots.as_ref())
                .and_then(|s| s.get(slot % SLOTS))
                .copied()
                .unwrap_or(0);
            let value = if unit { f64::from(v) / 255.0 } else { f64::from(v) };
            channels.push(Channel { name: Name::new(&format!("ch{}", i + 1))?, samples: vec![value] });
        }
        self.last = Some(reading);
        let chans = Chans::new(cx.rate().as_f64(), channels)?;
        Ok(Cooked::Outputs(vec![Data::new(DataKind::Chan, chans)]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn universe_lists() {
        assert_eq!(universe_list(&[3]), "3");
        assert_eq!(universe_list(&[3, 4]), "3 and 4");
        assert_eq!(universe_list(&[3, 4, 5]), "3, 4 and 5");
    }

    #[test]
    fn hosts() {
        assert_eq!(address("10.0.0.5", 6454).unwrap(), "10.0.0.5:6454".parse().unwrap());
        assert_eq!(address(" 10.0.0.5:7000 ", 6454).unwrap(), "10.0.0.5:7000".parse().unwrap());
        assert_eq!(address("localhost", 4048).unwrap(), "127.0.0.1:4048".parse().unwrap());
        assert_eq!(address("LOCALHOST:9", 4048).unwrap(), "127.0.0.1:9".parse().unwrap());
        assert_eq!(address("[::1]:5", 1).unwrap(), "[::1]:5".parse().unwrap());
        assert!(address("wled.local", 4048).is_err());
        assert!(address("localhost:x", 4048).is_err());
    }

    #[test]
    fn slots() {
        assert_eq!(slot(0.0, true), 0);
        assert_eq!(slot(1.0, true), 255);
        assert_eq!(slot(0.5, true), 128);
        assert_eq!(slot(2.0, true), 255);
        assert_eq!(slot(-1.0, true), 0);
        assert_eq!(slot(f64::NAN, true), 0);
        assert_eq!(slot(f64::INFINITY, false), 255);
        assert_eq!(slot(127.4, false), 127);
    }
}
