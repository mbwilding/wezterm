use crate::config::Config;
use crate::server::codec::*;
use crate::server::{UnixListener, UnixStream};
use failure::{err_msg, Error};
#[cfg(unix)]
use libc::{mode_t, umask};
use promise::Executor;
use std::fs::{remove_file, DirBuilder};
#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::Path;
use std::sync::Arc;
use std::thread;

pub struct Listener {
    acceptor: UnixListener,
    executor: Box<Executor>,
}

impl Listener {
    pub fn new(acceptor: UnixListener, executor: Box<Executor>) -> Self {
        Self { acceptor, executor }
    }

    fn run(&mut self) {
        for stream in self.acceptor.incoming() {
            match stream {
                Ok(stream) => {
                    let executor = self.executor.clone_executor();
                    let mut session = ClientSession::new(stream, executor);
                    thread::spawn(move || session.run());
                }
                Err(err) => {
                    eprintln!("accept failed: {}", err);
                    return;
                }
            }
        }
    }
}

pub struct ClientSession {
    stream: UnixStream,
    executor: Box<Executor>,
}

impl ClientSession {
    fn new(stream: UnixStream, executor: Box<Executor>) -> Self {
        Self { stream, executor }
    }

    fn process(&mut self) -> Result<(), Error> {
        loop {
            let decoded = Pdu::decode(&mut self.stream)?;
            eprintln!("got pdu {:?} from client", decoded);
            match decoded.pdu {
                Pdu::Ping(Ping {}) => {
                    Pdu::Pong(Pong {}).encode(&mut self.stream, decoded.serial)?;
                }

                Pdu::Pong { .. } | Pdu::Invalid { .. } => {}
            }
        }
    }

    fn run(&mut self) {
        self.process().ok();
    }
}

/// Unfortunately, novice unix users can sometimes be running
/// with an overly permissive umask so we take care to install
/// a more restrictive mask while we might be creating things
/// in the filesystem.
/// This struct locks down the umask for its lifetime, restoring
/// the prior umask when it is dropped.
struct UmaskSaver {
    #[cfg(unix)]
    mask: mode_t,
}

impl UmaskSaver {
    fn new() -> Self {
        Self {
            #[cfg(unix)]
            mask: unsafe { umask(0o077) },
        }
    }
}

impl Drop for UmaskSaver {
    fn drop(&mut self) {
        #[cfg(unix)]
        unsafe {
            umask(self.mask);
        }
    }
}

/// Take care when setting up the listener socket;
/// we need to be sure that the directory that we create it in
/// is owned by the user and has appropriate file permissions
/// that prevent other users from manipulating its contents.
fn safely_create_sock_path(sock_path: &String) -> Result<UnixListener, Error> {
    let sock_path = Path::new(sock_path);

    eprintln!("setting up {}", sock_path.display());

    let _saver = UmaskSaver::new();

    let sock_dir = sock_path
        .parent()
        .ok_or_else(|| format_err!("sock_path {} has no parent dir", sock_path.display()))?;

    let mut builder = DirBuilder::new();
    builder.recursive(true);

    #[cfg(unix)]
    {
        builder.mode(0o700);
    }

    builder.create(sock_dir)?;

    // Let's be sure that the ownership looks sane
    let meta = sock_dir.symlink_metadata()?;

    #[cfg(unix)]
    {
        let permissions = meta.permissions();
        if (permissions.mode() & 0o22) != 0 {
            bail!(
                "The permissions for {} are insecure and currently
                allow other users to write to it (permissions={:?})",
                sock_dir.display(),
                permissions
            );
        }
    }

    if sock_path.exists() {
        remove_file(sock_path)?;
    }

    UnixListener::bind(sock_path)
        .map_err(|e| format_err!("Failed to bind to {}: {}", sock_path.display(), e))
}

pub fn spawn_listener(config: &Arc<Config>, executor: Box<Executor>) -> Result<(), Error> {
    let sock_path = config
        .mux_server_unix_domain_socket_path
        .as_ref()
        .ok_or_else(|| err_msg("no mux_server_unix_domain_socket_path"))?;
    let mut listener = Listener::new(safely_create_sock_path(sock_path)?, executor);
    thread::spawn(move || {
        listener.run();
    });
    Ok(())
}
