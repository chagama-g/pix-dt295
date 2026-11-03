//! `dt295 logo --schedule` / `--unschedule`: a daily Windows Task Scheduler task that
//! runs `dt295 logo` for a TVTest folder, the way the vendor's application fetches
//! logos in the background on its own.

use std::path::Path;

use anyhow::{Context, Result, bail};

use super::tvtest::TvTest;
use super::{JST_OFFSET_SECS, unix_now};
use crate::bonini::BonDriver;

pub(super) const TASK_NAME: &str = "dt295 logo";
pub(super) const DEFAULT_TIME: &str = "03:55";
/// The log the task appends to, in the TVTest folder.
pub(super) const LOG_NAME: &str = "dt295_logo.log";

/// "HH:MM" as (hour, minute).
pub(super) fn parse_hhmm(s: &str) -> Result<(u8, u8)> {
    let parsed = s
        .split_once(':')
        .and_then(|(h, m)| Some((h.trim().parse::<u8>().ok()?, m.trim().parse::<u8>().ok()?)))
        .filter(|&(h, m)| h < 24 && m < 60);
    parsed.with_context(|| format!("{s:?}: not a time of day (HH:MM)"))
}

/// (year, month, day) of a day count since 1970-01-01.
pub(super) fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

/// One command-line argument quoted for Windows (CommandLineToArgvW rules).
pub(super) fn quote_arg(s: &str) -> String {
    let mut out = String::from('"');
    let mut backslashes = 0;
    for c in s.chars() {
        match c {
            '\\' => backslashes += 1,
            '"' => {
                out.push_str(&"\\".repeat(backslashes * 2 + 1));
                out.push('"');
                backslashes = 0;
            }
            _ => {
                out.push_str(&"\\".repeat(backslashes));
                out.push(c);
                backslashes = 0;
            }
        }
    }
    out.push_str(&"\\".repeat(backslashes * 2));
    out.push('"');
    out
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// The task: daily at `start` (a local "YYYY-MM-DDTHH:MM:00"), run late if the PC was
/// off then, one run at a time, at most 3 hours, as the user who registers it, only
/// while that user is logged on.
pub(super) fn task_xml(command: &str, arguments: &str, start: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo>
    <Description>Receive the BS / CS station logos for TVTest with a free PIX-DT295 (dt295 logo).</Description>
  </RegistrationInfo>
  <Triggers>
    <CalendarTrigger>
      <StartBoundary>{start}</StartBoundary>
      <ScheduleByDay>
        <DaysInterval>1</DaysInterval>
      </ScheduleByDay>
    </CalendarTrigger>
  </Triggers>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <StartWhenAvailable>true</StartWhenAvailable>
    <ExecutionTimeLimit>PT3H</ExecutionTimeLimit>
  </Settings>
  <Actions>
    <Exec>
      <Command>{}</Command>
      <Arguments>{}</Arguments>
    </Exec>
  </Actions>
</Task>
"#,
        xml_escape(command),
        xml_escape(arguments)
    )
}

/// A folder path without a trailing separator (one before a closing quote would
/// escape it), a drive root kept usable.
fn trim_dir(p: &Path) -> String {
    let s = p.display().to_string();
    let t = s.trim_end_matches(['\\', '/']);
    if t.ends_with(':') || t.is_empty() {
        format!("{t}\\.")
    } else {
        t.to_owned()
    }
}

/// The task's first run (Japan time): today at h:m if that is still ahead, else
/// tomorrow (a start in the past would make "run late if missed" start it at once).
pub(super) fn start_boundary(now_unix: u64, h: u8, m: u8) -> String {
    let local = now_unix + JST_OFFSET_SECS;
    let at = u64::from(h) * 3600 + u64::from(m) * 60;
    let day = local / 86_400 + u64::from(local % 86_400 >= at);
    let (y, mo, d) = civil_from_days(i64::try_from(day).unwrap_or_default());
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{m:02}:00")
}

/// Register (or replace) the task for this TVTest folder.
pub(super) fn create(tv: &TvTest, at: &str) -> Result<()> {
    let (h, m) = parse_hhmm(at)?;
    let drivers = tv.driver_dir();
    if BonDriver::find(&drivers)?.is_empty() {
        bail!(
            "no BonDriver_DT295*.ini in {}: the task finds its units there",
            drivers.display()
        );
    }
    let exe = std::env::current_exe().context("where is dt295.exe")?;
    let dir = trim_dir(&std::path::absolute(&tv.dir).context("the TVTest folder")?);
    let log = format!("{dir}\\{LOG_NAME}");
    let arguments = format!(
        "logo --tvtest {} --log {}",
        quote_arg(&dir),
        quote_arg(&log)
    );
    let start = start_boundary(unix_now(), h, m);
    let xml = task_xml(&exe.display().to_string(), &arguments, &start);
    register(&xml)?;
    println!(
        "registered the Task Scheduler task \"{TASK_NAME}\": every day at {h:02}:{m:02} \
         (later if the PC was off), running\n  {} {arguments}",
        exe.display()
    );
    println!("keep dt295.exe where it is; `dt295 logo --unschedule` removes the task");
    Ok(())
}

#[cfg(windows)]
fn schtasks(args: &[&std::ffi::OsStr]) -> Result<()> {
    let out = std::process::Command::new("schtasks")
        .args(args)
        .output()
        .context("cannot run schtasks")?;
    if !out.status.success() {
        bail!(
            "schtasks failed: {}{}",
            String::from_utf8_lossy(&out.stdout).trim(),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(())
}

#[cfg(windows)]
fn register(xml: &str) -> Result<()> {
    let file = std::env::temp_dir().join(format!("dt295_logo_task_{}.xml", std::process::id()));
    let mut bytes = vec![0xFF, 0xFE];
    bytes.extend(xml.encode_utf16().flat_map(u16::to_le_bytes));
    std::fs::write(&file, bytes).with_context(|| format!("cannot write {}", file.display()))?;
    let r = schtasks(&[
        "/Create".as_ref(),
        "/TN".as_ref(),
        TASK_NAME.as_ref(),
        "/XML".as_ref(),
        file.as_os_str(),
        "/F".as_ref(),
    ]);
    let _ = std::fs::remove_file(&file);
    r
}

#[cfg(not(windows))]
fn register(_: &str) -> Result<()> {
    bail!("--schedule is only on Windows (Task Scheduler)")
}

/// Remove the task.
#[cfg(windows)]
pub(super) fn remove() -> Result<()> {
    schtasks(&[
        "/Delete".as_ref(),
        "/TN".as_ref(),
        TASK_NAME.as_ref(),
        "/F".as_ref(),
    ])?;
    println!("removed the Task Scheduler task \"{TASK_NAME}\"");
    Ok(())
}

#[cfg(not(windows))]
pub(super) fn remove() -> Result<()> {
    bail!("--unschedule is only on Windows (Task Scheduler)")
}
