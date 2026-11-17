//! Read-only server overview over an independent SSH exec channel.
use crate::ssh::{run_cancellable, Cancellation, RemoteExec, SshError};
use coshell_types::{Metric, ServerOverviewSnapshot};
use std::{collections::HashMap, time::Duration};

/// Fixed POSIX shell program. It reads only system identity and resource counters.
/// No connection data, user input, process list or environment values are interpolated.
pub const COLLECT_COMMAND: &str = r#"export LC_ALL=C
identity() { if raw=$(uname "$2" 2>/dev/null); then value=$(printf '%s' "$raw" | tr -cd '[:alnum:]_.-') || return; printf '%s=%s\n' "$1" "$value"; fi; }
safe_awk() { if result=$(awk "$@"); then printf '%s\n' "$result"; fi; }
identity hostname -n
identity os -s
identity kernel -r
identity architecture -m
system=$(uname -s 2>/dev/null) || system=''
case "$system" in
Linux)
if [ -r /proc/uptime ]; then if value=$(awk '{printf "uptime=%s\n",$1}' /proc/uptime); then printf '%s\n' "$value"; fi; fi
if [ -r /proc/stat ]; then
  epoch=$(awk '/^btime / {print $2; exit}' /proc/stat) || epoch=''
  if [ -n "$epoch" ]; then boot=$(date -u -d "@$epoch" '+%Y-%m-%dT%H:%M:%SZ' 2>/dev/null) || boot=''; if [ -n "$boot" ]; then printf 'boot_time=%s\n' "$boot"; fi; fi
  if value=$(awk '/^cpu[0-9]+ / {n++} END {if(n) print "logical_cores=" n}' /proc/stat); then printf '%s\n' "$value"; fi
  a=$(awk '/^cpu[[:space:]]/ {n++; if(NF<5) bad=1; for(i=2;i<=NF && i<=9;i++) if($i !~ /^[0-9]+$/) bad=1; idle=$5+$6; total=0; for(i=2;i<=NF && i<=9;i++) total+=$i} END {if(n==1 && !bad) printf "%.0f %.0f",idle,total}' /proc/stat) || a=''
  sleep 1
  b=$(awk '/^cpu[[:space:]]/ {n++; if(NF<5) bad=1; for(i=2;i<=NF && i<=9;i++) if($i !~ /^[0-9]+$/) bad=1; idle=$5+$6; total=0; for(i=2;i<=NF && i<=9;i++) total+=$i} END {if(n==1 && !bad) printf "%.0f %.0f",idle,total}' /proc/stat) || b=''
  if value=$(printf '%s %s\n' "$a" "$b" | awk 'NF==4 && $4>$2 && $3>=$1 && $3-$1<=$4-$2 {printf "cpu_usage=%.2f\n",100*(1-($3-$1)/($4-$2))}'); then printf '%s\n' "$value"; fi
fi
if [ -r /proc/meminfo ]; then if value=$(awk '
  $1=="MemTotal:" || $1=="MemAvailable:" || $1=="SwapTotal:" || $1=="SwapFree:" {key=$1; count[key]++; if(NF==3 && $2 ~ /^[0-9]+$/ && $3=="kB") {valid[key]=1; value[key]=$2*1024} else valid[key]=0}
  END {
    mt=(count["MemTotal:"]==1 && valid["MemTotal:"]); ma=(count["MemAvailable:"]==1 && valid["MemAvailable:"]);
    st=(count["SwapTotal:"]==1 && valid["SwapTotal:"]); sf=(count["SwapFree:"]==1 && valid["SwapFree:"]);
    if(mt && value["MemTotal:"]>0) printf "memory_total=%.0f\n",value["MemTotal:"];
    if(mt && ma && value["MemTotal:"]>0 && value["MemAvailable:"]<=value["MemTotal:"]) printf "memory_used=%.0f\n",value["MemTotal:"]-value["MemAvailable:"];
    if(st) printf "swap_total=%.0f\n",value["SwapTotal:"];
    if(st && sf && value["SwapFree:"]<=value["SwapTotal:"]) printf "swap_used=%.0f\n",value["SwapTotal:"]-value["SwapFree:"];
  }' /proc/meminfo); then printf '%s\n' "$value"; fi; fi
if [ -r /proc/loadavg ]; then if value=$(awk '{print "load=" $1}' /proc/loadavg); then printf '%s\n' "$value"; fi; fi
;;
Darwin|FreeBSD)
  # Wall-clock subtraction is an approximation; clock adjustments can affect uptime.
  boot_raw=$(sysctl -n kern.boottime 2>/dev/null) || boot_raw=''
  epoch=$(printf '%s\n' "$boot_raw" | awk '{for(i=1;i<=NF;i++) if($i=="sec" && $(i+1)=="=") {v=$(i+2); if(v ~ /^[0-9]+[,]?$/) {sub(/[,]$/,"",v); print v} exit}}') || epoch=''
  case "$epoch" in ''|*[!0-9]*) ;; *)
    now=$(date -u +%s 2>/dev/null) || now=''
    case "$now" in ''|*[!0-9]*) ;; *) if [ "$now" -ge "$epoch" ]; then printf 'uptime=%s\n' "$((now-epoch))"; fi ;; esac
    boot=$(date -u -r "$epoch" '+%Y-%m-%dT%H:%M:%SZ' 2>/dev/null) || boot=''
    if [ -n "$boot" ]; then printf 'boot_time=%s\n' "$boot"; fi ;;
  esac
  cores=$(sysctl -n hw.logicalcpu 2>/dev/null) || cores=''; if [ -z "$cores" ]; then cores=$(sysctl -n hw.ncpu 2>/dev/null) || cores=''; fi
  case "$cores" in ''|*[!0-9]*) ;; *) printf 'logical_cores=%s\n' "$cores" ;; esac
  load_raw=$(sysctl -n vm.loadavg 2>/dev/null) || load_raw=''
  load=$(printf '%s\n' "$load_raw" | awk '{gsub(/[{}]/, ""); for(i=1;i<=NF;i++) if($i ~ /^[0-9]+([.][0-9]+)?$/) {print $i; exit}}') || load=''
  if [ -n "$load" ]; then printf 'load=%s\n' "$load"; fi
  if [ "$system" = Darwin ]; then
    total=$(sysctl -n hw.memsize 2>/dev/null) || total=''
    case "$total" in ''|*[!0-9]*) ;; *) printf 'memory_total=%s\n' "$total" ;;
    esac
    # Darwin used = physical total minus free, inactive and speculative pages;
    # FreeBSD below uses physical total minus free pages (different OS semantics).
    if vm_output=$(vm_stat 2>/dev/null); then printf '%s\n' "$vm_output" | safe_awk -v total="$total" '
      /page size of/ {for(i=1;i<=NF;i++) if($i=="of") {page=$(i+1)+0; break}}
      /^Pages free:/ {if($3 ~ /^[0-9]+[.]?$/) {free=$3+0; hf=1}}
      /^Pages inactive:/ {if($3 ~ /^[0-9]+[.]?$/) {inactive=$3+0; hi=1}}
      /^Pages speculative:/ {if($3 ~ /^[0-9]+[.]?$/) {spec=$3+0; hs=1}}
      END {if(total ~ /^[0-9]+$/ && page>0 && hf && hi && hs && total >= (free+inactive+spec)*page) printf "memory_used=%.0f\n",total-(free+inactive+spec)*page}'; fi
    if swap_output=$(sysctl -n vm.swapusage 2>/dev/null); then printf '%s\n' "$swap_output" | safe_awk '
      function bytes(s,n,u) {if(s !~ /^[0-9]+([.][0-9]+)?[KkMmGgTtBb]?$/) return -1; n=s+0; u=substr(s,length(s),1); if(u=="K"||u=="k") n*=1024; else if(u=="M"||u=="m") n*=1048576; else if(u=="G"||u=="g") n*=1073741824; else if(u=="T"||u=="t") n*=1099511627776; return n}
      {for(i=1;i<=NF;i++) {if($i=="total" && $(i+1)=="=") {t=bytes($(i+2)); ht=1} if($i=="used" && $(i+1)=="=") {u=bytes($(i+2)); hu=1}}}
      END {if(ht && t>=0) printf "swap_total=%.0f\n",t; if(hu && u>=0) printf "swap_used=%.0f\n",u}'; fi
    if top_output=$(top -l 2 -n 0 -s 1 2>/dev/null); then printf '%s\n' "$top_output" | safe_awk '/CPU usage:/ {for(i=1;i<=NF;i++) if($i=="idle") {v=$(i-1); gsub(/[^0-9.]/,"",v); if(v ~ /^[0-9]+([.][0-9]+)?$/ && (v+0)<=100) {last=100-v; found=1}}} END {if(found) printf "cpu_usage=%.2f\n",last}'; fi
  else
    a=$(sysctl -n kern.cp_time 2>/dev/null) || a=''; sleep 1; b=$(sysctl -n kern.cp_time 2>/dev/null) || b=''
    a=$(printf '%s\n' "$a" | awk 'NR==1 && NF==5 {ok=1; for(i=1;i<=5;i++) if($i !~ /^[0-9]+$/) ok=0; line=$0} END {if(NR==1 && ok) print line}') || a=''
    b=$(printf '%s\n' "$b" | awk 'NR==1 && NF==5 {ok=1; for(i=1;i<=5;i++) if($i !~ /^[0-9]+$/) ok=0; line=$0} END {if(NR==1 && ok) print line}') || b=''
    if [ -n "$a" ] && [ -n "$b" ]; then printf '%s\n%s\n' "$a" "$b" | safe_awk 'NR==1 {for(i=1;i<=5;i++) first+=$i; idle=$5} NR==2 {for(i=1;i<=5;i++) total+=$i; if(total>first && $5>=idle && $5-idle<=total-first) printf "cpu_usage=%.2f\n",100*(1-($5-idle)/(total-first))}'; fi
    total=$(sysctl -n hw.physmem 2>/dev/null) || total=''; free=$(sysctl -n vm.stats.vm.v_free_count 2>/dev/null) || free=''; page=$(sysctl -n hw.pagesize 2>/dev/null) || page=''
    case "$total" in ''|*[!0-9]*) ;; *) printf 'memory_total=%s\n' "$total"; case "$free:$page" in *[!0-9:]*|:*|*:) ;; *) safe_awk -v t="$total" -v f="$free" -v p="$page" 'BEGIN {if(t>=f*p) printf "memory_used=%.0f\n",t-f*p}' ;; esac ;; esac
    swap_total=$(sysctl -n vm.swap_total 2>/dev/null) || swap_total=''
    case "$swap_total" in ''|*[!0-9]*) ;; *) printf 'swap_total=%s\n' "$swap_total" ;; esac
    if swap_output=$(swapinfo -k 2>/dev/null); then
      printf '%s\n' "$swap_output" | safe_awk -v total="$swap_total" 'NR==1 {if($1!="Device" || $2!="1K-blocks" || $3!="Used") bad=1; next} NF==0 {next} $1=="Total" {next} {if(NF<3 || $2 !~ /^[0-9]+$/ || $3 !~ /^[0-9]+$/ || $3>$2) {bad=1;next} t+=$2;u+=$3;n++} END {if(!bad && total ~ /^[0-9]+$/ && ((n && t*1024==total) || (!n && total==0))) printf "swap_used=%.0f\n",u*1024}'
    fi
  fi
;;
esac
if disk_output=$(df -Pk / 2>/dev/null); then printf '%s\n' "$disk_output" | safe_awk 'NR==2 && NF>=6 && $2 ~ /^[0-9]+$/ && $3 ~ /^[0-9]+$/ {printf "disk_total=%.0f\ndisk_used=%.0f\n",$2*1024,$3*1024}'; fi
"#;

fn metric(fields: &HashMap<&str, &str>, key: &str, unit: &str, max: Option<f64>) -> Option<Metric> {
    let value = fields
        .get(key)
        .and_then(|s| s.parse::<f64>().ok())
        .filter(|v| v.is_finite() && *v >= 0.0 && max.is_none_or(|m| *v <= m));
    Some(Metric {
        value,
        unit: unit.into(),
        unavailable_reason: value.is_none().then(|| "unavailable".into()),
    })
}

/// Parse a bounded, key/value collector response. Unknown keys are ignored.
pub fn parse_snapshot(
    connection_id: &str,
    collected_at: &str,
    text: &str,
) -> ServerOverviewSnapshot {
    let mut fields = HashMap::new();
    let mut duplicates = std::collections::HashSet::new();
    for line in text.lines() {
        if let Some((key, value)) = line.split_once('=') {
            if key.len() > 64
                || value.len() > 256
                || key.chars().any(char::is_control)
                || value.chars().any(char::is_control)
            {
                continue;
            }
            if fields.insert(key, value).is_some() {
                duplicates.insert(key);
            }
        }
    }
    for key in duplicates {
        fields.remove(key);
    }
    let text_field = |key: &str| {
        fields
            .get(key)
            .filter(|v| !v.is_empty())
            .map(|v| (*v).to_owned())
    };
    let mut snapshot = ServerOverviewSnapshot {
        connection_id: connection_id.into(),
        collected_at: collected_at.into(),
        online: true,
        hostname: text_field("hostname"),
        operating_system: text_field("os"),
        kernel: text_field("kernel"),
        architecture: text_field("architecture"),
        boot_time: text_field("boot_time").filter(|value| valid_utc_timestamp(value)),
        uptime: metric(&fields, "uptime", "seconds", None),
        cpu_usage: metric(&fields, "cpu_usage", "%", Some(100.0)),
        logical_cores: metric(&fields, "logical_cores", "cores", None),
        memory_total: metric(&fields, "memory_total", "bytes", None),
        memory_used: metric(&fields, "memory_used", "bytes", None),
        load: metric(&fields, "load", "load", None),
        disk_total: metric(&fields, "disk_total", "bytes", None),
        disk_used: metric(&fields, "disk_used", "bytes", None),
        swap_total: metric(&fields, "swap_total", "bytes", None),
        swap_used: metric(&fields, "swap_used", "bytes", None),
    };
    if let Some(metric) = snapshot.logical_cores.as_mut() {
        if metric
            .value
            .is_some_and(|value| value < 1.0 || value.fract() != 0.0)
        {
            metric.value = None;
            metric.unavailable_reason = Some("invalid".into());
        }
    }
    for (total, used, zero_allowed) in [
        (&mut snapshot.memory_total, &mut snapshot.memory_used, false),
        (&mut snapshot.swap_total, &mut snapshot.swap_used, true),
        (&mut snapshot.disk_total, &mut snapshot.disk_used, false),
    ] {
        if let Some(total) = total.as_mut() {
            if !zero_allowed && total.value == Some(0.0) {
                total.value = None;
                total.unavailable_reason = Some("invalid".into());
            }
            if let Some(used) = used.as_mut() {
                if total.value.is_none()
                    || used
                        .value
                        .zip(total.value)
                        .is_some_and(|(used, total)| used > total)
                {
                    used.value = None;
                    used.unavailable_reason = Some("inconsistent".into());
                }
            }
        }
    }
    snapshot
}

fn valid_utc_timestamp(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() != 20
        || bytes[4] != b'-'
        || bytes[7] != b'-'
        || bytes[10] != b'T'
        || bytes[13] != b':'
        || bytes[16] != b':'
        || bytes[19] != b'Z'
    {
        return false;
    }
    let parse = |range: std::ops::Range<usize>| -> Option<u32> {
        let part = bytes.get(range)?;
        if !part.iter().all(u8::is_ascii_digit) {
            return None;
        }
        std::str::from_utf8(part).ok()?.parse().ok()
    };
    let (Some(year), Some(month), Some(day), Some(hour), Some(minute), Some(second)) = (
        parse(0..4),
        parse(5..7),
        parse(8..10),
        parse(11..13),
        parse(14..16),
        parse(17..19),
    ) else {
        return false;
    };
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let days = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => return false,
    };
    year > 0 && day >= 1 && day <= days && hour < 24 && minute < 60 && second < 60
}

/// Execute the fixed collector over an independently opened exec channel. The
/// caller supplies an RFC3339 collection timestamp and owns the connection.
pub async fn collect(
    exec: &mut dyn RemoteExec,
    connection_id: &str,
    collected_at: &str,
    timeout: Duration,
    cancel: &Cancellation,
) -> Result<ServerOverviewSnapshot, SshError> {
    struct AbortOnDrop<'a>(&'a mut dyn RemoteExec);
    impl Drop for AbortOnDrop<'_> {
        fn drop(&mut self) {
            self.0.abort();
        }
    }
    let guard = AbortOnDrop(exec);
    let result = run_cancellable(
        timeout,
        cancel,
        guard.0.execute_bounded(COLLECT_COMMAND, 65_536, 65_536),
    )
    .await;
    // Cleanup is bounded separately so cancellation does not skip channel close.
    let close = tokio::time::timeout(Duration::from_secs(2), guard.0.close()).await;
    let output = result?;
    close.map_err(|_| SshError::Timeout)??;
    if output.exit_status != Some(0) || output.stdout.len() > 65_536 || output.stderr.len() > 65_536
    {
        return Err(SshError::Transport);
    }
    let text = std::str::from_utf8(&output.stdout).map_err(|_| SshError::Transport)?;
    Ok(parse_snapshot(connection_id, collected_at, text))
}
