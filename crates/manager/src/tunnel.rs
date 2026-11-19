//! 页面的世界连接：局域网里页面只认识管理服务的地址，这里把 WebSocket 升级原样接到本机世界。
use crate::error::ApiError;
use crate::{routes, App};
use std::io::{self, BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const HEAD_LIMIT: usize = 64 * 1024;

pub fn wants(req: &crate::http::Request) -> bool {
    req.method == "GET"
        && req
            .header("upgrade")
            .is_some_and(|v| v.eq_ignore_ascii_case("websocket"))
}

fn refuse(status: u16) -> ApiError {
    ApiError::new(status, "世界连接未获允许")
}

/// 只转这一条升级请求；之后两边都是原样的 TCP 字节，世界那边的 WebSocket 自己继续握手。
pub fn forward(app: &App, mut page: TcpStream, req: &crate::http::Request) -> Result<(), ApiError> {
    routes::check_authority(app, req).map_err(|_| refuse(403))?;
    if app.stopping() {
        return Err(refuse(503));
    }
    let Some(rest) = req.path.strip_prefix("/api/towns/") else {
        return Err(refuse(404));
    };
    let Some(id) = rest.strip_suffix("/socket") else {
        return Err(refuse(404));
    };
    if crate::storage::safe_id(id).is_err() {
        return Err(refuse(404));
    }
    let session = req.query.get("session").ok_or_else(|| refuse(403))?;
    let endpoint = app
        .world_connection(id, session)
        .ok_or_else(|| refuse(403))?;
    let Some(target) = endpoint.strip_prefix("ws://") else {
        return Err(refuse(502));
    };
    let Some((authority, path)) = target.split_once('/') else {
        return Err(refuse(502));
    };
    let Some(address) = world_address(authority) else {
        return Err(refuse(502));
    };
    let mut world =
        TcpStream::connect_timeout(&address, CONNECT_TIMEOUT).map_err(|_| refuse(502))?;
    // 世界端只认它自己签发的口令；其余原样带上，包括页面的来源。
    let mut head = format!("GET /{path} HTTP/1.1\r\n");
    for name in [
        "host",
        "origin",
        "sec-websocket-key",
        "sec-websocket-version",
        "sec-websocket-protocol",
    ] {
        if let Some(value) = req.header(name) {
            head.push_str(&format!("{name}: {value}\r\n"));
        }
    }
    head.push_str("Connection: Upgrade\r\nUpgrade: websocket\r\n\r\n");
    world
        .set_read_timeout(Some(CONNECT_TIMEOUT))
        .map_err(ApiError::from)?;
    world.write_all(head.as_bytes()).map_err(ApiError::from)?;

    let mut reader = BufReader::new(&mut world);
    let mut reply = Vec::new();
    loop {
        let buf = reader.fill_buf()?;
        if buf.is_empty() {
            return Err(refuse(502));
        }
        let end = buf.windows(4).position(|w| w == b"\r\n\r\n");
        let take = end.map_or(buf.len(), |end| end + 4);
        reply.extend_from_slice(&buf[..take]);
        reader.consume(take);
        if reply.len() > HEAD_LIMIT {
            return Err(refuse(502));
        }
        if end.is_some() {
            break;
        }
    }
    let status = String::from_utf8_lossy(&reply)
        .split(' ')
        .nth(1)
        .and_then(|s| s.parse::<u16>().ok())
        .unwrap_or(502);
    page.write_all(&reply).map_err(ApiError::from)?;
    if status != 101 {
        let _ = io::copy(&mut reader, &mut page);
        return Ok(());
    }
    drop(reader);
    world.set_read_timeout(None).map_err(ApiError::from)?;
    // 两边各有线程抄，任一方向断开就关掉整条连接，另一个线程随即退出。
    let mut world_copy = world.try_clone()?;
    let mut page_copy = page.try_clone()?;
    let copy = std::thread::spawn(move || {
        let _ = io::copy(&mut world_copy, &mut page_copy);
        let _ = page_copy.shutdown(std::net::Shutdown::Both);
    });
    let _ = io::copy(&mut page, &mut world);
    let _ = world.shutdown(std::net::Shutdown::Both);
    let _ = copy.join();
    Ok(())
}

/// 世界进程只在本机监听；地址来自管理服务自己的就绪文件。
fn world_address(authority: &str) -> Option<SocketAddr> {
    let (host, port) = authority.rsplit_once(':')?;
    let port: u16 = port.parse().ok()?;
    match host {
        "127.0.0.1" | "localhost" | "::1" | "[::1]" => {}
        _ => return None,
    }
    format!("127.0.0.1:{port}").parse().ok()
}
