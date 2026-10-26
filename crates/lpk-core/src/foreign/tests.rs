//! The broker over recorded worker output (the malicious-worker matrix) and over a real
//! sandboxed test worker (round trips, faults, cancellation, the AppContainer folder).

use super::*;
use lpk_proto::fixture::{self, Member};
use lpk_proto::frame::RECORD_MIN;

/// Recorded worker output: frames concatenated.
fn script(frames: &[Response]) -> Vec<u8> {
    let mut out = Vec::new();
    for f in frames {
        f.write_to(&mut out).unwrap();
    }
    out
}

fn hello() -> Response {
    Response::Hello {
        proto: lpk_proto::PROTO,
        build: lpk_proto::BUILD.into(),
    }
}

fn opened() -> Response {
    Response::Opened {
        format: "test".into(),
    }
}

fn rec(name: &str, size: u64, offset: u64) -> Record {
    Record {
        kind: RecordKind::File,
        flags: 0,
        mtime_ns: i64::MIN,
        size,
        packed_offset: offset,
        packed_len: size,
        block: NO_BLOCK,
        name: name.as_bytes().to_vec(),
        target: Vec::new(),
    }
}

fn entries(records: Vec<Record>) -> Response {
    Response::Entries {
        records,
        more: false,
    }
}

/// A good session for one member `a` of `data`.
fn one_member(data: &[u8]) -> Vec<Response> {
    vec![
        hello(),
        opened(),
        entries(vec![rec("a", data.len() as u64, 0)]),
        Response::MemberStart { index: 0 },
        Response::Data(data.to_vec()),
        Response::MemberEnd {
            index: 0,
            checked: false,
        },
    ]
}

fn run(frames: &[Response], archive_len: u64) -> (Result<ForeignSummary, CoreError>, u64) {
    replay(
        &script(frames),
        archive_len,
        None,
        &crate::DefaultPolicy,
        &Limits::default(),
    )
}

#[test]
fn a_recorded_session_replays() {
    let (r, written) = run(&one_member(b"hello"), 100);
    let s = r.unwrap();
    assert_eq!((s.files, s.bytes, written), (1, 5, 5));
}

#[test]
fn the_handshake_is_checked() {
    let mut f = one_member(b"x");
    f[0] = Response::Hello {
        proto: 1,
        build: "other".into(),
    };
    let (r, _) = run(&f, 100);
    assert!(matches!(
        r,
        Err(CoreError::Worker(WorkerError::BuildMismatch { .. }))
    ));
    let (r, _) = run(&f[1..], 100);
    assert!(matches!(
        r,
        Err(CoreError::Worker(WorkerError::Protocol { .. }))
    ));
    let (r, _) = run(&[], 100);
    assert!(matches!(
        r,
        Err(CoreError::Worker(WorkerError::Pipe { .. }))
    ));
}

#[test]
fn credentials_map_to_the_format_errors() {
    let (r, _) = run(&[hello(), Response::NeedPassword], 100);
    assert!(matches!(
        r,
        Err(CoreError::Format(FormatError::PasswordRequired))
    ));
    let (r, _) = run(
        &[
            hello(),
            Response::Error {
                code: ErrorCode::WrongPassword as u16,
                message: "no".into(),
            },
        ],
        100,
    );
    assert!(matches!(r, Err(CoreError::Format(FormatError::WrongKey))));
}

/// Hostile names: refused by the policy pass before any byte is written.
#[test]
fn hostile_names_are_refused_before_any_write() {
    for name in [
        "../evil",
        "a/../../evil",
        "/abs",
        "C:\\evil",
        "\\\\server\\share\\x",
        "CON",
        "dir/NUL.txt",
        "file.txt:ads",
        "trailing.",
        "",
        "a//b",
    ] {
        let mut f = one_member(b"data");
        f[2] = entries(vec![rec(name, 4, 0)]);
        let (r, written) = run(&f, 100);
        assert!(r.is_err(), "{name:?} passed");
        assert_eq!(written, 0, "{name:?}");
        match r {
            Err(CoreError::Format(FormatError::UnsafePath { .. }))
            | Err(CoreError::Format(FormatError::InvalidPath { .. })) => {}
            other => panic!("{name:?}: {other:?}"),
        }
    }
    let mut f = one_member(b"data");
    f[2] = entries(vec![Record {
        name: vec![0xFF, b'a'],
        ..rec("x", 4, 0)
    }]);
    let (r, written) = run(&f, 100);
    assert!(matches!(
        r,
        Err(CoreError::Foreign(ForeignError::Corrupt { .. }))
    ));
    assert_eq!(written, 0);
}

#[test]
fn lying_sizes_are_size_mismatches() {
    // More than declared: refused before the excess is written.
    let mut f = one_member(b"abc");
    f[4] = Response::Data(b"abcdef".to_vec());
    let (r, written) = run(&f, 100);
    assert!(matches!(
        r,
        Err(CoreError::Foreign(ForeignError::SizeMismatch { .. }))
    ));
    assert_eq!(written, 0);
    // Fewer than declared.
    let mut f = one_member(b"abc");
    f[4] = Response::Data(b"a".to_vec());
    let (r, _) = run(&f, 100);
    assert!(matches!(
        r,
        Err(CoreError::Foreign(ForeignError::SizeMismatch {
            declared: 3,
            received: 1,
            ..
        }))
    ));
    // An endless stream for a member declared small.
    let mut f = one_member(b"abc");
    f.truncate(4);
    for _ in 0..1000 {
        f.push(Response::Data(vec![0; 1]));
    }
    let (r, written) = run(&f, 100);
    assert!(matches!(
        r,
        Err(CoreError::Foreign(ForeignError::SizeMismatch { .. }))
    ));
    assert_eq!(written, 3);
}

#[test]
fn an_empty_data_frame_is_a_protocol_error() {
    let mut f = one_member(b"abc");
    f.insert(4, Response::Data(Vec::new()));
    let (r, written) = run(&f, 100);
    assert!(matches!(
        r,
        Err(CoreError::Worker(WorkerError::Protocol { .. }))
    ));
    assert_eq!(written, 0);
}

#[test]
fn declared_output_past_the_expansion_limit_is_refused_before_any_write() {
    let mut f = one_member(b"x");
    let mut r0 = rec("a", 4 << 30, 0);
    r0.packed_len = 1;
    f[2] = entries(vec![r0]);
    let (r, written) = run(&f, 100);
    match r {
        Err(CoreError::Foreign(ForeignError::LimitExceeded { flag, .. })) => {
            assert_eq!(flag, "--max-expansion")
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(written, 0);
}

#[test]
fn ranges_outside_or_overlapping_are_refused() {
    let mut f = one_member(b"x");
    f[2] = entries(vec![rec("a", 50, 60)]);
    let (r, _) = run(&f, 100);
    assert!(matches!(
        r,
        Err(CoreError::Foreign(ForeignError::Corrupt { .. }))
    ));
    let mut f = one_member(b"x");
    let mut b = rec("b", 10, 5);
    b.block = 2;
    let mut a = rec("a", 10, 0);
    a.block = 1;
    f[2] = entries(vec![a, b]);
    let (r, _) = run(&f, 100);
    assert!(matches!(
        r,
        Err(CoreError::Foreign(ForeignError::OverlappingEntries { .. }))
    ));
    // Same block may share bytes (a solid block).
    let mut f = vec![hello(), opened()];
    let mut a = rec("a", 0, 0);
    a.packed_len = 10;
    a.block = 1;
    let mut b = a.clone();
    b.name = b"b".to_vec();
    f.push(entries(vec![a, b]));
    for i in 0..2 {
        f.push(Response::MemberStart { index: i });
        f.push(Response::MemberEnd {
            index: i,
            checked: false,
        });
    }
    let (r, _) = run(&f, 100);
    assert_eq!(r.unwrap().files, 2);
}

#[test]
fn entry_caps_and_endless_pages() {
    // `more = true` forever: the structural cap (archive length / smallest header) stops it.
    let mut f = vec![hello(), opened()];
    for i in 0..100 {
        f.push(Response::Entries {
            records: vec![rec(&format!("e{i}"), 0, 0)],
            more: true,
        });
    }
    let (r, _) = run(&f, 100);
    assert!(
        matches!(
            r,
            Err(CoreError::Foreign(ForeignError::LimitExceeded {
                limit: "entries",
                ..
            }))
        ),
        "{r:?}"
    );
    // --max-entries.
    let limits = Limits {
        max_entries: Some(2),
        ..Limits::default()
    };
    let recs: Vec<Record> = (0..3).map(|i| rec(&format!("e{i}"), 0, 0)).collect();
    let (r, _) = replay(
        &script(&[hello(), opened(), entries(recs)]),
        10_000,
        None,
        &crate::DefaultPolicy,
        &limits,
    );
    assert!(matches!(
        r,
        Err(CoreError::Foreign(ForeignError::LimitExceeded {
            flag: "--max-entries",
            ..
        }))
    ));
    // An empty page saying more follow.
    let (r, _) = run(
        &[
            hello(),
            opened(),
            Response::Entries {
                records: vec![],
                more: true,
            },
        ],
        100,
    );
    assert!(matches!(
        r,
        Err(CoreError::Worker(WorkerError::Protocol { .. }))
    ));
}

#[test]
fn the_listing_is_charged_to_the_memory_budget() {
    let limits = Limits {
        memory: Some(ENTRY_CHARGE * 2),
        ..Limits::default()
    };
    let recs: Vec<Record> = (0..3).map(|i| rec(&format!("e{i}"), 0, 0)).collect();
    let (r, _) = replay(
        &script(&[hello(), opened(), entries(recs)]),
        10_000,
        None,
        &crate::DefaultPolicy,
        &limits,
    );
    assert!(matches!(
        r,
        Err(CoreError::Foreign(ForeignError::LimitExceeded {
            limit: "listing memory",
            ..
        }))
    ));
}

#[test]
fn huge_counts_on_the_wire_are_protocol_errors() {
    let mut bytes = script(&[hello(), opened()]);
    // An Entries frame claiming u32::MAX records in a small payload.
    let mut p = Vec::new();
    p.extend_from_slice(&u32::MAX.to_le_bytes());
    p.push(0);
    p.extend_from_slice(&[0u8; RECORD_MIN]);
    lpk_proto::frame::write_frame(&mut bytes, lpk_proto::frame::kind::ENTRIES, &p).unwrap();
    let (r, _) = replay(&bytes, 100, None, &crate::DefaultPolicy, &Limits::default());
    assert!(matches!(
        r,
        Err(CoreError::Worker(WorkerError::Protocol { .. }))
    ));
    // A frame header claiming 4 GiB.
    let mut bytes = script(&[hello()]);
    bytes.extend_from_slice(&u32::MAX.to_le_bytes());
    bytes.extend_from_slice(&lpk_proto::frame::kind::DATA.to_le_bytes());
    let (r, _) = replay(&bytes, 100, None, &crate::DefaultPolicy, &Limits::default());
    assert!(matches!(
        r,
        Err(CoreError::Worker(WorkerError::Protocol { .. }))
    ));
}

#[test]
fn a_panic_report_is_named() {
    let (r, _) = run(
        &[
            hello(),
            Response::Fatal {
                message: "boom\x1b".into(),
            },
        ],
        100,
    );
    match r {
        Err(CoreError::Worker(WorkerError::Panicked { message })) => {
            assert_eq!(message, "boom\\u{1b}")
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_partial_file_is_removed_on_failure() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("out");
    let mut f = one_member(b"abc");
    f[4] = Response::Data(b"a".to_vec());
    f.truncate(5); // the pipe closes mid-member
    let (r, _) = replay(
        &script(&f),
        100,
        Some(&out),
        &crate::DefaultPolicy,
        &Limits::default(),
    );
    assert!(
        matches!(r, Err(CoreError::Worker(WorkerError::Pipe { .. }))),
        "{r:?}"
    );
    assert!(!out.join("a").exists());
    // And a good one is written.
    let (r, _) = replay(
        &script(&one_member(b"abc")),
        100,
        Some(&out),
        &crate::DefaultPolicy,
        &Limits::default(),
    );
    r.unwrap();
    assert_eq!(std::fs::read(out.join("a")).unwrap(), b"abc");
}

#[test]
fn hard_links_become_copies_or_are_refused() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("out");
    let mut f = one_member(b"abc");
    let link = Record {
        kind: RecordKind::HardLink,
        target: b"a".to_vec(),
        ..rec("b", 0, 0)
    };
    f[2] = entries(vec![rec("a", 3, 0), link.clone()]);
    let (r, _) = replay(
        &script(&f),
        100,
        Some(&out),
        &crate::DefaultPolicy,
        &Limits::default(),
    );
    assert_eq!(r.unwrap().files, 2);
    assert_eq!(std::fs::read(out.join("b")).unwrap(), b"abc");
    let mut f = one_member(b"abc");
    let bad = Record {
        target: b"missing".to_vec(),
        ..link
    };
    f[2] = entries(vec![rec("a", 3, 0), bad]);
    let (r, written) = run(&f, 100);
    assert!(matches!(
        r,
        Err(CoreError::Foreign(ForeignError::Unsupported { .. }))
    ));
    assert_eq!(written, 0);
}

/// Hard links resolve through a map: 20,000 links and 20,000 files plan in well under a
/// second (the quadratic search took minutes).
#[test]
fn many_hard_links_resolve_in_linear_time() {
    let n = 20_000u64;
    let mut recs: Vec<Record> = (0..n).map(|i| rec(&format!("f{i}"), 0, 0)).collect();
    recs.extend((0..n).map(|i| Record {
        kind: RecordKind::HardLink,
        target: format!("f{}", n - 1 - i).into_bytes(),
        ..rec(&format!("h{i}"), 0, 0)
    }));
    let mut f = vec![hello(), opened()];
    for chunk in recs.chunks(PAGE as usize) {
        f.push(Response::Entries {
            records: chunk.to_vec(),
            more: true,
        });
    }
    if let Some(Response::Entries { more, .. }) = f.last_mut() {
        *more = false;
    }
    for i in 0..n {
        f.push(Response::MemberStart { index: i });
        f.push(Response::MemberEnd {
            index: i,
            checked: false,
        });
    }
    let t = std::time::Instant::now();
    let (r, _) = run(&f, 10_000_000);
    assert_eq!(r.unwrap().files, 2 * n);
    assert!(
        t.elapsed() < std::time::Duration::from_secs(20),
        "{:?}",
        t.elapsed()
    );
}

#[test]
fn duplicates_are_refused() {
    let mut f = one_member(b"abc");
    f[2] = entries(vec![rec("a", 3, 0), rec("a", 3, 3)]);
    let (r, written) = run(&f, 100);
    assert!(matches!(r, Err(CoreError::DuplicatePath { .. })));
    assert_eq!(written, 0);
}

#[test]
fn detection_recognises_the_fixture_only() {
    assert_eq!(
        detect(&fixture::write(&[Member::file("a", b"x")], None)),
        Some(Format::TestFixture)
    );
    assert_eq!(detect(b"PK\x03\x04"), None);
    assert_eq!(detect(b""), None);
}

/// What the `worker_proto` fuzz target does, over every truncation and single-byte change of
/// a good session: never a panic, never more written than declared.
#[test]
fn truncated_and_flipped_output_never_panics() {
    let good = script(&one_member(b"hello world"));
    for cut in 0..good.len() {
        let (_, written) = replay(
            &good[..cut],
            100,
            None,
            &crate::DefaultPolicy,
            &Limits::default(),
        );
        assert!(written <= 11);
    }
    for i in 0..good.len() {
        for x in [0x01u8, 0x80, 0xFF] {
            let mut b = good.clone();
            b[i] ^= x;
            let (_, written) = replay(&b, 100, None, &crate::DefaultPolicy, &Limits::default());
            assert!(written <= 100 * DEFAULT_MAX_EXPANSION);
        }
    }
}

#[cfg(any(windows, target_os = "linux"))]
#[test]
fn the_protocol_roles_equal_the_launchers() {
    use lpk_proto::worker::roles as p;
    use lpk_sandbox_sys::roles as s;
    assert_eq!(
        (p::REQUESTS, p::RESPONSES, p::STDERR, p::ARCHIVE),
        (s::REQUESTS, s::RESPONSES, s::STDERR, s::ARCHIVE)
    );
}

/// A worker whose request writer never frees up (the channel full, the writer blocked on a
/// pipe the worker does not read): `send` must not block, and ends at the watchdog or the
/// cancel token. (With the old blocking write this test would hang.)
struct BusyPeer;

impl Peer for BusyPeer {
    fn try_send(&mut self, frame: Frame) -> Result<Option<Frame>, RecvError> {
        Ok(Some(frame))
    }
    fn recv(&mut self, tick: Duration) -> Result<Option<Response>, RecvError> {
        std::thread::sleep(tick);
        Ok(None)
    }
    fn usage(&self) -> Option<Usage> {
        None
    }
    fn set_cpu_limit(&self, _d: Duration) {}
    fn exit_code(&self, _wait: Duration) -> Option<u32> {
        None
    }
    fn kill(&self) {}
    fn stderr(&self) -> String {
        String::new()
    }
}

#[test]
fn a_full_request_channel_ends_at_the_watchdog_or_the_cancel() {
    let limits = Limits {
        no_progress: Duration::from_millis(300),
        ..Limits::default()
    };
    let monitor = Monitor::default();
    let mut conv = Conv::new(BusyPeer, limits.clone(), 100, &monitor);
    let t = Instant::now();
    let e = conv
        .send(&Request::ReadMember {
            indices: (0..MEMBER_BATCH as u64).collect(),
        })
        .unwrap_err();
    assert!(
        matches!(e, CoreError::Worker(WorkerError::Timeout { .. })),
        "{e}"
    );
    assert!(t.elapsed() >= limits.no_progress);
    assert!(
        t.elapsed() < limits.no_progress + Duration::from_secs(2),
        "{:?}",
        t.elapsed()
    );
    let cancel = crate::CancelToken::new();
    cancel.cancel();
    let monitor = Monitor::with_cancel(cancel);
    let mut conv = Conv::new(BusyPeer, limits, 100, &monitor);
    let e = conv.send(&Request::Close).unwrap_err();
    assert!(matches!(e, CoreError::Cancelled), "{e}");
}

/// Recorded output delivered slowly: each frame after `delay`.
struct SlowPeer<'a> {
    inner: ScriptPeer<'a>,
    delay: Duration,
}

impl Peer for SlowPeer<'_> {
    fn try_send(&mut self, frame: Frame) -> Result<Option<Frame>, RecvError> {
        self.inner.try_send(frame)
    }
    fn recv(&mut self, tick: Duration) -> Result<Option<Response>, RecvError> {
        std::thread::sleep(self.delay);
        self.inner.recv(tick)
    }
    fn usage(&self) -> Option<Usage> {
        None
    }
    fn set_cpu_limit(&self, _d: Duration) {}
    fn exit_code(&self, _wait: Duration) -> Option<u32> {
        Some(0)
    }
    fn kill(&self) {}
    fn stderr(&self) -> String {
        String::new()
    }
}

/// Many empty members: no byte is read or received after the listing, yet each finished
/// member is progress, so a stream longer than the watchdog's limit finishes.
#[test]
fn many_empty_members_are_progress() {
    let n = 40u64;
    let mut f = vec![
        hello(),
        opened(),
        entries((0..n).map(|i| rec(&format!("e{i}"), 0, 0)).collect()),
    ];
    for i in 0..n {
        f.push(Response::MemberStart { index: i });
        f.push(Response::MemberEnd {
            index: i,
            checked: false,
        });
    }
    let bytes = script(&f);
    // The five frames before the first `MemberEnd` carry no progress: 5 x 40 ms, which a
    // coarse Windows timer under load stretched past a 250 ms limit (a flake seen while E4-5's
    // tests ran beside other builds); 500 ms keeps the stream (83 frames) far longer.
    let limits = Limits {
        no_progress: Duration::from_millis(500),
        ..Limits::default()
    };
    let monitor = Monitor::default();
    let peer = SlowPeer {
        inner: ScriptPeer {
            input: &bytes,
            buf: Vec::new(),
        },
        delay: Duration::from_millis(40),
    };
    let mut conv = Conv::new(peer, limits, 10_000, &monitor);
    let t = Instant::now();
    conv.handshake().unwrap();
    conv.open(None).unwrap();
    let records = conv.list(Format::TestFixture).unwrap();
    let mut out = NullOut::default();
    let mut summary = ForeignSummary::default();
    run_extract(
        &mut conv,
        records,
        Path::new("lpk-replay\0nowhere"),
        false,
        &mut out,
        &crate::DefaultPolicy,
        &Selection::all(),
        &mut summary,
    )
    .unwrap();
    assert_eq!(summary.files, n);
    // Far longer than the watchdog's limit: the finished members kept it fed.
    assert!(t.elapsed() > Duration::from_secs(2), "{:?}", t.elapsed());
}

/// A worker that died with `.0` before its first frame, its job accounting unreadable.
struct DeadPeer(u32);

impl Peer for DeadPeer {
    fn try_send(&mut self, _frame: Frame) -> Result<Option<Frame>, RecvError> {
        Err(RecvError::Closed)
    }
    fn recv(&mut self, _tick: Duration) -> Result<Option<Response>, RecvError> {
        Err(RecvError::Closed)
    }
    fn usage(&self) -> Option<Usage> {
        None
    }
    fn set_cpu_limit(&self, _d: Duration) {}
    fn exit_code(&self, _wait: Duration) -> Option<u32> {
        Some(self.0)
    }
    fn kill(&self) {}
    fn stderr(&self) -> String {
        String::new()
    }
}

/// E4-5: the job's user-time kill (exit status STATUS_QUOTA_EXCEEDED) names `CpuLimit` on
/// Windows by the status alone, when no usage can be read; elsewhere that status means nothing
/// and the death is `Crashed`. Any other status without usage stays `Crashed`.
#[test]
fn quota_exceeded_status_names_the_cpu_limit_without_usage() {
    const STATUS_QUOTA_EXCEEDED: u32 = 0xC000_0044;
    const STATUS_ACCESS_VIOLATION: u32 = 0xC000_0005;
    let monitor = Monitor::default();
    let mut conv = Conv::new(
        DeadPeer(STATUS_QUOTA_EXCEEDED),
        Limits::default(),
        100,
        &monitor,
    );
    let e = conv.handshake().unwrap_err();
    if cfg!(windows) {
        assert!(
            matches!(e, CoreError::Worker(WorkerError::CpuLimit { .. })),
            "{e:?}"
        );
    } else {
        assert!(
            matches!(
                e,
                CoreError::Worker(WorkerError::Crashed {
                    code: STATUS_QUOTA_EXCEEDED,
                    ..
                })
            ),
            "{e:?}"
        );
    }
    let mut conv = Conv::new(
        DeadPeer(STATUS_ACCESS_VIOLATION),
        Limits::default(),
        100,
        &monitor,
    );
    let e = conv.handshake().unwrap_err();
    assert!(
        matches!(
            e,
            CoreError::Worker(WorkerError::Crashed {
                code: STATUS_ACCESS_VIOLATION,
                ..
            })
        ),
        "{e:?}"
    );
}
