use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

use filterway::proto::{self, read_packet, write_packet, Packet};
use tempfile::tempdir;

// ---------------------------------------------------------------------------
// Proto functions over a real UnixStream pair
// ---------------------------------------------------------------------------

#[test]
fn packet_roundtrip_over_unix_stream() {
    let (mut a, mut b) = UnixStream::pair().unwrap();
    a.set_read_timeout(Some(Duration::from_secs(1))).unwrap();

    let packet = Packet {
        id: 42,
        opcode: 3,
        body: vec![1, 2, 3, 4],
    };
    write_packet(&mut a, &packet).unwrap();
    drop(a);

    let result = read_packet(&mut b).unwrap().unwrap();
    assert_eq!(result, packet);
}

#[test]
fn multiple_packets_over_unix_stream() {
    let (mut a, mut b) = UnixStream::pair().unwrap();
    b.set_read_timeout(Some(Duration::from_secs(1))).unwrap();

    let packets: Vec<Packet> = (0..5)
        .map(|i| Packet {
            id: i,
            opcode: i as u16,
            body: vec![i as u8; 4],
        })
        .collect();

    for p in &packets {
        write_packet(&mut a, p).unwrap();
    }
    drop(a);

    for expected in &packets {
        let received = read_packet(&mut b).unwrap().unwrap();
        assert_eq!(&received, expected);
    }
    assert!(read_packet(&mut b).unwrap().is_none());
}

#[test]
fn read_packet_returns_none_on_closed_stream() {
    let (a, mut b) = UnixStream::pair().unwrap();
    drop(a);
    assert!(read_packet(&mut b).unwrap().is_none());
}

#[test]
fn bidirectional_messages_over_unix_stream() {
    let (mut a, mut b) = UnixStream::pair().unwrap();
    a.set_read_timeout(Some(Duration::from_secs(1))).unwrap();
    b.set_read_timeout(Some(Duration::from_secs(1))).unwrap();

    let msg_a = Packet {
        id: 1,
        opcode: 0,
        body: vec![0xAA],
    };
    let msg_b = Packet {
        id: 2,
        opcode: 1,
        body: vec![0xBB],
    };

    write_packet(&mut a, &msg_a).unwrap();
    write_packet(&mut b, &msg_b).unwrap();

    let recv_b = read_packet(&mut b).unwrap().unwrap();
    assert_eq!(recv_b, msg_a);

    let recv_a = read_packet(&mut a).unwrap().unwrap();
    assert_eq!(recv_a, msg_b);
}

#[test]
fn large_packet_over_unix_stream() {
    let (mut a, mut b) = UnixStream::pair().unwrap();
    b.set_read_timeout(Some(Duration::from_secs(5))).unwrap();

    // Max body size: u16 message_size = body + 8 bytes header ≤ 65535.
    let body = vec![0xABu8; 65527];
    let packet = Packet {
        id: u32::MAX,
        opcode: u16::MAX,
        body,
    };
    write_packet(&mut a, &packet).unwrap();
    drop(a);

    let received = read_packet(&mut b).unwrap().unwrap();
    assert_eq!(received, packet);
}

// ---------------------------------------------------------------------------
// Filterway binary passthrough (end-to-end)
// ---------------------------------------------------------------------------

fn filterway_binary() -> PathBuf {
    if let Ok(path) = std::env::var("CARGO_BIN_EXE_filterway") {
        return PathBuf::from(path);
    }
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/debug/filterway");
    if p.exists() {
        return p;
    }
    panic!("filterway binary not found — run `cargo build` first");
}

fn connect_with_retry(path: &std::path::Path, timeout: Duration) -> UnixStream {
    let start = Instant::now();
    loop {
        match UnixStream::connect(path) {
            Ok(stream) => return stream,
            Err(_) if start.elapsed() >= timeout => {
                panic!("timed out connecting to {}", path.display());
            }
            Err(_) => std::thread::sleep(Duration::from_millis(20)),
        }
    }
}

#[test]
fn filterway_basic_passthrough() {
    let dir = tempdir().unwrap();
    let upstream = dir.path().join("upstream.sock");
    let downstream = dir.path().join("downstream.sock");

    // Start mock compositor (listener for filterway's upstream connection).
    let mock_listener = std::os::unix::net::UnixListener::bind(&upstream).unwrap();

    // Launch filterway.
    let mut filterway = Command::new(filterway_binary())
        .args([
            "--upstream",
            upstream.to_str().unwrap(),
            "--downstream",
            downstream.to_str().unwrap(),
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("failed to start filterway");

    // Connect as a client to filterway's downstream socket.
    let mut client = connect_with_retry(&downstream, Duration::from_secs(5));

    // Mock compositor accepts filterway's connection.
    let (mut compositor, _) = mock_listener.accept().unwrap();

    client
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    compositor
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();

    // Send message client → filterway → compositor.
    let sent = Packet {
        id: 1,
        opcode: 0,
        body: vec![0xAB, 0xCD, 0x00, 0x00],
    };
    write_packet(&mut client, &sent).unwrap();
    let received = read_packet(&mut compositor).unwrap().unwrap();
    assert_eq!(received, sent, "client→compositor passthrough failed");

    // Send message compositor → filterway → client.
    // Use opcode=0 (no special handling on Display) with a non-empty body.
    let reply = Packet {
        id: 1,
        opcode: 0,
        body: vec![0xAA],
    };
    write_packet(&mut compositor, &reply).unwrap();
    let received = read_packet(&mut client).unwrap().unwrap();
    assert_eq!(received, reply, "compositor→client passthrough failed");

    // Cleanup.
    filterway.kill().unwrap();
    let output = filterway.wait_with_output().unwrap();
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if !stderr.is_empty() {
            eprintln!("filterway stderr:\n{stderr}");
        }
    }
}

#[test]
fn filterway_object_chain_and_app_id_replacement() {
    let dir = tempdir().unwrap();
    let upstream = dir.path().join("upstream.sock");
    let downstream = dir.path().join("downstream.sock");

    // Mock compositor listener.
    let mock_listener = std::os::unix::net::UnixListener::bind(&upstream).unwrap();

    // Launch filterway with --app-id replacement.
    let mut filterway = Command::new(filterway_binary())
        .args([
            "--upstream",
            upstream.to_str().unwrap(),
            "--downstream",
            downstream.to_str().unwrap(),
            "--app-id",
            "filtered",
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("failed to start filterway");

    // Connect client to filterway's downstream.
    let mut client = connect_with_retry(&downstream, Duration::from_secs(5));

    // Mock compositor accepts filterway's connection.
    let (mut compositor, _) = mock_listener.accept().unwrap();

    client
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    compositor
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();

    // -----------------------------------------------------------------------
    // Build the Wayland object chain so filterway tracks XdgToplevel.
    // -----------------------------------------------------------------------

    // 1. Client sends Display.get_registry (opcode=1) → creates registry at id 2.
    write_packet(
        &mut client,
        &Packet {
            id: 1,
            opcode: 1,
            body: {
                let mut b = vec![];
                proto::write_arg_uint(&mut b, 2).unwrap();
                b
            },
        },
    )
    .unwrap();
    let _registry_req = read_packet(&mut compositor).unwrap().unwrap();

    // 2. Compositor emits Registry.global (opcode=0) for xdg_wm_base.
    //    type_id = 0, interface = "xdg_wm_base", version = 1.
    let xdgwm_base_type_id = 0u32;
    {
        let mut body = vec![];
        proto::write_arg_uint(&mut body, xdgwm_base_type_id).unwrap();
        proto::write_arg_string(&mut body, "xdg_wm_base".to_string()).unwrap();
        proto::write_arg_uint(&mut body, 1).unwrap();
        write_packet(
            &mut compositor,
            &Packet {
                id: 2,
                opcode: 0,
                body,
            },
        )
        .unwrap();
    }
    let _global_event = read_packet(&mut client).unwrap().unwrap();

    // 3. Client sends Registry.bind (opcode=0) → binds xdg_wm_base at id 3.
    {
        let mut body = vec![];
        proto::write_arg_uint(&mut body, xdgwm_base_type_id).unwrap();
        proto::write_arg_string(&mut body, "xdg_wm_base".to_string()).unwrap();
        proto::write_arg_uint(&mut body, 1).unwrap();
        proto::write_arg_uint(&mut body, 3).unwrap();
        write_packet(
            &mut client,
            &Packet {
                id: 2,
                opcode: 0,
                body,
            },
        )
        .unwrap();
    }
    let _bind_req = read_packet(&mut compositor).unwrap().unwrap();

    // 4. Client sends XdgWmBase.get_xdg_surface (opcode=2) → creates surface at id 4.
    //    (version 1–6 all use opcode 2)
    {
        let mut body = vec![];
        proto::write_arg_uint(&mut body, 4).unwrap();
        write_packet(
            &mut client,
            &Packet {
                id: 3,
                opcode: 2,
                body,
            },
        )
        .unwrap();
    }
    let _get_surface_req = read_packet(&mut compositor).unwrap().unwrap();

    // 5. Client sends XdgSurface.create_toplevel (opcode=1) → creates toplevel at id 5.
    {
        let mut body = vec![];
        proto::write_arg_uint(&mut body, 5).unwrap();
        write_packet(
            &mut client,
            &Packet {
                id: 4,
                opcode: 1,
                body,
            },
        )
        .unwrap();
    }
    let _create_toplevel_req = read_packet(&mut compositor).unwrap().unwrap();

    // 6. Client sends XdgToplevel.set_app_id (opcode=3) with original app_id "my-app".
    {
        let mut body = vec![];
        proto::write_arg_string(&mut body, "my-app".to_string()).unwrap();
        write_packet(
            &mut client,
            &Packet {
                id: 5,
                opcode: 3,
                body,
            },
        )
        .unwrap();
    }
    let modified = read_packet(&mut compositor).unwrap().unwrap();

    // 7. Filterway should have replaced app_id "my-app" with "filtered".
    let mut cursor = std::io::Cursor::new(&modified.body[..]);
    let replaced = proto::read_arg_string(&mut cursor).unwrap();
    assert_eq!(
        replaced.as_deref(),
        Some("filtered"),
        "app_id replacement failed: got {replaced:?}"
    );

    // Cleanup.
    filterway.kill().unwrap();
    let output = filterway.wait_with_output().unwrap();
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if !stderr.is_empty() {
            eprintln!("filterway stderr:\n{stderr}");
        }
    }
}
