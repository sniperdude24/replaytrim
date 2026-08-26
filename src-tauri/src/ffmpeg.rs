use std::path::Path;
use std::process::Command;

/// Renders a visual waveform PNG for the given media file using ffmpeg's
/// showwavespic filter — avoids hand-rolling PCM decode/peak extraction.
pub fn generate_waveform(input: &Path, output_png: &Path, width: u32, height: u32) -> anyhow::Result<()> {
    let filter = format!("showwavespic=s={width}x{height}:colors=0x4f8cff");
    let status = Command::new("ffmpeg")
        .arg("-y")
        .arg("-i")
        .arg(input)
        .args(["-filter_complex", &filter])
        .arg(output_png)
        .status()?;
    if !status.success() {
        anyhow::bail!("ffmpeg waveform generation failed (exit {:?})", status.code());
    }
    Ok(())
}

/// Trims [start_secs, end_secs) out of `input` into `output`.
/// `fast` = stream-copy (near-instant, snaps to the nearest preceding keyframe).
/// `!fast` = re-encode (frame-accurate, slower).
pub fn trim(input: &Path, output: &Path, start_secs: f64, end_secs: f64, fast: bool) -> anyhow::Result<()> {
    let duration = (end_secs - start_secs).max(0.05);
    let mut cmd = Command::new("ffmpeg");
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

    cmd.arg(output);
    let status = cmd.status()?;
    if !status.success() {
        anyhow::bail!("ffmpeg trim failed (exit {:?})", status.code());
    }
    Ok(())
}
