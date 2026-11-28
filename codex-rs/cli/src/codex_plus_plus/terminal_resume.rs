use std::ffi::OsStr;
use std::path::Path;

use codex_protocol::ThreadId;

pub(crate) fn apply_home() -> anyhow::Result<()> {
    let mut args = std::env::args_os().skip(1);
    if args.next().as_deref() != Some(OsStr::new("--codex-resume-home")) {
        return Ok(());
    }
    let home = args
        .next()
        .filter(|home| Path::new(home).is_absolute())
        .ok_or_else(|| anyhow::anyhow!("Program resume requires an absolute Codex home"))?;
    anyhow::ensure!(
        args.next().as_deref() == Some(OsStr::new("resume"))
            && args
                .next()
                .and_then(|id| ThreadId::from_string(id.to_str()?).ok())
                .is_some(),
        "Program resume requires an exact thread ID"
    );
    // SAFETY: the CLI calls this on the startup thread before arg0 loads dotenv or starts Tokio.
    unsafe { std::env::set_var("CODEX_HOME", home) };
    Ok(())
}
