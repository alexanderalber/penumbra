//! Serial, UDP and TCP nodes end to end over loopback: sockets of ours on 127.0.0.1, listeners
//! on port 0 (any free port), no outside network and no serial port.

use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::time::Duration;

use serde_json::{Value as Json, json};

use super::{Fixture, eventually};

/// The port the link of the node at `path` (at the top) bound. Links name their nodes by name.
fn bound_port(f: &Fixture, path: &str) -> u16 {
    let mut port = 0;
    eventually("the link to bind", || {
        // A read cooks the node, which opens its link.
        f.registry.dispatch("chan.read", &json!({"path": path}));
        let status = f.call("net.status", json!({}));
        port = status["links"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|l| {
                l["readers"].as_array().is_some_and(|r| r.iter().any(|x| x == path.trim_start_matches('/')))
            })
            .and_then(|l| l["local"].as_str())
            .and_then(|a| a.parse::<SocketAddr>().ok())
            .map_or(0, |a| a.port());
        port != 0
    });
    port
}

fn channel(f: &Fixture, path: &str, name: &str) -> Option<f64> {
    let read = f.registry.dispatch("chan.read", &json!({"path": path}));
    read["result"]["channels"].as_array()?.iter().find(|c| c["name"] == name)?["value"].as_f64()
}

fn warning(f: &Fixture, path: &str) -> String {
    let info = f.call("node.info", json!({"path": path}));
    let w = &info["status"]["warning"];
    let e = &info["status"]["error"];
    format!("{} {}", w.as_str().unwrap_or(""), e.as_str().unwrap_or(""))
}

#[test]
fn udp_lines_become_channels_and_a_table_and_bind() {
    let f = Fixture::new("net_udp_in");
    f.call(
        "node.create",
        json!({"path": "/in", "kind": "chan.data_in", "params": {"port": 0, "names": "x y", "rows": 3}}),
    );
    let port = bound_port(&f, "/in");
    let tx = UdpSocket::bind("127.0.0.1:0").unwrap();
    tx.send_to(b"0.25,2,hello\n", ("127.0.0.1", port)).unwrap();
    tx.send_to(b"temp: 21.5\r\n", ("127.0.0.1", port)).unwrap();
    eventually("both lines", || channel(&f, "/in", "temp") == Some(21.5));
    let read = f.call("chan.read", json!({"path": "/in"}));
    assert_eq!(
        read["channels"],
        json!([{"name": "x", "value": 0.25}, {"name": "y", "value": 2.0}, {"name": "temp", "value": 21.5}]),
        "{read}"
    );
    let table = f.call("table.read", json!({"path": "/in", "output": "table"}));
    assert_eq!(table["rows"][0], json!(["x", "y", "chan2", "temp"]), "{table}");
    assert_eq!(table["rows"][1], json!(["0.25", "2", "hello", ""]), "{table}");

    // A binding follows the channel.
    f.call("node.create", json!({"path": "/m", "kind": "chan.math", "params": {"op": "multiply", "b": 1}}));
    f.call("node.bind", json!({"param": "/m.b", "from": "/in", "channel": "x"}));
    f.call("node.create", json!({"path": "/k", "kind": "chan.constant", "params": {"value0": 2}}));
    f.call("node.connect", json!({"from": "/k", "to": "/m"}));
    f.call("node.watch", json!({"path": "/m"}));
    tx.send_to(b"x=3", ("127.0.0.1", port)).unwrap();
    eventually("the bound value", || {
        f.call("chan.read", json!({"path": "/m"}))["channels"][0]["value"] == 6.0
    });

    // JSON, with a malformed message counted on the node.
    f.call("node.set", json!({"path": "/in", "params": {"format": "json"}}));
    // A new format starts the node over: what came before it is gone.
    eventually("the node to start over", || channel(&f, "/in", "x").is_none());
    tx.send_to(br#"{"acc": {"x": 1, "y": 2}, "on": true}"#, ("127.0.0.1", port)).unwrap();
    tx.send_to(b"{broken", ("127.0.0.1", port)).unwrap();
    eventually("the JSON", || channel(&f, "/in", "acc_y") == Some(2.0));
    eventually("the warning", || warning(&f, "/in").contains("1 message dropped"));
    let status = f.call("net.status", json!({}));
    assert_eq!(status["links"][0]["readers"], json!(["in"]), "{status}");
}

#[test]
fn data_out_sends_lines_json_and_bytes_over_udp() {
    let f = Fixture::new("net_udp_out");
    let rx = UdpSocket::bind("127.0.0.1:0").unwrap();
    rx.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let port = rx.local_addr().unwrap().port();
    f.call(
        "node.create",
        json!({"path": "/k", "kind": "chan.constant", "params": {"rows": 2, "value0": 0.5, "value1": 200}}),
    );
    f.call(
        "node.create",
        json!({"path": "/out", "kind": "chan.data_out", "params": {"port": port, "names": true}}),
    );
    f.call("node.connect", json!({"from": "/k", "to": "/out"}));
    let mut buf = [0u8; 256];
    let n = rx.recv(&mut buf).unwrap();
    assert_eq!(&buf[..n], b"chan0:0.5,chan1:200");
    f.call("node.set", json!({"path": "/out", "params": {"format": "json"}}));
    let mut got = Vec::new();
    eventually("JSON", || {
        let n = rx.recv(&mut buf).unwrap();
        got = buf[..n].to_vec();
        got.starts_with(b"{")
    });
    assert_eq!(got, br#"{"chan0":0.5,"chan1":200}"#);
    f.call("node.set", json!({"path": "/out", "params": {"format": "bytes"}}));
    eventually("bytes", || {
        let n = rx.recv(&mut buf).unwrap();
        got = buf[..n].to_vec();
        got.len() == 2
    });
    // Bytes are the values as they are, rounded.
    assert_eq!(got, [1, 200]);
    // A table's rows go out as messages of their own.
    f.call("node.create", json!({"path": "/t", "kind": "table.text", "params": {"text": "go,1\nstop,0"}}));
    f.call("node.connect", json!({"from": "/t", "to": "/out", "input": "table"}));
    f.call("node.set", json!({"path": "/out", "params": {"format": "lines"}}));
    eventually("the rows", || {
        let n = rx.recv(&mut buf).unwrap();
        &buf[..n] == b"stop,0"
    });
}

#[test]
fn tcp_in_and_out_share_a_connection_and_survive_the_server_going_away() {
    let f = Fixture::new("net_tcp");
    let server = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = server.local_addr().unwrap().port();
    let params = json!({"transport": "tcp_connect", "host": "127.0.0.1", "port": port});
    f.call("node.create", json!({"path": "/in", "kind": "chan.data_in", "params": params}));
    f.call("node.create", json!({"path": "/out", "kind": "chan.data_out", "params": params}));
    f.call("node.create", json!({"path": "/k", "kind": "chan.constant", "params": {"value0": 7}}));
    f.call("node.connect", json!({"from": "/k", "to": "/out"}));
    f.call("node.watch", json!({"path": "/in"}));
    let (conn, _) = server.accept().unwrap();
    conn.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let mut lines = BufReader::new(conn.try_clone().unwrap()).lines();
    assert_eq!(lines.next().unwrap().unwrap(), "7");
    (&conn).write_all(b"reply=1\n").unwrap();
    eventually("the reply", || channel(&f, "/in", "reply") == Some(1.0));
    let status = f.call("net.status", json!({}));
    let links = status["links"].as_array().unwrap();
    assert_eq!(links.len(), 1, "{status}");
    assert_eq!(links[0]["readers"], json!(["in"]));
    assert_eq!(links[0]["writers"], json!(["out"]));

    // The server goes away: the nodes keep cooking, the input holds its value and says so.
    drop(lines);
    drop(conn);
    drop(server);
    eventually("the drop noticed", || warning(&f, "/in").contains("holding the last values"));
    assert_eq!(channel(&f, "/in", "reply"), Some(1.0));
    // The server comes back on the same port: the link reconnects and the latest goes out.
    let server = TcpListener::bind(("127.0.0.1", port)).unwrap();
    let (conn, _) = server.accept().unwrap();
    conn.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let mut lines = BufReader::new(conn.try_clone().unwrap()).lines();
    assert_eq!(lines.next().unwrap().unwrap(), "7");
    (&conn).write_all(b"reply=2\n").unwrap();
    eventually("the second reply", || channel(&f, "/in", "reply") == Some(2.0));

    // Removing both nodes closes the link.
    f.call("node.delete", json!({"path": "/in"}));
    f.call("node.delete", json!({"path": "/out"}));
    eventually("the link closed", || f.call("net.status", json!({}))["links"] == json!([]));
}

#[test]
fn a_tcp_listener_takes_json_from_a_client() {
    let f = Fixture::new("net_tcp_listen");
    f.call(
        "node.create",
        json!({"path": "/in", "kind": "chan.data_in", "params": {"transport": "tcp_listen", "port": 0, "format": "json"}}),
    );
    let port = bound_port(&f, "/in");
    let mut c = TcpStream::connect(("127.0.0.1", port)).unwrap();
    c.write_all(b"{\"a\": 1}\n[5, 6]\n").unwrap();
    eventually("both", || channel(&f, "/in", "chan1") == Some(6.0));
    assert_eq!(channel(&f, "/in", "a"), Some(1.0));
}

#[test]
fn a_serial_port_that_is_not_there_is_an_error_not_a_crash() {
    let f = Fixture::new("net_serial");
    f.call(
        "node.create",
        json!({"path": "/in", "kind": "chan.data_in", "params": {"transport": "serial", "serial": "COM_PENUMBRA_NONE"}}),
    );
    f.call(
        "node.create",
        json!({"path": "/out", "kind": "chan.data_out", "params": {"transport": "serial", "serial": "COM_PENUMBRA_NONE", "baud": 115200}}),
    );
    f.call("node.create", json!({"path": "/k", "kind": "chan.constant"}));
    f.call("node.connect", json!({"from": "/k", "to": "/out"}));
    eventually("the error", || {
        f.registry.dispatch("chan.read", &json!({"path": "/in"}));
        warning(&f, "/in").contains("cannot open COM_PENUMBRA_NONE")
    });
    let status = f.call("net.status", json!({}));
    assert_eq!(status["links"].as_array().map(Vec::len), Some(1), "in and out share the port: {status}");
    let ports: Json = f.call("net.ports", json!({}));
    assert!(ports["ports"].is_array());
    // Params the transport does not use are greyed.
    let info = f.call("node.info", json!({"path": "/in"}));
    let host = info["params"].as_array().unwrap().iter().find(|p| p["name"] == "host").unwrap().clone();
    assert_eq!(host["applies"], false, "{host}");
}
