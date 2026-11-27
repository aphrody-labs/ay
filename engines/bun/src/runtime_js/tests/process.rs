//! The natives of `process` (`bun_runtime_js::node::process`), put on `process` as packages/bun-boa/src/js/bun.ts
//! puts them. Each expected value is what JavaScriptCore Bun answers for the same expression.

use bun_js::JSGlobalObject;
use bun_sema_standalone as _;

fn check(cases: &[(&str, &str)]) {
    JSGlobalObject::with_new(|global| {
        let native = bun_js::JSObject::create(global);
        for (name, value) in bun_runtime_js::node::process::functions(global) {
            native.put(global, name, value);
        }
        global.to_js_value().put(global, "native", native.to_js());
        global
            .evaluate_script(
                "globalThis.process = { execArgv: [], version: 'v24.3.0', arch: 'x64', platform: 'linux', versions: {}, release: { name: 'node' }, env: { A: '1' },
                   umask: native.umask, cpuUsage: native.cpuUsage, resourceUsage: native.resourceUsage,
                   hrtime: Object.assign(native.hrtime, { bigint: native.hrtimeBigint }),
                   memoryUsage: Object.assign(native.memoryUsage, { rss: native.memoryUsageRss }),
                   report: { getReport: native.getReport } };",
            )
            .expect("process");
        let failures: Vec<String> = cases
            .iter()
            .filter_map(|&(source, expected)| {
                let wrapped = format!(
                    "(() => {{ try {{ const v = {source}; return v === null ? 'null' : String(v) }} catch (e) {{ return `throws ${{e.name}}: ${{e.message}} (${{e.code}})` }} }})()"
                );
                let actual = global
                    .evaluate_script(&wrapped)
                    .expect("caught")
                    .to_utf8(global)
                    .expect("a string");
                (actual != expected).then(|| format!("{source}: {actual}, not {expected}"))
            })
            .collect();
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    });
}

#[test]
fn umask() {
    check(&[
        ("(process.umask(0o22), process.umask())", "18"),
        ("process.umask('0777')", "18"),
        ("process.umask(0o22)", "511"),
        (
            "process.umask('8')",
            "throws TypeError: The argument 'mask' must be a 32-bit unsigned integer or an octal string. Received '8' (ERR_INVALID_ARG_VALUE)",
        ),
        (
            "process.umask(-1)",
            "throws RangeError: The value of \"mask\" is out of range. It must be >= 0 && <= 4294967295. Received -1 (ERR_OUT_OF_RANGE)",
        ),
        (
            "process.umask(1.5)",
            "throws RangeError: The value of \"mask\" is out of range. It must be an integer. Received 1.5 (ERR_OUT_OF_RANGE)",
        ),
        (
            "process.umask({})",
            "throws TypeError: The \"mask\" argument must be of type number. Received an instance of Object (ERR_INVALID_ARG_TYPE)",
        ),
    ]);
}

#[test]
fn hrtime() {
    check(&[
        ("process.hrtime().length", "2"),
        (
            "process.hrtime([1])",
            "throws RangeError: The value of \"time\" is out of range. It must be 2. Received 1 (ERR_OUT_OF_RANGE)",
        ),
        (
            "process.hrtime(1)",
            "throws TypeError: The \"time\" argument must be an instance of Array. Received type number (1) (ERR_INVALID_ARG_TYPE)",
        ),
        ("process.hrtime([0, 0])[0] >= 0", "true"),
        ("(() => { const [s, ns] = process.hrtime(process.hrtime()); return s >= 0 && ns >= 0 && ns < 1e9 })()", "true"),
        ("typeof process.hrtime.bigint()", "bigint"),
        ("process.hrtime.bigint() <= process.hrtime.bigint()", "true"),
    ]);
}

#[test]
fn memory_and_cpu() {
    check(&[
        ("Object.keys(process.memoryUsage()).join()", "rss,heapTotal,heapUsed,external,arrayBuffers"),
        ("process.memoryUsage.rss() > 0", "true"),
        ("process.memoryUsage().heapTotal >= process.memoryUsage().heapUsed", "true"),
        ("Object.keys(process.cpuUsage()).join()", "user,system"),
        (
            "process.cpuUsage(1)",
            "throws TypeError: The \"prevValue\" argument must be of type object. Received type number (1) (ERR_INVALID_ARG_TYPE)",
        ),
        (
            "process.cpuUsage({ user: 'a', system: 0 })",
            "throws TypeError: The \"prevValue.user\" property must be of type number. Received type string ('a') (ERR_INVALID_ARG_TYPE)",
        ),
        (
            "process.cpuUsage({ user: -1, system: 0 })",
            "throws RangeError: The property 'prevValue.user' is invalid. Received -1 (ERR_INVALID_ARG_VALUE)",
        ),
        ("process.cpuUsage({ user: 0, system: 0 }).user >= 0", "true"),
        (
            "Object.keys(process.resourceUsage()).join()",
            "userCPUTime,systemCPUTime,maxRSS,sharedMemorySize,unsharedDataSize,unsharedStackSize,minorPageFault,majorPageFault,swappedOut,fsRead,fsWrite,ipcSent,ipcReceived,signalsCount,voluntaryContextSwitches,involuntaryContextSwitches",
        ),
        ("process.resourceUsage().maxRSS > 0", "true"),
    ]);
}

#[test]
fn report() {
    check(&[
        (
            "Object.keys(process.report.getReport()).join()",
            "header,javascriptStack,javascriptHeap,nativeStack,resourceUsage,uvthreadResourceUsage,libuv,workers,environmentVariables,userLimits,sharedObjects,cpus,networkInterfaces",
        ),
        ("process.report.getReport().header.processId > 0", "true"),
        ("process.report.getReport().header.nodejsVersion", "v24.3.0"),
        ("process.report.getReport().environmentVariables.A", "1"),
        ("typeof process.report.getReport().userLimits.open_files.soft", "number"),
    ]);
}
