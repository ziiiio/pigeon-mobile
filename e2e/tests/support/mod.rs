//! The out-of-process homeserver for the e2e lane.
//!
//! Each test gets a fresh, real `pigeon-server` **binary** (built from the
//! sibling `../pigeon` checkout) on a throwaway Postgres container, and talks to
//! it over HTTP exactly as the apps do.
//!
//! Why a separate process rather than the server's in-process test harness: an
//! in-process server put the homeserver's dependency graph into this crate's
//! binary, and Cargo allows one SQLite-linking package per binary. The server's
//! (unused) sqlx SQLite backend capped `libsqlite3-sys` below the version that
//! fixes RUSTSEC-2026-0459, which pinned the *app's* rusqlite to a vulnerable
//! release just so this test could link (CLAUDE.md, "Tests never pin
//! dependencies"). Out of process, the two graphs never meet.
//!
//! No new dependencies: Postgres is started with the `docker` CLI and the server
//! with `cargo build` + `std::process`. Needs Docker and the `../pigeon` checkout.

use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

/// The server name the tests' user ids use (`@alice:test.example`).
pub const SERVER_NAME: &str = "test.example";

/// A running homeserver and its database. Dropping it stops both.
pub struct LiveServer {
    pub base_url: String,
    child: Child,
    container: String,
    dir: PathBuf,
}

impl Drop for LiveServer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = Command::new("docker")
            .args(["rm", "-f", &self.container])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// `../pigeon`, the homeserver repo next to this one.
fn pigeon_repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../pigeon")
}

/// Build `pigeon-server` once per test run and return its path. `SQLX_OFFLINE`
/// compiles the server's query macros against its committed `.sqlx` cache, so
/// building needs no database.
fn server_binary() -> anyhow::Result<PathBuf> {
    static BIN: OnceLock<Result<PathBuf, String>> = OnceLock::new();
    BIN.get_or_init(|| {
        let repo = pigeon_repo();
        let status = Command::new(env!("CARGO"))
            .args(["build", "-q", "-p", "pigeon-server", "--bin", "pigeon-server"])
            .current_dir(&repo)
            .env("SQLX_OFFLINE", "true")
            .status()
            .map_err(|e| format!("running cargo build in {}: {e}", repo.display()))?;
        if !status.success() {
            return Err(format!("building pigeon-server in {} failed", repo.display()));
        }
        Ok(repo.join("target/debug/pigeon-server"))
    })
    .clone()
    .map_err(anyhow::Error::msg)
}

/// Run `docker` and return its trimmed stdout, or an error with its stderr.
fn docker(args: &[&str]) -> anyhow::Result<String> {
    let out = Command::new("docker").args(args).output()?;
    if !out.status.success() {
        anyhow::bail!(
            "docker {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

/// Start a throwaway Postgres 16 on a random local port; returns (id, url).
fn start_postgres() -> anyhow::Result<(String, String)> {
    let id = docker(&[
        "run", "-d", "--rm",
        "-e", "POSTGRES_PASSWORD=pigeon",
        "-e", "POSTGRES_DB=pigeon",
        "-p", "127.0.0.1::5432",
        "postgres:16",
    ])?;
    // `docker port` prints e.g. `127.0.0.1:55012`.
    let mapped = docker(&["port", &id, "5432/tcp"])?;
    let addr = mapped.lines().next().unwrap_or_default().to_owned();
    // Wait until Postgres accepts connections (it restarts once during init).
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let ready = Command::new("docker")
            .args(["exec", &id, "pg_isready", "-U", "postgres", "-d", "pigeon", "-h", "127.0.0.1"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if ready {
            break;
        }
        if Instant::now() > deadline {
            let _ = docker(&["rm", "-f", &id]);
            anyhow::bail!("Postgres container {id} never became ready");
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    Ok((id, format!("postgres://postgres:pigeon@{addr}/pigeon")))
}

/// A free local port (bound then released; the server binds it next).
fn free_port() -> anyhow::Result<u16> {
    Ok(TcpListener::bind(("127.0.0.1", 0))?.local_addr()?.port())
}

/// Boot a fresh homeserver: build (once), start Postgres, write a config, run
/// the binary, and wait for `/readiness`.
pub async fn spawn() -> anyhow::Result<LiveServer> {
    let bin = server_binary()?;
    let (container, db_url) = start_postgres()?;
    let port = free_port()?;
    let dir = std::env::temp_dir().join(format!("pigeon-e2e-{}-{port}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    let config = dir.join("config.toml");
    std::fs::write(
        &config,
        format!(
            "server_name = \"{SERVER_NAME}\"\n\
             database_url = \"{db_url}\"\n\
             listen_addr = \"127.0.0.1:{port}\"\n\
             signing_key_path = \"{}\"\n",
            dir.join("signing.key").display()
        ),
    )?;
    let log = std::fs::File::create(dir.join("server.log"))?;
    let child = Command::new(&bin)
        .arg("--config")
        .arg(&config)
        .stdout(log.try_clone()?)
        .stderr(log)
        .spawn()?;
    let mut server = LiveServer {
        base_url: format!("http://127.0.0.1:{port}"),
        child,
        container,
        dir,
    };

    // Ready once migrations have run and the DB answers.
    let http = reqwest::Client::new();
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        if let Ok(resp) = http.get(format!("{}/readiness", server.base_url)).send().await {
            if resp.status().is_success() {
                return Ok(server);
            }
        }
        if let Ok(Some(status)) = server.child.try_wait() {
            let log = std::fs::read_to_string(server.dir.join("server.log")).unwrap_or_default();
            anyhow::bail!("pigeon-server exited early ({status}):\n{log}");
        }
        if Instant::now() > deadline {
            anyhow::bail!("pigeon-server never became ready at {}", server.base_url);
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}
