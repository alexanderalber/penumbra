//! DMX end to end over loopback UDP: Art-Net and DDP to sockets of ours, no outside network
//! and no serial port.

use std::net::UdpSocket;
use std::time::{Duration, Instant};

use serde_json::json;

use super::{Fixture, eventually};

fn receiver() -> (UdpSocket, u16) {
    let rx = UdpSocket::bind("127.0.0.1:0").unwrap();
    rx.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let port = rx.local_addr().unwrap().port();
    (rx, port)
}

/// The next packet for which `want` holds, within two seconds.
fn packet_where(rx: &UdpSocket, want: impl Fn(&[u8]) -> bool) -> Vec<u8> {
    let mut buf = [0u8; 2048];
    let start = Instant::now();
    loop {
        let n = rx.recv(&mut buf).unwrap();
        if want(&buf[..n]) {
            return buf[..n].to_vec();
        }
        assert!(start.elapsed() < Duration::from_secs(2), "no such packet");
    }
}

#[test]
fn channels_go_out_as_art_net_and_keep_going() {
    let f = Fixture::new("dmx_artnet");
    let (rx, port) = receiver();
    f.call(
        "node.create",
        json!({"path": "/k", "kind": "chan.constant", "params": {"rows": 3, "value0": 1, "value1": 0.5, "value2": 0.1}}),
    );
    f.call(
        "node.create",
        json!({"path": "/dmx", "kind": "out.dmx", "params": {
            "protocol": "art_net", "host": format!("127.0.0.1:{port}"), "universe": 2, "address": 511
        }}),
    );
    f.call("node.connect", json!({"from": "/k", "to": "/dmx"}));
    // Slots 511 and 512 of universe 2, then slot 1 of universe 3.
    let p = packet_where(&rx, |p| p[14..16] == [2, 0] && p[18 + 510] == 255);
    assert_eq!(p[18 + 511], 128);
    let p = packet_where(&rx, |p| p[14..16] == [3, 0]);
    assert_eq!(p[18], 26);

    let status = f.call("dmx.status", json!({}));
    let out = &status["outputs"][0];
    assert_eq!(out["target"], json!({"protocol": "art_net", "to": format!("127.0.0.1:{port}")}), "{status}");
    assert_eq!(out["universes"], json!([2, 3]));
    assert_eq!(out["writers"], json!(["dmx"]));

    // Nothing changes, the frames keep coming at the rate.
    let start = Instant::now();
    let mut n = 0;
    while start.elapsed() < Duration::from_millis(500) {
        packet_where(&rx, |p| p[14..16] == [2, 0]);
        n += 1;
    }
    assert!((12..=18).contains(&n), "{n} frames in 0.5 s at 30 Hz");

    // Blackout from the command, then back.
    f.call("dmx.config", json!({"blackout": true}));
    packet_where(&rx, |p| p[14..16] == [2, 0] && p[18 + 510] == 0);
    f.call("dmx.config", json!({"blackout": false}));
    packet_where(&rx, |p| p[14..16] == [2, 0] && p[18 + 510] == 255);

    // A change of value goes out; deleting the node stops the output.
    f.call("node.set", json!({"path": "/k", "params": {"value0": 0.2}}));
    packet_where(&rx, |p| p[14..16] == [2, 0] && p[18 + 510] == 51);
    f.call("node.delete", json!({"path": "/dmx"}));
    eventually("the output to stop", || f.call("dmx.status", json!({}))["outputs"] == json!([]));
}

#[test]
fn ddp_needs_a_host_and_bad_settings_are_node_errors() {
    let f = Fixture::new("dmx_ddp");
    f.call(
        "node.create",
        json!({"path": "/k", "kind": "chan.constant", "params": {"rows": 2, "value0": 7, "value1": 300}}),
    );
    f.call(
        "node.create",
        json!({"path": "/dmx", "kind": "out.dmx", "params": {"protocol": "ddp", "values": "byte"}}),
    );
    f.call("node.connect", json!({"from": "/k", "to": "/dmx"}));
    let error = |f: &Fixture| {
        f.call("node.info", json!({"path": "/dmx"}))["status"]["error"].as_str().unwrap_or("").to_string()
    };
    eventually("an error about the host", || error(&f).contains("DDP needs a `host`"));
    f.call("node.set", json!({"path": "/dmx", "params": {"host": "wled.local"}}));
    eventually("an error about names", || error(&f).contains("names are not looked up"));
    let (rx, port) = receiver();
    f.call("node.set", json!({"path": "/dmx", "params": {"host": format!("localhost:{port}")}}));
    let p = packet_where(&rx, |p| p.len() == 12);
    assert_eq!(&p[10..], &[7, 255]);
    eventually("no error", || error(&f).is_empty());
    f.call("node.set", json!({"path": "/dmx", "params": {"protocol": "sacn", "universe": 0, "host": ""}}));
    eventually("an error about the universe", || error(&f).contains("start at 1"));
}

#[test]
fn dmx_in_reads_what_out_dmx_sends_and_says_what_is_missing() {
    let f = Fixture::new("dmx_in");
    let port = UdpSocket::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    f.call(
        "node.create",
        json!({"path": "/in", "kind": "chan.dmx_in", "params": {
            "interface": "127.0.0.1", "port": port, "universe": 2, "address": 511, "count": 3
        }}),
    );
    f.call("node.watch", json!({"path": "/in"}));
    let warning = |f: &Fixture| {
        f.call("node.info", json!({"path": "/in"}))["status"]["warning"].as_str().unwrap_or("").to_string()
    };
    eventually("a warning about silence", || {
        warning(&f).contains("nothing received on universe 2 and 3 yet")
    });
    assert_eq!(
        f.call("chan.read", json!({"path": "/in"}))["channels"][0],
        json!({"name": "ch1", "value": 0.0})
    );

    f.call(
        "node.create",
        json!({"path": "/k", "kind": "chan.constant", "params": {"rows": 3, "value0": 1, "value1": 0.2, "value2": 0.6}}),
    );
    f.call(
        "node.create",
        json!({"path": "/dmx", "kind": "out.dmx", "params": {
            "protocol": "art_net", "host": format!("127.0.0.1:{port}"), "universe": 2, "address": 511
        }}),
    );
    f.call("node.connect", json!({"from": "/k", "to": "/dmx"}));
    let values = |f: &Fixture| -> Vec<f64> {
        let read = f.call("chan.read", json!({"path": "/in"}));
        read["channels"].as_array().unwrap().iter().map(|c| c["value"].as_f64().unwrap()).collect()
    };
    eventually("the slots", || values(&f) == vec![1.0, 51.0 / 255.0, 153.0 / 255.0]);
    eventually("no warning", || warning(&f).is_empty());
    f.call("node.set", json!({"path": "/in", "params": {"values": "byte"}}));
    eventually("bytes", || values(&f) == vec![255.0, 51.0, 153.0]);
    f.call("node.set", json!({"path": "/k", "params": {"value1": 0.0}}));
    eventually("the change", || values(&f) == vec![255.0, 0.0, 153.0]);

    let status = f.call("dmx.status", json!({}));
    let input = &status["inputs"][0];
    assert_eq!(input["readers"], json!(["in"]), "{status}");
    assert_eq!(input["listen"]["protocol"], "art_net");
    assert_eq!(input["universes"].as_array().unwrap().len(), 2, "{status}");

    // Bad settings are node errors; the universe range follows the protocol.
    let error = |f: &Fixture| {
        f.call("node.info", json!({"path": "/in"}))["status"]["error"].as_str().unwrap_or("").to_string()
    };
    f.call("node.set", json!({"path": "/in", "params": {"protocol": "sacn", "universe": 0}}));
    eventually("an error about the universe", || error(&f).contains("`universe` is 1 to 63999"));
    f.call("node.set", json!({"path": "/in", "params": {"universe": 1, "interface": "eth0"}}));
    eventually("an error about the interface", || error(&f).contains("not an IPv4 address"));
    f.call("node.delete", json!({"path": "/in"}));
    eventually("the listener to stop", || f.call("dmx.status", json!({}))["inputs"] == json!([]));
}
