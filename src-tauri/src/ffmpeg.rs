use std::path::Path;
use std::process::Command;

#[cfg(windows)]
use std::os::windows::process::CommandExt;

// CREATE_NO_WINDOW — without it, every ffmpeg invocation from a GUI app
// flashes a visible console window on Windows.
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

fn ffmpeg_command() -> Command {
    let mut cmd = Command::new("ffmpeg");
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);
    cmd
}

/// Media duration in seconds via ffprobe.
pub fn probe_duration(input: &Path) -> anyhow::Result<f64> {
    let mut cmd = Command::new("ffprobe");
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);
    let output = cmd
        .args(["-v", "error", "-show_entries", "format=duration", "-of", "csv=p=0"])
        .arg(input)
        .output()?;
    if !output.status.success() {
        anyhow::bail!("ffprobe failed (exit {:?})", output.status.code());
    }
    String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse::<f64>()
        .map_err(|e| anyhow::anyhow!("could not parse duration: {e}"))
}

/// Renders a visual waveform PNG for the given media file using ffmpeg's
/// showwavespic filter, and measures the clip's peak level in the same pass
/// so the UI can tell the user when the audio is actually silent (instead of
/// showing an invisible flat line). Returns max volume in dB (0 = full scale,
/// -91 ≈ digital silence).
pub fn generate_waveform(
    input: &Path,
    output_png: &Path,
    width: u32,
    height: u32,
) -> anyhow::Result<f64> {
    // scale=sqrt boosts quiet audio visually; filter=peak keeps transients.
    let filter = format!(
        "[0:a]asplit=2[w][v];[w]showwavespic=s={width}x{height}:colors=0x6ea8ff:scale=sqrt:filter=peak[wave];[v]volumedetect[vd]"
    );
    let output = ffmpeg_command()
        .arg("-y")
        .arg("-i")
        .arg(input)
        .args(["-filter_complex", &filter])
        .args(["-map", "[wave]", "-frames:v", "1"])
        .arg(output_png)
        .args(["-map", "[vd]", "-f", "null", "NUL"])
        .output()?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let tail: String = stderr.lines().rev().take(4).collect::<Vec<_>>().join(" | ");
        anyhow::bail!("ffmpeg waveform generation failed: {tail}");
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let max_db = stderr
        .lines()
        .find_map(|l| {
            l.split("max_volume:")
                .nth(1)
                .and_then(|s| s.trim().trim_end_matches(" dB").parse::<f64>().ok())
        })
        .unwrap_or(0.0);
    Ok(max_db)
}

/// Trims [start_secs, end_secs) out of `input` into `output`.
/// `fast` = stream-copy (near-instant, snaps to the nearest preceding keyframe).
/// `!fast` = re-encode (frame-accurate, slower).
///
/// Writes to a temp name and renames into place so OBS can never open a
/// half-written file (it did once — instant NAL-unit decode garbage), and
/// uses +faststart so the MP4 index sits at the front of the file.
pub fn trim(input: &Path, output: &Path, start_secs: f64, end_secs: f64, fast: bool) -> anyhow::Result<()> {
    let duration = (end_secs - start_secs).max(0.05);
    let tmp = output.with_extension("tmp.mp4");
    let mut cmd = ffmpeg_command();
    cmd.arg("-y");

    if fast {
        cmd.args(["-ss", &start_secs.to_string()]);
        cmd.arg("-i").arg(input);
        cmd.args(["-t", &duration.to_string(), "-c", "copy"]);
    } else {
        cmd.arg("-i").arg(input);
        cmd.args(["-ss", &start_secs.to_string(), "-t", &duration.to_string()]);
        cmd.args(["-c:v", "libx264", "-preset", "veryfast", "-c:a", "aac"]);
    }

    cmd.args(["-movflags", "+faststart", "-f", "mp4"]);
    cmd.arg(&tmp);
    let status = cmd.status()?;
    if !status.success() {
        let _ = std::fs::remove_file(&tmp);
        anyhow::bail!("ffmpeg trim failed (exit {:?})", status.code());
    }
    std::fs::rename(&tmp, output)?;
    Ok(())
}
