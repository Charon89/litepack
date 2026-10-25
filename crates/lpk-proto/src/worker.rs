//! The worker loop: `Hello`, then requests until `Close` or the host goes away.
//!
//! Requests are read by a thread into a small channel, so that a `Cancel` is seen between the
//! data frames of a member being streamed. Responses are written by the calling thread only;
//! a full pipe blocks it (back-pressure is the host's pulling). A panic in a reader becomes a
//! `Fatal` frame (and a line on the diagnostics pipe) and the worker exits. The loop writes no
//! file and never touches `TEMP`: its only outputs are the response and diagnostics pipes.

use std::collections::VecDeque;
use std::fs::File;
use std::io::{Read, Write};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::mpsc;

use crate::frame::{Record, Request, Response, DATA_CHUNK, MAX_BULK};
use crate::reader::{ErrorCode, ForeignReader, Opener, ReaderError, Stopped};

/// Exit status after `Close` or when the host closed the request pipe.
pub const EXIT_OK: i32 = 0;
/// Exit status when the response pipe failed.
pub const EXIT_PIPE: i32 = 2;
/// Exit status after a reader panicked (a `Fatal` frame was sent).
pub const EXIT_PANIC: i32 = 101;
/// The most records the worker puts in one page, whatever the host asks.
pub const PAGE_MAX: usize = 4096;

/// Deliberate misbehaviour for the host's error tests (feature `faults`; never in a shipped
/// worker).
#[cfg(feature = "faults")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fault {
    /// Answer every request after `Hello` with bytes that are not a frame.
    Garbage,
    /// Never answer `Open` (sleep).
    Hang,
    /// Close the response pipe at `Open` and stay alive.
    ClosePipe,
    /// Answer every `List` with one new record and `more = true`, forever.
    EndlessMore,
    /// Stream member data in small, slow pieces (so a test can cancel mid-stream).
    SlowData,
    /// Leave a file in `TEMP` (the AppContainer's shared folder), then behave.
    Litter,
    /// Panic at `Open`.
    Panic,
    /// Answer `ReadMember` with empty `Data` frames, slowly, forever.
    EmptyData,
    /// Stop reading requests once `Open` and the first `List` are read (the host's
    /// `ReadMember` is never read; a large one would fill the pipe).
    HangReadMember,
}

#[cfg(feature = "faults")]
impl Fault {
    /// The fault named `s` (`garbage`, `hang`, `close`, `endless-more`, `slow-data`, `litter`,
    /// `panic`).
    pub fn parse(s: &str) -> Option<Fault> {
        Some(match s {
            "garbage" => Fault::Garbage,
            "hang" => Fault::Hang,
            "close" => Fault::ClosePipe,
            "endless-more" => Fault::EndlessMore,
            "slow-data" => Fault::SlowData,
            "litter" => Fault::Litter,
            "panic" => Fault::Panic,
            "empty-data" => Fault::EmptyData,
            "hang-read-member" => Fault::HangReadMember,
            _ => return None,
        })
    }
}

/// How the loop runs.
#[derive(Debug, Clone, Default)]
pub struct Config {
    /// The fault to inject.
    #[cfg(feature = "faults")]
    pub fault: Option<Fault>,
}

/// The argument prefix that selects a fault (`--fault=<name>`; feature `faults` only).
pub const FAULT_ARG: &str = "--fault=";

impl Config {
    /// The configuration from the worker's arguments: `--fault=<name>` with the `faults`
    /// feature; without it any `--fault=` argument is refused, so a shipped worker cannot be
    /// told to misbehave.
    pub fn from_args(args: &[String]) -> Result<Config, String> {
        #[cfg_attr(not(feature = "faults"), allow(unused_mut))]
        let mut cfg = Config::default();
        for a in args {
            if let Some(name) = a.strip_prefix(FAULT_ARG) {
                #[cfg(feature = "faults")]
                {
                    cfg.fault =
                        Some(Fault::parse(name).ok_or_else(|| format!("unknown fault {name}"))?);
                }
                #[cfg(not(feature = "faults"))]
                return Err(format!("fault {name}: this worker is built without faults"));
            }
        }
        Ok(cfg)
    }

    #[cfg(feature = "faults")]
    fn is(&self, f: Fault) -> bool {
        self.fault == Some(f)
    }
    #[cfg(not(feature = "faults"))]
    fn is(&self, _f: ()) -> bool {
        false
    }
}

/// Fault names usable whether or not the feature is on (always false without it).
#[cfg(feature = "faults")]
use Fault as F;
#[cfg(not(feature = "faults"))]
#[allow(non_snake_case, non_upper_case_globals)]
mod F {
    pub const Garbage: () = ();
    pub const Hang: () = ();
    pub const ClosePipe: () = ();
    pub const EndlessMore: () = ();
    pub const SlowData: () = ();
    pub const Litter: () = ();
    pub const Panic: () = ();
    pub const EmptyData: () = ();
    pub const HangReadMember: () = ();
}

/// What the request thread hands on.
enum Incoming {
    Req(Request),
    /// The request pipe ended or failed.
    Gone,
}

fn panic_text(p: &(dyn std::any::Any + Send)) -> String {
    p.downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| p.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "panic".to_string())
}

fn sleep_forever() -> ! {
    loop {
        std::thread::sleep(std::time::Duration::from_secs(3600));
    }
}

/// Serve one archive: `requests` from the host, `responses` to it, the archive handle, an
/// optional diagnostics pipe, and the opener of the format's reader. Returns the exit status.
pub fn serve<R, W>(
    requests: R,
    responses: W,
    archive: File,
    mut diag: Option<File>,
    opener: &Opener,
    cfg: &Config,
) -> i32
where
    R: Read + Send + 'static,
    W: Write,
{
    let mut out = responses;
    if cfg.is(F::Litter) {
        let _ = std::fs::write(std::env::temp_dir().join("lpk-litter.txt"), b"litter");
    }
    let (tx, rx) = mpsc::sync_channel::<Incoming>(16);
    let mut requests = requests;
    let deaf_after = if cfg.is(F::HangReadMember) {
        Some(2)
    } else {
        None
    };
    let spawned = std::thread::Builder::new()
        .name("lpk-requests".into())
        .spawn(move || {
            let mut buf = Vec::new();
            let mut read = 0u32;
            loop {
                if deaf_after == Some(read) {
                    sleep_forever();
                }
                read += 1;
                match Request::read_from(&mut requests, &mut buf) {
                    Ok(Some(r)) => {
                        if tx.send(Incoming::Req(r)).is_err() {
                            return;
                        }
                    }
                    _ => {
                        let _ = tx.send(Incoming::Gone);
                        return;
                    }
                }
            }
        });
    if spawned.is_err() {
        return EXIT_PIPE;
    }
    let hello = Response::Hello {
        proto: crate::PROTO,
        build: crate::BUILD.to_string(),
    };
    if hello.write_to(&mut out).is_err() {
        return EXIT_PIPE;
    }
    let mut reader: Option<Box<dyn ForeignReader>> = None;
    let mut page_from_endless = 0u64;
    let mut pending: VecDeque<Request> = VecDeque::new();
    loop {
        let req = match pending.pop_front() {
            Some(r) => r,
            None => match rx.recv() {
                Ok(Incoming::Req(r)) => r,
                Ok(Incoming::Gone) | Err(_) => return EXIT_OK,
            },
        };
        if cfg.is(F::Garbage) {
            // Not a frame: an unknown kind with a length past every cap.
            let junk = [0xFFu8, 0xFF, 0xFF, 0x7F, 0xEF, 0xBE, 1, 2, 3, 4, 5, 6, 7, 8];
            if out.write_all(&junk).and_then(|_| out.flush()).is_err() {
                return EXIT_PIPE;
            }
            continue;
        }
        let reply = match req {
            Request::Close => return EXIT_OK,
            Request::Cancel => Response::Cancelled,
            Request::Open { password } => {
                if cfg.is(F::Hang) {
                    sleep_forever();
                }
                if cfg.is(F::ClosePipe) {
                    drop(out);
                    sleep_forever();
                }
                if reader.is_some() {
                    error(ErrorCode::Protocol, "already open")
                } else {
                    let file = match archive.try_clone() {
                        Ok(f) => f,
                        Err(e) => return finish(&mut out, error(ErrorCode::Io, e.to_string())),
                    };
                    let pw = password.as_ref().map(|p| p.as_slice());
                    let opened = catch_unwind(AssertUnwindSafe(|| {
                        if cfg.is(F::Panic) {
                            panic!("fault: panic at open");
                        }
                        opener(file, pw)
                    }));
                    match opened {
                        Ok(Ok(r)) => {
                            let format = r.format();
                            reader = Some(r);
                            Response::Opened { format }
                        }
                        Ok(Err(e)) if e.code == ErrorCode::NeedPassword => Response::NeedPassword,
                        Ok(Err(e)) => from_reader(e),
                        Err(p) => return fatal(&mut out, &mut diag, &*p),
                    }
                }
            }
            Request::List { from, max } => match reader.as_mut() {
                None => error(ErrorCode::Protocol, "not open"),
                Some(_) if cfg.is(F::EndlessMore) => {
                    page_from_endless += 1;
                    Response::Entries {
                        records: vec![Record {
                            kind: crate::frame::RecordKind::File,
                            flags: 0,
                            mtime_ns: i64::MIN,
                            size: 0,
                            packed_offset: 0,
                            packed_len: 0,
                            block: crate::frame::NO_BLOCK,
                            name: format!("endless/{page_from_endless}").into_bytes(),
                            target: Vec::new(),
                        }],
                        more: true,
                    }
                }
                Some(r) => {
                    let max = usize::try_from(max).unwrap_or(PAGE_MAX).min(PAGE_MAX);
                    match catch_unwind(AssertUnwindSafe(|| r.entries(from, max))) {
                        Ok(Ok((records, more))) => page(records, more),
                        Ok(Err(e)) => from_reader(e),
                        Err(p) => return fatal(&mut out, &mut diag, &*p),
                    }
                }
            },
            Request::ReadMember { indices } => {
                let Some(r) = reader.as_mut() else {
                    if error(ErrorCode::Protocol, "not open")
                        .write_to(&mut out)
                        .is_err()
                    {
                        return EXIT_PIPE;
                    }
                    continue;
                };
                if indices.windows(2).any(|w| w[0] >= w[1]) {
                    error(ErrorCode::Protocol, "indices not strictly ascending")
                } else {
                    match stream(&mut **r, &indices, &mut out, &rx, &mut pending, cfg) {
                        Streamed::Done => continue,
                        Streamed::Reply(resp) => resp,
                        Streamed::Pipe => return EXIT_PIPE,
                        Streamed::Panic(p) => return fatal(&mut out, &mut diag, &*p),
                    }
                }
            }
        };
        if reply.write_to(&mut out).is_err() {
            return EXIT_PIPE;
        }
    }
}

/// The roles of the handles a worker receives; equal to `lpk_sandbox_sys::roles` (the host's
/// tests check it), repeated here so this crate depends on no launcher.
pub mod roles {
    /// Requests, host to worker.
    pub const REQUESTS: u32 = 1;
    /// Responses, worker to host.
    pub const RESPONSES: u32 = 2;
    /// Diagnostics, worker to host.
    pub const STDERR: u32 = 3;
    /// The archive.
    pub const ARCHIVE: u32 = 16;
}

/// Exit status when a needed handle was not passed.
pub const EXIT_NO_HANDLES: i32 = 4;
/// Exit status when the arguments are refused (a fault asked of a worker built without them).
pub const EXIT_BAD_ARGS: i32 = 5;

/// A worker binary's body after it took its handles: pick the pipes and the archive by role,
/// read the configuration from `args`, and [`serve`]. Returns the exit status.
pub fn run(handles: Vec<(u32, File)>, args: &[String], opener: &Opener) -> i32 {
    let (mut req, mut resp, mut diag, mut archive) = (None, None, None, None);
    for (role, f) in handles {
        match role {
            roles::REQUESTS => req = Some(f),
            roles::RESPONSES => resp = Some(f),
            roles::STDERR => diag = Some(f),
            roles::ARCHIVE => archive = Some(f),
            _ => {}
        }
    }
    let (Some(req), Some(resp), Some(archive)) = (req, resp, archive) else {
        return EXIT_NO_HANDLES;
    };
    let cfg = match Config::from_args(args) {
        Ok(c) => c,
        Err(e) => {
            if let Some(d) = diag.as_mut() {
                let _ = writeln!(d, "{e}");
            }
            return EXIT_BAD_ARGS;
        }
    };
    serve(
        req,
        std::io::BufWriter::with_capacity(DATA_CHUNK + 64, resp),
        archive,
        diag,
        opener,
        &cfg,
    )
}

fn error(code: ErrorCode, message: impl Into<String>) -> Response {
    Response::Error {
        code: code as u16,
        message: message.into(),
    }
}

fn from_reader(e: ReaderError) -> Response {
    error(e.code, e.message)
}

fn finish(out: &mut dyn Write, r: Response) -> i32 {
    match r.write_to(out) {
        Ok(()) => EXIT_OK,
        Err(_) => EXIT_PIPE,
    }
}

fn fatal(out: &mut dyn Write, diag: &mut Option<File>, p: &(dyn std::any::Any + Send)) -> i32 {
    let message = panic_text(p);
    if let Some(d) = diag.as_mut() {
        let _ = writeln!(d, "worker panicked: {message}");
    }
    let _ = Response::Fatal { message }.write_to(out);
    EXIT_PANIC
}

/// A page that fits one bulk frame: records past the cap are left for the next page.
fn page(mut records: Vec<Record>, mut more: bool) -> Response {
    let mut size = 5usize;
    let mut keep = 0;
    for r in &records {
        let n = r.encoded_len();
        if size + n > MAX_BULK {
            break;
        }
        size += n;
        keep += 1;
    }
    if keep < records.len() {
        records.truncate(keep);
        more = true;
    }
    Response::Entries { records, more }
}

enum Streamed {
    /// Every member streamed (frames already sent).
    Done,
    /// Send this and go on reading requests.
    Reply(Response),
    Pipe,
    Panic(Box<dyn std::any::Any + Send>),
}

fn stream(
    r: &mut dyn ForeignReader,
    indices: &[u64],
    out: &mut dyn Write,
    rx: &mpsc::Receiver<Incoming>,
    pending: &mut VecDeque<Request>,
    cfg: &Config,
) -> Streamed {
    #[derive(PartialEq)]
    enum Stop {
        No,
        Cancel,
        Pipe,
    }
    let mut stop = Stop::No;
    let slow = cfg.is(F::SlowData);
    if cfg.is(F::EmptyData) {
        loop {
            if Response::Data(Vec::new()).write_to(out).is_err() {
                return Streamed::Pipe;
            }
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
    }
    for &index in indices {
        if (Response::MemberStart { index }).write_to(out).is_err() {
            return Streamed::Pipe;
        }
        let mut sink = |mut b: &[u8]| -> Result<(), Stopped> {
            let piece = if slow { 4 << 10 } else { DATA_CHUNK };
            while !b.is_empty() {
                match rx.try_recv() {
                    Ok(Incoming::Req(Request::Cancel)) => stop = Stop::Cancel,
                    // Anything else (`Close` too) waits for the end of the stream; a host
                    // that went away is seen at the next write.
                    Ok(Incoming::Req(r)) => pending.push_back(r),
                    Ok(Incoming::Gone) => pending.push_back(Request::Close),
                    Err(mpsc::TryRecvError::Empty) | Err(mpsc::TryRecvError::Disconnected) => {}
                }
                if stop != Stop::No {
                    return Err(Stopped);
                }
                let n = b.len().min(piece);
                let (h, t) = b.split_at(n);
                if Response::Data(h.to_vec()).write_to(out).is_err() {
                    stop = Stop::Pipe;
                    return Err(Stopped);
                }
                if slow {
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
                b = t;
            }
            Ok(())
        };
        let res = catch_unwind(AssertUnwindSafe(|| r.read_member(index, &mut sink)));
        match res {
            Err(p) => return Streamed::Panic(p),
            Ok(_) if stop == Stop::Pipe => return Streamed::Pipe,
            Ok(_) if stop == Stop::Cancel => return Streamed::Reply(Response::Cancelled),
            Ok(Err(e)) => return Streamed::Reply(from_reader(e)),
            Ok(Ok(checked)) => {
                if (Response::MemberEnd { index, checked })
                    .write_to(out)
                    .is_err()
                {
                    return Streamed::Pipe;
                }
            }
        }
    }
    Streamed::Done
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture::{self, Member};
    use std::io::Seek;

    /// Requests written to a buffer, served against a fixture in a temp file; the responses.
    fn exchange(fx: &[u8], reqs: &[Request]) -> (i32, Vec<Response>) {
        let path = std::env::temp_dir().join(format!(
            "lpk-proto-worker-{}-{:?}.fx",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::write(&path, fx).unwrap();
        let mut file = File::open(&path).unwrap();
        file.rewind().unwrap();
        let mut input = Vec::new();
        for r in reqs {
            r.write_to(&mut input).unwrap();
        }
        let mut out = Vec::new();
        let code = serve(
            std::io::Cursor::new(input),
            &mut out,
            file,
            None,
            &fixture::open,
            &Config::default(),
        );
        let _ = std::fs::remove_file(&path);
        let mut rs = Vec::new();
        let mut cur = out.as_slice();
        let mut buf = Vec::new();
        while let Some(r) = Response::read_from(&mut cur, &mut buf).unwrap() {
            rs.push(r);
        }
        (code, rs)
    }

    #[test]
    fn a_session_lists_and_streams() {
        let big = vec![7u8; DATA_CHUNK + 10];
        let fx = fixture::write(
            &[
                Member::dir("d"),
                Member::file("d/a", b"abc"),
                Member::file("d/big", &big),
            ],
            None,
        );
        let (code, rs) = exchange(
            &fx,
            &[
                Request::Open { password: None },
                Request::List { from: 0, max: 2 },
                Request::List { from: 2, max: 2 },
                Request::ReadMember {
                    indices: vec![1, 2],
                },
                Request::ReadMember {
                    indices: vec![2, 1],
                },
                Request::Close,
            ],
        );
        assert_eq!(code, EXIT_OK);
        assert!(matches!(&rs[0], Response::Hello { proto: 1, build } if build == crate::BUILD));
        assert!(matches!(&rs[1], Response::Opened { format } if format == fixture::FORMAT));
        assert!(matches!(&rs[2], Response::Entries { records, more: true } if records.len() == 2));
        assert!(matches!(&rs[3], Response::Entries { records, more: false } if records.len() == 1));
        assert_eq!(rs[4], Response::MemberStart { index: 1 });
        assert_eq!(rs[5], Response::Data(b"abc".to_vec()));
        assert_eq!(
            rs[6],
            Response::MemberEnd {
                index: 1,
                checked: false
            }
        );
        assert_eq!(rs[7], Response::MemberStart { index: 2 });
        let mut at = 8;
        let mut total = 0;
        while let Response::Data(d) = &rs[at] {
            assert!(d.len() <= DATA_CHUNK);
            total += d.len();
            at += 1;
        }
        assert_eq!(total, big.len());
        assert!(matches!(rs[at], Response::MemberEnd { index: 2, .. }));
        assert!(
            matches!(&rs[at + 1], Response::Error { code, .. } if *code == ErrorCode::Protocol as u16)
        );
        assert_eq!(rs.len(), at + 2);
    }

    #[test]
    fn passwords_and_order() {
        let fx = fixture::write(&[Member::file("a", b"x")], Some(b"pw"));
        let (_, rs) = exchange(
            &fx,
            &[
                Request::List { from: 0, max: 1 },
                Request::Open { password: None },
                Request::Open {
                    password: Some(zeroize::Zeroizing::new(b"bad".to_vec())),
                },
                Request::Open {
                    password: Some(zeroize::Zeroizing::new(b"pw".to_vec())),
                },
                Request::Cancel,
            ],
        );
        assert!(
            matches!(&rs[1], Response::Error { code, .. } if *code == ErrorCode::Protocol as u16)
        );
        assert_eq!(rs[2], Response::NeedPassword);
        assert!(
            matches!(&rs[3], Response::Error { code, .. } if *code == ErrorCode::WrongPassword as u16)
        );
        assert!(matches!(rs[4], Response::Opened { .. }));
        assert_eq!(rs[5], Response::Cancelled);
    }

    #[test]
    fn pages_fit_one_frame() {
        let rec = |i: usize| Record {
            kind: crate::frame::RecordKind::File,
            flags: 0,
            mtime_ns: 0,
            size: 0,
            packed_offset: 0,
            packed_len: 0,
            block: 0,
            name: vec![b'a'; 60000 + i],
            target: Vec::new(),
        };
        let Response::Entries { records, more } = page((0..40).map(rec).collect(), false) else {
            panic!()
        };
        assert!(more);
        assert!(records.len() < 40 && !records.is_empty());
        let (k, p) = Response::Entries { records, more }.encode();
        assert!(p.len() <= MAX_BULK);
        assert_eq!(k, crate::frame::kind::ENTRIES);
    }

    #[test]
    fn fault_arguments_need_the_feature() {
        let r = Config::from_args(&["--fault=hang".to_string()]);
        #[cfg(feature = "faults")]
        assert_eq!(r.unwrap().fault, Some(Fault::Hang));
        #[cfg(not(feature = "faults"))]
        assert!(r.is_err());
        assert!(Config::from_args(&["other".to_string()]).is_ok());
    }
}
