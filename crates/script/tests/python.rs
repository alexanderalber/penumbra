//! The script host against a real Python (skipped when none is found). One test function, run
//! in order, because a process has one interpreter and one Python thread.
//!
//! Runaway scripts are bounded: in time by the watchdog under test (each case asserts how long
//! it took) and in memory by a Windows job object limiting this test process to 2 GiB, which
//! also covers CPython's own heap. `PENUMBRA_PYTHON` picks another Python, e.g. the 3.13 check.

// The job object below is the test's only unsafe code.
#![allow(unsafe_code)]

// In a test module, so clippy treats the helpers as test code too.
#[cfg(test)]
mod python {
    use std::collections::BTreeMap;
    use std::time::{Duration, Instant};

    use penumbra_core::{Channel, Chans, Name, Table, Value};
    use penumbra_script::{Call, Load, ScriptError, host};

    #[cfg(windows)]
    fn limit_memory(bytes: usize) {
        use windows_sys::Win32::System::JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_PROCESS_MEMORY,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation, SetInformationJobObject,
        };
        use windows_sys::Win32::System::Threading::GetCurrentProcess;
        // SAFETY: plain Win32 calls with a zeroed, correctly sized structure.
        unsafe {
            let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            assert!(!job.is_null(), "no job object");
            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_PROCESS_MEMORY;
            info.ProcessMemoryLimit = bytes;
            let ok = SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                (&raw const info).cast(),
                u32::try_from(size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>()).unwrap(),
            );
            assert_ne!(ok, 0, "job limit");
            assert_ne!(AssignProcessToJobObject(job, GetCurrentProcess()), 0, "job assign");
        }
    }

    #[cfg(not(windows))]
    fn limit_memory(_bytes: usize) {}

    fn chans(pairs: &[(&str, &[f64])]) -> Chans {
        Chans::new(
            60.0,
            pairs.iter().map(|(n, s)| Channel { name: Name::new(n).unwrap(), samples: s.to_vec() }).collect(),
        )
        .unwrap()
    }

    fn load(key: u64, source: &str) -> Result<penumbra_script::Loaded, ScriptError> {
        static GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let generation = GENERATION.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        host().load(&Load { key, generation, source, label: "test.py", folder: None }, &|| false)
    }

    /// Calls, retrying while the thread is busy with something else (nothing else runs here, but a
    /// drop request may be in front).
    fn call(key: u64, call: &Call<'_>, limit: Duration) -> Result<penumbra_script::CallOutput, ScriptError> {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            match host().call("test", &Call { key, ..call.clone() }, limit, Box::new(|| {})) {
                Err(ScriptError::Busy) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(2))
                }
                r => return r,
            }
        }
    }

    #[test]
    fn python_end_to_end() {
        limit_memory(2 << 30);
        if let Err(e) = host().install() {
            eprintln!("no Python, skipped: {e}");
            return;
        }
        let none = BTreeMap::new();
        let base = Call {
            key: 0,
            time: None,
            rate: 60.0,
            params: &none,
            chans: vec![None, None],
            tables: vec![None, None],
        };
        let limit = Duration::from_millis(200);

        // Inputs, params and both outputs.
        let src = r#"
PARAMS = {"gain": (2.0, 0.0, 10.0), "mode": ["sum", "max"], "label": "row"}

def cook(cx):
    a = cx.chans[0]
    total = sum(v[-1] for v in a.values()) if cx.params["mode"] == "sum" else max(v[-1] for v in a.values())
    cx.out["total"] = total * cx.params["gain"]
    cx.out["ramp"] = [0.0, 0.5, float("nan")]
    for row in cx.tables[1]:
        cx.table.append([cx.params["label"]] + row)
    if cx.chans[1] is None:
        cx.warn("b is not connected")
    print("cooked")
"#;
        let loaded = load(1, src).unwrap();
        assert!(!loaded.uses_time);
        let names: Vec<&str> = loaded.params.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["gain", "mode", "label"]);
        let a = chans(&[("x", &[1.0]), ("y", &[2.0])]);
        let t = Table::new(vec![vec!["p".into(), "q".into()]]).unwrap();
        let params = BTreeMap::from([
            ("gain".to_string(), Value::Float(3.0)),
            ("mode".to_string(), Value::Text("sum".into())),
            ("label".to_string(), Value::Text("row".into())),
        ]);
        let out = call(
            1,
            &Call {
                params: &params,
                chans: vec![Some(&a), None],
                tables: vec![None, Some(&t)],
                ..base.clone()
            },
            limit,
        )
        .unwrap();
        // The single number goes with the three samples of `ramp` as three equal samples.
        assert_eq!(out.chans.get("total").unwrap().samples, [9.0, 9.0, 9.0]);
        assert_eq!(out.chans.get("ramp").unwrap().samples, [0.0, 0.5, 0.0], "NaN becomes 0");
        assert_eq!(out.table.rows(), [vec!["row".to_string(), "p".into(), "q".into()]]);
        assert_eq!(out.warnings, ["b is not connected"]);
        assert_eq!(out.printed, "cooked\n");

        // Errors name the script's line.
        let e = load(2, "def cook(cx):\n    x = = 1\n").unwrap_err();
        assert!(
            matches!(&e, ScriptError::Script(m) if m.starts_with("test.py, line 2: SyntaxError")),
            "{e:?}"
        );
        let e = load(2, "x = 1\n").unwrap_err();
        assert!(e.to_string().contains("no function cook"), "{e}");
        load(2, "def helper():\n    return 1 / 0\n\ndef cook(cx):\n    helper()\n").unwrap();
        let e = call(2, &base, limit).unwrap_err();
        assert_eq!(e.to_string(), "test.py, line 2, in helper: ZeroDivisionError: division by zero");
        // A failed load keeps the old module: the slot still has the 1/0 script.
        assert!(load(2, "def cook(cx) oops").is_err());
        assert!(call(2, &base, limit).unwrap_err().to_string().contains("ZeroDivisionError"));

        // Time: detected statically, passed in.
        let loaded =
            load(3, "def cook(cx):\n    cx.out['t'] = cx.time\n    cx.out['f'] = cx.frame\n").unwrap();
        assert!(loaded.uses_time);
        let out = call(3, &Call { time: Some((2.5, 150)), ..base.clone() }, limit).unwrap();
        assert_eq!((out.chans.value(Some("t")), out.chans.value(Some("f"))), (Some(2.5), Some(150.0)));
        let e = call(3, &Call { time: None, ..base.clone() }, limit).unwrap_err();
        assert!(e.to_string().contains("TypeError"), "None is not a number: {e}");

        // State lives in the module between calls.
        load(4, "n = 0\ndef cook(cx):\n    global n\n    n += 1\n    cx.out['n'] = n\n").unwrap();
        call(4, &base, limit).unwrap();
        assert_eq!(call(4, &base, limit).unwrap().chans.value(None), Some(2.0));

        // A runaway loop is stopped near its limit, and the next call works.
        load(5, "def cook(cx):\n    while True:\n        pass\n").unwrap();
        let start = Instant::now();
        let e = call(5, &base, Duration::from_millis(100)).unwrap_err();
        let took = start.elapsed();
        assert_eq!(e, ScriptError::Timeout(Duration::from_millis(100)));
        assert!(took < Duration::from_millis(100) + penumbra_script::host::GRACE, "{took:?}");
        eprintln!("runaway loop stopped after {took:?} (limit 100 ms)");
        assert_eq!(call(4, &base, limit).unwrap().chans.value(None), Some(3.0));

        // `except Exception` does not swallow the watchdog.
        load(6, "def cook(cx):\n    while True:\n        try:\n            while True:\n                pass\n        except Exception:\n            pass\n").unwrap();
        assert!(matches!(call(6, &base, Duration::from_millis(50)), Err(ScriptError::Timeout(_))));

        // Memory runaway: stopped by the watchdog (or by MemoryError under the job limit).
        load(7, "def cook(cx):\n    x = []\n    while True:\n        x.append(bytes(65536))\n").unwrap();
        let start = Instant::now();
        let e = call(7, &base, Duration::from_millis(50)).unwrap_err();
        assert!(matches!(&e, ScriptError::Timeout(_)) || e.to_string().contains("MemoryError"), "{e:?}");
        eprintln!("memory runaway stopped after {:?}: {e}", start.elapsed());
        call(4, &base, limit).unwrap();

        // Freed slots are gone.
        host().forget(4);
        let e = call(4, &base, limit).unwrap_err();
        assert!(e.to_string().contains("not loaded"), "{e}");

        // Measurements (debug build).
        let b = host().bench(2000).unwrap();
        eprintln!("{b:?}");
        assert!(b.call_trivial_us < 5000.0, "{b:?}");

        // A script the watchdog cannot stop for 1.5 s (in C code: a sleep): the caller gives up
        // after its limit plus the grace period, every call meanwhile fails at once, and the host
        // recovers when the script returns. Last, because it holds the thread.
        load(8, "import time\ndef cook(cx):\n    time.sleep(1.5)\n").unwrap();
        load(9, "def cook(cx):\n    cx.out['ok'] = 1\n").unwrap();
        let start = Instant::now();
        let e = call(8, &base, Duration::from_millis(100)).unwrap_err();
        assert!(matches!(e, ScriptError::Hung(_)), "{e:?}");
        assert!(start.elapsed() < Duration::from_millis(400), "{:?}", start.elapsed());
        let quick = Instant::now();
        let e = host().call("test", &Call { key: 9, ..base.clone() }, limit, Box::new(|| {})).unwrap_err();
        assert!(matches!(&e, ScriptError::Stuck(m) if m.contains("stuck")), "{e:?}");
        assert!(quick.elapsed() < Duration::from_millis(20), "a stuck host must not make callers wait");
        assert_eq!(host().status().state, "stuck");
        let deadline = Instant::now() + Duration::from_secs(5);
        while host().status().state != "idle" {
            assert!(Instant::now() < deadline, "the host did not recover");
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(call(9, &base, limit).unwrap().chans.value(None), Some(1.0));
        let s = host().status();
        eprintln!("{s:?}");
        assert!(s.interrupts >= 4 && s.timeouts >= 1, "{s:?}");
    }

    #[test]
    fn no_python_is_a_clean_error() {
        let h = penumbra_script::Host::new(Err("no Python 3.12 or newer found".into()));
        let none = BTreeMap::new();
        let c = Call { key: 1, time: None, rate: 60.0, params: &none, chans: vec![], tables: vec![] };
        let e = h.call("x", &c, Duration::from_millis(10), Box::new(|| {})).unwrap_err();
        assert_eq!(e.to_string(), "Python is not available: no Python 3.12 or newer found");
        assert!(!h.available());
        assert_eq!(h.status().state, "not_started");
    }
}
