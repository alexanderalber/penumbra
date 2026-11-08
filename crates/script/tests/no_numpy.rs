//! Pictures and meshes without numpy: clear errors, no crash. A process of its own (one
//! interpreter per process), whose first script hides numpy from every later import.

#[cfg(test)]
mod no_numpy {
    use std::collections::BTreeMap;
    use std::time::{Duration, Instant};

    use penumbra_script::{Call, CallOutput, InPicture, Load, ScriptError, host};

    fn load(key: u64, source: &str) -> Result<penumbra_script::Loaded, ScriptError> {
        host().load(&Load { key, generation: key, source, label: "test.py", folder: None }, &|| false)
    }

    fn call(call: &Call<'_>) -> Result<CallOutput, ScriptError> {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            match host().call("test", call, Duration::from_millis(500), Box::new(|| {})) {
                Err(ScriptError::Busy) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(2))
                }
                r => return r,
            }
        }
    }

    #[test]
    fn missing_numpy_is_a_clear_error() {
        if let Err(e) = host().install() {
            eprintln!("no Python, skipped: {e}");
            return;
        }
        // `None` in sys.modules makes every `import numpy` fail, as when it is not installed.
        load(1, "import sys\nsys.modules['numpy'] = None\ndef cook(cx):\n    cx.picture = [[0]]\n").unwrap();
        let none = BTreeMap::new();
        let base = Call {
            key: 1,
            time: None,
            rate: 60.0,
            params: &none,
            chans: vec![None, None],
            tables: vec![None, None],
            pictures: vec![None, None],
            want_picture: true,
            want_mesh: true,
        };
        let e = call(&base).unwrap_err().to_string();
        assert!(e.contains("numpy is not installed") && e.contains("-m pip install numpy"), "{e}");
        let e = load(2, "import numpy\ndef cook(cx):\n    pass\n").unwrap_err().to_string();
        assert!(e.contains("-m pip install numpy"), "{e}");
        // A picture input needs numpy too; a script that uses neither still runs.
        load(3, "def cook(cx):\n    cx.out['ok'] = 1\n").unwrap();
        let pic = InPicture { width: 1, height: 1, rgba: vec![0.0; 4] };
        let e =
            call(&Call { key: 3, pictures: vec![Some(&pic), None], ..base.clone() }).unwrap_err().to_string();
        assert!(e.contains("numpy is not installed"), "{e}");
        assert_eq!(call(&Call { key: 3, ..base.clone() }).unwrap().chans.value(None), Some(1.0));
    }
}
