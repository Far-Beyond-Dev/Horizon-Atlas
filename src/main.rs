use socket2::{Socket, Domain, Type, Protocol};
use std::net::{SocketAddr, TcpStream};
use std::io::{Read, Write};
use std::thread;

// Minimal, robust TCP passthrough WebSocket proxy
fn handle_client(mut client: TcpStream, target_addr: SocketAddr) {
    // --- Handshake with client ---
    let mut buf = [0u8; 4096];
    let n = match client.read(&mut buf) {
        Ok(n) => n,
        Err(_) => return,
    };
    // --- Connect to target server ---
    let mut server = match TcpStream::connect(target_addr) {
        Ok(s) => s,
        Err(_) => return,
    };
    // --- Forward client's handshake to target server ---
    if server.write_all(&buf[..n]).is_err() {
        return;
    }
    // --- Read handshake response from target server ---
    let mut server_handshake_buf = [0u8; 4096];
    let server_handshake_n = match server.read(&mut server_handshake_buf) {
        Ok(n) => n,
        Err(_) => return,
    };
    if server_handshake_n == 0 {
        return;
    }
    // --- Forward handshake response to client ---
    if client.write_all(&server_handshake_buf[..server_handshake_n]).is_err() {
        return;
    }
    // --- Raw bidirectional TCP passthrough ---
    let mut client_for_server = match client.try_clone() {
        Ok(c) => c,
        Err(_) => return,
    };
    let mut server_for_client = match server.try_clone() {
        Ok(s) => s,
        Err(_) => return,
    };
    let c2s = thread::spawn(move || {
        let mut buf = [0u8; 4096];
        loop {
            match client.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    if server.write_all(&buf[..n]).is_err() {
                        break;
                    }
                },
                Err(_) => break,
            }
        }
    });
    let s2c = thread::spawn(move || {
        let mut buf = [0u8; 4096];
        loop {
            match server_for_client.read(&mut buf) {
                Ok(0) => {
                    // Server disconnected, shutdown client socket
                    let _ = client_for_server.shutdown(std::net::Shutdown::Both);
                    break;
                },
                Ok(n) => {
                    if client_for_server.write_all(&buf[..n]).is_err() {
                        break;
                    }
                },
                Err(_) => break,
            }
        }
    });
    let _ = c2s.join();
    let _ = s2c.join();
}

fn main() {
    let listen_addr: SocketAddr = "0.0.0.0:9000".parse().unwrap();
    let target_addr: SocketAddr = "127.0.0.1:8080".parse().unwrap();
    let socket = Socket::new(Domain::IPV4, Type::STREAM, Some(Protocol::TCP)).unwrap();
    socket.set_reuse_address(true).unwrap();
    socket.bind(&listen_addr.into()).unwrap();
    socket.listen(128).unwrap();
    println!("Proxy listening on {}", listen_addr);
    loop {
        let (client, _) = socket.accept().unwrap();
        let client = client.into();
        let target_addr = target_addr.clone();
        thread::spawn(move || {
            handle_client(client, target_addr);
        });
    }
}