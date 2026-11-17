use coshell_drivers::server_overview::{collect, parse_snapshot, COLLECT_COMMAND};
use coshell_drivers::ssh::{
    BoundedRemoteOutput, Cancellation, RemoteExec, RemoteOutput, SshError, SshFuture,
};
use std::{
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};

const COMPLETE: &str = "hostname=host\nos=Linux\nkernel=6.1\narchitecture=x86_64\nuptime=3600.5\nboot_time=2026-01-01T00:00:00Z\ncpu_usage=25.5\nlogical_cores=4\nmemory_total=2048\nmemory_used=1024\nload=1.25\ndisk_total=4096\ndisk_used=3072\nswap_total=0\nswap_used=0\n";

#[test]
fn parses_complete_snapshot_with_units_and_zero_swap() {
    let s = parse_snapshot("c1", "2026-01-01T01:00:00Z", COMPLETE);
    assert_eq!(s.hostname.as_deref(), Some("host"));
    assert_eq!(s.uptime.unwrap().value, Some(3600.5));
    assert_eq!(s.cpu_usage.unwrap().value, Some(25.5));
    assert_eq!(s.logical_cores.unwrap().value, Some(4.0));
    assert_eq!(s.memory_total.unwrap().unit, "bytes");
    assert_eq!(s.load.unwrap().value, Some(1.25));
    assert_eq!(s.disk_used.unwrap().value, Some(3072.0));
    assert_eq!(s.swap_total.unwrap().value, Some(0.0));
}

#[test]
fn malformed_missing_and_nonfinite_metrics_are_unavailable_not_zero() {
    let s = parse_snapshot("c1", "now", "cpu_usage=NaN\nmemory_total=-2\nload=inf\n");
    for metric in [s.cpu_usage, s.memory_total, s.load, s.swap_used] {
        let metric = metric.unwrap();
        assert_eq!(metric.value, None);
        assert!(metric.unavailable_reason.is_some());
    }
}

#[test]
fn inconsistent_resource_pairs_and_invalid_core_count_are_unavailable() {
    let snapshot = parse_snapshot("c", "now", "logical_cores=0\nmemory_total=100\nmemory_used=101\nswap_total=0\nswap_used=1\ndisk_total=10\ndisk_used=11\n");
    assert_eq!(snapshot.logical_cores.unwrap().value, None);
    assert_eq!(snapshot.memory_used.unwrap().value, None);
    assert_eq!(snapshot.swap_used.unwrap().value, None);
    assert_eq!(snapshot.disk_used.unwrap().value, None);
    assert_eq!(
        parse_snapshot("c", "now", "logical_cores=1.5\n")
            .logical_cores
            .unwrap()
            .value,
        None
    );
    for input in [
        "memory_used=1\ndisk_used=1\n",
        "memory_total=0\nmemory_used=0\ndisk_total=0\ndisk_used=0\n",
    ] {
        let value = parse_snapshot("c", "now", input);
        assert_eq!(value.memory_used.unwrap().value, None);
        assert_eq!(value.disk_used.unwrap().value, None);
        assert_eq!(value.memory_total.unwrap().value, None);
        assert_eq!(value.disk_total.unwrap().value, None);
    }
}

#[test]
fn darwin_and_freebsd_collectors_parse_real_shell_fixture() {
    let shell = if cfg!(windows) {
        r"C:\Program Files\Git\bin\sh.exe"
    } else {
        "sh"
    };
    if cfg!(windows) && !std::path::Path::new(shell).exists() {
        return;
    }
    for (os, used, swap) in [
        ("Darwin", Some(8_192.0), Some(524_288.0)),
        ("FreeBSD", Some(12_288.0), Some(524_288.0)),
    ] {
        let preamble = format!(
            r#"
uname() {{ case "$1" in -s) echo {os};; -n) echo fixture;; -r) echo 1;; -m) echo x86_64;; esac; }}
sysctl() {{ case "$2" in kern.boottime) echo '{{ sec = 100, usec = 0 }}';; hw.logicalcpu|hw.ncpu) echo 4;; hw.memsize|hw.physmem) echo 16384;; hw.pagesize) echo 4096;; vm.stats.vm.v_free_count) echo 1;; vm.swap_total) echo 1048576;; vm.swapusage) echo 'total = 1.00M  used = 0.50M  free = 0.50M';; vm.loadavg) echo '{{ 1.25 1.00 0.50 }}';; kern.cp_time) if [ -e "$CPU_FIXTURE" ]; then echo '120 0 30 0 250'; else touch "$CPU_FIXTURE"; echo '100 0 20 0 200'; fi;; esac; }}
vm_stat() {{ printf 'Mach Virtual Memory Statistics: (page size of 4096 bytes)\nPages free: 1.\nPages inactive: 1.\nPages speculative: 0.\n'; }}
date() {{ case "$*" in *'+%s'*) echo 200;; *) echo '1970-01-01T00:01:40Z';; esac; }}
df() {{ printf 'Filesystem 1024-blocks Used Available Capacity Mounted on\n/dev/test 100 25 75 25%% /\n'; }}
top() {{ printf 'CPU usage: 20.0%% user, 5.0%% sys, 75.0%% idle\n'; }}
swapinfo() {{ printf 'Device 1K-blocks Used Avail Capacity\n/dev/test 1024 512 512 50%%\n'; }}
"#
        );
        let cpu_fixture =
            std::env::temp_dir().join(format!("coshell-cpu-fixture-{}-{os}", std::process::id()));
        let _ = std::fs::remove_file(&cpu_fixture);
        let output = std::process::Command::new(shell)
            .env(
                "COSHELL_TEST_SCRIPT",
                format!("{preamble}\n{COLLECT_COMMAND}"),
            )
            .arg("-c")
            .arg(r#"eval "$COSHELL_TEST_SCRIPT""#)
            .env("CPU_FIXTURE", &cpu_fixture)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{os}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let output = String::from_utf8(output.stdout).unwrap();
        let snapshot = parse_snapshot("fixture", "2026-01-01T00:00:00Z", &output);
        assert_eq!(snapshot.operating_system.as_deref(), Some(os), "{output}");
        assert_eq!(snapshot.uptime.unwrap().value, Some(100.0), "{output}");
        assert_eq!(
            snapshot.boot_time.as_deref(),
            Some("1970-01-01T00:01:40Z"),
            "{output}"
        );
        assert_eq!(snapshot.logical_cores.unwrap().value, Some(4.0), "{output}");
        assert_eq!(
            snapshot.memory_total.unwrap().value,
            Some(16_384.0),
            "{output}"
        );
        assert_eq!(snapshot.memory_used.unwrap().value, used, "{output}");
        assert_eq!(snapshot.load.unwrap().value, Some(1.25), "{output}");
        if os == "Darwin" {
            assert_eq!(snapshot.cpu_usage.unwrap().value, Some(25.0), "{output}");
        } else {
            assert_eq!(snapshot.cpu_usage.unwrap().value, Some(37.5), "{output}");
        }
        assert_eq!(snapshot.swap_used.unwrap().value, swap, "{output}");
        let _ = std::fs::remove_file(cpu_fixture);
    }
}

#[test]
fn failed_bsd_commands_degrade_to_unavailable_without_zero_values() {
    let shell = if cfg!(windows) {
        r"C:\Program Files\Git\bin\sh.exe"
    } else {
        "sh"
    };
    if cfg!(windows) && !std::path::Path::new(shell).exists() {
        return;
    }
    let preamble = r#"
uname() { case "$1" in -s) echo FreeBSD;; -n) echo fixture;; -r) echo 1;; -m) echo x86_64;; esac; }
sysctl() { return 1; }
swapinfo() { return 1; }
df() { return 1; }
sleep() { :; }
"#;
    let output = std::process::Command::new(shell)
        .env(
            "COSHELL_TEST_SCRIPT",
            format!("{preamble}\n{COLLECT_COMMAND}"),
        )
        .arg("-c")
        .arg(r#"eval "$COSHELL_TEST_SCRIPT""#)
        .output()
        .unwrap();
    assert!(output.status.success());
    let output = String::from_utf8(output.stdout).unwrap();
    let snapshot = parse_snapshot("c", "now", &output);
    for metric in [
        snapshot.uptime,
        snapshot.cpu_usage,
        snapshot.logical_cores,
        snapshot.memory_total,
        snapshot.memory_used,
        snapshot.load,
        snapshot.disk_total,
        snapshot.disk_used,
        snapshot.swap_total,
        snapshot.swap_used,
    ] {
        assert_eq!(metric.unwrap().value, None, "{output}");
    }
}

#[test]
fn malformed_platform_outputs_do_not_fabricate_values() {
    let shell = if cfg!(windows) {
        r"C:\Program Files\Git\bin\sh.exe"
    } else {
        "sh"
    };
    if cfg!(windows) && !std::path::Path::new(shell).exists() {
        return;
    }
    for os in ["Darwin", "FreeBSD"] {
        let preamble = format!(
            r#"
uname() {{ case "$1" in -s) echo {os};; -n) echo fixture;; -r) echo 1;; -m) echo x86_64;; esac; }}
sysctl() {{ case "$2" in kern.boottime) echo '{{ sec = 12x34, usec = 0 }}';; hw.memsize|hw.physmem) echo 16384;; hw.pagesize) echo 4096;; vm.stats.vm.v_free_count) echo 1;; vm.swap_total) echo 1048576;; vm.swapusage) echo 'total = garbageM used = garbageM';; esac; }}
vm_stat() {{ printf 'Mach Virtual Memory Statistics: (page size of 4096 bytes)\nPages free: garbage.\nPages inactive: 1.\nPages speculative: 0.\n'; }}
swapinfo() {{ printf 'Device 1K-blocks Used Avail Capacity\n/dev/a 512 256 256 50%%\n/dev/b malformed\n'; }}
date() {{ echo 20000; }}
sleep() {{ :; }}
df() {{ return 1; }}
"#
        );
        let output = std::process::Command::new(shell)
            .env(
                "COSHELL_TEST_SCRIPT",
                format!("{preamble}\n{COLLECT_COMMAND}"),
            )
            .arg("-c")
            .arg(r#"eval "$COSHELL_TEST_SCRIPT""#)
            .output()
            .unwrap();
        let text = String::from_utf8(output.stdout).unwrap();
        let snapshot = parse_snapshot("c", "now", &text);
        assert_eq!(snapshot.uptime.unwrap().value, None, "{text}");
        assert_eq!(snapshot.boot_time, None, "{text}");
        assert_eq!(snapshot.swap_used.unwrap().value, None, "{text}");
        if os == "Darwin" {
            assert_eq!(snapshot.memory_used.unwrap().value, None, "{text}");
            assert_eq!(snapshot.swap_total.unwrap().value, None, "{text}");
        }
    }
}

#[test]
fn failed_commands_with_plausible_output_and_bad_cpu_are_unavailable() {
    let shell = if cfg!(windows) {
        r"C:\Program Files\Git\bin\sh.exe"
    } else {
        "sh"
    };
    if cfg!(windows) && !std::path::Path::new(shell).exists() {
        return;
    }
    for os in ["Darwin", "FreeBSD"] {
        let preamble = format!(
            r#"
uname() {{ case "$1" in -s) echo {os};; -n) echo fixture;; -r) echo 1;; -m) echo x86_64;; esac; }}
sysctl() {{ case "$2" in hw.memsize|hw.physmem) echo 16384;; kern.cp_time) echo '10 bad 10 10 10';; vm.swapusage) echo 'total = 1.00M used = 0.50M'; return 1;; esac; }}
vm_stat() {{ printf 'Mach Virtual Memory Statistics: (page size of 4096 bytes)\nPages free: 1.\nPages inactive: 1.\nPages speculative: 0.\n'; return 1; }}
top() {{ printf 'CPU usage: 20.0%% user, 5.0%% sys, 75.0%% idle\n'; return 1; }}
df() {{ printf 'Filesystem 1024-blocks Used Available Capacity Mounted on\n/dev/test 100 25 75 25%% /\n'; return 1; }}
sleep() {{ :; }}
"#
        );
        let output = std::process::Command::new(shell)
            .env(
                "COSHELL_TEST_SCRIPT",
                format!("{preamble}\n{COLLECT_COMMAND}"),
            )
            .arg("-c")
            .arg(r#"eval "$COSHELL_TEST_SCRIPT""#)
            .output()
            .unwrap();
        let text = String::from_utf8(output.stdout).unwrap();
        let s = parse_snapshot("c", "now", &text);
        assert_eq!(s.cpu_usage.unwrap().value, None, "{text}");
        assert_eq!(s.disk_total.unwrap().value, None, "{text}");
        if os == "Darwin" {
            assert_eq!(s.memory_used.unwrap().value, None, "{text}");
            assert_eq!(s.swap_total.unwrap().value, None, "{text}");
        }
    }
}

#[test]
fn valid_stdout_from_failed_identity_sysctl_and_date_is_not_used() {
    let shell = if cfg!(windows) {
        r"C:\Program Files\Git\bin\sh.exe"
    } else {
        "sh"
    };
    if cfg!(windows) && !std::path::Path::new(shell).exists() {
        return;
    }
    for os in ["Darwin", "FreeBSD"] {
        let preamble = format!(
            r#"
uname() {{ case "$1" in -s) echo {os};; *) echo plausible; return 1;; esac; }}
sysctl() {{ case "$2" in kern.boottime) echo '{{ sec = 100, usec = 0 }}';; hw.logicalcpu|hw.ncpu) echo 4; return 1;; hw.memsize|hw.physmem) echo 16384; return 1;; hw.pagesize) echo 4096; return 1;; vm.stats.vm.v_free_count) echo 1; return 1;; vm.swap_total) echo 1048576; return 1;; esac; }}
date() {{ case "$*" in *'+%s'*) echo 200;; *) echo '1970-01-01T00:01:40Z';; esac; return 1; }}
sleep() {{ :; }}
df() {{ return 1; }}
"#
        );
        let output = std::process::Command::new(shell)
            .env(
                "COSHELL_TEST_SCRIPT",
                format!("{preamble}\n{COLLECT_COMMAND}"),
            )
            .arg("-c")
            .arg(r#"eval "$COSHELL_TEST_SCRIPT""#)
            .output()
            .unwrap();
        let text = String::from_utf8(output.stdout).unwrap();
        let s = parse_snapshot("c", "now", &text);
        assert_eq!(s.hostname, None, "{text}");
        assert_eq!(s.kernel, None, "{text}");
        assert_eq!(s.architecture, None, "{text}");
        assert_eq!(s.boot_time, None, "{text}");
        assert_eq!(s.uptime.unwrap().value, None, "{text}");
        assert_eq!(s.logical_cores.unwrap().value, None, "{text}");
        assert_eq!(s.memory_total.unwrap().value, None, "{text}");
        assert_eq!(s.swap_total.unwrap().value, None, "{text}");
    }
}

#[test]
fn linux_proc_awk_failure_does_not_emit_plausible_output() {
    let shell = if cfg!(windows) {
        r"C:\Program Files\Git\bin\sh.exe"
    } else {
        "sh"
    };
    if cfg!(windows) && !std::path::Path::new(shell).exists() {
        return;
    }
    let preamble = r#"
uname() { case "$1" in -s) echo Linux;; *) echo fixture;; esac; }
awk() { case "$*" in */proc/uptime*) echo 'uptime=12'; return 1;; */proc/meminfo*) printf 'memory_total=16384\nmemory_used=8192\nswap_total=0\nswap_used=0\n'; return 1;; */proc/loadavg*) echo 'load=1'; return 1;; */proc/stat*) echo 'logical_cores=4'; return 1;; *) /usr/bin/awk "$@";; esac; }
sleep() { :; }
df() { return 1; }
"#;
    let output = std::process::Command::new(shell)
        .env(
            "COSHELL_TEST_SCRIPT",
            format!("{preamble}\n{COLLECT_COMMAND}"),
        )
        .arg("-c")
        .arg(r#"eval "$COSHELL_TEST_SCRIPT""#)
        .output()
        .unwrap();
    let text = String::from_utf8(output.stdout).unwrap();
    let s = parse_snapshot("c", "now", &text);
    for metric in [
        s.uptime,
        s.memory_total,
        s.memory_used,
        s.swap_total,
        s.swap_used,
        s.load,
        s.logical_cores,
    ] {
        assert_eq!(metric.unwrap().value, None, "{text}");
    }
}

#[test]
fn linux_meminfo_rejects_malformed_units_duplicates_and_missing_values() {
    let shell = if cfg!(windows) {
        r"C:\Program Files\Git\bin\sh.exe"
    } else {
        "sh"
    };
    if cfg!(windows) && !std::path::Path::new(shell).exists() {
        return;
    }
    for fixture in [
        "MemTotal: 100 kB\nMemAvailable: bad kB\nSwapTotal: bad kB\nSwapFree: bad kB",
        "MemTotal: 100 MB\nMemAvailable: 50 kB\nSwapTotal: 2 MB\nSwapFree: 1 kB",
        "MemTotal: 100 kB\nMemTotal: 100 kB\nMemAvailable: 50 kB\nSwapTotal: 2 kB\nSwapFree: 1 kB\nSwapFree: 1 kB",
        "MemAvailable: 50 kB\nSwapTotal: 2 kB",
    ] {
        let preamble=format!(r#"
uname() {{ case "$1" in -s) echo Linux;; *) echo fixture;; esac; }}
awk() {{ case "$*" in */proc/meminfo*) printf '%s\n' "$MEM_FIXTURE" | /usr/bin/awk "$1";; *) /usr/bin/awk "$@";; esac; }}
sleep() {{ :; }}
"#);
        let output=std::process::Command::new(shell).env("COSHELL_TEST_SCRIPT", format!("{preamble}\n{COLLECT_COMMAND}")).arg("-c").arg(r#"eval "$COSHELL_TEST_SCRIPT""#).env("MEM_FIXTURE",fixture).output().unwrap();
        let text=String::from_utf8(output.stdout).unwrap(); let s=parse_snapshot("c","now",&text);
        if fixture.contains("MemAvailable: bad") { assert_eq!(s.memory_used.unwrap().value,None,"{text}"); assert_eq!(s.swap_total.unwrap().value,None,"{text}"); assert_eq!(s.swap_used.unwrap().value,None,"{text}"); }
        else if fixture.contains("100 MB") { assert_eq!(s.memory_total.unwrap().value,None,"{text}"); assert_eq!(s.memory_used.unwrap().value,None,"{text}"); assert_eq!(s.swap_total.unwrap().value,None,"{text}"); }
        else if fixture.contains("MemTotal: 100 kB\nMemTotal") { assert_eq!(s.memory_total.unwrap().value,None,"{text}"); assert_eq!(s.memory_used.unwrap().value,None,"{text}"); assert_eq!(s.swap_used.unwrap().value,None,"{text}"); }
        else { assert_eq!(s.memory_total.unwrap().value,None,"{text}"); assert_eq!(s.memory_used.unwrap().value,None,"{text}"); assert_eq!(s.swap_used.unwrap().value,None,"{text}"); }
    }
}

#[test]
fn failed_awk_with_plausible_output_is_discarded_in_bsd_pipelines() {
    let shell = if cfg!(windows) {
        r"C:\Program Files\Git\bin\sh.exe"
    } else {
        "sh"
    };
    if cfg!(windows) && !std::path::Path::new(shell).exists() {
        return;
    }
    for os in ["Darwin", "FreeBSD"] {
        let preamble = format!(
            r#"
uname() {{ case "$1" in -s) echo {os};; *) echo fixture;; esac; }}
sysctl() {{ case "$2" in hw.memsize|hw.physmem) echo 16384;; hw.pagesize) echo 4096;; vm.stats.vm.v_free_count) echo 1;; vm.swap_total) echo 1048576;; vm.swapusage) echo 'total = 1.00M used = 0.50M';; kern.cp_time) echo '1 2 3 4 5';; esac; }}
vm_stat() {{ echo plausible; }}
top() {{ echo plausible; }}
swapinfo() {{ echo plausible; }}
df() {{ echo plausible; }}
awk() {{ printf 'memory_used=8192\nswap_used=524288\ncpu_usage=20\ndisk_total=1000\ndisk_used=500\n'; return 1; }}
sleep() {{ :; }}
"#
        );
        let output = std::process::Command::new(shell)
            .env(
                "COSHELL_TEST_SCRIPT",
                format!("{preamble}\n{COLLECT_COMMAND}"),
            )
            .arg("-c")
            .arg(r#"eval "$COSHELL_TEST_SCRIPT""#)
            .output()
            .unwrap();
        let text = String::from_utf8(output.stdout).unwrap();
        let s = parse_snapshot("c", "now", &text);
        for metric in [
            s.memory_used,
            s.swap_used,
            s.cpu_usage,
            s.disk_total,
            s.disk_used,
        ] {
            assert_eq!(metric.unwrap().value, None, "{text}");
        }
    }
}

#[test]
fn freebsd_zero_swap_is_known_only_after_successful_complete_query() {
    let shell = if cfg!(windows) {
        r"C:\Program Files\Git\bin\sh.exe"
    } else {
        "sh"
    };
    if cfg!(windows) && !std::path::Path::new(shell).exists() {
        return;
    }
    for (exit, expected) in [(0, Some(0.0)), (1, None)] {
        let preamble = format!(
            r#"
uname() {{ case "$1" in -s) echo FreeBSD;; -n) echo fixture;; -r) echo 1;; -m) echo x86_64;; esac; }}
sysctl() {{ case "$2" in vm.swap_total) echo 0;; esac; }}
swapinfo() {{ printf 'Device 1K-blocks Used Avail Capacity\n'; return {exit}; }}
sleep() {{ :; }}
df() {{ return 1; }}
"#
        );
        let output = std::process::Command::new(shell)
            .env(
                "COSHELL_TEST_SCRIPT",
                format!("{preamble}\n{COLLECT_COMMAND}"),
            )
            .arg("-c")
            .arg(r#"eval "$COSHELL_TEST_SCRIPT""#)
            .output()
            .unwrap();
        let text = String::from_utf8(output.stdout).unwrap();
        assert_eq!(
            parse_snapshot("c", "now", &text).swap_used.unwrap().value,
            expected,
            "{text}"
        );
    }
}

#[test]
fn linux_cpu_delta_rejects_idle_counter_regression_and_excess() {
    let shell = if cfg!(windows) {
        r"C:\Program Files\Git\bin\sh.exe"
    } else {
        "sh"
    };
    if cfg!(windows) && !std::path::Path::new(shell).exists() {
        return;
    }
    let dir = std::env::temp_dir().join(format!("coshell-linux-cpu-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for (index, second) in ["cpu 120 0 30 0 0 0 0 0", "cpu 0 0 0 400 0 0 0 0"]
        .iter()
        .enumerate()
    {
        let marker = dir.join(format!("{index}"));
        let preamble = format!(
            r#"
uname() {{ case "$1" in -s) echo Linux;; -n) echo fixture;; -r) echo 1;; -m) echo x86_64;; esac; }}
awk() {{ case "$1" in *"idle="*) if [ -e "$CPU_FIXTURE" ]; then printf '%s\n' '{second}'; else touch "$CPU_FIXTURE"; printf '%s\n' 'cpu 100 0 20 200 0 0 0 0'; fi | /usr/bin/awk "$1";; *) /usr/bin/awk "$@";; esac; }}
sleep() {{ :; }}
"#
        );
        let output = std::process::Command::new(shell)
            .env(
                "COSHELL_TEST_SCRIPT",
                format!("{preamble}\n{COLLECT_COMMAND}"),
            )
            .arg("-c")
            .arg(r#"eval "$COSHELL_TEST_SCRIPT""#)
            .env("CPU_FIXTURE", &marker)
            .output()
            .unwrap();
        let text = String::from_utf8(output.stdout).unwrap();
        assert_eq!(
            parse_snapshot("c", "now", &text).cpu_usage.unwrap().value,
            None,
            "{text}"
        );
    }
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn freebsd_cpu_rejects_multiple_lines_and_malformed_sample() {
    let shell = if cfg!(windows) {
        r"C:\Program Files\Git\bin\sh.exe"
    } else {
        "sh"
    };
    if cfg!(windows) && !std::path::Path::new(shell).exists() {
        return;
    }
    let preamble = r#"
uname() { case "$1" in -s) echo FreeBSD;; *) echo fixture;; esac; }
sysctl() { case "$2" in kern.cp_time) if [ ! -e "$CPU_FIXTURE" ]; then touch "$CPU_FIXTURE"; printf '100 0 20 0 200\n120 0 30 0 250\n'; fi;; esac; }
sleep() { :; }
df() { return 1; }
"#;
    let marker = std::env::temp_dir().join(format!("coshell-bsd-multiline-{}", std::process::id()));
    let _ = std::fs::remove_file(&marker);
    let output = std::process::Command::new(shell)
        .env(
            "COSHELL_TEST_SCRIPT",
            format!("{preamble}\n{COLLECT_COMMAND}"),
        )
        .arg("-c")
        .arg(r#"eval "$COSHELL_TEST_SCRIPT""#)
        .env("CPU_FIXTURE", &marker)
        .output()
        .unwrap();
    let _ = std::fs::remove_file(marker);
    let text = String::from_utf8(output.stdout).unwrap();
    assert_eq!(
        parse_snapshot("c", "now", &text).cpu_usage.unwrap().value,
        None,
        "{text}"
    );
}

#[test]
fn linux_cpu_rejects_failed_duplicate_or_non_numeric_samples() {
    let shell = if cfg!(windows) {
        r"C:\Program Files\Git\bin\sh.exe"
    } else {
        "sh"
    };
    if cfg!(windows) && !std::path::Path::new(shell).exists() {
        return;
    }
    let preamble = r#"
uname() { case "$1" in -s) echo Linux;; *) echo fixture;; esac; }
awk() { case "$1" in *"idle="*) printf 'cpu 100 bad 20 200 0 0 0 0\ncpu 100 0 20 200 0 0 0 0\n' | /usr/bin/awk "$1";; *) /usr/bin/awk "$@";; esac; }
sleep() { :; }
"#;
    let output = std::process::Command::new(shell)
        .env(
            "COSHELL_TEST_SCRIPT",
            format!("{preamble}\n{COLLECT_COMMAND}"),
        )
        .arg("-c")
        .arg(r#"eval "$COSHELL_TEST_SCRIPT""#)
        .output()
        .unwrap();
    let text = String::from_utf8(output.stdout).unwrap();
    assert_eq!(
        parse_snapshot("c", "now", &text).cpu_usage.unwrap().value,
        None,
        "{text}"
    );
}

struct Exec {
    closed: Arc<AtomicUsize>,
    pending: bool,
}
impl RemoteExec for Exec {
    fn execute<'a>(&'a mut self, cmd: &'a str) -> SshFuture<'a, RemoteOutput> {
        Box::pin(async move {
            assert_eq!(cmd, COLLECT_COMMAND);
            if self.pending {
                std::future::pending().await
            } else {
                Ok(RemoteOutput {
                    stdout: COMPLETE.as_bytes().to_vec(),
                    stderr: vec![],
                    exit_status: Some(0),
                })
            }
        })
    }
    fn close<'a>(&'a mut self) -> SshFuture<'a, ()> {
        Box::pin(async move {
            self.closed.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
    }
    fn execute_bounded<'a>(
        &'a mut self,
        cmd: &'a str,
        stdout_limit: usize,
        stderr_limit: usize,
    ) -> SshFuture<'a, RemoteOutput> {
        assert_eq!(stdout_limit, 65_536);
        assert_eq!(stderr_limit, 65_536);
        self.execute(cmd)
    }
    fn abort(&mut self) {
        self.closed.fetch_add(1, Ordering::SeqCst);
    }
}

#[tokio::test]
async fn uses_fixed_command_and_closes_independent_channel_on_success_and_timeout() {
    let closed = Arc::new(AtomicUsize::new(0));
    let mut exec = Exec {
        closed: closed.clone(),
        pending: false,
    };
    let s = collect(
        &mut exec,
        "c1",
        "now",
        Duration::from_secs(1),
        &Cancellation::new(),
    )
    .await
    .unwrap();
    assert_eq!(s.hostname.as_deref(), Some("host"));
    let mut pending = Exec {
        closed: closed.clone(),
        pending: true,
    };
    assert_eq!(
        collect(
            &mut pending,
            "c1",
            "now",
            Duration::from_millis(5),
            &Cancellation::new()
        )
        .await
        .unwrap_err(),
        SshError::Timeout
    );
    assert!(closed.load(Ordering::SeqCst) >= 2);
}

#[tokio::test]
async fn cancelled_collection_closes_channel_without_exposing_outputs() {
    let closed = Arc::new(AtomicUsize::new(0));
    let mut exec = Exec {
        closed: closed.clone(),
        pending: true,
    };
    let cancel = Cancellation::new();
    cancel.cancel();
    assert_eq!(
        collect(&mut exec, "c1", "now", Duration::from_secs(1), &cancel)
            .await
            .unwrap_err(),
        SshError::Cancelled
    );
    assert!(closed.load(Ordering::SeqCst) >= 1);
}

#[test]
fn rejects_duplicate_fields_and_control_characters() {
    let duplicate = parse_snapshot("c", "now", "hostname=ok\ncpu_usage=1\ncpu_usage=99\n");
    assert_eq!(duplicate.cpu_usage.unwrap().value, None);
    let control = parse_snapshot("c", "now", "hostname=bad\x1b[31m\n");
    assert_eq!(control.hostname, None);
    for value in [
        "not-a-date",
        "2026-02-30T00:00:00Z",
        "2026-01-01T25:00:00Z",
        "2026-01-01T00:00:00+00:00",
    ] {
        assert_eq!(
            parse_snapshot("c", "now", &format!("boot_time={value}\n")).boot_time,
            None
        );
    }
    assert_eq!(
        parse_snapshot("c", "now", "boot_time=2024-02-29T00:00:00Z\n")
            .boot_time
            .as_deref(),
        Some("2024-02-29T00:00:00Z")
    );
}

#[test]
fn bounded_output_rejects_overflow_before_appending_each_chunk() {
    let mut output = BoundedRemoteOutput::new(4, 3);
    output.push_stdout(b"ab").unwrap();
    output.push_stdout(b"cd").unwrap();
    assert_eq!(output.push_stdout(b"e"), Err(SshError::Transport));
    output.push_stderr(b"xyz").unwrap();
    assert_eq!(output.push_stderr(b"!"), Err(SshError::Transport));
    let result = output.finish(Some(0));
    assert_eq!(result.stdout, b"abcd");
    assert_eq!(result.stderr, b"xyz");
}

#[tokio::test]
async fn dropping_pending_collection_aborts_channel() {
    let closed = Arc::new(AtomicUsize::new(0));
    let mut exec = Exec {
        closed: closed.clone(),
        pending: true,
    };
    let cancel = Cancellation::new();
    let mut future = Box::pin(collect(
        &mut exec,
        "c",
        "now",
        Duration::from_secs(30),
        &cancel,
    ));
    assert!(tokio::time::timeout(Duration::from_millis(10), &mut future)
        .await
        .is_err());
    drop(future);
    assert_eq!(closed.load(Ordering::SeqCst), 1);
}

#[cfg(unix)]
#[test]
fn collector_is_real_shell_and_uses_valid_boot_time() {
    let output = std::process::Command::new("sh")
        .arg("-c")
        .arg(COLLECT_COMMAND)
        .output()
        .unwrap();
    assert!(output.status.success());
    let text = String::from_utf8(output.stdout).unwrap();
    if let Some(line) = text.lines().find(|l| l.starts_with("boot_time=")) {
        let value = &line[10..];
        assert_eq!(value.len(), 20);
        assert_eq!(&value[10..11], "T");
        assert!(value.ends_with('Z'));
    }
}
