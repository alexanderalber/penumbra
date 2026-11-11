//! Links on loopback: UDP and TCP on 127.0.0.1, listeners on port 0 (any free port), no outside
//! network and no serial hardware.

#![allow(clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use penumbra_net::{Decode, Endpoint, Format, Hub, Reader};

fn eventually(what: &str, mut f: impl FnMut() -> bool) {
    let until = Instant::now() + Duration::from_secs(10);
    while Instant::now() < until {
        if f() {
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("timed out waiting for {what}");
}

fn wakes() -> (Arc<AtomicU64>, Arc<dyn Fn() + Send + Sync>) {
    let n = Arc::new(AtomicU64::new(0));
    let m = n.clone();
    (
        n,
        Arc::new(move || {
            m.fetch_add(1, Ordering::Relaxed);
        }),
    )
}

fn local_port(r: &Reader) -> u16 {
    let mut port = 0;
    eventually("the link to bind", || {
        port = r.status().local.and_then(|l| l.parse::<SocketAddr>().ok()).map_or(0, |a| a.port());
        port != 0
    });
    port
}

/// Binds `to` again after it was let go, trying for a while: the system may hold it briefly.
fn rebind(to: SocketAddr) -> TcpListener {
    let mut listener = None;
    eventually("the port to be free again", || {
        listener = TcpListener::bind(to).ok();
        listener.is_some()
    });
    listener.unwrap()
}

fn value(r: &Reader, name: &str) -> Option<f64> {
    r.store().values.iter().find(|(n, _)| n == name).map(|(_, v)| *v)
}

#[test]
fn udp_datagrams_become_values_and_wake_the_reader() {
    let hub = Hub::new();
    let (woken, wake) = wakes();
    let r = hub.reader("/in", Endpoint::Udp { lan: false, port: 0 }, Decode::default(), wake).unwrap();
    let port = local_port(&r);
    let tx = UdpSocket::bind("127.0.0.1:0").unwrap();
    tx.send_to(b"x=0.25 y=2\n", ("127.0.0.1", port)).unwrap();
    tx.send_to(b"1,2\r\n3,4", ("127.0.0.1", port)).unwrap();
    eventually("both datagrams", || value(&r, "chan1") == Some(4.0));
    assert_eq!(value(&r, "x"), Some(0.25));
    assert!(woken.load(Ordering::Relaxed) >= 1);
    let st = r.status();
    assert!(st.open && st.readers == ["/in"] && st.bytes_in > 0, "{st:?}");
    assert_eq!(st.peers, [tx.local_addr().unwrap().to_string()]);

    // JSON, malformed messages counted and dropped, an oversized datagram counted.
    let json = Decode { format: Format::Json, max: 64, ..Decode::default() };
    r.configure(&json);
    tx.send_to(br#"{"a": 1, "b": [2, 3]}"#, ("127.0.0.1", port)).unwrap();
    tx.send_to(b"{nope", ("127.0.0.1", port)).unwrap();
    tx.send_to(&[b' '; 100], ("127.0.0.1", port)).unwrap();
    eventually("the JSON and the bad ones", || {
        let s = r.store();
        s.counts.malformed == 1 && s.counts.too_long == 1 && s.counts.messages == 1
    });
    let s = r.store();
    assert_eq!(s.values, [("a".to_string(), 1.0), ("b_0".to_string(), 2.0), ("b_1".to_string(), 3.0)]);

    // Two readers on one port share it, each with its own format.
    let (_, wake2) = wakes();
    let bytes = Decode { format: Format::Bytes, ..Decode::default() };
    let r2 = hub.reader("/raw", Endpoint::Udp { lan: false, port }, bytes, wake2).unwrap();
    assert_eq!(hub.status().len(), 1);
    tx.send_to(&[7, 8], ("127.0.0.1", port)).unwrap();
    eventually("the bytes", || value(&r2, "chan1") == Some(8.0));
    // The JSON reader on the same port got them too, and they are no JSON.
    eventually("the JSON reader to count them", || r.store().counts.malformed == 2);
    drop(r2);
    drop(r);
    eventually("the link to stop", || hub.status().is_empty());
    hub.shutdown();
}

#[test]
fn a_flood_of_garbage_over_udp_stays_bounded() {
    let hub = Hub::new();
    let (_, wake) = wakes();
    let d = Decode { format: Format::Json, rows: 4, ..Decode::default() };
    let r = hub.reader("/in", Endpoint::Udp { lan: false, port: 0 }, d, wake).unwrap();
    let port = local_port(&r);
    let tx = UdpSocket::bind("127.0.0.1:0").unwrap();
    let mut seed = 0x1234_5678_9abc_def0u64;
    for i in 0..5000u32 {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        let n = usize::try_from(seed % 300).unwrap();
        let mut packet: Vec<u8> = (0..n).map(|k| (seed >> (k % 56)) as u8).collect();
        if i % 10 == 0 {
            packet = format!("{{\"k{}\": {i}}}", i % 2000).into_bytes();
        }
        tx.send_to(&packet, ("127.0.0.1", port)).ok();
    }
    // The burst may overflow the socket's buffer: send the last one until it arrives.
    eventually("the last one", || {
        tx.send_to(b"{\"last\": 1}", ("127.0.0.1", port)).unwrap();
        std::thread::sleep(Duration::from_millis(20));
        value(&r, "last") == Some(1.0)
    });
    let s = r.store();
    assert!(s.rows.len() <= 4 && s.values.len() <= penumbra_net::parse::MAX_CHANNELS);
    assert!(s.counts.malformed > 0);
    drop(r);
    hub.shutdown();
}

#[test]
fn dropping_a_link_and_opening_it_again_does_not_wait_for_its_thread() {
    let hub = Hub::new();
    let server = TcpListener::bind("127.0.0.1:0").unwrap();
    let to = server.local_addr().unwrap();
    let (_, wake) = wakes();
    let r = hub.reader("/in", Endpoint::TcpConnect(to), Decode::default(), wake.clone()).unwrap();
    let (conn, _) = server.accept().unwrap();
    eventually("connected", || r.status().open);
    // A UDP link waits up to 100 ms in a receive: dropping it and opening the same port again
    // must not wait for that, and the new link binds once the old one let go.
    let port = UdpSocket::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let udp = Endpoint::Udp { lan: false, port };
    let mut u = hub.reader("/u", udp.clone(), Decode::default(), wake.clone()).unwrap();
    for _ in 0..10 {
        eventually("bound", || u.status().open);
        let start = Instant::now();
        drop(u);
        u = hub.reader("/u", udp.clone(), Decode::default(), wake.clone()).unwrap();
        assert!(start.elapsed() < Duration::from_millis(50), "{:?}", start.elapsed());
    }
    eventually("bound at last", || u.status().open);
    drop(u);
    let start = Instant::now();
    drop(r);
    let again = hub.reader("/in", Endpoint::TcpConnect(to), Decode::default(), wake).unwrap();
    assert!(start.elapsed() < Duration::from_millis(50), "{:?}", start.elapsed());
    drop(conn);
    let (mut conn, _) = server.accept().unwrap();
    conn.write_all(b"v=2\n").unwrap();
    eventually("the new link to read", || value(&again, "v") == Some(2.0));
    drop(again);
    hub.shutdown();
}

#[test]
fn a_tcp_listener_reads_every_client_and_writes_to_all() {
    let hub = Hub::new();
    let (_, wake) = wakes();
    let r = hub.reader("/in", Endpoint::TcpListen { lan: false, port: 0 }, Decode::default(), wake).unwrap();
    let port = local_port(&r);
    let w = hub.writer("/out", Endpoint::TcpListen { lan: false, port }).unwrap();
    assert_eq!(hub.status().len(), 1, "the reader and the writer share the listener");

    // Nothing connected: sending drops and says so.
    let sent = w.send(&[b"early".to_vec()], b"\n");
    assert_eq!((sent.sent, sent.dropped), (0, 1));

    let mut a = TcpStream::connect(("127.0.0.1", port)).unwrap();
    let mut b = TcpStream::connect(("127.0.0.1", port)).unwrap();
    eventually("two clients", || r.status().peers.len() == 2);
    // A line split across writes and clients apart: each client's half line is its own.
    a.write_all(b"x=1\ny=").unwrap();
    b.write_all(b"z=3\r\n").unwrap();
    a.write_all(b"2\n").unwrap();
    eventually("x, y and z", || value(&r, "y") == Some(2.0) && value(&r, "z") == Some(3.0));
    assert_eq!(value(&r, "x"), Some(1.0));

    let sent = w.send(&[b"0.5,1".to_vec(), b"hello".to_vec()], b"\r\n");
    assert_eq!(sent.sent, 2);
    for s in [&a, &b] {
        s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let mut lines = BufReader::new(s.try_clone().unwrap()).lines();
        // What was sent before it connected comes first: it was the latest message then.
        assert_eq!(lines.next().unwrap().unwrap(), "early");
        // `lines` takes off the `\r\n`.
        assert_eq!(lines.next().unwrap().unwrap(), "0.5,1");
        assert_eq!(lines.next().unwrap().unwrap(), "hello");
    }
    // A client that connects later gets the latest at once.
    let c = TcpStream::connect(("127.0.0.1", port)).unwrap();
    c.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let mut lines = BufReader::new(c).lines();
    assert_eq!(lines.next().unwrap().unwrap(), "0.5,1");
    assert_eq!(lines.next().unwrap().unwrap(), "hello");

    // A client that goes away is let go.
    drop(a);
    eventually("one client fewer", || r.status().peers.len() == 2);
    drop((r, w));
    hub.shutdown();
}

#[test]
fn a_tcp_listener_refuses_clients_beyond_its_cap_and_survives_garbage() {
    let hub = Hub::new();
    let (_, wake) = wakes();
    let d = Decode { max: 32, ..Decode::default() };
    let r = hub.reader("/in", Endpoint::TcpListen { lan: false, port: 0 }, d, wake).unwrap();
    let port = local_port(&r);
    let mut clients: Vec<TcpStream> = (0..penumbra_net::hub::MAX_CLIENTS)
        .map(|_| TcpStream::connect(("127.0.0.1", port)).unwrap())
        .collect();
    eventually("every client", || r.status().peers.len() == penumbra_net::hub::MAX_CLIENTS);
    let mut extra = TcpStream::connect(("127.0.0.1", port)).unwrap();
    extra.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let mut buf = [0u8; 1];
    // Closed at once: the read ends (0 bytes or a reset).
    assert!(matches!(extra.read(&mut buf), Ok(0) | Err(_)));
    eventually("the refusal counted", || r.status().refused == 1);
    // Megabytes without a delimiter, then a good line.
    let junk = vec![b'z'; 1 << 20];
    clients[0].write_all(&junk).unwrap();
    clients[0].write_all(b"\nok=1\n").unwrap();
    eventually("the good line after the junk", || value(&r, "ok") == Some(1.0));
    assert_eq!(r.store().counts.too_long, 1);
    clients.clear();
    eventually("the clients gone", || r.status().peers.is_empty());
    drop(r);
    hub.shutdown();
}

#[test]
fn a_tcp_connection_shares_reader_and_writer_and_reconnects() {
    let server = TcpListener::bind("127.0.0.1:0").unwrap();
    let to = server.local_addr().unwrap();
    let hub = Hub::new();
    let (_, wake) = wakes();
    let r = hub.reader("/in", Endpoint::TcpConnect(to), Decode::default(), wake).unwrap();
    let w = hub.writer("/out", Endpoint::TcpConnect(to)).unwrap();
    let (mut conn, _) = server.accept().unwrap();
    conn.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    eventually("connected", || r.status().open);
    // Request and answer on the one connection.
    w.send(&[b"get".to_vec()], b"\n");
    let mut line = String::new();
    BufReader::new(conn.try_clone().unwrap()).read_line(&mut line).unwrap();
    assert_eq!(line, "get\n");
    conn.write_all(b"answer=42\n").unwrap();
    eventually("the answer", || value(&r, "answer") == Some(42.0));

    // The server drops the connection: the link notices and connects again.
    drop(conn);
    eventually("the drop noticed", || !r.status().open);
    let (mut conn, _) = server.accept().unwrap();
    eventually("connected again", || r.status().open);
    assert!(r.status().connects >= 2);
    // The new connection gets the latest message first.
    conn.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let mut line = String::new();
    BufReader::new(conn.try_clone().unwrap()).read_line(&mut line).unwrap();
    assert_eq!(line, "get\n");
    conn.write_all(b"answer=43\n").unwrap();
    eventually("the second answer", || value(&r, "answer") == Some(43.0));
    drop(r);
    drop(w);
    hub.shutdown();
}

#[test]
fn a_tcp_peer_that_is_not_there_is_an_error_and_retried() {
    // A port nobody listens on: bind one and let it go.
    let to = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap();
    let hub = Hub::new();
    let (woken, wake) = wakes();
    let r = hub.reader("/in", Endpoint::TcpConnect(to), Decode::default(), wake).unwrap();
    eventually("the error", || r.status().error.is_some_and(|e| e.contains("cannot connect")));
    // The status is published before the readers are called: the wake follows shortly.
    eventually("the wake for the error", || woken.load(Ordering::Relaxed) >= 1);
    // It comes up later: the link finds it.
    let server = rebind(to);
    let (mut conn, _) = server.accept().unwrap();
    conn.write_all(b"v=1\n").unwrap();
    eventually("the value", || value(&r, "v") == Some(1.0));
    assert!(r.status().error.is_none());
    drop(r);
    hub.shutdown();
}

#[test]
fn a_link_that_fails_at_once_still_wakes_its_first_reader() {
    // A refused connect fails within microseconds; the reader must already be registered when
    // the link's thread reports it, or the one wake for the error is lost. Fresh links, many
    // times, to give the thread every chance to win.
    for _ in 0..200 {
        let to = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap();
        let hub = Hub::new();
        let (woken, wake) = wakes();
        let r = hub.reader("/in", Endpoint::TcpConnect(to), Decode::default(), wake).unwrap();
        eventually("the error", || r.status().error.is_some());
        eventually("the wake for the error", || woken.load(Ordering::Relaxed) >= 1);
        drop(r);
        hub.shutdown();
    }
}

#[test]
fn udp_out_sends_datagrams_without_a_delimiter() {
    let rx = UdpSocket::bind("127.0.0.1:0").unwrap();
    rx.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let hub = Hub::new();
    let w = hub.writer("/out", Endpoint::UdpTo(rx.local_addr().unwrap())).unwrap();
    let sent = w.send(&[b"{\"a\":1}".to_vec(), b"2".to_vec()], b"\n");
    assert_eq!(sent.sent, 2);
    let mut buf = [0u8; 64];
    let n = rx.recv(&mut buf).unwrap();
    assert_eq!(&buf[..n], b"{\"a\":1}");
    let n = rx.recv(&mut buf).unwrap();
    assert_eq!(&buf[..n], b"2");
    let sent = w.send(&[vec![0u8; 70_000]], b"");
    assert_eq!(sent.dropped, 1);
    let st = w.status();
    assert_eq!((st.messages_out, st.dropped_out), (2, 1));
    assert!(
        hub.reader("/x", Endpoint::UdpTo(rx.local_addr().unwrap()), Decode::default(), wakes().1).is_err()
    );
    drop(w);
    hub.shutdown();
}

#[test]
fn a_taken_port_is_an_error_until_it_is_free() {
    let holder = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = holder.local_addr().unwrap().port();
    let hub = Hub::new();
    let r =
        hub.reader("/in", Endpoint::TcpListen { lan: false, port }, Decode::default(), wakes().1).unwrap();
    eventually("the error", || r.status().error.is_some_and(|e| e.contains("cannot listen")));
    drop(holder);
    eventually("listening", || r.status().open);
    drop(r);
    hub.shutdown();
}
