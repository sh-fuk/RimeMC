//! Local admin control channel used by `./rime sync` and friends.
//!
//! The server listens on 127.0.0.1 (random port) and writes `port token` into
//! `.rime-control` with mode 0600, so only the OS user running the server (the
//! operator) can issue commands. Tokens are compared in constant time and the
//! socket never accepts non-loopback peers.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};

use crate::ServerState;

pub const CONTROL_FILE: &str = ".rime-control";

fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn write_private(path: &Path, content: &str) -> std::io::Result<()> {
    let _ = std::fs::remove_file(path);
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(path)?;
        f.write_all(content.as_bytes())
    }
    #[cfg(not(unix))]
    {
        std::fs::write(path, content)
    }
}

pub async fn serve(state: Arc<ServerState>, dir: &Path) -> anyhow::Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let port = listener.local_addr()?.port();
    let token = hex::encode(rand::random::<[u8; 32]>());
    write_private(&dir.join(CONTROL_FILE), &format!("{port} {token}\n"))?;
    let token = Arc::new(token);
    loop {
        let (sock, addr) = listener.accept().await?;
        if !addr.ip().is_loopback() {
            continue;
        }
        let (st, tok) = (state.clone(), token.clone());
        tokio::spawn(async move {
            let _ = tokio::time::timeout(Duration::from_secs(120), handle(sock, st, tok)).await;
        });
    }
}

async fn handle(sock: TcpStream, state: Arc<ServerState>, token: Arc<String>) -> anyhow::Result<()> {
    let (r, mut w) = sock.into_split();
    let mut line = String::new();
    let mut r = BufReader::new(r).take(512);
    r.read_line(&mut line).await?;
    let mut parts = line.trim().splitn(2, ' ');
    let (Some(tok), Some(cmd)) = (parts.next(), parts.next()) else {
        w.write_all(b"ERR bad request\n").await?;
        return Ok(());
    };
    if !ct_eq(tok.as_bytes(), token.as_bytes()) {
        tokio::time::sleep(Duration::from_millis(200)).await;
        w.write_all(b"ERR unauthorized\n").await?;
        return Ok(());
    }
    let reply = state.run_admin_command(cmd).await;
    w.write_all(reply.as_bytes()).await?;
    w.write_all(b"\n").await?;
    Ok(())
}

/// Client side: `rime sync` / `rime status` from a shell in the server directory.
pub async fn send(dir: &Path, cmd: &str) -> anyhow::Result<String> {
    let text = std::fs::read_to_string(dir.join(CONTROL_FILE)).map_err(|_| anyhow::anyhow!("no running RimeMC server found in {} (missing {CONTROL_FILE})", dir.display()))?;
    let mut it = text.split_whitespace();
    let port: u16 = it.next().and_then(|p| p.parse().ok()).ok_or_else(|| anyhow::anyhow!("corrupt {CONTROL_FILE}"))?;
    let token = it.next().ok_or_else(|| anyhow::anyhow!("corrupt {CONTROL_FILE}"))?;
    let mut s = TcpStream::connect(("127.0.0.1", port)).await.map_err(|e| anyhow::anyhow!("server not reachable: {e}"))?;
    s.write_all(format!("{token} {cmd}\n").as_bytes()).await?;
    let mut out = String::new();
    BufReader::new(s).take(1 << 20).read_line(&mut out).await?;
    Ok(out.trim_end().to_string())
}

#[cfg(test)]
mod tests {
    #[test]
    fn ct() {
        assert!(super::ct_eq(b"abc", b"abc"));
        assert!(!super::ct_eq(b"abc", b"abd"));
        assert!(!super::ct_eq(b"abc", b"ab"));
    }
}
